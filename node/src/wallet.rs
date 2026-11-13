//! The passkey wallet, served by every node at `/wallet/`, so a chain's owner sends friends one
//! link: they open it, make a passkey wallet, take coins from the faucet and pay, with nothing to
//! install or host.
//!
//! The wallet is web/'s static export (`npm run build:wallet`, `BLOCKULA_SERVED=1`), compiled
//! into the binary by build.rs. Its pages talk to this node only (their own origin), and every
//! dynamic route (`app/tx/[id]`, `app/account/[id]`, …) is one page built for the placeholder id
//! `_`: a request for `/wallet/tx/<anything>/…` gets `tx/_/…`, and the page reads the real id
//! from the URL (web/lib/route-param.ts).
//!
//! Each page's Content-Security-Policy is computed here, as the explorer's is: Next.js puts its
//! flight data in inline `<script>`s, and the policy allows exactly those, by hash, and this
//! origin's script files. Nothing else runs, and the page fetches from this node only.

use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Router,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[cfg(blockula_wallet_table)]
include!(concat!(env!("OUT_DIR"), "/wallet_files.rs"));
/// No table (a crate that compiles this module without the node's build.rs): no wallet.
#[cfg(not(blockula_wallet_table))]
pub static WALLET_FILES: &[(&str, bool, &[u8])] = &[];

/// Where the wallet lives on a node.
pub const BASE: &str = "/wallet";
/// The id every dynamic route's one page is built for (web/lib/route-params.ts).
pub const PLACEHOLDER: &str = "_";

/// Whether this binary has the wallet in it.
pub fn embedded() -> bool {
    WALLET_FILES.iter().any(|(p, _, _)| *p == "index.html")
}

/// One embedded file.
#[derive(Clone, Copy)]
struct File {
    gz: bool,
    bytes: &'static [u8],
}

/// A page, ready to serve: its HTML under the chain's name, and the policy for its inline scripts.
struct Page {
    html: Arc<str>,
    csp: Arc<str>,
}

/// The wallet as this node serves it.
pub struct Wallet {
    files: HashMap<&'static str, File>,
    pages: HashMap<&'static str, Page>,
    /// Directories whose children are a dynamic route's ids (those holding a `_/index.html`).
    dynamic: HashSet<String>,
    /// The export's `manifest.webmanifest` under the chain's name (`chain_manifest`), if it has one.
    manifest: Option<Arc<str>>,
}

fn gunzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes).read_to_end(&mut out).expect("build.rs gzipped it");
    out
}

/// The text of every inline `<script>` (one without `src`), as the CSP hashes it: the bytes
/// between the start tag's `>` and `</script>`.
pub fn inline_scripts(html: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find("<script") {
        let tag_and_after = &rest[i..];
        let Some(end) = tag_and_after.find('>') else { break };
        let tag = &tag_and_after[..end];
        let body = &tag_and_after[end + 1..];
        let Some(close) = body.find("</script>") else { break };
        if !tag.contains(" src=") {
            out.push(&body[..close]);
        }
        rest = &body[close + "</script>".len()..];
    }
    out
}

