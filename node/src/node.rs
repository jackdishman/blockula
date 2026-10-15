//! Process wiring: validators (tokio runtime + authenticated discovery p2p + consensus) and
//! full nodes (`join`: verified sync over HTTP from validators).

use crate::api::{self, Node};
use crate::chain::{Chain, Genesis};
use crate::checkpoints::{Pins, Trust};
use crate::consensus::{self, CBlock, Shared, ValidatorConfig, CHANNELS, MAX_MESSAGE_SIZE};
use crate::pq::{MlDsaScheme, PrivateKey};
use crate::protection::Protection;
use crate::upgrade::Halt;
use anyhow::{anyhow, bail, Context, Result};
use commonware_codec::{Decode, DecodeExt};
use commonware_consensus::simplex::types::Finalization;
use commonware_cryptography::{Digestible, blake3::Digest, certificate::Verifier as _};
use commonware_p2p::authenticated::discovery;
use commonware_runtime::{Runner as _, Supervisor as _, tokio as cw_tokio};
use commonware_utils::NZUsize;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

async fn serve_http(node: Node, bind: SocketAddr) {
    let home = node.chain.lock().home.clone();
    let listener = match tokio::net::TcpListener::bind(bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("❌ couldn't bind http {bind}: {e}");
            std::process::exit(1);
        }
    };
    let port = bind.port();
    println!("🦇 Explorer:  http://localhost:{port}\n🔌 API:       http://localhost:{port}/api/status   (bound to {bind})\n📁 Data:      {}", home.display());
    if let Err(e) = axum::serve(listener, api::router(node).into_make_service_with_connect_info::<SocketAddr>()).await {
        eprintln!("http server failed: {e}");
    }
}

fn port_of(addr: &str) -> Option<u16> {
    addr.rsplit(':').next()?.parse().ok()
}

/// Where and how a node listens. `bind` is the local interface (127.0.0.1, 0.0.0.0, ::);
/// the advertised addresses (genesis or node.json) are what peers dial.
#[derive(Clone, Debug)]
pub struct NetOpts {
    pub bind: String,
    pub http_port: Option<u16>,
    /// `local` allows private/loopback peer addresses; `recommended` is for public networks.
    /// Default: `local` when our advertised p2p host is private, else `recommended`.
    pub p2p_preset: Option<String>,
    /// Single-validator dev chains listen for p2p on an OS-picked port.
    pub ephemeral_p2p: bool,
    /// How HTTP rate limits identify callers (proxy headers, trusted forwarders).
    pub client_ip: crate::api::ClientIpPolicy,
    /// `--peer-http`: replaces the genesis http_addr list for sync and forwarding.
    pub peer_http: Vec<String>,
    /// `--i-know-this-key-is-not-running-elsewhere`: skip the double-signing guards.
    pub force_key: bool,
    /// `--full`: replay from genesis instead of fast-syncing from a certified snapshot.
    pub full: bool,
    /// `--prune-blocks` / `--archive`.
    pub pruning: Pruning,
    pub http: HttpOpts,
    /// `--halt-height`: execute this height, then stop (upgrade.rs).
    pub halt_height: Option<u64>,
}

/// How many blocks a node keeps on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pruning {
    /// Validators keep `chain::default_keep_blocks` (about two days); full nodes keep all.
    #[default]
    Default,
    /// Keep the last N blocks (at least `chain::min_keep_blocks`).
    Keep(u64),
    /// Keep everything.
    Archive,
}

impl Pruning {
    /// Blocks to keep (`None`: all) for a node of this role.
    pub fn keep(self, validator: bool, state: &crate::chain::State) -> Option<u64> {
        match self {
            Pruning::Default if validator => Some(crate::chain::default_keep_blocks(state)),
            Pruning::Default | Pruning::Archive => None,
            Pruning::Keep(n) => Some(n.max(crate::chain::min_keep_blocks(state))),
        }
    }
}

/// HTTP limits: per-client reads (token bucket) and the request timeout.
#[derive(Clone, Debug)]
pub struct HttpOpts {
    pub read_rps: u32,
    pub read_burst: u32,
    pub timeout: Duration,
}

impl Default for HttpOpts {
    fn default() -> Self {
        Self { read_rps: api::READ_RPS, read_burst: api::READ_BURST, timeout: api::HTTP_TIMEOUT }
    }
}

/// The HTTP API for `chain`, with this node's limits and trust.
fn http_node(chain: Shared, marshal: Option<consensus::MarshalMailbox>, peers: Vec<String>, net: &NetOpts, trust: &Trust) -> Node {
    let mut node = Node::new(chain, marshal, peers, net.client_ip.clone()).with_read_limits(net.http.read_rps, net.http.read_burst);
    node.timeout = net.http.timeout;
    node.checkpoint_height = trust.newest();
    node
}

/// A validator stops the process on a halt (upgrade.rs): the reason, then the halt's exit code
/// (`RestartPreventExitStatus` keeps systemd from restarting it). Everything it executed is
/// already durable: blocks are committed before consensus hears about them.
fn exit_halted(halt: &Halt) -> ! {
    use std::io::Write as _;
    eprintln!("🛑 {halt}");
    eprintln!("   exit code {} ({}): see docs/OPERATIONS.md, \"Upgrades\" and \"Incident response\"", halt.exit_code(), halt.kind());
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(halt.exit_code())
}

