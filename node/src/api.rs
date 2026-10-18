//! The HTTP API. Reads come from the view the chain publishes after each block (`view.rs`):
//! no read handler takes the consensus lock, and none scans the state (lookups go through the
//! store's indexes, rankings are computed once per block). Writes (submitting a tx, the faucet)
//! take the lock briefly. Every route has a timeout; reads are rate limited per client
//! (token bucket), writes by their own windows.

use crate::chain::{short, Chain, Role};
use crate::consensus::MarshalMailbox;
use crate::crypto;
use crate::store::{ix, BlockMeta};
use crate::types::*;
use crate::view::{hunt_status, Live, ReadView};
use axum::{
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub type Shared = Arc<Mutex<Chain>>;

/// Everything the HTTP handlers need.
#[derive(Clone)]
pub struct Node {
    /// The chain, for writes (submit, faucet). Reads use `live`.
    pub chain: Shared,
    /// What the chain published: the latest executed block's view, the mempool, the epochs.
    pub live: Arc<Live>,
    /// Validators: marshal, to serve finalized blocks + certificates to joining nodes.
    pub marshal: Option<MarshalMailbox>,
    /// Full nodes (and validators without a faucet account): validator HTTP endpoints that txs
    /// and faucet requests are forwarded to.
    pub peers: Vec<String>,
    pub client_ip: ClientIpPolicy,
    limits: Arc<Limits>,
    reads: Arc<ReadLimiter>,
    /// Longest any request may take.
    pub timeout: Duration,
    pub stats: Arc<Stats>,
    github_cache: Arc<Mutex<HashMap<String, (Instant, Value)>>>,
    /// Extra Prometheus text (e.g. Commonware's runtime/p2p metrics on validators).
    pub extra_metrics: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    /// Height of the newest checkpoint this node trusts (`checkpoints::Trust::newest`; 0: genesis).
    pub checkpoint_height: u64,
    /// Why the node stopped executing, if it did (upgrade.rs): /api/health and /metrics say so.
    pub halt: crate::upgrade::HaltSignal,
    /// Validators: slashing protection's state, for /metrics.
    pub protection: Option<Arc<crate::protection::Protection>>,
    /// Validators: connected p2p peers, for /metrics.
    pub p2p_peers: Option<Arc<dyn Fn() -> Option<usize> + Send + Sync>>,
    /// This node's validator address (its key's), if it has one: /metrics marks its series `own="1"`.
    pub me: Option<String>,
}

/// When this process started (unix seconds), for `blockula_process_start_time_seconds`.
static STARTED: std::sync::OnceLock<f64> = std::sync::OnceLock::new();

/// Records the process start time (`main` calls it first thing).
pub fn mark_process_start() {
    STARTED.get_or_init(|| crate::chain::now_ms() as f64 / 1000.0);
}

/// Connected p2p peers, from the runtime's Prometheus text: the series of the authenticated
/// p2p tracker's `connected` family (`network_tracker_directory_connected`, one per peer; the
/// network runs under the `network` label, node.rs).
pub fn connected_peers(runtime_metrics: &str) -> Option<usize> {
    let is_family = |name: &str| name.starts_with("network_tracker") && name.ends_with("_connected");
    let mut found = false;
    let mut n = 0;
    for line in runtime_metrics.lines() {
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            found |= rest.split(' ').next().is_some_and(is_family);
        } else if !line.starts_with('#') && is_family(line.split(['{', ' ']).next().unwrap_or_default()) {
            found = true;
            n += 1;
        }
    }
    found.then_some(n)
}

/// Counters for /metrics.
#[derive(Default)]
pub struct Stats {
    pub tx_submitted: std::sync::atomic::AtomicU64,
    pub tx_rejected: std::sync::atomic::AtomicU64,
    pub faucet_drips: std::sync::atomic::AtomicU64,
    pub reads_limited: std::sync::atomic::AtomicU64,
}

fn bump(c: &std::sync::atomic::AtomicU64) {
    c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Rate-limit key for an IP: IPv6 clients are limited per /64 (one allocation), IPv4 per address.
pub fn ip_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
    }
}

/// How to find a caller's IP for rate limits. Proxy headers are client-controlled unless a
/// trusted proxy overwrites them, so each source must be enabled explicitly.
#[derive(Clone, Default)]
pub struct ClientIpPolicy {
    /// Socket peers whose `X-Blockula-Client-IP` header is trusted (e.g. a full node on a LAN).
    pub trusted_forwarders: Vec<IpAddr>,
    /// Shared secret a forwarding full node sends in `X-Blockula-Forwarder-Key`; proves the
    /// forwarder when its IP isn't stable (e.g. on Fly). Also sent by `forward()`.
    pub forwarder_secret: Option<String>,
    /// Header set by the reverse proxy in front of this node (`fly-client-ip`, `x-real-ip`,
    /// `x-forwarded-for`). Only enable it when every request comes through that proxy.
    pub header: Option<String>,
    /// Trusted proxies that append to `header` (`--client-ip-hops`, default 1). Appending proxies
    /// (`X-Forwarded-For`) keep what the client sent first and add the address they saw last,
    /// so the client is this many entries from the right, counted over all of the header's
    /// lines in order (a proxy may add a line of its own); anything left of it is the client's
    /// own say. Headers a proxy overwrites have one entry.
    pub hops: usize,
}

impl std::fmt::Debug for ClientIpPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientIpPolicy")
            .field("trusted_forwarders", &self.trusted_forwarders)
            .field("forwarder_secret", &self.forwarder_secret.as_ref().map(|_| "<redacted>"))
            .field("header", &self.header)
            .field("hops", &self.hops)
            .finish()
    }
}

impl ClientIpPolicy {
    /// The client's IP, or `None` for a local caller (loopback, nothing configured speaks for it).
    pub fn resolve(&self, peer: SocketAddr, h: &HeaderMap) -> Option<IpAddr> {
        // The entry `hops` from the right of a (possibly comma-separated) header (F29: the
        // leftmost one is whatever the client sent), counted over every line of it in order: a
        // proxy may append its own line (HAProxy's `option forwardfor`) after the client's
        // instead of extending it, and the first line alone is the client's say. A line that
        // isn't text is still split on commas (only the proxy's entries need to parse).
        let header_ip = |name: &str, hops: usize| {
            let lines: Vec<String> = h.get_all(name).iter().map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned()).collect();
            let entries: Vec<&str> = lines.iter().flat_map(|l| l.split(',')).collect();
            entries.len().checked_sub(hops.max(1)).and_then(|i| entries[i].trim().parse::<IpAddr>().ok())
        };
        let policy = self;
        // A node bound to `::` sees IPv4 peers as IPv4-mapped IPv6 (`::ffff:127.0.0.1`): the
        // same caller, so local callers stay local and trusted forwarders stay trusted.
        let ip = peer.ip().to_canonical();
        let by_secret = match (&policy.forwarder_secret, h.get("x-blockula-forwarder-key")) {
            (Some(secret), Some(given)) => ct_eq(secret.as_bytes(), given.as_bytes()),
            _ => false,
        };
        if by_secret || policy.trusted_forwarders.contains(&ip) {
            if let Some(ip) = header_ip("x-blockula-client-ip", 1) {
                return Some(ip);
            }
        }
        if let Some(name) = policy.header.as_deref() {
            // Configured but missing or unreadable: limited as the proxy itself, never as a
            // local caller (which isn't limited at all).
            return Some(header_ip(name, policy.hops).unwrap_or(ip));
        }
        if ip.is_loopback() { None } else { Some(ip) }
    }
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Sliding-window rate limits, in memory.
#[derive(Default)]
struct Limits {
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Limits {
    /// Gives back a hit [`Limits::allow`] recorded at `at` (the action it reserved failed).
    fn refund(&self, rules: &[(String, usize, Duration)], at: Instant) {
        let mut hits = self.hits.lock();
        for (key, _, _) in rules {
            if let Some(q) = hits.get_mut(key) {
                if let Some(i) = q.iter().rposition(|t| *t == at) {
                    q.remove(i);
                }
            }
        }
    }

    /// Allows only if every `(key, max, window)` has room; then records a hit on each, in the
    /// same critical section (so concurrent callers can't all pass before any records: F28).
    /// Returns the hit's time, for a [`Limits::refund`].
    fn allow(&self, rules: &[(String, usize, Duration)]) -> Result<Instant, Duration> {
        let now = Instant::now();
        let mut hits = self.hits.lock();
        if hits.len() > 100_000 {
            hits.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < Duration::from_secs(3600)));
        }
        for (key, max, window) in rules {
            let q = hits.entry(key.clone()).or_default();
            while q.front().is_some_and(|t| now.duration_since(*t) >= *window) {
                q.pop_front();
            }
            if q.len() >= *max {
                return Err(*window - now.duration_since(*q.front().unwrap()));
            }
        }
        for (key, _, _) in rules {
            hits.get_mut(key).unwrap().push_back(now);
        }
        Ok(now)
    }
}

/// Per-client read budget: a token bucket refilled at `rps` per second, holding up to `burst`.
pub struct ReadLimiter {
    rps: u32,
    burst: u32,
    buckets: Mutex<HashMap<String, (u64, Instant)>>,
}

/// Default read limits per client IP (IPv6: per /64): generous for an explorer or a wallet, not
/// for a scraper hammering the node. `--read-rps 0` turns them off.
pub const READ_RPS: u32 = 50;
pub const READ_BURST: u32 = 200;
/// Default request timeout (`--http-timeout-secs`).
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

impl ReadLimiter {
    pub fn new(rps: u32, burst: u32) -> Self {
        Self { rps, burst: burst.max(1), buckets: Mutex::new(HashMap::new()) }
    }

    /// Takes a token for `key`, or says how long until one is there. Tokens are counted in
    /// thousandths, so refills are exact integers.
    fn allow(&self, key: &str) -> Result<(), Duration> {
        if self.rps == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let full = self.burst as u64 * 1000;
        let mut buckets = self.buckets.lock();
        if buckets.len() > 100_000 {
            // Idle buckets are full again anyway.
            let refill = Duration::from_millis(full / self.rps as u64 + 1);
            buckets.retain(|_, (_, at)| now.duration_since(*at) < refill);
        }
        let (tokens, at) = buckets.entry(key.to_string()).or_insert((full, now));
        let earned = (now.duration_since(*at).as_millis() as u64).saturating_mul(self.rps as u64);
        *tokens = tokens.saturating_add(earned).min(full);
        *at = now;
        if *tokens >= 1000 {
            *tokens -= 1000;
            Ok(())
        } else {
            Err(Duration::from_millis((1000 - *tokens).div_ceil(self.rps as u64)))
        }
    }
}

/// Rate-limits GETs per client (writes have their own limits). Loopback callers that no trusted
/// proxy speaks for aren't limited.
async fn limit_reads(State(n): State<Node>, req: Request, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD) {
        let ext = req.extensions();
        let peer = ext.get::<ConnectInfo<SocketAddr>>().map(|c| c.0).or_else(|| ext.get::<axum::extract::connect_info::MockConnectInfo<SocketAddr>>().map(|c| c.0));
        if let Some(ip) = peer.and_then(|p| n.client_ip(p, req.headers())) {
            if let Err(wait) = n.reads.allow(&ip_key(ip)) {
                bump(&n.stats.reads_limited);
                let secs = wait.as_secs().max(1).to_string();
                return (StatusCode::TOO_MANY_REQUESTS, [(axum::http::header::RETRY_AFTER, secs)], Json(json!({ "error": "too many requests from your IP; slow down" }))).into_response();
            }
        }
    }
    next.run(req).await
}

pub const TX_PER_IP_PER_MINUTE: usize = 120;
pub const FAUCET_IP_WINDOW: Duration = Duration::from_secs(600);
pub const FAUCET_ADDRESS_WINDOW: Duration = Duration::from_secs(3600);
pub const MAX_BODY_BYTES: usize = 128 * 1024;
/// `POST /api/tx` takes more: a ghost tx (up to `MAX_GHOST_TX_BYTES`), as wire bytes or as hex
/// JSON (twice the size), with room for the JSON around it.
pub const MAX_TX_BODY_BYTES: usize = 2 * MAX_GHOST_TX_BYTES + 64 * 1024;

fn too_many(what: &str, wait: Duration) -> ApiError {
    let mins = wait.as_secs().div_ceil(60);
    ApiError(StatusCode::TOO_MANY_REQUESTS, format!("{what}: try again in {mins} minute{}", if mins == 1 { "" } else { "s" }))
}

impl Node {
    pub fn new(chain: Shared, marshal: Option<MarshalMailbox>, peers: Vec<String>, client_ip: ClientIpPolicy) -> Self {
        let (live, halt, me) = {
            let c = chain.lock();
            (c.live.clone(), c.halt.signal.clone(), c.me())
        };
        Self { chain, live, marshal, peers, client_ip, limits: Default::default(), reads: Arc::new(ReadLimiter::new(READ_RPS, READ_BURST)), timeout: HTTP_TIMEOUT, stats: Default::default(), extra_metrics: None, github_cache: Default::default(), checkpoint_height: 0, halt, protection: None, p2p_peers: None, me }
    }

    /// Per-client read limits: `rps` per second, bursts of `burst` (`rps` 0: unlimited).
    pub fn with_read_limits(mut self, rps: u32, burst: u32) -> Self {
        self.reads = Arc::new(ReadLimiter::new(rps, burst));
        self
    }

    /// The client's IP for rate limiting, or `None` for local (loopback, un-proxied) callers.
    /// Order: `X-Blockula-Client-IP` from an authenticated forwarder (secret or trusted socket
    /// IP), then the configured proxy header, then the socket peer. Unconfigured headers are ignored.
    fn client_ip(&self, peer: SocketAddr, h: &HeaderMap) -> Option<IpAddr> {
        self.client_ip.resolve(peer, h)
    }


    async fn forward(&self, path: &str, body: &Value, client: Option<IpAddr>) -> Result<Value, ApiError> {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(15)).build().unwrap();
        let mut last = String::from("no peers configured");
        for peer in &self.peers {
            let mut req = http.post(format!("{}/api{path}", peer.trim_end_matches('/'))).json(body).header(FORWARDED_HEADER, "1");
            if let Some(ip) = client {
                req = req.header("x-blockula-client-ip", ip.to_string());
            }
            if let Some(secret) = &self.client_ip.forwarder_secret {
                req = req.header("x-blockula-forwarder-key", secret);
            }
            match req.send().await {
                Ok(res) => {
                    let status = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                    let ok = res.status().is_success();
                    let v: Value = res.json().await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;
                    if ok {
                        return Ok(v);
                    }
                    let code = if status.is_client_error() || status == StatusCode::SERVICE_UNAVAILABLE { status } else { StatusCode::BAD_GATEWAY };
                    return Err(ApiError(code, v["error"].as_str().unwrap_or("peer rejected the request").to_string()));
                }
                Err(e) => last = format!("{:#}", anyhow::Error::from(e)),
            }
        }
        Err(ApiError(StatusCode::BAD_GATEWAY, format!("couldn't reach a validator: {last}")))
    }
}

const EXPLORER: &str = include_str!("explorer.html");
/// The explorer's animated ASCII backdrop, bundled from web/lib/ascii by `npm run build:haunt`.
const HAUNT_JS: &str = include_str!("haunt.js");

/// The explorer's one inline script (what its CSP hash covers: the text between the tags).
fn explorer_script() -> &'static str {
    EXPLORER.split_once("<script>").and_then(|(_, rest)| rest.split_once("</script>")).map(|(s, _)| s).unwrap_or_default()
}

/// The explorer's Content-Security-Policy (F64, defense in depth): only its own inline script
/// runs (by hash: no injected `<script>` or `on*=` handler), plus what that script adds itself
/// ('strict-dynamic': the backdrop, `/haunt.js`), it fetches from this origin only,
/// fonts come from Google Fonts, and no other page may frame it.
fn explorer_csp() -> &'static str {
    static CSP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CSP.get_or_init(|| {
        use commonware_cryptography::Hasher as _;
        let digest = commonware_cryptography::Sha256::hash(&[explorer_script().as_bytes()]);
        format!(
            "default-src 'none'; script-src 'sha256-{}' 'strict-dynamic'; style-src 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; img-src data:; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            base64(digest.as_ref())
        )
    })
}

/// Standard base64 with padding (RFC 4648), for the CSP hash.
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { ABC[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// The explorer's backdrop script, which the explorer's own script loads.
async fn haunt_js() -> Response {
    use axum::http::header;
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600"), (header::X_CONTENT_TYPE_OPTIONS, "nosniff")], HAUNT_JS).into_response()
}

async fn explorer() -> Response {
    use axum::http::header;
    ([(header::CONTENT_SECURITY_POLICY, explorer_csp()), (header::X_CONTENT_TYPE_OPTIONS, "nosniff"), (header::REFERRER_POLICY, "no-referrer")], Html(EXPLORER)).into_response()
}

/// Set on requests a node forwards to another; a forwarded request is never forwarded again
/// (so misconfigured peers — e.g. a node listed as its own peer — can't loop).
pub const FORWARDED_HEADER: &str = "x-blockula-forwarded";

fn forwarded(h: &HeaderMap) -> bool {
    h.contains_key(FORWARDED_HEADER)
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_SHA: &str = match option_env!("BLOCKULA_GIT_SHA") {
    Some(s) => s,
    None => "unknown",
};

/// `crypto.p2p_transport` in `/api/status`, which operators check at the genesis ceremony. Built
/// from constants only the vendored hybrid handshake exports (node/vendor/commonware-stream), so a
/// build that links upstream's classical stream (an unapplied `[patch.crates-io]`) doesn't
/// compile instead of misreporting. `vendored_stream_is_linked` pins the rest (one copy, ours).
pub fn p2p_transport() -> String {
    let label = std::str::from_utf8(commonware_stream::hybrid::PROTOCOL).unwrap_or("?");
    format!("{}, ML-DSA-65 peer auth ({label})", commonware_stream::hybrid::SUITE)
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // 410: history this node pruned (or never held, after a fast sync).
        let body = if self.0 == StatusCode::GONE { json!({ "error": self.1, "pruned": true }) } else { json!({ "error": self.1 }) };
        (self.0, Json(body)).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::BAD_REQUEST, e.to_string())
    }
}

fn not_found(what: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("{what} not found"))
}

type ApiResult = Result<Json<Value>, ApiError>;

pub fn router(node: Node) -> Router {
    Router::new()
        .route("/", get(explorer))
        .route("/haunt.js", get(haunt_js))
        .route("/api/status", get(status))
        .route("/api/blocks", get(blocks))
        .route("/api/block/{id}", get(block))
        .route("/api/tx/{id}", get(tx))
        .route("/api/tx", post(submit).layer(DefaultBodyLimit::max(MAX_TX_BODY_BYTES)))
        .route("/api/account/{id}", get(account))
        .route("/api/name/{name}", get(name))
        .route("/api/params", get(params))
        .route("/api/fees", get(fees))
        .route("/api/key/{key_id}", get(key_accounts))
        .route("/api/search/{q}", get(search))
        .route("/api/faucet", post(faucet))
        .route("/api/genesis", get(genesis))
        .route("/api/sync", get(sync))
        .route("/api/validators", get(validators))
        .route("/api/staking/{account}", get(staking))
        .route("/api/mempool", get(mempool))
        .route("/api/snapshot/latest", get(snapshot_latest))
        .route("/api/snapshot/{height}/chunk/{i}", get(snapshot_chunk))
        .route("/api/epochs", get(epochs))
        .route("/api/hunts", get(hunts))
        .route("/api/hunt/{id}", get(hunt))
        .route("/api/github/{handle}", get(github))
        .route("/api/contributions", get(contributions))
        .route("/api/claims", get(claims))
        .route("/api/claim/{id}", get(claim))
        .route("/api/seance/{id}", get(seance))
        .route("/api/souls", get(souls))
        .route("/api/proposals", get(proposals))
        .route("/api/proposal/{id}", get(proposal))
        .route("/api/distribution/{id}", get(distribution))
        .route("/api/multisig/{account}", get(multisig))
        .route("/api/proof/{namespace}/{key}", get(proof))
        .route("/api/ghost", get(ghost))
        .route("/api/ghost/address/{id}", get(ghost_address))
        .route("/api/health", get(health))
        .route("/api/upgrade", get(upgrade))
        .route("/api/beacon", get(beacon))
        .route("/metrics", get(metrics))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(axum::middleware::from_fn_with_state(node.clone(), limit_reads))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, node.timeout))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(node)
}

fn label(v: &ReadView, addr: &str) -> Value {
    let st = &v.state;
    let system = crate::chain::system_account(st.params.prefix(), addr).map(|role| match role {
        "treasury" => "🏛️ treasury".to_string(),
        "contribution pool" => "🪦 contribution pool".to_string(),
        other => format!("🏦 {other}"),
    });
    let name = if system.is_some() {
        system
    } else if st.validators.get(addr).is_some_and(|r| r.status == crate::chain::ValidatorStatus::Active) {
        v.primary_name(addr).or(Some("🧛 validator".into()))
    } else {
        v.primary_name(addr)
    };
    json!({ "address": addr, "name": name, "short": short(addr) })
}

fn tx_json(v: &ReadView, tx: &Tx, height: Option<u64>) -> Value {
    tx_json_with(v, tx, height.map(|height| Ran { height, idx: None, meta: None }), None)
}

/// Where a sealed tx ran: its block's height and, when the caller has them, its index in the
/// block and the block's index row (for who its action named, see `named_at_execution`).
#[derive(Clone, Copy)]
struct Ran<'a> {
    height: u64,
    idx: Option<usize>,
    meta: Option<&'a BlockMeta>,
}

/// The account a sealed tx's action named, as the chain resolved it when its block ran: the tx's
/// index rows (`BlockMeta::history`) hold the accounts it joined: the sender and the ones it
/// named, resolved just before it ran (`State::named_by`). A name signed as a name (audit gap-7-1)
/// pays whoever owned it then, so resolving it against today's state, or against the state after
/// its whole block, would credit a later owner, or nobody once it lapsed (audit
/// main13-protocol-3, review-main13-1). No other row: it named its own sender, or (only a failed
/// tx) a name that didn't resolve, so a failed one's is unknown (`None`).
fn named_at_execution(meta: &BlockMeta, idx: usize, sender: &str, failed: bool) -> Option<String> {
    let mut rows = meta.history.iter().filter(|(i, a)| *i as usize == idx && a != sender).map(|(_, a)| a);
    match (rows.next(), rows.next()) {
        (Some(a), None) => Some(a.clone()),
        (None, _) if !failed => Some(sender.to_string()),
        _ => None,
    }
}

