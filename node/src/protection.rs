//! Slashing protection (MAINNET-DESIGN 3.9, OPS-6): what keeps a validator from signing two
//! conflicting votes, including after its disk was restored from an old snapshot.
//!
//! Every vote signature passes [`Protection::check_and_record`] first (`pq::MlDsaScheme::sign`),
//! which applies three rules:
//!
//! 1. **The record** ([`RECORD_FILE`], next to `validator.key`): for the newest epoch this key
//!    signed in, the last [`RECORD_VIEWS`] views' notarize and finalize messages (hashed) and
//!    whether it nullified. It refuses what simplex would report as equivocation (another
//!    notarize or finalize message in a view, a finalize after a nullify, a nullify after a
//!    finalize), any vote in an older epoch, and views below the window. Signing the identical
//!    message again is fine. A new vote is written and fsynced before its signature exists.
//! 2. **Observe before signing.** A restored volume rolls the record (and simplex's journal) back
//!    with it, so the record can't be the real defense. After every start the node signs nothing
//!    in its first epoch until it has watched the network finish [`Timing::views`] more views
//!    than it saw once the startup settled, and from then on only views above every certificate
//!    it saw. An earlier instance stopped before this one started, and the network had certified
//!    the view before its last vote by then, so none of its votes can be in a view this instance
//!    signs. Later epochs need no watching: the earlier instance never reached them. If the
//!    network makes no progress without this node (a halted chain, or every validator restarting
//!    at once), it signs again after [`Timing::escape_ms`], relying on the record: the one case a
//!    restored disk could still double-sign, which is why OPERATIONS.md says never to restore one.
//! 3. **Doppelganger detection.** A vote or certificate carrying this key's signature on a
//!    message the record doesn't have, in a view an earlier, stopped instance can't have reached,
//!    means another instance is signing with this key. The node then signs nothing more and halts
//!    (`upgrade::Halt::DoubleSignRisk`, exit code 12).
//!
//! The network is observed through simplex's reporter ([`Observer`]): only certificates (quorum
//! proofs, verified) move the views this node waits for, so no single peer can silence it.

use crate::pq::MlDsaScheme;
use anyhow::{bail, Context, Result};
use commonware_consensus::{
    simplex::types::{Activity, Attributable as _, Subject},
    types::Round,
    Reporter,
};
use commonware_cryptography::{blake3::Digest, certificate::Subject as _};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{error, info, warn};

/// The record's file in a validator home.
pub const RECORD_FILE: &str = "slashing-protection.json";
const FORMAT: &str = "blockula-slashing-protection-v1";
/// Views of the newest epoch the record keeps. Simplex votes only in views near its current one;
/// a vote below the window is refused (a missed vote, never a double-sign).
pub const RECORD_VIEWS: usize = 32;

/// The three simplex votes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Notarize,
    Nullify,
    Finalize,
}

/// What this key signed in one view: the hex BLAKE3 of each message, and whether it nullified
/// (a nullify's message is the round itself).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signed {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notarize: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub nullify: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalize: Option<String>,
}

impl Signed {
    fn has(&self, kind: Kind, hash: &str) -> bool {
        match kind {
            Kind::Notarize => self.notarize.as_deref() == Some(hash),
            Kind::Nullify => self.nullify,
            Kind::Finalize => self.finalize.as_deref() == Some(hash),
        }
    }
}

/// The slashing-protection record ([`RECORD_FILE`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub format: String,
    /// Hex BLAKE3 of the consensus public key this record protects.
    pub key_id: String,
    /// The newest epoch this key signed in; older epochs are refused.
    pub epoch: u64,
    /// Views up to this one (in `epoch`) are refused: they fell out of the window.
    pub floor: u64,
    /// What it signed, by view (the newest `RECORD_VIEWS` views of `epoch`).
    pub votes: BTreeMap<u64, Signed>,
}

impl Record {
    fn new(key_id: String) -> Self {
        Self { format: FORMAT.into(), key_id, epoch: 0, floor: 0, votes: BTreeMap::new() }
    }

