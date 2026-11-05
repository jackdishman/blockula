//! What makes a node's explorer the chain's own app: the page under the chain's name, a web app
//! manifest and home-screen icons (Safari's Add to Dock on a Mac, Add to Home Screen on an
//! iPhone or iPad), and amounts in the chain's ticker.
//!
//! A chain summoned with `--token` still counts in drops, and the node's text names the unit
//! FANG (some of it, failed transactions' errors, is part of the chain's history). On such a chain
//! the API relabels what it shows people: an amount ("12.5 FANG") in a `summary`, an `error` or a
//! `*_display` field reads in the chain's ticker ("12.5 JACK"). Nothing it signs, hashes or stores
//! changes, and on a FANG chain nothing is touched.

use crate::chain::{Genesis, DEFAULT_TOKEN};
use serde_json::{json, Value};

/// The palette of the explorer and the wallet: ink and paper.
const INK: [u8; 3] = [0x17, 0x12, 0x0f];
const PAPER: [u8; 3] = [0xec, 0xe3, 0xcd];

/// The chain's name for people: its `--name`, else its id.
pub fn chain_name(g: &Genesis) -> String {
    g.brand.as_ref().map(|b| b.name.clone()).unwrap_or_else(|| g.chain_id.clone())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// The explorer page for this chain: its title and home-screen name are the chain's. Only the
/// head changes, so the CSP's hash of the inline script still holds.
pub fn explorer_page(template: &str, g: &Genesis) -> String {
    let name = html_escape(&chain_name(g));
    let head = format!(
        "<title>{name} Explorer</title>\n<link rel=\"manifest\" href=\"/manifest.webmanifest\" />\n<link rel=\"apple-touch-icon\" href=\"/icon-180.png\" />\n<meta name=\"apple-mobile-web-app-title\" content=\"{name}\" />\n<meta name=\"apple-mobile-web-app-capable\" content=\"yes\" />\n<meta name=\"mobile-web-app-capable\" content=\"yes\" />\n<meta name=\"theme-color\" content=\"#17120f\" />"
    );
    template.replacen("<title>Blockula Explorer</title>", &head, 1)
}

/// `/manifest.webmanifest`: installs the explorer as an app named after the chain.
pub fn manifest(g: &Genesis) -> Value {
    let name = chain_name(g);
    let short: String = if name.chars().count() <= 12 { name.clone() } else { g.token.clone() };
    json!({
        "name": name,
        "short_name": short,
        "description": g.brand.as_ref().map(|b| b.description.as_str()).filter(|d| !d.is_empty()).unwrap_or("A quantum-safe ledger, made with Blockula."),
        "start_url": "/",
        "scope": "/",
        "display": "standalone",
        "background_color": "#ece3cd",
        "theme_color": "#17120f",
        "icons": [
            { "src": "/icon-192.png", "sizes": "192x192", "type": "image/png" },
            { "src": "/icon-512.png", "sizes": "512x512", "type": "image/png" },
        ],
    })
}

/// One wing of the bat (the favicon's path, on a 120-unit grid), as points: lines and cubic
/// Béziers from the start.
const WING_START: (f64, f64) = (60.0, 21.0);
enum Seg {
    L(f64, f64),
    C(f64, f64, f64, f64, f64, f64),
}
const WING: [Seg; 9] = [
    Seg::L(62.5, 20.5),
    Seg::L(65.0, 11.0),
    Seg::L(67.5, 20.5),
    Seg::C(72.0, 21.0, 76.0, 19.5, 80.0, 16.5),
    Seg::C(90.0, 9.5, 103.0, 6.0, 118.0, 5.5),
    Seg::C(113.5, 10.5, 110.5, 16.0, 109.5, 24.0),
    Seg::C(104.5, 21.0, 99.5, 22.0, 96.0, 28.5),
    Seg::C(91.0, 25.5, 85.0, 27.0, 81.5, 34.0),
    Seg::C(76.5, 32.0, 71.5, 35.0, 68.5, 42.5),
];
const WING_END: [Seg; 1] = [Seg::C(65.5, 41.5, 62.5, 43.5, 60.0, 48.0)];

/// The wing as a closed polygon on the icon's 100-unit canvas (the favicon's
/// `translate(11 31) scale(.65)`), mirrored for the left wing.
fn wing(mirror: bool) -> Vec<(f64, f64)> {
    let place = |(x, y): (f64, f64)| (11.0 + 0.65 * if mirror { 120.0 - x } else { x }, 31.0 + 0.65 * y);
    let mut pts = vec![WING_START];
    for s in WING.iter().chain(WING_END.iter()) {
        let p0 = *pts.last().unwrap();
        match *s {
            Seg::L(x, y) => pts.push((x, y)),
            Seg::C(x1, y1, x2, y2, x, y) => {
                for i in 1..=16 {
                    let t = i as f64 / 16.0;
                    let u = 1.0 - t;
                    pts.push((u * u * u * p0.0 + 3.0 * u * u * t * x1 + 3.0 * u * t * t * x2 + t * t * t * x, u * u * u * p0.1 + 3.0 * u * u * t * y1 + 3.0 * u * t * t * y2 + t * t * t * y));
                }
            }
        }
    }
    pts.into_iter().map(place).collect()
}

/// Even-odd: is (x, y) inside the polygon?
fn inside(poly: &[(f64, f64)], x: f64, y: f64) -> bool {
    let mut c = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let ((xi, yi), (xj, yj)) = (poly[i], poly[j]);
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            c = !c;
        }
        j = i;
    }
    c
}