fn tx_json_with(v: &ReadView, tx: &Tx, ran: Option<Ran>, error: Option<&str>) -> Value {
    let st = &v.state;
    let height = ran.map(|r| r.height);
    let sender = tx.sender().unwrap_or_default();
    // An address names itself. For a sealed tx, a name is who the block's index rows say it paid
    // when it ran, and unknown without them (never who owns it now); a pending one's resolves now.
    let named = |named: &String| -> Option<String> {
        if crypto::is_address(named) {
            return Some(named.clone());
        }
        match ran {
            Some(r) => named_at_execution(r.meta?, r.idx?, &sender, error.is_some()),
            None => st.resolve(named).ok(),
        }
    };
    // The name the action was signed with, for the kinds whose `to` it names.
    let signed_name = |n: &String| (!crypto::is_address(n)).then(|| format!("{}{NAME_SUFFIX}", normalize_name(n).unwrap_or_else(|_| n.clone())));
    let to_name = match &tx.action {
        Action::Transfer { to, .. } | Action::AwardHunt { to, .. } | Action::GrantSoul { to, .. } | Action::TransferName { to, .. } | Action::Unshield { to, .. } => signed_name(to),
        Action::Vouch { account } | Action::Unvouch { account } | Action::FlagSybil { account, .. } | Action::Clawback { account, .. } => signed_name(account),
        _ => None,
    };
    let (kind, to, amount) = match &tx.action {
        Action::Transfer { to, amount } => ("transfer", named(to), Some(*amount)),
        Action::RegisterName { .. } => ("register_name", None, None),
        Action::AddKey { .. } => ("add_key", None, None),
        Action::RemoveKey { .. } => ("remove_key", None, None),
        Action::RegisterValidator { bond, .. } => ("register_validator", None, Some(*bond)),
        Action::ApproveValidator { .. } => ("approve_validator", None, None),
        Action::RemoveValidator { .. } => ("remove_validator", None, None),
        Action::Unbond { .. } => ("unbond", None, None),
        Action::LinkGithub { .. } => ("link_github", None, None),
        Action::UnlinkGithub { .. } => ("unlink_github", None, None),
        Action::CreateHunt { amount, .. } => ("create_hunt", None, Some(*amount)),
        Action::FundHunt { amount, .. } => ("fund_hunt", None, Some(*amount)),
        Action::AwardHunt { to, .. } => ("award_hunt", named(to), None),
        Action::CancelHunt { .. } => ("cancel_hunt", None, None),
        Action::RegisterFamiliar { .. } => ("register_familiar", None, None),
        Action::SetRepo { .. } => ("set_repo", None, None),
        Action::GrantSoul { to, .. } => ("grant_soul", named(to), None),
        Action::Vouch { account } => ("vouch", named(account), Some(crate::poc::VOUCH_BOND)),
        Action::Unvouch { account } => ("unvouch", named(account), None),
        Action::FlagSybil { account, .. } => ("flag_sybil", named(account), None),
        Action::SubmitClaim { .. } => ("submit_claim", None, None),
        Action::AttestClaim { .. } => ("attest_claim", None, None),
        Action::RejectClaim { .. } => ("reject_claim", None, None),
        Action::JoinJury { bond } => ("join_jury", None, Some(*bond)),
        Action::LeaveJury {} => ("leave_jury", None, None),
        Action::CommitVotes { .. } => ("commit_votes", None, None),
        Action::RevealVotes { .. } => ("reveal_votes", None, None),
        Action::FundPool { amount } => ("fund_pool", Some(st.pool()), Some(*amount)),
        Action::WithdrawVested {} => ("withdraw_vested", None, None),
        Action::Clawback { account, .. } => ("clawback", named(account), None),
        Action::Delegate { validator, amount } => ("delegate", Some(validator.clone()), Some(*amount)),
        Action::Propose { deposit, .. } => ("propose", None, Some(*deposit)),
        Action::Vote { amount, .. } => ("vote", None, Some(*amount)),
        Action::TransferName { to, .. } => ("transfer_name", named(to), None),
        Action::ClaimDistribution { amount, .. } => ("claim_distribution", None, Some(*amount)),
        Action::Shield { amount, .. } => ("shield", None, Some(*amount)),
        Action::Unshield { to, amount, .. } => ("unshield", named(to), Some(*amount)),
        a => (a.kind(), None, None),
    };
    // An unsigned ghost tx pays exactly its fee (it's in its proof's value balance).
    let charged = if tx.is_unsigned() { tx.fee } else { charged(v, tx, height) };
    json!({
        "id": tx.id(),
        "summary": v.describe(tx, height),
        "kind": kind,
        "status": match (height, error) { (_, Some(_)) => "failed", (Some(_), None) => "finalized", (None, None) => "pending" },
        "error": error,
        "height": height,
        // Unsigned ghost txs have no sender to show.
        "from": (!tx.is_unsigned()).then(|| label(v, &sender)),
        // Who the action named: for a sealed tx, the account the chain resolved when it ran.
        "to": to.map(|t| label(v, &t)),
        "to_name": to_name,
        "amount": amount.map(|a| a.to_string()),
        "amount_display": amount.map(fmt_fang),
        // What the tx is charged (base fee burned + tip); `max_fee` is what it allowed.
        "fee": charged.to_string(),
        "fee_display": fmt_fang(charged),
        "max_fee": tx.fee.to_string(),
        "tip": tx.tip.to_string(),
        "valid_until": tx.valid_until,
        "size_bytes": tx.size(),
        "nonce": tx.nonce,
        "signer_key": tx.signer_key_id().ok(),
        "memo": tx.memo,
        // Ghost bundles without their ~130 KiB proof (its size and hash instead).
        "action": crate::ghost::action_json(&tx.action).unwrap_or_else(|| json!(tx.action)),
        "signature_scheme": if tx.is_unsigned() { "none: a Plonky3 STARK (the Action proof) authorizes it" } else { "ML-DSA-65 (FIPS 204)" },
    })
}

/// What a tx was (or, pending, would next be) charged: its block's base fee for its size plus
/// its tip, at most its `fee`. Blocks record their base fee (P-ECON); a pending tx is priced at
/// the next block's.
fn charged(v: &ReadView, tx: &Tx, height: Option<u64>) -> u64 {
    let charge = match height.and_then(|h| v.base_fee(h)) {
        Some(slot) => slot.fee(tx.size()).checked_add(tx.tip),
        None => crate::econ::charge(&v.state, tx, height.unwrap_or_else(|| crate::econ::now(&v.state))),
    };
    charge.unwrap_or(tx.fee).min(tx.fee)
}

fn block_summary(height: u64, txs: usize) -> String {
    format!("Block #{height} sealed with {txs} transaction{}", if txs == 1 { "" } else { "s" })
}

/// A block's header as the explorer lists it, from the index row alone.
fn header_json(v: &ReadView, height: u64, m: &BlockMeta) -> Value {
    json!({
        "height": height,
        "hash": hex::encode(m.hash),
        "prev_hash": hex::encode(m.prev_hash),
        "timestamp_ms": m.timestamp_ms,
        "proposer": label(v, &m.proposer),
        "state_root": hex::encode(m.state_root),
        "tx_count": m.tx_count,
        "version": m.version,
        "signal": m.signal,
        "summary": block_summary(height, m.tx_count as usize),
    })
}

fn block_json(v: &ReadView, b: &Block) -> Value {
    let meta = v.block_meta(b.height);
    json!({
        "height": b.height,
        "hash": b.hash,
        "prev_hash": b.prev_hash,
        "timestamp_ms": b.timestamp_ms,
        "proposer": label(v, &b.proposer),
        "state_root": b.state_root,
        "tx_count": b.txs.len(),
        "version": b.version,
        "signal": b.signal,
        "view": b.view,
        "parent_view": b.parent_view,
        // The base fee its txs paid (flat drops + `rate` milli-drops per byte, econ.rs).
        "base_fee": b.base_fee.map(|s| json!({ "flat": s.flat.to_string(), "rate": s.rate.to_string(), "max_block_bytes": s.max_block_bytes })),
        "summary": block_summary(b.height, b.txs.len()),
        "txs": b.txs.iter().enumerate().map(|(i, t)| tx_json_with(v, t, Some(Ran { height: b.height, idx: Some(i), meta: meta.as_ref() }), b.failed.get(&i).map(|s| s.as_str()))).collect::<Vec<_>>(),
        "signature_bytes": v.cert_len(b.height),
        "failed_count": b.failed.len(),
    })
}

fn sync_progress(live: &Live, height: u64) -> f64 {
    let t = live.network_height_claimed(height);
    if t == 0 { 1.0 } else { (height as f64 / t as f64).min(1.0) }
}

fn head_age_ms(v: &ReadView) -> Option<u64> {
    v.head_time_ms().map(|t| crate::chain::now_ms().saturating_sub(t))
}

/// `/api/status`'s summary of upgrades and halts (`/api/upgrade` has the details).
fn upgrade_status(n: &Node, v: &ReadView) -> Value {
    json!({
        "supported_protocol_version": crate::consensus::SUPPORTED_PROTOCOL_VERSION,
        "next": crate::upgrade::next_after(&v.state, v.height).map(|u| json!({ "version": u.version, "activation_height": u.activation_height })),
        "halted": n.halt.get().map(|h| json!({ "kind": h.kind(), "reason": h.to_string() })),
    })
}

async fn status(State(n): State<Node>) -> ApiResult {
    let v = n.live.view();
    let (st, g, h) = (&v.state, &v.genesis, v.height);
    let epoch = st.params.epoch_of(h + 1);
    Ok(Json(json!({
        "chain_id": g.chain_id,
        "token": g.token,
        "decimals": g.decimals,
        "height": h,
        "block_time_ms": g.block_time_ms,
        "validator": label(&v, &g.validators[0].address),
        "validators": st.active_sets.get(&epoch).map(|s| s.iter().map(|a| label(&v, a)).collect::<Vec<_>>()).unwrap_or_default(),
        "role": v.role,
        "finalized_height": n.live.finalized(h),
        "network_height_claimed": n.live.network_height_claimed(h),
        "syncing": n.live.syncing(h),
        "sync_progress": sync_progress(&n.live, h),
        "state_root": st.root(),
        "consensus": "simplex BFT",
        "epoch": epoch,
        "active_validators": st.active_sets.get(&epoch).map(|s| s.len()).unwrap_or(0),
        "faucet": g.faucet,
        "supply": st.supply.to_string(),
        "supply_display": fmt_fang(st.supply),
        "burned": st.burned.to_string(),
        "burned_display": fmt_fang(st.burned),
        "accounts": st.accounts.len(),
        "names": st.names.len(),
        "mempool": n.live.mempool().len(),
        "version": VERSION,
        "git_sha": GIT_SHA,
        "genesis_hash": v.genesis_hash,
        "head_age_ms": head_age_ms(&v),
        "history_from": v.history_from(),
        "pruning": v.keep_blocks.map(|k| json!({ "keep_blocks": k })),
        "fast_synced_from": v.base.as_ref().map(|b| b.height),
        // Sync trust (CRY-2): the newest trusted checkpoint, and how old one may be for a joiner.
        "checkpoint_height": n.checkpoint_height,
        "weak_subjectivity_blocks": crate::checkpoints::ws_period_blocks(&st.chain_params, st.params.epoch_blocks),
        "address_prefix": g.address_prefix,
        "max_tx_validity_blocks": st.chain_params.max_tx_validity_blocks,
        "protocol_version": crate::upgrade::version_at(st, h + 1),
        "upgrade": upgrade_status(&n, &v),
        "crypto": { "signatures": "ML-DSA-65 (FIPS 204)", "hash": "BLAKE3-256", "addresses": format!("bech32m({}, BLAKE3(pubkey))", g.address_prefix), "p2p_transport": p2p_transport() },
    })))
}

#[derive(Deserialize)]
struct Page {
    limit: Option<usize>,
    before: Option<u64>,
}

async fn blocks(State(n): State<Node>, Query(p): Query<Page>) -> ApiResult {
    let v = n.live.view();
    let limit = p.limit.unwrap_or(20).min(100);
    let before = p.before.unwrap_or(u64::MAX);
    let out: Vec<Value> = v.headers(before, limit).iter().map(|(h, m)| header_json(&v, *h, m)).collect();
    Ok(Json(json!(out)))
}

fn find_block(v: &ReadView, id: &str) -> Option<Arc<Block>> {
    match id.parse::<u64>() {
        Ok(h) if h >= 1 => v.block(h),
        _ => v.block_by_hash(id),
    }
}

/// 410 for a height this node executed but no longer holds.
fn pruned(v: &ReadView, height: u64) -> ApiError {
    let why = match &v.base {
        Some(b) if height <= b.height => format!("it fast-synced from a certified snapshot at #{}", b.height),
        _ => "it keeps only recent blocks (ask an archive node)".to_string(),
    };
    ApiError(StatusCode::GONE, format!("block #{height} is pruned on this node: history starts at #{} ({why})", v.history_from()))
}

async fn block(State(n): State<Node>, Path(id): Path<String>) -> ApiResult {
    let v = n.live.view();
    if let Ok(h) = id.parse::<u64>() {
        if v.is_pruned(h) {
            return Err(pruned(&v, h));
        }
    }
    let b = find_block(&v, &id).ok_or_else(|| not_found("block"))?;
    Ok(Json(block_json(&v, &b)))
}

async fn tx(State(n): State<Node>, Path(id): Path<String>) -> ApiResult {
    let v = n.live.view();
    if let Some((h, i)) = v.tx_location(&id) {
        if let Some((t, err)) = v.finalized_tx(h, i) {
            let meta = v.block_meta(h);
            return Ok(Json(tx_json_with(&v, &t, Some(Ran { height: h, idx: Some(i), meta: meta.as_ref() }), err.as_deref())));
        }
    }
    let mp = n.live.mempool();
    if let Some(pending) = mp.get(&id) {
        return Ok(Json(tx_json(&v, pending, None)));
    }
    if let Some((reason, height)) = mp.failure(&id) {
        // A failure that ran is in a block, normally answered above (this view may predate the
        // block, or the block may be pruned): its fee was charged. Anything else was dropped from
        // this node's mempool without running: nothing was charged, and a node that still holds it
        // may include it until its `valid_until` (audit review-fix-clients-2).
        if mp.failure_ran(&id) {
            return Ok(Json(json!({ "id": id, "status": "failed", "dropped": false, "error": reason, "height": height, "summary": format!("Transaction failed: {reason}") })));
        }
        return Ok(Json(json!({ "id": id, "status": "failed", "dropped": true, "error": reason, "height": height, "summary": format!("Transaction dropped without running: {reason}") })));
    }
    Err(not_found("transaction"))
}

async fn submit(State(n): State<Node>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, body: axum::body::Bytes) -> ApiResult {
    let client = n.client_ip(peer, &headers);
    if let Some(ip) = client {
        n.limits.allow(&[(format!("tx:{}", ip_key(ip)), TX_PER_IP_PER_MINUTE, Duration::from_secs(60))]).map_err(|w| too_many("too many transactions from your IP", w))?;
    }
    let result = match parse_tx(&headers, &body) {
        Ok(tx) => submit_inner(&n, &headers, client, tx).await,
        Err(e) => Err(e),
    };
    bump(if result.is_ok() { &n.stats.tx_submitted } else { &n.stats.tx_rejected });
    result
}

/// The client IP a full node passes on with a tx it forwards (so the validator's per-IP limits
/// see the end client): none for an unsigned ghost tx (audit F40). It names no one, and the IP
/// would tell the validator (and anyone on the path) who sent it. This node's own per-IP limit
/// still applied; the validator counts it against this node.
fn forwarded_client_ip(tx: &Tx, client: Option<IpAddr>) -> Option<IpAddr> {
    if tx.is_unsigned() { None } else { client }
}

/// A submitted tx: JSON (the API format), or its wire form (`Tx::to_bytes`) with
/// `Content-Type: application/octet-stream`, which is how ghost txs (~135 KiB) should travel.
fn parse_tx(headers: &HeaderMap, body: &[u8]) -> Result<Tx, ApiError> {
    let content_type = headers.get(axum::http::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default();
    if content_type.starts_with("application/octet-stream") {
        return Ok(Tx::from_bytes(body)?);
    }
    serde_json::from_slice(body).map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("bad transaction JSON: {e}")))
}

async fn submit_inner(n: &Node, headers: &HeaderMap, client: Option<IpAddr>, tx: Tx) -> ApiResult {
    // Format + ML-DSA signature checks run without the chain lock, after the cheap check that the
    // fee covers the next block's base fee.
    let (chain_id, genesis_hash, prefix, role, ghost_off) = {
        let v = n.live.view();
        // Ghost txs are refused before their proof is verified while Ghost Mode is off or paused.
        let ghost_off = if tx.action.is_ghost() { crate::ghost::check_enabled(&v.state, v.height + 1).err() } else { None };
        if ghost_off.is_none() {
            crate::econ::check_fee(&v.state, &tx, crate::econ::now(&v.state))?;
        }
        (v.genesis.chain_id.clone(), v.genesis_hash.clone(), v.genesis.address_prefix.clone(), v.role, ghost_off)
    };
    if let Some(e) = ghost_off {
        return Err(e.into());
    }
    let tx = if tx.action.ghost_bundle().is_some() {
        // A proof takes ~6 ms to verify: off the async workers.
        let (c, g, p) = (chain_id.clone(), genesis_hash.clone(), prefix.clone());
        tokio::task::spawn_blocking(move || crate::consensus::check_tx_stateless(&tx, &c, &g, &p).map(|_| tx)).await.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??
    } else {
        crate::consensus::check_tx_stateless(&tx, &chain_id, &genesis_hash, &prefix)?;
        tx
    };
    if role == Role::FullNode {
        if forwarded(headers) {
            return Err(ApiError(StatusCode::LOOP_DETECTED, "forwarding loop: this full node was sent a forwarded tx".into()));
        }
        // Admit locally (same checks as a validator), then hand it to a validator.
        let id = n.chain.lock().submit(tx.clone())?;
        if let Err(e) = n.forward("/tx", &serde_json::to_value(&tx).unwrap(), forwarded_client_ip(&tx, client)).await {
            n.chain.lock().drop_pending(&id);
            return Err(e);
        }
        return Ok(Json(json!({ "id": id, "status": "pending" })));
    }
    let id = n.chain.lock().submit(tx)?;
    Ok(Json(json!({ "id": id, "status": "pending" })))
}

/// Pending transactions (newest first).
async fn mempool(State(n): State<Node>, Query(p): Query<Page>) -> ApiResult {
    let (v, mp) = (n.live.view(), n.live.mempool());
    let limit = p.limit.unwrap_or(50).min(200);
    let txs: Vec<Value> = mp.newest().take(limit).map(|t| tx_json(&v, t, None)).collect();
    Ok(Json(json!({ "count": mp.len(), "txs": txs })))
}