    /// The record after signing `kind` over a message hashing to `hash` in `(epoch, view)`:
    /// `Ok(None)` if it holds that already, `Err` (why) if it conflicts.
    fn with(&self, epoch: u64, view: u64, kind: Kind, hash: &str) -> Result<Option<Record>, &'static str> {
        let mut r = if epoch > self.epoch { Record { epoch, floor: 0, votes: BTreeMap::new(), ..self.clone() } } else { self.clone() };
        if epoch < r.epoch {
            return Err("an older epoch than the record's");
        }
        if view <= r.floor {
            return Err("below the record's window");
        }
        let s = r.votes.entry(view).or_default();
        if s.has(kind, hash) {
            return Ok(None);
        }
        match kind {
            Kind::Notarize if s.notarize.is_some() => return Err("another notarize in this view"),
            Kind::Notarize => s.notarize = Some(hash.to_string()),
            Kind::Nullify if s.finalize.is_some() => return Err("a nullify after a finalize"),
            Kind::Nullify => s.nullify = true,
            Kind::Finalize if s.nullify => return Err("a finalize after a nullify"),
            Kind::Finalize if s.finalize.is_some() => return Err("another finalize in this view"),
            Kind::Finalize => s.finalize = Some(hash.to_string()),
        }
        while r.votes.len() > RECORD_VIEWS {
            if let Some((v, _)) = r.votes.pop_first() {
                r.floor = r.floor.max(v);
            }
        }
        Ok(Some(r))
    }

    /// The newest view signed in `epoch`.
    pub fn last_view(&self) -> u64 {
        self.votes.keys().next_back().copied().unwrap_or(self.floor)
    }
}

/// How long the startup rule watches the network (derived from the block time).
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// After starting to watch an epoch, the newest certificate seen this long later is the
    /// baseline: by then the network's latest certificates have arrived (each view broadcasts one).
    pub settle_ms: u64,
    /// The least time spent watching.
    pub watch_ms: u64,
    /// With no progress for this long, the network needs this node: sign again on the record alone.
    pub escape_ms: u64,
    /// Views the network must finish past the baseline before this node signs.
    pub views: u64,
}

impl Timing {
    pub fn for_block_time(block_time_ms: u64) -> Self {
        Self { settle_ms: block_time_ms + 1_000, watch_ms: 3 * block_time_ms + 2_000, escape_ms: 10 * block_time_ms + 10_000, views: 2 }
    }
}

/// The startup rule's state for one epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gate {
    /// Watching the network since `since` (ms); `baseline` is the newest certificate view seen
    /// once the startup settled.
    Watching { since: u64, baseline: Option<u64> },
    /// Signing views above `above`.
    Open { above: u64 },
}

/// What `/metrics` shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// The newest (epoch, view) this key signed in.
    pub epoch: u64,
    pub view: u64,
    /// In the active set of the newest epoch it entered.
    pub voting: bool,
    /// Watching the network before signing (after a start).
    pub abstaining: bool,
    /// Signatures refused since the start.
    pub refused: u64,
}

struct Inner {
    record: Record,
    gates: BTreeMap<u64, Gate>,
    /// A gate opened in this run: later epochs need no watching.
    current: bool,
    /// Newest certificate view seen per epoch.
    observed: BTreeMap<u64, u64>,
    /// Newest epoch entered or seen certified.
    newest_epoch: u64,
    /// This key's signer index per epoch (`None`: not in the set).
    me: BTreeMap<u64, Option<u32>>,
    now: u64,
    halt: Option<String>,
    refused: u64,
    /// Every signature released in this run (tests).
    #[cfg(test)]
    log: Vec<(u64, u64, Kind, String)>,
}

/// One validator key's slashing protection (shared by the signing scheme and the observer).
pub struct Protection {
    /// `None`: kept in memory only (tests).
    path: Option<PathBuf>,
    /// fsync every write (off in the simulator, whose disks don't crash).
    durable: bool,
    timing: Timing,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for Protection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Protection").field("path", &self.path).finish_non_exhaustive()
    }
}

