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
//!    Silence alone isn't that case (audit F18): a node stuck in an epoch the network has left, or
//!    cut off from it, sees no certificates either. So the escape fails safe. It also needs peers
//!    outside consensus to show the network standing still where this node is
//!    ([`Protection::note_network`]: HTTP peers at exactly this node's height outnumber those
//!    claiming more, and this node's height stayed put for most of the escape time,
//!    [`Timing::standstill_ms`]; see [`network_view`]), or the operator to name the height the
//!    network halted at (`--network-halted-at`, which stands in for the peers' answers, whatever
//!    they say); and no evidence of a later epoch or of a network past this node. Evidence
//!    counts only when it's verified (a finalized height or a certificate this node checked
//!    against a set it knows) or comes from more of this epoch's validators than can be faulty
//!    ([`Protection::note_later_epoch`]: their consensus traffic for a later epoch, recently).
//!    No single unverified source can keep a whole network from restarting (review of F18): one
//!    lying peer, p2p or HTTP, is outvoted, its hints expire, and the operator's word overrides
//!    every unverified answer. Without the standstill the node keeps abstaining: a missed vote,
//!    never a double-sign. And it signs only views above the newest finalized block it executed,
//!    whatever simplex replayed.
//! 3. **Doppelganger detection.** A vote or certificate carrying this key's signature on a
//!    message the record doesn't have, in a view an earlier, stopped instance can't have reached,
//!    means another instance is signing with this key. The node then signs nothing more and halts
//!    (`upgrade::Halt::DoubleSignRisk`, exit code 12).
//!
//! The network is observed through simplex's reporter ([`Observer`]): only certificates (quorum
//! proofs, verified) move the views this node waits for, so no single peer can silence it. And
//! nothing is ever signed in an epoch the network has finalized past (a node catching up after a
//! long stop doesn't vote in epochs it's replaying).

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
use tracing::{debug, error, info, warn};

/// The record's file in a validator home.
pub const RECORD_FILE: &str = "slashing-protection.json";
const FORMAT: &str = "blockula-slashing-protection-v1";
/// Why a signature waits while the restart rule watches the network (expected, logged quietly).
const WATCHING: &str = "watching the network after a start";
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

    /// How long peers must show the network standing still where this node is before the escape
    /// may fire: all of the escape time but the first [`Timing::watch_ms`] (audit F18).
    pub fn standstill_ms(&self) -> u64 {
        self.escape_ms.saturating_sub(self.watch_ms)
    }

    /// How long a validator's consensus traffic for a later epoch counts against the escape
    /// ([`Protection::note_later_epoch`]): a network that really is in a later epoch keeps sending
    /// it every view, so a hint that isn't renewed this long is stale.
    pub fn hint_ms(&self) -> u64 {
        self.escape_ms
    }
}

