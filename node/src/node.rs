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
use commonware_utils::{NZU32, NZUsize};
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
    /// `--network-halted-at`: the operator says the network halted at this height, which lets
    /// slashing protection's restart rule escape there when no HTTP peer answers (protection.rs).
    pub network_halted_at: Option<u64>,
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

/// Where a validator home records that slashing protection stopped it (exit 12). The halt itself
/// lives in memory, so without this a restart (a supervisor without `RestartPreventExitStatus`,
/// a redeploy, a host move or reboot) would start the node again next to the other instance. With
/// it, the node stops again at once, with the same code, until an operator who has found and
/// stopped the other instance starts it with `--i-know-this-key-is-not-running-elsewhere` (or
/// deletes the file). Audit F52.
pub const DOUBLE_SIGN_HALT_FILE: &str = "double-sign-halt.json";

#[derive(Serialize, Deserialize)]
struct DoubleSignHalt {
    reason: String,
    at_ms: u64,
}

/// Records a slashing-protection halt in `home` (best effort: the node is stopping anyway).
fn record_double_sign_halt(home: &Path, reason: &str) {
    let path = home.join(DOUBLE_SIGN_HALT_FILE);
    let body = serde_json::to_vec_pretty(&DoubleSignHalt { reason: reason.to_string(), at_ms: crate::chain::now_ms() }).unwrap_or_default();
    let written = std::fs::File::create(&path).and_then(|mut f| {
        use std::io::Write as _;
        f.write_all(&body)?;
        f.sync_all()
    });
    if let Err(e) = written {
        eprintln!("⚠️  couldn't record the halt in {}: {e}. Keep this node stopped until you've found the other instance.", path.display());
    }
}

/// The halt to stop with at start if slashing protection stopped this home before and nobody has
/// cleared it. `force` (`--i-know-this-key-is-not-running-elsewhere`) clears it.
pub fn recorded_double_sign_halt(home: &Path, force: bool) -> Result<Option<Halt>> {
    let path = home.join(DOUBLE_SIGN_HALT_FILE);
    let Ok(bytes) = std::fs::read(&path) else { return Ok(None) };
    if force {
        std::fs::remove_file(&path).with_context(|| format!("clearing {}", path.display()))?;
        eprintln!("⚠️  {} said slashing protection stopped this validator; cleared by --i-know-this-key-is-not-running-elsewhere.", path.display());
        return Ok(None);
    }
    let why = serde_json::from_slice::<DoubleSignHalt>(&bytes).map(|h| h.reason).unwrap_or_else(|_| "(unreadable record)".into());
    Ok(Some(Halt::DoubleSignRisk {
        reason: format!(
            "{why}. Recorded in {} when it stopped; it stays stopped across restarts. Find and stop every other copy of this key, then start once with --i-know-this-key-is-not-running-elsewhere (or delete the file)",
            path.display()
        ),
    }))
}

/// A validator's stop: a slashing-protection halt is recorded in its home first, so it outlasts
/// the process.
fn exit_validator(home: &Path, halt: &Halt) -> ! {
    if let Halt::DoubleSignRisk { reason } = halt {
        record_double_sign_halt(home, reason);
    }
    exit_halted(halt)
}