impl Protection {
    /// Loads (or starts) the record at `path` for the consensus key `public_key`. A record for
    /// another key, or one that doesn't parse, stops the node: guessing could double-sign.
    pub fn open(path: &Path, public_key: &[u8], timing: Timing, durable: bool) -> Result<Arc<Self>> {
        let key_id = hex::encode(crate::crypto::hash(public_key));
        let record = match std::fs::read(path) {
            Ok(bytes) => {
                let r: Record = serde_json::from_slice(&bytes).with_context(|| format!("{} isn't a valid slashing-protection record; move it aside only if this key never signed elsewhere (docs/OPERATIONS.md)", path.display()))?;
                if r.format != FORMAT {
                    bail!("{} has format {}, this binary reads {FORMAT}", path.display(), r.format);
                }
                if r.key_id != key_id {
                    bail!("{} protects key {}, not this validator.key ({key_id})", path.display(), r.key_id);
                }
                r
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Record::new(key_id),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Arc::new(Self::with_record(Some(path.to_path_buf()), durable, timing, record)))
    }

    /// Protection kept in memory only.
    #[cfg(test)]
    pub fn in_memory(public_key: &[u8], timing: Timing) -> Arc<Self> {
        Arc::new(Self::with_record(None, false, timing, Record::new(hex::encode(crate::crypto::hash(public_key)))))
    }

    fn with_record(path: Option<PathBuf>, durable: bool, timing: Timing, record: Record) -> Self {
        let inner = Inner {
            record,
            gates: BTreeMap::new(),
            current: false,
            observed: BTreeMap::new(),
            newest_epoch: 0,
            me: BTreeMap::new(),
            now: 0,
            halt: None,
            refused: 0,
            #[cfg(test)]
            log: vec![],
        };
        Self { path, durable, timing, inner: Mutex::new(inner) }
    }

    /// A copy of the record as it stands.
    pub fn record(&self) -> Record {
        self.inner.lock().record.clone()
    }

    /// Why this node must stop (another instance signs with its key), once it must.
    pub fn halted(&self) -> Option<String> {
        self.inner.lock().halt.clone()
    }

    pub fn status(&self) -> Status {
        let g = self.inner.lock();
        let e = g.newest_epoch;
        let voting = g.me.get(&e).copied().flatten().is_some();
        Status {
            epoch: g.record.epoch,
            view: g.record.last_view(),
            voting,
            abstaining: voting && matches!(g.gates.get(&e), Some(Gate::Watching { .. })),
            refused: g.refused,
        }
    }

    /// The node entered `epoch` (index `me` in a set of `participants`) at `now_ms`. The first
    /// epoch of a run watches the network before signing; later ones, and a set of one (nobody
    /// else to watch), don't.
    pub fn enter_epoch(&self, epoch: u64, me: Option<u32>, participants: usize, now_ms: u64) {
        let mut g = self.inner.lock();
        g.now = g.now.max(now_ms);
        g.me.insert(epoch, me);
        g.newest_epoch = g.newest_epoch.max(epoch);
        if !g.gates.contains_key(&epoch) {
            let gate = if g.current || participants <= 1 { Gate::Open { above: 0 } } else { Gate::Watching { since: now_ms, baseline: None } };
            if matches!(gate, Gate::Watching { .. }) && me.is_some() {
                info!(epoch, "slashing protection: watching the network before signing (restart rule)");
            }
            g.gates.insert(epoch, gate);
        }
        // Old epochs' bookkeeping isn't needed any more.
        let keep = epoch.saturating_sub(4);
        g.gates.retain(|e, _| *e >= keep);
        g.observed.retain(|e, _| *e >= keep);
        g.me.retain(|e, _| *e >= keep);
    }

    /// Advances the clock the startup rule runs on (a runtime task calls it a few times a second).
    pub fn tick(&self, now_ms: u64) {
        let t = self.timing;
        let mut g = self.inner.lock();
        g.now = g.now.max(now_ms);
        let now = g.now;
        let watching: Vec<(u64, u64, Option<u64>)> = g.gates.iter().filter_map(|(e, gate)| if let Gate::Watching { since, baseline } = gate { Some((*e, *since, *baseline)) } else { None }).collect();
        for (epoch, since, baseline) in watching {
            let seen = g.observed.get(&epoch).copied();
            let baseline = baseline.or(if now >= since + t.settle_ms { seen } else { None });
            let open = match (baseline, seen) {
                (Some(b), Some(m)) if now >= since + t.watch_ms && m >= b + t.views => Some(m),
                _ if now >= since + t.escape_ms => {
                    warn!(epoch, "slashing protection: the network made no progress without this node; signing again on the local record alone");
                    Some(seen.unwrap_or(0))
                }
                _ => None,
            };
            let gate = match open {
                Some(above) => {
                    info!(epoch, above, "slashing protection: signing views above {above}");
                    g.current = true;
                    Gate::Open { above }
                }
                None => Gate::Watching { since, baseline },
            };
            g.gates.insert(epoch, gate);
        }
    }

    /// The signing hook: `true` if this key may sign `kind` over `message` in `(epoch, view)`.
    /// A new vote is in the record (and on disk, fsynced) before this returns `true`.
    pub fn check_and_record(&self, epoch: u64, view: u64, kind: Kind, message: &[u8]) -> bool {
        let hash = hex::encode(crate::crypto::hash(message));
        let mut g = self.inner.lock();
        match g.permit(epoch, view, kind, &hash) {
            Ok(None) => true,
            Ok(Some(next)) => {
                if let Err(e) = self.persist(&next) {
                    error!(error = %e, "slashing protection: couldn't write the record; not signing");
                    g.refused += 1;
                    return false;
                }
                g.record = next;
                #[cfg(test)]
                g.log.push((epoch, view, kind, hash));
                true
            }
            Err(why) => {
                g.refused += 1;
                warn!(epoch, view, ?kind, why, "slashing protection refused a signature");
                false
            }
        }
    }

    fn persist(&self, r: &Record) -> Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec(r)?)?;
            if self.durable {
                f.sync_all()?;
            }
        }
        std::fs::rename(&tmp, path)?;
        if self.durable {
            if let Some(dir) = path.parent() {
                std::fs::File::open(dir)?.sync_all()?;
            }
        }
        Ok(())
    }

    /// A certificate for `(round, kind)` over `message`, signed by `signers` (indices).
    fn observe_certificate(&self, round: Round, kind: Kind, message: impl FnOnce() -> Vec<u8>, signers: impl Iterator<Item = u32>) {
        let (epoch, view) = (round.epoch().get(), round.view().get());
        let mut g = self.inner.lock();
        let seen = g.observed.entry(epoch).or_default();
        *seen = (*seen).max(view);
        g.newest_epoch = g.newest_epoch.max(epoch);
        let me = g.me.get(&epoch).copied().flatten();
        if me.is_some_and(|i| signers.into_iter().any(|s| s == i)) {
            g.check_foreign(epoch, view, kind, &hex::encode(crate::crypto::hash(&message())), self.timing);
        }
    }

    /// A vote for `(round, kind)` over `message` by signer `signer`.
    fn observe_vote(&self, round: Round, kind: Kind, message: impl FnOnce() -> Vec<u8>, signer: u32) {
        let (epoch, view) = (round.epoch().get(), round.view().get());
        let mut g = self.inner.lock();
        if g.me.get(&epoch).copied().flatten() == Some(signer) {
            g.check_foreign(epoch, view, kind, &hex::encode(crate::crypto::hash(&message())), self.timing);
        }
    }

    /// Everything released in this run, in order (tests).
    #[cfg(test)]
    pub fn log(&self) -> Vec<(u64, u64, Kind, String)> {
        self.inner.lock().log.clone()
    }
}