/// Watches a validator for a reason to stop: a halt recorded by execution (--halt-height, an
/// unsupported version) or another instance signing with its key (slashing protection).
async fn watch_halts(chain: Shared, protection: Arc<Protection>) {
    let signal = chain.lock().halt.signal.clone();
    loop {
        if let Some(reason) = protection.halted() {
            signal.set(Halt::DoubleSignRisk { reason });
        }
        if let Some(halt) = signal.get() {
            // The block that halted is stored; wait for the executor to let go of the chain.
            drop(chain.lock());
            exit_halted(&halt);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Deletes the simplex journals of epochs before `epoch - JOURNAL_EPOCHS_KEPT` from a consensus
/// storage directory (OPS-6). The running node deletes them as epochs pass; this catches the
/// ones left from before a restart. Returns how many it deleted.
pub fn prune_epoch_journals(dir: &Path, prefix: &str, epoch: u64) -> Result<usize> {
    let keep_from = epoch.saturating_sub(consensus::JOURNAL_EPOCHS_KEPT);
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(0) };
    let mut deleted = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let old = name.to_str().and_then(|n| n.strip_prefix(prefix)).and_then(|n| n.strip_prefix("-simplex-")).and_then(|n| n.parse::<u64>().ok());
        if old.is_some_and(|e| e < keep_from) && entry.file_type()?.is_dir() && consensus::simplex_partition(prefix, old.unwrap_or_default()) == name.to_string_lossy() {
            std::fs::remove_dir_all(entry.path())?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

/// Pulls peers' validator-set handoff signatures every 30s (and signs our own, if we're a
/// validator), so this node can serve complete proofs to fast-syncing joiners.
async fn exchange_epochs(chain: Shared, peers: Vec<String>) {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().unwrap();
    chain.lock().sign_handoffs();
    loop {
        for peer in &peers {
            if let Ok(body) = get_capped(&client, &format!("{peer}/api/epochs"), 16 << 20).await {
                if let Ok(reg) = serde_json::from_slice::<crate::epochs::EpochRegistry>(&body) {
                    chain.lock().merge_epochs(&reg);
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

/// Validators check pending contribution claims against GitHub (the PR and the claimant's gist)
/// and attest what they find. A quorum of attestations settles a claim on-chain.
async fn attest_claims(chain: Shared) {
    use crate::github::Verdict;
    use crate::types::Action;
    use std::time::Instant;
    let mut tick = tokio::time::interval(Duration::from_secs(20));
    let mut retry_after: std::collections::HashMap<u64, Instant> = Default::default();
    loop {
        tick.tick().await;
        let todo: Vec<(u64, String, String, u64, String, Option<String>)> = {
            let c = chain.lock();
            let (Some(me), Some(keys)) = (c.me(), c.keys.as_ref()) else { return };
            let epoch = c.state.params.epoch_of(c.height() + 1);
            if !c.state.active_sets.get(&epoch).is_some_and(|s| s.contains(&me)) || c.syncing() {
                continue;
            }
            // Ours whichever account pays for them (SF-13): signed by our consensus key.
            let my_key = hex::encode(keys.public_key());
            let queued: std::collections::BTreeSet<u64> = c.mempool.iter().filter(|t| t.public_key == my_key).filter_map(|t| if let Action::AttestClaim { claim_id, .. } = t.action { Some(claim_id) } else { None }).collect();
            c.state
                .poc
                .claims
                .values()
                .filter(|cl| cl.status == crate::poc::ClaimStatus::Pending && !cl.attestations.contains_key(&me) && !queued.contains(&cl.id))
                .filter(|cl| retry_after.get(&cl.id).is_none_or(|t| Instant::now() >= *t))
                .take(4)
                .map(|cl| {
                    let gist = c.state.github.get(&cl.handle.to_ascii_lowercase()).filter(|l| l.account == cl.claimant).map(|l| l.gist_id.clone());
                    (cl.id, cl.handle.clone(), cl.repo.clone(), cl.pr, cl.claimant.clone(), gist)
                })
                .collect()
        };
        for (id, handle, repo, pr, account, gist) in todo {
            let verdict = match gist {
                None => Ok(Verdict::Refuted(format!("@{handle} is no longer linked to the claimant"))),
                Some(g) => crate::github::check_claim(&handle, &repo, pr, &account, &g).await,
            };
            let (action, memo) = match verdict {
                Ok(Verdict::Confirmed { merge_sha }) => (Action::AttestClaim { claim_id: id, ok: true, merge_sha: merge_sha.clone(), note: String::new() }, format!("🔎 {repo}#{pr} merged as {}", &merge_sha[..12])),
                Ok(Verdict::Refuted(why)) => {
                    let mut note = String::new();
                    for ch in why.chars().filter(|c| !c.is_control()) {
                        if note.len() + ch.len_utf8() > crate::types::MAX_REASON {
                            break;
                        }
                        note.push(ch);
                    }
                    (Action::AttestClaim { claim_id: id, ok: false, merge_sha: String::new(), note: note.clone() }, format!("🔎 {note}"))
                }
                Ok(Verdict::Wait) => {
                    retry_after.insert(id, Instant::now() + Duration::from_secs(600));
                    continue;
                }
                Err(e) => {
                    tracing::warn!(claim = id, error = %e, "couldn't check a claim on GitHub; retrying later");
                    retry_after.insert(id, Instant::now() + Duration::from_secs(300));
                    continue;
                }
            };
            match chain.lock().submit_system(action, &memo) {
                Ok(_) => tracing::info!(claim = id, %memo, "attested contribution claim"),
                Err(e) => tracing::warn!(claim = id, error = %e, "couldn't submit an attestation"),
            }
        }
    }
}

/// Submits the equivocation evidence consensus verified (`consensus::EvidenceSink`) as
/// `ReportEquivocation` txs, signed by this validator's consensus key and paid as SF-13 says
/// (`Chain::submit_system`), until the chain has punished each fault or never will (too old,
/// invalid). A fault another validator already reported (in our mempool) isn't reported twice.
/// Simplex doesn't persist the votes it compared, so pending evidence waits in `evidence.json`
/// across restarts. Runs on the consensus runtime (the deterministic one in the simulator).
pub async fn report_evidence<E: commonware_runtime::Clock>(context: E, chain: Shared, mut found: futures::channel::mpsc::UnboundedReceiver<Vec<u8>>) {
    use crate::staking::{decode_evidence, evidence_status, EvidenceStatus};
    use crate::types::Action;
    use futures::StreamExt as _;
    let path = chain.lock().home.join("evidence.json");
    // Evidence (hex) → the id of the tx we submitted for it, if any.
    let mut pending: std::collections::BTreeMap<String, Option<String>> = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok()).unwrap_or_default().into_iter().map(|e| (e, None)).collect();
    let save = |pending: &std::collections::BTreeMap<String, Option<String>>| {
        let list: Vec<&String> = pending.keys().collect();
        if let Err(e) = serde_json::to_vec(&list).map_err(anyhow::Error::from).and_then(|b| crate::store::write_durably(&path, &b)) {
            tracing::warn!(error = %e, "couldn't save evidence.json");
        }
    };
    loop {
        commonware_macros::select! {
            e = found.next() => {
                let Some(e) = e else { return };
                let e = hex::encode(e);
                if !pending.contains_key(&e) {
                    pending.insert(e, None);
                    save(&pending);
                }
            },
            _ = context.sleep(Duration::from_secs(2)) => {},
        }
        let before = pending.len();
        let mut c = chain.lock();
        if c.syncing() {
            continue;
        }
        let (height, chain_id) = (c.height() + 1, c.genesis.chain_id.clone());
        let fault_of = |evidence: &str| hex::decode(evidence).ok().and_then(|b| decode_evidence(&b).ok()).map(|e| (e.epoch, e.view, e.signer));
        pending.retain(|evidence, submitted| {
            let Ok(bytes) = hex::decode(evidence) else { return false };
            match evidence_status(&c.state, &chain_id, &bytes, height) {
                EvidenceStatus::Punished | EvidenceStatus::Never => false,
                EvidenceStatus::NotYet => true,
                EvidenceStatus::Open(fault) => {
                    let mine = fault_of(evidence);
                    let reported = submitted.as_ref().is_some_and(|id| c.in_mempool(id)) || c.mempool.iter().any(|t| matches!(&t.action, Action::ReportEquivocation { evidence: other } if fault_of(other) == mine));
                    if !reported {
                        match c.submit_system(Action::ReportEquivocation { evidence: evidence.clone() }, "🧛 double-signing evidence") {
                            Ok(tx) => {
                                tracing::warn!(validator = %fault.validator, epoch = fault.epoch, view = fault.view, "reported a validator for double-signing");
                                *submitted = Some(tx.id());
                            }
                            Err(e) => tracing::warn!(error = %e, "couldn't submit equivocation evidence; retrying"),
                        }
                    }
                    true
                }
            }
        });
        drop(c);
        if pending.len() != before {
            save(&pending);
        }
    }
}

/// Validators take part in the randomness beacon every block (`beacon::duties`): reveal the
/// secret committed to for the next epoch, commit to a fresh one for the epoch after. Secrets
/// live in `beacon-secrets.json` (0600) under the home, written before a commitment is sent, so
/// a restart can still reveal.
async fn beacon_duties(chain: Shared, home: PathBuf, block_time: Duration) {
    let mut secrets = match crate::beacon::Secrets::open(&home) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "can't read the beacon secrets; this validator won't take part in the beacon");
            return;
        }
    };
    let mut tick = tokio::time::interval(block_time.max(Duration::from_millis(500)));
    let mut last_error = String::new();
    loop {
        tick.tick().await;
        let mut c = chain.lock();
        if c.syncing() {
            continue;
        }
        match crate::beacon::duties(&mut c, &mut secrets) {
            Ok(done) => {
                for d in done {
                    tracing::info!("🎲 {d}");
                }
                last_error.clear();
            }
            // Once per distinct problem, not every block.
            Err(e) if e.to_string() != last_error => {
                tracing::warn!(error = %e, "beacon duty failed");
                last_error = e.to_string();
            }
            Err(_) => {}
        }
    }
}

/// How often a validator re-checks a claim in its revert window, and retries after GitHub errors.
const REVERT_RECHECK: Duration = Duration::from_secs(3600);
const REVERT_RETRY: Duration = Duration::from_secs(600);

/// Validators re-check verified claims during their revert window (`beacon::revert_window_open`)
/// for a revert on GitHub, and attest it (`AttestRevert`). A quorum on the same revert commit
/// rejects the claim.
async fn attest_reverts(chain: Shared) {
    use crate::types::Action;
    use std::time::Instant;
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    let mut next_check: std::collections::HashMap<u64, Instant> = Default::default();
    loop {
        tick.tick().await;
        let todo: Vec<(u64, String, u64, String)> = {
            let c = chain.lock();
            let (Some(me), Some(keys)) = (c.me(), c.keys.as_ref()) else { return };
            let next = c.height() + 1;
            let epoch = c.state.params.epoch_of(next);
            if c.syncing() || crate::staking::validator_for_signer(&c.state, &me, &crate::types::key_id(&keys.public_key()), epoch).is_none() {
                continue;
            }
            let pk = hex::encode(keys.public_key());
            let queued: std::collections::BTreeSet<u64> = c.mempool.iter().filter(|t| t.public_key == pk).filter_map(|t| if let Action::AttestRevert { claim_id, .. } = t.action { Some(claim_id) } else { None }).collect();
            let st = &c.state;
            next_check.retain(|id, _| st.poc.claims.get(id).is_some_and(|cl| crate::beacon::revert_window_open(st, cl, next)));
            st.poc
                .claims
                .values()
                .filter(|cl| crate::beacon::revert_window_open(st, cl, next) && !queued.contains(&cl.id))
                .filter(|cl| !st.beacon.reverts.get(&cl.id).is_some_and(|r| r.contains_key(&me)))
                .filter(|cl| next_check.get(&cl.id).is_none_or(|t| Instant::now() >= *t))
                .filter_map(|cl| Some((cl.id, cl.repo.clone(), cl.pr, cl.merge_sha.clone()?)))
                .take(4)
                .collect()
        };
        for (id, repo, pr, merge_sha) in todo {
            match crate::github::find_revert(&repo, pr, &merge_sha).await {
                Ok(Some(r)) => {
                    let memo = format!("↩️ {repo}#{pr}: {}", r.how);
                    match chain.lock().submit_system(Action::AttestRevert { claim_id: id, revert_sha: r.sha }, &memo) {
                        Ok(_) => tracing::info!(claim = id, %memo, "attested a revert"),
                        Err(e) => tracing::warn!(claim = id, error = %e, "couldn't submit a revert attestation"),
                    }
                    next_check.insert(id, Instant::now() + REVERT_RECHECK);
                }
                Ok(None) => {
                    next_check.insert(id, Instant::now() + REVERT_RECHECK);
                }
                Err(e) => {
                    tracing::warn!(claim = id, error = %e, "couldn't check a claim for reverts on GitHub; retrying later");
                    next_check.insert(id, Instant::now() + REVERT_RETRY);
                }
            }
        }
    }
}

/// How often (every 10 s) fast sync waits for a snapshot newer than a trusted checkpoint
/// before it falls back to syncing from genesis.
const SNAPSHOT_WAITS: usize = 30;

async fn maybe_fast_sync(chain: &Shared, peers: &[String], full: bool, trust: &Trust) {
    let empty = {
        let c = chain.lock();
        c.height() == 0 && c.base.is_none()
    };
    if !empty || full || peers.is_empty() {
        return;
    }
    for wait in 0.. {
        match crate::snapshot::fast_sync(chain, peers, trust).await {
            Ok(h) => return println!("⚡ fast-synced to #{h} from a certified snapshot; history before it isn't stored on this node"),
            // A checkpoint newer than every peer's snapshot: one is taken every 100 blocks.
            Err(e) if wait < SNAPSHOT_WAITS && e.chain().any(|c| c.is::<crate::snapshot::SnapshotBehind>()) => {
                println!("⏳ {e:#}; waiting for a newer snapshot");
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            Err(e) => return println!("ℹ️  no usable snapshot ({e:#}); syncing from genesis instead"),
        }
    }
}

/// The HTTP peers a node syncs from and forwards to: `--peer-http` if given, otherwise the
/// genesis validators' http addresses (minus our own).
pub fn http_peers(genesis: &Genesis, override_: &[String], me: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let list: Vec<String> = if override_.is_empty() {
        genesis.validators.iter().filter(|v| Some(v.address.as_str()) != me).map(|v| crate::chain::http_url(&v.http_addr)).collect()
    } else {
        override_.iter().map(|u| crate::chain::http_url(u)).collect()
    };
    for u in list {
        if !out.contains(&u) {
            out.push(u);
        }
    }
    out
}

pub fn socket(bind: &str, port: u16) -> Result<SocketAddr> {
    let host = bind.trim_start_matches('[').trim_end_matches(']');
    let ip: std::net::IpAddr = host.parse().with_context(|| format!("--bind must be an IP address, got {bind}"))?;
    Ok(SocketAddr::new(ip, port))
}

/// Parses an advertised `host:port` (IP or DNS name) into a dialable ingress.
pub fn ingress(addr: &str) -> Result<commonware_p2p::Ingress> {
    if let Ok(sa) = addr.parse::<SocketAddr>() {
        return Ok(sa.into());
    }
    let (host, port) = addr.rsplit_once(':').ok_or_else(|| anyhow!("expected host:port, got {addr}"))?;
    let host = commonware_utils::Hostname::new(host.to_string()).map_err(|e| anyhow!("bad hostname {host}: {e:?}"))?;
    Ok(commonware_p2p::Ingress::Dns { host, port: port.parse()? })
}

fn is_private_host(addr: &str) -> bool {
    match addr.parse::<SocketAddr>() {
        Ok(sa) => match sa.ip() {
            std::net::IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified(),
            std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.is_unspecified() || (ip.segments()[0] & 0xfe00) == 0xfc00,
        },
        Err(_) => addr.starts_with("localhost:"),
    }
}

/// A validator's own advertised addresses: from genesis, or node.json for validators that
/// joined later (`blockula validator init`).
#[derive(Serialize, Deserialize, Clone)]
pub struct NodeAddrs {
    pub p2p_addr: String,
    pub http_addr: String,
}

/// Refuses to start a validator that may already be signing somewhere else: a key with a
/// chain history but no consensus journal (restored/copied home), or a home last used by a
/// different machine (`machine.lock` vs `BLOCKULA_INSTANCE_ID` / `FLY_MACHINE_ID`).
pub fn double_sign_guard(home: &Path, height: u64, force: bool) -> Result<()> {
    let instance = std::env::var("BLOCKULA_INSTANCE_ID").ok().or_else(|| std::env::var("FLY_MACHINE_ID").ok()).filter(|s| !s.is_empty());
    double_sign_guard_with(home, height, force, instance)
}

pub fn double_sign_guard_with(home: &Path, height: u64, force: bool, instance: Option<String>) -> Result<()> {
    let journal = home.join("consensus");
    let journal_empty = std::fs::read_dir(&journal).map(|mut d| d.next().is_none()).unwrap_or(true);
    if height > 0 && journal_empty && !force {
        bail!(
            "refusing to start: this home has {height} executed blocks but no consensus journal ({}). If this validator.key is running anywhere else, starting here could double-sign. Pass --i-know-this-key-is-not-running-elsewhere once you're sure.",
            journal.display()
        );
    }
    if let Some(id) = instance {
        let lock = home.join("machine.lock");
        match std::fs::read_to_string(&lock) {
            Ok(prev) if prev.trim() != id && !force => bail!(
                "refusing to start: {} says this validator home belongs to instance `{}`, but this is `{id}`. Running one key on two machines double-signs. Pass --i-know-this-key-is-not-running-elsewhere after stopping the other one.",
                lock.display(),
                prev.trim()
            ),
            Ok(prev) if prev.trim() == id => {}
            _ => std::fs::write(&lock, &id)?,
        }
    }
    Ok(())
}

/// Consensus storage written by an older layout can't be read; say so instead of crashing.
pub fn check_storage_version(home: &Path) -> Result<()> {
    let marker = home.join("consensus.version");
    let dir = home.join("consensus");
    let nonempty = std::fs::read_dir(&dir).map(|mut d| d.next().is_some()).unwrap_or(false);
    match std::fs::read_to_string(&marker) {
        Ok(v) if v.trim() == consensus::STORAGE_VERSION => Ok(()),
        _ if nonempty => bail!(
            "{} was written by an older Blockula. Re-summon the chain, or (single validator) delete it and start with --i-know-this-key-is-not-running-elsewhere to rebuild it.",
            dir.display()
        ),
        _ => Ok(std::fs::write(&marker, consensus::STORAGE_VERSION)?),
    }
}

/// Dev chains made by an older Blockula are reset automatically.
pub fn dev_is_stale(home: &Path) -> bool {
    let nonempty = std::fs::read_dir(home.join("consensus")).map(|mut d| d.next().is_some()).unwrap_or(false);
    nonempty && std::fs::read_to_string(home.join("consensus.version")).map(|v| v.trim() != consensus::STORAGE_VERSION).unwrap_or(true)
}

pub fn run_validator(home: PathBuf, net: NetOpts) -> Result<()> {
    let mut chain = Chain::open(&home)?;
    // Validators take their genesis from the signed release; a known chain's must match it.
    let trust = Arc::new(Trust::new(&chain.genesis, &Pins::default(), false)?);
    let keep = net.pruning.keep(true, &chain.state);
    chain.set_pruning(keep);
    crate::upgrade::configure(&mut chain, consensus::SUPPORTED_PROTOCOL_VERSION, net.halt_height);
    if let Some(halt) = chain.halt.signal.get() {
        exit_halted(&halt);
    }
    double_sign_guard(&home, chain.height(), net.force_key)?;
    check_storage_version(&home)?;
    let genesis = chain.genesis.clone();
    let seed: [u8; 32] = hex::decode(std::fs::read_to_string(home.join("validator.key"))?.trim())?.try_into().map_err(|_| anyhow!("bad validator.key"))?;
    let key = PrivateKey::from_seed_bytes(seed);
    // Slashing protection: the record next to the key, and the restart rule (protection.rs).
    let protection = Protection::open(&home.join(crate::protection::RECORD_FILE), commonware_cryptography::Signer::public_key(&key).as_ref(), crate::protection::Timing::for_block_time(genesis.block_time_ms), true)?;
    let me = commonware_cryptography::Signer::public_key(&key).address(&genesis.address_prefix);
    let addrs = match genesis.validators.iter().find(|v| v.address == me) {
        Some(v) => NodeAddrs { p2p_addr: v.p2p_addr.clone(), http_addr: v.http_addr.clone() },
        None => serde_json::from_slice(&std::fs::read(home.join("node.json")).context("validator.key isn't a genesis validator and node.json is missing — use `blockula validator init`")?)?,
    };
    let http_port = net.http_port.or_else(|| port_of(&addrs.http_addr).filter(|_| !addrs.http_addr.starts_with("http"))).unwrap_or(7070);
    let p2p_port = if net.ephemeral_p2p { 0 } else { port_of(&addrs.p2p_addr).ok_or_else(|| anyhow!("bad p2p_addr {}", addrs.p2p_addr))? };
    let listen = socket(&net.bind, p2p_port)?;
    let http_bind = socket(&net.bind, http_port)?;
    let dialable = if net.ephemeral_p2p { listen.into() } else { ingress(&addrs.p2p_addr)? };
    let peers = if net.ephemeral_p2p { vec![] } else { http_peers(&genesis, &net.peer_http, Some(&me)) };

    // Bootstrap from every validator we know of (genesis + on-chain registry).
    let mut seen = std::collections::BTreeSet::new();
    let mut bootstrappers = vec![];
    let known = genesis.validators.iter().map(|v| (v.address.clone(), v.public_key.clone(), v.p2p_addr.clone()))
        .chain(chain.state.validators.values().filter(|v| v.status == crate::chain::ValidatorStatus::Active).map(|v| (v.address.clone(), v.consensus_key.clone(), v.p2p_addr.clone())));
    for (addr, pk, p2p) in known {
        if addr == me || !seen.insert(addr) {
            continue;
        }
        let pk = crate::pq::PublicKey::from_bytes(&hex::decode(&pk)?).ok_or_else(|| anyhow!("bad key"))?;
        match ingress(&p2p) {
            Ok(i) => bootstrappers.push((pk, i)),
            Err(e) => eprintln!("⚠️  skipping bootstrapper {p2p}: {e}"),
        }
    }
    let preset = match net.p2p_preset.as_deref() {
        Some(p @ ("local" | "recommended" | "public-nat")) => p.to_string(),
        Some(other) => bail!("--p2p-preset must be local, recommended or public-nat, got {other}"),
        None if net.ephemeral_p2p || is_private_host(&addrs.p2p_addr) => "local".into(),
        None => "recommended".into(),
    };
    if let Some(why) = trust.stale(chain.height(), crate::chain::now_ms()).filter(|_| !peers.is_empty()) {
        eprintln!("⚠️  {why}. Validators get recent checkpoints with each release: run the latest one.");
    }
    let shared: Shared = Arc::new(Mutex::new(chain));
    // Far behind (new validator, long downtime)? Catch up over HTTP first: verified batches,
    // much faster than consensus backfill, and peers prune old consensus data.
    if !peers.is_empty() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        rt.block_on(async {
            maybe_fast_sync(&shared, &peers, net.full, &trust).await;
            catch_up(shared.clone(), peers.clone(), trust.clone()).await;
        });
    }
    if let Some(halt) = shared.lock().halt.signal.get() {
        exit_halted(&halt);
    }
    {
        let chain = shared.lock();
        let epoch = chain.state.params.epoch_of(chain.height() + 1);
        match prune_epoch_journals(&home.join("consensus"), "blockula", epoch) {
            Ok(0) => {}
            Ok(n) => println!("🧹 deleted {n} old epochs' consensus journals"),
            Err(e) => eprintln!("⚠️  couldn't prune old consensus journals: {e:#}"),
        }
        let active = chain.state.active_sets.get(&epoch).map(|s| s.len()).unwrap_or(0);
        let voting = chain.state.active_sets.get(&epoch).is_some_and(|s| s.contains(&me));
        println!(
            "🧛 Blockula `{}` rising from height {} — epoch {epoch}, {active} active validator{}; this node {} (simplex BFT, ML-DSA-65)",
            genesis.chain_id,
            chain.height(),
            if active == 1 { "" } else { "s" },
            if voting { "votes" } else { "observes until it's in the active set" }
        );
    }
    if net.ephemeral_p2p {
        println!("🔗 p2p: single-validator dev chain, listening on {listen}");
    } else {
        println!("🔗 p2p: listening on {listen}, advertised as {} ({preset} preset)", addrs.p2p_addr);
    }
    consensus::log_memory_budget(bootstrappers.len());
    match keep {
        Some(k) => println!("🗄️  keeping the last {k} blocks on disk (--archive keeps all)"),
        None => println!("🗄️  archive node: keeping every block"),
    }
    let block_time_ms = genesis.block_time_ms;
    let genesis_block = consensus::genesis_block(&genesis);
    let chain_id = genesis.chain_id.clone();

    let cfg = cw_tokio::Config::new().with_storage_directory(home.join("consensus"));
    let halt = shared.lock().halt.signal.clone();
    let watched = protection.clone();
    let stopped = cw_tokio::Runner::new(cfg).start(|context| async move {
        let ns = [consensus::namespace(&chain_id).as_slice(), b"/p2p"].concat();
        let max_peers = NonZeroUsize::new(consensus::MAX_PEERS_PER_SET).unwrap();
        let mut p2p_cfg = if preset == "local" {
            discovery::Config::local(key.clone(), &ns, listen, dialable, bootstrappers, max_peers, MAX_MESSAGE_SIZE)
        } else {
            discovery::Config::recommended(key.clone(), &ns, listen, dialable, bootstrappers, max_peers, MAX_MESSAGE_SIZE)
        };
        if preset == "public-nat" {
            // Recommended limits, but inbound connections arrive from a NAT/proxy's private address.
            p2p_cfg.allow_private_ips = true;
        }
        p2p_cfg.mailbox_size = NZUsize!(4096);
        p2p_cfg.tracked_peer_sets = NonZeroUsize::new(consensus::TRACKED_PEER_SETS).unwrap();
        let (mut network, oracle) = discovery::Network::new(context.child("network"), p2p_cfg);
        let channels: Vec<_> = CHANNELS.iter().map(|c| network.register(*c, consensus::quota(*c))).collect();
        network.start();

        let vcfg = ValidatorConfig { chain: shared.clone(), key, genesis: genesis_block, block_time_ms, epoch_blocks: genesis.epoch_blocks, partition: "blockula".into(), protection: Some(protection.clone()) };
        let (handle, marshal) = consensus::start_validator(context.child("node"), vcfg, oracle.clone(), oracle.clone(), channels).await;
        tokio::spawn(exchange_epochs(shared.clone(), peers.clone()));
        tokio::spawn(attest_claims(shared.clone()));
        tokio::spawn(beacon_duties(shared.clone(), home.clone(), Duration::from_millis(block_time_ms)));
        tokio::spawn(attest_reverts(shared.clone()));
        tokio::spawn(watch_halts(shared.clone(), protection.clone()));
        let mut node = http_node(shared, Some(marshal), peers, &net, &trust);
        node.protection = Some(protection);
        let metrics = context.child("metrics");
        node.p2p_peers = Some(Arc::new(move || api::connected_peers(&commonware_runtime::Metrics::encode(&metrics))));
        tokio::spawn(serve_http(node, http_bind));
        handle.await
    });
    // A halt stops consensus too (the halting block is never acknowledged): its exit code wins.
    if let Some(reason) = watched.halted() {
        halt.set(Halt::DoubleSignRisk { reason });
    }
    if let Some(h) = halt.get() {
        exit_halted(&h);
    }
    match stopped {
        Ok(()) => bail!("consensus stopped (a finalized block failed to execute? see the log above)"),
        Err(e) => bail!("consensus stopped: {e:?}"),
    }
}

#[derive(Serialize, Deserialize)]
struct JoinConfig {
    peers: Vec<String>,
    /// Start from a certified snapshot (default) instead of replaying from genesis.
    #[serde(default = "yes")]
    fast: bool,
    /// What the operator pinned (`--genesis-hash`, `--checkpoint`, `--allow-stale-checkpoint`),
    /// enforced on every start.
    #[serde(default, flatten)]
    pins: Pins,
}

fn yes() -> bool {
    true
}

/// Prepares a full-node home: genesis.json (copied or fetched from a node) + join.json.
/// The genesis must be pinned: by `pins.genesis_hash`, or by this binary for a known chain id
/// (`checkpoints.rs`). Checkpoints must agree with it, and the newest root must be younger than
/// the weak-subjectivity period unless `pins.allow_stale_checkpoint`.
pub fn join(genesis: &str, home: &Path, pins: &Pins, fast: bool) -> Result<()> {
    if home.join("validator.key").exists() {
        bail!("{} is a validator home", home.display());
    }
    let (g, mut peers): (Genesis, Vec<String>) = if genesis.starts_with("http://") || genesis.starts_with("https://") {
        let url = genesis.trim_end_matches('/').to_string();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let g: Genesis = rt.block_on(async { reqwest::get(format!("{url}/api/genesis")).await?.error_for_status()?.json().await })?;
        (g, vec![url])
    } else {
        (Genesis::load(Path::new(genesis))?, vec![])
    };
    let trust = Trust::new(&g, pins, true)?;
    trust.check_fresh(0, crate::chain::now_ms())?;
    match trust.newest() {
        0 => println!("🔐 genesis {} pinned", trust.genesis_hash),
        h => println!("🔐 genesis {} pinned; newest trusted checkpoint #{h}", trust.genesis_hash),
    }
    std::fs::create_dir_all(home)?;
    for u in http_peers(&g, &[], None) {
        if !peers.contains(&u) {
            peers.push(u);
        }
    }
    let path = home.join("genesis.json");
    if path.exists() {
        let existing = Genesis::load(&path)?;
        if serde_json::to_string(&existing)? != serde_json::to_string(&g)? {
            bail!("{} already holds a different chain", home.display());
        }
    } else {
        std::fs::write(&path, serde_json::to_string_pretty(&g)?)?;
    }
    std::fs::write(home.join("join.json"), serde_json::to_string_pretty(&JoinConfig { peers, fast, pins: pins.clone() })?)?;
    Ok(())
}

pub fn run_full_node(home: PathBuf, net: NetOpts) -> Result<()> {
    let bind = socket(&net.bind, net.http_port.unwrap_or(7171))?;
    let jc: JoinConfig = serde_json::from_slice(&std::fs::read(home.join("join.json")).context("not a joined home — use `blockula join`")?)?;
    let mut chain = Chain::open(&home)?;
    // The pins were checked at `join`; they (and the embedded checkpoints) apply on every start.
    let trust = Arc::new(Trust::new(&chain.genesis, &jc.pins, false)?);
    // An empty home is where a stale root bites (a fresh sync); a synced one trusts its own head.
    if let Some(why) = trust.check_fresh(chain.height(), crate::chain::now_ms())? {
        eprintln!("⚠️  {why}");
    }
    let keep = net.pruning.keep(false, &chain.state);
    chain.set_pruning(keep);
    // A halted full node keeps serving reads at its height, with the reason in /api/health.
    crate::upgrade::configure(&mut chain, consensus::SUPPORTED_PROTOCOL_VERSION, net.halt_height);
    let peers = if net.peer_http.is_empty() { jc.peers } else { http_peers(&chain.genesis, &net.peer_http, None) };
    println!("🦇 Full node for `{}` at height {} — syncing from {}", chain.genesis.chain_id, chain.height(), peers.join(", "));
    let shared: Shared = Arc::new(Mutex::new(chain));
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let fast = jc.fast && !net.full;
    rt.block_on(async move {
        // The API and explorer come up immediately, while syncing.
        tokio::spawn(serve_http(http_node(shared.clone(), None, peers.clone(), &net, &trust), bind));
        maybe_fast_sync(&shared, &peers, !fast, &trust).await;
        tokio::spawn(exchange_epochs(shared.clone(), peers.clone()));
        sync_loop(shared, peers, false, trust).await;
    });
    Ok(())
}

#[derive(Deserialize)]
pub(crate) struct SyncBatch {
    #[serde(default)]
    pub tip: u64,
    pub blocks: Vec<String>,
    pub to: Option<u64>,
    pub finalization: Option<String>,
}

/// Largest `/api/sync` response we'll read (500 blocks × 4 MiB is the protocol max, but honest
/// batches are far smaller; anything bigger is cut off and the peer rotated).
pub const MAX_SYNC_RESPONSE: usize = 32 << 20;

pub async fn get_capped(client: &reqwest::Client, url: &str, cap: usize) -> Result<Vec<u8>> {
    let mut res = client.get(url).send().await?;
    if !res.status().is_success() {
        let status = res.status();
        bail!("{status}");
    }
    if res.content_length().is_some_and(|l| l as usize > cap) {
        bail!("response too large");
    }
    let mut body = vec![];
    while let Some(chunk) = res.chunk().await? {
        body.extend_from_slice(&chunk);
        if body.len() > cap {
            bail!("response too large");
        }
    }
    Ok(body)
}

/// One sync round against `peer`. Ok(true) = applied blocks, Ok(false) = peer had nothing new.
async fn sync_once(chain: &Shared, provider: &consensus::EpochProvider, client: &reqwest::Client, peer: &str, trust: &Trust) -> Result<bool> {
    // Batches never cross an epoch boundary: each is verified with that epoch's validator set,
    // which we learn by executing the previous epoch's last block.
    let (from, limit, epoch) = {
        let c = chain.lock();
        let from = c.height() + 1;
        let epoch = c.state.params.epoch_of(from);
        (from, (c.state.params.last_of(epoch) - from + 1).min(200), epoch)
    };
    let verifier = provider.epoch_scheme(epoch).ok_or_else(|| anyhow!("no validator set known for epoch {epoch}"))?;
    let body = get_capped(client, &format!("{peer}/api/sync?from={from}&limit={limit}"), MAX_SYNC_RESPONSE).await?;
    let batch: SyncBatch = serde_json::from_slice(&body)?;
    chain.lock().note_claim(peer, batch.tip);
    if batch.blocks.is_empty() {
        if batch.tip >= from {
            bail!("peer claims height {} but sent nothing", batch.tip);
        }
        return Ok(false);
    }
    apply_batch_trusted(chain, &verifier, epoch, &batch, Some(trust))?;
    Ok(true)
}

/// Follows peers forever (full nodes). Peers rotate on errors, bad batches and empty replies,
/// so one stalling or lying peer can't hold the node back.
async fn sync_loop(chain: Shared, peers: Vec<String>, stop_when_caught_up: bool, trust: Arc<Trust>) {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();
    let provider = consensus::EpochProvider::new(chain.clone(), None);
    let block_time = Duration::from_millis(chain.lock().genesis.block_time_ms);
    if peers.is_empty() {
        eprintln!("❌ no peers to sync from (set --peer-http)");
        return;
    }
    let mut peer = 0usize;
    let mut idle_rounds = 0usize;
    let mut failures = 0usize;
    let halt = chain.lock().halt.signal.clone();
    loop {
        if let Some(h) = halt.get() {
            if stop_when_caught_up {
                return; // a validator catching up: run_validator exits with the halt's code
            }
            // A full node stops following the chain but keeps serving reads (/api/health says why).
            eprintln!("🛑 {h}\n   still serving reads at height {}; /api/health reports the halt", chain.lock().height());
            return futures::future::pending().await;
        }
        let url = &peers[peer % peers.len()];
        match sync_once(&chain, &provider, &client, url, &trust).await {
            Ok(true) => {
                idle_rounds = 0;
                failures = 0;
            }
            Ok(false) => {
                idle_rounds += 1;
                peer += 1; // ask someone else next time, so no single peer can stall us
                if stop_when_caught_up && idle_rounds >= peers.len() {
                    return;
                }
                tokio::time::sleep(block_time / peers.len().max(1) as u32).await;
            }
            Err(e) => {
                tracing::debug!(peer = %url, error = %e, "sync round failed");
                if !e.to_string().contains("sent nothing") && halt.get().is_none() {
                    eprintln!("⚠️  sync from {url}: {e:#}");
                }
                peer += 1;
                failures += 1;
                if stop_when_caught_up && failures >= peers.len() * 2 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Validators: sync over HTTP until no peer has anything newer (or none answer).
async fn catch_up(chain: Shared, peers: Vec<String>, trust: Arc<Trust>) {
    let start = chain.lock().height();
    sync_loop(chain.clone(), peers, true, trust).await;
    let end = chain.lock().height();
    if end > start {
        println!("⏩ caught up over HTTP: {start} → {end}");
    }
}

/// Verifies a batch (contiguous heights, hash-linked to our tip, quorum finalization over the
/// last block) and executes it. (The simulator and the fuzz harness; nodes check their trust.)
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn apply_batch(chain: &Shared, verifier: &MlDsaScheme, epoch: u64, batch: &SyncBatch) -> Result<()> {
    apply_batch_trusted(chain, verifier, epoch, batch, None)
}

/// [`apply_batch`], refusing a batch that contradicts `trust`: a block at a checkpoint's height
/// with another hash, or an epoch whose validator set (learned by executing up to it) isn't the
/// one a checkpoint pins. Both are checked before anything executes.
pub(crate) fn apply_batch_trusted(chain: &Shared, verifier: &MlDsaScheme, epoch: u64, batch: &SyncBatch, trust: Option<&Trust>) -> Result<()> {
    if let Some(t) = trust {
        let keys: Vec<String> = chain.lock().state.active_set(epoch).map(|s| s.iter().map(|v| v.consensus_key.clone()).collect()).unwrap_or_default();
        t.check_set(epoch, &keys)?;
    }
    let blocks: Vec<CBlock> = batch.blocks.iter().map(|h| Ok(CBlock::decode(hex::decode(h)?.as_slice())?)).collect::<Result<_>>()?;
    let fin_hex = batch.finalization.as_ref().ok_or_else(|| anyhow!("missing finalization"))?;
    let fin_bytes = hex::decode(fin_hex)?;
    let fin = Finalization::<MlDsaScheme, Digest>::decode_cfg(fin_bytes.as_slice(), &verifier.certificate_codec_config())?;
    let last = blocks.last().ok_or_else(|| anyhow!("empty batch"))?;
    if Some(last.height) != batch.to || fin.proposal.payload != last.digest() {
        bail!("finalization does not cover the batch");
    }
    if fin.proposal.round.epoch().get() != epoch {
        bail!("finalization is from epoch {}, expected {epoch}", fin.proposal.round.epoch().get());
    }
    if !fin.verify(&mut commonware_utils::sys_rng(), verifier, &commonware_parallel::Sequential) {
        bail!("invalid finalization certificate");
    }
    let (mut prev, mut height) = {
        let c = chain.lock();
        (c.last_hash().to_string(), c.height())
    };
    for b in &blocks {
        if b.height != height + 1 || hex::encode(b.parent_bytes()) != prev {
            bail!("batch does not extend our chain at height {}", b.height);
        }
        prev = hex::encode(b.digest_bytes());
        height = b.height;
        if let Some(t) = trust {
            t.check_block(height, &prev)?;
        }
    }
    // One block at a time, the chain lock held only for the in-memory parts (sync is this
    // node's only executor, so nothing else extends the chain in between).
    for b in &blocks {
        let cert = (b.height == last.height).then(|| fin_bytes.clone());
        Chain::execute_shared(chain, consensus::finalized_from(b, cert))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::SummonOpts;

    fn home(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("blockula-node-{tag}-{}-{}", crate::chain::now_ms(), std::process::id()))
    }

    #[test]
    fn double_sign_guard_refuses_copied_homes_and_other_machines() {
        let h = home("guard");
        std::fs::create_dir_all(&h).unwrap();
        // Fresh validator: nothing to protect.
        double_sign_guard_with(&h, 0, false, None).unwrap();
        // Has executed blocks but no consensus journal: a restored/copied home.
        let err = double_sign_guard_with(&h, 10, false, None).unwrap_err().to_string();
        assert!(err.contains("double-sign"), "{err}");
        double_sign_guard_with(&h, 10, true, None).unwrap();
        // machine.lock pins the home to one instance.
        std::fs::create_dir_all(h.join("consensus").join("x")).unwrap();
        double_sign_guard_with(&h, 10, false, Some("machine-a".into())).unwrap();
        double_sign_guard_with(&h, 10, false, Some("machine-a".into())).unwrap();
        assert!(double_sign_guard_with(&h, 10, false, Some("machine-b".into())).unwrap_err().to_string().contains("machine-a"));
        std::fs::remove_dir_all(h).ok();
    }

    /// OPS-6: at startup, simplex journals of epochs before the current one minus two go; the
    /// rest of the consensus storage (marshal, other prefixes, look-alikes) stays.
    #[test]
    fn old_epoch_journals_are_pruned_at_startup() {
        let dir = home("journals");
        for d in ["blockula-simplex-0", "blockula-simplex-7", "blockula-simplex-8", "blockula-simplex-9", "blockula-simplex-10", "blockula-marshal-x", "blockula-finalizations-key", "other-simplex-1", "blockula-simplex-07x"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
            std::fs::write(dir.join(d).join("blob"), b"x").unwrap();
        }
        std::fs::write(dir.join("blockula-simplex-3"), b"a file, not a partition").unwrap();
        assert_eq!(prune_epoch_journals(&dir, "blockula", 10).unwrap(), 2);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["blockula-finalizations-key", "blockula-marshal-x", "blockula-simplex-07x", "blockula-simplex-10", "blockula-simplex-3", "blockula-simplex-8", "blockula-simplex-9", "other-simplex-1"]);
        assert_eq!(prune_epoch_journals(&dir, "blockula", 10).unwrap(), 0);
        assert_eq!(prune_epoch_journals(&dir.join("missing"), "blockula", 10).unwrap(), 0);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn peer_http_replaces_genesis_list_and_urls_are_accepted() {
        let root = home("peers");
        let homes = Chain::summon(&root, &SummonOpts::new("peers", 2)).unwrap();
        let mut g = Genesis::load(&homes[0].join("genesis.json")).unwrap();
        g.validators[0].http_addr = "https://rpc.example.com".into();
        let me = g.validators[1].address.clone();
        assert_eq!(http_peers(&g, &[], Some(&me)), vec!["https://rpc.example.com".to_string()]);
        let over = vec!["http://blockula-v0.internal:7070".to_string(), "10.0.0.2:7070".to_string()];
        assert_eq!(http_peers(&g, &over, Some(&me)), vec!["http://blockula-v0.internal:7070".to_string(), "http://10.0.0.2:7070".to_string()]);
        crate::chain::check_http_addr("https://blockula-testnet.fly.dev").unwrap();
        assert!(crate::chain::check_http_addr("https://bad host").is_err());
        std::fs::remove_dir_all(root).ok();
    }

    /// CRY-2: `join` needs a pinned genesis (no more warn-only), checkpoints that agree with it,
    /// and a root younger than the weak-subjectivity period; the pins outlive the command.
    #[test]
    fn join_pins_the_genesis_and_refuses_stale_roots() {
        use crate::checkpoints::Checkpoint;
        let root = home("pin");
        let homes = Chain::summon(&root, &SummonOpts::new("pin", 1)).unwrap();
        let gpath = homes[0].join("genesis.json");
        let g = Genesis::load(&gpath).unwrap();
        let hash = hex::encode(consensus::genesis_block(&g).digest_bytes());
        let pin = |h: &str| Pins { genesis_hash: Some(h.to_string()), ..Default::default() };
        let try_join = |path: &Path, name: &str, pins: &Pins| join(path.to_str().unwrap(), &root.join(name), pins, true);
        let err = try_join(&gpath, "full-none", &Pins::default()).unwrap_err().to_string();
        assert!(err.contains("--genesis-hash") && err.contains(&hash), "{err}");
        assert!(!root.join("full-none").exists(), "a refused join writes nothing");
        assert!(try_join(&gpath, "full-bad", &pin(&"00".repeat(32))).unwrap_err().to_string().contains("mismatch"));
        let wrong_genesis = Pins { checkpoints: vec![Checkpoint { height: 0, hash: "ab".repeat(32), set_hash: None }], ..pin(&hash) };
        assert!(try_join(&gpath, "full-cp0", &wrong_genesis).unwrap_err().to_string().contains("checkpoint #0"));
        // Pinned: joined, and `run` finds the pins in join.json.
        let pins = Pins { checkpoints: vec![Checkpoint { height: 5, hash: "cd".repeat(32), set_hash: None }], ..pin(&hash) };
        try_join(&gpath, "full-ok", &pins).unwrap();
        let jc: JoinConfig = serde_json::from_slice(&std::fs::read(root.join("full-ok").join("join.json")).unwrap()).unwrap();
        assert_eq!(jc.pins, pins);
        // A month-old chain with nothing newer than its genesis: refused unless the operator says so.
        let mut old = g.clone();
        old.created_at_ms -= 30 * 86_400_000;
        let old_path = root.join("old-genesis.json");
        std::fs::write(&old_path, serde_json::to_string(&old).unwrap()).unwrap();
        let old_hash = hex::encode(consensus::genesis_block(&old).digest_bytes());
        let err = try_join(&old_path, "full-stale", &pin(&old_hash)).unwrap_err().to_string();
        assert!(err.contains("weak-subjectivity") && err.contains("--allow-stale-checkpoint"), "{err}");
        try_join(&old_path, "full-stale", &Pins { allow_stale_checkpoint: true, ..pin(&old_hash) }).unwrap();
        std::fs::remove_dir_all(root).ok();
    }

    /// Sync batches that contradict a trusted checkpoint are refused before anything executes: a
    /// block at the checkpoint's height with another hash, or another set for the epoch it pins.
    #[test]
    fn sync_batches_must_match_trusted_checkpoints() {
        use crate::checkpoints::Checkpoint;
        use commonware_codec::Encode as _;
        use commonware_consensus::simplex::types::{Finalize, Proposal};
        use commonware_consensus::types::{Epoch, Round, View};
        let root = home("cpbatch");
        let homes = Chain::summon(&root, &SummonOpts::new("cpbatch", 1)).unwrap();
        let g = Genesis::load(&homes[0].join("genesis.json")).unwrap();
        let seed: [u8; 32] = hex::decode(std::fs::read_to_string(homes[0].join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
        let full = root.join("full");
        std::fs::create_dir_all(&full).unwrap();
        std::fs::copy(homes[0].join("genesis.json"), full.join("genesis.json")).unwrap();
        let chain: Shared = Arc::new(Mutex::new(Chain::open(&full).unwrap()));
        // Blocks 1..=3 on the genesis, the last certified by the one validator.
        let root_hash = consensus::hex_32(&g.initial_state().root());
        let mut blocks = vec![consensus::genesis_block(&g)];
        for h in 1..=3u64 {
            let parent = blocks.last().unwrap().digest();
            blocks.push(CBlock::new(parent, h, h, h - 1, g.created_at_ms + 1000 * h, g.validators[0].address.clone(), 0, root_hash, vec![]));
        }
        let scheme = MlDsaScheme::signer(&consensus::namespace(&g.chain_id), consensus::participants(&g).unwrap(), PrivateKey::from_seed_bytes(seed)).unwrap();
        let last = &blocks[3];
        let vote = Finalize::sign(&scheme, Proposal::new(Round::new(Epoch::new(0), View::new(3)), View::new(2), last.digest())).unwrap();
        let fin = Finalization::from_finalizes(&scheme, commonware_utils::iter::NonEmpty::try_new([vote].iter()).unwrap(), &commonware_parallel::Sequential).unwrap();
        let batch = SyncBatch { tip: 3, blocks: blocks[1..].iter().map(|b| hex::encode(b.encode())).collect(), to: Some(3), finalization: Some(hex::encode(fin.encode())) };
        let verifier = consensus::EpochProvider::new(chain.clone(), None).epoch_scheme(0).unwrap();
        let hash2 = hex::encode(blocks[2].digest_bytes());
        let genesis_set = crate::checkpoints::genesis_checkpoint(&g).set_hash;
        let trust = |hash: &str, set: Option<String>| Trust::new(&g, &Pins { checkpoints: vec![Checkpoint { height: 2, hash: hash.into(), set_hash: set }], ..Default::default() }, false).unwrap();
        let err = apply_batch_trusted(&chain, &verifier, 0, &batch, Some(&trust(&"ab".repeat(32), None))).unwrap_err().to_string();
        assert!(err.contains("another chain"), "{err}");
        let err = apply_batch_trusted(&chain, &verifier, 0, &batch, Some(&trust(&hash2, Some("cd".repeat(32))))).unwrap_err().to_string();
        assert!(err.contains("forged validator-set history"), "{err}");
        assert_eq!(chain.lock().height(), 0, "nothing executed");
        apply_batch_trusted(&chain, &verifier, 0, &batch, Some(&trust(&hash2, genesis_set))).unwrap();
        assert_eq!(chain.lock().hash_at(2), Some(hash2));
        drop(chain);
        std::fs::remove_dir_all(root).ok();
    }

    /// A peer that claims a huge tip but sends nothing, or streams an endless body, must not
    /// stall or exhaust a joining node.
    #[tokio::test(flavor = "multi_thread")]
    async fn sync_rejects_lying_and_oversized_peers() {
        use axum::{routing::get, Router};
        let root = home("evil");
        let homes = Chain::summon(&root, &SummonOpts::new("evil", 1)).unwrap();
        let chain: Shared = Arc::new(Mutex::new(Chain::open(&homes[0]).unwrap()));
        let app = Router::new()
            .route("/liar/api/sync", get(|| async { axum::Json(serde_json::json!({ "tip": 1_000_000_000_000u64, "blocks": [], "to": null, "finalization": null })) }))
            .route("/huge/api/sync", get(|| async { "x".repeat(MAX_SYNC_RESPONSE + 1) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        let provider = consensus::EpochProvider::new(chain.clone(), None);
        let trust = Trust::new(&chain.lock().genesis, &Pins::default(), false).unwrap();
        let liar = sync_once(&chain, &provider, &client, &format!("http://{addr}/liar"), &trust).await.unwrap_err().to_string();
        assert!(liar.contains("sent nothing"), "{liar}");
        let huge = sync_once(&chain, &provider, &client, &format!("http://{addr}/huge"), &trust).await.unwrap_err().to_string();
        assert!(huge.contains("too large"), "{huge}");
        // Claims never move our finalized tip; only verified certificates do.
        assert_eq!(chain.lock().finalized_tip, 0);
        std::fs::remove_dir_all(root).ok();
    }
}