/// The policy for one page: its inline scripts by hash and this origin's script files, styles
/// and fonts from this origin, data from this node only, never framed.
pub fn page_csp(html: &str) -> String {
    use commonware_cryptography::Hasher as _;
    let mut hashes: Vec<String> = inline_scripts(html).iter().map(|s| format!("'sha256-{}'", crate::api::base64(commonware_cryptography::Sha256::hash(&[s.as_bytes()]).as_ref()))).collect();
    hashes.dedup();
    format!(
        "default-src 'self'; script-src 'self' {}; style-src 'self'; font-src 'self'; img-src 'self' data:; connect-src 'self'; manifest-src 'self'; worker-src 'none'; frame-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'; object-src 'none'",
        hashes.join(" ")
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

impl Wallet {
    /// The embedded wallet, its pages titled with the chain's name.
    pub fn new(chain_name: &str) -> Self {
        Self::from_files(WALLET_FILES, chain_name)
    }

    fn from_files(table: &'static [(&'static str, bool, &'static [u8])], chain_name: &str) -> Self {
        let files: HashMap<_, _> = table.iter().map(|(p, gz, bytes)| (*p, File { gz: *gz, bytes })).collect();
        let title = format!("<title>{} Wallet</title>", html_escape(chain_name));
        let apple_title = format!("name=\"apple-mobile-web-app-title\" content=\"{}\"", html_escape(&home_screen_name(chain_name)));
        let pages = files
            .iter()
            .filter(|(p, _)| p.ends_with(".html"))
            .map(|(p, f)| {
                let raw = if f.gz { gunzip(f.bytes) } else { f.bytes.to_vec() };
                let html = String::from_utf8_lossy(&raw).replacen("<title>Blockula Wallet</title>", &title, 1).replacen("name=\"apple-mobile-web-app-title\" content=\"Blockula\"", &apple_title, 1);
                let csp = page_csp(&html);
                (*p, Page { html: html.into(), csp: csp.into() })
            })
            .collect();
        let dynamic = files.keys().filter_map(|p| p.strip_suffix(&format!("/{PLACEHOLDER}/index.html"))).map(str::to_string).collect();
        let manifest = files.get("manifest.webmanifest").and_then(|f| chain_manifest(&if f.gz { gunzip(f.bytes) } else { f.bytes.to_vec() }, chain_name)).map(Into::into);
        Self { files, pages, dynamic, manifest }
    }

    /// The export file a request path (below `/wallet/`, still percent-encoded) is answered
    /// with, or `None`. `Err` for a path that tries to leave the export.
    pub fn resolve(&self, raw: &str) -> Result<Option<&'static str>, ()> {
        let mut segs = Vec::new();
        for s in raw.split('/') {
            let s = percent_decode(s).ok_or(())?;
            if s == ".." || s == "." || s.contains(['/', '\\', '\0']) {
                return Err(());
            }
            segs.push(s);
        }
        if let Some(found) = self.lookup(&segs) {
            return Ok(Some(found));
        }
        // A dynamic route's id: the placeholder's page, its payloads and segments.
        for k in 1..segs.len() {
            if !segs[k].is_empty() && self.dynamic.contains(&segs[..k].join("/")) {
                let mut mapped = segs.clone();
                mapped[k] = PLACEHOLDER.to_string();
                // An id that looks like a file name (`alice.boo`) gets no trailing slash, so
                // Next.js asks for its payload as `<id>.txt` instead of `<id>/index.txt`.
                if k + 1 == segs.len() && segs[k].ends_with(".txt") {
                    let payload = [mapped.clone(), vec!["index.txt".into()]].concat();
                    if let Some(found) = self.lookup(&payload) {
                        return Ok(Some(found));
                    }
                }
                if let Some(found) = self.lookup(&mapped) {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }

    /// A file at exactly these segments, or the index of that directory.
    fn lookup(&self, segs: &[String]) -> Option<&'static str> {
        let path = segs.join("/");
        let path = path.trim_end_matches('/');
        let candidates = if path.is_empty() { vec!["index.html".to_string()] } else { vec![path.to_string(), format!("{path}/index.html")] };
        candidates.iter().find_map(|c| self.files.get_key_value(c.as_str()).map(|(k, _)| *k))
    }

    /// The response for a request path below `/wallet/`.
    pub fn respond(&self, raw: &str, headers: &HeaderMap) -> Response {
        let file = match self.resolve(raw) {
            Err(()) => return (StatusCode::BAD_REQUEST, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "Bad path").into_response(),
            Ok(Some(f)) => f,
            // A missing asset is just missing; a missing page gets the wallet's own 404 page.
            Ok(None) if raw.starts_with("_next/") || !self.pages.contains_key("404.html") => return (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "Not found").into_response(),
            Ok(None) => return self.page("404.html", StatusCode::NOT_FOUND),
        };
        if self.pages.contains_key(file) {
            return self.page(file, StatusCode::OK);
        }
        if let (true, Some(m)) = (file == "manifest.webmanifest", &self.manifest) {
            let mut res = Response::new(Body::from(m.to_string()));
            let h = res.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/manifest+json"));
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            common_headers(h);
            return res;
        }
        let f = self.files[file];
        let accepts_gzip = headers.get(header::ACCEPT_ENCODING).and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(',').any(|e| e.trim().split(';').next() == Some("gzip")));
        let cache = if file.starts_with("_next/static/") { "public, max-age=31536000, immutable" } else if file.ends_with(".txt") { "no-cache" } else { "public, max-age=3600" };
        let mut res = if f.gz && accepts_gzip {
            let mut r = Response::new(Body::from(f.bytes));
            r.headers_mut().insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
            r
        } else if f.gz {
            Response::new(Body::from(gunzip(f.bytes)))
        } else {
            Response::new(Body::from(f.bytes))
        };
        let h = res.headers_mut();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type(file)));
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
        if f.gz {
            h.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
        }
        common_headers(h);
        res
    }

    fn page(&self, file: &str, status: StatusCode) -> Response {
        let p = &self.pages[file];
        let mut res = (status, p.html.to_string()).into_response();
        let h = res.headers_mut();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
        h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_str(&p.csp).expect("an ASCII policy"));
        // A new build of the node may bring a new wallet: pages are checked every time.
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
        h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        h.insert("cross-origin-opener-policy", HeaderValue::from_static("same-origin"));
        h.insert("permissions-policy", HeaderValue::from_static("publickey-credentials-get=(self), publickey-credentials-create=(self), camera=(self), microphone=(), geolocation=(), payment=(), usb=()"));
        common_headers(h);
        res
    }
}