impl Inner {
    fn permit(&self, epoch: u64, view: u64, kind: Kind, hash: &str) -> Result<Option<Record>, &'static str> {
        if self.halt.is_some() {
            return Err("another instance signs with this key");
        }
        if epoch < self.newest_epoch {
            return Err("a later epoch is under way");
        }
        match self.gates.get(&epoch) {
            Some(Gate::Open { above }) if view <= *above => return Err("at or below views the network certified before this node signed again"),
            Some(Gate::Watching { .. }) => return Err("watching the network after a start"),
            None if !self.current => return Err("watching the network after a start"),
            _ => {}
        }
        self.record.with(epoch, view, kind, hash)
    }

    /// A signature by this key that the record doesn't have, in a view an earlier instance
    /// (stopped before this one started) can't have reached: another instance is running.
    fn check_foreign(&mut self, epoch: u64, view: u64, kind: Kind, hash: &str, t: Timing) {
        if self.halt.is_some() || epoch < self.record.epoch {
            return;
        }
        if epoch == self.record.epoch && (view <= self.record.floor || self.record.votes.get(&view).is_some_and(|s| s.has(kind, hash))) {
            return;
        }
        let foreign = match self.gates.get(&epoch) {
            Some(Gate::Open { above }) => view > *above,
            Some(Gate::Watching { baseline: Some(b), .. }) => view > b + t.views,
            _ => false,
        };
        if foreign {
            let why = format!("a {kind:?} vote in epoch {epoch}, view {view} carries this validator's signature, but this process never signed it: another instance is running with this validator.key");
            error!("{why}");
            self.halt = Some(why);
        }
    }
}