/// The startup rule's state for one epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gate {
    /// Watching the network since `since` (ms); `baseline` is the newest certificate view seen
    /// once the startup settled. `stuck`: the escape time passed, but nothing showed the network
    /// halted here (logged once).
    Watching { since: u64, baseline: Option<u64>, stuck: bool },
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
    /// Per epoch, the members of its set (by signer index) whose consensus traffic was for a later
    /// epoch, and when it last arrived (ms). Unverified one by one: it counts against the escape
    /// only from more members than can be faulty, and only while fresh ([`Timing::hint_ms`]).
    hints: BTreeMap<u64, BTreeMap<u32, u64>>,
    /// Each epoch's set size (for the fault bound the hints are counted against).
    sizes: BTreeMap<u64, usize>,
    /// What peers last said: the network standing still where this node is, since when.
    standstill: Option<Standstill>,
    /// View of the newest finalized block this node executed, per epoch: never signed at or below.
    executed: BTreeMap<u64, u64>,
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
            hints: BTreeMap::new(),
            sizes: BTreeMap::new(),
            standstill: None,
            executed: BTreeMap::new(),
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
    #[cfg(test)]
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
        g.sizes.insert(epoch, participants);
        g.newest_epoch = g.newest_epoch.max(epoch);
        if !g.gates.contains_key(&epoch) {
            // A set of one has nobody to watch (and nobody to double-sign against).
            g.current |= participants <= 1;
            let gate = if g.current { Gate::Open { above: 0 } } else { Gate::Watching { since: now_ms, baseline: None, stuck: false } };
            if matches!(gate, Gate::Watching { .. }) && me.is_some() {
                info!(epoch, "slashing protection: watching the network before signing (restart rule)");
            }
            g.gates.insert(epoch, gate);
        }
        // Old epochs' bookkeeping isn't needed any more.
        let keep = epoch.saturating_sub(4);
        g.gates.retain(|e, _| *e >= keep);
        g.observed.retain(|e, _| *e >= keep);
        g.executed.retain(|e, _| *e >= keep);
        g.me.retain(|e, _| *e >= keep);
        g.hints.retain(|e, _| *e >= keep);
        g.sizes.retain(|e, _| *e >= keep);
    }

    /// The network has finalized a block of `epoch` (a certificate this node verified): nothing
    /// is signed in an earlier epoch any more, escape or not. A node catching up after a long
    /// stop (or a restore) never votes in epochs the network has left.
    pub fn note_finalized_epoch(&self, epoch: u64) {
        let mut g = self.inner.lock();
        g.newest_epoch = g.newest_epoch.max(epoch);
    }

    /// This node executed a finalized block proposed in `(epoch, view)`: it never signs at or
    /// below that view again, so an escape (or a gate opened on certificates simplex replayed from
    /// an old journal) can't land on a view the network has finalized.
    pub fn note_executed(&self, epoch: u64, view: u64) {
        let mut g = self.inner.lock();
        let e = g.executed.entry(epoch).or_default();
        *e = (*e).max(view);
    }

    /// Consensus traffic for epoch `theirs` arrived from `member` (its signer index in epoch
    /// `ours`'s set; `None`: not in that set) while this node is in `ours`. The epoch on that
    /// traffic is the sender's word (a mux subchannel it picks), so one sender proves nothing,
    /// and one lying peer mustn't keep every validator of a restarting network from escaping
    /// (review of F18). It counts against the escape in `ours` once more members than can be
    /// faulty sent it within [`Timing::hint_ms`] (then an honest one is in a later epoch). Traffic
    /// from outside the set doesn't count. It never stops the watch itself (certificates, which
    /// are verified, still open the gate).
    pub fn note_later_epoch(&self, ours: u64, member: Option<u32>, theirs: u64) {
        if theirs <= ours {
            return;
        }
        let mut g = self.inner.lock();
        let Some(m) = member else {
            debug!(ours, theirs, "slashing protection: later-epoch traffic from outside this epoch's set (not counted)");
            return;
        };
        let now = g.now;
        if g.hints.entry(ours).or_default().insert(m, now).is_none() {
            debug!(ours, theirs, member = m, "slashing protection: a validator of this epoch is said to be in a later epoch");
        }
    }

    /// Whether the restart rule is watching the network in an epoch this key votes in (when
    /// peers are worth asking for [`Protection::note_network`]).
    pub fn watching(&self) -> bool {
        let g = self.inner.lock();
        g.gates.iter().any(|(e, gate)| matches!(gate, Gate::Watching { .. }) && *e >= g.newest_epoch && g.me.get(e).copied().flatten().is_some())
    }

    /// Advances the clock the startup rule runs on (a runtime task calls it a few times a second).
    pub fn tick(&self, now_ms: u64) {
        let t = self.timing;
        let mut g = self.inner.lock();
        g.now = g.now.max(now_ms);
        let now = g.now;
        let watching: Vec<(u64, u64, Option<u64>, bool)> = g.gates.iter().filter_map(|(e, gate)| if let Gate::Watching { since, baseline, stuck } = gate { Some((*e, *since, *baseline, *stuck)) } else { None }).collect();
        for (epoch, since, baseline, mut stuck) in watching {
            let seen = g.observed.get(&epoch).copied();
            let baseline = baseline.or(if now >= since + t.settle_ms { seen } else { None });
            let open = match (baseline, seen) {
                (Some(b), Some(m)) if now >= since + t.watch_ms && m >= b + t.views => Some(m),
                _ if now >= since + t.escape_ms => match g.escape(epoch, since, now, t) {
                    Ok(()) => {
                        warn!(epoch, "slashing protection: the network made no progress without this node, and its peers stand still where this node is; signing again on the local record alone");
                        Some(seen.unwrap_or(0))
                    }
                    Err(why) => {
                        if !stuck && g.me.get(&epoch).copied().flatten().is_some() {
                            warn!(epoch, why, "slashing protection: no progress in this epoch since the start, and nothing shows the network halted here, so still not signing (a node behind or cut off from the network sees no certificates either). If the whole network is halted at this node's height and no peer answers, restart with --network-halted-at <height> (docs/OPERATIONS.md)");
                        }
                        stuck = true;
                        None
                    }
                },
                _ => None,
            };
            let gate = match open {
                Some(above) => {
                    let above = above.max(g.executed.get(&epoch).copied().unwrap_or(0));
                    info!(epoch, above, "slashing protection: signing views above {above}");
                    g.current = true;
                    Gate::Open { above }
                }
                None => Gate::Watching { since, baseline, stuck },
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
                if why == WATCHING {
                    debug!(epoch, view, ?kind, "slashing protection: not signing yet (watching the network after a start)");
                } else {
                    warn!(epoch, view, ?kind, why, "slashing protection refused a signature");
                }
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
        if let Some(dir) = path.parent().filter(|_| self.durable) {
            std::fs::File::open(dir)?.sync_all()?;
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
    /// Whether more of `epoch`'s validators than can be faulty sent traffic for a later epoch
    /// within [`Timing::hint_ms`] ([`Protection::note_later_epoch`]).
    fn later_by_quorum(&self, epoch: u64, now: u64, t: Timing) -> bool {
        let f = self.sizes.get(&epoch).copied().unwrap_or(0).saturating_sub(1) / 3;
        let fresh = self.hints.get(&epoch).map_or(0, |h| h.values().filter(|at| now.saturating_sub(**at) <= t.hint_ms()).count());
        fresh > f
    }

    /// Whether the restart rule may escape in `epoch` (watched since `since`, no progress for
    /// the escape time): no verified sign of a later epoch, nor one from more of the epoch's
    /// validators than can be faulty, and peers' latest answer (or the operator's word) shows the
    /// network standing still where this node is, fresh, for at least [`Timing::standstill_ms`]
    /// of this watch. A standstill first seen before the watch began counts from its start (the
    /// node executed an epoch's last block, and peers answered before it entered the next epoch:
    /// review of F18). Anything else keeps it abstaining (audit F18).
    fn escape(&self, epoch: u64, since: u64, now: u64, t: Timing) -> Result<(), &'static str> {
        if self.newest_epoch > epoch {
            return Err("the network finalized a later epoch");
        }
        if self.later_by_quorum(epoch, now, t) {
            return Err("more of this epoch's validators than can be faulty are in a later epoch");
        }
        match self.standstill {
            Some(s) if s.epoch != epoch => Err("no peer shows the network standing where this node is"),
            Some(s) if now.saturating_sub(s.at_ms) > 2 * t.watch_ms => Err("peers haven't answered lately"),
            Some(s) if s.at_ms < since || s.at_ms - s.from_ms.max(since) < t.standstill_ms() => Err("the network hasn't stood still for long enough"),
            Some(_) => Ok(()),
            None => Err("no peer shows the network standing where this node is"),
        }
    }

    fn permit(&self, epoch: u64, view: u64, kind: Kind, hash: &str) -> Result<Option<Record>, &'static str> {
        if self.halt.is_some() {
            return Err("another instance signs with this key");
        }
        if epoch < self.newest_epoch {
            return Err("a later epoch is under way");
        }
        match self.gates.get(&epoch) {
            Some(Gate::Open { above }) if view <= *above => return Err("at or below views the network certified before this node signed again"),
            Some(Gate::Watching { .. }) => return Err(WATCHING),
            None if !self.current => return Err(WATCHING),
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

/// The network standing still where this node is, as peers outside consensus (or the operator)
/// last reported it ([`Protection::note_network`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Standstill {
    /// The epoch of this node's next block.
    epoch: u64,
    /// This node's height.
    height: u64,
    /// When it was first reported at exactly this height, and when last.
    from_ms: u64,
    at_ms: u64,
}

/// What peers outside consensus say about where the network is, for the restart rule's escape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkView {
    /// Standing still where this node is: more answering peers are at exactly its height than
    /// claim more, or the operator said so (`operator`). `epoch` is its next block's.
    Here { epoch: u64, operator: bool },
    /// Past this node: `verified` if a finalized height it checked says so (nothing overrides
    /// that), else peers' unverified answers (the operator's word overrides those).
    Ahead { verified: bool },
    /// Nobody answered, or nobody at this node's height.
    Unknown,
}

/// Reads what peers outside consensus report, for a node whose executed height is `height`:
/// `finalized`, the newest finalized height it verified (certificates, marshal's tips, a peer's
/// certified blocks); `claimed`, what its sync peers claim (the median, unverified); `tips`, the
/// heights its HTTP peers answer with (unverified); `halted_at`, the operator's
/// `--network-halted-at`. A verified height above this node means the network finalized a view
/// it never saw, which an earlier run of it (a restored volume) may have voted in: no escape,
/// whatever anyone says. The operator's word at exactly this height stands in for the peers,
/// whatever they answer (an unverified claim can't veto a restart the operator called: review of
/// F18). Otherwise unverified answers are counted: peers at exactly this height must outnumber
/// those claiming more (one liar among honest peers can't hold a restart back, and one stale peer
/// far below, which says nothing about this height, can't make a standstill), and so must the
/// median sync claim stay at or below it.
pub fn network_view(params: &crate::chain::Params, height: u64, finalized: u64, claimed: u64, tips: &[u64], halted_at: Option<u64>) -> NetworkView {
    let epoch = params.epoch_of(height + 1);
    if finalized > height {
        return NetworkView::Ahead { verified: true };
    }
    if halted_at == Some(height) {
        return NetworkView::Here { epoch, operator: true };
    }
    let at = tips.iter().filter(|t| **t == height).count();
    let ahead = tips.iter().filter(|t| **t > height).count();
    if claimed > height || (ahead > 0 && ahead >= at) {
        NetworkView::Ahead { verified: false }
    } else if at == 0 {
        NetworkView::Unknown
    } else {
        NetworkView::Here { epoch, operator: false }
    }
}

impl Protection {
    /// Feeds what peers outside consensus reported at `now_ms` ([`network_view`]: this node's
    /// executed `height`, its verified `finalized` height, its sync peers' median `claimed`
    /// height, the HTTP peers' `tips`, the operator's `halted_at`). The escape needs the network
    /// standing still where this node is, so this node's height moving starts the wait over, and
    /// an answer that isn't [`NetworkView::Here`] (or no answer) withdraws it.
    #[allow(clippy::too_many_arguments)]
    pub fn note_network(&self, params: &crate::chain::Params, height: u64, finalized: u64, claimed: u64, tips: &[u64], halted_at: Option<u64>, now_ms: u64) {
        let view = network_view(params, height, finalized, claimed, tips, halted_at);
        let mut g = self.inner.lock();
        match view {
            NetworkView::Here { epoch, operator } => match &mut g.standstill {
                Some(s) if (s.epoch, s.height) == (epoch, height) => s.at_ms = s.at_ms.max(now_ms),
                s => {
                    if operator && tips.iter().any(|t| *t != height) {
                        warn!(height, ?tips, "slashing protection: --network-halted-at {height} stands in for HTTP peers that answer otherwise (their answers aren't verified; a certificate past this height would still keep this node abstaining)");
                    }
                    *s = Some(Standstill { epoch, height, from_ms: now_ms, at_ms: now_ms });
                }
            },
            _ => g.standstill = None,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pq::PrivateKey;
    use commonware_consensus::simplex::types::{Finalization, Notarization, Notarize, Proposal};
    use commonware_consensus::types::{Epoch, View};
    use commonware_cryptography::{certificate::Scheme as _, Signer as _};
    use commonware_utils::{iter::NonEmpty, ordered::Set};

    const T: Timing = Timing { settle_ms: 2_000, watch_ms: 5_000, escape_ms: 20_000, views: 2 };

    fn public_key(seed: u8) -> Vec<u8> {
        PrivateKey::from_seed_bytes([seed; 32]).public_key().as_ref().to_vec()
    }

    fn round(epoch: u64, view: u64) -> Round {
        Round::new(Epoch::new(epoch), View::new(view))
    }

    /// A certificate by `signers` (indices) for `(epoch, view)` over `msg`.
    fn cert(p: &Protection, epoch: u64, view: u64, kind: Kind, msg: &[u8], signers: &[u32]) {
        p.observe_certificate(round(epoch, view), kind, || msg.to_vec(), signers.iter().copied());
    }

    /// The network finishing views `from..=to` of `epoch` (signed by validators 1–3), one every
    /// 500 ms from `t` on; returns the time after the last.
    fn progress(p: &Protection, epoch: u64, from: u64, to: u64, mut t: u64) -> u64 {
        for v in from..=to {
            cert(p, epoch, v, Kind::Notarize, format!("block {v}").as_bytes(), &[1, 2, 3]);
            t += 500;
            p.tick(t);
        }
        t
    }

    #[test]
    fn the_record_refuses_equivocation_and_allows_signing_again() {
        let p = Protection::in_memory(&public_key(1), T);
        p.enter_epoch(3, Some(0), 1, 0); // alone in the set: nothing to watch
        let sign = |e, v, k, m: &str| p.check_and_record(e, v, k, m.as_bytes());
        assert!(sign(3, 5, Kind::Notarize, "a"));
        assert!(sign(3, 5, Kind::Notarize, "a"), "the identical vote again");
        assert!(!sign(3, 5, Kind::Notarize, "b"), "ConflictingNotarize");
        assert!(sign(3, 5, Kind::Nullify, ""), "notarize then nullify is allowed");
        assert!(sign(3, 5, Kind::Nullify, ""));
        assert!(!sign(3, 5, Kind::Finalize, "a"), "NullifyFinalize");
        assert!(sign(3, 6, Kind::Notarize, "x") && sign(3, 6, Kind::Finalize, "x"));
        assert!(!sign(3, 6, Kind::Nullify, ""), "nullify after finalize");
        assert!(!sign(3, 6, Kind::Finalize, "y"), "ConflictingFinalize");
        assert!(sign(3, 6, Kind::Finalize, "x"));
        assert_eq!(p.status().refused, 4);
        // A new epoch starts a new record; the old epoch is closed.
        p.enter_epoch(4, Some(0), 1, 0);
        assert!(!sign(3, 7, Kind::Notarize, "a"), "an older epoch");
        assert!(sign(4, 1, Kind::Notarize, "a"));
        assert_eq!((p.record().epoch, p.record().votes.len()), (4, 1));
        // The window: views that fell out of it are refused.
        for v in 2..=(RECORD_VIEWS as u64 + 9) {
            assert!(sign(4, v, Kind::Notarize, "z"));
        }
        let r = p.record();
        assert_eq!((r.votes.len(), r.floor, r.last_view()), (RECORD_VIEWS, 9, RECORD_VIEWS as u64 + 9));
        assert!(!sign(4, 9, Kind::Nullify, ""), "below the window");
        assert!(sign(4, 10, Kind::Nullify, ""));
        assert_eq!((p.status().epoch, p.status().view), (4, RECORD_VIEWS as u64 + 9));
    }

    #[test]
    fn the_record_is_on_disk_before_the_signature_and_bound_to_its_key() {
        let dir = std::env::temp_dir().join(format!("blockula-protection-{}-{}", crate::chain::now_ms(), std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(RECORD_FILE);
        let p = Protection::open(&path, &public_key(1), T, true).unwrap();
        p.enter_epoch(2, Some(0), 1, 0);
        assert!(p.check_and_record(2, 7, Kind::Finalize, b"block"));
        let on_disk: Record = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(on_disk, p.record());
        assert_eq!(on_disk.votes[&7].finalize.as_deref(), Some(hex::encode(crate::crypto::hash(b"block")).as_str()));
        assert!(!dir.join("slashing-protection.json.tmp").exists());
        drop(p);
        // A restart refuses what conflicts with the record.
        let p = Protection::open(&path, &public_key(1), T, true).unwrap();
        p.enter_epoch(2, Some(0), 1, 0);
        assert!(!p.check_and_record(2, 7, Kind::Nullify, b""));
        assert!(p.check_and_record(2, 7, Kind::Finalize, b"block"));
        // Another key's record, or a broken one, stops the node instead of being ignored.
        assert!(Protection::open(&path, &public_key(2), T, true).unwrap_err().to_string().contains("protects key"));
        std::fs::write(&path, b"{\"format\":").unwrap();
        assert!(Protection::open(&path, &public_key(1), T, true).unwrap_err().to_string().contains("isn't a valid"));
        std::fs::remove_dir_all(dir).ok();
    }

    /// After a start, nothing is signed until the network has finished two more views than it
    /// had once the start settled, and then only views above every certificate seen.
    #[test]
    fn a_start_watches_the_network_before_signing() {
        let p = Protection::in_memory(&public_key(1), T);
        p.enter_epoch(5, Some(0), 4, 1_000);
        assert!(p.status().abstaining);
        assert!(!p.check_and_record(5, 1, Kind::Nullify, b""), "watching");
        // Certificates replayed or caught up on: the network had reached view 40.
        cert(&p, 5, 39, Kind::Notarize, b"39", &[0, 1, 2]);
        cert(&p, 5, 40, Kind::Notarize, b"40", &[1, 2, 3]);
        p.tick(3_000); // settled: baseline 40
        p.tick(6_500); // watched long enough, but no progress yet
        assert!(!p.check_and_record(5, 41, Kind::Notarize, b"41"));
        let t = progress(&p, 5, 41, 41, 6_500);
        assert!(p.status().abstaining, "one view isn't enough");
        progress(&p, 5, 42, 42, t);
        assert!(!p.status().abstaining);
        assert!(!p.check_and_record(5, 42, Kind::Nullify, b""), "at or below what the network certified");
        assert!(!p.check_and_record(5, 30, Kind::Notarize, b"old"));
        assert!(p.check_and_record(5, 43, Kind::Notarize, b"43"));
        // Later epochs need no watching (the earlier instance never reached them) ...
        p.enter_epoch(6, Some(0), 4, 9_000);
        assert!(p.check_and_record(6, 1, Kind::Notarize, b"e6"));
        // ... and the previous epoch is over.
        assert!(!p.check_and_record(5, 44, Kind::Notarize, b"44"));
        assert_eq!(p.status().refused, 5);
    }

    /// 10-block epochs: epoch `e`'s heights are `10e..=10e+9`.
    fn params() -> crate::chain::Params {
        crate::chain::Params { epoch_blocks: 10, ..Default::default() }
    }

    /// Runs `p`'s clock from `from` to `to` (a tick every 250 ms), with `poll(t)` every 2 s: what
    /// node.rs `watch_network` does, with the peers' answers `poll` feeds it.
    fn run(p: &Protection, from: u64, to: u64, mut poll: impl FnMut(u64)) {
        for t in (from..=to).step_by(250) {
            if t % 2_000 == 0 {
                poll(t);
            }
            p.tick(t);
        }
    }

    /// Peers answering for a node at height `h` (it verified everything up to `h`, and its sync
    /// peers claim no more): `note_network` at `t`.
    fn answer(p: &Protection, h: u64, tips: &[u64], halted_at: Option<u64>, t: u64) {
        p.note_network(&params(), h, h, h, tips, halted_at, t)
    }

    /// Peers that answer with this node's own height, 5 into `epoch` (`10 * epoch + 5`), and two
    /// behind it: a network standing still where this node is.
    fn standing(p: &Protection, epoch: u64) -> impl FnMut(u64) + '_ {
        let h = 10 * epoch + 5;
        move |t| answer(p, h, &[h, h - 1, 0], None, t)
    }

    /// A network that can't progress without this node (every validator restarting, or a halt)
    /// gets its vote back after the escape time, with the record as the only guard, once peers
    /// show the network standing still where this node is (audit F18: silence alone doesn't),
    /// and only above the newest block it executed.
    #[test]
    fn a_stalled_network_gets_this_node_back_after_the_escape_time() {
        let p = Protection::in_memory(&public_key(1), T);
        p.enter_epoch(2, Some(0), 4, 0);
        cert(&p, 2, 10, Kind::Nullify, b"", &[1, 2, 3]);
        let mut poll = standing(&p, 2);
        for t in (0..20_000).step_by(250) {
            if t % 2_000 == 0 {
                poll(t);
            }
            p.tick(t);
            assert!(!p.check_and_record(2, 11, Kind::Nullify, b""), "{t}");
        }
        p.tick(20_000);
        assert!(!p.check_and_record(2, 10, Kind::Nullify, b""));
        assert!(p.check_and_record(2, 11, Kind::Nullify, b""));
        // It executed a block finalized in view 14 (whatever simplex replayed): above that only.
        let executed = Protection::in_memory(&public_key(1), T);
        executed.enter_epoch(2, Some(0), 4, 0);
        cert(&executed, 2, 10, Kind::Nullify, b"", &[1, 2, 3]);
        executed.note_executed(2, 14);
        executed.note_executed(1, 99);
        run(&executed, 0, 20_000, standing(&executed, 2));
        assert!(!executed.check_and_record(2, 14, Kind::Nullify, b"") && executed.check_and_record(2, 15, Kind::Nullify, b""));
        // Far behind: the network finalized a later epoch, so this one gets nothing, even after
        // the escape time and with peers standing still.
        let behind = Protection::in_memory(&public_key(1), T);
        behind.enter_epoch(2, Some(0), 4, 0);
        behind.note_finalized_epoch(7);
        run(&behind, 0, 60_000, standing(&behind, 2));
        assert!(!behind.check_and_record(2, 11, Kind::Nullify, b""));
        // Every validator restarting at once (here at genesis): nothing to observe at all, but
        // the peers' tips are this node's.
        let q = Protection::in_memory(&public_key(1), T);
        q.enter_epoch(0, Some(0), 4, 0);
        run(&q, 0, 19_999, |t| answer(&q, 0, &[0, 0, 0], None, t));
        assert!(!q.check_and_record(0, 1, Kind::Notarize, b"b"));
        q.tick(20_000);
        assert!(q.check_and_record(0, 1, Kind::Notarize, b"b"));
    }

    /// Audit F18 (the auditor's PoC): a volume restored from an old snapshot, whose node is stuck
    /// in an epoch the network has left (or can't reach the network at all), sees no
    /// certificates for its epoch past the ones it replays. That silence isn't a halted network:
    /// with nothing showing the network standing still where it is, it never escapes, so it
    /// can't sign a view its earlier run finalized. Neither do answers that don't show that:
    /// more of the epoch's validators than can be faulty in a later epoch, a verified height past
    /// it, peers ahead that outnumber those at its height, a network still crawling forward,
    /// stale or early answers, only stale peers far below it, or the operator's word at another
    /// height or against a verified height.
    #[test]
    fn silence_alone_never_opens_the_escape() {
        // The earlier run signed views 1..=120 of epoch 4; the volume was snapshotted at 100.
        let old = Protection::in_memory(&public_key(1), T);
        old.enter_epoch(4, Some(0), 1, 0);
        let mut snapshot = None;
        for v in 1..=120 {
            assert!(old.check_and_record(4, v, Kind::Notarize, format!("{v}").as_bytes()) && old.check_and_record(4, v, Kind::Finalize, format!("{v}").as_bytes()));
            if v == 100 {
                snapshot = Some(old.record());
            }
        }
        let restored = |at: u64| {
            let p = Protection::with_record(None, false, T, snapshot.clone().unwrap());
            p.enter_epoch(4, Some(0), 4, at);
            // The rolled-back journal replays certificates up to view 100.
            for v in 90..=100 {
                cert(&p, 4, v, Kind::Finalize, format!("{v}").as_bytes(), &[0, 1, 2]);
            }
            p.note_finalized_epoch(4);
            p
        };
        let refused = |p: &Protection| !p.check_and_record(4, 101, Kind::Nullify, b"");
        let params = params();

        // Nobody answers: a minute (or an hour) on, it still abstains.
        let p = restored(0);
        run(&p, 0, 60_000, |_| {});
        assert!(refused(&p), "the earlier run finalized view 101");
        assert!(p.status().abstaining && p.watching() && p.halted().is_none());
        run(&p, 60_000, 3_600_000, |t| p.note_network(&params, 45, 45, 45, &[], None, t));
        assert!(refused(&p));
        // Two of the epoch's four validators (more than its one faulty) keep sending traffic for
        // a later epoch: peers standing still don't help.
        let mut stand = standing(&p, 4);
        run(&p, 3_600_000, 3_700_000, |t| {
            p.note_later_epoch(4, Some(1), 5);
            p.note_later_epoch(4, Some(2), 5);
            stand(t)
        });
        assert!(refused(&p));
        // Certificates (verified) still open it the usual way: the network moves on from 101.
        let t = progress(&p, 4, 101, 103, 3_700_000);
        p.tick(t);
        assert!(refused(&p) && p.check_and_record(4, 104, Kind::Nullify, b""));

        // Answers that don't count, each for a minute:
        type Answer<'a> = Box<dyn Fn(&Protection, u64) + 'a>;
        let cases: Vec<(&str, Answer)> = vec![
            ("a peer one block ahead, and only one at its height", Box::new(|p, t| p.note_network(&params, 45, 45, 45, &[45, 46], None, t))),
            ("peers ahead outnumber those at its height", Box::new(|p, t| p.note_network(&params, 45, 45, 45, &[45, 46, 50], None, t))),
            ("this node verified a finalized height above its own", Box::new(|p, t| p.note_network(&params, 45, 47, 47, &[45, 45, 45], None, t))),
            ("its sync peers' median claim is ahead", Box::new(|p, t| p.note_network(&params, 45, 45, 47, &[45], None, t))),
            ("peers in a later epoch", Box::new(|p, t| p.note_network(&params, 45, 45, 45, &[50], None, t))),
            ("the network still crawling forward", Box::new(|p, t| { let h = 40 + t / 10_000; p.note_network(&params, h, h, h, &[h], None, t) })),
            ("a standstill in another epoch", Box::new(|p, t| p.note_network(&params, 35, 35, 35, &[35], None, t))),
            ("only stale peers far below it", Box::new(|p, t| p.note_network(&params, 45, 45, 45, &[0, 3, 44], None, t))),
            ("the operator's word at another height", Box::new(|p, t| p.note_network(&params, 45, 45, 45, &[], Some(44), t))),
            ("the operator's word against a finalized height it verified", Box::new(|p, t| p.note_network(&params, 45, 46, 46, &[46], Some(45), t))),
        ];
        for (why, answer) in &cases {
            let p = restored(0);
            run(&p, 0, 60_000, |t| answer(&p, t));
            assert!(refused(&p), "{why}");
        }
        // Peers that stood still before the watch began, or stopped answering long ago.
        let p = restored(30_000);
        run(&p, 0, 29_750, standing(&p, 4));
        run(&p, 30_000, 90_000, |_| {});
        assert!(refused(&p), "before the watch, and stale");
        // The halted-network case: peers standing still where it is (or the operator's
        // --network-halted-at, whatever unverified answers say), for long enough. It signs on the
        // record alone.
        let p = restored(0);
        run(&p, 0, 30_000, standing(&p, 4));
        assert!(!refused(&p));
        for tips in [&[][..], &[46], &[45, 1_000, 1_001]] {
            let p = restored(0);
            run(&p, 0, 30_000, |t| p.note_network(&params, 45, 45, 47, tips, Some(45), t));
            assert!(!refused(&p), "the operator's word over {tips:?}");
        }
    }

    /// Review of F18: in a full restart (every validator starting at once, so nobody's votes are
    /// observed and the escape is the only way back), no single unverified source keeps a
    /// validator from escaping. One validator's consensus traffic for a later epoch (whatever
    /// epoch it names) is one sender's word; traffic from outside the set doesn't count; such
    /// hints expire. One HTTP peer claiming to be ahead (or changing its claim every answer) is
    /// outvoted by the peers at this node's height, and the operator's word overrides it.
    #[test]
    fn one_unverified_source_cant_hold_a_restart_back() {
        // Epoch 4 of a 4-validator set, halted at height 45; the escape fires at 20 s.
        let at = |t: u64| {
            let p = Protection::in_memory(&public_key(1), T);
            p.enter_epoch(4, Some(0), 4, t);
            p
        };
        let signs = |p: &Protection| p.check_and_record(4, 1, Kind::Nullify, b"");
        let p = at(0);
        run(&p, 0, 20_000, |t| answer(&p, 45, &[45, 45, 45], None, t));
        assert!(signs(&p), "the control: honest peers standing still");
        // One member's traffic for epoch u64::MAX, renewed every poll, and any number of
        // non-members': nothing.
        let p = at(0);
        run(&p, 0, 20_000, |t| {
            p.note_later_epoch(4, Some(3), u64::MAX);
            p.note_later_epoch(4, None, 9);
            answer(&p, 45, &[45, 45, 45], None, t)
        });
        assert!(signs(&p), "one validator's word");
        // Two members (more than the one that can be faulty) do count, while they keep at it;
        // once they stop, their hints expire.
        let p = at(0);
        run(&p, 0, 60_000, |t| {
            p.note_later_epoch(4, Some(2), 5);
            p.note_later_epoch(4, Some(3), 5);
            answer(&p, 45, &[45, 45, 45], None, t)
        });
        assert!(!signs(&p), "two of four validators in a later epoch");
        run(&p, 60_250, 100_000, |t| answer(&p, 45, &[45, 45, 45], None, t));
        assert!(signs(&p), "hints expire");
        // One HTTP peer a block ahead, or claiming a new tip every answer: outvoted.
        for liar in [Box::new(|_: u64| 46u64) as Box<dyn Fn(u64) -> u64>, Box::new(|t: u64| 46 + t / 2_000)] {
            let p = at(0);
            run(&p, 0, 20_000, |t| answer(&p, 45, &[45, 45, liar(t)], None, t));
            assert!(signs(&p), "one lying HTTP peer among honest ones");
        }
        // As many claiming more as at its height: abstains, until the operator, who called the
        // restart at 45, says so (overriding unverified answers, far ahead or not).
        let p = at(0);
        run(&p, 0, 60_000, |t| answer(&p, 45, &[45, 46], None, t));
        assert!(!signs(&p), "a tie keeps it abstaining");
        let p = at(0);
        run(&p, 0, 20_000, |t| answer(&p, 45, &[45, u64::MAX], Some(45), t));
        assert!(signs(&p), "the operator's word");
    }

    /// Review of F18: a validator that executes an epoch's last block while the network halts
    /// there can hear peers standing still at that height before it enters the next epoch (the
    /// executor signals the epoch only after the block is on disk). That standstill counts from
    /// the new epoch's watch, so it still escapes there instead of abstaining for good.
    #[test]
    fn a_standstill_seen_just_before_the_next_epoch_counts_from_its_watch() {
        let p = Protection::in_memory(&public_key(1), T);
        p.enter_epoch(4, Some(0), 4, 0);
        // Watching epoch 4 at height 48, peers at 49 (ahead).
        run(&p, 0, 4_000, |t| answer(&p, 48, &[49, 49, 49], None, t));
        // It executes 49 (the last block of epoch 4); a poll lands before it enters epoch 5.
        answer(&p, 49, &[49, 49, 49], None, 4_100);
        p.enter_epoch(5, Some(0), 4, 4_200);
        for t in (4_250..=24_000).step_by(250) {
            if t % 2_000 == 250 {
                answer(&p, 49, &[49, 49, 49], None, t);
            }
            p.tick(t);
        }
        assert!(!p.check_and_record(5, 1, Kind::Nullify, b""), "not before the escape time of epoch 5's watch");
        for t in (24_250..=26_000).step_by(250) {
            if t % 2_000 == 250 {
                answer(&p, 49, &[49, 49, 49], None, t);
            }
            p.tick(t);
        }
        assert!(p.check_and_record(5, 1, Kind::Nullify, b""), "escaped in epoch 5");
    }

    /// What peers' answers say, for a node at height 25 of a chain with 10-block epochs (its next
    /// block, 26, is in epoch 2), and how long the network must stand still.
    #[test]
    fn peers_tips_show_only_a_network_standing_still_here() {
        let params = params();
        let view = |finalized, claimed, tips: &[u64], halted_at| network_view(&params, 25, finalized, claimed, tips, halted_at);
        let here = NetworkView::Here { epoch: 2, operator: false };
        let unverified = NetworkView::Ahead { verified: false };
        assert_eq!(view(25, 25, &[], None), NetworkView::Unknown, "nobody answered");
        assert_eq!(view(25, 25, &[25, 24, 0], None), here, "at or behind this node, one at its height");
        assert_eq!(view(25, 25, &[3, 24], None), NetworkView::Unknown, "stale peers below say nothing about this height");
        assert_eq!(view(25, 25, &[25, 26], None), unverified, "as many claim more as are here");
        assert_eq!(view(25, 25, &[25, 25, 26], None), here, "one lying peer is outvoted");
        assert_eq!(view(25, 25, &[25, 29, 1_000], None), unverified, "peers far ahead outnumber those here");
        assert_eq!(view(25, 26, &[25], None), unverified, "the sync peers' median claim counts");
        assert_eq!(view(26, 26, &[25, 25, 25], None), NetworkView::Ahead { verified: true }, "a finalized height this node verified");
        assert_eq!(view(26, 26, &[], Some(25)), NetworkView::Ahead { verified: true }, "... which the operator's word doesn't override");
        assert_eq!(view(25, 30, &[26, 1_000], Some(25)), NetworkView::Here { epoch: 2, operator: true }, "unverified answers it does");
        assert_eq!(view(25, 25, &[], Some(24)), NetworkView::Unknown, "only at this node's height");

        // The wait starts over whenever this node's height moves, and nothing short of a
        // standstill at its height starts it.
        let p = Protection::in_memory(&public_key(1), T);
        p.enter_epoch(2, Some(0), 4, 0);
        run(&p, 0, 24_000, |t| p.note_network(&params, 25, 25, 25, &[t / 2_000], None, t));
        assert!(!p.check_and_record(2, 1, Kind::Nullify, b""), "a peer catching up until 24 s");
        run(&p, 24_250, 40_000, |t| p.note_network(&params, 25, 25, 25, &[25], None, t));
        assert!(!p.check_and_record(2, 1, Kind::Nullify, b""), "still since 26 s: 14 s");
        p.note_network(&params, 25, 25, 25, &[25], None, 41_000);
        p.tick(41_000);
        assert!(p.check_and_record(2, 1, Kind::Nullify, b""), "still for 15 s (T.standstill_ms)");
        let q = Protection::in_memory(&public_key(1), T);
        q.enter_epoch(2, Some(0), 4, 0);
        let poll = |t: u64| q.note_network(&params, 24 + (t / 10_000).min(1), 25, 25, &[25], None, t);
        run(&q, 0, 24_000, poll);
        assert!(!q.check_and_record(2, 1, Kind::Nullify, b""), "it executed the finalized block 25 at 10 s: still for 14 s");
        run(&q, 24_250, 26_000, poll);
        assert!(q.check_and_record(2, 1, Kind::Nullify, b""), "and for 16 s");
        // A tip in a later epoch is one unverified answer: it doesn't stick.
        let q = Protection::in_memory(&public_key(1), T);
        q.enter_epoch(2, Some(0), 4, 0);
        q.note_network(&params, 25, 25, 25, &[31], None, 0);
        run(&q, 0, 30_000, |t| q.note_network(&params, 25, 25, 25, &[25], None, t));
        assert!(q.check_and_record(2, 1, Kind::Nullify, b""));
        // Answering stops (peers unreachable): the standstill is withdrawn, not kept.
        let r = Protection::in_memory(&public_key(1), T);
        r.enter_epoch(2, Some(0), 4, 0);
        run(&r, 0, 30_000, |t| r.note_network(&params, 25, 25, 25, if t < 18_000 { &[25] } else { &[] }, None, t));
        assert!(!r.check_and_record(2, 1, Kind::Nullify, b""));
    }

    /// The case the restart rule exists for: a volume restored from a snapshot taken at view 10,
    /// after the old instance had signed through view 20. The record alone (rolled back) would
    /// let it nullify view 15, which it had finalized: slashable. Watching the network doesn't.
    #[test]
    fn a_restored_record_alone_would_double_sign_but_watching_the_network_does_not() {
        let old = Protection::in_memory(&public_key(1), T);
        old.enter_epoch(1, Some(0), 1, 0);
        let mut snapshot = None;
        for v in 1..=20 {
            assert!(old.check_and_record(1, v, Kind::Notarize, format!("{v}").as_bytes()) && old.check_and_record(1, v, Kind::Finalize, format!("{v}").as_bytes()));
            if v == 10 {
                snapshot = Some(old.record());
            }
        }
        let snapshot = snapshot.unwrap();
        let restored = |participants| {
            let p = Protection::with_record(None, false, T, snapshot.clone());
            p.enter_epoch(1, Some(0), participants, 0);
            p
        };
        // Record only (nobody to watch): the stale record lets the conflicting vote through.
        assert!(restored(1).check_and_record(1, 15, Kind::Nullify, b""), "the rolled-back record knows nothing of view 15");
        // With the restart rule, the restored node first sees the network move on from view 21.
        let p = restored(4);
        cert(&p, 1, 21, Kind::Finalize, b"21", &[1, 2, 3]);
        p.tick(2_000);
        progress(&p, 1, 22, 23, 4_000);
        assert!(!p.check_and_record(1, 15, Kind::Nullify, b""));
        assert!(!p.check_and_record(1, 20, Kind::Nullify, b""));
        assert!(p.check_and_record(1, 24, Kind::Nullify, b""));
    }

    #[test]
    fn a_signature_this_process_never_made_halts_it() {
        // Open at view 30 after watching (this node is validator 0).
        let p = Protection::in_memory(&public_key(1), T);
        p.enter_epoch(1, Some(0), 4, 0);
        cert(&p, 1, 26, Kind::Notarize, b"26", &[1, 2, 3]);
        p.tick(2_000);
        let t = progress(&p, 1, 27, 30, 2_000);
        p.tick(t + 1_000);
        assert!(p.check_and_record(1, 31, Kind::Notarize, b"ours"));
        // Our own vote coming back, in a certificate or as a vote: fine.
        cert(&p, 1, 31, Kind::Notarize, b"ours", &[0, 2, 3]);
        p.observe_vote(round(1, 31), Kind::Notarize, || b"ours".to_vec(), 0);
        // An earlier instance's signature in a view the network had certified: not proof.
        cert(&p, 1, 29, Kind::Finalize, b"29", &[0, 1, 2]);
        assert!(p.halted().is_none());
        // Our key on something we never signed, past what an earlier instance could reach.
        cert(&p, 1, 32, Kind::Notarize, b"theirs", &[1, 2, 3]);
        assert!(p.halted().is_none(), "not our index");
        cert(&p, 1, 32, Kind::Notarize, b"theirs", &[0, 2, 3]);
        assert!(p.halted().unwrap().contains("another instance"));
        assert!(!p.check_and_record(1, 33, Kind::Notarize, b"next"), "signs nothing more");
        // While watching: a vote under our key far past the baseline is a running clone.
        let q = Protection::in_memory(&public_key(1), T);
        q.enter_epoch(1, Some(0), 4, 0);
        cert(&q, 1, 50, Kind::Notarize, b"50", &[1, 2, 3]);
        q.tick(2_000);
        q.observe_vote(round(1, 52), Kind::Nullify, Vec::new, 0);
        assert!(q.halted().is_none(), "within reach of a stopped instance");
        q.observe_vote(round(1, 53), Kind::Nullify, Vec::new, 0);
        assert!(q.halted().is_some());
        // Before the start settles there's no baseline to judge against.
        let r = Protection::in_memory(&public_key(1), T);
        r.enter_epoch(1, Some(0), 4, 0);
        r.observe_vote(round(1, 500), Kind::Nullify, Vec::new, 0);
        assert!(r.halted().is_none());
    }

    /// The observer reads real simplex certificates and votes (ML-DSA, assembled by the scheme).
    #[test]
    fn the_observer_reads_simplex_activity() {
        let keys: Vec<PrivateKey> = (1..=4).map(|s| PrivateKey::from_seed_bytes([s; 32])).collect();
        let set = Set::try_from(keys.iter().map(|k| k.public_key()).collect::<Vec<_>>()).unwrap();
        let schemes: Vec<MlDsaScheme> = keys.iter().map(|k| MlDsaScheme::signer(b"blockula/test", set.clone(), k.clone()).unwrap()).collect();
        let index = |k: &PrivateKey| set.position(&k.public_key()).unwrap() as u32;
        let (me, others): (Vec<&MlDsaScheme>, Vec<&MlDsaScheme>) = schemes.iter().partition(|s| s.me().map(|i| usize::from(i) as u32) == Some(index(&keys[0])));
        let p = Protection::in_memory(keys[0].public_key().as_ref(), T);
        p.enter_epoch(1, Some(index(&keys[0])), 4, 0);
        let mut observer = Observer(p.clone());
        let notarization = |view: u64, signers: &[&MlDsaScheme], payload: u8| {
            let proposal = Proposal::new(round(1, view), View::new(view - 1), Digest([payload; 32]));
            let votes: Vec<Notarize<MlDsaScheme, Digest>> = signers.iter().map(|s| Notarize::sign(*s, proposal.clone()).unwrap()).collect();
            Notarization::from_notarizes(signers[0], NonEmpty::try_new(votes.iter()).unwrap(), &commonware_parallel::Sequential).unwrap()
        };
        for v in 10..=14 {
            let n = notarization(v, &others, v as u8);
            observer.report(Activity::Notarization(n));
            p.tick(v * 1_000);
        }
        assert!(!p.status().abstaining, "watched views 10–14 go by");
        // A finalization including our signature on a block we never notarized or finalized.
        let proposal = Proposal::new(round(1, 15), View::new(14), Digest([7; 32]));
        let finalizes: Vec<_> = [me[0], others[0], others[1]].iter().map(|s| commonware_consensus::simplex::types::Finalize::sign(*s, proposal.clone()).unwrap()).collect();
        let f = Finalization::from_finalizes(me[0], NonEmpty::try_new(finalizes.iter()).unwrap(), &commonware_parallel::Sequential).unwrap();
        assert!(p.halted().is_none());
        observer.report(Activity::Finalization(f));
        assert!(p.halted().unwrap().contains("Finalize vote in epoch 1, view 15"));
    }
}