/// 200 when the head is fresh and we're not syncing; 503 with a reason otherwise. A halted node
/// (upgrade.rs: an unsupported version next, `--halt-height`) says so first.
async fn health(State(n): State<Node>) -> Response {
    let v = n.live.view();
    let bt = v.genesis.block_time_ms.max(1);
    let age = head_age_ms(&v);
    let halt = n.halt.get();
    let reason = match age {
        _ if halt.is_some() => halt.as_ref().map(|h| format!("halted: {h}")),
        _ if n.live.syncing(v.height) => Some(format!("syncing: {} of {}", v.height, n.live.network_height_claimed(v.height))),
        None => Some("no blocks yet".to_string()),
        Some(a) if a > 10 * bt => Some(format!("head is {}s old", a / 1000)),
        _ => None,
    };
    let body = json!({ "ok": reason.is_none(), "reason": reason, "halted": halt.map(|h| h.kind()), "height": v.height, "head_age_ms": age, "finalized_height": n.live.finalized(v.height) });
    let code = if reason.is_none() { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (code, Json(body)).into_response()
}

/// Blocks `/api/upgrade` reads proposers' signals from (each active validator proposes about
/// once every `n` blocks).
const SIGNAL_WINDOW: usize = 200;

/// The upgrade schedule, what this binary supports, whether the node halted, and readiness: the
/// newest version each active validator signalled in the blocks it proposed recently.
async fn upgrade(State(n): State<Node>) -> ApiResult {
    let v = n.live.view();
    let (st, h) = (&v.state, v.height);
    let epoch = st.params.epoch_of(h + 1);
    let active = st.active_sets.get(&epoch).cloned().unwrap_or_default();
    let headers = v.headers(u64::MAX, SIGNAL_WINDOW);
    let signals = crate::upgrade::signals(&active, headers.iter().map(|(height, m)| (*height, m.proposer.as_str(), m.signal)));
    let next = crate::upgrade::next_after(st, h);
    let target = next.map(|u| u.version).unwrap_or(crate::consensus::SUPPORTED_PROTOCOL_VERSION);
    let ready = signals.values().filter(|(s, _)| *s >= target).count();
    Ok(Json(json!({
        "height": h,
        "protocol_version": crate::upgrade::version_at(st, h + 1),
        "supported_protocol_version": crate::consensus::SUPPORTED_PROTOCOL_VERSION,
        "schedule": st.upgrade.schedule.iter().map(|u| json!({ "version": u.version, "activation_height": u.activation_height, "active": u.activation_height <= h + 1 })).collect::<Vec<_>>(),
        // What cancellations withdrew: versions through `highest_version` are never scheduled
        // again, and upgrade proposals made by `last_cancelled_at` are superseded; the highest of
        // those (`superseded_version`) isn't spent, but a release may carry it.
        // `next_version`: what the next upgrade should ship as (past all of them and what's
        // scheduled; a new one is at most 16 past `next_version - 1`).
        "withdrawn": { "highest_version": st.upgrade.withdrawals.highest, "last_cancelled_at": st.upgrade.withdrawals.last_at, "superseded_version": st.upgrade.withdrawals.superseded },
        "next_version": crate::upgrade::next_version(st),
        "next": next.map(|u| json!({ "version": u.version, "activation_height": u.activation_height, "blocks_left": u.activation_height.saturating_sub(h + 1), "this_binary_supports_it": u.version <= crate::consensus::SUPPORTED_PROTOCOL_VERSION })),
        "halted": n.halt.get().map(|h| json!({ "kind": h.kind(), "reason": h.to_string() })),
        "readiness": {
            "version": target,
            "ready": ready,
            "active": active.len(),
            "two_thirds": ready * 3 >= active.len() * 2 && !active.is_empty(),
            "window_blocks": headers.len(),
            "validators": active.iter().map(|a| json!({ "validator": label(&v, a), "signal": signals.get(a).map(|s| s.0), "seen_at": signals.get(a).map(|s| s.1) })).collect::<Vec<_>>(),
        },
        "explainer": "Blocks carry the protocol version in force at their height and signal the newest version their proposer's binary supports. An upgrade is scheduled by governance once at least two-thirds of the active set signal it, and can be withdrawn by governance (CancelUpgrade) until 64 blocks before it activates (a withdrawn version number is never scheduled again, and upgrade proposals made before the withdrawal are superseded); a node whose binary doesn't support the version in force stops before that height instead of diverging.",
    })))
}

/// Epochs of beacon misses `/api/beacon` lists (the state keeps `beacon::VALUES_KEPT`).
const BEACON_MISS_HISTORY: u64 = 16;

/// What the randomness beacon's rounds stand at for `epoch` (beacon.rs): validators commit for
/// `epoch + 2` and reveal for `epoch + 1`. `(committed for e + 2, committed for e + 1, revealed
/// for e + 1)`.
fn beacon_participation(st: &crate::chain::State, epoch: u64) -> (usize, usize, usize) {
    let committing = st.beacon.rounds.get(&(epoch + 2)).map_or(0, |r| r.len());
    let revealing = st.beacon.rounds.get(&(epoch + 1));
    (committing, revealing.map_or(0, |r| r.len()), revealing.map_or(0, |r| r.values().filter(|e| e.secret.is_some()).count()))
}

/// 🎲 The randomness beacon (beacon.rs): this epoch's value and the next one's (derived at this
/// epoch's last block), the two rounds in progress (commitments for the epoch after next, reveals
/// for the next) with every active validator's part in them, and recent epochs' misses
/// (validators that committed and didn't reveal, jailed for it).
async fn beacon(State(n): State<Node>) -> ApiResult {
    let v = n.live.view();
    let (st, h) = (&v.state, v.height);
    let epoch = st.params.epoch_of(h + 1);
    let value = |e: u64| {
        st.beacon.values.get(&e).map(|x| json!({ "epoch": e, "value": hex::encode(x.value), "reveals": x.reveals, "missed": x.missed.iter().map(|a| label(&v, a)).collect::<Vec<_>>(), "hash_chain_only": x.reveals == 0 }))
    };
    let active = st.active_sets.get(&epoch).cloned().unwrap_or_default();
    let committing = st.beacon.rounds.get(&(epoch + 2));
    let revealing = st.beacon.rounds.get(&(epoch + 1));
    let validators: Vec<Value> = active
        .iter()
        .map(|a| {
            let c = committing.and_then(|r| r.get(a));
            let r = revealing.and_then(|r| r.get(a));
            json!({ "validator": label(&v, a), "committed_for_epoch_after_next": c.map(|e| e.committed_at), "committed_for_next": r.map(|e| e.committed_at), "revealed_for_next": r.and_then(|e| e.revealed_at) })
        })
        .collect();
    let (committed, owed, revealed) = beacon_participation(st, epoch);
    // Owed reveals that came from validators no longer in the set still count (they committed).
    let round = |target: u64, r: Option<&BTreeMap<String, crate::beacon::Entry>>| {
        json!({ "epoch": target, "committed": r.map_or(0, |r| r.len()), "revealed": r.map_or(0, |r| r.values().filter(|e| e.secret.is_some()).count()), "validators": r.map(|r| r.keys().map(|a| label(&v, a)).collect::<Vec<_>>()).unwrap_or_default() })
    };
    let misses: Vec<Value> = st.beacon.values.range(epoch.saturating_sub(BEACON_MISS_HISTORY)..=epoch).rev().filter(|(_, x)| !x.missed.is_empty()).map(|(e, x)| json!({ "epoch": e, "missed": x.missed.iter().map(|a| label(&v, a)).collect::<Vec<_>>() })).collect();
    Ok(Json(json!({
        "height": h,
        "epoch": epoch,
        "epoch_ends_at": st.params.last_of(epoch),
        "current": value(epoch),
        "next": value(epoch + 1),
        "active": active.len(),
        "commit_round": round(epoch + 2, committing),
        "reveal_round": round(epoch + 1, revealing),
        "participation": { "committed": committed, "of": active.len(), "reveals_owed": owed, "revealed": revealed },
        "validators": validators,
        "misses": misses,
        "miss_slash_bps": st.chain_params.beacon_miss_slash_bps,
        "explainer": "Each epoch, active validators commit to a secret for the epoch after next and reveal the one they committed to last epoch. At the epoch's last block the next epoch's value is BLAKE3 over this one and the reveals, sorted; it seeds the next epoch's leader order and Séances. A validator that committed and doesn't reveal is jailed and slashed (misses).",
    })))
}

/// Prometheus text format.
async fn metrics(State(n): State<Node>) -> Response {
    use std::fmt::Write as _;
    use std::sync::atomic::Ordering::Relaxed;
    let mut out = String::new();
    {
        let v = n.live.view();
        let h = v.height;
        let epoch = v.state.params.epoch_of(h + 1);
        let active = v.state.active_sets.get(&epoch).map(|s| s.len()).unwrap_or(0);
        let head_age = head_age_ms(&v).unwrap_or(0);
        let online = online_validators(&v);
        let finalized = n.live.finalized(h);
        for (name, help, val) in [
            ("blockula_height", "Executed height", h as f64),
            ("blockula_finalized_height", "Highest certified finalized height", finalized as f64),
            ("blockula_finalized_lag", "Finalized height minus executed height", finalized.saturating_sub(h) as f64),
            ("blockula_head_age_seconds", "Seconds since the head block's timestamp", head_age as f64 / 1000.0),
            ("blockula_mempool_size", "Pending transactions", n.live.mempool().len() as f64),
            ("blockula_epoch", "Current epoch", epoch as f64),
            ("blockula_active_validators", "Validators in the active set", active as f64),
            ("blockula_validators_online", "Signers on the latest finalization certificate", online.map(|o| o as f64).unwrap_or(f64::NAN)),
            ("blockula_history_from", "First height whose block this node holds", v.history_from() as f64),
            ("blockula_epochs_known_through", "Newest epoch whose validator set this node's history holds", n.live.epochs().known_through as f64),
            ("blockula_epochs_proven_through", "Newest epoch through which every set change has a quorum of handoff proofs here (it should follow known_through)", n.live.epochs().proven_through() as f64),
            ("blockula_epochs_from", "Oldest epoch of the validator-set history this node keeps and serves", n.live.epochs().base as f64),
            ("blockula_faucet_drips_total", "Faucet drips served", n.stats.faucet_drips.load(Relaxed) as f64),
            ("blockula_tx_submitted_total", "Transactions accepted over HTTP", n.stats.tx_submitted.load(Relaxed) as f64),
            ("blockula_tx_rejected_total", "Transactions rejected over HTTP", n.stats.tx_rejected.load(Relaxed) as f64),
            ("blockula_http_reads_limited_total", "HTTP reads refused by the per-client rate limit", n.stats.reads_limited.load(Relaxed) as f64),
        ] {
            let kind = if name.ends_with("_total") { "counter" } else { "gauge" };
            let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {val}");
        }
        let _ = writeln!(out, "# HELP blockula_build_info Version and git sha\n# TYPE blockula_build_info gauge\nblockula_build_info{{version=\"{VERSION}\",git_sha=\"{GIT_SHA}\"}} 1");
        // Upgrades and halts (upgrade.rs).
        let next = crate::upgrade::next_after(&v.state, h);
        let halt = n.halt.get();
        let mut gauges = vec![
            ("blockula_protocol_version", "Protocol version in force at the next height", crate::upgrade::version_at(&v.state, h + 1) as f64),
            ("blockula_supported_protocol_version", "Newest protocol version this binary runs", crate::consensus::SUPPORTED_PROTOCOL_VERSION as f64),
            ("blockula_upgrade_next_version", "Next scheduled protocol version (0: none)", next.map(|u| u.version as f64).unwrap_or(0.0)),
            ("blockula_upgrade_next_height", "Activation height of the next scheduled version (0: none)", next.map(|u| u.activation_height as f64).unwrap_or(0.0)),
            ("blockula_halted", "1 while the node has stopped executing (see /api/health)", f64::from(u8::from(halt.is_some()))),
        ];
        if let Some(started) = STARTED.get() {
            gauges.push(("blockula_process_start_time_seconds", "When this process started (unix seconds)", *started));
        }
        if let Some(peers) = n.p2p_peers.as_ref().and_then(|f| f()) {
            gauges.push(("blockula_p2p_peers", "Connected p2p peers", peers as f64));
        }
        // Slashing protection (protection.rs).
        if let Some(p) = &n.protection {
            let st = p.status();
            gauges.extend([
                ("blockula_slashing_protection_epoch", "Newest epoch this validator's key signed in", st.epoch as f64),
                ("blockula_slashing_protection_last_view", "Newest view this validator's key signed in that epoch", st.view as f64),
                ("blockula_slashing_protection_voting", "1 if this validator is in the current active set", f64::from(u8::from(st.voting))),
                ("blockula_slashing_protection_abstaining", "1 while it watches the network after a start before signing", f64::from(u8::from(st.abstaining))),
                ("blockula_slashing_protection_refused_total", "Vote signatures slashing protection refused", st.refused as f64),
            ]);
        }
        gauges.push(("blockula_downtime_max_missed", "Leader slots a validator may miss in its downtime window before it's jailed", v.state.chain_params.downtime_max_missed as f64));
        // The randomness beacon (beacon.rs): the rounds in progress and this epoch's value.
        let (committed, owed, revealed) = beacon_participation(&v.state, epoch);
        let current = v.state.beacon.values.get(&epoch);
        gauges.extend([
            ("blockula_beacon_commitments", "Validators committed to the beacon for the epoch after next", committed as f64),
            ("blockula_beacon_reveals_owed", "Validators that committed for the next epoch's beacon (each owes a reveal this epoch)", owed as f64),
            ("blockula_beacon_reveals", "Reveals for the next epoch's beacon so far", revealed as f64),
            ("blockula_beacon_last_reveals", "Reveals that went into this epoch's beacon value (0: the hash chain alone)", current.map_or(0.0, |x| x.reveals as f64)),
            ("blockula_beacon_last_missed", "Validators that committed for this epoch's beacon and didn't reveal (jailed for it)", current.map_or(0.0, |x| x.missed.len() as f64)),
        ]);
        if let Some(me) = n.me.as_deref() {
            gauges.push(("blockula_beacon_own_missed", "1 if this validator missed its reveal for this epoch's beacon", f64::from(u8::from(current.is_some_and(|x| x.missed.iter().any(|a| a == me))))));
        }
        for (name, help, val) in gauges {
            let kind = if name.ends_with("_total") { "counter" } else { "gauge" };
            let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {val}");
        }
        // Staking (staking.rs), per validator: leader-slot liveness over the downtime window, and
        // jail and tombstone flags. For the active set, every jailed validator, validators
        // tombstoned within the unbonding period (bounded: the set is at most 64) and this node's
        // own, which is labelled `own="1"` (what its operator alerts on).
        let st = &v.state;
        let mut shown: std::collections::BTreeSet<&String> = st.active_sets.get(&epoch).map(|s| s.iter().collect()).unwrap_or_default();
        let recent = h.saturating_sub(st.chain_params.unbonding_blocks);
        shown.extend(st.staking.pools.iter().filter(|(_, p)| p.jail.is_some() || p.tombstoned_at.is_some_and(|t| t >= recent)).map(|(a, _)| a));
        shown.extend(n.me.iter().filter(|a| st.validators.contains_key(*a)));
        let series = |name: &str, help: &str, kind: &str, val: &dyn Fn(&str) -> f64| {
            let mut s = format!("# HELP {name} {help}\n# TYPE {name} {kind}\n");
            for a in &shown {
                let own = u8::from(n.me.as_deref() == Some(a.as_str()));
                let _ = writeln!(s, "{name}{{validator=\"{a}\",own=\"{own}\"}} {}", val(a));
            }
            s
        };
        let liveness = |a: &str| st.staking.liveness.get(a);
        let pool = |a: &str| st.staking.pools.get(a);
        out.push_str(&series("blockula_missed_leader_slots_total", "Leader slots the validator missed since its liveness record started (it restarts on unjail)", "counter", &|a| liveness(a).map_or(0.0, |l| l.missed_total as f64)));
        out.push_str(&series("blockula_validator_missed_slots", "Leader slots the validator missed in its current downtime window (jailed past blockula_downtime_max_missed)", "gauge", &|a| liveness(a).map_or(0.0, |l| l.missed as f64)));
        out.push_str(&series("blockula_validator_jailed", "1 while the validator is jailed", "gauge", &|a| f64::from(u8::from(pool(a).is_some_and(|p| p.jail.is_some())))));
        out.push_str(&series("blockula_validator_tombstoned", "1 once the validator was tombstoned for double-signing (never active again)", "gauge", &|a| f64::from(u8::from(pool(a).is_some_and(|p| p.tombstoned_at.is_some())))));
        if let Some(h) = halt {
            let _ = writeln!(out, "# HELP blockula_halt_info Why the node halted\n# TYPE blockula_halt_info gauge\nblockula_halt_info{{kind=\"{}\"}} 1", h.kind());
        }
    }
    if let Some(extra) = &n.extra_metrics {
        out.push_str(&extra());
    }
    ([(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")], out).into_response()
}

/// How many validators signed the most recent finalization certificate we hold.
pub fn online_validators(v: &ReadView) -> Option<usize> {
    use commonware_codec::Decode as _;
    let h = (v.height.saturating_sub(16)..=v.height).rev().find(|h| v.cert_len(*h) > 0)?;
    let bytes = v.cert(h)?;
    let f = commonware_consensus::simplex::types::Finalization::<crate::pq::MlDsaScheme, commonware_cryptography::blake3::Digest>::decode_cfg(bytes.as_slice(), &1024usize).ok()?;
    Some(f.certificate.signers.count())
}

/// Entries per list in `/api/account` (names, Hunts, claims, vouches).
pub const ACCOUNT_LIST_LIMIT: usize = 100;

async fn account(State(n): State<Node>, Path(id): Path<String>) -> ApiResult {
    let v = n.live.view();
    let st = &v.state;
    let addr = st.resolve(&id)?;
    let acct = st.account(&addr);
    let names: Vec<String> = v.names_of(&addr, ACCOUNT_LIST_LIMIT).into_iter().map(|n| format!("{n}{NAME_SUFFIX}")).collect();
    let mp = n.live.mempool();
    // The mempool is capped (`MAX_MEMPOOL`), so this scan is bounded.
    let mut recent: Vec<Value> = mp.newest().filter(|t| t.from == addr || matches!(&t.action, Action::Transfer { to, .. } if st.resolve(to).ok().as_deref() == Some(addr.as_str()))).map(|t| tx_json(&v, t, None)).collect();
    // Each block's index row once, for who its txs paid when they ran.
    let history = v.history(&addr, 25);
    let mut metas: BTreeMap<u64, Option<BlockMeta>> = BTreeMap::new();
    for &(h, _) in &history {
        metas.entry(h).or_insert_with(|| v.block_meta(h));
    }
    recent.extend(history.into_iter().filter_map(|(h, i)| v.finalized_tx(h, i).map(|(t, e)| tx_json_with(&v, &t, Some(Ran { height: h, idx: Some(i), meta: metas.get(&h).and_then(Option::as_ref) }), e.as_deref()))));
    let hunts_created: Vec<u64> = v.ids(ix::HUNT_CREATOR, &addr, false, ACCOUNT_LIST_LIMIT);
    let hunts_won: Vec<Value> = v.ids(ix::HUNT_WINNER, &addr, false, ACCOUNT_LIST_LIMIT).into_iter().filter_map(|id| st.hunts.get(&id)).map(|h| json!({ "id": h.id, "repo": h.repo, "issue": h.issue, "amount_display": fmt_fang(h.amount), "pr": h.pr, "self_awarded": h.self_awarded() })).collect();
    let keys = st.keys_of(&addr);
    // What it can move out now (econ::free: vesting and vote locks hold the rest, net of stake),
    // and the locks still holding something, as the next block runs them.
    let now = crate::econ::now(st);
    let (transferable, locked) = (crate::econ::transferable(st, &addr, now), acct.locked_at(now));
    let locks: Vec<Value> = acct
        .locks
        .iter()
        .filter(|l| l.amount.locked_at(now) > 0)
        .map(|l| {
            let until = match l.amount {
                crate::chain::LockAmount::Vesting { end, .. } => end,
                crate::chain::LockAmount::Until { until, .. } => until,
            };
            json!({ "id": l.id, "reason": l.reason, "amount": l.amount.locked_at(now).to_string(), "amount_display": fmt_fang(l.amount.locked_at(now)), "until": until, "vesting": matches!(l.amount, crate::chain::LockAmount::Vesting { .. }) })
        })
        .collect();
    Ok(Json(json!({
        "account": label(&v, &addr),
        "balance": acct.balance.to_string(),
        "balance_display": fmt_fang(acct.balance),
        "transferable": transferable.to_string(),
        "transferable_display": fmt_fang(transferable),
        "locked": locked.to_string(),
        "locked_display": fmt_fang(locked),
        "locks": locks,
        // Whether the chain has an account here yet, and what a payment that opens it burns from
        // its sender (`account_creation_fee`; 0 for an existing or keyless system account).
        "exists": st.accounts.contains_key(&addr),
        "creation_fee": st.creation_fee_for(&addr).to_string(),
        "nonce": acct.nonce,
        "next_nonce": acct.nonce + mp.pending_count(&addr) as u64,
        "names": names,
        "keys": keys.iter().map(|(id, k)| json!({ "key_id": id, "label": k.label, "role": k.role, "added_at": k.added_at, "spent_this_period": k.spent.to_string(), "window_start": k.window_start })).collect::<Vec<_>>(),
        "recent_txs": recent,
        "github": st.github_by_account.get(&addr).and_then(|h| st.github.get(h)).map(|l| json!({ "handle": l.handle, "gist_id": l.gist_id, "linked_at": l.linked_at, "verify_url": format!("/api/github/{}", l.handle) })),
        "familiars": st.familiars.get(&addr).map(|f| f.iter().filter(|(k, _)| keys.contains_key(*k)).map(|(k, l)| json!({ "key_id": k, "label": l })).collect::<Vec<_>>()).unwrap_or_default(),
        "hunts_created": hunts_created,
        "hunts_won": hunts_won,
        "contribution": contribution_profile(&v, &addr),
    })))
}

/// Which accounts a key may act for — lets a newly linked device find its account. From the
/// store's key index, not a scan of every account.
#[derive(Deserialize)]
struct KeyQuery {
    /// The last account of the previous page.
    after: Option<String>,
    limit: Option<usize>,
}

/// `/api/key/{key_id}?after=&limit=`: the accounts a key is on, in address order, a page (at most
/// `ACCOUNT_LIST_LIMIT`) at a time; `next` is the next page's `after` (`null` at the end). The
/// key's own address (its original account) leads the first page. Only accounts the key agreed to
/// join can list it (AddKey carries its proof of possession), so strangers can't pad this list to
/// push an account off it (audit gap-7-2).
async fn key_accounts(State(n): State<Node>, Path(key_id): Path<String>, Query(q): Query<KeyQuery>) -> ApiResult {
    let v = n.live.view();
    let st = &v.state;
    let limit = q.limit.unwrap_or(ACCOUNT_LIST_LIMIT).clamp(1, ACCOUNT_LIST_LIMIT);
    let after = q.after.as_deref().filter(|a| !a.is_empty());
    // One more than a page, to know whether there's another.
    let mut page = v.accounts_with_key_after(&key_id, after, limit + 1);
    let next = (page.len() > limit).then(|| {
        page.truncate(limit);
        page.last().cloned()
    });
    let mut accounts: Vec<Value> = page
        .into_iter()
        .filter_map(|addr| {
            let k = st.accounts.get(&addr)?.keys.get(&key_id)?.clone();
            Some(json!({ "account": label(&v, &addr), "role": k.role, "label": k.label }))
        })
        .collect();
    if let Ok(bytes) = hex::decode(&key_id) {
        if let Ok(bytes) = <[u8; 32]>::try_from(bytes) {
            let own = crypto::render_address(st.params.prefix(), &bytes);
            let listed = accounts.iter().any(|a| a["account"]["address"] == own.as_str());
            if after.is_none() && !listed && st.keys_of(&own).contains_key(&key_id) {
                accounts.insert(0, json!({ "account": label(&v, &own), "role": { "type": "owner" }, "label": "original key" }));
            }
        }
    }
    Ok(Json(json!({ "key_id": key_id, "accounts": accounts, "next": next.flatten() })))
}

/// A name (DEC-11): its `status` (`active`; `grace`, when only its owner can renew it;
/// `reserved`; or `available`), its owner and expiry, the account it's offered to (`offered_to`:
/// theirs once they accept it), and what registering it or renewing it for a year costs.
async fn name(State(node): State<Node>, Path(n): Path<String>) -> ApiResult {
    let v = node.live.view();
    let st = &v.state;
    let n = normalize_name(&n)?;
    let cp = &st.chain_params;
    let (price, renewal) = (cp.name_price(n.len()), cp.name_renewal_price(n.len(), 1));
    let mut out = json!({
        "name": format!("{n}{NAME_SUFFIX}"),
        "price": price.to_string(),
        "price_display": fmt_fang(price),
        "renewal_price": renewal.to_string(),
        "renewal_price_display": fmt_fang(renewal),
        "registration_blocks": cp.name_registration_blocks,
        "grace_blocks": cp.name_grace_blocks,
    });
    let now = crate::econ::now(st);
    let status = match st.names.get(&n) {
        Some(r) => {
            let grace = crate::econ::grace_until(st, r);
            out["owner"] = label(&v, &r.owner);
            out["registered_at"] = r.registered_at.into();
            out["expires_at"] = r.expires_at.into();
            out["grace_until"] = grace.into();
            if let Some(to) = &r.offered_to {
                out["offered_to"] = label(&v, to);
            }
            if now <= r.expires_at {
                "active"
            } else if now <= grace {
                "grace"
            } else {
                "available"
            }
        }
        None if st.econ.reserved_names.contains_key(&n) => "reserved",
        None => "available",
    };
    out["status"] = status.into();
    out["available"] = (status == "available").into();
    Ok(Json(out))
}

/// The governance-adjustable parameters (amounts as strings), the genesis constants, and the
/// issuance year under way.
async fn params(State(n): State<Node>) -> ApiResult {
    let v = n.live.view();
    let st = &v.state;
    let params: serde_json::Map<String, Value> = st.chain_params.to_map().into_iter().map(|(k, v)| (k.to_string(), v.to_string().into())).collect();
    let epy = crate::econ::epochs_per_year(&st.params);
    let iss = st.econ.issuance;
    Ok(Json(json!({
        "params": params,
        "genesis": { "block_time_ms": st.params.block_time_ms, "epoch_blocks": st.params.epoch_blocks, "address_prefix": st.params.prefix(), "epochs_per_year": epy },
        "issuance": {
            "year": iss.year,
            "rate_ppm": iss.rate_ppm,
            "base_supply": iss.base_supply.to_string(),
            "per_epoch": crate::econ::epoch_mint(&iss, epy).to_string(),
            "per_epoch_display": fmt_fang(crate::econ::epoch_mint(&iss, epy)),
        },
        "supply": st.supply.to_string(),
        "burned": st.burned.to_string(),
        // Who changes what: governance's safety parameters and the validators' own economics take
        // the token supermajority and never pass by the bootstrap council; the council may also
        // pass the `council` keys (any key not listed there is the token holders' alone).
        "classes": {
            "governance": crate::params::GOVERNANCE_KEYS,
            "validators": crate::params::VALIDATOR_KEYS,
            "council": crate::params::COUNCIL_KEYS,
        },
    })))
}

/// The base fee (MAINNET-DESIGN 3.6): every block's slot from the last executed one to `lag`
/// blocks ahead (flat drops, `rate` in milli-drops per byte, the block's tx-byte limit), and
/// `suggested`, the highest flat part and rate among the upcoming ones: a tx whose `fee` covers
/// `flat + rate × size / 1000 + tip` is accepted in any of the next `lag` blocks, and is charged
/// only its block's base fee plus its tip. A tip may be at most `tip_cap_multiple` × the base
/// fee of the block that includes it; `lowest` (the lowest flat part and rate upcoming) prices a
/// tip that fits the cap in any of them.
async fn fees(State(n): State<Node>) -> ApiResult {
    use crate::econ::{FEE_LAG, RATE_SCALE};
    let v = n.live.view();
    let st = &v.state;
    let cp = &st.chain_params;
    let slot = |h: u64, s: &crate::econ::FeeSlot| json!({ "height": h, "flat": s.flat.to_string(), "rate": s.rate.to_string(), "max_block_bytes": s.max_block_bytes, "ghost_max_tx_bytes": s.ghost_max_tx_bytes, "ghost_max_block_bytes": s.ghost_max_block_bytes });
    let slots: Vec<Value> = st.econ.fees.iter().enumerate().map(|(i, s)| slot(st.econ.height + i as u64, s)).collect();
    let (s, low) = (crate::econ::suggested(st), crate::econ::lowest(st));
    Ok(Json(json!({
        "height": st.econ.height,
        "lag": FEE_LAG,
        "rate_scale": RATE_SCALE,
        "slots": slots,
        "suggested": { "flat": s.flat.to_string(), "rate": s.rate.to_string() },
        "lowest": { "flat": low.flat.to_string(), "rate": low.rate.to_string() },
        "tip_cap_multiple": cp.tip_cap_multiple,
        "floor": { "flat": cp.base_fee_floor_flat.to_string(), "per_byte": cp.base_fee_floor_per_byte.to_string() },
        "target_fullness_bps": cp.target_fullness_bps,
        "max_change_bps": cp.base_fee_max_change_bps,
        "account_creation_fee": cp.account_creation_fee.to_string(),
        "account_creation_fee_display": fmt_fang(cp.account_creation_fee),
    })))
}

async fn search(State(n): State<Node>, Path(q): Path<String>) -> ApiResult {
    let v = n.live.view();
    let mp = n.live.mempool();
    let q = q.trim().to_lowercase();
    let hit = if find_block(&v, &q).is_some() {
        json!({ "type": "block", "id": q })
    } else if v.tx_location(&q).is_some() || mp.contains(&q) || mp.failure(&q).is_some() {
        json!({ "type": "tx", "id": q })
    } else if crypto::is_address(&q) {
        json!({ "type": "account", "id": q })
    } else if let Ok(n) = normalize_name(&q) {
        if v.state.names.contains_key(&n) { json!({ "type": "account", "id": format!("{n}{NAME_SUFFIX}") }) } else { json!({ "type": "name", "id": n }) }
    } else {
        return Err(not_found("nothing matching that"));
    };
    Ok(Json(hit))
}

async fn faucet(State(n): State<Node>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(body): Json<HashMap<String, String>>) -> ApiResult {
    let to = body.get("to").ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "missing `to`".into()))?.clone();
    let to = &to;
    let (faucet_here, resolved, enabled, busy) = {
        let c = n.chain.lock();
        let here = c.me().is_some_and(|me| c.genesis.is_validator(&me));
        (here, c.state.resolve(to)?, c.genesis.faucet, here && c.faucet_busy())
    };
    if !enabled {
        return Err(ApiError(StatusCode::BAD_REQUEST, "this chain has no faucet".into()));
    }
    if busy {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "faucet busy, retry in a few seconds".into()));
    }
    let client = n.client_ip(peer, &headers);
    let mut rules = vec![(format!("faucet-addr:{resolved}"), 1, FAUCET_ADDRESS_WINDOW)];
    if let Some(ip) = client {
        rules.push((format!("faucet-ip:{}", ip_key(ip)), 1, FAUCET_IP_WINDOW));
    }
    // Loopback callers (local dev, tests) aren't limited at all.
    let limited = client.is_some() || !peer.ip().to_canonical().is_loopback();
    // The slot is taken before the drip (atomically: parallel requests can't all pass a check
    // first, F28) and given back if the drip doesn't go out.
    let reserved = if limited { Some(n.limits.allow(&rules).map_err(|w| too_many("the faucet already dripped for you", w))?) } else { None };
    let dripped = async {
        if !faucet_here {
            if forwarded(&headers) {
                return Err(ApiError(StatusCode::LOOP_DETECTED, "forwarding loop: no faucet here".into()));
            }
            let v = n.forward("/faucet", &json!({ "to": to }), client).await?;
            if let Ok(tx) = serde_json::from_value::<Tx>(v["tx"].clone()) {
                let _ = n.chain.lock().track_pending(tx);
            }
            Ok(v)
        } else {
            let tx = n.chain.lock().faucet(to, 100 * DROPS_PER_FANG).map_err(|e| {
                let msg = e.to_string();
                ApiError(if msg.contains("faucet busy") { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::BAD_REQUEST }, msg)
            })?;
            Ok(json!({ "id": tx.id(), "amount_display": "100 FANG", "tx": tx }))
        }
    }
    .await;
    let v = match dripped {
        Ok(v) => v,
        Err(e) => {
            if let Some(at) = reserved {
                n.limits.refund(&rules, at);
            }
            return Err(e);
        }
    };
    bump(&n.stats.faucet_drips);
    Ok(Json(v))
}

#[derive(Deserialize)]
struct SnapshotQuery {
    /// The manifest must reach at least this height (the joiner's newest checkpoint).
    min: Option<u64>,
}