/// Feeds simplex's activity (certificates, and votes under this key) to [`Protection`].
#[derive(Clone)]
pub struct Observer(pub Arc<Protection>);

impl Reporter for Observer {
    type Activity = Activity<MlDsaScheme, Digest>;

    fn report(&mut self, activity: Self::Activity) -> commonware_actor::Feedback {
        let signers = |c: &crate::pq::Certificate| c.signers.iter().map(|s| usize::from(s) as u32).collect::<Vec<_>>();
        let p = &self.0;
        match &activity {
            Activity::Notarization(n) => p.observe_certificate(n.proposal.round, Kind::Notarize, || Subject::Notarize { proposal: &n.proposal }.message().to_vec(), signers(&n.certificate).into_iter()),
            Activity::Nullification(n) => p.observe_certificate(n.round, Kind::Nullify, || Subject::<Digest>::Nullify { round: n.round }.message().to_vec(), signers(&n.certificate).into_iter()),
            Activity::Finalization(f) => p.observe_certificate(f.proposal.round, Kind::Finalize, || Subject::Finalize { proposal: &f.proposal }.message().to_vec(), signers(&f.certificate).into_iter()),
            Activity::Notarize(v) => p.observe_vote(v.proposal.round, Kind::Notarize, || Subject::Notarize { proposal: &v.proposal }.message().to_vec(), usize::from(v.signer()) as u32),
            Activity::Nullify(v) => p.observe_vote(v.round, Kind::Nullify, || Subject::<Digest>::Nullify { round: v.round }.message().to_vec(), usize::from(v.signer()) as u32),
            Activity::Finalize(v) => p.observe_vote(v.proposal.round, Kind::Finalize, || Subject::Finalize { proposal: &v.proposal }.message().to_vec(), usize::from(v.signer()) as u32),
            _ => {}
        }
        commonware_actor::Feedback::Ok
    }
}

/// The kind, epoch and view of a vote subject.
pub fn subject_parts<D: commonware_cryptography::Digest>(subject: &Subject<'_, D>) -> (Kind, Round) {
    match subject {
        Subject::Notarize { proposal } => (Kind::Notarize, proposal.round),
        Subject::Nullify { round } => (Kind::Nullify, *round),
        Subject::Finalize { proposal } => (Kind::Finalize, proposal.round),
    }
}
