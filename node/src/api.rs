use crate::chain::{short, Chain, Role};
use crate::consensus::MarshalMailbox;
use crate::crypto;
use crate::types::*;
use axum::{
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
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
    pub chain: Shared,
    /// Validators: marshal, to serve finalized blocks + certificates to joining nodes.
    pub marshal: Option<MarshalMailbox>,
    /// Full nodes (and validators without a faucet account): validator HTTP endpoints that txs
    /// and faucet requests are forwarded to.
    pub peers: Vec<String>,
    pub client_ip: ClientIpPolicy,
    limits: Arc<Limits>,
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
        Self { chain, marshal, peers, client_ip, limits: Default::default(), stats: Default::default(), extra_metrics: None, github_cache: Default::default() }
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
        (self.0, Json(json!({ "error": self.1 }))).into_response()
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
        .route("/api/health", get(health))
        .route("/metrics", get(metrics))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(node)
}

fn label(c: &Chain, addr: &str) -> Value {
    let name = if addr == c.state.treasury() { Some("🏛️ treasury".to_string()) } else if addr == c.state.pool() { Some("🪦 contribution pool".to_string()) } else if c.state.validators.get(addr).is_some_and(|v| v.status == crate::chain::ValidatorStatus::Active) { c.state.primary_name(addr).or(Some("🧛 validator".into())) } else { c.state.primary_name(addr) };
    json!({ "address": addr, "name": name, "short": short(addr) })
}

fn tx_json(c: &Chain, tx: &Tx, height: Option<u64>) -> Value {
    tx_json_with(c, tx, height, None)
}