fn crc32(chunks: &[&[u8]]) -> u32 {
    let mut c = !0u32;
    for b in chunks.iter().flat_map(|c| c.iter()) {
        c ^= *b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    out.extend(kind);
    out.extend(data);
    out.extend(crc32(&[kind, data]).to_be_bytes());
}

/// The bat on ink, `size` pixels square, as a PNG (2-colour palette, 4× supersampled edges
/// rounded to the nearer colour).
pub fn icon_png(size: u32) -> Vec<u8> {
    use std::io::Write;
    let wings = [wing(false), wing(true)];
    let row_bytes = (size as usize).div_ceil(8);
    let mut raw = Vec::with_capacity((row_bytes + 1) * size as usize);
    for py in 0..size {
        raw.push(0); // filter: none
        let mut row = vec![0u8; row_bytes];
        for px in 0..size {
            let mut hits = 0;
            for (sx, sy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let (x, y) = ((px as f64 + sx) * 100.0 / size as f64, (py as f64 + sy) * 100.0 / size as f64);
                hits += wings.iter().any(|w| inside(w, x, y)) as u32;
            }
            if hits >= 2 {
                row[px as usize / 8] |= 0x80 >> (px % 8);
            }
        }
        raw.extend(row);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    z.write_all(&raw).expect("in memory");
    let idat = z.finish().expect("in memory");
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = size.to_be_bytes().to_vec();
    ihdr.extend(size.to_be_bytes());
    ihdr.extend([1, 3, 0, 0, 0]); // 1 bit per pixel, palette, deflate, no filter, no interlace
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"PLTE", &[INK, PAPER].concat());
    png_chunk(&mut out, b"IDAT", &idat);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

/// "12.5 FANG" → "12.5 JACK": every amount (digits, then " FANG", then not a letter or digit).
pub fn relabel(s: &str, token: &str) -> String {
    const UNIT: &str = " FANG";
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(UNIT) {
        let after = &rest[i + UNIT.len()..];
        let amount = rest[..i].ends_with(|c: char| c.is_ascii_digit()) && !after.starts_with(|c: char| c.is_alphanumeric() || c == '_');
        out.push_str(&rest[..i]);
        if amount {
            out.push(' ');
            out.push_str(token);
        } else {
            out.push_str(UNIT);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Relabels the amounts people read in a response: in `summary`, `error`, `does`, `reason` and
/// every `*_display` / `display` field, at any depth. Other text (memos, titles, names, untrusted
/// program data) is left as it was.
pub fn relabel_json(v: &mut Value, token: &str) {
    match v {
        Value::Object(m) => {
            for (k, v) in m.iter_mut() {
                let shown = k.ends_with("_display") || matches!(k.as_str(), "display" | "summary" | "error" | "does" | "reason" | "outcome");
                match v {
                    Value::String(s) if shown => *s = relabel(s, token),
                    // `*_display` lists (a validator's history, a tally's lines).
                    Value::Array(a) if shown => a.iter_mut().for_each(|x| {
                        if let Value::String(s) = x {
                            *s = relabel(s, token)
                        } else {
                            relabel_json(x, token)
                        }
                    }),
                    _ => relabel_json(v, token),
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| relabel_json(x, token)),
        _ => {}
    }
}

/// Whether this chain's amounts need relabeling at all.
pub fn needs_relabel(g: &Genesis) -> bool {
    g.token != DEFAULT_TOKEN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relabels_amounts_only() {
        assert_eq!(relabel("Sent 12.5 FANG to alice.boo", "JACK"), "Sent 12.5 JACK to alice.boo");
        assert_eq!(relabel("has 0 FANG, needs 1,000 FANG (including the fee)", "JACK"), "has 0 JACK, needs 1,000 JACK (including the fee)");
        assert_eq!(relabel("invalid FANG amount", "JACK"), "invalid FANG amount", "the word alone stays");
        assert_eq!(relabel("5 FANGS", "JACK"), "5 FANGS");
        assert_eq!(relabel("5 FANG_X 6 FANG.", "JACK"), "5 FANG_X 6 JACK.");
        assert_eq!(relabel("🦇 7 FANG", "JACK"), "🦇 7 JACK");
        let mut v = json!({
            "summary": "alice sent 5 FANG",
            "memo": "pay me 5 FANG",
            "balance_display": "100 FANG",
            "tx": { "fee_display": "0.0001 FANG", "title": "Give 9 FANG" },
            "lines_display": ["1 FANG", { "x_display": "2 FANG" }],
            "untrusted_message": "3 FANG",
        });
        relabel_json(&mut v, "JACK");
        assert_eq!(v, json!({
            "summary": "alice sent 5 JACK",
            "memo": "pay me 5 FANG",
            "balance_display": "100 JACK",
            "tx": { "fee_display": "0.0001 JACK", "title": "Give 9 FANG" },
            "lines_display": ["1 JACK", { "x_display": "2 JACK" }],
            "untrusted_message": "3 FANG",
        }));
    }

    #[test]
    fn icons_are_pngs_of_the_bat() {
        for size in [180, 192, 512] {
            let png = icon_png(size);
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
            assert_eq!(&png[16..24], [size.to_be_bytes(), size.to_be_bytes()].concat().as_slice());
            assert!(png.len() < 20_000, "{size}: {} bytes", png.len());
        }
        // The middle of the body is bat; the corners are ink.
        let (l, r) = (wing(true), wing(false));
        assert!(inside(&r, 52.0, 50.0) && inside(&l, 48.0, 50.0));
        assert!(!inside(&r, 2.0, 2.0) && !inside(&l, 98.0, 98.0));
        assert_eq!(crc32(&[b"IEND"]), 0xAE42_6082, "the IEND chunk's well-known CRC");
    }
}