/// The name under the wallet's icon on a home screen: the chain's, when it's short enough to fit
/// (iOS cuts a label at about 12 characters), else "Blockula".
fn home_screen_name(chain_name: &str) -> String {
    if chain_name.chars().count() <= 12 { chain_name.to_string() } else { "Blockula".to_string() }
}

/// The export's web app manifest (web/app/manifest.ts) named after the chain, as the pages'
/// titles are: Add to Home Screen then offers "<chain> Wallet". Paths in it are relative to the
/// manifest, so they already point under `/wallet/`. `None` if it isn't a JSON object.
fn chain_manifest(raw: &[u8], chain_name: &str) -> Option<String> {
    let mut m: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let o = m.as_object_mut()?;
    o.insert("name".into(), format!("{chain_name} Wallet").into());
    o.insert("short_name".into(), home_screen_name(chain_name).into());
    serde_json::to_string(&m).ok()
}

fn common_headers(h: &mut HeaderMap) {
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
}

/// `%xx` escapes decoded; `None` for a broken escape or bytes that aren't UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The media type for an export file, by its extension.
pub fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        // RSC payloads: a static export's are text/plain (Next.js checks for it).
        "txt" => "text/plain; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    }
}

/// What `/wallet/` answers when this binary was built without the wallet.
const NOT_BUILT: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Wallet not built</title>
<style>body{margin:0;background:#ece3cd;color:#17120f;font:17px/1.55 Georgia,serif}main{max-width:36rem;margin:0 auto;padding:3rem 1rem}h1{font-weight:normal;font-size:1.9rem;margin:0 0 .5rem}code,pre{font-family:ui-monospace,Menlo,monospace;font-size:.85em}pre{background:#17120f;color:#ece3cd;padding:.8rem 1rem;border-radius:6px;overflow-x:auto}a{color:#7a1212}</style>
</head><body><main>
<h1>This node was built without its wallet</h1>
<p>Every <code>blockula</code> release serves a passkey wallet here. This binary was built from source without it, so the explorer works but the wallet doesn't.</p>
<p>To build it in, from a clone of the repository:</p>
<pre>npm ci
npm run build:wallet
cargo install --path node --locked</pre>
<p>Or install a release: <code>curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh</code>. Then restart the node.</p>
<p><a href="/">Back to the explorer</a></p>
</main></body></html>"#;

fn not_built() -> Response {
    (
        StatusCode::NOT_FOUND,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CONTENT_SECURITY_POLICY, "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        NOT_BUILT,
    )
        .into_response()
}

/// `/boo/<name>` (`vlad` or `vlad.boo`): a boo's profile by its name, a link short enough to
/// post (the X proof post carries it: sdk-ts/src/xlink.ts `booProfileUrl`). It goes on to the
/// name's profile in the wallet (web/app/boo), or to the account in the explorer when this binary
/// has no wallet. Anything that isn't a name is a 404.
fn boo(name: &str) -> Response {
    let Ok(label) = crate::types::normalize_name(name) else {
        return (StatusCode::NOT_FOUND, "no such page: /boo/<name> takes a .boo name\n").into_response();
    };
    let to = if embedded() { format!("{BASE}/boo/{label}/") } else { format!("/#/account/{label}{}", crate::types::NAME_SUFFIX) };
    Redirect::temporary(&to).into_response()
}

/// `/wallet`, `/wallet/`, everything below, and `/boo/<name>`.
pub fn router(chain_name: &str) -> Router {
    let wallet = embedded().then(|| Arc::new(Wallet::new(chain_name)));
    let serve = move |uri: Uri, headers: HeaderMap| {
        let wallet = wallet.clone();
        async move {
            match wallet {
                None => not_built(),
                Some(w) => w.respond(uri.path().strip_prefix(&format!("{BASE}/")).unwrap_or(""), &headers),
            }
        }
    };
    Router::new()
        .route("/boo/{name}", get(|axum::extract::Path(name): axum::extract::Path<String>| async move { boo(&name) }))
        .route(BASE, get(|| async { Redirect::permanent(&format!("{BASE}/")) }))
        .route(&format!("{BASE}/"), get(serve.clone()))
        .route(&format!("{BASE}/{{*path}}"), get(serve))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = r#"<!DOCTYPE html><html><head><title>Blockula Wallet</title><script src="/wallet/_next/static/chunks/a.js" async=""></script></head><body><script>(self.__next_f=self.__next_f||[]).push([0])</script><script id="_R_">self.x=1</script><script>self.__next_f.push([1,"a"])</script></body></html>"#;
    const PAGE_404: &str = "<!DOCTYPE html><html><head><title>Blockula Wallet</title></head><body>gone<script>self.y=2</script></body></html>";

    fn gz(s: &str) -> &'static [u8] {
        use std::io::Write;
        let mut z = flate2::GzBuilder::new().mtime(0).write(Vec::new(), flate2::Compression::best());
        z.write_all(s.as_bytes()).unwrap();
        Box::leak(z.finish().unwrap().into_boxed_slice())
    }

    fn table() -> &'static [(&'static str, bool, &'static [u8])] {
        Box::leak(
            vec![
                ("404.html", true, gz(PAGE_404)),
                ("_next/static/chunks/a.js", true, gz("console.log(1)")),
                ("_next/static/media/f.woff2", false, &b"wOF2"[..]),
                ("index.html", true, gz(INDEX)),
                ("send/index.html", true, gz(INDEX)),
                ("tx/_/index.html", true, gz(INDEX)),
                ("tx/_/index.txt", true, gz("1:payload")),
                ("tx/_/__next.tx.$d$id.__PAGE__.txt", true, gz("2:segment")),
                ("programs/index.html", true, gz(INDEX)),
                ("programs/_/index.html", true, gz(INDEX)),
                ("manifest.webmanifest", true, gz(r#"{"name":"Blockula Wallet","short_name":"Blockula","start_url":"./","scope":"./","display":"standalone","icons":[{"src":"icon-192.png","sizes":"192x192","type":"image/png"}]}"#)),
                ("apple-icon.png", false, &b"\x89PNG\r\n\x1a\n"[..]),
            ]
            .into_boxed_slice(),
        )
    }

    fn wallet() -> Wallet {
        Wallet::from_files(table(), "Jack's <Chain>")
    }

    #[test]
    fn resolves_pages_assets_and_placeholder_routes() {
        let w = wallet();
        let r = |p: &str| w.resolve(p).unwrap();
        assert_eq!(r(""), Some("index.html"));
        assert_eq!(r("send"), Some("send/index.html"));
        assert_eq!(r("send/"), Some("send/index.html"));
        assert_eq!(r("_next/static/chunks/a.js"), Some("_next/static/chunks/a.js"));
        // Any id is the placeholder's page, payload and segments.
        assert_eq!(r("tx/ab12/"), Some("tx/_/index.html"));
        assert_eq!(r("tx/ab12"), Some("tx/_/index.html"));
        assert_eq!(r("tx/ab12/index.txt"), Some("tx/_/index.txt"));
        assert_eq!(r("tx/ab12/__next.tx.$d$id.__PAGE__.txt"), Some("tx/_/__next.tx.$d$id.__PAGE__.txt"));
        assert_eq!(r("tx/ab12/__next.tx.%24d%24id.__PAGE__.txt"), Some("tx/_/__next.tx.$d$id.__PAGE__.txt"), "escaped as the browser may");
        assert_eq!(r("tx/_/"), Some("tx/_/index.html"));
        // A name looks like a file to Next.js: no trailing slash, and its payload is `<id>.txt`.
        assert_eq!(r("tx/alice.boo"), Some("tx/_/index.html"));
        assert_eq!(r("tx/alice.boo.txt"), Some("tx/_/index.txt"));
        assert_eq!(r("tx/alice.boo/__next.tx.$d$id.__PAGE__.txt"), Some("tx/_/__next.tx.$d$id.__PAGE__.txt"));
        // A static page beside a dynamic route keeps its own.
        assert_eq!(r("programs/"), Some("programs/index.html"));
        assert_eq!(r("programs/transfer/"), Some("programs/_/index.html"));
        assert_eq!(r("tx/"), None, "a route's directory isn't a page");
        assert_eq!(r("tx/a/b/c"), None);
        assert_eq!(r("nope/"), None);
        assert_eq!(r("send/x"), None, "only dynamic routes take ids");
    }

    #[test]
    fn refuses_paths_out_of_the_export() {
        let w = wallet();
        for p in ["../Cargo.toml", "tx/../../etc/passwd", "%2e%2e/secret", "tx/..%2f..%2fx", "a\\b", "./index.html", "%zz", "%ff%fe"] {
            assert!(w.resolve(p).is_err(), "{p}");
        }
        let res = w.respond("../../etc/passwd", &HeaderMap::new());
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn finds_every_inline_script() {
        assert_eq!(inline_scripts(INDEX), vec!["(self.__next_f=self.__next_f||[]).push([0])", "self.x=1", "self.__next_f.push([1,\"a\"])"]);
        assert_eq!(inline_scripts("<script>a</script><script src=x></script><script>b"), vec!["a"], "an unclosed one isn't served as script");
    }

    #[tokio::test]
    async fn pages_carry_a_hash_csp_for_their_own_inline_scripts() {
        use commonware_cryptography::Hasher as _;
        let w = wallet();
        let res = w.respond("tx/deadbeef/", &HeaderMap::new());
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CONTENT_TYPE], "text/html; charset=utf-8");
        let csp = res.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap().to_string();
        let html = String::from_utf8(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap();
        assert!(html.contains("<title>Jack&#39;s &lt;Chain&gt; Wallet</title>"), "the chain's name, escaped");
        let scripts = inline_scripts(&html);
        assert_eq!(scripts.len(), 3);
        for s in scripts {
            let h = crate::api::base64(commonware_cryptography::Sha256::hash(&[s.as_bytes()]).as_ref());
            assert!(csp.contains(&format!("'sha256-{h}'")), "{s} is allowed by hash");
        }
        let script_src = csp.split("; ").find(|d| d.starts_with("script-src")).unwrap();
        assert!(script_src.starts_with("script-src 'self' 'sha256-"), "{script_src}");
        assert!(!csp.contains("unsafe-inline") && !csp.contains("unsafe-eval") && !csp.contains("strict-dynamic"), "{csp}");
        for d in ["connect-src 'self'", "frame-ancestors 'none'", "object-src 'none'", "base-uri 'none'", "style-src 'self'"] {
            assert!(csp.contains(d), "{d} in {csp}");
        }
        // The 404 page has its own script, and its own hash.
        let res = w.respond("nowhere/", &HeaderMap::new());
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let csp404 = res.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        let h = crate::api::base64(commonware_cryptography::Sha256::hash(&[b"self.y=2"]).as_ref());
        assert!(csp404.contains(&h) && !csp.contains(&h));
    }

    #[tokio::test]
    async fn assets_have_types_caching_and_gzip() {
        let w = wallet();
        let mut gzip = HeaderMap::new();
        gzip.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("br, gzip;q=0.8"));
        let res = w.respond("_next/static/chunks/a.js", &gzip);
        assert_eq!(res.headers()[header::CONTENT_TYPE], "text/javascript; charset=utf-8");
        assert_eq!(res.headers()[header::CONTENT_ENCODING], "gzip");
        assert_eq!(res.headers()[header::CACHE_CONTROL], "public, max-age=31536000, immutable");
        assert_eq!(res.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(gunzip(&body), b"console.log(1)");
        // Without gzip, the plain bytes.
        let res = w.respond("_next/static/chunks/a.js", &HeaderMap::new());
        assert!(res.headers().get(header::CONTENT_ENCODING).is_none());
        assert_eq!(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()[..], b"console.log(1)");
        let res = w.respond("tx/ff/index.txt", &gzip);
        assert_eq!(res.headers()[header::CONTENT_TYPE], "text/plain; charset=utf-8", "RSC payloads as a static export's");
        assert_eq!(res.headers()[header::CACHE_CONTROL], "no-cache");
        let res = w.respond("_next/static/media/f.woff2", &gzip);
        assert_eq!(res.headers()[header::CONTENT_TYPE], "font/woff2");
        assert!(res.headers().get(header::CONTENT_ENCODING).is_none(), "fonts are stored as they are");
        let res = w.respond("_next/static/chunks/missing.js", &gzip);
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(res.headers()[header::CONTENT_TYPE], "text/plain; charset=utf-8");
        for (p, t) in [("a.css", "text/css; charset=utf-8"), ("i.svg", "image/svg+xml"), ("x.bin", "application/octet-stream"), ("m.webmanifest", "application/manifest+json")] {
            assert_eq!(content_type(p), t);
        }
    }

    /// Add to Home Screen: the manifest is the chain's, its paths stay relative (under
    /// `/wallet/`), and the touch icon is a PNG.
    #[tokio::test]
    async fn the_manifest_and_touch_icon_are_the_chains() {
        let w = wallet();
        let res = w.respond("manifest.webmanifest", &HeaderMap::new());
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CONTENT_TYPE], "application/manifest+json");
        let m: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert_eq!(m["name"], "Jack's <Chain> Wallet", "JSON, not HTML: nothing to escape");
        assert_eq!(m["short_name"], "Blockula", "too long for a home screen");
        assert_eq!((m["start_url"].as_str(), m["scope"].as_str(), m["display"].as_str()), (Some("./"), Some("./"), Some("standalone")));
        assert_eq!(m["icons"][0]["src"], "icon-192.png");
        let res = w.respond("apple-icon.png", &HeaderMap::new());
        assert_eq!(res.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(home_screen_name("DISHNET"), "DISHNET");
        let page = br#"<head><title>Blockula Wallet</title><meta name="apple-mobile-web-app-title" content="Blockula"/></head>"#;
        let html = Wallet::from_files(Box::leak(vec![("index.html", false, &page[..])].into_boxed_slice()), "DISHNET").pages["index.html"].html.to_string();
        assert!(html.contains(r#"<meta name="apple-mobile-web-app-title" content="DISHNET"/>"#), "{html}");
    }

    #[tokio::test]
    async fn without_the_export_it_says_how_to_get_it() {
        let res = not_built();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let csp = res.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap().to_string();
        assert!(csp.starts_with("default-src 'none'") && !csp.contains("script-src"), "no scripts at all");
        let html = String::from_utf8(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap();
        assert!(html.contains("npm run build:wallet") && html.contains("install.sh") && !html.contains("<script"));
    }

    #[tokio::test]
    async fn the_router_serves_the_wallet_or_the_note() {
        use tower::ServiceExt;
        let app = router("Test chain");
        let res = app.clone().oneshot(axum::http::Request::builder().uri("/wallet").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(res.headers()[header::LOCATION], "/wallet/");
        // /boo/<name> goes on to the name's profile; nothing that isn't a name does.
        for uri in ["/boo/vlad", "/boo/Vlad.boo"] {
            let res = app.clone().oneshot(axum::http::Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(res.status(), StatusCode::TEMPORARY_REDIRECT);
            assert_eq!(res.headers()[header::LOCATION], if embedded() { "/wallet/boo/vlad/" } else { "/#/account/vlad.boo" });
        }
        for uri in ["/boo/v", "/boo/-vlad", "/boo/vl%2Fad", "/boo/vlad%0d%0aset-cookie", "/boo/vlad/more", "/boo/"] {
            let res = app.clone().oneshot(axum::http::Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
        }
        let res = app.oneshot(axum::http::Request::builder().uri("/wallet/tx/abc/").body(Body::empty()).unwrap()).await.unwrap();
        // Whichever this binary is: the wallet's page, or the note on how to build it in.
        let csp = res.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap().to_string();
        if embedded() {
            assert_eq!(res.status(), StatusCode::OK);
            assert!(csp.contains("script-src 'self' 'sha256-"));
            let html = String::from_utf8(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap();
            assert!(html.contains("<title>Test chain Wallet</title>"), "the export's title is the one webapp replaces");
            assert!(!html.contains(" style=\""), "no inline style attributes: style-src is 'self'");
        } else {
            assert_eq!(res.status(), StatusCode::NOT_FOUND);
            assert!(csp.starts_with("default-src 'none'"));
        }
    }

    #[test]
    fn the_embedded_export_has_its_placeholder_routes() {
        if !embedded() {
            return;
        }
        let w = Wallet::new("x");
        for route in ["tx", "account", "block", "name", "boo", "hunt", "claim", "seance", "program", "programs", "trace"] {
            assert!(w.dynamic.contains(route), "{route}/_/index.html is in the export");
            let want = format!("{route}/_/index.html");
            assert_eq!(w.resolve(&format!("{route}/something/")).unwrap(), Some(want.as_str()));
        }
        assert!(w.pages.contains_key("welcome/index.html") && w.pages.contains_key("404.html"));
    }
}