fn tx_json_with(c: &Chain, tx: &Tx, height: Option<u64>, error: Option<&str>) -> Value {
    let sender = tx.sender().unwrap_or_default();
    let (kind, to, amount) = match &tx.action {
        Action::Transfer { to, amount } => ("transfer", c.state.resolve(to).ok(), Some(*amount)),
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
        Action::AwardHunt { to, .. } => ("award_hunt", c.state.resolve(to).ok(), None),
        Action::CancelHunt { .. } => ("cancel_hunt", None, None),
        Action::RegisterFamiliar { .. } => ("register_familiar", None, None),
        Action::SetRepo { .. } => ("set_repo", None, None),
        Action::GrantSoul { to, .. } => ("grant_soul", c.state.resolve(to).ok(), None),
        Action::Vouch { account } => ("vouch", c.state.resolve(account).ok(), Some(crate::poc::VOUCH_BOND)),
        Action::Unvouch { account } => ("unvouch", c.state.resolve(account).ok(), None),
        Action::FlagSybil { account, .. } => ("flag_sybil", c.state.resolve(account).ok(), None),
        Action::SubmitClaim { .. } => ("submit_claim", None, None),
        Action::AttestClaim { .. } => ("attest_claim", None, None),
        Action::RejectClaim { .. } => ("reject_claim", None, None),
        Action::JoinJury { bond } => ("join_jury", None, Some(*bond)),
        Action::LeaveJury {} => ("leave_jury", None, None),
        Action::CommitVotes { .. } => ("commit_votes", None, None),
        Action::RevealVotes { .. } => ("reveal_votes", None, None),
        Action::FundPool { amount } => ("fund_pool", Some(c.state.pool()), Some(*amount)),
        Action::WithdrawVested {} => ("withdraw_vested", None, None),
        Action::Clawback { account, .. } => ("clawback", c.state.resolve(account).ok(), None),
    };
    json!({
        "id": tx.id(),
        "summary": c.state.describe(tx, height),
        "kind": kind,
        "status": match (height, error) { (_, Some(_)) => "failed", (Some(_), None) => "finalized", (None, None) => "pending" },
        "error": error,
        "height": height,
        "from": label(c, &sender),
        "to": to.map(|t| label(c, &t)),
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

fn block_json(c: &Chain, b: &Block, full: bool) -> Value {
    let mut v = json!({
        "height": b.height,
        "hash": b.hash,
        "prev_hash": b.prev_hash,
        "timestamp_ms": b.timestamp_ms,
        "proposer": label(c, &b.proposer),
        "state_root": b.state_root,
        "tx_count": b.txs.len(),
        "summary": format!("Block #{} sealed with {} transaction{}", b.height, b.txs.len(), if b.txs.len() == 1 { "" } else { "s" }),
    });
    if full {
        v["txs"] = b.txs.iter().enumerate().map(|(i, t)| tx_json_with(c, t, Some(b.height), b.failed.get(&i).map(|s| s.as_str()))).collect();
        v["signature_bytes"] = json!(c.cert_len(b.height));
        v["failed_count"] = json!(b.failed.len());
    }
    v
}

fn sync_progress(c: &Chain) -> f64 {
    let t = c.network_height_claimed();
    if t == 0 { 1.0 } else { (c.height() as f64 / t as f64).min(1.0) }
}

async fn status(State(n): State<Node>) -> ApiResult {
    let c = n.chain.lock();
    Ok(Json(json!({
        "chain_id": c.genesis.chain_id,
        "token": c.genesis.token,
        "decimals": c.genesis.decimals,
        "height": c.height(),
        "block_time_ms": c.genesis.block_time_ms,
        "validator": label(&c, &c.genesis.validators[0].address),
        "validators": c.state.active_sets.get(&c.state.params.epoch_of(c.height() + 1)).map(|s| s.iter().map(|a| label(&c, a)).collect::<Vec<_>>()).unwrap_or_default(),
        "role": c.role,
        "finalized_height": c.finalized_tip.max(c.height()),
        "network_height_claimed": c.network_height_claimed(),
        "syncing": c.syncing(),
        "sync_progress": sync_progress(&c),
        "state_root": c.state.root(),
        "consensus": "simplex BFT",
        "epoch": c.state.params.epoch_of(c.height() + 1),
        "active_validators": c.state.active_sets.get(&c.state.params.epoch_of(c.height() + 1)).map(|s| s.len()).unwrap_or(0),
        "faucet": c.genesis.faucet,
        "supply": c.state.supply.to_string(),
        "supply_display": fmt_fang(c.state.supply),
        "burned": c.state.burned.to_string(),
        "burned_display": fmt_fang(c.state.burned),
        "accounts": c.state.accounts.len(),
        "names": c.state.names.len(),
        "mempool": c.mempool.len(),
        "version": VERSION,
        "git_sha": GIT_SHA,
        "genesis_hash": c.genesis_hash,
        "head_age_ms": c.last_block().map(|b| crate::chain::now_ms().saturating_sub(b.timestamp_ms)),
        "history_from": c.base_height() + 1,
        "fast_synced_from": c.base.as_ref().map(|b| b.height),
        "address_prefix": c.genesis.address_prefix,
        "max_tx_validity_blocks": c.state.params.max_tx_validity_blocks(),
        "protocol_version": crate::consensus::PROTOCOL_VERSION,
        "crypto": { "signatures": "ML-DSA-65 (FIPS 204)", "hash": "BLAKE3-256", "addresses": format!("bech32m({}, BLAKE3(pubkey))", c.genesis.address_prefix), "p2p_transport": "hybrid X25519 + ML-KEM-768 key exchange, ChaCha20-Poly1305, ML-DSA-65 peer auth" },
    })))
}

#[derive(Deserialize)]
struct Page {
    limit: Option<usize>,
    before: Option<u64>,
}

async fn blocks(State(n): State<Node>, Query(p): Query<Page>) -> ApiResult {
    let c = n.chain.lock();
    let limit = p.limit.unwrap_or(20).min(100);
    let before = p.before.unwrap_or(u64::MAX);
    let out: Vec<Value> = c.blocks_before(before, limit).iter().map(|b| block_json(&c, b, false)).collect();
    Ok(Json(json!(out)))
}

fn find_block(c: &Chain, id: &str) -> Option<Block> {
    match id.parse::<u64>() {
        Ok(h) if h >= 1 => c.block(h),
        _ => c.block_by_hash(id),
    }
}

/// A finalized tx and its failure reason, if any.
fn finalized_tx(c: &Chain, h: u64, i: usize) -> Option<(Tx, Option<String>)> {
    let b = c.block(h)?;
    let err = b.failed.get(&i).cloned();
    b.txs.into_iter().nth(i).map(|t| (t, err))
}

async fn block(State(n): State<Node>, Path(id): Path<String>) -> ApiResult {
    let c = n.chain.lock();
    if let Ok(h) = id.parse::<u64>() {
        if h >= 1 && h <= c.base_height() {
            return Err(ApiError(StatusCode::NOT_FOUND, format!("history before #{} isn't available on this node (it fast-synced from a certified snapshot at #{})", c.base_height() + 1, c.base_height())));
        }
    }
    let b = find_block(&c, &id).ok_or_else(|| not_found("block"))?;
    Ok(Json(block_json(&c, &b, true)))
}

async fn tx(State(n): State<Node>, Path(id): Path<String>) -> ApiResult {
    let c = n.chain.lock();
    if let Some(&(h, i)) = c.tx_index.get(&id) {
        if let Some((t, err)) = finalized_tx(&c, h, i) {
            return Ok(Json(tx_json_with(&c, &t, Some(h), err.as_deref())));
        }
    }
    if c.in_mempool(&id) {
        if let Some(pending) = c.mempool.iter().find(|t| t.id() == id) {
            return Ok(Json(tx_json(&c, pending, None)));
        }
    }
    if let Some((reason, height)) = c.failure(&id) {
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
    // Format + ML-DSA signature checks run without holding the chain lock.
    let (chain_id, prefix, role) = {
        let c = n.chain.lock();
        (c.genesis.chain_id.clone(), c.genesis.address_prefix.clone(), c.role)
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
    let c = n.chain.lock();
    let limit = p.limit.unwrap_or(50).min(200);
    let txs: Vec<Value> = c.mempool.iter().rev().take(limit).map(|t| tx_json(&c, t, None)).collect();
    Ok(Json(json!({ "count": c.mempool.len(), "txs": txs })))
}

/// 200 when the head is fresh and we're not syncing; 503 with a reason otherwise.
async fn health(State(n): State<Node>) -> Response {
    let c = n.chain.lock();
    let bt = c.genesis.block_time_ms.max(1);
    let age = c.last_block().map(|b| crate::chain::now_ms().saturating_sub(b.timestamp_ms));
    let syncing = c.syncing();
    let reason = match age {
        _ if syncing => Some(format!("syncing: {} of {}", c.height(), c.network_height_claimed())),
        None => Some("no blocks yet".to_string()),
        Some(a) if a > 10 * bt => Some(format!("head is {}s old", a / 1000)),
        _ => None,
    };
    let body = json!({ "ok": reason.is_none(), "reason": reason, "height": c.height(), "head_age_ms": age, "finalized_height": c.finalized_tip.max(c.height()) });
    let code = if reason.is_none() { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (code, Json(body)).into_response()
}

/// Prometheus text format.
async fn metrics(State(n): State<Node>) -> Response {
    use std::fmt::Write as _;
    use std::sync::atomic::Ordering::Relaxed;
    let mut out = String::new();
    {
        let c = n.chain.lock();
        let epoch = c.state.params.epoch_of(c.height() + 1);
        let active = c.state.active_sets.get(&epoch).map(|s| s.len()).unwrap_or(0);
        let head_age = c.last_block().map(|b| crate::chain::now_ms().saturating_sub(b.timestamp_ms)).unwrap_or(0);
        let online = online_validators(&c);
        for (name, help, v) in [
            ("blockula_height", "Executed height", c.height() as f64),
            ("blockula_finalized_height", "Highest certified finalized height", c.finalized_tip.max(c.height()) as f64),
            ("blockula_finalized_lag", "Finalized height minus executed height", c.finalized_tip.saturating_sub(c.height()) as f64),
            ("blockula_head_age_seconds", "Seconds since the head block's timestamp", head_age as f64 / 1000.0),
            ("blockula_mempool_size", "Pending transactions", c.mempool.len() as f64),
            ("blockula_epoch", "Current epoch", epoch as f64),
            ("blockula_active_validators", "Validators in the active set", active as f64),
            ("blockula_validators_online", "Signers on the latest finalization certificate", online.map(|o| o as f64).unwrap_or(f64::NAN)),
            ("blockula_faucet_drips_total", "Faucet drips served", n.stats.faucet_drips.load(Relaxed) as f64),
            ("blockula_tx_submitted_total", "Transactions accepted over HTTP", n.stats.tx_submitted.load(Relaxed) as f64),
            ("blockula_tx_rejected_total", "Transactions rejected over HTTP", n.stats.tx_rejected.load(Relaxed) as f64),
        ] {
            let kind = if name.ends_with("_total") { "counter" } else { "gauge" };
            let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {v}");
        }
        let _ = writeln!(out, "# HELP blockula_build_info Version and git sha\n# TYPE blockula_build_info gauge\nblockula_build_info{{version=\"{VERSION}\",git_sha=\"{GIT_SHA}\"}} 1");
    }
    if let Some(extra) = &n.extra_metrics {
        out.push_str(&extra());
    }
    ([(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")], out).into_response()
}

/// How many validators signed the most recent finalization certificate we hold.
pub fn online_validators(c: &Chain) -> Option<usize> {
    use commonware_codec::Decode as _;
    let h = (c.height().saturating_sub(16)..=c.height()).rev().find(|h| c.cert_len(*h) > 0)?;
    let bytes = c.cert(h)?;
    let f = commonware_consensus::simplex::types::Finalization::<crate::pq::MlDsaScheme, commonware_cryptography::blake3::Digest>::decode_cfg(bytes.as_slice(), &1024usize).ok()?;
    Some(f.certificate.signers.count())
}

async fn account(State(n): State<Node>, Path(id): Path<String>) -> ApiResult {
    let c = n.chain.lock();
    let addr = c.state.resolve(&id)?;
    let acct = c.state.account(&addr);
    let names: Vec<String> = c.state.names.values().filter(|r| r.owner == addr).map(|r| format!("{}{NAME_SUFFIX}", r.name)).collect();
    let mut recent: Vec<Value> = c
        .mempool
        .iter()
        .filter(|t| t.from == addr || matches!(&t.action, Action::Transfer { to, .. } if c.state.resolve(to).ok().as_deref() == Some(addr.as_str())))
        .rev()
        .map(|t| tx_json(&c, t, None))
        .collect();
    recent.extend(c.history.get(&addr).map(|ids| {
        ids.iter().rev().take(25).filter_map(|id| c.tx_index.get(id)).filter_map(|&(h, i)| finalized_tx(&c, h, i).map(|(t, e)| tx_json_with(&c, &t, Some(h), e.as_deref()))).collect::<Vec<_>>()
    }).unwrap_or_default());
    Ok(Json(json!({
        "account": label(&c, &addr),
        "balance": acct.balance.to_string(),
        "balance_display": fmt_fang(acct.balance),
        "nonce": acct.nonce,
        "next_nonce": c.next_nonce(&addr),
        "names": names,
        "keys": c.state.keys_of(&addr).into_iter().map(|(id, k)| json!({ "key_id": id, "label": k.label, "role": k.role, "added_at": k.added_at, "spent_this_period": k.spent.to_string(), "window_start": k.window_start })).collect::<Vec<_>>(),
        "recent_txs": recent,
        "github": c.state.github_by_account.get(&addr).and_then(|h| c.state.github.get(h)).map(|l| json!({ "handle": l.handle, "gist_id": l.gist_id, "linked_at": l.linked_at, "verify_url": format!("/api/github/{}", l.handle) })),
        "familiars": c.state.familiars.get(&addr).map(|f| f.iter().filter(|(k, _)| c.state.keys_of(&addr).contains_key(*k)).map(|(k, l)| json!({ "key_id": k, "label": l })).collect::<Vec<_>>()).unwrap_or_default(),
        "hunts_created": c.state.hunts.values().filter(|h| h.creator == addr).map(|h| h.id).collect::<Vec<_>>(),
        "hunts_won": c.state.hunts.values().filter(|h| h.awarded_to.as_deref() == Some(addr.as_str())).map(|h| json!({ "id": h.id, "repo": h.repo, "issue": h.issue, "amount_display": fmt_fang(h.amount), "pr": h.pr, "self_awarded": h.self_awarded() })).collect::<Vec<_>>(),
        "contribution": contribution_profile(&c, &addr),
    })))
}

/// Which accounts a key may act for — lets a newly linked device find its account.
async fn key_accounts(State(n): State<Node>, Path(key_id): Path<String>) -> ApiResult {
    let c = n.chain.lock();
    let mut accounts: Vec<Value> = c.state.accounts.iter().filter(|(_, a)| a.keys.contains_key(&key_id)).map(|(addr, a)| json!({ "account": label(&c, addr), "role": a.keys[&key_id].role, "label": a.keys[&key_id].label })).collect();
    if let Ok(bytes) = hex::decode(&key_id) {
        if let Ok(bytes) = <[u8; 32]>::try_from(bytes) {
            let own = crypto::render_address(c.state.params.prefix(), &bytes);
            let listed = accounts.iter().any(|a| a["account"]["address"] == own.as_str());
            if !listed && c.state.keys_of(&own).contains_key(&key_id) {
                accounts.insert(0, json!({ "account": label(&c, &own), "role": { "type": "owner" }, "label": "original key" }));
            }
        }
    }
    Ok(Json(json!({ "key_id": key_id, "accounts": accounts })))
}

async fn name(State(node): State<Node>, Path(n): Path<String>) -> ApiResult {
    let c = node.chain.lock();
    let n = normalize_name(&n)?;
    match c.state.names.get(&n) {
        Some(r) => Ok(Json(json!({ "name": format!("{n}{NAME_SUFFIX}"), "available": false, "owner": label(&c, &r.owner), "registered_at": r.registered_at }))),
        None => Ok(Json(json!({ "name": format!("{n}{NAME_SUFFIX}"), "available": true, "price": name_price(&n).to_string(), "price_display": fmt_fang(name_price(&n)) }))),
    }
}

async fn search(State(n): State<Node>, Path(q): Path<String>) -> ApiResult {
    let c = n.chain.lock();
    let q = q.trim().to_lowercase();
    let hit = if find_block(&c, &q).is_some() {
        json!({ "type": "block", "id": q })
    } else if c.tx_index.contains_key(&q) || c.in_mempool(&q) || c.failure(&q).is_some() {
        json!({ "type": "tx", "id": q })
    } else if crypto::is_address(&q) {
        json!({ "type": "account", "id": q })
    } else if let Ok(n) = normalize_name(&q) {
        if c.state.names.contains_key(&n) { json!({ "type": "account", "id": format!("{n}{NAME_SUFFIX}") }) } else { json!({ "type": "name", "id": n }) }
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
    let c = n.chain.lock();
    let m = crate::snapshot::build_manifest(&c).map_err(|e| ApiError(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    Ok(Json(serde_json::to_value(m).unwrap()))
}

async fn snapshot_chunk(State(n): State<Node>, Path((height, i)): Path<(u64, usize)>) -> Response {
    let home = n.chain.lock().home.clone();
    match crate::snapshot::chunk(&home, height, i) {
        Some(bytes) => ([(axum::http::header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response(),
        None => not_found("snapshot chunk").into_response(),
    }
}

/// Validator-set history with handoff proofs.
async fn epochs(State(n): State<Node>) -> ApiResult {
    let c = n.chain.lock();
    let mut v = serde_json::to_value(&c.epochs).unwrap();
    v["proven_through"] = json!(c.epochs.proven_through());
    Ok(Json(v))
}

fn hunt_json(c: &Chain, h: &crate::chain::Hunt) -> Value {
    let expired = h.status == crate::chain::HuntStatus::Open && c.height() > h.expires_at;
    let familiar = h.familiar_key.as_ref().and_then(|k| h.awarded_to.as_ref().and_then(|a| c.state.familiars.get(a)?.get(k).cloned()));
    json!({
        "id": h.id,
        "repo": h.repo,
        "issue": h.issue,
        "issue_url": format!("https://github.com/{}/issues/{}", h.repo, h.issue),
        "title": h.title,
        "amount": h.amount.to_string(),
        "amount_display": fmt_fang(h.amount),
        "creator": label(c, &h.creator),
        "funders": h.funders.iter().map(|(a, v)| json!({ "account": label(c, a), "amount": v, "amount_display": fmt_fang(v.parse().unwrap_or(0)) })).collect::<Vec<_>>(),
        "status": if expired { "expired" } else { match h.status { crate::chain::HuntStatus::Open => "open", crate::chain::HuntStatus::Awarded => "awarded", crate::chain::HuntStatus::Cancelled => "cancelled" } },
        "created_at": h.created_at,
        "expires_at": h.expires_at,
        "awarded_to": h.awarded_to.as_ref().map(|a| label(c, a)),
        "awarded_by": h.awarded_by.as_ref().map(|a| label(c, a)),
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

/// Hunts, biggest bounty first. `?status=open|awarded|cancelled|expired`.
async fn hunts(State(n): State<Node>, Query(q): Query<HuntQuery>) -> ApiResult {
    let c = n.chain.lock();
    let mut list: Vec<Value> = c.state.hunts.values().map(|h| hunt_json(&c, h)).filter(|h| q.status.as_deref().is_none_or(|s| h["status"] == s)).collect();
    list.sort_by_key(|h| std::cmp::Reverse(h["amount"].as_str().and_then(|a| a.parse::<u64>().ok()).unwrap_or(0)));
    list.truncate(q.limit.unwrap_or(100).min(500));
    let open_total: u64 = c.state.hunts.values().filter(|h| h.status == crate::chain::HuntStatus::Open && c.height() <= h.expires_at).map(|h| h.amount).sum();
    Ok(Json(json!({ "count": c.state.hunts.len(), "open_bounties": open_total.to_string(), "open_bounties_display": fmt_fang(open_total), "hunts": list })))
}

async fn hunt(State(n): State<Node>, Path(id): Path<u64>) -> ApiResult {
    let c = n.chain.lock();
    let h = c.state.hunts.get(&id).ok_or_else(|| not_found("hunt"))?;
    Ok(Json(hunt_json(&c, h)))
}

/// Upstream GitHub calls allowed per minute (per node), and how long results are cached.
pub const GITHUB_PER_MINUTE: usize = 30;
pub const GITHUB_CACHE: Duration = Duration::from_secs(600);

/// Checks a GitHub link off-chain: the gist must be owned by the handle and contain the linked
/// account's address. Only ever calls api.github.com; results are cached.
async fn github(State(n): State<Node>, Path(handle): Path<String>) -> ApiResult {
    check_github_handle(&handle)?;
    let lower = handle.to_ascii_lowercase();
    let link = n.chain.lock().state.github.get(&lower).cloned().ok_or_else(|| not_found("GitHub link"))?;
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
        let c = n.chain.lock();
        json!({ "handle": link.handle, "account": label(&c, &link.account), "gist_id": link.gist_id, "gist_url": format!("https://gist.github.com/{}", link.gist_id), "linked_at": link.linked_at, "verified": verified, "reason": reason, "attested_by": "this node (off-chain check; validators re-check it for every contribution claim)" })
    };
    n.github_cache.lock().insert(key, (Instant::now(), v.clone()));
    Ok(Json(v))
}

fn validator_json(c: &Chain, v: &crate::chain::ValidatorRecord) -> Value {
    json!({
        "validator": label(c, &v.address),
        "owner": label(c, &v.owner),
        "status": v.status,
        "consensus_key": v.consensus_key,
        "p2p_addr": v.p2p_addr,
        "http_addr": v.http_addr,
        "bond": v.bond.to_string(),
        "bond_display": fmt_fang(v.bond),
        "registered_at": v.registered_at,
        "approved_at": v.approved_at,
        "approved_by": v.approved_by.as_ref().map(|a| label(c, a)),
        "active_from_epoch": v.active_from,
        "removed_at": v.removed_at,
        "removed_by": v.removed_by.as_ref().map(|a| label(c, a)),
        "active_until_epoch": v.active_until,
        "unbond_epoch": v.unbond_epoch,
        "genesis": c.genesis.is_validator(&v.address),
    })
}

/// The validator set: who's active this epoch, who's next, who's waiting, and when it changes.
async fn validators(State(n): State<Node>) -> ApiResult {
    use crate::chain::ValidatorStatus::*;
    let c = n.chain.lock();
    let p = &c.state.params;
    let next_height = c.height() + 1;
    let epoch = p.epoch_of(next_height);
    let ids = |set: Option<&Vec<String>>| -> Vec<Value> { set.map(|s| s.iter().filter_map(|a| c.state.validators.get(a)).map(|v| validator_json(&c, v)).collect()).unwrap_or_default() };
    let by_status = |st: crate::chain::ValidatorStatus| -> Vec<Value> { c.state.validators.values().filter(|v| v.status == st).map(|v| validator_json(&c, v)).collect() };
    let next_set = c.state.next_set(epoch);
    Ok(Json(json!({
        "epoch": epoch,
        "epoch_blocks": p.epoch_blocks,
        "epoch_ends_at_height": p.last_of(epoch),
        "blocks_until_next_epoch": p.last_of(epoch) + 1 - next_height,
        "active": ids(c.state.active_sets.get(&epoch)),
        "next_epoch": ids(Some(&next_set)),
        "pending": by_status(Pending),
        "approved": by_status(Approved),
        "removed": by_status(Removed),
        "admins": p.admins.iter().map(|a| label(&c, a)).collect::<Vec<_>>(),
        "min_bond": p.min_bond.to_string(),
        "min_bond_display": fmt_fang(p.min_bond),
        "max_validators": p.max_validators,
        "explainer": "Validators take turns proposing blocks and vote to finalize them; a block is final once more than two-thirds of the active set have signed it (ML-DSA-65). The set can only change at epoch boundaries. In Season 0, admins approve new validators, who post a bond and join at the next epoch.",
    })))
}

async fn genesis(State(n): State<Node>) -> ApiResult {
    let c = n.chain.lock();
    let mut v = serde_json::to_value(&c.genesis).unwrap();
    v["genesis_hash"] = json!(c.genesis_hash);
    Ok(Json(v))
}

#[derive(Deserialize)]
struct SyncQuery {
    from: u64,
    limit: Option<u64>,
}

/// Finalized consensus blocks `[from, to]` plus the finalization certificate of block `to`, so a
/// joining node can verify the whole batch (hash links + one ML-DSA quorum certificate).
/// Bytes of block data per `/api/sync` response (the joiner caps what it reads, too).
pub const SYNC_BATCH_BYTES: usize = 8 << 20;

/// Finalized consensus blocks `[from, to]` plus the finalization certificate of block `to`, so a
/// joining node can verify the whole batch (hash links + one ML-DSA quorum certificate).
/// Served by validators and full nodes from their own block/certificate stores.
async fn sync(State(n): State<Node>, Query(q): Query<SyncQuery>) -> ApiResult {
    use commonware_codec::Encode;
    use commonware_consensus::types::Height;
    let (tip, from, range_end) = {
        let c = n.chain.lock();
        let from = q.from.max(1);
        let epoch_end = c.state.params.last_of(c.state.params.epoch_of(from));
        (c.height(), from, c.height().min(epoch_end)) // never mix epochs (different validator sets)
    };
    if from > tip {
        return Ok(Json(json!({ "tip": tip, "blocks": [], "to": null, "finalization": null })));
    }
    let want = (from + q.limit.unwrap_or(100).clamp(1, 500) - 1).min(range_end);
    // Blocks up to `want`, cut at the byte budget, and the highest certified height among them.
    let (mut blocks, mut cert) = (vec![], None);
    let mut bytes = 0usize;
    let mut last_cert_len = 0usize;
    {
        let c = n.chain.lock();
        for h in from..=want {
            let Some(b) = c.cblock(h) else { break };
            bytes += b.len() * 2;
            if bytes > SYNC_BATCH_BYTES && !blocks.is_empty() {
                break;
            }
            blocks.push(hex::encode(&b));
            if let Some(f) = c.cert(h) {
                cert = Some((h, hex::encode(f)));
                last_cert_len = blocks.len();
            }
        }
        // No certificate in range: extend to the next certified height (still within the epoch).
        if cert.is_none() && bytes <= SYNC_BATCH_BYTES {
            let mut h = from + blocks.len() as u64;
            while h <= range_end && h < from + 600 {
                let Some(b) = c.cblock(h) else { break };
                blocks.push(hex::encode(&b));
                if let Some(f) = c.cert(h) {
                    cert = Some((h, hex::encode(f)));
                    last_cert_len = blocks.len();
                    break;
                }
                h += 1;
            }
        }
    }
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

fn soul_json(c: &Chain, addr: &str) -> Value {
    let h = c.height();
    let soul = c.state.soul_of(addr, h);
    let earned = c.state.poc.souls.get(addr).map(|s| s.earned).unwrap_or(0);
    json!({ "points": soul, "display": crate::poc::fmt_soul(soul), "earned_display": crate::poc::fmt_soul(earned) })
}

fn ppm_display(ppm: u64) -> String {
    format!("{}%", (ppm + 5_000) / 10_000)
}

fn claim_json(c: &Chain, cl: &crate::poc::Claim) -> Value {
    let p = &c.state.params.poc;
    let familiar = cl.familiar_key.as_ref().and_then(|k| c.state.familiars.get(&cl.claimant)?.get(k).cloned());
    json!({
        "id": cl.id,
        "claimant": label(c, &cl.claimant),
        "handle": cl.handle,
        "repo": cl.repo,
        "pr": cl.pr,
        "pr_url": format!("https://github.com/{}/pull/{}", cl.repo, cl.pr),
        "familiar": familiar,
        "bond": cl.bond.to_string(),
        "bond_display": fmt_fang(cl.bond),
        "status": cl.status,
        "submitted_at": cl.submitted_at,
        "attestations": cl.attestations.iter().map(|(v, a)| json!({ "validator": label(c, v), "ok": a.ok, "merge_sha": a.merge_sha, "note": a.note, "at": a.at })).collect::<Vec<_>>(),
        "merge_sha": cl.merge_sha,
        "verified_at": cl.verified_at,
        "eligible_at": cl.verified_at.map(|v| v + p.claim_maturity_blocks),
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

fn seance_json(c: &Chain, s: &crate::poc::Seance, full: bool) -> Value {
    let h = c.height();
    let phase = if s.closed { "closed" } else if h <= s.commit_until { "sealing" } else { "revealing" };
    let jurors: Vec<Value> = s
        .jurors
        .iter()
        .map(|(j, seat)| {
            let mut v = json!({
                "juror": label(c, j),
                "pairs": seat.pairs.len(),
                "committed": seat.commitment.is_some(),
                "revealed": seat.votes.is_some(),
                "agreement_ppm": seat.agreement_ppm,
                "agreement_display": seat.agreement_ppm.map(ppm_display),
                "reward_display": fmt_fang(seat.reward),
                "soul_delta_display": format!("{}{}", if seat.soul_delta < 0 { "−" } else if seat.soul_delta > 0 { "+" } else { "" }, crate::poc::fmt_soul(seat.soul_delta.unsigned_abs())),
            });
            if full {
                v["assigned_pairs"] = json!(seat.pairs);
                v["commitment"] = json!(seat.commitment);
                v["votes"] = json!(seat.votes);
                v["spam"] = json!(seat.spam);
            }
            v
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
        "claims": s.claims.iter().filter_map(|id| c.state.poc.claims.get(id)).map(|cl| claim_json(c, cl)).collect::<Vec<_>>(),
        "jurors": jurors,
        "results": s.results,
        "results_display": s.results.as_ref().map(|r| json!({ "paid": fmt_fang(r.paid), "returned": fmt_fang(r.returned) })),
    })
}

/// Soul, vouches, jury seat, claims and vesting rewards for an account (`/api/account`).
fn contribution_profile(c: &Chain, addr: &str) -> Value {
    let h = c.height();
    let p = &c.state.poc;
    let (total, withdrawable, unvested) = c.state.grant_totals(addr, h);
    json!({
        "soul": soul_json(c, addr),
        "sybil": p.sybils.get(addr).map(|s| json!({ "at": s.at, "by": label(c, &s.by), "reason": s.reason })),
        "vouched_by": p.vouches.get(addr).map(|v| json!({ "voucher": label(c, &v.voucher), "at": v.at, "bond_display": fmt_fang(v.bond) })),
        "vouching_for": p.vouches.iter().filter(|(_, v)| v.voucher == addr).map(|(to, v)| json!({ "account": label(c, to), "at": v.at, "unlocks_at": v.at + c.state.params.poc.vouch_lock_blocks })).collect::<Vec<_>>(),
        "juror": p.jurors.get(addr).map(|r| json!({ "bond_display": fmt_fang(r.bond), "active": r.active, "served": r.served, "joined_at": r.joined_at })),
        "claims": p.claims.values().filter(|cl| cl.claimant == addr).rev().take(50).map(|cl| claim_json(c, cl)).collect::<Vec<_>>(),
        "next_claim_bond": c.state.claim_bond(addr).to_string(),
        "next_claim_bond_display": fmt_fang(c.state.claim_bond(addr)),
        "max_open_claims": c.state.max_open_claims(addr, h),
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
    let c = n.chain.lock();
    let p = &c.state.poc;
    let t = &c.state.params.poc;
    let pool = c.state.account(&c.state.pool()).balance;
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for cl in p.claims.values() {
        *counts.entry(serde_json::to_value(cl.status).unwrap().as_str().unwrap_or_default().to_string()).or_default() += 1;
    }
    use crate::poc::*;
    Ok(Json(json!({
        "active": p.is_active(),
        "pool": { "account": label(&c, &c.state.pool()), "balance": pool.to_string(), "balance_display": fmt_fang(pool), "next_budget_display": fmt_fang((pool as u128 * SEANCE_POOL_BPS as u128 / 10_000) as u64) },
        "repos": p.repos.iter().map(|(r, w)| json!({ "repo": r, "weight": w })).collect::<Vec<_>>(),
        "open_seance": p.open_seance.and_then(|id| p.seances.get(&id)).map(|s| seance_json(&c, s, false)),
        "next_seance_at": p.next_seance_at,
        "recent_seances": p.seances.values().rev().filter(|s| s.closed).take(10).map(|s| json!({ "id": s.id, "opened_at": s.opened_at, "closed_at": s.reveal_until, "claims": s.claims.len(), "jurors": s.jurors.len(), "budget_display": fmt_fang(s.budget), "results": s.results })).collect::<Vec<_>>(),
        "claims": counts,
        "jurors": { "pool": p.jurors.len(), "active": p.jurors.values().filter(|r| r.active).count() },
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
async fn claims(State(n): State<Node>, Query(q): Query<ClaimQuery>) -> ApiResult {
    let c = n.chain.lock();
    let who = q.account.as_deref().map(|a| c.state.resolve(a)).transpose()?;
    let list: Vec<Value> = c
        .state
        .poc
        .claims
        .values()
        .rev()
        .filter(|cl| who.as_ref().is_none_or(|w| &cl.claimant == w))
        .filter(|cl| q.status.as_deref().is_none_or(|s| serde_json::to_value(cl.status).unwrap() == s))
        .take(q.limit.unwrap_or(100).min(500))
        .map(|cl| claim_json(&c, cl))
        .collect();
    Ok(Json(json!({ "count": c.state.poc.claims.len(), "claims": list })))
}

async fn claim(State(n): State<Node>, Path(id): Path<u64>) -> ApiResult {
    let c = n.chain.lock();
    let cl = c.state.poc.claims.get(&id).ok_or_else(|| not_found("claim"))?;
    Ok(Json(claim_json(&c, cl)))
}

/// A Séance with every juror's assigned pairs (what they must judge), commitments and reveals.
async fn seance(State(n): State<Node>, Path(id): Path<u64>) -> ApiResult {
    let c = n.chain.lock();
    let s = c.state.poc.seances.get(&id).ok_or_else(|| not_found("Séance"))?;
    Ok(Json(seance_json(&c, s, true)))
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
}

/// Soul leaderboard (effective Soul, after decay).
async fn souls(State(n): State<Node>, Query(q): Query<LimitQuery>) -> ApiResult {
    let c = n.chain.lock();
    let h = c.height();
    let mut list: Vec<(u64, &String)> = c.state.poc.souls.keys().map(|a| (c.state.soul_of(a, h), a)).filter(|(s, _)| *s > 0).collect();
    list.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    let rows: Vec<Value> = list
        .into_iter()
        .take(q.limit.unwrap_or(50).min(500))
        .map(|(_, a)| json!({ "account": label(&c, a), "soul": soul_json(&c, a), "github": c.state.github_by_account.get(a), "juror": c.state.poc.jurors.get(a).is_some_and(|r| r.active), "vouched_by": c.state.poc.vouches.get(a).map(|v| label(&c, &v.voucher)) }))
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