/// Watches a validator for a reason to stop: a halt recorded by execution (--halt-height, an
/// unsupported version) or another instance signing with its key (slashing protection).
async fn watch_halts(chain: Shared, protection: Arc<Protection>, home: PathBuf) {
    let signal = chain.lock().halt.signal.clone();
    loop {
        if let Some(reason) = protection.halted() {
            signal.set(Halt::DoubleSignRisk { reason });
        }
        if let Some(halt) = signal.get() {
            // The block that halted is stored; wait for the executor to let go of the chain.
            drop(chain.lock());
            exit_validator(&home, &halt);
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

/// Handoff signatures checked per peer per round, at most (a peer can only offer ones we lack, for
/// changes still short of a quorum; this bounds what a lying one costs us each round).
const HANDOFF_VERIFIES_PER_ROUND: usize = 512;
/// `/api/epochs` pages read per peer per round, at most: past a change nobody has a quorum for,
/// the next pages may still hold signatures we lack.
const HANDOFF_PAGES_PER_ROUND: usize = 4;

/// Where the next `/api/epochs` page starts (`null` after the last).
#[derive(serde::Deserialize)]
struct PageNext {
    #[serde(default)]
    next: Option<u64>,
}

/// Pulls peers' validator-set handoff signatures every 30s (and signs our own, if we're a
/// validator), so this node can serve complete proofs to fast-syncing joiners. Incremental
/// (F19, F26): it asks only while some set change lacks a quorum here, only for pages from the
/// first such change on, and verifies only signatures it lacks, without the chain lock.
async fn exchange_epochs(chain: Shared, peers: Vec<String>) {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().unwrap();
    let (live, instance) = {
        let mut c = chain.lock();
        c.sign_handoffs();
        (c.live.clone(), c.instance())
    };
    loop {
        for peer in &peers {
            let ours = live.epochs();
            let from = ours.proven_through();
            if from >= ours.known_through {
                break; // every change has its quorum: nothing to ask anyone for
            }
            let mut wanted = vec![];
            let mut at = Some(from);
            for _ in 0..HANDOFF_PAGES_PER_ROUND {
                let Some(from) = at.filter(|a| *a < ours.known_through) else { break };
                let url = format!("{peer}/api/epochs?from={from}");
                let (page, next) = match get_capped(&client, &url, crate::epochs::MAX_PAGE_BYTES).await.and_then(|b| Ok((serde_json::from_slice::<crate::epochs::EpochRegistry>(&b)?, serde_json::from_slice::<PageNext>(&b)?.next))) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::debug!(peer = %peer, error = %format!("{e:#}"), "couldn't fetch handoff proofs");
                        break;
                    }
                };
                wanted = ours.candidates(&instance, &page, HANDOFF_VERIFIES_PER_ROUND);
                if !wanted.is_empty() {
                    break;
                }
                at = next.filter(|n| *n > from);
            }
            if wanted.is_empty() {
                continue;
            }
            let offered = wanted.len();
            let verified = tokio::task::spawn_blocking(move || crate::epochs::verify_candidates(wanted)).await.unwrap_or_default();
            if verified.len() < offered {
                tracing::warn!(peer = %peer, offered, valid = verified.len(), "a peer served handoff signatures that don't verify");
            }
            if !verified.is_empty() {
                chain.lock().add_handoffs(verified);
            }
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

/// Slashing protection's view of the network from outside consensus (protection.rs, audit F18):
/// while its restart rule watches, every HTTP peer is asked for its tip every couple of seconds.
/// Only a network standing still where this node is (halted, or every validator restarting) lets
/// the rule's escape fire: peers at its height must outnumber those claiming more, and silence
/// keeps it shut (`protection::network_view`), unless the operator said where the network halted.
/// A peer that claims to be ahead is asked to show it, once per peer and height of this node:
/// the finalization certificate of the blocks after this node's tip, checked against the set this
/// node knows for them ([`verify_batch`]). A claim it proves is a verified finalized height, which
/// keeps the escape shut whatever anyone else (the operator included) says; one it can't prove
/// is a single unverified answer (review of F18).
async fn watch_network(chain: Shared, peers: Vec<String>, protection: Arc<Protection>, halted_at: Option<u64>) {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(3)).build().unwrap();
    let probe_client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();
    let provider = consensus::EpochProvider::new(chain.clone(), None);
    let genesis_hash = chain.lock().genesis_hash.clone();
    // Which height of ours each peer last proved (or failed to prove) it's past.
    let mut probed: std::collections::HashMap<usize, u64> = Default::default();
    loop {
        if protection.watching() {
            let answers: Vec<Option<u64>> = futures::future::join_all(peers.iter().map(|p| peer_tip(&client, p, &genesis_hash))).await;
            let height = chain.lock().height();
            let mut ahead = vec![];
            for (i, tip) in answers.iter().enumerate() {
                if tip.is_some_and(|t| t > height) && probed.insert(i, height) != Some(height) {
                    ahead.push(i);
                }
            }
            // All at once: a peer that claims to be ahead and then stalls its answer costs one
            // probe timeout per height of this node, however many do it.
            let proofs = futures::future::join_all(ahead.iter().map(|&i| probe_ahead(&chain, &provider, &probe_client, &peers[i]))).await;
            for (i, proof) in ahead.into_iter().zip(proofs) {
                match proof {
                    Ok(to) => {
                        tracing::warn!(peer = %peers[i], height, to, "a peer proved the network finalized blocks past this node's tip (slashing protection keeps abstaining)");
                        chain.lock().set_finalized_tip(to);
                    }
                    Err(e) => tracing::info!(peer = %peers[i], height, error = %e, "a peer claims to be ahead but didn't prove it (one unverified answer)"),
                }
            }
            let tips: Vec<u64> = answers.into_iter().flatten().collect();
            let (params, height, finalized, claimed) = {
                let c = chain.lock();
                (c.state.params.clone(), c.height(), c.finalized_tip, c.network_height_claimed())
            };
            protection.note_network(&params, height, finalized, claimed, &tips, halted_at, crate::chain::now_ms());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Asks `peer` for the blocks after this node's tip (`/api/sync`) and verifies them without
/// executing anything ([`verify_batch`]): the finalized height they prove.
async fn probe_ahead(chain: &Shared, provider: &consensus::EpochProvider, client: &reqwest::Client, peer: &str) -> Result<u64> {
    let (from, epoch) = {
        let c = chain.lock();
        (c.height() + 1, c.state.params.epoch_of(c.height() + 1))
    };
    let verifier = provider.epoch_scheme(epoch).ok_or_else(|| anyhow!("no validator set known for epoch {epoch}"))?;
    let body = get_capped(client, &format!("{peer}/api/sync?from={from}&limit=1"), MAX_SYNC_RESPONSE).await?;
    let batch: SyncBatch = serde_json::from_slice(&body)?;
    let (blocks, _) = verify_batch(chain, &verifier, epoch, &batch)?;
    Ok(blocks.last().map_or(0, |b| b.height))
}

/// A peer's tip as its `/api/status` reports it (the highest of its executed, finalized and
/// claimed heights), if it answers for this chain instance.
async fn peer_tip(client: &reqwest::Client, peer: &str, genesis_hash: &str) -> Option<u64> {
    let body = get_capped(client, &format!("{peer}/api/status"), 1 << 20).await.ok()?;
    let v: serde_json::Value = serde_json::from_slice(&body).ok()?;
    if v["genesis_hash"].as_str() != Some(genesis_hash) {
        return None;
    }
    ["height", "finalized_height", "network_height_claimed"].iter().filter_map(|k| v[*k].as_u64()).max()
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
        let (height, chain_id, genesis_hash) = (c.height() + 1, c.genesis.chain_id.clone(), c.genesis_hash.clone());
        let fault_of = |evidence: &str| hex::decode(evidence).ok().and_then(|b| decode_evidence(&b).ok()).map(|e| (e.epoch, e.view, e.signer));
        pending.retain(|evidence, submitted| {
            let Ok(bytes) = hex::decode(evidence) else { return false };
            match evidence_status(&c.state, &chain_id, &genesis_hash, &bytes, height) {
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
    let mut waiting = false;
    loop {
        tick.tick().await;
        let mut c = chain.lock();
        // Only verified heights hold the duties back (`Chain::syncing`); say so when they do.
        if c.syncing() != waiting {
            waiting = !waiting;
            if waiting {
                tracing::warn!(height = c.height(), finalized = c.finalized_tip, "beacon duties wait until this node catches up");
            } else {
                tracing::info!(height = c.height(), "caught up; beacon duties resume");
            }
        }
        if waiting {
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
            Ok(h) => {
                println!("⚡ fast-synced to #{h} from a certified snapshot; history before it isn't stored on this node");
                // The snapshot's parameters may put the period below what this node assumed
                // (governance shortened it): then the root it trusted was older than is safe, and
                // the snapshot is only as good as the peers that served it (gap-6-1). A forged
                // state could hide such a change, so this only catches the honest case.
                let mut live = trust.clone();
                let shorter = live.follow(&chain.lock().state.chain_params);
                if let Some(why) = live.stale((0, None), crate::chain::now_ms()).filter(|_| shorter) {
                    eprintln!("⚠️  under the chain's parameters in force: {why}; if in doubt, wipe this home and join again with that checkpoint");
                }
                return;
            }
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

/// The p2p configuration for a `--p2p-preset`: Commonware's `local` or `recommended` limits, or
/// for `public-nat` (validators behind a NAT or a TCP proxy, such as Fly's) `recommended` with
/// shorter reconnection limits.
fn p2p_config(preset: &str, key: PrivateKey, namespace: &[u8], listen: SocketAddr, dialable: commonware_p2p::Ingress, bootstrappers: Vec<discovery::Bootstrapper<crate::pq::PublicKey>>) -> discovery::Config<PrivateKey> {
    let max_peers = NonZeroUsize::new(consensus::MAX_PEERS_PER_SET).unwrap();
    let mut cfg = if preset == "local" {
        discovery::Config::local(key, namespace, listen, dialable, bootstrappers, max_peers, MAX_MESSAGE_SIZE)
    } else {
        discovery::Config::recommended(key, namespace, listen, dialable, bootstrappers, max_peers, MAX_MESSAGE_SIZE)
    };
    if preset == "public-nat" {
        // Inbound connections arrive from the NAT's or proxy's address, often a private one, and
        // the same one for every peer.
        cfg.allow_private_ips = true;
        // So recommended's one inbound handshake per 5 s per address is one for the whole set,
        // and with its 60 s cooldown (any reservation of a peer, failed dials too, holds off the
        // next dial to it for 60–120 s and refuses its inbound for 60 s) a restarted validator
        // got its links back after a median 106 s, up to minutes: that stalled
        // blockula-testnet-2's launch and jailed a validator (2026-10-18). With these it re-linked
        // in 4–23 s locally. Neither lets a stranger in: only keys in a tracked peer set (and
        // bootstrappers) get past the handshake to reserve, and the per-subnet rate and the
        // concurrent-handshake cap stay. Both limits are this node's own, so it still works with
        // `recommended` peers, which keep Commonware's: each comes from its own address there,
        // and a shorter cooldown for every public validator is a change to measure on its own.
        cfg.peer_connection_cooldown = Duration::from_secs(2);
        cfg.allowed_handshake_rate_per_ip = commonware_runtime::Quota::per_second(NZU32!(16));
    }
    cfg.mailbox_size = NZUsize!(4096);
    cfg.tracked_peer_sets = NonZeroUsize::new(consensus::TRACKED_PEER_SETS).unwrap();
    cfg
}

pub fn run_validator(home: PathBuf, net: NetOpts) -> Result<()> {
    let mut chain = Chain::open(&home)?;
    // Validators take their genesis from the signed release; a known chain's must match it. The
    // weak-subjectivity period is the one the parameters in force set, if governance shortened it.
    let mut trust = Trust::new(&chain.genesis, &Pins::default(), false)?;
    trust.follow(&chain.state.chain_params);
    let empty = chain.height() == 0 && chain.base.is_none();
    let keep = net.pruning.keep(true, &chain.state);
    chain.set_pruning(keep);
    crate::upgrade::configure(&mut chain, consensus::SUPPORTED_PROTOCOL_VERSION, net.halt_height);
    if let Some(halt) = chain.halt.signal.get() {
        exit_halted(&halt);
    }
    if let Some(halt) = recorded_double_sign_halt(&home, net.force_key)? {
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
    let shared: Shared = Arc::new(Mutex::new(chain));
    // Far behind (new validator, long downtime)? Catch up over HTTP first: verified batches,
    // much faster than consensus backfill, and peers prune old consensus data.
    let trust = if !peers.is_empty() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        rt.block_on(async {
            // An empty home also heeds the period its peers report (gap-6-1); a validator warns.
            if empty {
                learn_checkpoint_time(&mut trust, &peers).await;
                heed_peers(&mut trust, &peers, false).await;
            }
            let own = shared.lock().head_time();
            if let Some(why) = trust.stale(own, crate::chain::now_ms()) {
                eprintln!("⚠️  {why}. Validators get recent checkpoints with each release: run the latest one.");
            }
            let trust = Arc::new(trust);
            maybe_fast_sync(&shared, &peers, net.full, &trust).await;
            catch_up(shared.clone(), peers.clone(), trust.clone()).await;
            trust
        })
    } else {
        Arc::new(trust)
    };
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
    let validator_home = home.clone();
    let stopped = cw_tokio::Runner::new(cfg).start(|context| async move {
        let ns = consensus::p2p_namespace(&chain_id);
        let p2p_cfg = p2p_config(&preset, key.clone(), &ns, listen, dialable, bootstrappers);
        let (mut network, oracle) = discovery::Network::new(context.child("network"), p2p_cfg);
        let channels: Vec<_> = CHANNELS.iter().map(|c| network.register(*c, consensus::quota(*c))).collect();
        network.start();

        let vcfg = ValidatorConfig { chain: shared.clone(), key, genesis: genesis_block, block_time_ms, epoch_blocks: genesis.epoch_blocks, partition: "blockula".into(), protection: Some(protection.clone()) };
        let (handle, marshal) = consensus::start_validator(context.child("node"), vcfg, oracle.clone(), oracle.clone(), channels).await;
        tokio::spawn(exchange_epochs(shared.clone(), peers.clone()));
        tokio::spawn(attest_claims(shared.clone()));
        tokio::spawn(beacon_duties(shared.clone(), home.clone(), Duration::from_millis(block_time_ms)));
        tokio::spawn(attest_reverts(shared.clone()));
        tokio::spawn(watch_halts(shared.clone(), protection.clone(), home.clone()));
        tokio::spawn(watch_network(shared.clone(), peers.clone(), protection.clone(), net.network_halted_at));
        let mut node = http_node(shared, Some(marshal), peers, &net, &trust);
        node.protection = Some(protection);
        let metrics = context.child("metrics");
        node.p2p_peers = Some(Arc::new(move || api::connected_peer_labels(&commonware_runtime::Metrics::encode(&metrics))));
        tokio::spawn(serve_http(node, http_bind));
        handle.await
    });
    // A halt stops consensus too (the halting block is never acknowledged): its exit code wins.
    if let Some(reason) = watched.halted() {
        halt.set(Halt::DoubleSignRisk { reason });
    }
    if let Some(h) = halt.get() {
        exit_validator(&validator_home, &h);
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

/// How long `join` and an empty home's first start wait for each peer's `/api/status`.
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);

/// An empty home has nothing of its own to go by, so before it syncs it asks every peer for the
/// weak-subjectivity period in force and enforces the shortest ([`Trust::heed`]: never below the
/// protocol's floor), so a governance change after this release can't leave it trusting a root
/// that's too old for the chain's evidence window, as long as one honest peer answers (gap-6-1).
/// With `wait`, it asks again every 10 s until a peer answers for the chain (a node about to sync
/// can't without one anyway). Returns how many answered.
async fn heed_peers(trust: &mut Trust, peers: &[String], wait: bool) -> usize {
    let mut waiting = false;
    loop {
        let reports = crate::checkpoints::peer_periods(peers, &trust.genesis_hash, STATUS_TIMEOUT).await;
        if !reports.is_empty() || !wait || peers.is_empty() {
            if trust.heed(&reports) {
                let (peer, w) = reports.iter().min_by_key(|(_, w)| *w).expect("a report");
                eprintln!("⚠️  {peer} reports the weak-subjectivity period in force as {w} blocks: shorter than this release knows (governance shortened the evidence window since); enforcing {}", trust.ws_blocks());
            }
            return reports.len();
        }
        if !waiting {
            println!("⏳ no peer answers /api/status for this chain yet (to learn the weak-subjectivity period in force before syncing an empty home); asking every 10 s");
            waiting = true;
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

/// An empty home whose newest checkpoint records no time (one in an older form) reads it from a
/// peer's copy of that block (whose hash covers it), so the root's age counts from the checkpoint
/// rather than from the genesis, where every slot the chain ever missed adds up (review-r3-sync-1).
/// Without one, it says so: the age then counts from the genesis.
async fn learn_checkpoint_time(trust: &mut Trust, peers: &[String]) {
    let Some((height, hash)) = trust.untimed_newest() else { return };
    match crate::checkpoints::peer_block_time(peers, height, &hash, STATUS_TIMEOUT).await {
        Some((peer, time)) => match trust.learn_time(height, time) {
            Ok(()) => println!("🕰️  checkpoint #{height} records no time; its block (from {peer}, whose copy hashes to it) carries {time}: its age counts from there"),
            Err(e) => eprintln!("⚠️  checkpoint #{height}'s block carries the time {time}, which the other trusted checkpoints rule out ({e:#}): its age counts from the genesis"),
        },
        None => println!("ℹ️  checkpoint #{height} records no time, and no peer serves its block: its age counts from the genesis, so every slot the chain missed since adds to it (`blockula checkpoint` prints the HEIGHT:HASH:SET_HASH:TIMESTAMP_MS form)"),
    }
}

/// Prepares a full-node home: genesis.json (copied or fetched from a node) + join.json.
/// The genesis must be pinned: by `pins.genesis_hash`, or by this binary for a known chain id
/// (`checkpoints.rs`). Checkpoints must agree with it, and the newest root must be younger than
/// the weak-subjectivity period, the shortest of this release's and the peers' reports
/// ([`heed_peers`]), unless `pins.allow_stale_checkpoint`.
pub fn join(genesis: &str, home: &Path, pins: &Pins, fast: bool) -> Result<()> {
    if home.join("validator.key").exists() {
        bail!("{} is a validator home", home.display());
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let (g, mut peers): (Genesis, Vec<String>) = if genesis.starts_with("http://") || genesis.starts_with("https://") {
        let url = genesis.trim_end_matches('/').to_string();
        let g: Genesis = rt.block_on(async { reqwest::get(format!("{url}/api/genesis")).await?.error_for_status()?.json().await })?;
        (g, vec![url])
    } else {
        (Genesis::load(Path::new(genesis))?, vec![])
    };
    let mut trust = Trust::new(&g, pins, true)?;
    for u in http_peers(&g, &[], None) {
        if !peers.contains(&u) {
            peers.push(u);
        }
    }
    rt.block_on(learn_checkpoint_time(&mut trust, &peers));
    trust.check_fresh((0, None), crate::chain::now_ms())?;
    if rt.block_on(heed_peers(&mut trust, &peers, false)) == 0 && !peers.is_empty() {
        println!("ℹ️  no peer answered /api/status for this chain: the weak-subjectivity period in force is checked again when the node first starts");
    }
    trust.check_fresh((0, None), crate::chain::now_ms())?;
    match trust.newest() {
        0 => println!("🔐 genesis {} pinned", trust.genesis_hash),
        h => println!("🔐 genesis {} pinned; newest trusted checkpoint #{h}", trust.genesis_hash),
    }
    std::fs::create_dir_all(home)?;
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
    // The pins were checked at `join`; they (and the embedded checkpoints) apply on every start,
    // under the weak-subjectivity period of the parameters in force in this node's own state.
    let mut trust = Trust::new(&chain.genesis, &jc.pins, false)?;
    trust.follow(&chain.state.chain_params);
    let peers = if net.peer_http.is_empty() { jc.peers } else { http_peers(&chain.genesis, &net.peer_http, None) };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    // An empty home is where a stale root bites (a fresh sync); a synced one trusts its own head.
    let (empty, own) = (chain.height() == 0 && chain.base.is_none(), chain.head_time());
    if empty {
        rt.block_on(learn_checkpoint_time(&mut trust, &peers));
    }
    trust.check_fresh(own, crate::chain::now_ms())?;
    let keep = net.pruning.keep(false, &chain.state);
    chain.set_pruning(keep);
    // A halted full node keeps serving reads at its height, with the reason in /api/health.
    crate::upgrade::configure(&mut chain, consensus::SUPPORTED_PROTOCOL_VERSION, net.halt_height);
    println!("🦇 Full node for `{}` at height {} — syncing from {}", chain.genesis.chain_id, chain.height(), peers.join(", "));
    let shared: Shared = Arc::new(Mutex::new(chain));
    let fast = jc.fast && !net.full;
    rt.block_on(async move {
        // The API and explorer come up immediately, while syncing.
        tokio::spawn(serve_http(http_node(shared.clone(), None, peers.clone(), &net, &trust), bind));
        // The period in force, as the peers report it, before anything is fetched (gap-6-1):
        // an empty home waits for a peer's answer and refuses a root too old for it (`join` asked
        // too, but governance may have acted since); a synced one only warns.
        heed_peers(&mut trust, &peers, empty).await;
        if let Some(why) = trust.check_fresh(own, crate::chain::now_ms())? {
            eprintln!("⚠️  {why}");
        }
        let trust = Arc::new(trust);
        maybe_fast_sync(&shared, &peers, !fast, &trust).await;
        tokio::spawn(exchange_epochs(shared.clone(), peers.clone()));
        sync_loop(shared, peers, false, trust).await;
        Ok(())
    })
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

/// What one sync round brought: the blocks it applied (0: the peer had nothing new), their size
/// as sent (hex) and how many were asked for, both counted only up to what an honest batch holds,
/// and how long the peer took to send them.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Round {
    blocks: u64,
    bytes: u64,
    limit: u64,
    took: Duration,
}

/// One sync round against `peer`: what it applied ([`Round`]).
async fn sync_once(chain: &Shared, provider: &consensus::EpochProvider, client: &reqwest::Client, peer: &str, trust: &Trust) -> Result<Round> {
    // Batches never cross an epoch boundary: each is verified with that epoch's validator set,
    // which we learn by executing the previous epoch's last block.
    let (from, limit, epoch) = {
        let c = chain.lock();
        let from = c.height() + 1;
        let epoch = c.state.params.epoch_of(from);
        (from, (c.state.params.last_of(epoch) - from + 1).min(200), epoch)
    };
    let verifier = provider.epoch_scheme(epoch).ok_or_else(|| anyhow!("no validator set known for epoch {epoch}"))?;
    let asked = std::time::Instant::now();
    let body = get_capped(client, &format!("{peer}/api/sync?from={from}&limit={limit}"), MAX_SYNC_RESPONSE).await?;
    let took = asked.elapsed();
    let batch: SyncBatch = serde_json::from_slice(&body)?;
    // A peer's claimed tip is display-only, and only noted once its answer checked out.
    if batch.blocks.is_empty() {
        if batch.tip >= from {
            bail!("peer claims height {} but sent nothing", batch.tip);
        }
        chain.lock().note_claim(peer, batch.tip);
        return Ok(Round { blocks: 0, bytes: 0, limit, took });
    }
    apply_batch_trusted(chain, &verifier, epoch, &batch, Some(trust))?;
    chain.lock().note_claim(peer, batch.tip);
    // What an honest peer would have sent for this request at most: no credit for more.
    let bytes = batch.blocks.iter().take(limit as usize).map(|b| b.len() as u64).sum::<u64>().min(api::SYNC_BATCH_BYTES as u64);
    Ok(Round { blocks: batch.blocks.len() as u64, bytes, limit, took })
}

/// How long a peer that sent blocks slower than the chain makes them is asked last, at first;
/// each slow round in a row doubles it, up to `SLOW_PEER_MAX`.
const SLOW_PEER_BENCH: Duration = Duration::from_secs(60);
const SLOW_PEER_MAX: Duration = Duration::from_secs(15 * 60);
/// A round at least a block time long that delivered blocks this many times slower than another
/// peer's recent best, in blocks and in bytes per second, is too slow too: peers are judged
/// against each other, not only the chain.
const SLOWER_THAN_BEST: u32 = 4;
/// A peer's rate counts as recent (for the best, and for "keep the fastest") this long.
const RATE_WINDOW: Duration = Duration::from_secs(120);
/// Every this many good rounds the next peer is tried instead of the fastest, so a faster one is
/// found and the rates stay fresh; a slow peer gets no more than that share.
const EXPLORE_EVERY: u64 = 8;
/// A round's rate is measured over at least this share of a block time (a quarter): a tiny round
/// answered at once counts as if it took that long, so it can't set a bar real batches can't
/// meet (review-r2-sync-1). To beat a peer by [`SLOWER_THAN_BEST`], another must really deliver,
/// within a quarter of a block time, more than that peer delivers in a block time.
const RATE_FLOOR_DIVISOR: u32 = 4;

/// A peer's newest round, as rates: blocks and bytes a second, and when.
#[derive(Clone, Copy, Debug)]
struct Rate {
    blocks: f64,
    bytes: f64,
    at: std::time::Instant,
}

impl Rate {
    fn of(r: &Round, block_time: Duration, at: std::time::Instant) -> Self {
        let secs = r.took.max(block_time / RATE_FLOOR_DIVISOR).as_secs_f64().max(0.001);
        Rate { blocks: r.blocks.min(r.limit.max(1)) as f64 / secs, bytes: r.bytes as f64 / secs, at }
    }

    /// [`SLOWER_THAN_BEST`] times slower than `best` in both blocks and bytes a second.
    fn far_below(&self, best: (f64, f64)) -> bool {
        let k = f64::from(SLOWER_THAN_BEST);
        self.blocks * k < best.0 && self.bytes * k < best.1
    }
}

/// Which peer sync asks next (F27, review-r2-sync-1). Peers take turns on empty replies. After a
/// good round sync goes on with the fastest peer (one not asked for [`RATE_WINDOW`] first, then
/// the best recent rate), trying the next one every [`EXPLORE_EVERY`] rounds. A peer is benched
/// (asked again only once every other peer is benched too, or its time is up) when a round
/// failed, or took long for what it delivered: blocks at less than twice the chain's rate
/// (`blocks × block_time < 2 × time taken`: a node fed that way never catches up, or only
/// slowly), or, in a round at least a block time long, [`SLOWER_THAN_BEST`] times slower than
/// another peer's recent rate (blocks and bytes, each round measured over at least a quarter of
/// a block time), or an empty answer after more than two block times. So a peer that trickles
/// real, certified blocks just fast enough for the chain, or stalls before saying it has
/// nothing, gets a round now and then, not the node. A peer benched only for being slower than
/// another keeps its rate as a bar while it waits, and is free again once the peers that beat it
/// stop doing so: when every peer not benched has said "nothing new" since the last blocks (a
/// slow peer with blocks beats a fast one with none; a peer with nothing new loses its rate), or
/// when a short round that left blocks behind is far below its rate. The first peer asked is a
/// random one.
struct Rotation {
    at: usize,
    benched_until: Vec<Option<std::time::Instant>>,
    /// Whether a peer's bench is only for being slower than another peer (not for failing,
    /// stalling or falling short of the chain's rate).
    relative: Vec<bool>,
    strikes: Vec<u32>,
    /// Each peer's newest good or slow round, until it answers "nothing new".
    rates: Vec<Option<Rate>>,
    /// When each peer was last asked.
    asked: Vec<Option<std::time::Instant>>,
    /// Which peers answered "nothing new" since any peer last delivered blocks.
    quiet: Vec<bool>,
    good_rounds: u64,
}

impl Rotation {
    fn new(peers: usize) -> Self {
        let n = peers.max(1);
        let mut r = [0u8; 8];
        let start = if getrandom::fill(&mut r).is_ok() { (u64::from_le_bytes(r) % n as u64) as usize } else { 0 };
        Self::starting_at(n, start)
    }

    fn starting_at(peers: usize, at: usize) -> Self {
        let n = peers.max(1);
        Self { at: at % n, benched_until: vec![None; n], relative: vec![false; n], strikes: vec![0; n], rates: vec![None; n], asked: vec![None; n], quiet: vec![false; n], good_rounds: 0 }
    }

    fn current(&self) -> usize {
        self.at
    }

    fn benched(&self, i: usize, now: std::time::Instant) -> bool {
        self.benched_until[i].is_some_and(|t| t > now)
    }

    /// Peer `i`'s rate if it's a fair bar for the others: from the last [`RATE_WINDOW`], or still
    /// on the bench for being slower than another (it delivered at that rate; the rate stays the
    /// bar while it waits, so a peer far slower than it can't take its place). Not a peer benched
    /// for failing or stalling.
    fn bar(&self, i: usize, now: std::time::Instant) -> Option<Rate> {
        let benched = self.benched(i, now);
        self.rates[i].filter(|r| if benched { self.relative[i] } else { now.saturating_duration_since(r.at) <= RATE_WINDOW })
    }

    fn recent_rate(&self, i: usize, now: std::time::Instant) -> Option<f64> {
        self.rates[i].filter(|r| now.saturating_duration_since(r.at) <= RATE_WINDOW).map(|r| r.blocks)
    }

    /// Moves on to the next peer that isn't benched (or just the next one, if all are).
    fn advance(&mut self, now: std::time::Instant) {
        let n = self.benched_until.len();
        let next = (1..=n).map(|step| (self.at + step) % n).find(|i| !self.benched(*i, now));
        self.at = next.unwrap_or((self.at + 1) % n);
    }

    /// After a good round: the next peer not benched that wasn't asked for [`RATE_WINDOW`] (in
    /// turn), else the one with the best recent rate (this one, unless another did better).
    fn fastest(&mut self, now: std::time::Instant) {
        let n = self.benched_until.len();
        let order: Vec<usize> = (1..=n).map(|step| (self.at + step) % n).filter(|i| !self.benched(*i, now)).collect();
        if let Some(i) = order.iter().copied().find(|i| self.asked[*i].is_none_or(|t| now.saturating_duration_since(t) > RATE_WINDOW)) {
            self.at = i;
            return;
        }
        let mut best: Option<(usize, f64)> = None;
        for i in order {
            if let Some(r) = self.recent_rate(i, now).filter(|r| best.is_none_or(|(_, b)| *r > b)) {
                best = Some((i, r));
            }
        }
        if let Some((i, _)) = best {
            self.at = i;
        }
    }

    /// Benches peer `i` (`relative`: only for being slower than another): 60 s, doubling with
    /// each bench in a row up to [`SLOW_PEER_MAX`].
    fn bench_at(&mut self, i: usize, now: std::time::Instant, relative: bool) -> Duration {
        self.strikes[i] = self.strikes[i].saturating_add(1);
        let bench = SLOW_PEER_BENCH.saturating_mul(1u32 << (self.strikes[i] - 1).min(8)).min(SLOW_PEER_MAX);
        self.benched_until[i] = Some(now + bench);
        self.relative[i] = relative;
        bench
    }

    /// Benches the current peer and moves on.
    fn bench(&mut self, now: std::time::Instant, relative: bool) -> Duration {
        let bench = self.bench_at(self.at, now, relative);
        self.advance(now);
        bench
    }

    /// A round that applied `round.blocks` (≥ 1). Returns how long the peer is benched for when
    /// that was too slow (and moves on), `None` when it was fine.
    fn progress(&mut self, round: Round, block_time: Duration, now: std::time::Instant) -> Option<Duration> {
        let i = self.at;
        self.asked[i] = Some(now);
        self.quiet.fill(false);
        let rate = Rate::of(&round, block_time, now);
        let others = (0..self.rates.len()).filter(|j| *j != i).filter_map(|j| self.bar(j, now));
        let best = others.fold(None, |b: Option<(f64, f64)>, r| Some(b.map_or((r.blocks, r.bytes), |(x, y)| (x.max(r.blocks), y.max(r.bytes)))));
        self.rates[i] = Some(rate);
        let below_chain = u128::from(round.blocks) * block_time.as_millis() < 2 * round.took.as_millis();
        let below_best = round.took >= block_time && best.is_some_and(|b| rate.far_below(b));
        if below_chain || below_best {
            return Some(self.bench(now, !below_chain));
        }
        self.strikes[i] = 0;
        self.benched_until[i] = None;
        self.relative[i] = false;
        // A short round that left blocks behind (fewer than asked) far below the rate a peer
        // benched only for being slower than another delivered: that peer is the faster one now,
        // so it's free again. A peer can't bench another with one fast round and then keep the
        // turn by trickling (review-r2-sync-1); whoever holds it delivers at least a quarter of
        // what the benched peer did, in full batches, or in rounds a block time long (judged
        // against it above).
        if round.blocks < round.limit && round.took < block_time {
            for j in (0..self.rates.len()).filter(|j| *j != i) {
                if self.benched(j, now) && self.relative[j] && self.rates[j].is_some_and(|b| rate.far_below((b.blocks, b.bytes))) {
                    self.benched_until[j] = None;
                    self.relative[j] = false;
                }
            }
        }
        self.good_rounds += 1;
        if self.good_rounds.is_multiple_of(EXPLORE_EVERY) {
            self.advance(now);
        } else {
            self.fastest(now);
        }
        None
    }

    /// A round with nothing new, answered after `took`: benched when that took more than two
    /// block times (an honest peer says "nothing new" at once), otherwise the next peer's turn.
    /// Either way the peer's rate is dropped: one with nothing to offer isn't the bar for the
    /// others. Once every peer not benched has said "nothing new" since the last blocks, the
    /// peers benched only for being slower than another are free again (review-r2-sync-1).
    fn nothing(&mut self, took: Duration, block_time: Duration, now: std::time::Instant) -> Option<Duration> {
        let i = self.at;
        self.asked[i] = Some(now);
        self.rates[i] = None;
        self.quiet[i] = true;
        let stalled = (took > 2 * block_time).then(|| self.bench_at(i, now, false));
        let n = self.benched_until.len();
        if (0..n).filter(|j| !self.benched(*j, now)).all(|j| self.quiet[j]) {
            for j in 0..n {
                if self.benched(j, now) && self.relative[j] {
                    self.benched_until[j] = None;
                    self.relative[j] = false;
                }
            }
        }
        self.advance(now);
        stalled
    }

    /// A round that failed (no answer in time, a bad or lying answer, an unreachable peer):
    /// benched, so it isn't asked again after every good round.
    fn failed(&mut self, now: std::time::Instant) -> Duration {
        self.asked[self.at] = Some(now);
        self.bench(now, false)
    }
}

/// Follows peers forever (full nodes). Peers rotate on errors, bad batches and empty replies,
/// sync keeps to the fastest peer, and one that sends blocks slower than the chain makes them or
/// far slower than the others, or stalls, is benched ([`Rotation`]), so one stalling, trickling
/// or lying peer can't hold the node back.
async fn sync_loop(chain: Shared, peers: Vec<String>, stop_when_caught_up: bool, trust: Arc<Trust>) {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();
    let provider = consensus::EpochProvider::new(chain.clone(), None);
    let block_time = Duration::from_millis(chain.lock().genesis.block_time_ms);
    if peers.is_empty() {
        eprintln!("❌ no peers to sync from (set --peer-http)");
        return;
    }
    let mut rotation = Rotation::new(peers.len());
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
        let url = &peers[rotation.current()];
        match sync_once(&chain, &provider, &client, url, &trust).await {
            Ok(round) if round.blocks > 0 => match rotation.progress(round, block_time, std::time::Instant::now()) {
                // A slow round doesn't count as progress: catching up must not hinge on it.
                Some(bench) => eprintln!("⚠️  sync from {url}: {} block(s) in {:.1}s, slower than the chain or the other peers; asking other peers first for {}s", round.blocks, round.took.as_secs_f64(), bench.as_secs()),
                None => {
                    idle_rounds = 0;
                    failures = 0;
                }
            },
            Ok(Round { took, .. }) => {
                idle_rounds += 1;
                // Ask someone else next time, so no single peer can stall us.
                if let Some(bench) = rotation.nothing(took, block_time, std::time::Instant::now()) {
                    eprintln!("⚠️  sync from {url}: nothing new, after {:.1}s; asking other peers first for {}s", took.as_secs_f64(), bench.as_secs());
                }
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
                rotation.failed(std::time::Instant::now());
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
    let (blocks, fin_bytes) = verify_batch(chain, verifier, epoch, batch)?;
    if let Some(t) = trust {
        for b in &blocks {
            t.check_block(b.height, &hex::encode(b.digest_bytes()), Some(b.timestamp_ms))?;
        }
    }
    // One block at a time, the chain lock held only for the in-memory parts (sync is this
    // node's only executor, so nothing else extends the chain in between).
    let last = blocks.last().map(|b| b.height);
    for b in &blocks {
        let cert = (Some(b.height) == last).then(|| fin_bytes.clone());
        Chain::execute_shared(chain, consensus::finalized_from(b, cert))?;
    }
    Ok(())
}

/// Checks a sync batch without executing it: contiguous heights hash-linked to this node's tip,
/// the last block covered by a finalization certificate of `epoch` that `verifier` (the epoch's
/// set) accepts. Returns the decoded blocks and the certificate's bytes.
pub(crate) fn verify_batch(chain: &Shared, verifier: &MlDsaScheme, epoch: u64, batch: &SyncBatch) -> Result<(Vec<CBlock>, Vec<u8>)> {
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
    }
    Ok((blocks, fin_bytes))
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

    /// F52: a slashing-protection stop (exit 12) outlasts the process. Whatever restarts the node
    /// (a supervisor's restart policy, a redeploy, a host move), it stops again with code 12 until
    /// the operator clears it with --i-know-this-key-is-not-running-elsewhere.
    #[test]
    fn a_double_sign_halt_survives_restarts_until_cleared() {
        let h = home("sticky");
        std::fs::create_dir_all(&h).unwrap();
        assert!(recorded_double_sign_halt(&h, false).unwrap().is_none(), "nothing recorded");
        record_double_sign_halt(&h, "another instance signed a Finalize vote in epoch 1, view 15");
        for _ in 0..2 {
            let halt = recorded_double_sign_halt(&h, false).unwrap().expect("stays halted");
            assert_eq!(halt.exit_code(), crate::upgrade::EXIT_DOUBLE_SIGN_RISK);
            let text = halt.to_string();
            assert!(text.contains("view 15") && text.contains(DOUBLE_SIGN_HALT_FILE) && text.contains("--i-know-this-key-is-not-running-elsewhere"), "{text}");
        }
        // The operator found the other copy: one forced start clears it, later starts are normal.
        assert!(recorded_double_sign_halt(&h, true).unwrap().is_none());
        assert!(!h.join(DOUBLE_SIGN_HALT_FILE).exists());
        assert!(recorded_double_sign_halt(&h, false).unwrap().is_none());
        // A record that doesn't parse still stops the node.
        std::fs::write(h.join(DOUBLE_SIGN_HALT_FILE), "garbage").unwrap();
        assert_eq!(recorded_double_sign_halt(&h, false).unwrap().map(|h| h.exit_code()), Some(crate::upgrade::EXIT_DOUBLE_SIGN_RISK));
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

    /// The blockula-testnet-2 launch stall: behind Fly's proxy every peer shares one address, and
    /// recommended's reconnection limits kept restarted validators unlinked for minutes.
    /// `public-nat` re-links in seconds; the other presets stay Commonware's.
    #[test]
    fn public_nat_reconnects_in_seconds_and_other_presets_stay_commonwares() {
        let key = PrivateKey::from_seed_bytes([7; 32]);
        let listen: SocketAddr = "127.0.0.1:30300".parse().unwrap();
        let ours = |preset: &str| p2p_config(preset, key.clone(), b"ns", listen, listen.into(), vec![]);
        let max_peers = NonZeroUsize::new(consensus::MAX_PEERS_PER_SET).unwrap();
        let recommended = discovery::Config::recommended(key.clone(), b"ns", listen, listen, vec![], max_peers, MAX_MESSAGE_SIZE);
        let local = discovery::Config::local(key.clone(), b"ns", listen, listen, vec![], max_peers, MAX_MESSAGE_SIZE);

        let nat = ours("public-nat");
        assert!(nat.allow_private_ips);
        assert!(nat.peer_connection_cooldown <= Duration::from_secs(5), "{:?}", nat.peer_connection_cooldown);
        let rate = nat.allowed_handshake_rate_per_ip;
        assert!(rate.burst_size().get() >= 16 && rate.replenish_interval() <= Duration::from_secs(1) / 16, "{rate:?}");
        // The rest is recommended's, the gossip timing peers rate-limit each other on included.
        assert_eq!(nat.allowed_handshake_rate_per_subnet, recommended.allowed_handshake_rate_per_subnet);
        assert_eq!(nat.gossip_bit_vec_frequency, recommended.gossip_bit_vec_frequency);
        assert_eq!(nat.dial_frequency, recommended.dial_frequency);

        for (preset, theirs) in [("recommended", &recommended), ("local", &local)] {
            let cfg = ours(preset);
            assert_eq!(cfg.peer_connection_cooldown, theirs.peer_connection_cooldown, "{preset}");
            assert_eq!(cfg.allowed_handshake_rate_per_ip, theirs.allowed_handshake_rate_per_ip, "{preset}");
            assert_eq!(cfg.allowed_handshake_rate_per_subnet, theirs.allowed_handshake_rate_per_subnet, "{preset}");
            assert_eq!(cfg.allow_private_ips, theirs.allow_private_ips, "{preset}");
            assert_eq!(cfg.dial_frequency, theirs.dial_frequency, "{preset}");
            assert_eq!(cfg.gossip_bit_vec_frequency, theirs.gossip_bit_vec_frequency, "{preset}");
        }
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
        let wrong_genesis = Pins { checkpoints: vec![Checkpoint { height: 0, hash: "ab".repeat(32), set_hash: None, timestamp_ms: None }], ..pin(&hash) };
        assert!(try_join(&gpath, "full-cp0", &wrong_genesis).unwrap_err().to_string().contains("checkpoint #0"));
        // Pinned: joined, and `run` finds the pins in join.json.
        let pins = Pins { checkpoints: vec![Checkpoint { height: 5, hash: "cd".repeat(32), set_hash: None, timestamp_ms: None }], ..pin(&hash) };
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

    /// gap-6-1: a joiner on a release that predates a governance change shortening the evidence
    /// window learns the period in force from its peers' `/api/status`. A 9-day-old checkpoint is
    /// fine at the genesis period (10.5 days); once governance has cut the window to its floor
    /// (two weeks, so a week's period), the same `join` is refused and says why, and so is the
    /// first start of a home that joined before the change and never synced (`run_full_node`'s
    /// empty-home check). A fresher checkpoint or `--allow-stale-checkpoint` still joins. A peer
    /// claiming less than any chain can have is held to the floor, and a peer of another chain
    /// isn't heard.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_joiner_heeds_the_period_its_peers_report() {
        use crate::checkpoints::Checkpoint;
        use axum::{routing::get, Json, Router};
        let root = home("wsjoin");
        let homes = Chain::summon(&root, &SummonOpts::new("wsjoin", 1)).unwrap();
        // A month-old chain.
        let gpath = homes[0].join("genesis.json");
        let mut g = Genesis::load(&gpath).unwrap();
        g.created_at_ms -= 30 * 86_400_000;
        std::fs::write(&gpath, serde_json::to_string_pretty(&g).unwrap()).unwrap();
        let chain: Shared = Arc::new(Mutex::new(Chain::open(&homes[0]).unwrap()));
        let hash = crate::checkpoints::genesis_hash(&g);
        let serve = |app: Router| async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
            url
        };
        let url = serve(crate::api::router(crate::api::Node::new(chain.clone(), None, vec![], Default::default()))).await;
        let day = 86_400_000 / g.block_time_ms;
        let pins = |at: u64| Pins { genesis_hash: Some(hash.clone()), checkpoints: vec![Checkpoint { height: at, hash: "cd".repeat(32), set_hash: None, timestamp_ms: None }], ..Default::default() };
        let try_join = |from: &str, name: &str, pins: Pins| {
            let (from, home) = (from.to_string(), root.join(name));
            tokio::task::spawn_blocking(move || join(&from, &home, &pins, true))
        };
        // Before the change: 9 days old is fresh.
        try_join(&url, "before", pins(21 * day)).await.unwrap().unwrap();
        // Governance cuts the evidence window as far as it can: two weeks.
        {
            let mut c = chain.lock();
            let mut j = crate::chain::Journal::default();
            c.state.exec_set_param(&mut j, "evidence_max_age_epochs", crate::params::evidence_floor_epochs(&g.params())).unwrap();
            c.set_pruning(None); // publishes the state to the API
        }
        let err = try_join(&url, "after", pins(21 * day)).await.unwrap().unwrap_err().to_string();
        assert!(err.contains(&format!("~7.0 days, in force on the chain as {url} reports it")) && err.contains("--allow-stale-checkpoint"), "{err}");
        assert!(!root.join("after").exists(), "a refused join writes nothing");
        try_join(&url, "fresh", pins(24 * day)).await.unwrap().unwrap();
        try_join(&url, "allowed", Pins { allow_stale_checkpoint: true, ..pins(21 * day) }).await.unwrap().unwrap();
        // The home that joined before the change: its first start asks again, and stops.
        let jc: JoinConfig = serde_json::from_slice(&std::fs::read(root.join("before").join("join.json")).unwrap()).unwrap();
        assert!(jc.peers.contains(&url));
        let mut trust = Trust::new(&g, &jc.pins, false).unwrap();
        assert_eq!(trust.check_fresh((0, None), crate::chain::now_ms()).unwrap(), None, "by this release alone: fresh");
        assert_eq!(heed_peers(&mut trust, &jc.peers, true).await, 1, "the peer answered; the genesis validator's address didn't");
        assert!(trust.check_fresh((0, None), crate::chain::now_ms()).unwrap_err().to_string().contains("~7.0 days"));
        // A liar claiming a 1-block period is held to the floor: a week-old checkpoint still joins.
        let status = |genesis_hash: String| move || {
            let genesis_hash = genesis_hash.clone();
            async move { Json(serde_json::json!({ "genesis_hash": genesis_hash, "weak_subjectivity_blocks": 1 })) }
        };
        let served = g.clone();
        let liar = serve(Router::new().route("/api/genesis", get(move || async move { Json(served.clone()) })).route("/api/status", get(status(hash.clone())))).await;
        let err = try_join(&liar, "lied-to", pins(21 * day)).await.unwrap().unwrap_err().to_string();
        assert!(err.contains("~7.0 days") && err.contains("the protocol's floor; it reports 1"), "{err}");
        try_join(&liar, "lied-to", pins(24 * day)).await.unwrap().unwrap();
        // Another chain's node isn't heard at all.
        let other = serve(Router::new().route("/api/status", get(status("00".repeat(32))))).await;
        assert_eq!(crate::checkpoints::peer_periods(&[other.clone(), url.clone()], &hash, Duration::from_secs(5)).await, vec![(url.clone(), 7 * day)]);
        drop(chain);
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
        let scheme = MlDsaScheme::signer(&consensus::genesis_namespace(&g), consensus::participants(&g).unwrap(), PrivateKey::from_seed_bytes(seed)).unwrap();
        let last = &blocks[3];
        let vote = Finalize::sign(&scheme, Proposal::new(Round::new(Epoch::new(0), View::new(3)), View::new(2), last.digest())).unwrap();
        let fin = Finalization::from_finalizes(&scheme, commonware_utils::iter::NonEmpty::try_new([vote].iter()).unwrap(), &commonware_parallel::Sequential).unwrap();
        let batch = SyncBatch { tip: 3, blocks: blocks[1..].iter().map(|b| hex::encode(b.encode())).collect(), to: Some(3), finalization: Some(hex::encode(fin.encode())) };
        let verifier = consensus::EpochProvider::new(chain.clone(), None).epoch_scheme(0).unwrap();
        let hash2 = hex::encode(blocks[2].digest_bytes());
        let genesis_set = crate::checkpoints::genesis_checkpoint(&g).set_hash;
        let timed = |height: u64, hash: &str, set: Option<String>, time: Option<u64>| Trust::new(&g, &Pins { checkpoints: vec![Checkpoint { height, hash: hash.into(), set_hash: set, timestamp_ms: time }], ..Default::default() }, false).unwrap();
        let trust = |hash: &str, set: Option<String>| timed(2, hash, set, None);
        let err = apply_batch_trusted(&chain, &verifier, 0, &batch, Some(&trust(&"ab".repeat(32), None))).unwrap_err().to_string();
        assert!(err.contains("another chain"), "{err}");
        let err = apply_batch_trusted(&chain, &verifier, 0, &batch, Some(&trust(&hash2, Some("cd".repeat(32))))).unwrap_err().to_string();
        assert!(err.contains("forged validator-set history"), "{err}");
        // review-r3-sync-1: a checkpoint's recorded time is the block's (its hash covers it), and
        // later blocks are no earlier than it allows: a checkpoint claiming to be fresher than it
        // is gets caught by the blocks.
        let at2 = blocks[2].timestamp_ms;
        let err = apply_batch_trusted(&chain, &verifier, 0, &batch, Some(&timed(2, &hash2, genesis_set.clone(), Some(at2 + 1)))).unwrap_err().to_string();
        assert!(err.contains("carries the time") && err.contains("the checkpoint's time is wrong"), "{err}");
        let hash1 = hex::encode(blocks[1].digest_bytes());
        let err = apply_batch_trusted(&chain, &verifier, 0, &batch, Some(&timed(1, &hash1, None, Some(blocks[1].timestamp_ms + 500)))).unwrap_err().to_string();
        assert!(err.contains("block #1") && err.contains("the checkpoint's time is wrong"), "{err}");
        assert_eq!(chain.lock().height(), 0, "nothing executed");
        apply_batch_trusted(&chain, &verifier, 0, &batch, Some(&timed(2, &hash2, genesis_set, Some(at2)))).unwrap();
        assert_eq!(chain.lock().hash_at(2), Some(hash2));
        drop(chain);
        std::fs::remove_dir_all(root).ok();
    }

    /// review-r3-sync-1, for a checkpoint in an older form (no time): a chain a year old whose
    /// blocks only started a minute ago (a year of missed slots, the extreme of an aging chain).
    /// Counted from the genesis, a checkpoint at its tip is a year old, and `join` refuses it. An
    /// empty home reads the time from a peer's copy of the checkpoint's block (`/api/sync`, the
    /// consensus block, whose hash covers its time), so it's a minute old and joins. A peer
    /// serving junk or another block is passed over; without a copy, the age counts from the
    /// genesis as before.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_untimed_checkpoint_counts_from_its_blocks_time() {
        use crate::checkpoints::Checkpoint;
        use axum::{routing::get, Json, Router};
        use commonware_codec::Encode as _;
        use commonware_consensus::simplex::types::{Finalize, Proposal};
        use commonware_consensus::types::{Epoch, Round, View};
        let root = home("cptime");
        let homes = Chain::summon(&root, &SummonOpts::new("cptime", 1)).unwrap();
        let gpath = homes[0].join("genesis.json");
        let mut g = Genesis::load(&gpath).unwrap();
        g.created_at_ms -= 365 * 86_400_000;
        std::fs::write(&gpath, serde_json::to_string_pretty(&g).unwrap()).unwrap();
        let seed: [u8; 32] = hex::decode(std::fs::read_to_string(homes[0].join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
        let source = root.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::copy(&gpath, source.join("genesis.json")).unwrap();
        let chain: Shared = Arc::new(Mutex::new(Chain::open(&source).unwrap()));
        // Blocks 1..=3, a minute ago, the last certified by the one validator.
        let start = crate::chain::now_ms() - 60_000;
        let root_hash = consensus::hex_32(&g.initial_state().root());
        let mut blocks = vec![consensus::genesis_block(&g)];
        for h in 1..=3u64 {
            let parent = blocks.last().unwrap().digest();
            blocks.push(CBlock::new(parent, h, h, h - 1, start + 1000 * h, g.validators[0].address.clone(), 0, root_hash, vec![]));
        }
        let scheme = MlDsaScheme::signer(&consensus::genesis_namespace(&g), consensus::participants(&g).unwrap(), PrivateKey::from_seed_bytes(seed)).unwrap();
        let vote = Finalize::sign(&scheme, Proposal::new(Round::new(Epoch::new(0), View::new(3)), View::new(2), blocks[3].digest())).unwrap();
        let fin = Finalization::from_finalizes(&scheme, commonware_utils::iter::NonEmpty::try_new([vote].iter()).unwrap(), &commonware_parallel::Sequential).unwrap();
        let batch = SyncBatch { tip: 3, blocks: blocks[1..].iter().map(|b| hex::encode(b.encode())).collect(), to: Some(3), finalization: Some(hex::encode(fin.encode())) };
        let verifier = consensus::EpochProvider::new(chain.clone(), None).epoch_scheme(0).unwrap();
        apply_batch_trusted(&chain, &verifier, 0, &batch, None).unwrap();
        chain.lock().set_pruning(None); // publishes the state to the API
        let serve = |app: Router| async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
            url
        };
        let url = serve(crate::api::router(crate::api::Node::new(chain.clone(), None, vec![], Default::default()))).await;
        let (hash, hash2, t2) = (crate::checkpoints::genesis_hash(&g), hex::encode(blocks[2].digest_bytes()), blocks[2].timestamp_ms);
        // A peer serving junk, or block #3 as #2, is passed over; the honest copy is read.
        let b3 = hex::encode(blocks[3].encode());
        let junk = serve(Router::new().route("/api/sync", get(|| async { Json(serde_json::json!({ "blocks": ["00ff"] })) }))).await;
        let wrong = serve(Router::new().route("/api/sync", get(move || async move { Json(serde_json::json!({ "blocks": [b3] })) }))).await;
        let peers = [junk.clone(), wrong.clone(), url.clone()];
        assert_eq!(crate::checkpoints::peer_block_time(&peers, 2, &hash2, STATUS_TIMEOUT).await, Some((url.clone(), t2)));
        assert_eq!(crate::checkpoints::peer_block_time(&peers[..2], 2, &hash2, STATUS_TIMEOUT).await, None);
        assert_eq!(crate::checkpoints::peer_block_time(&peers, 2, &"ab".repeat(32), STATUS_TIMEOUT).await, None, "no block hashes to another hash");
        // Counted from the genesis, the tip is a year old; from its block's time, a minute.
        let pins = |hash2: &str| Pins { genesis_hash: Some(hash.clone()), checkpoints: vec![Checkpoint { height: 2, hash: hash2.into(), set_hash: None, timestamp_ms: None }], ..Default::default() };
        let mut trust = Trust::new(&g, &pins(&hash2), true).unwrap();
        let err = trust.check_fresh((0, None), crate::chain::now_ms()).unwrap_err().to_string();
        assert!(err.contains("~364.9 days") || err.contains("~365.0 days"), "{err}");
        learn_checkpoint_time(&mut trust, std::slice::from_ref(&url)).await;
        assert_eq!(trust.check_fresh((0, None), crate::chain::now_ms()).unwrap(), None);
        assert_eq!(trust.checkpoints().next().unwrap().timestamp_ms, Some(t2));
        // A time the other checkpoints rule out isn't learned (a later timed checkpoint can't be
        // closer than the blocks between them allow).
        let mut pinned = pins(&hash2);
        pinned.checkpoints.push(Checkpoint { height: 5, hash: "cd".repeat(32), set_hash: None, timestamp_ms: Some(t2 + 1000) });
        let mut ruled_out = Trust::new(&g, &pinned, true).unwrap();
        assert!(ruled_out.learn_time(2, t2).unwrap_err().to_string().contains("checkpoint #5 records"));
        assert_eq!(ruled_out.untimed_newest(), None, "the newest (#5) records its time");
        assert_eq!(ruled_out.checkpoints().next().unwrap().timestamp_ms, None, "unchanged");
        // `join` learns it the same way, and refuses (from the genesis' line) when no peer can
        // show the block, or the checkpoint names another one.
        let try_join = |name: &str, pins: Pins| {
            let (from, home) = (url.clone(), root.join(name));
            tokio::task::spawn_blocking(move || join(&from, &home, &pins, true))
        };
        try_join("joined", pins(&hash2)).await.unwrap().unwrap();
        let err = try_join("refused", pins(&"cd".repeat(32))).await.unwrap().unwrap_err().to_string();
        assert!((err.contains("~364.9 days") || err.contains("~365.0 days")) && err.contains("TIMESTAMP_MS"), "{err}");
        // The joined home's first start reads it again (join.json keeps what the operator passed).
        let jc: JoinConfig = serde_json::from_slice(&std::fs::read(root.join("joined").join("join.json")).unwrap()).unwrap();
        assert_eq!(jc.pins, pins(&hash2));
        drop(chain);
        std::fs::remove_dir_all(root).ok();
    }

    /// Review of F18: slashing protection's network watch counts a peer's claim to be past this
    /// node only once the peer shows it: blocks after this node's tip, hash-linked to it, with a
    /// finalization certificate the epoch's set signed (`verify_batch`, which executes nothing).
    /// A certificate by anyone else, blocks that don't extend this node's chain, or a batch the
    /// certificate doesn't cover prove nothing.
    #[test]
    fn a_claim_to_be_ahead_counts_only_once_its_blocks_verify() {
        use commonware_codec::Encode as _;
        use commonware_consensus::simplex::types::{Finalize, Proposal};
        use commonware_consensus::types::{Epoch, Round, View};
        let root = home("probe");
        let homes = Chain::summon(&root, &SummonOpts::new("probe", 1)).unwrap();
        let g = Genesis::load(&homes[0].join("genesis.json")).unwrap();
        let seed: [u8; 32] = hex::decode(std::fs::read_to_string(homes[0].join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
        let chain: Shared = Arc::new(Mutex::new(Chain::open(&homes[0]).unwrap()));
        let root_hash = consensus::hex_32(&g.initial_state().root());
        let mut blocks = vec![consensus::genesis_block(&g)];
        for h in 1..=3u64 {
            let parent = blocks.last().unwrap().digest();
            blocks.push(CBlock::new(parent, h, h, h - 1, g.created_at_ms + 1000 * h, g.validators[0].address.clone(), 0, root_hash, vec![]));
        }
        let namespace = consensus::genesis_namespace(&g);
        let certify = |key: PrivateKey, set, block: &CBlock| {
            let scheme = MlDsaScheme::signer(&namespace, set, key).unwrap();
            let vote = Finalize::sign(&scheme, Proposal::new(Round::new(Epoch::new(0), View::new(block.height)), View::new(block.height - 1), block.digest())).unwrap();
            hex::encode(Finalization::from_finalizes(&scheme, commonware_utils::iter::NonEmpty::try_new([vote].iter()).unwrap(), &commonware_parallel::Sequential).unwrap().encode())
        };
        let batch = |from: usize, to: usize, fin: String| SyncBatch { tip: 3, blocks: blocks[from..=to].iter().map(|b| hex::encode(b.encode())).collect(), to: Some(to as u64), finalization: Some(fin) };
        let verifier = consensus::EpochProvider::new(chain.clone(), None).epoch_scheme(0).unwrap();
        let validator = || PrivateKey::from_seed_bytes(seed);
        let set = || consensus::participants(&g).unwrap();
        // Proven: blocks 1..=3, the last certified by the set.
        let (got, _) = verify_batch(&chain, &verifier, 0, &batch(1, 3, certify(validator(), set(), &blocks[3]))).unwrap();
        assert_eq!(got.last().unwrap().height, 3);
        assert_eq!(chain.lock().height(), 0, "nothing executed");
        // Not proven: a certificate by a key outside the set (in a set of its own), ...
        let forger = PrivateKey::from_seed_bytes([9; 32]);
        let forged_set = commonware_utils::ordered::Set::try_from(vec![commonware_cryptography::Signer::public_key(&forger)]).unwrap();
        let err = verify_batch(&chain, &verifier, 0, &batch(1, 3, certify(forger, forged_set, &blocks[3]))).unwrap_err().to_string();
        assert!(err.contains("invalid finalization certificate"), "{err}");
        // ... blocks that don't start at this node's tip, ...
        let err = verify_batch(&chain, &verifier, 0, &batch(2, 3, certify(validator(), set(), &blocks[3]))).unwrap_err().to_string();
        assert!(err.contains("does not extend our chain"), "{err}");
        // ... a certificate for another block, or for another epoch's set.
        let err = verify_batch(&chain, &verifier, 0, &batch(1, 3, certify(validator(), set(), &blocks[2]))).unwrap_err().to_string();
        assert!(err.contains("does not cover"), "{err}");
        assert!(verify_batch(&chain, &verifier, 1, &batch(1, 3, certify(validator(), set(), &blocks[3]))).is_err());
        drop(chain);
        std::fs::remove_dir_all(root).ok();
    }

    /// F27: the rotation benches a peer whose round delivered blocks slower than twice the
    /// chain's rate, or (in a round a block time long) far slower than another peer's recent best,
    /// or that took long to deliver nothing; skips it while benched (unless every peer is), and
    /// backs off more each time. After a good round it tries an untried peer, else keeps the
    /// fastest, and every `EXPLORE_EVERY` rounds the next one.
    #[test]
    fn slow_peers_are_benched_and_skipped() {
        let t0 = std::time::Instant::now();
        let second = Duration::from_secs(1);
        let mut r = Rotation::starting_at(3, 0);
        assert_eq!(r.progress(round(200, Duration::from_secs(20)), second, t0), None, "10 blocks a second: fine");
        assert_eq!(r.current(), 1, "an untried peer next");
        assert_eq!(r.progress(round(1, Duration::from_millis(400)), second, t0), None, "one block in 0.4 s keeps up (too short a round to judge against the best)");
        assert_eq!(r.current(), 2);
        assert_eq!(r.progress(round(1, Duration::from_millis(29_000)), second, t0), Some(SLOW_PEER_BENCH), "a trickle");
        assert_eq!(r.current(), 0);
        assert_eq!(r.progress(round(200, Duration::from_secs(20)), second, t0), None);
        assert_eq!(r.current(), 0, "the fastest recent peer goes on (2 is benched, 1 slower)");
        r.advance(t0);
        r.advance(t0);
        assert_eq!(r.current(), 0, "peer 2 is skipped while benched");
        r.advance(t0);
        assert_eq!(r.progress(round(3, Duration::from_secs(2)), second, t0), Some(SLOW_PEER_BENCH));
        assert_eq!(r.current(), 0);
        assert_eq!(r.progress(round(1, Duration::from_secs(1)), second, t0), Some(SLOW_PEER_BENCH));
        assert_eq!(r.current(), 1, "all benched: plain turns");
        assert_eq!(r.progress(round(1, Duration::from_secs(5)), second, t0), Some(2 * SLOW_PEER_BENCH), "twice in a row: longer");
        let later = t0 + SLOW_PEER_MAX;
        r.advance(later);
        assert_eq!(r.current(), 0, "benches run out");
        for _ in 0..20 {
            r.progress(round(1, Duration::from_secs(5)), second, later);
        }
        assert!(r.strikes.iter().all(|s| *s <= 21) && r.benched_until.iter().flatten().all(|t| *t <= later + SLOW_PEER_MAX));
        // Nothing new: at once is fine (the next peer's turn); after a long wait it's a stall.
        let mut r = Rotation::starting_at(3, 0);
        assert_eq!(r.nothing(Duration::from_millis(50), second, t0), None);
        assert_eq!(r.current(), 1);
        assert_eq!(r.nothing(Duration::from_secs(29), second, t0), Some(SLOW_PEER_BENCH));
        assert_eq!(r.current(), 2);
        r.advance(t0);
        assert_eq!(r.current(), 0, "the staller is skipped");
        // A failure benches at once: an unreachable peer isn't retried after every good round.
        let mut r = Rotation::starting_at(2, 0);
        assert_eq!(r.failed(t0), SLOW_PEER_BENCH);
        assert_eq!(r.current(), 1);
        for _ in 0..3 {
            assert_eq!(r.progress(round(200, Duration::from_millis(500)), second, t0), None);
            assert_eq!(r.current(), 1);
        }
        let mut r = Rotation::starting_at(2, 0);
        assert_eq!(r.progress(round(200, Duration::from_millis(500)), second, t0), None);
        assert_eq!(r.current(), 1, "an unasked peer next");
        assert_eq!(r.nothing(Duration::from_millis(10), second, t0), None);
        assert_eq!(r.current(), 0);
        assert_eq!(r.progress(round(200, Duration::from_millis(500)), second, t0), None);
        assert_eq!(r.current(), 0, "asked recently, and no rate to beat this one's: not asked again after every round");
        // The first peer asked is a random one, not always the list's first.
        let firsts: std::collections::BTreeSet<usize> = (0..64).map(|_| Rotation::new(4).current()).collect();
        assert!(firsts.len() > 1, "{firsts:?}");
    }

    /// review-r2-sync-1: the bar a round is judged against is what another peer really delivered.
    /// A tiny round answered at once counts as a quarter block time long, so it can't bench a
    /// full one; a real fast batch can, until its peer has nothing new (its rate goes, and with
    /// every free peer quiet the benched one is free and isn't judged against the old bar) or
    /// trickles far below the benched peer (which is free again); a full batch at an epoch's end
    /// or a trickler's rate frees nobody; and a peer is far slower only in blocks and in bytes.
    #[test]
    fn a_bar_is_only_what_a_peer_really_delivered() {
        let t0 = std::time::Instant::now();
        let second = Duration::from_secs(1);
        let ms = Duration::from_millis;
        // 1 block in 1 ms is 4 a second, not 1,000: a 200-block round of 1.2 s isn't below it.
        let mut r = Rotation::starting_at(2, 0);
        assert_eq!(r.progress(round(1, ms(1)), second, t0), None);
        assert_eq!(r.rates[0].map(|b| b.blocks), Some(4.0));
        assert_eq!(r.current(), 1, "the untried peer");
        assert_eq!(r.progress(round(200, ms(1_200)), second, t0), None);
        // 200 in 10 ms (800 a second over a quarter block time) benches a 4 s round (50 a second).
        let fast_then_slow = || {
            let mut r = Rotation::starting_at(2, 0);
            assert_eq!(r.progress(round(200, ms(10)), second, t0), None);
            assert_eq!(r.progress(round(200, ms(4_000)), second, t0), Some(SLOW_PEER_BENCH));
            assert_eq!((r.current(), r.relative[1]), (0, true), "benched for being slower");
            r
        };
        let mut r = fast_then_slow();
        assert_eq!(r.nothing(ms(1), second, t0), None);
        assert!(r.rates[0].is_none(), "a peer with nothing new is no bar");
        assert_eq!(r.current(), 1, "every free peer has nothing: the slower one is free again");
        assert_eq!(r.progress(round(200, ms(4_000)), second, t0), None, "and no old bar benches it");
        // A trickle far below the benched peer's rate frees it, and the faster one goes on.
        let mut r = fast_then_slow();
        assert_eq!(r.progress(round(1, ms(490)), second, t0), None);
        assert_eq!((r.current(), r.benched(1, t0)), (1, false));
        // Not a full batch, however small (the epoch's last blocks) ...
        let mut r = fast_then_slow();
        assert_eq!(r.progress(Round { blocks: 3, bytes: 3 * BLOCK_BYTES, limit: 3, took: ms(200) }, second, t0), None);
        assert!(r.benched(1, t0));
        // ... nor a short round no slower than the benched peer's rate.
        assert_eq!(r.progress(round(20, ms(200)), second, t0), None);
        assert!(r.benched(1, t0));
        // A round a block time long far below the benched peer's rate is benched in its turn.
        let mut r = fast_then_slow();
        assert_eq!(r.progress(round(2, ms(1_000)), second, t0), Some(SLOW_PEER_BENCH), "2 a second against the benched peer's 50");
        // Far slower in blocks but not in bytes (bigger blocks) isn't far slower.
        let mut r = Rotation::starting_at(2, 0);
        r.progress(round(200, ms(10)), second, t0);
        let big = Round { blocks: 20, bytes: 20 * 100 * BLOCK_BYTES, limit: 200, took: ms(2_000) };
        assert_eq!(r.progress(big, second, t0), None);
        // A failed or stalled peer's rate is no bar.
        let mut r = Rotation::starting_at(2, 0);
        r.progress(round(200, ms(10)), second, t0);
        assert_eq!(r.progress(round(200, ms(1_000)), second, t0), None, "200 a second is a quarter of 800: not far below");
        assert_eq!(r.current(), 0, "the faster one goes on");
        r.failed(t0);
        assert_eq!(r.current(), 1);
        assert_eq!(r.progress(round(200, ms(4_000)), second, t0), None);
    }

    /// F27 (R1 bypass): a peer delivering blocks just over twice the chain's rate passes the
    /// chain-rate floor, but not the comparison with a much faster peer. It gets a first round
    /// (nothing to compare with yet) and the exploration round, then it's benched; and a peer
    /// unheard from for `RATE_WINDOW` counts as untried again.
    #[test]
    fn a_peer_just_above_the_chain_rate_loses_to_a_fast_one() {
        let t0 = std::time::Instant::now();
        let second = Duration::from_secs(1);
        let trickle = |r: &mut Rotation| r.progress(round(4, Duration::from_millis(1_800)), second, t0);
        let honest = |r: &mut Rotation| r.progress(round(200, Duration::from_millis(500)), second, t0);
        let mut r = Rotation::starting_at(2, 0);
        assert_eq!(trickle(&mut r), None, "2.2 blocks a second, over twice the chain's 1");
        assert_eq!(r.current(), 1, "the untried peer next");
        let mut honest_rounds = 0;
        while r.good_rounds + 1 < EXPLORE_EVERY {
            assert_eq!(honest(&mut r), None);
            assert_eq!(r.current(), 1, "the fastest goes on");
            honest_rounds += 1;
        }
        assert_eq!(honest(&mut r), None);
        assert_eq!((r.current(), honest_rounds + 1), (0, EXPLORE_EVERY - 1), "the exploration round");
        assert_eq!(trickle(&mut r), Some(SLOW_PEER_BENCH), "a block time or more for 4 blocks, against 400 a second");
        assert_eq!(r.current(), 1);
        for _ in 0..3 * EXPLORE_EVERY {
            assert_eq!(honest(&mut r), None);
            assert_eq!(r.current(), 1, "the benched peer isn't explored");
        }
        // Once the honest rate is old, the next good round tries the other peer again.
        let mut r = Rotation::starting_at(2, 1);
        r.progress(round(200, Duration::from_millis(500)), second, t0);
        assert_eq!(r.current(), 0);
        r.progress(round(200, Duration::from_millis(500)), second, t0);
        assert_eq!(r.current(), 1);
        r.progress(round(200, Duration::from_millis(500)), second, t0 + RATE_WINDOW + second);
        assert_eq!(r.current(), 0, "peer 0's rate is stale: untried");
    }

    /// What a simulated peer does with one `/api/sync` request.
    enum Answer {
        /// This many blocks (of `BLOCK_BYTES` each), sent in this long.
        Blocks(u64, Duration),
        /// "Nothing new", after this long.
        Nothing(Duration),
        /// An error, after this long.
        Fail(Duration),
    }
    const BLOCK_BYTES: u64 = 2_000;

    /// What a simulated peer sees of the node's rotation when it's asked: the worst case, an
    /// attacker that knows how long each peer stays benched.
    struct Seen {
        benched: Vec<Duration>,
    }
    type Peer = Box<dyn FnMut(u64, u64, u64, &Seen) -> Answer>;

    /// A round of `blocks` blocks of `BLOCK_BYTES` each, out of 200 asked for, sent in `took`.
    fn round(blocks: u64, took: Duration) -> Round {
        Round { blocks, bytes: blocks * BLOCK_BYTES, limit: 200, took }
    }

    /// Drives the real [`Rotation`] the way `sync_loop` does, in virtual time (1 s blocks, mainnet
    /// epochs, batches of at most 200 that never cross an epoch): no pause after a round that
    /// delivered, `block_time / peers` after "nothing new". `peers[i](from, limit, tip, now)`
    /// answers a request. Starts `behind` blocks behind at peer `start`; returns how far behind
    /// the node is after `run`, and how often each peer was asked.
    fn simulate(peers: &mut [Peer], behind: u64, start: usize, run: Duration) -> (u64, Vec<usize>) {
        let block_time = Duration::from_secs(1);
        let epoch = 3_600u64;
        let t0 = std::time::Instant::now();
        let mut r = Rotation::starting_at(peers.len(), start);
        let (mut now, mut height) = (Duration::ZERO, 10_000u64);
        let tip = |now: Duration| 10_000 + behind + now.as_secs();
        let mut asked = vec![0; peers.len()];
        while now < run {
            let i = r.current();
            asked[i] += 1;
            let from = height + 1;
            let limit = ((from / epoch + 1) * epoch - from).min(200);
            let seen = Seen { benched: r.benched_until.iter().map(|b| b.map_or(Duration::ZERO, |t| t.saturating_duration_since(t0 + now))).collect() };
            match peers[i](from, limit, tip(now), &seen) {
                Answer::Blocks(n, took) => {
                    let n = n.min(tip(now).saturating_sub(height)).min(limit);
                    now += took;
                    height += n;
                    if n > 0 {
                        r.progress(Round { blocks: n, bytes: n * BLOCK_BYTES, limit, took }, block_time, t0 + now);
                    } else {
                        r.nothing(took, block_time, t0 + now);
                        now += block_time / peers.len() as u32;
                    }
                }
                Answer::Nothing(took) => {
                    now += took;
                    r.nothing(took, block_time, t0 + now);
                    now += block_time / peers.len() as u32;
                }
                Answer::Fail(took) => {
                    now += took;
                    r.failed(t0 + now);
                    now += Duration::from_millis(500);
                }
            }
        }
        (tip(now) - height, asked)
    }

    /// An honest peer: everything asked for that exists, in `rtt` plus `per_block` a block.
    fn honest(rtt: Duration, per_block: Duration) -> Peer {
        Box::new(move |from, limit, tip, _| match tip.checked_sub(from) {
            Some(more) => {
                let n = (more + 1).min(limit);
                Answer::Blocks(n, rtt + per_block * n as u32)
            }
            None => Answer::Nothing(rtt),
        })
    }

    /// review-r2-sync-1: a colocated peer answers one round in 50 with a single block in 1 ms
    /// (1,000 blocks a second, as R2 measured it) and every other round "nothing new" in 1 ms,
    /// against an honest peer whose full rounds take a block time or more (0.2 s + 4 ms a block,
    /// 1 s for 200). Its tiny round must not become the bar that benches the honest peer, and its
    /// instant empty answers must not keep the turn while the honest peer has blocks: the node
    /// keeps up from any lag, from either peer. The same with the R2 trickler (4 blocks a round,
    /// 450 ms each) in place of the liar, and with both.
    #[test]
    fn a_fast_tiny_round_then_empty_answers_does_not_capture_sync() {
        let liar = || -> Peer {
            let mut round = 0u64;
            Box::new(move |_, _, _, _| {
                round += 1;
                if round % 50 == 1 {
                    Answer::Blocks(1, Duration::from_millis(1))
                } else {
                    Answer::Nothing(Duration::from_millis(1))
                }
            })
        };
        let trickler = || -> Peer { Box::new(|_, limit, _, _| Answer::Blocks(limit.min(4), Duration::from_millis(450 * limit.min(4)))) };
        let slow_honest = || honest(Duration::from_millis(200), Duration::from_millis(4));
        for behind in [600, 3_600, 86_400] {
            for start in [0, 1] {
                let (lag, asked) = simulate(&mut [liar(), slow_honest()], behind, start, Duration::from_secs(3 * 3_600));
                assert!(lag <= 2, "{behind} behind, starting at {start}: {lag} behind after 3 h; asked {asked:?}");
                let (lag, asked) = simulate(&mut [trickler(), slow_honest()], behind, start, Duration::from_secs(3 * 3_600));
                assert!(lag <= 2, "trickler, {behind} behind, starting at {start}: {lag} behind after 3 h; asked {asked:?}");
            }
            let (lag, asked) = simulate(&mut [liar(), trickler(), slow_honest()], behind, 0, Duration::from_secs(3 * 3_600));
            assert!(lag <= 3, "liar and trickler, {behind} behind: {lag} behind after 3 h; asked {asked:?}");
        }
    }

    /// review-r2-sync-1, the variants, against a slow honest peer (a 200-block round takes 4 s,
    /// 50 blocks a second) and hostile peers that see the rotation's state. From a second before
    /// the honest peer's bench ends they send just enough real blocks to keep the turn and bench
    /// it again when it's tried, for longer each time: 51 blocks in 240 ms, 4× its pace and a
    /// little more. While it's benched they trickle single blocks just over twice the chain's rate (one in 490 ms, too short a
    /// round to judge), answer "nothing new" at once, fail at once, or mix those; one such peer,
    /// or two. None of them keeps the node from catching up a day of blocks within an hour (the
    /// honest peer alone takes about half an hour): a trickle far below the benched peer's rate
    /// frees it, and so does every other peer having nothing. (With 6a338b6's rules, or without
    /// either way back, the node is tens of thousands of blocks behind after the hour.)
    #[test]
    fn hostile_peers_cant_hold_the_turn_after_benching_an_honest_one() {
        let smart = |honest: usize, then: fn(u64) -> Answer| -> Peer {
            let mut round = 0u64;
            Box::new(move |_, limit, _, seen: &Seen| {
                round += 1;
                if seen.benched[honest] > Duration::from_secs(1) {
                    then(round)
                } else {
                    Answer::Blocks(limit.min(51), Duration::from_millis(240))
                }
            })
        };
        let trickle = |_| Answer::Blocks(1, Duration::from_millis(490));
        let nothing = |_| Answer::Nothing(Duration::from_millis(1));
        let fail = |_| Answer::Fail(Duration::from_millis(1));
        let mixed = |round: u64| match round % 3 {
            0 => Answer::Blocks(1, Duration::from_millis(490)),
            1 => Answer::Nothing(Duration::from_millis(1)),
            _ => Answer::Fail(Duration::from_millis(1)),
        };
        let slow_honest = || honest(Duration::from_millis(200), Duration::from_millis(19));
        let day = 86_400;
        let (alone, _) = simulate(&mut [slow_honest()], day, 0, Duration::from_secs(30 * 60));
        assert!(alone < 1_000, "the honest peer alone: {alone} behind after half an hour");
        for (name, then) in [("trickle", trickle as fn(u64) -> Answer), ("nothing", nothing), ("fail", fail), ("mixed", mixed)] {
            for start in [0, 1] {
                let (lag, asked) = simulate(&mut [smart(1, then), slow_honest()], day, start, Duration::from_secs(3_600));
                assert!(lag <= 2, "{name}, starting at {start}: {lag} behind after an hour; asked {asked:?}");
            }
            let (lag, asked) = simulate(&mut [smart(2, then), smart(2, then), slow_honest()], day, 2, Duration::from_secs(3_600));
            assert!(lag <= 3, "two {name}: {lag} behind after an hour; asked {asked:?}");
        }
    }

    /// F27: a peer that serves real, certified blocks just slower than the chain gets one round,
    /// and so (R1's bypass) does one just over twice the chain's rate, 4 blocks per round at
    /// 450 ms each, against an honest peer serving 5 blocks per round at once: it's benched by
    /// comparison after at most its first and its exploration round, the honest peer brings the
    /// node up to its tip at about its own pace, and catching up ends. Whichever peer is asked
    /// first (a random one).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_trickling_peer_does_not_capture_sync() {
        use axum::extract::{Path as AxPath, Query, State as AxState};
        use axum::{routing::get, Router};
        use commonware_codec::Encode as _;
        use commonware_consensus::simplex::types::{Finalize, Proposal};
        use commonware_consensus::types::{Epoch, Round, View};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = home("trickle");
        let homes = Chain::summon(&root, &SummonOpts::new("trickle", 1)).unwrap();
        let g = Genesis::load(&homes[0].join("genesis.json")).unwrap();
        let seed: [u8; 32] = hex::decode(std::fs::read_to_string(homes[0].join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
        const TIP: u64 = 60;
        let root_hash = consensus::hex_32(&g.initial_state().root());
        let mut blocks = vec![consensus::genesis_block(&g)];
        for h in 1..=TIP {
            let parent = blocks.last().unwrap().digest();
            blocks.push(CBlock::new(parent, h, h, h - 1, g.created_at_ms + 1000 * h, g.validators[0].address.clone(), 0, root_hash, vec![]));
        }
        let scheme = Arc::new(MlDsaScheme::signer(&consensus::genesis_namespace(&g), consensus::participants(&g).unwrap(), PrivateKey::from_seed_bytes(seed)).unwrap());
        #[derive(Clone)]
        struct Peer {
            blocks: Arc<Vec<CBlock>>,
            scheme: Arc<MlDsaScheme>,
            hits: Arc<[AtomicUsize; 2]>,
        }
        let peer = Peer { blocks: Arc::new(blocks), scheme, hits: Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]) };
        async fn serve(AxState(p): AxState<Peer>, AxPath(who): AxPath<String>, Query(q): Query<std::collections::HashMap<String, u64>>) -> axum::Json<serde_json::Value> {
            let honest = who == "honest";
            let from = q["from"];
            // Rounds with blocks to give. Once the node is at the tip and the honest peer says
            // "nothing new", a peer benched only for being slower is asked again (review-r2-sync-1)
            // and says "nothing new" at once: that probe isn't a captured round.
            if from <= TIP {
                p.hits[usize::from(honest)].fetch_add(1, Ordering::SeqCst);
            }
            // "slow": one block per 0.7 s, slower than twice the chain's rate; "trickle": 4 per
            // round at 0.45 s each, just over it; "honest": 5 per round, at once.
            let (per_round, per_block) = match who.as_str() {
                "slow" => (1, 700),
                "trickle" => (4, 450),
                _ => (5, 0),
            };
            let to = (from + q["limit"].min(per_round) - 1).min(TIP);
            if from <= TIP {
                tokio::time::sleep(Duration::from_millis(per_block * (to - from + 1))).await;
            }
            if from > TIP {
                return axum::Json(serde_json::json!({ "tip": TIP, "blocks": [], "to": null, "finalization": null }));
            }
            let last = &p.blocks[to as usize];
            let vote = Finalize::sign(&*p.scheme, Proposal::new(Round::new(Epoch::new(0), View::new(to)), View::new(to - 1), last.digest())).unwrap();
            let fin = Finalization::from_finalizes(&*p.scheme, commonware_utils::iter::NonEmpty::try_new([vote].iter()).unwrap(), &commonware_parallel::Sequential).unwrap();
            let blocks: Vec<String> = (from..=to).map(|h| hex::encode(p.blocks[h as usize].encode())).collect();
            axum::Json(serde_json::json!({ "tip": TIP, "blocks": blocks, "to": to, "finalization": hex::encode(fin.encode()) }))
        }
        let app = Router::new().route("/{who}/api/sync", get(serve)).with_state(peer.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let trust = Arc::new(Trust::new(&g, &Pins::default(), false).unwrap());
        for (who, most) in [("slow", 1), ("trickle", 2)] {
            let full = root.join(format!("full-{who}"));
            std::fs::create_dir_all(&full).unwrap();
            std::fs::copy(homes[0].join("genesis.json"), full.join("genesis.json")).unwrap();
            let chain: Shared = Arc::new(Mutex::new(Chain::open(&full).unwrap()));
            for h in peer.hits.iter() {
                h.store(0, Ordering::SeqCst);
            }
            let started = std::time::Instant::now();
            let peers = vec![format!("http://{addr}/{who}"), format!("http://{addr}/honest")];
            tokio::time::timeout(Duration::from_secs(40), catch_up(chain.clone(), peers, trust.clone())).await.expect("catching up ends");
            assert_eq!(chain.lock().height(), TIP);
            let (trickled, honest) = (peer.hits[0].load(Ordering::SeqCst), peer.hits[1].load(Ordering::SeqCst));
            assert!(trickled <= most, "{who}: {trickled} rounds ({honest} to the honest peer)");
            // Alone, the trickler would take 15 rounds of 1.8 s (27 s of its sleeps, whatever the
            // load); the honest peer takes 12 quick ones, and executing the 60 blocks the rest
            // (~4 s here unloaded, more under a parallel test run).
            assert!(started.elapsed() < Duration::from_secs(25), "{who}: {:?}", started.elapsed());
            drop(chain);
        }
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
        // Claims never move our finalized tip; only verified certificates do. A claim whose
        // answer didn't check out isn't even shown, and none holds back validator duties (F11).
        assert_eq!(chain.lock().finalized_tip, 0);
        assert_eq!(chain.lock().network_height_claimed(), 0);
        assert!(!chain.lock().syncing());
        std::fs::remove_dir_all(root).ok();
    }
}