/// Manifest of a certified state snapshot (for `join --fast`): the newest, or for a big state the
/// newest long-lived one (`snapshot::long_interval`), reaching `min` if given (when none does,
/// the one reaching furthest, so the joiner sees it's behind and waits for the next one).
async fn snapshot_latest(State(n): State<Node>, Query(q): Query<SnapshotQuery>) -> ApiResult {
    let m = crate::snapshot::build_manifest_reaching(&n.live, q.min.unwrap_or(0)).map_err(|e| ApiError(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    Ok(Json(serde_json::to_value(m).unwrap()))
}

async fn snapshot_chunk(State(n): State<Node>, Path((height, i)): Path<(u64, usize)>) -> Response {
    let home = n.live.disk.home.clone();
    match crate::snapshot::chunk(&home, height, i) {
        Some(bytes) => ([(axum::http::header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response(),
        None => not_found("snapshot chunk").into_response(),
    }
}

#[derive(Deserialize)]
struct EpochsQuery {
    from: Option<u64>,
}

/// Validator-set history with handoff proofs, a page at a time: the set in effect at `from`
/// (default: the oldest epoch this node keeps, `base`) and the set changes after it, each with a
/// quorum of its proofs, up to about `epochs::PAGE_BYTES`. `next` is the `from` of the next page
/// (`null` at the end); `proven_through` covers this page. Pages are cached per registry.
async fn epochs(State(n): State<Node>, Query(q): Query<EpochsQuery>) -> Response {
    let from = q.from.unwrap_or_else(|| n.live.epochs().base);
    let page = n.live.epoch_page(from);
    ([(axum::http::header::CONTENT_TYPE, "application/json")], page.as_ref().clone()).into_response()
}

/// A record and its Merkle proof against the state root of the latest executed block:
/// `/api/proof/{namespace}/{key as hex}`. Check it with `state::tree::verify` (the SDK has the
/// same 20 lines): `verify(root, namespace, key, value, proof)`.
async fn proof(State(n): State<Node>, Path((namespace, key_hex)): Path<(u8, String)>) -> ApiResult {
    let key = hex::decode(&key_hex).map_err(|_| ApiError(StatusCode::BAD_REQUEST, "the key must be hex".into()))?;
    if key.len() > crate::state::MAX_KEY_BYTES {
        return Err(ApiError(StatusCode::BAD_REQUEST, format!("keys are at most {} bytes", crate::state::MAX_KEY_BYTES)));
    }
    let v = n.live.view();
    let value = v.state.record(namespace, &key);
    let proof = v.state.prove(namespace, &key).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({
        "namespace": namespace,
        "key": hex::encode(&key),
        "value": value.as_deref().map(hex::encode),
        "proof": proof,
        "root": hex::encode(v.state.root_hash()),
        "height": v.height,
        "block_hash": v.last_hash,
    })))
}

/// 👻 Ghost Mode: on or off, paused, the pool's balance and cap, the commitment tree's size and
/// recent anchors, the limits, and the chain instance wallets bind proofs to (ghost.rs).
async fn ghost(State(n): State<Node>) -> ApiResult {
    let v = n.live.view();
    Ok(Json(crate::ghost::status_json(&v.state, v.height)))
}

/// The Ghost Mode address an account published (`SetGhostAddress`), by address or `.boo` name.
async fn ghost_address(State(n): State<Node>, Path(id): Path<String>) -> ApiResult {
    let v = n.live.view();
    let account = v.state.resolve(&id).map_err(|_| not_found("account"))?;
    let address = v.state.ghost.addresses.get(&account).ok_or_else(|| not_found("ghost address"))?;
    Ok(Json(json!({ "account": label(&v, &account), "address": hex::encode(address), "fingerprint": crate::ghost::fingerprint(address) })))
}

fn hunt_json(v: &ReadView, h: &crate::chain::Hunt) -> Value {
    let familiar = h.familiar_key.as_ref().and_then(|k| h.awarded_to.as_ref().and_then(|a| v.state.familiars.get(a)?.get(k).cloned()));
    json!({
        "id": h.id,
        "repo": h.repo,
        "issue": h.issue,
        "issue_url": format!("https://github.com/{}/issues/{}", h.repo, h.issue),
        "title": h.title,
        "amount": h.amount.to_string(),
        "amount_display": fmt_fang(h.amount),
        "creator": label(v, &h.creator),
        "funders": h.funders.iter().map(|(a, amt)| json!({ "account": label(v, a), "amount": amt, "amount_display": fmt_fang(amt.parse().unwrap_or(0)) })).collect::<Vec<_>>(),
        "status": hunt_status(h, v.height),
        "created_at": h.created_at,
        "expires_at": h.expires_at,
        "awarded_to": h.awarded_to.as_ref().map(|a| label(v, a)),
        "awarded_by": h.awarded_by.as_ref().map(|a| label(v, a)),
        "awarded_at": h.awarded_at,
        "pr": h.pr,
        "pr_url": h.pr.map(|p| format!("https://github.com/{}/pull/{}", h.repo, p)),
        "familiar": familiar,
        "self_awarded": h.self_awarded(),
        "cancelled_at": h.cancelled_at,
    })
}

#[derive(Deserialize)]
struct HuntQuery {
    status: Option<String>,
    limit: Option<usize>,
}

/// Hunts, biggest bounty first. `?status=open|awarded|cancelled|expired`. The order is computed
/// once per block (`ReadView::hunts`), then each request takes `limit` from it.
async fn hunts(State(n): State<Node>, Query(q): Query<HuntQuery>) -> ApiResult {
    let v = n.live.view();
    let memo = v.hunts();
    let ids: &[u64] = match q.status.as_deref() {
        None => &memo.all,
        Some(s) => memo.by_status.get(s).map(Vec::as_slice).unwrap_or(&[]),
    };
    let list: Vec<Value> = ids.iter().take(q.limit.unwrap_or(100).min(500)).filter_map(|id| v.state.hunts.get(id)).map(|h| hunt_json(&v, h)).collect();
    Ok(Json(json!({ "count": v.state.hunts.len(), "open_bounties": memo.open_total.to_string(), "open_bounties_display": fmt_fang(memo.open_total), "hunts": list })))
}

async fn hunt(State(n): State<Node>, Path(id): Path<u64>) -> ApiResult {
    let v = n.live.view();
    let h = v.state.hunts.get(&id).ok_or_else(|| not_found("hunt"))?;
    Ok(Json(hunt_json(&v, h)))
}

/// Upstream GitHub calls allowed per minute (per node), and how long results are cached.
pub const GITHUB_PER_MINUTE: usize = 30;
pub const GITHUB_CACHE: Duration = Duration::from_secs(600);

/// Checks a GitHub link off-chain: the gist must be owned by the handle and contain the linked
/// account's address. Only ever calls api.github.com; results are cached.
async fn github(State(n): State<Node>, Path(handle): Path<String>) -> ApiResult {
    check_github_handle(&handle)?;
    let lower = handle.to_ascii_lowercase();
    let link = n.live.view().state.github.get(&lower).cloned().ok_or_else(|| not_found("GitHub link"))?;
    let key = format!("{lower}:{}:{}", link.account, link.gist_id);
    if let Some((at, v)) = n.github_cache.lock().get(&key) {
        if at.elapsed() < GITHUB_CACHE {
            return Ok(Json(v.clone()));
        }
    }
    n.limits.allow(&[("github-upstream".into(), GITHUB_PER_MINUTE, Duration::from_secs(60))]).map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "GitHub checks are rate limited; try again in a minute".into()))?;
    let (verified, reason) = match crate::github::fetch_gist(&link.gist_id).await {
        Ok(None) => (false, "gist not found".to_string()),
        Ok(Some((owner, files))) => {
            if !owner.eq_ignore_ascii_case(&link.handle) {
                (false, format!("gist is owned by @{owner}, not @{}", link.handle))
            } else if !files.iter().any(|f| f.contains(&link.account)) {
                (false, "gist doesn't contain the account's boo1 address".to_string())
            } else {
                (true, "gist owned by the handle contains the account's address".to_string())
            }
        }
        Err(e) => (false, format!("couldn't check the gist: {e}")),
    };
    let v = {
        let view = n.live.view();
        json!({ "handle": link.handle, "account": label(&view, &link.account), "gist_id": link.gist_id, "gist_url": format!("https://gist.github.com/{}", link.gist_id), "linked_at": link.linked_at, "verified": verified, "reason": reason, "attested_by": "this node (off-chain check; validators re-check it for every contribution claim)" })
    };
    n.github_cache.lock().insert(key, (Instant::now(), v.clone()));
    Ok(Json(v))
}

fn validator_json(v: &ReadView, r: &crate::chain::ValidatorRecord) -> Value {
    use crate::staking;
    let st = &v.state;
    let p = st.staking.pools.get(&r.address).cloned().unwrap_or_default();
    let self_bond = staking::self_bond(st, &r.address);
    json!({
        "validator": label(v, &r.address),
        "owner": label(v, &r.owner),
        "status": r.status,
        "consensus_key": r.consensus_key,
        "p2p_addr": r.p2p_addr,
        "http_addr": r.http_addr,
        "stake": p.stake.to_string(),
        "stake_display": fmt_fang(p.stake),
        // The pool's stake (older clients read `bond`).
        "bond": p.stake.to_string(),
        "bond_display": fmt_fang(p.stake),
        "shares": p.shares.to_string(),
        "self_bond": self_bond.to_string(),
        "self_bond_display": fmt_fang(self_bond),
        // What its rewards are charged: its commission, at most `commission_max_bps` (audit
        // gap-5-1); `commission_recorded_bps` is its own setting when a lowered cap binds it.
        "commission_bps": staking::commission_rate(st, &r.address),
        "commission_display": format!("{}%", staking::fmt_bps(staking::commission_rate(st, &r.address))),
        "commission_recorded_bps": (r.commission_bps != staking::commission_rate(st, &r.address)).then_some(r.commission_bps),
        "eligible": staking::eligible(st, &r.address),
        "jailed": p.jail.as_ref().map(|j| json!({ "at": j.at, "until": j.until, "reason": j.reason })),
        "tombstoned_at": p.tombstoned_at,
        "slashed_display": fmt_fang(p.slashed),
        "rewards_display": fmt_fang(p.rewards),
        "junk_charged_display": fmt_fang(p.junk_charged),
        "liveness": st.staking.liveness.get(&r.address).map(|l| json!({ "window": l.window, "recorded": l.filled, "missed": l.missed, "max_missed": st.chain_params.downtime_max_missed, "led": l.led, "missed_total": l.missed_total })),
        "registered_at": r.registered_at,
        "approved_at": r.approved_at,
        "approved_by": r.approved_by.as_ref().map(|a| label(v, a)),
        "active_from_epoch": r.active_from,
        "removed_at": r.removed_at,
        "removed_by": r.removed_by.as_ref().map(|a| label(v, a)),
        "active_until_epoch": r.active_until,
        "genesis": v.genesis.is_validator(&r.address),
        // A set of one or two's admission (audit review-fix-staking-2): the height its consensus
        // key last signaled readiness, and the candidate it last admitted (each counts for its
        // own epoch only).
        "ready_at": p.ready_at,
        "admitted": p.admit.as_ref().map(|(c, h)| json!({ "validator": label(v, c), "at": h, "epoch": st.params.epoch_of(*h) })),
    })
}

/// The validator set: who's active this epoch, who joins and leaves at the next boundary, the
/// candidates by stake, who's jailed, and the rules. (Registrations are bonded, so the validator
/// map stays small.)
async fn validators(State(n): State<Node>) -> ApiResult {
    use crate::chain::ValidatorStatus::*;
    use crate::staking;
    let v = n.live.view();
    let st = &v.state;
    let (p, cp) = (&st.params, &st.chain_params);
    let next_height = v.height + 1;
    let epoch = p.epoch_of(next_height);
    let ids = |set: &[String]| -> Vec<Value> { set.iter().filter_map(|a| st.validators.get(a)).map(|r| validator_json(&v, r)).collect() };
    let current = st.active_sets.get(&epoch).cloned().unwrap_or_default();
    let next = staking::select_next(st, epoch);
    let candidates = staking::likely_candidates(st, epoch, usize::MAX);
    let jailed: Vec<String> = st.staking.pools.iter().filter(|(_, p)| p.jail.is_some() && p.tombstoned_at.is_none()).map(|(a, _)| a.clone()).collect();
    let by_status = |s: crate::chain::ValidatorStatus| -> Vec<Value> { st.validators.values().filter(|r| r.status == s).map(|r| validator_json(&v, r)).collect() };
    Ok(Json(json!({
        "epoch": epoch,
        "epoch_blocks": p.epoch_blocks,
        "epoch_ends_at_height": p.last_of(epoch),
        "blocks_until_next_epoch": p.last_of(epoch) + 1 - next_height,
        "active": ids(&current),
        "next_epoch": ids(&next.next),
        "joining": ids(&next.joining),
        "leaving": ids(&next.leaving),
        "candidates": ids(&candidates),
        "jailed": ids(&jailed),
        "tombstoned": by_status(Tombstoned),
        "removed": by_status(Removed),
        "admins": p.admins.iter().map(|a| label(&v, a)).collect::<Vec<_>>(),
        "min_bond": cp.min_self_bond.to_string(),
        "min_bond_display": fmt_fang(cp.min_self_bond),
        "max_validators": cp.max_validators,
        "churn_limit": staking::churn_limit(current.len()),
        // While the set carries over one or two members, its next newcomer is needed for every
        // quorum: it joins only once a quorum of `members` admitted it this epoch and it showed up
        // from `ready_from` on (`null`: newcomers join on stake).
        "admission": staking::members_admitting(st, epoch).map(|m| json!({ "members": m.iter().map(|a| label(&v, a)).collect::<Vec<_>>(), "quorum": crate::epochs::quorum(m.len()), "ready_from": staking::ready_from(p, epoch) })),
        "unbonding_blocks": cp.unbonding_blocks,
        "jail_blocks": cp.jail_blocks,
        "evidence_max_age_epochs": cp.evidence_max_age_epochs,
        "downtime_window": cp.downtime_window,
        "downtime_max_missed": cp.downtime_max_missed,
        "slash_double_sign_display": format!("{}%", staking::fmt_bps(cp.slash_double_sign_bps)),
        "slash_downtime_display": format!("{}%", staking::fmt_bps(cp.slash_downtime_bps)),
        "explainer": "Validators take turns proposing blocks and vote to finalize them; a block is final once more than two-thirds of the active set have signed it (ML-DSA-65), one vote each. Anyone can register a validator with a self-bond and anyone can delegate to one. At each epoch boundary the next set is the top validators by stake, with newcomers at most a third of it, so the rest can always finalize while they sync. Double-signing is slashed and banned for good; a validator that misses too many of its turns is jailed until it unjails.",
    })))
}

/// One account's staking: what it delegated where, what's unbonding (and when it's free), and
/// the validators it is or owns.
async fn staking(State(n): State<Node>, Path(id): Path<String>) -> ApiResult {
    use crate::staking::{self, Entry};
    let v = n.live.view();
    let st = &v.state;
    let addr = st.resolve(&id)?;
    let next_height = v.height + 1;
    let (bonded, unbonding) = staking::holdings(st, &addr);
    let delegations = staking::delegations_of(st, &addr);
    let rows: Vec<Value> = delegations
        .iter()
        .map(|(val, d)| {
            let value = staking::delegation_value(st, &addr, val);
            let own = st.validators.get(val).is_some_and(|r| r.owner == addr);
            json!({ "validator": label(&v, val), "shares": d.shares.to_string(), "value": value.to_string(), "value_display": fmt_fang(value), "principal": d.principal.to_string(), "earned_display": fmt_fang(value.saturating_sub(d.principal)), "self_bond": own })
        })
        .collect();
    let mut ready = 0u64;
    let entries: Vec<Value> = staking::entries_of_account(st, &addr)
        .into_iter()
        .map(|(id, e)| match e {
            Entry::Unbonding { validator, amount, started_at, release_at } => {
                if release_at <= next_height {
                    ready += amount;
                }
                json!({ "id": id, "type": "unbonding", "validator": label(&v, &validator), "amount": amount.to_string(), "amount_display": fmt_fang(amount), "started_at": started_at, "release_at": release_at, "ready": release_at <= next_height })
            }
            Entry::Redelegation { from, to, amount, started_at, release_at } => json!({ "id": id, "type": "redelegation", "from": label(&v, &from), "to": label(&v, &to), "amount": amount.to_string(), "amount_display": fmt_fang(amount), "started_at": started_at, "release_at": release_at }),
        })
        .collect();
    let validators: Vec<Value> = st.validators.values().filter(|r| r.address == addr || r.owner == addr).map(|r| validator_json(&v, r)).collect();
    let who = label(&v, &addr)["name"].as_str().map(String::from).unwrap_or_else(|| short(&addr));
    let staked = match delegations.len() {
        0 => "nothing staked".to_string(),
        1 => format!("{} staked with 1 validator", fmt_fang(bonded)),
        n => format!("{} staked with {n} validators", fmt_fang(bonded)),
    };
    let summary = if unbonding == 0 { format!("{who} has {staked}") } else { format!("{who} has {staked}, and {} unbonding ({} ready to withdraw)", fmt_fang(unbonding), fmt_fang(ready)) };
    Ok(Json(json!({
        "account": label(&v, &addr),
        "bonded": bonded.to_string(),
        "bonded_display": fmt_fang(bonded),
        "unbonding": unbonding.to_string(),
        "unbonding_display": fmt_fang(unbonding),
        "withdrawable": ready.to_string(),
        "withdrawable_display": fmt_fang(ready),
        "delegations": rows,
        "entries": entries,
        "validators": validators,
        "summary": summary,
    })))
}

async fn genesis(State(n): State<Node>) -> ApiResult {
    let v = n.live.view();
    let mut g = serde_json::to_value(&*v.genesis).unwrap();
    g["genesis_hash"] = json!(v.genesis_hash);
    Ok(Json(g))
}

#[derive(Deserialize)]
struct SyncQuery {
    from: u64,
    limit: Option<u64>,
}

/// Block bytes (hex) per `/api/sync` batch: a batch ends at the last certified height inside it.
pub const SYNC_BATCH_BYTES: usize = 8 << 20;
/// A batch with no certified height inside the budget (or the `limit`) goes on to the next one,
/// but never past this (blocks as hex), below the joiner's `MAX_SYNC_RESPONSE` with room for the
/// certificate and the JSON. Certificates are stored at least every `CERT_SPACING_BYTES` of
/// blocks, so an honest node's stretches fit with lots to spare (F09).
pub const SYNC_MAX_BYTES: usize = crate::node::MAX_SYNC_RESPONSE - (4 << 20);

fn no_cert(from: u64, to: u64) -> ApiError {
    ApiError(StatusCode::SERVICE_UNAVAILABLE, format!("no finalization certificate stored for heights {from}..={to} within one response; try another peer"))
}

/// Finalized consensus blocks `[from, to]` plus the finalization certificate of block `to`, so a
/// joining node can verify the whole batch (hash links + one ML-DSA quorum certificate).
/// Served by validators and full nodes from their own block/certificate stores; heights a node
/// pruned get a 410.
async fn sync(State(n): State<Node>, Query(q): Query<SyncQuery>) -> ApiResult {
    use commonware_codec::Encode;
    use commonware_consensus::types::Height;
    let v = n.live.view();
    let tip = v.height;
    let from = q.from.max(1);
    // Before any arithmetic on `from` (a caller's u64::MAX overflowed the epoch math, F61).
    if from > tip {
        return Ok(Json(json!({ "tip": tip, "blocks": [], "to": null, "finalization": null })));
    }
    let epoch_end = v.state.params.last_of(v.state.params.epoch_of(from));
    let range_end = tip.min(epoch_end); // never mix epochs (different validator sets)
    if v.is_pruned(from) {
        return Err(pruned(&v, from));
    }
    let want = (from + q.limit.unwrap_or(100).clamp(1, 500) - 1).min(range_end);
    // Blocks up to `want` within the byte budget, ending at the highest certified height among
    // them; without one, on to the next certified height (if this node holds one in the epoch),
    // within `SYNC_MAX_BYTES`. Either way the bytes read and sent are bounded.
    let next_cert = v.next_cert(from..=range_end);
    if next_cert.is_none() && n.marshal.is_none() {
        return Err(no_cert(from, range_end));
    }
    let (mut blocks, mut cert) = (vec![], None);
    let mut bytes = 0usize;
    let mut last_cert_len = 0usize;
    let mut h = from;
    while h <= next_cert.map_or(want, |c| c.max(want)) {
        if cert.is_some() && h > want {
            break;
        }
        let Some(b) = v.cblock(h) else { break };
        let size = b.len() * 2;
        let within = h <= want && bytes + size <= SYNC_BATCH_BYTES;
        if !blocks.is_empty() && !within && (cert.is_some() || bytes + size > SYNC_MAX_BYTES) {
            break;
        }
        bytes += size;
        blocks.push(hex::encode(&b));
        if let Some(f) = v.cert(h) {
            cert = Some((h, hex::encode(f)));
            last_cert_len = blocks.len();
            if !within {
                break;
            }
        }
        h += 1;
    }
    drop(v);
    // Validators can fall back to marshal's archive for the certificate.
    if cert.is_none() {
        if let Some(m) = &n.marshal {
            for i in (0..blocks.len()).rev().take(64) {
                let h = from + i as u64;
                if let Some(f) = m.get_finalization(Height::new(h)).await {
                    cert = Some((h, hex::encode(f.encode())));
                    last_cert_len = i + 1;
                    break;
                }
            }
        }
    }
    let Some((to, finalization)) = cert else { return Err(no_cert(from, range_end)) };
    blocks.truncate(last_cert_len);
    Ok(Json(json!({ "tip": tip, "blocks": blocks, "to": to, "finalization": finalization })))
}

// ---------------------------------------------------------------- 🪦 Proof of Contribution

fn soul_json(v: &ReadView, addr: &str) -> Value {
    let soul = v.state.soul_of(addr, v.height);
    let earned = v.state.poc.souls.get(addr).map(|s| s.earned).unwrap_or(0);
    json!({ "points": soul, "display": crate::poc::fmt_soul(soul), "earned_display": crate::poc::fmt_soul(earned) })
}

fn ppm_display(ppm: u64) -> String {
    format!("{}%", (ppm + 5_000) / 10_000)
}

fn claim_json(v: &ReadView, cl: &crate::poc::Claim) -> Value {
    let p = &v.state.params.poc;
    let familiar = cl.familiar_key.as_ref().and_then(|k| v.state.familiars.get(&cl.claimant)?.get(k).cloned());
    json!({
        "id": cl.id,
        "claimant": label(v, &cl.claimant),
        "handle": cl.handle,
        "repo": cl.repo,
        "pr": cl.pr,
        "pr_url": format!("https://github.com/{}/pull/{}", cl.repo, cl.pr),
        "familiar": familiar,
        "bond": cl.bond.to_string(),
        "bond_display": fmt_fang(cl.bond),
        "status": cl.status,
        "submitted_at": cl.submitted_at,
        "attestations": cl.attestations.iter().map(|(a, at)| json!({ "validator": label(v, a), "ok": at.ok, "merge_sha": at.merge_sha, "note": at.note, "at": at.at })).collect::<Vec<_>>(),
        "merge_sha": cl.merge_sha,
        "verified_at": cl.verified_at,
        "eligible_at": cl.verified_at.map(|x| x + p.claim_maturity_blocks),
        "expires_at": (cl.status == crate::poc::ClaimStatus::Pending).then(|| cl.submitted_at + p.claim_ttl_blocks),
        "seance": cl.seance,
        "score_ppm": cl.score_ppm,
        "score_display": cl.score_ppm.map(ppm_display),
        "payout": cl.payout.to_string(),
        "payout_display": fmt_fang(cl.payout),
        "soul_awarded_display": crate::poc::fmt_soul(cl.soul_awarded),
        "closed_at": cl.closed_at,
        "reason": cl.reason,
        // GOV-3: validators re-check a verified claim for reverts during its revert window; a
        // BFT quorum of the current set naming one revert commit rejects it (beacon.rs).
        "revert_window_until": (cl.status == crate::poc::ClaimStatus::Verified).then(|| cl.verified_at.map(|x| x + p.claim_maturity_blocks)).flatten(),
        "revert_attestations": revert_attestations_json(v, cl.id),
    })
}

/// A claim's revert attestations (while its window is open): who attested which revert commit,
/// whether they're in the current set, and how many of the set each commit has of the quorum.
fn revert_attestations_json(v: &ReadView, claim_id: u64) -> Value {
    let st = &v.state;
    let Some(atts) = st.beacon.reverts.get(&claim_id) else { return json!(null) };
    let set = st.active_sets.get(&st.params.epoch_of(v.height + 1)).cloned().unwrap_or_default();
    let n = set.len();
    let mut by_sha: BTreeMap<&str, usize> = BTreeMap::new();
    for a in atts.iter().filter(|(val, _)| set.contains(val)).map(|(_, a)| a) {
        *by_sha.entry(a.revert_sha.as_str()).or_default() += 1;
    }
    json!({
        "attestations": atts.iter().map(|(val, a)| json!({ "validator": label(v, val), "revert_sha": a.revert_sha, "at": a.at, "in_current_set": set.contains(val) })).collect::<Vec<_>>(),
        "quorum": n - n.saturating_sub(1) / 3,
        "set": n,
        "by_revert": by_sha.into_iter().map(|(sha, c)| json!({ "revert_sha": sha, "validators": c })).collect::<Vec<_>>(),
    })
}

fn seance_json(v: &ReadView, s: &crate::poc::Seance, full: bool) -> Value {
    let h = v.height;
    let phase = if s.closed { "closed" } else if h <= s.commit_until { "sealing" } else { "revealing" };
    let jurors: Vec<Value> = s
        .jurors
        .iter()
        .map(|(j, seat)| {
            let mut out = json!({
                "juror": label(v, j),
                "pairs": seat.pairs.len(),
                "committed": seat.commitment.is_some(),
                "revealed": seat.votes.is_some(),
                "agreement_ppm": seat.agreement_ppm,
                "agreement_display": seat.agreement_ppm.map(ppm_display),
                "reward_display": fmt_fang(seat.reward),
                "soul_delta_display": format!("{}{}", if seat.soul_delta < 0 { "−" } else if seat.soul_delta > 0 { "+" } else { "" }, crate::poc::fmt_soul(seat.soul_delta.unsigned_abs())),
            });
            if full {
                out["assigned_pairs"] = json!(seat.pairs);
                out["commitment"] = json!(seat.commitment);
                out["votes"] = json!(seat.votes);
                out["spam"] = json!(seat.spam);
            }
            out
        })
        .collect();
    json!({
        "id": s.id,
        "phase": phase,
        "opened_at": s.opened_at,
        "commit_until": s.commit_until,
        "reveal_until": s.reveal_until,
        "seed": s.seed,
        "budget": s.budget.to_string(),
        "budget_display": fmt_fang(s.budget),
        "claims": s.claims.iter().filter_map(|id| v.state.poc.claims.get(id)).map(|cl| claim_json(v, cl)).collect::<Vec<_>>(),
        "jurors": jurors,
        "results": s.results,
        "results_display": s.results.as_ref().map(|r| json!({ "paid": fmt_fang(r.paid), "returned": fmt_fang(r.returned) })),
    })
}

/// Soul, vouches, jury seat, claims and vesting rewards for an account (`/api/account`).
fn contribution_profile(v: &ReadView, addr: &str) -> Value {
    let (st, h) = (&v.state, v.height);
    let p = &st.poc;
    let (total, withdrawable, unvested) = st.grant_totals(addr, h);
    json!({
        "soul": soul_json(v, addr),
        "sybil": p.sybils.get(addr).map(|s| json!({ "at": s.at, "by": label(v, &s.by), "reason": s.reason })),
        "vouched_by": p.vouches.get(addr).map(|x| json!({ "voucher": label(v, &x.voucher), "at": x.at, "bond_display": fmt_fang(x.bond) })),
        "vouching_for": v.vouching_for(addr, ACCOUNT_LIST_LIMIT).into_iter().filter_map(|to| p.vouches.get(&to).map(|x| json!({ "account": label(v, &to), "at": x.at, "unlocks_at": x.at + st.params.poc.vouch_lock_blocks }))).collect::<Vec<_>>(),
        // Withdrawn vouches whose voucher still answers for the vouchee (`poc::DroppedVouch`):
        // the bond is held until the lock is over (and while a FlagSybil proposal names it).
        "vouches_withdrawn": st.dropped_vouches_of(addr).into_iter().filter(|(_, d)| d.bond > 0).take(ACCOUNT_LIST_LIMIT).map(|(by, d)| json!({ "voucher": label(v, &by), "dropped_at": d.dropped_at, "held_until": d.at + st.params.poc.vouch_lock_blocks })).collect::<Vec<_>>(),
        "vouch_bonds_held": p.dropped_vouches.iter().filter(|(k, d)| d.bond > 0 && k.rsplit_once('/').is_some_and(|(_, by)| by == addr)).take(ACCOUNT_LIST_LIMIT).map(|(k, d)| json!({ "account": label(v, k.split_once('/').map(|(to, _)| to).unwrap_or_default()), "bond_display": fmt_fang(d.bond), "dropped_at": d.dropped_at, "held_until": d.at + st.params.poc.vouch_lock_blocks })).collect::<Vec<_>>(),
        "juror": p.jurors.get(addr).map(|r| json!({ "bond_display": fmt_fang(r.bond), "active": r.active, "served": r.served, "joined_at": r.joined_at })),
        "claims": v.ids(ix::CLAIMANT, addr, true, 50).into_iter().filter_map(|id| p.claims.get(&id)).map(|cl| claim_json(v, cl)).collect::<Vec<_>>(),
        "next_claim_bond": st.claim_bond(addr).to_string(),
        "next_claim_bond_display": fmt_fang(st.claim_bond(addr)),
        "max_open_claims": st.max_open_claims(addr, h),
        "rewards": {
            "total_display": fmt_fang(total),
            "withdrawable": withdrawable.to_string(),
            "withdrawable_display": fmt_fang(withdrawable),
            "unvested_display": fmt_fang(unvested),
            "grants": p.grants.get(addr).map(|g| g.values().map(|g| json!({ "id": g.id, "amount_display": fmt_fang(g.amount), "withdrawn_display": fmt_fang(g.withdrawn), "start": g.start, "end": g.end, "source": g.source })).collect::<Vec<_>>()).unwrap_or_default(),
        },
    })
}

/// The contribution pool at a glance: balance, repos, rules, the open Séance, recent ones.
async fn contributions(State(n): State<Node>) -> ApiResult {
    let v = n.live.view();
    let st = &v.state;
    let p = &st.poc;
    let t = &st.params.poc;
    let pool = st.account(&st.pool()).balance;
    let claims = v.claims();
    use crate::poc::*;
    Ok(Json(json!({
        "active": p.is_active(),
        "pool": { "account": label(&v, &st.pool()), "balance": pool.to_string(), "balance_display": fmt_fang(pool), "next_budget_display": fmt_fang((pool as u128 * SEANCE_POOL_BPS as u128 / 10_000) as u64) },
        "repos": p.repos.iter().map(|(r, w)| json!({ "repo": r, "weight": w })).collect::<Vec<_>>(),
        "open_seance": p.open_seance.and_then(|id| p.seances.get(&id)).map(|s| seance_json(&v, s, false)),
        "next_seance_at": p.next_seance_at,
        "recent_seances": p.seances.values().rev().filter(|s| s.closed).take(10).map(|s| json!({ "id": s.id, "opened_at": s.opened_at, "closed_at": s.reveal_until, "claims": s.claims.len(), "jurors": s.jurors.len(), "budget_display": fmt_fang(s.budget), "results": s.results })).collect::<Vec<_>>(),
        "claims": claims.counts,
        "jurors": { "pool": claims.jurors, "active": claims.active_jurors },
        "rules": {
            "seance_blocks": t.seance_blocks, "commit_blocks": t.commit_blocks(), "reveal_blocks": t.reveal_blocks(),
            "claim_maturity_blocks": t.claim_maturity_blocks, "claim_ttl_blocks": t.claim_ttl_blocks, "vesting_blocks": t.vesting_blocks,
            "vouch_lock_blocks": t.vouch_lock_blocks, "soul_half_life_blocks": t.soul_half_life_blocks,
            "claim_bond_display": fmt_fang(CLAIM_BOND), "vouch_bond_display": fmt_fang(VOUCH_BOND), "juror_bond_display": fmt_fang(JUROR_BOND),
            "vouch_min_soul": fmt_soul(VOUCH_MIN_SOUL), "juror_min_soul": fmt_soul(JUROR_MIN_SOUL),
            "jury_size": JURY_SIZE, "min_jury": MIN_JURY, "pairs_per_juror": PAIRS_PER_JUROR, "max_claims_per_seance": MAX_SEANCE_CLAIMS,
            "pool_share_per_seance": ppm_display(SEANCE_POOL_BPS * 100), "juror_share": ppm_display(JUROR_SHARE_BPS * 100), "min_score": ppm_display(MIN_SCORE_PPM),
            // The budget reaches the full share at this many people; within a Séance a person's
            // claims share one win rate pooled over all their comparisons, and each after the
            // first counts 1/`claim_rank_divisor` of the one above it.
            "full_budget_people": FULL_BUDGET_PEOPLE, "min_people": 2, "claim_rank_divisor": 1u64 << CLAIM_RANK_SHIFT,
        },
    })))
}

#[derive(Deserialize)]
struct ClaimQuery {
    status: Option<String>,
    account: Option<String>,
    limit: Option<usize>,
}

/// Claims, newest first. `?status=pending|verified|in_seance|scored|spam|rejected&account=`.
/// By account from the claimant index; by status from the per-block memo.
async fn claims(State(n): State<Node>, Query(q): Query<ClaimQuery>) -> ApiResult {
    let v = n.live.view();
    let st = &v.state;
    let limit = q.limit.unwrap_or(100).min(500);
    let all = &st.poc.claims;
    let status_ok = |cl: &crate::poc::Claim| q.status.as_deref().is_none_or(|s| serde_json::to_value(cl.status).unwrap() == s);
    let list: Vec<Value> = match q.account.as_deref().map(|a| st.resolve(a)).transpose()? {
        Some(who) => v.ids(ix::CLAIMANT, &who, true, usize::MAX).into_iter().filter_map(|id| all.get(&id)).filter(|cl| status_ok(cl)).take(limit).map(|cl| claim_json(&v, cl)).collect(),
        None => match q.status.as_deref() {
            Some(s) => v.claims().by_status.get(s).map(Vec::as_slice).unwrap_or(&[]).iter().take(limit).filter_map(|id| all.get(id)).map(|cl| claim_json(&v, cl)).collect(),
            None => all.values().rev().take(limit).map(|cl| claim_json(&v, cl)).collect(),
        },
    };
    Ok(Json(json!({ "count": all.len(), "claims": list })))
}

async fn claim(State(n): State<Node>, Path(id): Path<u64>) -> ApiResult {
    let v = n.live.view();
    let cl = v.state.poc.claims.get(&id).ok_or_else(|| not_found("claim"))?;
    Ok(Json(claim_json(&v, cl)))
}

/// A Séance with every juror's assigned pairs (what they must judge), commitments and reveals.
async fn seance(State(n): State<Node>, Path(id): Path<u64>) -> ApiResult {
    let v = n.live.view();
    let s = v.state.poc.seances.get(&id).ok_or_else(|| not_found("Séance"))?;
    Ok(Json(seance_json(&v, s, true)))
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
}

/// Soul leaderboard (effective Soul, after decay), ranked once per block.
async fn souls(State(n): State<Node>, Query(q): Query<LimitQuery>) -> ApiResult {
    let v = n.live.view();
    let st = &v.state;
    let rows: Vec<Value> = v
        .souls()
        .iter()
        .take(q.limit.unwrap_or(50).min(500))
        .map(|(_, a)| json!({ "account": label(&v, a), "soul": soul_json(&v, a), "github": st.github_by_account.get(a), "juror": st.poc.jurors.get(a).is_some_and(|r| r.active), "vouched_by": st.poc.vouches.get(a).map(|x| label(&v, &x.voucher)) }))
        .collect();
    Ok(Json(json!({ "souls": rows })))
}

// ---------------------------------------------------------------- 🏛️ Governance

/// A u128 amount (vote tallies) as a string, with its display.
fn big_amount(drops: u128) -> Value {
    json!({ "drops": drops.to_string(), "display": fmt_fang(drops.min(u64::MAX as u128) as u64) })
}

fn proposal_json(v: &ReadView, p: &crate::governance::Proposal) -> Value {
    let st = &v.state;
    json!({
        "id": p.id,
        "title": p.title,
        "url": p.url,
        "kind": p.kind,
        "summary": crate::governance::describe_proposal(st, p, &|a| v.primary_name(a)),
        "proposer": label(v, &p.proposer),
        "deposit": p.deposit.to_string(),
        "deposit_display": fmt_fang(p.deposit),
        "status": p.status,
        "created_at": p.created_at,
        "voting_end": p.voting_end,
        "execute_at": p.execute_at,
        // Decided at the close; until then, what it would need now.
        "supermajority": p.tally.as_ref().map(|t| t.supermajority).unwrap_or_else(|| crate::governance::needs_supermajority(st, &p.kind)),
        // Whether the bootstrap council's ⅔ can pass it (not the token holders' parameters).
        "council_may_pass": crate::governance::council_may_pass(&p.kind),
        "ayes": { "amount": big_amount(p.aye_amount), "weight": big_amount(p.aye_weight) },
        "nays": { "amount": big_amount(p.nay_amount), "weight": big_amount(p.nay_weight) },
        "voters": p.voters,
        "council": p.council.iter().map(|(val, c)| json!({ "validator": label(v, val), "aye": c.aye, "voter": label(v, &c.voter) })).collect::<Vec<_>>(),
        "tally": p.tally.as_ref().map(|t| json!({
            "unlocked_supply": big_amount(t.unlocked),
            "quorum": big_amount(t.quorum),
            "quorum_reached": t.quorum_reached,
            "supermajority": t.supermajority,
            "council_size": t.council_size,
            "council_ayes": t.council_ayes,
            "council_nays": t.council_nays,
        })),
        "outcome": p.outcome,
    })
}

#[derive(Deserialize)]
struct ProposalQuery {
    status: Option<String>,
    limit: Option<usize>,
}

/// Proposals, newest first (`?status=voting|queued|rejected|executed|failed`), with the rules
/// they're judged by. Proposals are few (each bonds a deposit), so this walks their map.
async fn proposals(State(n): State<Node>, Query(q): Query<ProposalQuery>) -> ApiResult {
    let v = n.live.view();
    let st = &v.state;
    let (g, cp) = (&st.governance, &st.chain_params);
    let list: Vec<Value> = g.proposals.values().rev().filter(|p| q.status.as_deref().is_none_or(|s| p.status.as_str() == s)).take(q.limit.unwrap_or(50).min(500)).map(|p| proposal_json(&v, p)).collect();
    let balance = |a: &str| st.account(a).balance;
    let community = crate::chain::community(st.params.prefix());
    Ok(Json(json!({
        "count": g.proposals.len(),
        "next_id": g.next_proposal_id,
        // A distribution id no distribution or open proposal uses: what a new tree is built for.
        "next_distribution_id": crate::governance::next_distribution_id(st),
        // The largest id a `Distribution` proposed now may name (the next proposal id + 1,000).
        "max_distribution_id": crate::governance::max_distribution_id(st),
        "council_active": v.height < cp.bootstrap_end_height,
        "spending_open": v.height >= cp.governance_spend_height,
        "params": {
            "proposal_deposit": cp.proposal_deposit.to_string(),
            "proposal_deposit_display": fmt_fang(cp.proposal_deposit),
            "voting_blocks": cp.voting_blocks,
            "timelock_blocks": cp.timelock_blocks,
            "lock_period_blocks": cp.lock_period_blocks,
            "quorum_bps": cp.quorum_bps,
            "quorum_floor": cp.quorum_floor.to_string(),
            "quorum_floor_display": fmt_fang(cp.quorum_floor),
            "supermajority_bps": cp.supermajority_bps,
            "treasury_large_bps": cp.treasury_large_bps,
            "governance_spend_height": cp.governance_spend_height,
            "bootstrap_end_height": cp.bootstrap_end_height,
            "multisig_expiry_blocks": cp.multisig_expiry_blocks,
        },
        "treasury": { "account": label(&v, &st.treasury()), "balance": balance(&st.treasury()).to_string(), "balance_display": fmt_fang(balance(&st.treasury())) },
        "community": { "account": label(&v, &community), "balance": balance(&community).to_string(), "balance_display": fmt_fang(balance(&community)) },
        "proposals": list,
    })))
}

/// One proposal and its votes (up to 500, by voter). Once a vote closes its tally is final and
/// its vote records are swept over the next blocks (`votes_swept`; the txs stay in history).
async fn proposal(State(n): State<Node>, Path(id): Path<u64>) -> ApiResult {
    let v = n.live.view();
    let g = &v.state.governance;
    let p = g.proposals.get(&id).ok_or_else(|| not_found("proposal"))?;
    let votes: Vec<Value> = g
        .votes
        .range(crate::governance::IdKey::new(id, "")..)
        .take_while(|(k, _)| k.id == id)
        .take(500)
        .map(|(k, r)| json!({ "voter": label(&v, &k.account), "aye": r.aye, "amount": r.amount.to_string(), "amount_display": fmt_fang(r.amount), "conviction": r.conviction, "weight": big_amount(r.weight), "locked_until": r.locked_until, "council": r.council.as_ref().map(|c| label(&v, c)), "at": r.at }))
        .collect();
    let mut out = proposal_json(&v, p);
    out["votes"] = json!(votes);
    out["votes_swept"] = json!(p.status != crate::governance::ProposalStatus::Voting && !g.closed_votes.contains(&id));
    out["distribution"] = json!(g.distributions.contains_key(&id).then(|| format!("/api/distribution/{id}")));
    Ok(Json(out))
}

#[derive(Deserialize)]
struct DistributionQuery {
    /// Whether this account (address or name) has claimed.
    address: Option<String>,
}

/// A community distribution: what's left, until when, and (with `?address=`) one claim.
async fn distribution(State(n): State<Node>, Path(id): Path<u64>, Query(q): Query<DistributionQuery>) -> ApiResult {
    let v = n.live.view();
    let st = &v.state;
    let d = st.governance.distributions.get(&id).ok_or_else(|| not_found("distribution"))?;
    let claim = match &q.address {
        Some(a) => {
            let addr = st.resolve(a)?;
            let c = st.governance.distribution_claims.get(&crate::governance::IdKey::new(id, &addr));
            Some(json!({ "account": label(&v, &addr), "claimed": c.map(|c| json!({ "amount": c.amount.to_string(), "amount_display": fmt_fang(c.amount), "at": c.at })) }))
        }
        None => None,
    };
    let remaining = d.total - d.claimed;
    Ok(Json(json!({
        "id": d.id,
        "proposal": d.proposal,
        "merkle_root": d.merkle_root,
        "total": d.total.to_string(),
        "total_display": fmt_fang(d.total),
        "claimed": d.claimed.to_string(),
        "claimed_display": fmt_fang(d.claimed),
        "remaining": remaining.to_string(),
        "remaining_display": fmt_fang(remaining),
        "claims": d.claims,
        "opened_at": d.opened_at,
        "claim_until": d.claim_until,
        "open": d.closed_at.is_none() && v.height <= d.claim_until,
        "closed_at": d.closed_at,
        "returned": d.returned.to_string(),
        "returned_display": fmt_fang(d.returned),
        "claim": claim,
    })))
}

/// A multisig account: its threshold, owner keys and open proposals.
async fn multisig(State(n): State<Node>, Path(id): Path<String>) -> ApiResult {
    let v = n.live.view();
    let st = &v.state;
    let addr = st.resolve(&id)?;
    let m = st.governance.multisigs.get(&addr).ok_or_else(|| not_found("multisig account"))?;
    let owners = crate::governance::owner_keys(st, &addr);
    let keys = st.keys_of(&addr);
    let pending: Vec<Value> = m
        .pending
        .iter()
        .filter_map(|pid| st.governance.multisig_proposals.get(pid))
        .map(|p| {
            let tx = Tx { chain_id: String::new(), genesis_hash: String::new(), from: addr.clone(), nonce: 0, fee: 0, tip: 0, valid_until: 0, action: p.action.clone(), memo: String::new(), public_key: String::new(), signature: String::new() };
            json!({
                "id": p.id,
                "action": p.action,
                "summary": v.describe(&tx, None),
                "proposed_by": p.proposed_by,
                "approvals": p.approvals,
                "valid_approvals": p.approvals.iter().filter(|k| owners.contains(*k)).count(),
                "created_at": p.created_at,
                "expires_at": p.expires_at,
                "expired": v.height > p.expires_at,
                // Why no approval can file it any more (an upgrade proposal a cancellation
                // superseded), or null.
                "superseded": crate::governance::superseded_multisig_action(st, p),
            })
        })
        .collect();
    Ok(Json(json!({
        "account": label(&v, &addr),
        "threshold": m.threshold,
        "owners": owners.iter().map(|k| json!({ "key_id": k, "label": keys.get(k).map(|r| r.label.clone()) })).collect::<Vec<_>>(),
        "created_at": m.created_at,
        "pending": pending,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, v.parse().unwrap());
        }
        h
    }

    #[test]
    fn client_ip_headers_are_only_trusted_when_configured() {
        let peer: SocketAddr = "203.0.113.7:5000".parse().unwrap();
        let spoof = headers(&[("fly-client-ip", "1.2.3.4"), ("x-forwarded-for", "5.6.7.8"), ("x-blockula-client-ip", "9.9.9.9")]);
        // Default: proxy and forwarder headers are ignored.
        assert_eq!(ClientIpPolicy::default().resolve(peer, &spoof), Some(peer.ip()));
        // Behind Fly: only the configured header counts.
        let fly = ClientIpPolicy { header: Some("fly-client-ip".into()), ..Default::default() };
        assert_eq!(fly.resolve(peer, &spoof), Some("1.2.3.4".parse().unwrap()));
        // F29: an appending proxy's header is read from the right (what the proxy saw), never
        // the client's own leftmost entries; with two proxies, the second entry from the right.
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let xff = ClientIpPolicy { header: Some("x-forwarded-for".into()), ..Default::default() };
        let forged = headers(&[("x-forwarded-for", "9.9.9.9, 203.0.113.7")]);
        assert_eq!(xff.resolve(local, &forged), Some("203.0.113.7".parse().unwrap()));
        let two = ClientIpPolicy { hops: 2, ..xff.clone() };
        assert_eq!(two.resolve(local, &headers(&[("x-forwarded-for", "9.9.9.9, 203.0.113.7, 10.0.0.9")])), Some("203.0.113.7".parse().unwrap()));
        // Unparseable or missing: limited as the proxy, never treated as a local, unlimited caller.
        assert_eq!(xff.resolve(local, &headers(&[("x-forwarded-for", "203.0.113.7, nope")])), Some(local.ip()));
        assert_eq!(xff.resolve(local, &HeaderMap::new()), Some(local.ip()));
        assert_eq!(two.resolve(local, &headers(&[("x-forwarded-for", "203.0.113.7")])), Some(local.ip()), "fewer entries than proxies");
        // A proxy that appends its own header line (HAProxy `option forwardfor`) after the
        // client's: every line counts, in order, so the client's first line can't name it.
        let lines = |vals: &[&str]| {
            let mut h = HeaderMap::new();
            for v in vals {
                h.append("x-forwarded-for", v.parse().unwrap());
            }
            h
        };
        assert_eq!(xff.resolve(local, &lines(&["9.9.9.9", "203.0.113.7"])), Some("203.0.113.7".parse().unwrap()));
        assert_eq!(xff.resolve(local, &lines(&["8.8.8.8, 9.9.9.9", "203.0.113.7"])), Some("203.0.113.7".parse().unwrap()));
        assert_eq!(two.resolve(local, &lines(&["9.9.9.9", "203.0.113.7", "10.0.0.9"])), Some("203.0.113.7".parse().unwrap()), "hops count entries across lines");
        assert_eq!(two.resolve(local, &lines(&["9.9.9.9, 203.0.113.7", "10.0.0.9"])), Some("203.0.113.7".parse().unwrap()));
        let mut binary = lines(&[]);
        binary.append("x-forwarded-for", axum::http::HeaderValue::from_bytes(b"\xff\xfe, 9.9.9.9").unwrap());
        binary.append("x-forwarded-for", "203.0.113.7".parse().unwrap());
        assert_eq!(xff.resolve(local, &binary), Some("203.0.113.7".parse().unwrap()), "a client line that isn't text doesn't hide the proxy's");
        assert_eq!(ClientIpPolicy::default().resolve(local, &forged), None, "no header configured: loopback is local");
        // Forwarder secret: wrong key is ignored, right key is honored.
        let secret = ClientIpPolicy { forwarder_secret: Some("s".repeat(32)), ..Default::default() };
        let wrong = headers(&[("x-blockula-forwarder-key", "nope"), ("x-blockula-client-ip", "9.9.9.9")]);
        assert_eq!(secret.resolve(peer, &wrong), Some(peer.ip()));
        let right = headers(&[("x-blockula-forwarder-key", &"s".repeat(32)), ("x-blockula-client-ip", "9.9.9.9")]);
        assert_eq!(secret.resolve(peer, &right), Some("9.9.9.9".parse().unwrap()));
        // A spoofed Fly header can't impersonate a trusted forwarder IP.
        let lan = ClientIpPolicy { trusted_forwarders: vec!["10.0.0.2".parse().unwrap()], ..Default::default() };
        let fake = headers(&[("fly-client-ip", "10.0.0.2"), ("x-blockula-client-ip", "9.9.9.9")]);
        assert_eq!(lan.resolve(peer, &fake), Some(peer.ip()));
        // A node bound to `::` (dual stack) sees IPv4 peers as IPv4-mapped: 127.0.0.1 is still
        // a local caller, a trusted forwarder still trusted, and anyone else limited by their
        // IPv4 address.
        let mapped = |s: &str| SocketAddr::new(IpAddr::V6(s.parse::<std::net::Ipv4Addr>().unwrap().to_ipv6_mapped()), 5000);
        assert_eq!(ClientIpPolicy::default().resolve(mapped("127.0.0.1"), &HeaderMap::new()), None);
        assert_eq!(lan.resolve(mapped("10.0.0.2"), &headers(&[("x-blockula-client-ip", "9.9.9.9")])), Some("9.9.9.9".parse().unwrap()));
        assert_eq!(ClientIpPolicy::default().resolve(mapped("203.0.113.7"), &HeaderMap::new()), Some(peer.ip()));
    }

    /// Audit F40: a full node forwards a signed tx with its client's IP (for the validator's
    /// per-IP limits) but an unsigned ghost tx without it.
    #[test]
    fn unsigned_ghost_txs_are_forwarded_without_the_client_ip() {
        let ip: Option<IpAddr> = Some("203.0.113.9".parse().unwrap());
        let ghost = crate::types::tests::vector_tx(Action::Unshield { to: "x.boo".into(), amount: 1, bundle: crate::types::tests::vector_bundle(1) });
        let signed = crate::types::tests::vector_tx(Action::Transfer { to: "x.boo".into(), amount: 1 });
        let shield = crate::types::tests::vector_tx(Action::Shield { amount: 1, bundle: crate::types::tests::vector_bundle(1) });
        assert!(ghost.is_unsigned() && !signed.is_unsigned() && !shield.is_unsigned());
        assert_eq!(forwarded_client_ip(&ghost, ip), None);
        assert_eq!((forwarded_client_ip(&signed, ip), forwarded_client_ip(&shield, ip)), (ip, ip), "signed txs (a shield too) name their sender anyway");
    }

    /// F64: the explorer resolves routes against its own views only (`#/constructor/<html>` used
    /// to reach `Object`, which handed the decoded id to innerHTML unescaped), and never puts a
    /// non-string into innerHTML. Runs the page's script under Node with a stub DOM (skipped
    /// where `node` isn't installed; CI has it).
    #[test]
    fn explorer_routes_only_to_its_own_views() {
        let Ok(out) = std::process::Command::new("node").arg("--version").output() else {
            eprintln!("skipping: node isn't installed");
            return;
        };
        assert!(out.status.success());
        let stub = r##"
const app = { innerHTML: "" };
globalThis.document = { querySelector: (s) => (s === "#app" ? app : { addEventListener() {}, textContent: "", value: "" }), querySelectorAll: () => [], head: { append() {} }, createElement: () => ({}) };
globalThis.window = { addEventListener() {} };
globalThis.setInterval = () => 0;
globalThis.fetch = async () => ({ ok: false, json: async () => ({ error: "offline" }) });
globalThis.location = { hash: "" };
"##;
        let probe = r##"
(async () => {
  const out = {};
  for (const [name, hash] of Object.entries({ constructor: "#/constructor/%3Cimg%20src%3Dx%20onerror%3Dalert(1)%3E", proto: "#/__proto__/%3Cb%3Ex%3C%2Fb%3E", toString: "#/toString/%3Cb%3Ex%3C%2Fb%3E", hasOwnProperty: "#/hasOwnProperty/%3Cb%3Ex%3C%2Fb%3E" })) {
    location.hash = hash;
    await route();
    out[name] = app.innerHTML;
  }
  console.log(JSON.stringify(out));
})();
"##;
        let src = format!("{stub}{}{probe}", explorer_script());
        let out = std::process::Command::new("node").arg("-e").arg(&src).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let pages: BTreeMap<String, String> = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(pages.len(), 4);
        for (route, html) in &pages {
            // Each falls back to the home view, which fails offline: the escaped error card.
            assert!(!html.contains("<img") && !html.contains("<b>"), "{route}: {html}");
            assert!(html.contains("offline"), "{route}: {html}");
        }
    }

    /// The explorer's tx page for one `/api/tx` answer, rendered by the page's own script under
    /// Node with a stub DOM; `None` where `node` isn't installed (CI has it).
    pub(super) fn explorer_tx_page(answer: &Value) -> Option<String> {
        let ok = std::process::Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success());
        if !ok {
            eprintln!("skipping: node isn't installed");
            return None;
        }
        let stub = format!(
            r##"
const app = {{ innerHTML: "" }};
globalThis.document = {{ querySelector: (s) => (s === "#app" ? app : {{ addEventListener() {{}}, textContent: "", value: "" }}), querySelectorAll: () => [], head: {{ append() {{}} }}, createElement: () => ({{}}) }};
globalThis.window = {{ addEventListener() {{}} }};
globalThis.setInterval = () => 0;
const ANSWER = {answer};
globalThis.fetch = async (p) => (String(p).startsWith("/api/tx/") ? {{ ok: true, json: async () => ANSWER }} : {{ ok: false, json: async () => ({{ error: "offline" }}) }});
globalThis.location = {{ hash: "#/tx/{id}" }};
"##,
            id = "ab".repeat(32)
        );
        // The page routes once as it loads; this second pass is the one awaited.
        let probe = r##"
(async () => {
  await route();
  console.log(JSON.stringify(app.innerHTML));
})();
"##;
        let src = format!("{stub}{}{probe}", explorer_script());
        let out = std::process::Command::new("node").arg("-e").arg(&src).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        Some(serde_json::from_slice(&out.stdout).unwrap())
    }

    /// review-fix-clients-2: the node's explorer (which the MCP's in-flight answers link to) tells a
    /// tx dropped unrun (nothing charged; another node may still include it, so it must not be sent
    /// again yet) from a failure that ran (its fee was charged; final), and calls a drop "failed"
    /// nowhere. An older node's tx-less failure, which doesn't say, reads as a drop: the answer that
    /// never invites a re-send.
    #[test]
    fn explorer_tells_a_drop_from_a_failure_that_ran() {
        const DROPPED: &str = "dropped from this node's mempool without running";
        const NOT_YET: &str = "nothing was charged, but another node may still include it until its valid_until, so don't send it again before then";
        const RAN: &str = "failed in <a href=\"#/block/12\">block #12</a>: its fee was charged";
        let id = "ab".repeat(32);
        let dropped = json!({ "id": id, "status": "failed", "dropped": true, "error": "expired: valid_until 11", "height": 12, "summary": "Transaction dropped without running: expired" });
        let older = json!({ "id": id, "status": "failed", "error": "expired", "height": 12, "summary": "Transaction failed: expired" });
        let ran = json!({ "id": id, "status": "failed", "dropped": false, "error": "insufficient balance", "height": 12, "summary": "Transaction failed: insufficient balance" });
        for (what, answer) in [("dropped", &dropped), ("an older node's tx-less failure", &older)] {
            let Some(page) = explorer_tx_page(answer) else { return };
            assert!(page.contains(DROPPED) && page.contains(NOT_YET), "{what}: {page}");
            assert!(page.contains(r#"<span class="pill pending">dropped</span>"#), "{what}: a non-final pill: {page}");
            assert!(!page.contains("pill failed") && !page.contains("fee was charged") && !page.contains("didn't go through"), "{what}: {page}");
        }
        let Some(page) = explorer_tx_page(&ran) else { return };
        assert!(page.contains(RAN) && page.contains("final"), "{page}");
        assert!(page.contains(r#"<span class="pill failed">failed</span>"#), "{page}");
        assert!(!page.contains(DROPPED) && !page.contains("may still include"), "{page}");
        // Node text stays text.
        let hostile = json!({ "id": id, "status": "failed", "dropped": true, "error": "<img src=x onerror=alert(1)>", "height": "<b>12</b>", "summary": "" });
        let Some(page) = explorer_tx_page(&hostile) else { return };
        assert!(!page.contains("<img") && !page.contains("<b>"), "{page}");
    }

    /// The p2p transport `/api/status` reports is the one linked (audit #1/#3). Compiling
    /// `p2p_transport` already proves the node links the vendored crate; the lockfile check
    /// proves nothing else (commonware-p2p) links a second, upstream copy, which an unapplied or
    /// version-mismatched `[patch.crates-io]` would add with only a cargo warning.
    #[test]
    fn vendored_stream_is_linked() {
        let t = p2p_transport();
        assert!(t.starts_with("hybrid X25519 + ML-KEM-768") && t.contains("_BLOCKULA_STREAM_HANDSHAKE_X25519MLKEM768_V1"), "{t}");
        let streams: Vec<&str> = include_str!("../Cargo.lock").split("[[package]]").filter(|p| p.contains("\nname = \"commonware-stream\"\n")).collect();
        assert_eq!(streams.len(), 1, "exactly one commonware-stream in the build graph: {streams:?}");
        assert!(!streams[0].contains("\nsource = "), "commonware-stream comes from node/vendor (a path dependency), not a registry: {}", streams[0]);
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use crate::chain::SummonOpts;
    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::Request;
    use tower::ServiceExt as _;

    fn node(tag: &str) -> (std::path::PathBuf, Node) {
        let root = std::env::temp_dir().join(format!("blockula-api-{tag}-{}-{}", crate::chain::now_ms(), std::process::id()));
        let homes = Chain::summon(&root, &SummonOpts::new(tag, 1)).unwrap();
        let chain = Arc::new(Mutex::new(Chain::open(&homes[0]).unwrap()));
        (root, Node::new(chain, None, vec![], ClientIpPolicy::default()))
    }

    async fn call(n: &Node, method: &str, uri: &str, body: Option<Value>, peer: &str) -> (StatusCode, Value, String) {
        let app = router(n.clone()).layer(MockConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        let req = Request::builder().method(method).uri(uri).header("content-type", "application/json");
        let req = req.body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty)).unwrap();
        let res = app.oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null), text)
    }

    #[tokio::test]
    async fn forwarding_supports_https_peers() {
        let (root, mut n) = node("tlsfwd");
        n.peers = vec!["https://127.0.0.1:9".into()];
        let err = n.forward("/faucet", &json!({ "to": "x" }), None).await.err().unwrap();
        assert_eq!(err.0, StatusCode::BAD_GATEWAY);
        assert!(!err.1.contains("scheme"), "{}", err.1);
        assert!(err.1.contains("couldn't reach"), "{}", err.1);
        std::fs::remove_dir_all(root).ok();
    }

    /// `call` for big responses: the status and the body's size, and the JSON when it parses.
    async fn get_big(n: &Node, uri: &str) -> (StatusCode, usize, Value) {
        let app = router(n.clone()).layer(MockConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));
        let res = app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 256 << 20).await.unwrap();
        (status, bytes.len(), serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// Executes a block whose consensus bytes are `size` long, with a certificate if `cert`.
    fn big_block(n: &Node, size: usize, cert: bool) {
        let mut c = n.chain.lock();
        let mut f = c.test_block(vec![]);
        f.cblock = vec![7; size];
        f.certificate = cert.then(|| vec![0xce; 64]);
        c.execute(f).unwrap();
    }

    /// F64: `GET /` carries a CSP that only lets the explorer's own inline script run (by its
    /// SHA-256), so an injected handler or script doesn't run even if a view forgot to escape.
    #[tokio::test]
    async fn explorer_is_served_with_a_script_hash_csp() {
        assert_eq!([base64(b""), base64(b"f"), base64(b"fo"), base64(b"foo"), base64(b"foobar")], ["", "Zg==", "Zm8=", "Zm9v", "Zm9vYmFy"]);
        {
            use commonware_cryptography::Hasher as _;
            assert_eq!(base64(commonware_cryptography::Sha256::hash(&[b"abc"]).as_ref()), "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=");
        }
        assert_eq!(EXPLORER.matches("<script").count(), 1, "one inline script, the one the CSP hashes");
        assert!(explorer_script().contains("async function route()"));
        let html = EXPLORER.split("<script>").next().unwrap();
        assert!(!html.contains(" onclick=") && !html.contains(" onload=") && !html.contains(" onerror="), "no inline handlers: the CSP would block them");
        let (root, n) = node("csp");
        let res = router(n).oneshot(Request::builder().uri("/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let csp = res.headers()[axum::http::header::CONTENT_SECURITY_POLICY].to_str().unwrap().to_string();
        assert!(csp.contains("default-src 'none'") && csp.contains("connect-src 'self'") && csp.contains("frame-ancestors 'none'"), "{csp}");
        let script_src = csp.split(';').find(|d| d.trim().starts_with("script-src")).unwrap();
        assert!(!script_src.contains("unsafe-inline") && script_src.contains("'sha256-"), "{script_src}");
        // Only what the hashed script adds runs: no origin (not even 'self') may serve script by URL.
        assert!(script_src.contains("'strict-dynamic'") && !script_src.contains("'self'") && !script_src.contains("http"), "{script_src}");
        assert_eq!(res.headers()[axum::http::header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        std::fs::remove_dir_all(root).ok();
    }

    /// F61/F65: `from` near u64::MAX used to overflow the epoch math before the `from > tip`
    /// check (a panic per request); the epoch helpers saturate too.
    #[tokio::test]
    async fn sync_from_u64_max_is_an_empty_batch() {
        let (root, n) = node("syncmax");
        n.chain.lock().produce_block_with(vec![]).unwrap();
        for from in [u64::MAX, u64::MAX - 1, 1 << 63, 2] {
            let (s, b, _) = call(&n, "GET", &format!("/api/sync?from={from}&limit=500"), None, "127.0.0.1:1").await;
            assert_eq!((s, b["tip"].as_u64(), b["blocks"].as_array().map(Vec::len)), (StatusCode::OK, Some(1), Some(0)), "from={from}");
        }
        let p = n.chain.lock().state.params.clone();
        assert_eq!(p.last_of(p.epoch_of(u64::MAX)), u64::MAX);
        assert_eq!((p.last_of(0), p.last_of(1)), (p.epoch_blocks - 1, 2 * p.epoch_blocks - 1));
        assert!(p.is_boundary(p.epoch_blocks - 1) && !p.is_boundary(p.epoch_blocks));
        assert_eq!(p.is_boundary(u64::MAX), u64::MAX % p.epoch_blocks == p.epoch_blocks - 1);
        std::fs::remove_dir_all(root).ok();
    }

    /// F09: a full node (no marshal fallback) stores a certificate at least every
    /// `CERT_SPACING_BYTES` of blocks, so a joiner's batch over big blocks always has one (after a
    /// restart too, from disk); a stretch with no certificate inside the byte budget is extended to
    /// the next stored one within `SYNC_MAX_BYTES` instead of answering 503; and no response, not
    /// even `limit=1`'s, goes past `SYNC_MAX_BYTES` of blocks.
    #[tokio::test]
    async fn sync_batches_over_big_blocks_end_at_a_stored_certificate_within_the_cap() {
        let (root, n) = node("syncbig");
        let size = 300_000;
        // Certificates for every block: kept on disk by bytes (every 4th block here), not only at
        // multiples of 100.
        for _ in 0..40 {
            big_block(&n, size, true);
        }
        let home = n.chain.lock().home.clone();
        drop(n);
        let chain = Arc::new(Mutex::new(Chain::open(&home).unwrap()));
        let n = Node::new(chain, None, vec![], ClientIpPolicy::default());
        let stored: Vec<u64> = (1..=40).filter(|h| n.chain.lock().cert(*h).is_some()).collect();
        assert!(stored.windows(2).all(|w| (w[1] - w[0]) * size as u64 <= crate::chain::CERT_SPACING_BYTES + size as u64), "{stored:?}");
        let (s, len, b) = get_big(&n, "/api/sync?from=1&limit=200").await;
        assert_eq!(s, StatusCode::OK);
        let to = b["to"].as_u64().unwrap();
        assert!(stored.contains(&to) && len <= SYNC_BATCH_BYTES + (1 << 20), "to {to}, {len} bytes");
        // A stretch whose blocks pass the budget, its only certificate at its end (what CERT_CHECKPOINT
        // spacing left before): served up to it, for the joiner's request and for `limit=1`.
        for _ in 41..=60 {
            big_block(&n, size, false);
        }
        big_block(&n, size, true);
        for limit in [200, 1] {
            let (s, len, b) = get_big(&n, &format!("/api/sync?from=41&limit={limit}")).await;
            assert_eq!((s, b["to"].as_u64()), (StatusCode::OK, Some(61)), "limit={limit}");
            assert!(len > SYNC_BATCH_BYTES && len <= SYNC_MAX_BYTES + (1 << 20), "{len} bytes");
        }
        // Past the hard cap there is no batch to give: 503, the joiner tries another peer.
        for _ in 62..=120 {
            big_block(&n, size, false);
        }
        big_block(&n, size, true);
        let (s, len, _) = get_big(&n, "/api/sync?from=62&limit=1").await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
        assert!(len < 1024);
        // No certificate ahead at all (and no marshal): 503 without reading the stretch.
        let (s, _, _) = get_big(&n, "/api/sync?from=122").await;
        assert_eq!(s, StatusCode::OK, "past the tip");
        big_block(&n, size, false);
        let (s, _, b) = get_big(&n, "/api/sync?from=122").await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{b}");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn explorer_serves_its_haunted_backdrop() {
        let (root, n) = node("haunt");
        let (s, _, html) = call(&n, "GET", "/", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        assert!(html.contains(r#"src: "/haunt.js""#) && explorer_script().contains("/haunt.js"), "the explorer's own script loads the backdrop");
        let app = router(n.clone()).layer(MockConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));
        let res = app.oneshot(Request::builder().uri("/haunt.js").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[axum::http::header::CONTENT_TYPE], "text/javascript; charset=utf-8");
        assert_eq!(res.headers()[axum::http::header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        let body = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        assert!(body.starts_with(b"\"use strict\";(()=>{") && body.len() > 10_000, "a bundled script, not a stub");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn status_health_metrics_genesis_and_sync() {
        let (root, n) = node("ops");
        let (s, status, _) = call(&n, "GET", "/api/status", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        assert!(status.get("data_dir").is_none(), "data_dir isn't exposed");
        assert_eq!(status["version"], VERSION);
        assert_eq!((status["checkpoint_height"].as_u64(), status["weak_subjectivity_blocks"].as_u64()), (Some(0), Some(21 * 86_400 / 2)), "only the genesis; half of 21 days");
        assert!(status["genesis_hash"].as_str().unwrap().len() == 64);
        assert_eq!(status["crypto"]["p2p_transport"], p2p_transport());
        // No blocks yet: unhealthy.
        let (s, h, _) = call(&n, "GET", "/api/health", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
        assert!(h["reason"].as_str().is_some());
        n.chain.lock().produce_block_with(vec![]).unwrap();
        let (s, _, _) = call(&n, "GET", "/api/health", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        let (s, _, text) = call(&n, "GET", "/metrics", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        assert!(text.contains("blockula_height 1") && text.contains("blockula_mempool_size"), "{text}");
        let (_, g, _) = call(&n, "GET", "/api/genesis", None, "127.0.0.1:1").await;
        assert_eq!(g["genesis_hash"], status["genesis_hash"]);
        assert_eq!((status["address_prefix"].as_str(), g["address_prefix"].as_str()), (Some("boo"), Some("boo")));
        assert_eq!(status["max_tx_validity_blocks"], 3_600);
        // A node without a stored certificate for the range says so (the joiner rotates).
        let (s, _, _) = call(&n, "GET", "/api/sync?from=1", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
        // With one, it serves the batch up to the certified height.
        n.chain.lock().produce_block_with(vec![]).unwrap();
        n.chain.lock().store_cert(1, vec![1, 2, 3]).unwrap();
        let (s, b, _) = call(&n, "GET", "/api/sync?from=1", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["to"], 1);
        assert_eq!(b["finalization"], "010203");
        std::fs::remove_dir_all(root).ok();
    }

    /// The randomness beacon over HTTP (`/api/beacon`): the current value, the commit and reveal
    /// rounds with each active validator's part, recent misses; their metrics; and a claim's
    /// revert attestations in its JSON (who attested which revert, against the set's quorum).
    #[tokio::test]
    async fn beacon_rounds_misses_and_revert_attestations() {
        let (root, n) = node("beacon");
        let me = n.chain.lock().me().unwrap();
        let entry = |revealed: bool| crate::beacon::Entry { commitment: [7; 32], committed_at: 1, secret: revealed.then_some([8; 32]), revealed_at: revealed.then_some(2) };
        {
            let mut c = n.chain.lock();
            let st = &mut c.state;
            // Epoch 0: the validator committed for epoch 2 and committed and revealed for epoch 1;
            // the genesis value records a miss (as a derived value would).
            st.beacon.rounds.insert(2, BTreeMap::from([(me.clone(), entry(false))]));
            st.beacon.rounds.insert(1, BTreeMap::from([(me.clone(), entry(true))]));
            st.beacon.values.get_mut(&0).unwrap().missed = vec![me.clone()];
            // A verified claim in its revert window, with one attestation.
            let claim = crate::poc::Claim { id: 0, claimant: me.clone(), handle: "someone".into(), repo: "o/r".into(), pr: 1, familiar_key: None, bond: 0, submitted_at: 1, status: crate::poc::ClaimStatus::Verified, attestations: BTreeMap::new(), merge_sha: Some("a".repeat(40)), verified_at: Some(1), seance: None, score_ppm: None, payout: 0, soul_awarded: 0, grant: None, closed_at: None, reason: None };
            st.poc.claims.insert(0, claim);
            st.poc.next_claim_id = 1;
            st.beacon.reverts.insert(0, BTreeMap::from([(me.clone(), crate::beacon::RevertAttestation { revert_sha: "b".repeat(40), at: 1 })]));
            c.produce_block().unwrap();
        }
        let (s, b, _) = call(&n, "GET", "/api/beacon", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!((b["epoch"].as_u64(), b["active"].as_u64(), b["current"]["epoch"].as_u64(), b["current"]["hash_chain_only"].as_bool()), (Some(0), Some(1), Some(0), Some(true)));
        assert_eq!(b["current"]["missed"][0]["address"], me.as_str());
        assert!(b["next"].is_null(), "epoch 1's value is derived at epoch 0's last block");
        assert_eq!((b["commit_round"]["epoch"].as_u64(), b["commit_round"]["committed"].as_u64()), (Some(2), Some(1)));
        assert_eq!((b["reveal_round"]["epoch"].as_u64(), b["reveal_round"]["revealed"].as_u64()), (Some(1), Some(1)));
        assert_eq!(b["participation"], json!({ "committed": 1, "of": 1, "reveals_owed": 1, "revealed": 1 }));
        assert_eq!((b["validators"][0]["committed_for_epoch_after_next"].as_u64(), b["validators"][0]["revealed_for_next"].as_u64()), (Some(1), Some(2)));
        assert_eq!(b["misses"][0]["epoch"].as_u64(), Some(0));
        let (_, _, text) = call(&n, "GET", "/metrics", None, "127.0.0.1:1").await;
        for want in ["blockula_beacon_commitments 1", "blockula_beacon_reveals_owed 1", "blockula_beacon_reveals 1", "blockula_beacon_last_reveals 0", "blockula_beacon_last_missed 1", "blockula_beacon_own_missed 1"] {
            assert!(text.contains(want), "{want} in {text}");
        }
        let (s, cl, _) = call(&n, "GET", "/api/claim/0", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        let r = &cl["revert_attestations"];
        assert_eq!((r["quorum"].as_u64(), r["set"].as_u64(), r["by_revert"][0]["validators"].as_u64()), (Some(1), Some(1), Some(1)));
        assert_eq!((r["attestations"][0]["revert_sha"].as_str(), r["attestations"][0]["in_current_set"].as_bool()), (Some("b".repeat(40).as_str()), Some(true)));
        assert!(cl["revert_window_until"].as_u64().is_some());
        std::fs::remove_dir_all(root).ok();
    }

    /// Upgrades and halts over HTTP: the schedule and readiness (`/api/upgrade`), blocks'
    /// signals, a halted node's 503 with the reason, and the /metrics gauges operators alert on.
    #[tokio::test]
    async fn upgrade_status_signals_halts_and_their_metrics() {
        let (root, mut n) = node("upgrade");
        let v2 = crate::consensus::SUPPORTED_PROTOCOL_VERSION + 1;
        {
            let mut c = n.chain.lock();
            c.state.upgrade.schedule = vec![crate::upgrade::ScheduledUpgrade { version: v2, activation_height: 4 }];
            c.produce_block_with(vec![]).unwrap();
            c.produce_block_with(vec![]).unwrap();
        }
        let (s, u, _) = call(&n, "GET", "/api/upgrade", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!((u["height"].as_u64(), u["next"]["version"].as_u64(), u["next"]["blocks_left"].as_u64(), u["next"]["this_binary_supports_it"].as_bool()), (Some(2), Some(v2 as u64), Some(1), Some(false)));
        // Nothing withdrawn or superseded; the next upgrade ships past what's scheduled.
        assert_eq!((u["withdrawn"]["highest_version"].as_u64(), u["withdrawn"]["superseded_version"].as_u64(), u["next_version"].as_u64()), (Some(0), Some(0), Some(v2 as u64 + 1)));
        // The one validator proposed both blocks, signalling what its binary supports.
        assert_eq!((u["readiness"]["version"].as_u64(), u["readiness"]["ready"].as_u64(), u["readiness"]["active"].as_u64()), (Some(v2 as u64), Some(0), Some(1)));
        assert_eq!(u["readiness"]["validators"][0]["signal"].as_u64(), Some(crate::consensus::SUPPORTED_PROTOCOL_VERSION as u64));
        let (_, blocks, _) = call(&n, "GET", "/api/blocks", None, "127.0.0.1:1").await;
        assert_eq!((blocks[0]["version"].as_u64(), blocks[0]["signal"].as_u64()), (Some(1), Some(crate::consensus::SUPPORTED_PROTOCOL_VERSION as u64)));
        let (_, _, text) = call(&n, "GET", "/metrics", None, "127.0.0.1:1").await;
        for want in ["blockula_upgrade_next_version 2", "blockula_upgrade_next_height 4", "blockula_supported_protocol_version 1", "blockula_protocol_version 1", "blockula_halted 0"] {
            assert!(text.contains(want), "{want} in {text}");
        }
        // Block 3 executes; block 4 runs v2, which this binary doesn't: halted.
        assert!(n.chain.lock().produce_block_with(vec![]).is_err());
        let (s, h, _) = call(&n, "GET", "/api/health", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(h["halted"], "upgrade_required");
        assert!(h["reason"].as_str().unwrap().starts_with("halted: block 4 runs protocol v2"), "{h}");
        let (_, status, _) = call(&n, "GET", "/api/status", None, "127.0.0.1:1").await;
        assert_eq!(status["upgrade"]["halted"]["kind"], "upgrade_required");
        // Reads keep working.
        let (s, _, _) = call(&n, "GET", "/api/block/3", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        // A validator's slashing-protection gauges.
        let key = crate::crypto::Keys::from_seed([1; 32]).public_key();
        let p = crate::protection::Protection::in_memory(&key, crate::protection::Timing::for_block_time(1_000));
        p.enter_epoch(0, Some(0), 4, 0);
        n.protection = Some(p);
        n.p2p_peers = Some(Arc::new(|| Some(3)));
        mark_process_start();
        let (_, _, text) = call(&n, "GET", "/metrics", None, "127.0.0.1:1").await;
        for want in ["blockula_halted 1", "blockula_halt_info{kind=\"upgrade_required\"} 1", "blockula_slashing_protection_abstaining 1", "blockula_slashing_protection_voting 1", "blockula_slashing_protection_last_view 0", "blockula_p2p_peers 3", "blockula_process_start_time_seconds"] {
            assert!(text.contains(want), "{want} in {text}");
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn peer_count_from_the_runtime_metrics() {
        // As commonware-p2p 2026.9.0 exports it (captured from a 4-validator local chain).
        let text = "# HELP network_tracker_directory_connected Unix timestamp in milliseconds when each connected peer became active.\n# TYPE network_tracker_directory_connected gauge\nnetwork_tracker_directory_connected{peer=\"3de892cdf0a5\"} 1792045530601\nnetwork_tracker_directory_connected{peer=\"111ec16d2af5\"} 1792045529793\n# TYPE network_tracker_directory_tracked gauge\nnetwork_tracker_directory_tracked 3\nnode_validator_marshal_connected 1\n";
        assert_eq!(connected_peers(text), Some(2));
        assert_eq!(connected_peers("# HELP network_tracker_directory_connected x\n# TYPE network_tracker_directory_connected gauge\n"), Some(0));
        assert_eq!(connected_peers("runtime_tasks_spawned_total 3\n"), None, "no p2p tracker");
    }

    /// P-ECON's reads: the fee ring and what wallets should pay, the parameters and the
    /// issuance year, a name's status, prices and expiry, and a tx's actual charge.
    #[tokio::test]
    async fn fees_params_and_names() {
        let (root, n) = node("econ");
        let (s, f, _) = call(&n, "GET", "/api/fees", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!((f["lag"].as_u64(), f["slots"].as_array().unwrap().len()), (Some(17), 18));
        assert_eq!((f["slots"][17]["height"].as_u64(), f["slots"][0]["rate"].as_str()), (Some(17), Some("10000")));
        assert_eq!((f["suggested"]["flat"].as_str(), f["suggested"]["rate"].as_str(), f["rate_scale"].as_u64()), (Some("10000"), Some("10000"), Some(1_000)));
        assert_eq!((f["lowest"]["flat"].as_str(), f["lowest"]["rate"].as_str(), f["tip_cap_multiple"].as_u64()), (Some("10000"), Some("10000"), Some(20)));
        assert_eq!((f["floor"]["per_byte"].as_str(), f["account_creation_fee"].as_str()), (Some("10"), Some("100000000")));
        let (_, p, _) = call(&n, "GET", "/api/params", None, "127.0.0.1:1").await;
        assert_eq!((p["params"]["max_block_bytes"].as_str(), p["params"]["name_price_5"].as_str()), (Some("2097152"), Some("5000000000")));
        assert_eq!((p["genesis"]["epochs_per_year"].as_u64(), p["issuance"]["year"].as_u64()), (Some(52_596), Some(0)));
        let in_class = |class: &str, key: &str| p["classes"][class].as_array().unwrap().iter().any(|k| k == key);
        assert!(in_class("governance", "supermajority_bps") && in_class("validators", "slash_downtime_bps") && in_class("council", "max_validators") && !in_class("council", "issuance_initial_ppm"));
        let (_, a, _) = call(&n, "GET", "/api/name/alice.boo", None, "127.0.0.1:1").await;
        assert_eq!((a["status"].as_str(), a["available"].as_bool(), a["price"].as_str(), a["renewal_price"].as_str()), (Some("available"), Some(true), Some("5000000000"), Some("1000000000")));
        // Register it; the name shows its owner and expiry, and the tx what it was charged.
        let alice = crate::crypto::Keys::from_seed([3; 32]);
        let id = {
            let mut c = n.chain.lock();
            c.faucet(&alice.address(), 10 * DROPS_PER_FANG).unwrap();
            c.produce_block().unwrap();
            let mut tx = Tx { chain_id: "econ".into(), genesis_hash: c.genesis_hash.clone(), from: alice.address(), nonce: 0, fee: 0, tip: 25, valid_until: c.height() + 10, action: Action::RegisterName { name: "alice".into() }, memo: String::new(), public_key: hex::encode(alice.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
            tx.fee = 3 * min_fee(tx.size());
            let tx = tx.sign(&alice);
            let id = c.submit(tx).unwrap();
            c.produce_block().unwrap();
            id
        };
        let (_, a, _) = call(&n, "GET", "/api/name/alice", None, "127.0.0.1:1").await;
        let h = n.chain.lock().height();
        let year = n.chain.lock().state.chain_params.name_registration_blocks;
        assert_eq!((a["status"].as_str(), a["available"].as_bool(), a["owner"]["address"].as_str()), (Some("active"), Some(false), Some(alice.address().as_str())));
        assert_eq!((a["expires_at"].as_u64(), a["grace_until"].as_u64()), (Some(h + year), Some(h + year + n.chain.lock().state.chain_params.name_grace_blocks)));
        assert!(a.get("offered_to").is_none());
        // Offered to bob: still alice's, and the name says who may accept it.
        let bob = crate::crypto::Keys::from_seed([4; 32]).address();
        {
            let mut c = n.chain.lock();
            let mut tx = Tx { chain_id: "econ".into(), genesis_hash: c.genesis_hash.clone(), from: alice.address(), nonce: 1, fee: 0, tip: 0, valid_until: c.height() + 10, action: Action::TransferName { name: "alice".into(), to: bob.clone() }, memo: String::new(), public_key: hex::encode(alice.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
            tx.fee = 3 * min_fee(tx.size());
            c.submit(tx.sign(&alice)).unwrap();
            c.produce_block().unwrap();
        }
        let (_, a, _) = call(&n, "GET", "/api/name/alice", None, "127.0.0.1:1").await;
        assert_eq!((a["owner"]["address"].as_str(), a["offered_to"]["address"].as_str()), (Some(alice.address().as_str()), Some(bob.as_str())));
        let (_, t, _) = call(&n, "GET", &format!("/api/tx/{id}"), None, "127.0.0.1:1").await;
        let size = t["size_bytes"].as_u64().unwrap();
        assert_eq!(t["fee"].as_str().unwrap(), (min_fee(size) + 25).to_string(), "charged: the block's base fee + the tip, not the max fee");
        let (_, b, _) = call(&n, "GET", &format!("/api/block/{}", t["height"]), None, "127.0.0.1:1").await;
        assert_eq!((b["base_fee"]["flat"].as_str(), b["base_fee"]["rate"].as_str()), (Some("10000"), Some("10000")), "the block shows the base fee it charged");
        assert_eq!(t["max_fee"].as_str().unwrap(), (3 * min_fee(size)).to_string());
        std::fs::remove_dir_all(root).ok();
    }

    /// The validator set and one account's staking, as the explorer and the CLI read them.
    #[tokio::test]
    async fn validators_and_staking() {
        let (root, n) = node("stake");
        let me = n.chain.lock().me().unwrap();
        let (s, v, _) = call(&n, "GET", "/api/validators", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        let active = &v["active"][0];
        assert_eq!((active["validator"]["address"].as_str(), active["status"].as_str(), active["eligible"].as_bool()), (Some(me.as_str()), Some("active"), Some(true)));
        assert_eq!(active["self_bond"], active["stake"]);
        assert_eq!((v["churn_limit"].as_u64(), v["candidates"].as_array().map(Vec::len)), (Some(0), Some(0)), "a set of one takes no newcomers");
        assert!(v["admission"].is_null() && active["ready_at"].is_null() && active["admitted"].is_null(), "its admins pin newcomers: no admission round");
        assert_eq!(v["next_epoch"][0]["validator"]["address"], me.as_str());
        // A delegator: its stake, then its way out.
        let alice = crate::crypto::Keys::from_seed([1; 32]);
        n.chain.lock().faucet(&alice.address(), 50 * DROPS_PER_FANG).unwrap();
        n.chain.lock().produce_block().unwrap();
        let tx = |c: &Chain, nonce: u64, action: Action| {
            let mut t = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), from: alice.address(), nonce, fee: 0, tip: 0, valid_until: c.height() + 10, action, memo: String::new(), public_key: hex::encode(alice.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
            t.fee = t.charge().unwrap();
            t.sign(&alice)
        };
        let delegate = tx(&n.chain.lock(), 0, Action::Delegate { validator: me.clone(), amount: 20 * DROPS_PER_FANG });
        n.chain.lock().submit(delegate).unwrap();
        n.chain.lock().produce_block().unwrap();
        let undelegate = tx(&n.chain.lock(), 1, Action::Undelegate { validator: me.clone(), shares: 5 * DROPS_PER_FANG, amount: 0 });
        n.chain.lock().submit(undelegate).unwrap();
        n.chain.lock().produce_block().unwrap();
        let (s, st, _) = call(&n, "GET", &format!("/api/staking/{}", alice.address()), None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!((st["bonded"].as_str(), st["unbonding"].as_str(), st["withdrawable"].as_str()), (Some("15000000000"), Some("5000000000"), Some("0")));
        assert_eq!(st["delegations"][0]["validator"]["address"], me.as_str());
        assert_eq!((st["entries"][0]["type"].as_str(), st["entries"][0]["ready"].as_bool()), (Some("unbonding"), Some(false)));
        assert!(st["summary"].as_str().unwrap().ends_with("has 15 FANG staked with 1 validator, and 5 FANG unbonding (0 FANG ready to withdraw)"), "{}", st["summary"]);
        // The validator's own account: its pool, and its self-bond as a delegation.
        let (_, mine, _) = call(&n, "GET", &format!("/api/staking/{me}"), None, "127.0.0.1:1").await;
        assert_eq!(mine["validators"][0]["stake"].as_str().map(|s| s.parse::<u64>().unwrap()), Some(crate::staking::self_bond(&n.chain.lock().state, &me) + 15 * DROPS_PER_FANG));
        assert_eq!(mine["delegations"][0]["self_bond"], true);
        let (s, _, _) = call(&n, "GET", "/api/staking/nobody.boo", None, "127.0.0.1:1").await;
        assert!(s.is_client_error());
        // The staking series operators alert on (deploy/monitoring), for the active set.
        let (_, _, text) = call(&n, "GET", "/metrics", None, "127.0.0.1:1").await;
        for want in [format!("blockula_missed_leader_slots_total{{validator=\"{me}\",own=\"1\"}} 0"), format!("blockula_validator_missed_slots{{validator=\"{me}\",own=\"1\"}} 0"), format!("blockula_validator_jailed{{validator=\"{me}\",own=\"1\"}} 0"), format!("blockula_validator_tombstoned{{validator=\"{me}\",own=\"1\"}} 0"), "blockula_downtime_max_missed ".into()] {
            assert!(text.contains(&want), "{want} in {text}");
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn faucet_limits_use_ipv6_64_and_only_count_successful_drips() {
        let (root, n) = node("faucet");
        let a = crate::crypto::Keys::from_seed([1; 32]).address();
        let b = crate::crypto::Keys::from_seed([2; 32]).address();
        // Two addresses in the same IPv6 /64 share one budget.
        let (s, _, _) = call(&n, "POST", "/api/faucet", Some(json!({ "to": a })), "[2001:db8:1:2::1]:5000").await;
        assert_eq!(s, StatusCode::OK);
        let (s, _, _) = call(&n, "POST", "/api/faucet", Some(json!({ "to": b })), "[2001:db8:1:2::ffff]:5000").await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
        // A failed drip (busy faucet) doesn't burn the caller's quota.
        for i in 0..crate::chain::FAUCET_MAX_PENDING as u8 {
            n.chain.lock().faucet(&crate::crypto::Keys::from_seed([100 + i; 32]).address(), DROPS_PER_FANG).ok();
        }
        let (s, e, _) = call(&n, "POST", "/api/faucet", Some(json!({ "to": b })), "203.0.113.5:1").await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{e}");
        n.chain.lock().produce_block().unwrap();
        let (s, _, _) = call(&n, "POST", "/api/faucet", Some(json!({ "to": b })), "203.0.113.5:1").await;
        assert_eq!(s, StatusCode::OK, "quota wasn't consumed by the busy attempt");
        assert_eq!(ip_key("2001:db8:1:2:aaaa::1".parse().unwrap()), "2001:db8:1:2::/64");
        std::fs::remove_dir_all(root).ok();
    }

    /// F28: the faucet takes its rate-limit slot atomically before dripping, so a burst of
    /// parallel requests from one IP to one address gets one drip, not one per request that
    /// passed a check before any recorded; a drip that fails gives the slot back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn faucet_limits_hold_under_concurrent_requests() {
        let (root, n) = node("faucetrace");
        let a = crate::crypto::Keys::from_seed([5; 32]).address();
        let tasks: Vec<_> = (0..32).map(|_| {
            let (n, a) = (n.clone(), a.clone());
            tokio::spawn(async move { call(&n, "POST", "/api/faucet", Some(json!({ "to": a })), "203.0.113.5:1").await.0 })
        }).collect();
        let mut ok = 0;
        for t in tasks {
            match t.await.unwrap() {
                StatusCode::OK => ok += 1,
                s => assert_eq!(s, StatusCode::TOO_MANY_REQUESTS),
            }
        }
        assert_eq!(ok, 1, "one drip per IP and address window");
        // A reserved slot that's given back is free again; others' hits stay.
        let rules = vec![("k".to_string(), 1, Duration::from_secs(60))];
        let at = n.limits.allow(&rules).unwrap();
        assert!(n.limits.allow(&rules).is_err());
        n.limits.refund(&rules, at);
        n.limits.allow(&rules).unwrap();
        assert!(n.limits.allow(&rules).is_err());
        std::fs::remove_dir_all(root).ok();
    }

    /// `/api/proof`: a record's value and Merkle proof against the latest state root, checked with
    /// `state::tree::verify` exactly as a light client would; an absent key gets an exclusion
    /// proof.
    #[tokio::test]
    async fn proofs_verify_against_the_state_root() {
        use crate::state::{codec::decode, ns, tree};
        let (root, n) = node("proof");
        let alice = crate::crypto::Keys::from_seed([3; 32]);
        n.chain.lock().faucet(&alice.address(), 10 * DROPS_PER_FANG).unwrap();
        n.chain.lock().produce_block().unwrap();
        let (_, status, _) = call(&n, "GET", "/api/status", None, "127.0.0.1:1").await;
        let parse = |v: &Value| -> ([u8; 32], tree::Proof, Option<Vec<u8>>) {
            let r: [u8; 32] = hex::decode(v["root"].as_str().unwrap()).unwrap().try_into().unwrap();
            (r, serde_json::from_value(v["proof"].clone()).unwrap(), v["value"].as_str().map(|h| hex::decode(h).unwrap()))
        };
        let key = alice.address();
        let (s, p, _) = call(&n, "GET", &format!("/api/proof/{}/{}", ns::ACCOUNTS, hex::encode(&key)), None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK, "{p}");
        let (r, proof, value) = parse(&p);
        assert_eq!((p["height"].as_u64(), p["root"].as_str()), (Some(1), status["state_root"].as_str()));
        let value = value.expect("alice exists");
        assert!(tree::verify(&r, ns::ACCOUNTS, key.as_bytes(), Some(&value), &proof));
        assert_eq!(decode::<crate::chain::Account>(&value).unwrap().balance, 10 * DROPS_PER_FANG);
        assert!(!tree::verify(&r, ns::ACCOUNTS, key.as_bytes(), Some(b"forged"), &proof));
        // A scalar, and an absent account.
        let (_, p, _) = call(&n, "GET", &format!("/api/proof/{}/{}", ns::META, hex::encode(crate::state::meta::SUPPLY)), None, "127.0.0.1:1").await;
        let (r, proof, value) = parse(&p);
        assert!(tree::verify(&r, ns::META, crate::state::meta::SUPPLY, value.as_deref(), &proof) && value.is_some());
        let (_, p, _) = call(&n, "GET", &format!("/api/proof/{}/{}", ns::ACCOUNTS, hex::encode("boo1nobody")), None, "127.0.0.1:1").await;
        let (r, proof, value) = parse(&p);
        assert!(value.is_none() && tree::verify(&r, ns::ACCOUNTS, b"boo1nobody", None, &proof));
        let (s, _, _) = call(&n, "GET", "/api/proof/1/not-hex", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        std::fs::remove_dir_all(root).ok();
    }

    const READS: [&str; 14] = ["/api/status", "/api/blocks", "/api/block/1", "/api/account/alice.boo", "/api/name/alice", "/api/search/alice", "/api/validators", "/api/hunts", "/api/contributions", "/api/claims", "/api/souls", "/api/mempool", "/api/epochs", "/api/health"];

    /// Every read is served while consensus holds the chain (SEC-4): handlers read the published
    /// view, never the `Mutex<Chain>`.
    #[test]
    fn reads_never_take_the_chain_lock() {
        let (root, n) = node("nolock");
        let alice = crate::crypto::Keys::from_seed([4; 32]);
        {
            let mut c = n.chain.lock();
            c.faucet(&alice.address(), 10 * DROPS_PER_FANG).unwrap();
            c.produce_block().unwrap();
            let mut t = Tx { chain_id: "nolock".into(), genesis_hash: c.genesis_hash.clone(), from: alice.address(), nonce: 0, fee: 0, tip: 0, valid_until: 50, action: Action::RegisterName { name: "alice".into() }, memo: String::new(), public_key: hex::encode(alice.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
            t.fee = min_fee(t.size());
            c.submit(t.sign(&alice)).unwrap();
            c.produce_block().unwrap();
        }
        let id = n.chain.lock().block(2).unwrap().txs[0].id();
        let held = n.chain.lock(); // as if a block were executing
        let (tx, rx) = std::sync::mpsc::channel();
        let n2 = n.clone();
        let tx_uri = format!("/api/tx/{id}");
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            for uri in READS.iter().map(|u| u.to_string()).chain([tx_uri]) {
                let (s, v, _) = rt.block_on(call(&n2, "GET", &uri, None, "127.0.0.1:1"));
                tx.send((uri, s, v)).unwrap();
            }
        });
        for _ in 0..=READS.len() {
            let (uri, s, v) = rx.recv_timeout(Duration::from_secs(20)).expect("a read is waiting for the chain lock");
            assert!(s == StatusCode::OK || (uri == "/api/health" && s == StatusCode::SERVICE_UNAVAILABLE), "{uri}: {s} {v}");
            if uri.starts_with("/api/account") {
                assert_eq!(v["names"][0], "alice.boo");
                assert_eq!(v["recent_txs"].as_array().unwrap().len(), 2);
            }
        }
        drop(held);
        std::fs::remove_dir_all(root).ok();
    }

    /// Readers hammering the API don't hold up execution: blocks keep executing at their own
    /// pace while four threads read as fast as they can.
    #[test]
    fn heavy_reads_dont_slow_block_execution() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let (root, n) = node("hammer");
        let stop = Arc::new(AtomicBool::new(false));
        let reads = Arc::new(AtomicUsize::new(0));
        let mut threads = vec![];
        for i in 0..4 {
            let (n, stop, reads) = (n.clone(), stop.clone(), reads.clone());
            threads.push(std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                while !stop.load(Ordering::Relaxed) {
                    let uri = ["/api/status", "/api/blocks", "/api/mempool", "/api/validators"][i];
                    let (s, _, _) = rt.block_on(call(&n, "GET", uri, None, "127.0.0.1:1"));
                    assert_eq!(s, StatusCode::OK);
                    reads.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        let mut took = vec![];
        for _ in 0..40 {
            let t = Instant::now();
            n.chain.lock().produce_block_with(vec![]).unwrap();
            took.push(t.elapsed());
        }
        stop.store(true, Ordering::Relaxed);
        for t in threads {
            t.join().unwrap();
        }
        took.sort();
        let (median, worst) = (took[took.len() / 2], *took.last().unwrap());
        println!("40 blocks under {} concurrent reads: median {median:?}, worst {worst:?}", reads.load(Ordering::Relaxed));
        assert!(reads.load(Ordering::Relaxed) >= 40, "the readers ran alongside execution");
        assert!(worst < Duration::from_secs(2), "a block took {worst:?} to execute under read load");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn reads_are_rate_limited_per_client() {
        let (root, n) = node("readlimit");
        let n = n.with_read_limits(1, 3);
        for _ in 0..3 {
            assert_eq!(call(&n, "GET", "/api/status", None, "203.0.113.9:1").await.0, StatusCode::OK);
        }
        let (s, v, _) = call(&n, "GET", "/api/status", None, "203.0.113.9:2").await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{v}");
        assert_eq!(call(&n, "GET", "/api/status", None, "203.0.113.10:1").await.0, StatusCode::OK, "another client has its own budget");
        for _ in 0..10 {
            assert_eq!(call(&n, "GET", "/api/status", None, "127.0.0.1:1").await.0, StatusCode::OK, "local callers aren't limited");
        }
        let (_, _, metrics) = call(&n, "GET", "/metrics", None, "127.0.0.1:1").await;
        assert!(metrics.contains("blockula_http_reads_limited_total 1"), "{metrics}");
        // Tokens come back at `rps`.
        let l = ReadLimiter::new(1_000, 2);
        assert!(l.allow("a").is_ok() && l.allow("a").is_ok() && l.allow("a").is_err());
        std::thread::sleep(Duration::from_millis(5));
        assert!(l.allow("a").is_ok());
        assert!(ReadLimiter::new(0, 0).allow("a").is_ok(), "0 turns the limit off");
        std::fs::remove_dir_all(root).ok();
    }

    /// A pruning node says "pruned" (410) for heights it no longer holds, on `/api/block` and
    /// `/api/sync`.
    #[tokio::test]
    async fn pruned_heights_say_so() {
        let root = std::env::temp_dir().join(format!("blockula-api-pruned-{}-{}", crate::chain::now_ms(), std::process::id()));
        let homes = Chain::summon(&root, &SummonOpts { block_time_ms: 60_000, ..SummonOpts::new("pruned", 1) }).unwrap();
        let mut c = Chain::open(&homes[0]).unwrap();
        c.set_pruning(Some(1));
        let keep = c.keep_blocks().unwrap();
        for _ in 0..keep + 20 {
            c.produce_block_with(vec![]).unwrap();
        }
        let n = Node::new(Arc::new(Mutex::new(c)), None, vec![], ClientIpPolicy::default());
        for uri in ["/api/block/5", "/api/sync?from=5"] {
            let (s, v, _) = call(&n, "GET", uri, None, "127.0.0.1:1").await;
            assert_eq!(s, StatusCode::GONE, "{uri}: {v}");
            assert!(v["pruned"] == true && v["error"].as_str().unwrap().contains("pruned"), "{v}");
        }
        let (_, status, _) = call(&n, "GET", "/api/status", None, "127.0.0.1:1").await;
        assert_eq!(status["history_from"], 21);
        assert_eq!(status["pruning"]["keep_blocks"], keep);
        assert_eq!(call(&n, "GET", "/api/block/21", None, "127.0.0.1:1").await.0, StatusCode::OK);
        let (s, b, _) = call(&n, "GET", "/api/blocks?limit=100", None, "127.0.0.1:1").await;
        assert_eq!((s, b.as_array().unwrap().len()), (StatusCode::OK, 100));
        std::fs::remove_dir_all(root).ok();
    }

    /// `/api/ghost` and ghost txs over HTTP: the status (off by default), the wire form as
    /// `application/octet-stream` (a ghost tx is bigger than the JSON body limit), refused while
    /// Ghost Mode is off, without verifying its proof; a signed tx's wire form is accepted like its
    /// JSON; a published ghost address is found by name.
    #[tokio::test]
    async fn ghost_status_and_binary_submission() {
        let (root, n) = node("ghostapi");
        let (s, g, _) = call(&n, "GET", "/api/ghost", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!((g["active"].as_bool(), g["paused"].as_bool(), g["pool"].as_str(), g["frontier_size"].as_u64()), (Some(false), Some(false), Some("0"), Some(0)));
        assert_eq!(g["instance"].as_str().map(str::len), Some(64));
        assert_eq!(g["audited"], false);
        let send = |bytes: Vec<u8>| {
            let app = router(n.clone()).layer(MockConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));
            async move {
                let req = Request::builder().method("POST").uri("/api/tx").header("content-type", "application/octet-stream").body(Body::from(bytes)).unwrap();
                let res = app.oneshot(req).await.unwrap();
                let status = res.status();
                let body = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
                (status, serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null))
            }
        };
        // A ghost tx of real size (its proof is junk: it's never verified while Ghost Mode is off).
        let mut bundle = crate::types::tests::vector_bundle(1);
        bundle.proof = HexBytes(vec![1; 150_000]);
        let genesis_hash = n.chain.lock().genesis_hash.clone();
        let ghost = Tx { chain_id: "ghostapi".into(), genesis_hash: genesis_hash.clone(), from: String::new(), nonce: 0, fee: 10_000_000, tip: 0, valid_until: 50, action: Action::PrivateTransfer { bundle }, memo: String::new(), public_key: String::new(), signature: String::new() };
        assert!(ghost.to_bytes().len() > MAX_BODY_BYTES);
        let (s, v) = send(ghost.to_bytes()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"].as_str().unwrap().contains("Ghost Mode is off"), "{v}");
        let (s, v) = send(vec![0; 40]).await;
        assert!(s == StatusCode::BAD_REQUEST && v["error"].as_str().unwrap().contains("v3"), "{v}");
        // A signed transfer in its wire form.
        let alice = crate::crypto::Keys::from_seed([6; 32]);
        n.chain.lock().faucet(&alice.address(), 10 * DROPS_PER_FANG).unwrap();
        n.chain.lock().produce_block().unwrap();
        let mut t = Tx { chain_id: "ghostapi".into(), genesis_hash, from: alice.address(), nonce: 0, fee: 0, tip: 0, valid_until: 50, action: Action::RegisterName { name: "alice".into() }, memo: String::new(), public_key: hex::encode(alice.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
        t.fee = min_fee(t.size());
        let t = t.sign(&alice);
        let (s, v) = send(t.to_bytes()).await;
        assert_eq!((s, v["id"].as_str()), (StatusCode::OK, Some(t.id().as_str())), "{v}");
        let (s, _, _) = call(&n, "GET", "/api/ghost/address/alice.boo", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        // Published (as `SetGhostAddress` would), it's found by name, with its fingerprint.
        n.chain.lock().produce_block().unwrap();
        let addr = blockula_ghost::SpendingKey::from_seed([7; 32]).address();
        n.chain.lock().state.ghost.addresses.insert(alice.address(), addr.to_bytes().to_vec());
        n.chain.lock().produce_block().unwrap();
        let (s, v, _) = call(&n, "GET", "/api/ghost/address/alice.boo", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!((v["account"]["address"].as_str(), v["fingerprint"].as_str()), (Some(alice.address().as_str()), Some(addr.fingerprint().as_str())));
        assert_eq!(v["address"].as_str().map(str::len), Some(2 * GHOST_ADDRESS_BYTES));
        std::fs::remove_dir_all(root).ok();
    }

    /// Audit gap-7-2: `/api/key` lists only accounts the key agreed to join (a stranger's AddKey
    /// without its consent is refused, so 100 of them can't push the real account off the list),
    /// and it pages past its limit with `next`.
    #[tokio::test]
    async fn key_listings_hold_only_consenting_accounts_and_page() {
        let (root, n) = node("keypage");
        let kit = crate::crypto::Keys::from_seed([40; 32]);
        let kid = key_id(&kit.public_key());
        let owners: Vec<_> = (0..4u8).map(|i| crate::crypto::Keys::from_seed([50 + i; 32])).collect();
        let mallory = crate::crypto::Keys::from_seed([60; 32]);
        let (cid, gh) = { let c = n.chain.lock(); (c.genesis.chain_id.clone(), c.genesis_hash.clone()) };
        let mk = |c: &Chain, from: &crate::crypto::Keys, action: Action| {
            let mut tx = Tx { chain_id: cid.clone(), genesis_hash: gh.clone(), from: from.address(), nonce: c.next_nonce(&from.address()), fee: 0, tip: 0, valid_until: c.height() + 100, action, memo: String::new(), public_key: hex::encode(from.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
            tx.fee = min_fee(tx.size());
            tx.sign(from)
        };
        {
            let mut c = n.chain.lock();
            for k in owners.iter().chain([&mallory]) {
                c.faucet(&k.address(), 10 * DROPS_PER_FANG).unwrap();
            }
            c.produce_block().unwrap();
            for k in &owners {
                let add = mk(&c, k, crate::types::add_key(&kit, &cid, &gh, &k.address(), "kit", KeyRole::Owner));
                c.submit(add).unwrap();
            }
            // The PoC's sybil: the key id is public, but its consent names other accounts.
            let theirs = crate::types::add_key(&kit, &cid, &gh, &owners[0].address(), "kit", KeyRole::Owner);
            let sybil = mk(&c, &mallory, theirs);
            assert!(c.submit(sybil).unwrap_err().to_string().contains("proof of possession"));
            c.produce_block().unwrap();
        }
        let mut want: Vec<String> = owners.iter().map(|k| k.address()).collect();
        want.sort();
        let (s, v, _) = call(&n, "GET", &format!("/api/key/{kid}"), None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let listed = |v: &Value| v["accounts"].as_array().unwrap().iter().map(|a| a["account"]["address"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(listed(&v)[1..], want[..], "the kit's own address first, then the consenting accounts");
        assert!(v["next"].is_null());
        // Two at a time: the pages add up to the whole list, each once.
        let (mut seen, mut after) = (vec![], None::<String>);
        loop {
            let uri = format!("/api/key/{kid}?limit=2{}", after.as_ref().map(|a| format!("&after={a}")).unwrap_or_default());
            let (_, v, _) = call(&n, "GET", &uri, None, "127.0.0.1:1").await;
            seen.extend(listed(&v).into_iter().filter(|a| *a != kit.address()));
            match v["next"].as_str() {
                Some(next) => after = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(seen, want);
        std::fs::remove_dir_all(root).ok();
    }

    /// Audit main13-protocol-3: a payment signed to a name (gap-7-1) is reported as paid to whoever
    /// owned the name when it ran, with the name it was signed to, even after the name changed hands.
    #[tokio::test]
    async fn a_payment_to_a_name_reports_who_it_paid() {
        let (root, n) = node("paidname");
        let (alice, bob, payer) = (crate::crypto::Keys::from_seed([21; 32]), crate::crypto::Keys::from_seed([22; 32]), crate::crypto::Keys::from_seed([23; 32]));
        let (cid, gh) = { let c = n.chain.lock(); (c.genesis.chain_id.clone(), c.genesis_hash.clone()) };
        let mk = |c: &Chain, from: &crate::crypto::Keys, action: Action| {
            let mut tx = Tx { chain_id: cid.clone(), genesis_hash: gh.clone(), from: from.address(), nonce: c.next_nonce(&from.address()), fee: 0, tip: 0, valid_until: c.height() + 100, action, memo: String::new(), public_key: hex::encode(from.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
            tx.fee = min_fee(tx.size());
            tx.sign(from)
        };
        let step = |who: &crate::crypto::Keys, action: Action| {
            let mut c = n.chain.lock();
            let tx = mk(&c, who, action);
            c.submit(tx.clone()).unwrap();
            c.produce_block().unwrap();
            tx
        };
        {
            let mut c = n.chain.lock();
            for k in [&alice, &bob, &payer] {
                c.faucet(&k.address(), 100 * DROPS_PER_FANG).unwrap();
            }
            c.produce_block().unwrap();
        }
        step(&alice, Action::RegisterName { name: "crypt".into() });
        let pay = step(&payer, Action::Transfer { to: "crypt.boo".into(), amount: 5 * DROPS_PER_FANG });
        // alice offers the name to bob, and bob accepts it.
        step(&alice, Action::TransferName { name: "crypt".into(), to: bob.address() });
        step(&bob, Action::TransferName { name: "crypt".into(), to: bob.address() });
        assert_eq!(n.chain.lock().state.resolve("crypt.boo").unwrap(), bob.address());
        let (s, v, _) = call(&n, "GET", &format!("/api/tx/{}", pay.id()), None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!((v["to"]["address"].as_str(), v["to_name"].as_str()), (Some(alice.address().as_str()), Some("crypt.boo")), "paid to the owner when it ran: {v}");
        let (_, a, _) = call(&n, "GET", &format!("/api/account/{}", alice.address()), None, "127.0.0.1:1").await;
        let row = a["recent_txs"].as_array().unwrap().iter().find(|t| t["id"] == json!(pay.id())).cloned().expect("in alice's history");
        assert_eq!(row["to"]["address"].as_str(), Some(alice.address().as_str()), "{row}");
        let h = v["height"].as_u64().unwrap();
        let (_, b, _) = call(&n, "GET", &format!("/api/block/{h}"), None, "127.0.0.1:1").await;
        assert_eq!(b["txs"][0]["to"]["address"].as_str(), Some(alice.address().as_str()), "{b}");
        // Audit review-main13-1: the name changes hands later in the payment's own block. Who it
        // named is resolved just before each tx runs, so it's still reported as paid to alice.
        step(&alice, Action::RegisterName { name: "shop".into() });
        step(&alice, Action::TransferName { name: "shop".into(), to: bob.address() });
        let (same, accept) = {
            let c = n.chain.lock();
            (mk(&c, &payer, Action::Transfer { to: "shop.boo".into(), amount: 5 * DROPS_PER_FANG }), mk(&c, &bob, Action::TransferName { name: "shop".into(), to: bob.address() }))
        };
        let had = n.chain.lock().state.account(&alice.address()).balance;
        n.chain.lock().produce_block_with(vec![same.clone(), accept]).unwrap();
        {
            let c = n.chain.lock();
            assert_eq!((c.state.account(&alice.address()).balance, c.state.resolve("shop.boo").unwrap()), (had + 5 * DROPS_PER_FANG, bob.address()), "the chain paid alice; bob owns it after the block");
        }
        let (_, v, _) = call(&n, "GET", &format!("/api/tx/{}", same.id()), None, "127.0.0.1:1").await;
        assert_eq!((v["to"]["address"].as_str(), v["to_name"].as_str()), (Some(alice.address().as_str()), Some("shop.boo")), "{v}");
        let (_, a, _) = call(&n, "GET", &format!("/api/account/{}", alice.address()), None, "127.0.0.1:1").await;
        assert!(a["recent_txs"].as_array().unwrap().iter().any(|t| t["id"] == json!(same.id())), "in alice's history: {a}");
        let (_, b, _) = call(&n, "GET", &format!("/api/account/{}", bob.address()), None, "127.0.0.1:1").await;
        assert!(!b["recent_txs"].as_array().unwrap().iter().any(|t| t["id"] == json!(same.id())), "not in bob's: {b}");
        // An address-signed payment names no name.
        let direct = step(&payer, Action::Transfer { to: bob.address(), amount: DROPS_PER_FANG });
        let (_, d, _) = call(&n, "GET", &format!("/api/tx/{}", direct.id()), None, "127.0.0.1:1").await;
        assert_eq!((d["to"]["address"].as_str(), d["to_name"].is_null()), (Some(bob.address().as_str()), true), "{d}");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn failed_txs_report_status_instead_of_404() {
        let (root, n) = node("txfail");
        let k = crate::crypto::Keys::from_seed([9; 32]);
        n.chain.lock().faucet(&k.address(), 10 * DROPS_PER_FANG).unwrap();
        n.chain.lock().produce_block().unwrap();
        let me = n.chain.lock().me().unwrap();
        let valid_until = n.chain.lock().height() + 100;
        let genesis_hash = n.chain.lock().genesis_hash.clone();
        let mk = |nonce: u64| {
            let mut tx = Tx { chain_id: "txfail".into(), genesis_hash: genesis_hash.clone(), from: k.address(), nonce, fee: 0, tip: 0, valid_until, action: Action::Transfer { to: me.clone(), amount: 6 * DROPS_PER_FANG }, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
            tx.fee = min_fee(tx.size());
            tx.sign(&k)
        };
        let (t1, t2) = (mk(0), mk(1));
        // JSON is strict: an unsigned extra field is refused, not carried along.
        let mut padded = serde_json::to_value(&t1).unwrap();
        padded["padding"] = json!("x".repeat(1000));
        let (s, _, _) = call(&n, "POST", "/api/tx", Some(padded), "127.0.0.1:1").await;
        assert!(s.is_client_error(), "{s}");
        let (s, v, _) = call(&n, "GET", &format!("/api/tx/{}", t1.id()), None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
        n.chain.lock().produce_block_with(vec![t1, t2.clone()]).unwrap();
        let (s, v, _) = call(&n, "GET", &format!("/api/tx/{}", t2.id()), None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["status"], "failed");
        assert!(v["error"].as_str().unwrap().contains("insufficient"));
        assert_ne!(v["dropped"], true, "it ran in a block: its fee was charged");
        // The failure memory knows which failures ran, for when the block isn't answered above.
        assert!(n.live.mempool().failure_ran(&t2.id()));
        // review-fix-clients-2: a tx dropped from the mempool unrun (here: it expired before anyone
        // included it) is `dropped`: nothing charged, and it isn't "failed at execution".
        let h = n.chain.lock().height();
        let mut t3 = Tx { chain_id: "txfail".into(), genesis_hash: genesis_hash.clone(), from: k.address(), nonce: 2, fee: 0, tip: 0, valid_until: h + 1, action: Action::Transfer { to: me.clone(), amount: 1 }, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
        t3.fee = min_fee(t3.size());
        let t3 = t3.sign(&k);
        n.chain.lock().submit(t3.clone()).unwrap();
        n.chain.lock().produce_block_with(vec![]).unwrap();
        n.chain.lock().produce_block_with(vec![]).unwrap();
        let (s, v, _) = call(&n, "GET", &format!("/api/tx/{}", t3.id()), None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!((v["status"].as_str(), v["dropped"].as_bool()), (Some("failed"), Some(true)), "{v}");
        assert!(v["error"].as_str().unwrap().contains("expired"), "{v}");
        assert!(!n.live.mempool().failure_ran(&t3.id()));
        // The node's own explorer reads this answer as a drop too, not as a final failure.
        if let Some(page) = super::tests::explorer_tx_page(&v) {
            assert!(page.contains("dropped from this node's mempool without running") && page.contains("so don't send it again before then") && !page.contains("pill failed"), "{page}");
        }
        std::fs::remove_dir_all(root).ok();
    }

    /// F66: a finalized tx's charge comes from its block's index row, so `/api/tx` and
    /// `/api/account` read one tx's span, never parse (and cache) the whole block.
    #[tokio::test]
    async fn tx_and_account_reads_dont_parse_whole_blocks() {
        let root = std::env::temp_dir().join(format!("blockula-api-txspan-{}-{}", crate::chain::now_ms(), std::process::id()));
        let homes = Chain::summon(&root, &SummonOpts::new("txspan", 1)).unwrap();
        let k = crate::crypto::Keys::from_seed([9; 32]);
        let (id, at, fee) = {
            let mut c = Chain::open(&homes[0]).unwrap();
            c.faucet(&k.address(), 10 * DROPS_PER_FANG).unwrap();
            c.produce_block().unwrap();
            let mut tx = Tx { chain_id: "txspan".into(), genesis_hash: c.genesis_hash.clone(), from: k.address(), nonce: 0, fee: 0, tip: 7, valid_until: c.height() + 100, action: Action::Transfer { to: c.me().unwrap(), amount: DROPS_PER_FANG }, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
            tx.fee = min_fee(tx.size()) + 1_000_000;
            let tx = tx.sign(&k);
            c.produce_block_with(vec![tx.clone()]).unwrap();
            let at = c.height();
            let slot = c.view().block(at).unwrap().base_fee.expect("blocks record their base fee");
            c.produce_block().unwrap();
            (tx.id(), at, slot.fee(tx.size()) + 7)
        };
        // A fresh start: nothing cached, and the tx's block isn't the head.
        let chain = Arc::new(Mutex::new(Chain::open(&homes[0]).unwrap()));
        let n = Node::new(chain, None, vec![], ClientIpPolicy::default());
        assert!(n.live.view().disk.cached_block(at).is_none());
        let (s, v, _) = call(&n, "GET", &format!("/api/tx/{id}"), None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!((v["status"].as_str(), v["fee"].as_str()), (Some("finalized"), Some(fee.to_string().as_str())), "{v}");
        let (s, a, _) = call(&n, "GET", &format!("/api/account/{}", k.address()), None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        assert!(a["recent_txs"].as_array().unwrap().iter().any(|t| t["id"] == json!(id) && t["fee"] == v["fee"]), "{a}");
        assert!(n.live.view().disk.cached_block(at).is_none(), "the block was parsed for its base fee");
        std::fs::remove_dir_all(root).ok();
    }

    /// Governance reads: a proposal with its tally and votes, the distribution it opened and a
    /// claim on it, a multisig account with an open proposal.
    #[tokio::test]
    async fn governance_endpoints() {
        const FANG: u64 = DROPS_PER_FANG;
        let root = std::env::temp_dir().join(format!("blockula-api-gov-{}-{}", crate::chain::now_ms(), std::process::id()));
        let params = [("voting_blocks", 10), ("timelock_blocks", 1), ("governance_spend_height", 0), ("proposal_deposit", 10 * FANG)].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let homes = Chain::summon(&root, &SummonOpts { params, ..SummonOpts::new("govapi", 1) }).unwrap();
        // The community account, which distributions pay out of.
        let path = homes[0].join("genesis.json");
        let mut g = crate::chain::Genesis::load(&path).unwrap();
        g.allocations.push(crate::chain::Allocation { address: crate::chain::community("boo"), amount: 1_000 * FANG, vesting: None, label: None });
        std::fs::write(&path, serde_json::to_string_pretty(&g).unwrap()).unwrap();
        let n = Node::new(Arc::new(Mutex::new(Chain::open(&homes[0]).unwrap())), None, vec![], ClientIpPolicy::default());
        let (k, k2) = (crate::crypto::Keys::from_seed([9; 32]), crate::crypto::Keys::from_seed([10; 32]));
        let (tree_root, proofs) = crate::governance::build_distribution(0, &[(k.address(), 3 * FANG)]).unwrap();
        let me = {
            let mut c = n.chain.lock();
            c.faucet(&k.address(), 10 * FANG).unwrap();
            let claim_until = c.height() + 100;
            c.submit_own(Action::Propose { kind: ProposalKind::Distribution { distribution_id: 0, merkle_root: hex::encode(tree_root), total: 3 * FANG, claim_until }, deposit: 10 * FANG, title: "First drop".into(), url: String::new() }, "").unwrap();
            c.produce_block().unwrap();
            let proposal_digest = crate::governance::digest_of(&c.state.governance.proposals[&0]);
            c.submit_own(Action::Vote { proposal_id: 0, aye: true, amount: 100_000_000 * FANG, conviction: 1, proposal_digest }, "").unwrap();
            for _ in 0..13 {
                c.produce_block().unwrap();
            }
            let signed = |c: &Chain, signer: &crate::crypto::Keys, nonce: u64, action: Action| {
                let mut tx = Tx { chain_id: "govapi".into(), genesis_hash: c.genesis_hash.clone(), from: k.address(), nonce, fee: 0, tip: 0, valid_until: c.height() + 50, action, memo: String::new(), public_key: hex::encode(signer.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
                tx.fee = min_fee(tx.size());
                tx.sign(signer)
            };
            let k2_id = key_id(&k2.public_key());
            let txs = vec![
                signed(&c, &k, 0, Action::ClaimDistribution { distribution_id: 0, amount: 3 * FANG, proof: proofs[0].iter().map(hex::encode).collect() }),
                signed(&c, &k, 1, crate::types::add_key(&k2, "govapi", &c.genesis_hash, &k.address(), "laptop", KeyRole::Owner)),
                signed(&c, &k, 2, Action::MultisigCreate { owners: vec![key_id(&k.public_key()), k2_id], threshold: 2 }),
                signed(&c, &k2, 3, Action::MultisigPropose { action: Box::new(Action::Transfer { to: c.me().unwrap(), amount: FANG }) }),
            ];
            c.produce_block_with(txs).unwrap();
            c.me().unwrap()
        };
        let get = |uri: String| {
            let n = n.clone();
            async move { call(&n, "GET", &uri, None, "127.0.0.1:1").await }
        };
        let (s, v, _) = get("/api/proposals".into()).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!((v["count"].as_u64(), v["next_id"].as_u64(), v["params"]["voting_blocks"].as_u64(), v["council_active"].as_bool()), (Some(1), Some(1), Some(10), Some(true)));
        let p = &v["proposals"][0];
        assert_eq!((p["status"].as_str(), p["kind"]["type"].as_str(), p["deposit_display"].as_str()), (Some("executed"), Some("distribution"), Some("10 FANG")));
        assert!(p["summary"].as_str().unwrap().starts_with("distribute 3 FANG from the community account"), "{p}");
        assert_eq!(get("/api/proposals?status=voting".into()).await.1["proposals"], json!([]));
        let (_, p, _) = get("/api/proposal/0".into()).await;
        // Closed: the tally stays, the vote records were swept.
        assert_eq!((p["voters"].as_u64(), p["votes"].as_array().map(Vec::len), p["votes_swept"].as_bool(), p["tally"]["quorum_reached"].as_bool()), (Some(1), Some(0), Some(true), Some(true)));
        assert_eq!((p["ayes"]["weight"]["display"].as_str(), p["distribution"].as_str()), (Some("100000000 FANG"), Some("/api/distribution/0")));
        assert_eq!(get("/api/proposal/9".into()).await.0, StatusCode::NOT_FOUND);
        let (_, d, _) = get(format!("/api/distribution/0?address={}", k.address())).await;
        assert_eq!((d["claimed"].as_str(), d["remaining"].as_str(), d["claims"].as_u64(), d["open"].as_bool()), (Some("3000000000"), Some("0"), Some(1), Some(true)));
        assert_eq!(d["claim"]["claimed"]["amount_display"], "3 FANG");
        let (s, m, _) = get(format!("/api/multisig/{}", k.address())).await;
        assert_eq!(s, StatusCode::OK, "{m}");
        assert_eq!((m["threshold"].as_u64(), m["owners"].as_array().map(Vec::len), m["pending"][0]["valid_approvals"].as_u64()), (Some(2), Some(2), Some(1)));
        assert!(m["pending"][0]["summary"].as_str().unwrap().contains("sent 1 FANG to"), "{m}");
        assert_eq!(get(format!("/api/multisig/{me}")).await.0, StatusCode::NOT_FOUND);
        // What an account can move: the vote's conviction lock holds its amount, net of stake
        // (econ::free), and `locks` says why; a wallet's preflight checks payments against it.
        let (_, a, _) = get(format!("/api/account/{me}")).await;
        let transferable = {
            let c = n.chain.lock();
            crate::econ::transferable(&c.state, &me, crate::econ::now(&c.state))
        };
        assert_eq!(a["locked"].as_str(), Some((100_000_000 * FANG).to_string().as_str()), "{a}");
        assert_eq!(a["transferable"].as_str(), Some(transferable.to_string().as_str()), "{a}");
        assert!(a["transferable"].as_str().unwrap().parse::<u64>().unwrap() <= a["balance"].as_str().unwrap().parse::<u64>().unwrap(), "{a}");
        assert_eq!((a["locks"][0]["id"].as_str(), a["locks"][0]["vesting"].as_bool()), (Some("vote:0"), Some(false)), "{a}");
        assert_eq!((a["exists"].as_bool(), a["creation_fee"].as_str()), (Some(true), Some("0")), "{a}");
        // An address with no account yet: paying it burns the creation fee from the payer.
        let fresh = crate::crypto::Keys::from_seed([77; 32]).address();
        let (_, f, _) = get(format!("/api/account/{fresh}")).await;
        let fee = n.chain.lock().state.chain_params.account_creation_fee;
        assert!(fee > 0);
        assert_eq!((f["exists"].as_bool(), f["creation_fee"].as_str(), f["locked"].as_str(), f["transferable"].as_str()), (Some(false), Some(fee.to_string().as_str()), Some("0"), Some("0")), "{f}");
        std::fs::remove_dir_all(root).ok();
    }
}
