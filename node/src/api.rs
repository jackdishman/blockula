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
use std::collections::{HashMap, VecDeque};
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
    /// Header set by the reverse proxy in front of this node (`fly-client-ip`, `x-forwarded-for`).
    /// Only enable it when every request comes through that proxy.
    pub header: Option<String>,
}

impl std::fmt::Debug for ClientIpPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientIpPolicy")
            .field("trusted_forwarders", &self.trusted_forwarders)
            .field("forwarder_secret", &self.forwarder_secret.as_ref().map(|_| "<redacted>"))
            .field("header", &self.header)
            .finish()
    }
}

impl ClientIpPolicy {
    pub fn resolve(&self, peer: SocketAddr, h: &HeaderMap) -> Option<IpAddr> {
        let header_ip = |name: &str| h.get(name).and_then(|v| v.to_str().ok()).and_then(|v| v.split(',').next()).and_then(|v| v.trim().parse::<IpAddr>().ok());
        let policy = self;
        let by_secret = match (&policy.forwarder_secret, h.get("x-blockula-forwarder-key")) {
            (Some(secret), Some(given)) => ct_eq(secret.as_bytes(), given.as_bytes()),
            _ => false,
        };
        if by_secret || policy.trusted_forwarders.contains(&peer.ip()) {
            if let Some(ip) = header_ip("x-blockula-client-ip") {
                return Some(ip);
            }
        }
        if let Some(ip) = policy.header.as_deref().and_then(header_ip) {
            return Some(ip);
        }
        if peer.ip().is_loopback() { None } else { Some(peer.ip()) }
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
    /// Whether every `(key, max, window)` has room (nothing recorded).
    fn check(&self, rules: &[(String, usize, Duration)]) -> Result<(), Duration> {
        let now = Instant::now();
        let mut hits = self.hits.lock();
        for (key, max, window) in rules {
            if let Some(q) = hits.get_mut(key) {
                while q.front().is_some_and(|t| now.duration_since(*t) >= *window) {
                    q.pop_front();
                }
                if q.len() >= *max {
                    return Err(*window - now.duration_since(*q.front().unwrap()));
                }
            }
        }
        Ok(())
    }

    /// Records a hit on each rule (after the action succeeded).
    fn record(&self, rules: &[(String, usize, Duration)]) {
        let now = Instant::now();
        let mut hits = self.hits.lock();
        for (key, _, _) in rules {
            hits.entry(key.clone()).or_default().push_back(now);
        }
    }

    /// Allows only if every `(key, max, window)` has room; then records a hit on each.
    fn allow(&self, rules: &[(String, usize, Duration)]) -> Result<(), Duration> {
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
        Ok(())
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

fn too_many(what: &str, wait: Duration) -> ApiError {
    let mins = wait.as_secs().div_ceil(60);
    ApiError(StatusCode::TOO_MANY_REQUESTS, format!("{what}: try again in {mins} minute{}", if mins == 1 { "" } else { "s" }))
}

impl Node {
    pub fn new(chain: Shared, marshal: Option<MarshalMailbox>, peers: Vec<String>, client_ip: ClientIpPolicy) -> Self {
        let live = chain.lock().live.clone();
        Self { chain, live, marshal, peers, client_ip, limits: Default::default(), reads: Arc::new(ReadLimiter::new(READ_RPS, READ_BURST)), timeout: HTTP_TIMEOUT, stats: Default::default(), extra_metrics: None, github_cache: Default::default() }
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
        .route("/", get(|| async { Html(EXPLORER) }))
        .route("/api/status", get(status))
        .route("/api/blocks", get(blocks))
        .route("/api/block/{id}", get(block))
        .route("/api/tx/{id}", get(tx))
        .route("/api/tx", post(submit))
        .route("/api/account/{id}", get(account))
        .route("/api/name/{name}", get(name))
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
        .route("/api/proof/{namespace}/{key}", get(proof))
        .route("/api/health", get(health))
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
    tx_json_with(v, tx, height, None)
}

fn tx_json_with(v: &ReadView, tx: &Tx, height: Option<u64>, error: Option<&str>) -> Value {
    let st = &v.state;
    let sender = tx.sender().unwrap_or_default();
    let (kind, to, amount) = match &tx.action {
        Action::Transfer { to, amount } => ("transfer", st.resolve(to).ok(), Some(*amount)),
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
        Action::AwardHunt { to, .. } => ("award_hunt", st.resolve(to).ok(), None),
        Action::CancelHunt { .. } => ("cancel_hunt", None, None),
        Action::RegisterFamiliar { .. } => ("register_familiar", None, None),
        Action::SetRepo { .. } => ("set_repo", None, None),
        Action::GrantSoul { to, .. } => ("grant_soul", st.resolve(to).ok(), None),
        Action::Vouch { account } => ("vouch", st.resolve(account).ok(), Some(crate::poc::VOUCH_BOND)),
        Action::Unvouch { account } => ("unvouch", st.resolve(account).ok(), None),
        Action::FlagSybil { account, .. } => ("flag_sybil", st.resolve(account).ok(), None),
        Action::SubmitClaim { .. } => ("submit_claim", None, None),
        Action::AttestClaim { .. } => ("attest_claim", None, None),
        Action::RejectClaim { .. } => ("reject_claim", None, None),
        Action::JoinJury { bond } => ("join_jury", None, Some(*bond)),
        Action::LeaveJury {} => ("leave_jury", None, None),
        Action::CommitVotes { .. } => ("commit_votes", None, None),
        Action::RevealVotes { .. } => ("reveal_votes", None, None),
        Action::FundPool { amount } => ("fund_pool", Some(st.pool()), Some(*amount)),
        Action::WithdrawVested {} => ("withdraw_vested", None, None),
        Action::Clawback { account, .. } => ("clawback", st.resolve(account).ok(), None),
        Action::Delegate { validator, amount } => ("delegate", Some(validator.clone()), Some(*amount)),
        Action::Propose { deposit, .. } => ("propose", None, Some(*deposit)),
        Action::Vote { amount, .. } => ("vote", None, Some(*amount)),
        Action::TransferName { to, .. } => ("transfer_name", st.resolve(to).ok(), None),
        Action::ClaimDistribution { amount, .. } => ("claim_distribution", None, Some(*amount)),
        a => (a.kind(), None, None),
    };
    json!({
        "id": tx.id(),
        "summary": v.describe(tx, height),
        "kind": kind,
        "status": match (height, error) { (_, Some(_)) => "failed", (Some(_), None) => "finalized", (None, None) => "pending" },
        "error": error,
        "height": height,
        "from": label(v, &sender),
        "to": to.map(|t| label(v, &t)),
        "amount": amount.map(|a| a.to_string()),
        "amount_display": amount.map(fmt_fang),
        // What the tx is charged (base fee burned + tip); `max_fee` is what it allowed.
        "fee": tx.charge().unwrap_or(tx.fee).to_string(),
        "fee_display": fmt_fang(tx.charge().unwrap_or(tx.fee)),
        "max_fee": tx.fee.to_string(),
        "tip": tx.tip.to_string(),
        "valid_until": tx.valid_until,
        "size_bytes": tx.size(),
        "nonce": tx.nonce,
        "signer_key": tx.signer_key_id().ok(),
        "memo": tx.memo,
        "action": tx.action,
        "signature_scheme": "ML-DSA-65 (FIPS 204)",
    })
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
        "summary": block_summary(height, m.tx_count as usize),
    })
}

fn block_json(v: &ReadView, b: &Block) -> Value {
    json!({
        "height": b.height,
        "hash": b.hash,
        "prev_hash": b.prev_hash,
        "timestamp_ms": b.timestamp_ms,
        "proposer": label(v, &b.proposer),
        "state_root": b.state_root,
        "tx_count": b.txs.len(),
        "summary": block_summary(b.height, b.txs.len()),
        "txs": b.txs.iter().enumerate().map(|(i, t)| tx_json_with(v, t, Some(b.height), b.failed.get(&i).map(|s| s.as_str()))).collect::<Vec<_>>(),
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
        "address_prefix": g.address_prefix,
        "max_tx_validity_blocks": st.chain_params.max_tx_validity_blocks,
        "protocol_version": crate::consensus::PROTOCOL_VERSION,
        "crypto": { "signatures": "ML-DSA-65 (FIPS 204)", "hash": "BLAKE3-256", "addresses": format!("bech32m({}, BLAKE3(pubkey))", g.address_prefix), "p2p_transport": "hybrid X25519 + ML-KEM-768 key exchange, ChaCha20-Poly1305, ML-DSA-65 peer auth" },
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
            return Ok(Json(tx_json_with(&v, &t, Some(h), err.as_deref())));
        }
    }
    let mp = n.live.mempool();
    if let Some(pending) = mp.get(&id) {
        return Ok(Json(tx_json(&v, pending, None)));
    }
    if let Some((reason, height)) = mp.failure(&id) {
        return Ok(Json(json!({ "id": id, "status": "failed", "error": reason, "height": height, "summary": format!("Transaction failed: {reason}") })));
    }
    Err(not_found("transaction"))
}

async fn submit(State(n): State<Node>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(tx): Json<Tx>) -> ApiResult {
    let client = n.client_ip(peer, &headers);
    if let Some(ip) = client {
        n.limits.allow(&[(format!("tx:{}", ip_key(ip)), TX_PER_IP_PER_MINUTE, Duration::from_secs(60))]).map_err(|w| too_many("too many transactions from your IP", w))?;
    }
    let result = submit_inner(&n, &headers, client, tx).await;
    bump(if result.is_ok() { &n.stats.tx_submitted } else { &n.stats.tx_rejected });
    result
}

async fn submit_inner(n: &Node, headers: &HeaderMap, client: Option<IpAddr>, tx: Tx) -> ApiResult {
    // Format + ML-DSA signature checks run without the chain lock.
    let (chain_id, prefix, role) = {
        let v = n.live.view();
        (v.genesis.chain_id.clone(), v.genesis.address_prefix.clone(), v.role)
    };
    crate::consensus::check_tx_stateless(&tx, &chain_id, &prefix)?;
    if role == Role::FullNode {
        if forwarded(headers) {
            return Err(ApiError(StatusCode::LOOP_DETECTED, "forwarding loop: this full node was sent a forwarded tx".into()));
        }
        // Admit locally (same checks as a validator), then hand it to a validator.
        let id = n.chain.lock().submit(tx.clone())?;
        if let Err(e) = n.forward("/tx", &serde_json::to_value(&tx).unwrap(), client).await {
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

/// 200 when the head is fresh and we're not syncing; 503 with a reason otherwise.
async fn health(State(n): State<Node>) -> Response {
    let v = n.live.view();
    let bt = v.genesis.block_time_ms.max(1);
    let age = head_age_ms(&v);
    let reason = match age {
        _ if n.live.syncing(v.height) => Some(format!("syncing: {} of {}", v.height, n.live.network_height_claimed(v.height))),
        None => Some("no blocks yet".to_string()),
        Some(a) if a > 10 * bt => Some(format!("head is {}s old", a / 1000)),
        _ => None,
    };
    let body = json!({ "ok": reason.is_none(), "reason": reason, "height": v.height, "head_age_ms": age, "finalized_height": n.live.finalized(v.height) });
    let code = if reason.is_none() { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (code, Json(body)).into_response()
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
            ("blockula_faucet_drips_total", "Faucet drips served", n.stats.faucet_drips.load(Relaxed) as f64),
            ("blockula_tx_submitted_total", "Transactions accepted over HTTP", n.stats.tx_submitted.load(Relaxed) as f64),
            ("blockula_tx_rejected_total", "Transactions rejected over HTTP", n.stats.tx_rejected.load(Relaxed) as f64),
            ("blockula_http_reads_limited_total", "HTTP reads refused by the per-client rate limit", n.stats.reads_limited.load(Relaxed) as f64),
        ] {
            let kind = if name.ends_with("_total") { "counter" } else { "gauge" };
            let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {val}");
        }
        let _ = writeln!(out, "# HELP blockula_build_info Version and git sha\n# TYPE blockula_build_info gauge\nblockula_build_info{{version=\"{VERSION}\",git_sha=\"{GIT_SHA}\"}} 1");
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
    recent.extend(v.history(&addr, 25).into_iter().filter_map(|(h, i)| v.finalized_tx(h, i).map(|(t, e)| tx_json_with(&v, &t, Some(h), e.as_deref()))));
    let hunts_created: Vec<u64> = v.ids(ix::HUNT_CREATOR, &addr, false, ACCOUNT_LIST_LIMIT);
    let hunts_won: Vec<Value> = v.ids(ix::HUNT_WINNER, &addr, false, ACCOUNT_LIST_LIMIT).into_iter().filter_map(|id| st.hunts.get(&id)).map(|h| json!({ "id": h.id, "repo": h.repo, "issue": h.issue, "amount_display": fmt_fang(h.amount), "pr": h.pr, "self_awarded": h.self_awarded() })).collect();
    let keys = st.keys_of(&addr);
    Ok(Json(json!({
        "account": label(&v, &addr),
        "balance": acct.balance.to_string(),
        "balance_display": fmt_fang(acct.balance),
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
async fn key_accounts(State(n): State<Node>, Path(key_id): Path<String>) -> ApiResult {
    let v = n.live.view();
    let st = &v.state;
    let mut accounts: Vec<Value> = v
        .accounts_with_key(&key_id, ACCOUNT_LIST_LIMIT)
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
            if !listed && st.keys_of(&own).contains_key(&key_id) {
                accounts.insert(0, json!({ "account": label(&v, &own), "role": { "type": "owner" }, "label": "original key" }));
            }
        }
    }
    Ok(Json(json!({ "key_id": key_id, "accounts": accounts })))
}

async fn name(State(node): State<Node>, Path(n): Path<String>) -> ApiResult {
    let v = node.live.view();
    let n = normalize_name(&n)?;
    match v.state.names.get(&n) {
        Some(r) => Ok(Json(json!({ "name": format!("{n}{NAME_SUFFIX}"), "available": false, "owner": label(&v, &r.owner), "registered_at": r.registered_at }))),
        None => Ok(Json(json!({ "name": format!("{n}{NAME_SUFFIX}"), "available": true, "price": name_price(&n).to_string(), "price_display": fmt_fang(name_price(&n)) }))),
    }
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
    let limited = client.is_some() || !peer.ip().is_loopback();
    if limited {
        n.limits.check(&rules).map_err(|w| too_many("the faucet already dripped for you", w))?;
    }
    // Quota is only consumed once the drip actually went out.
    let v = if !faucet_here {
        if forwarded(&headers) {
            return Err(ApiError(StatusCode::LOOP_DETECTED, "forwarding loop: no faucet here".into()));
        }
        let v = n.forward("/faucet", &json!({ "to": to }), client).await?;
        if let Ok(tx) = serde_json::from_value::<Tx>(v["tx"].clone()) {
            let _ = n.chain.lock().track_pending(tx);
        }
        v
    } else {
        let tx = n.chain.lock().faucet(to, 100 * DROPS_PER_FANG).map_err(|e| {
            let msg = e.to_string();
            ApiError(if msg.contains("faucet busy") { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::BAD_REQUEST }, msg)
        })?;
        json!({ "id": tx.id(), "amount_display": "100 FANG", "tx": tx })
    };
    if limited {
        n.limits.record(&rules);
    }
    bump(&n.stats.faucet_drips);
    Ok(Json(v))
}

/// Manifest of the newest certified state snapshot (for `join --fast`).
async fn snapshot_latest(State(n): State<Node>) -> ApiResult {
    let m = crate::snapshot::build_manifest(&n.live).map_err(|e| ApiError(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    Ok(Json(serde_json::to_value(m).unwrap()))
}

async fn snapshot_chunk(State(n): State<Node>, Path((height, i)): Path<(u64, usize)>) -> Response {
    let home = n.live.disk.home.clone();
    match crate::snapshot::chunk(&home, height, i) {
        Some(bytes) => ([(axum::http::header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response(),
        None => not_found("snapshot chunk").into_response(),
    }
}

/// Validator-set history with handoff proofs.
async fn epochs(State(n): State<Node>) -> ApiResult {
    let e = n.live.epochs();
    let mut v = serde_json::to_value(&*e).unwrap();
    v["proven_through"] = json!(e.proven_through());
    Ok(Json(v))
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
        "commission_bps": r.commission_bps,
        "commission_display": format!("{}%", staking::fmt_bps(r.commission_bps)),
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
        "unbonding_blocks": cp.unbonding_blocks,
        "jail_blocks": cp.jail_blocks,
        "evidence_max_age_epochs": cp.evidence_max_age_epochs,
        "downtime_window": cp.downtime_window,
        "downtime_max_missed": cp.downtime_max_missed,
        "slash_double_sign_display": format!("{}%", staking::fmt_bps(cp.slash_double_sign_bps)),
        "slash_downtime_display": format!("{}%", staking::fmt_bps(cp.slash_downtime_bps)),
        "explainer": "Validators take turns proposing blocks and vote to finalize them; a block is final once more than two-thirds of the active set have signed it (ML-DSA-65), one vote each. Anyone can register a validator with a self-bond and anyone can delegate to one. At each epoch boundary the next set is the top validators by stake, changing at most a third of the seats at a time. Double-signing is slashed and banned for good; a validator that misses too many of its turns is jailed until it unjails.",
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
    let summary = match (bonded, unbonding) {
        (0, 0) => format!("{who} has nothing staked"),
        _ => {
            let pools = match delegations.len() {
                1 => "1 validator".to_string(),
                n => format!("{n} validators"),
            };
            let out = if unbonding == 0 { String::new() } else { format!(" and {} unbonding ({} ready to withdraw)", fmt_fang(unbonding), fmt_fang(ready)) };
            format!("{who} has {} staked with {pools}{out}", fmt_fang(bonded))
        }
    };
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

/// Bytes of block data per `/api/sync` response (the joiner caps what it reads, too).
pub const SYNC_BATCH_BYTES: usize = 8 << 20;

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
    let epoch_end = v.state.params.last_of(v.state.params.epoch_of(from));
    let range_end = tip.min(epoch_end); // never mix epochs (different validator sets)
    if from > tip {
        return Ok(Json(json!({ "tip": tip, "blocks": [], "to": null, "finalization": null })));
    }
    if v.is_pruned(from) {
        return Err(pruned(&v, from));
    }
    let want = (from + q.limit.unwrap_or(100).clamp(1, 500) - 1).min(range_end);
    // Blocks up to `want`, cut at the byte budget, and the highest certified height among them.
    let (mut blocks, mut cert) = (vec![], None);
    let mut bytes = 0usize;
    let mut last_cert_len = 0usize;
    for h in from..=want {
        let Some(b) = v.cblock(h) else { break };
        bytes += b.len() * 2;
        if bytes > SYNC_BATCH_BYTES && !blocks.is_empty() {
            break;
        }
        blocks.push(hex::encode(&b));
        if let Some(f) = v.cert(h) {
            cert = Some((h, hex::encode(f)));
            last_cert_len = blocks.len();
        }
    }
    // No certificate in range: extend to the next certified height (still within the epoch).
    if cert.is_none() && bytes <= SYNC_BATCH_BYTES {
        let mut h = from + blocks.len() as u64;
        while h <= range_end && h < from + 600 {
            let Some(b) = v.cblock(h) else { break };
            blocks.push(hex::encode(&b));
            if let Some(f) = v.cert(h) {
                cert = Some((h, hex::encode(f)));
                last_cert_len = blocks.len();
                break;
            }
            h += 1;
        }
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
    let Some((to, finalization)) = cert else {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, format!("no finalization certificate stored for heights {from}..={range_end}; try another peer")));
    };
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

    #[tokio::test]
    async fn status_health_metrics_genesis_and_sync() {
        let (root, n) = node("ops");
        let (s, status, _) = call(&n, "GET", "/api/status", None, "127.0.0.1:1").await;
        assert_eq!(s, StatusCode::OK);
        assert!(status.get("data_dir").is_none(), "data_dir isn't exposed");
        assert_eq!(status["version"], VERSION);
        assert!(status["genesis_hash"].as_str().unwrap().len() == 64);
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
            let mut t = Tx { chain_id: "nolock".into(), from: alice.address(), nonce: 0, fee: 0, tip: 0, valid_until: 50, action: Action::RegisterName { name: "alice".into() }, memo: String::new(), public_key: hex::encode(alice.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
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

    #[tokio::test]
    async fn failed_txs_report_status_instead_of_404() {
        let (root, n) = node("txfail");
        let k = crate::crypto::Keys::from_seed([9; 32]);
        n.chain.lock().faucet(&k.address(), 10 * DROPS_PER_FANG).unwrap();
        n.chain.lock().produce_block().unwrap();
        let me = n.chain.lock().me().unwrap();
        let valid_until = n.chain.lock().height() + 100;
        let mk = |nonce: u64| {
            let mut tx = Tx { chain_id: "txfail".into(), from: k.address(), nonce, fee: 0, tip: 0, valid_until, action: Action::Transfer { to: me.clone(), amount: 6 * DROPS_PER_FANG }, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
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
        std::fs::remove_dir_all(root).ok();
    }
}
