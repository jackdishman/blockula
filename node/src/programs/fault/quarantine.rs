//! The node-local safeguards beside the fault paths (docs/PROGRAMS-V0.md §6.2, §9.2): the
//! quarantine, strikes, and the record of blocks this node's own verification ran clean. None of
//! them is consensus: they decide what this node proposes, relays and abstains on, never what a
//! block means. Every one is bounded, expires, starts empty, and stays empty on a chain that has
//! no program transactions.
//!
//! All heights here are **block heights**, and every "until" is the first height at which an
//! entry no longer holds, asked about with `next`, the height of the next block (the executed
//! height plus one): an entry holds while `next < until`.

use super::marker::Record;
use crate::chain::MEMPOOL_TTL_BLOCKS;
use crate::programs::{digest, is_program_tx};
use crate::types::{Action, Tx};
use anyhow::bail;
use std::borrow::Borrow;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// How long a faulting transaction's sender is held back from sending program transactions, and
/// so how long a transaction other validators may be faulting on keeps its sender out of this
/// node's mempool: [`MEMPOOL_TTL_BLOCKS`] (ten minutes at 1 s blocks) from the fault.
pub const QUARANTINE_HOLD_BLOCKS: u64 = MEMPOOL_TTL_BLOCKS;
/// The most transaction ids, senders and code hashes held at once, oldest out first (§6.2).
pub const MAX_QUARANTINED_IDS: usize = 1_024;
pub const MAX_QUARANTINED_SENDERS: usize = 1_024;
pub const MAX_QUARANTINED_CODES: usize = 256;

/// What the quarantine holds now, by kind (`tx`, `sender`, `code`): the gauge
/// `blockula_program_quarantined{kind}`. Process-wide, like the fault counters (a node runs one
/// chain), set whenever an entry comes or goes.
pub(super) static QUARANTINED: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// Keys held until a height, oldest first, at most `cap` of them.
#[derive(Debug)]
struct Held<K> {
    cap: usize,
    entries: VecDeque<(K, u64)>,
}

impl<K: PartialEq> Held<K> {
    fn new(cap: usize) -> Self {
        Held { cap, entries: VecDeque::new() }
    }

    /// Holds `key` until `until`. A key held already keeps the later end, and counts as the newest
    /// entry; past `cap` the oldest go.
    fn hold(&mut self, key: K, until: u64) {
        let until = self.entries.iter().find(|(k, _)| *k == key).map_or(until, |(_, u)| (*u).max(until));
        self.entries.retain(|(k, _)| *k != key);
        self.entries.push_back((key, until));
        while self.entries.len() > self.cap {
            self.entries.pop_front();
        }
    }

    /// The height `key` is held until, if it is held at `next`.
    fn until<Q: ?Sized + PartialEq>(&self, key: &Q, next: u64) -> Option<u64>
    where
        K: Borrow<Q>,
    {
        self.entries.iter().find(|(k, u)| k.borrow() == key && next < *u).map(|(_, u)| *u)
    }

    /// Lets go of what no longer holds at `next`.
    fn expire(&mut self, next: u64) {
        self.entries.retain(|(_, u)| next < *u);
    }
}

/// Why a sender's program transactions are held back, which decides what the door tells it.
/// `Strikes` sorts below `Fault`, so a fault's message wins a tie.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Why {
    /// Two of this node's proposals carrying its program transactions never certified ([`Strikes`]).
    Strikes,
    /// One of its program transactions faulted in a proposer's trial, or in the one the trial marker remembers.
    Fault,
}

/// What this node won't take into its mempool, or vote for a block carrying, after a program
/// execution fault in a proposer's trial (§6.2). It keys on what the faulting transaction's author
/// controls, never on other people's programs or accounts:
/// - **its id**, until no block can carry it anywhere (the height after its `valid_until`);
/// - **its sender, for program transactions only** (tags 62–64: transfers and every other native
///   action go through, so the sender can always replace the nonce the dropped transaction held),
///   for [`QUARANTINE_HOLD_BLOCKS`] from the fault;
/// - **for an upload, the new code's hash**, for a day of blocks: bytes the sender wrote, refused
///   from anyone.
///
/// A call's or a create's declared code is never quarantined: it's the callee's and the bindings'
/// code, other people's, and one caller's input that trips an engine bug says nothing about anyone
/// else's call. A restart empties it (the trial marker refills what crashed the process).
#[derive(Debug)]
pub struct Quarantine {
    ids: Held<String>,
    /// A sender held for both reasons has an entry for each, with its own end.
    senders: Held<(String, Why)>,
    codes: Held<[u8; 32]>,
}

impl Default for Quarantine {
    fn default() -> Self {
        Quarantine { ids: Held::new(MAX_QUARANTINED_IDS), senders: Held::new(MAX_QUARANTINED_SENDERS), codes: Held::new(MAX_QUARANTINED_CODES) }
    }
}

impl Quarantine {
    /// Nothing is held: the state of every chain without program faults, which then pays one
    /// comparison per admitted transaction.
    pub fn is_empty(&self) -> bool {
        self.ids.entries.is_empty() && self.senders.entries.is_empty() && self.codes.entries.is_empty()
    }

    /// How many ids, senders and code hashes are held (expired entries too, until [`expire`]; a
    /// sender held for both reasons counts twice).
    ///
    /// [`expire`]: Quarantine::expire
    pub fn lens(&self) -> [usize; 3] {
        [self.ids.entries.len(), self.senders.entries.len(), self.codes.entries.len()]
    }

    fn publish(&self) {
        for (gauge, n) in QUARANTINED.iter().zip(self.lens()) {
            gauge.store(n as u64, Relaxed);
        }
    }

    /// A `Tx` fault in the trial of `tx` for the block at `height`, or what the trial marker
    /// remembers of one: holds its id, its sender (for program transactions) and, for an upload,
    /// its code, as above. `day` is a day in blocks at this chain's block time.
    pub fn hold(&mut self, tx: &Record, height: u64, day: u64) {
        self.ids.hold(tx.id.clone(), tx.valid_until.saturating_add(1));
        self.senders.hold((tx.sender.clone(), Why::Fault), height.saturating_add(QUARANTINE_HOLD_BLOCKS));
        if let Some(hash) = tx.wasm_hash {
            self.codes.hold(hash, height.saturating_add(day));
        }
        self.publish();
    }

    /// Holds `sender`'s program transactions back for [`QUARANTINE_HOLD_BLOCKS`] from block
    /// `height` (two uncertified proposals of one of them: [`Strikes`]). No fault happened, and
    /// the door says so ([`Quarantine::refuses`]).
    pub fn hold_sender(&mut self, sender: &str, height: u64) {
        self.senders.hold((sender.to_string(), Why::Strikes), height.saturating_add(QUARANTINE_HOLD_BLOCKS));
        self.publish();
    }

    /// The height `sender` is held until at `next`, and why: of its holds, the one that lasts
    /// longest (the fault's, if two end together).
    fn sender_hold(&self, sender: &str, next: u64) -> Option<(u64, Why)> {
        self.senders.entries.iter().filter(|((s, _), until)| s == sender && next < *until).map(|((_, why), until)| (*until, *why)).max()
    }

    /// The mempool's door (§6.2): refuses `tx`, whose id is `id`, if the quarantine holds it: its
    /// id; its sender, when it is a program transaction; or, for an upload, its code. `next` is
    /// the height of the next block. Nothing else is refused. A sender held after strikes is told
    /// so, in its own words: nothing faulted, and other nodes accept its transactions.
    pub fn refuses(&self, tx: &Tx, id: &str, next: u64) -> anyhow::Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        let by_fault = |until: u64| format!("quarantined on this node after a program execution fault, until block {until}: see its log");
        let sender = || match self.sender_hold(&tx.from, next)? {
            (until, Why::Fault) => Some(by_fault(until)),
            (until, Why::Strikes) => Some(format!("held on this node until block {until}: two of this node's proposals carrying this sender's program transactions weren't certified; other nodes accept them")),
        };
        let refusal = self.ids.until(id, next).map(by_fault).or_else(|| if is_program_tx(tx) { sender() } else { None }).or_else(|| match &tx.action {
            Action::UploadCode { wasm, .. } if !self.codes.entries.is_empty() => self.codes.until(&digest::wasm_hash(&wasm.0), next).map(by_fault),
            _ => None,
        });
        match refusal {
            Some(why) => bail!("{why}"),
            None => Ok(()),
        }
    }

    /// Tests: the heights the id, the sender and the code of `record` are each held until (first
    /// block they no longer hold at), as of block `next`.
    #[cfg(test)]
    pub fn held_until(&self, record: &Record, next: u64) -> [Option<u64>; 3] {
        [self.ids.until(record.id.as_str(), next), self.sender_hold(&record.sender, next).map(|(until, _)| until), record.wasm_hash.and_then(|h| self.codes.until(&h, next))]
    }

    /// A quarantined transaction id at `next` that `in_block` says the block carries, if any: a
    /// block carrying one is a block this node can't judge (§6.2), whoever else it carries. Only
    /// ids count, not a held sender's other transactions: a Byzantine proposer that includes one
    /// gets it judged, and abstained on only if it faults. It walks the quarantine (at most
    /// [`MAX_QUARANTINED_IDS`] ids, oldest first) and asks the block, not the other way round.
    pub fn holds_id(&self, in_block: impl Fn(&str) -> bool, next: u64) -> Option<&String> {
        self.ids.entries.iter().find(|(id, until)| next < *until && in_block(id)).map(|(id, _)| id)
    }

    /// Lets go of every entry that no longer holds at `next` (each executed block).
    pub fn expire(&mut self, next: u64) {
        if self.is_empty() {
            return;
        }
        self.ids.expire(next);
        self.senders.expire(next);
        self.codes.expire(next);
        self.publish();
    }
}

/// A program transaction a proposal carried, which a nullified view's strike names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Carried {
    pub id: String,
    pub sender: String,
    /// The last block that can carry it: its strikes are forgotten after.
    pub valid_until: u64,
}

impl Carried {
    pub fn of(tx: &Tx) -> Carried {
        Carried { id: tx.id(), sender: tx.from.clone(), valid_until: tx.valid_until }
    }
}

/// Strikes that end a transaction's turn in this node's proposals (§6.2).
pub const STRIKES_TO_DROP: u32 = 2;
/// The most views whose proposals are remembered, and the most struck ids.
pub const MAX_STRIKE_VIEWS: usize = 64;
pub const MAX_STRUCK_IDS: usize = 4_096;

/// Strikes since the start, counted once per transaction per nullified view: the series
/// `blockula_program_uncertified_proposals_total`.
pub(super) static UNCERTIFIED: AtomicU64 = AtomicU64::new(0);

/// A leader that never faults on a transaction can still find that its proposals carrying it never
/// certify, because enough other validators fault on it and abstain. So when a proposal is made,
/// its program transaction ids are recorded with its epoch and view ([`Strikes::record`]); when
/// this node then executes a finalized block of that epoch whose `parent_view < v < view` for a
/// view `v` it proposed in, that view was nullified (the block's own fields say so) and each of
/// those ids gets a strike ([`Strikes::nullified`], [`Strikes::strike`]). Views restart at each
/// epoch (a new consensus engine runs it), so a record is keyed by both: a record of an earlier
/// epoch is settled without a strike when a block of a later one executes, and is the first to go
/// when the records are bounded. At [`STRIKES_TO_DROP`] strikes `fault::on_block` drops the
/// transaction from the mempool and holds its sender's program transactions at this node's door.
/// A view nullified for any other reason (a slow network) costs an innocent sender that, at this
/// node only.
#[derive(Debug, Default)]
pub struct Strikes {
    /// The program transactions each recent proposal of this node carried, by epoch and view.
    proposed: BTreeMap<(u64, u64), Vec<Carried>>,
    /// Struck ids with their strikes and the height they're forgotten at, oldest first.
    struck: VecDeque<(String, u32, u64)>,
}

impl Strikes {
    /// This node proposed in `view` of `epoch`, carrying `carried`: remembered until a finalized
    /// block settles that view. A proposal that carried no program transaction records nothing.
    pub fn record(&mut self, epoch: u64, view: u64, carried: Vec<Carried>) {
        if carried.is_empty() {
            return;
        }
        self.proposed.insert((epoch, view), carried);
        while self.proposed.len() > MAX_STRIKE_VIEWS {
            self.proposed.pop_first();
        }
    }

    /// The finalized block at `view` of `epoch`, whose parent was proposed in `parent_view`,
    /// executed: every view of this node's up to and including `view` (and every view of an
    /// earlier epoch, whose chain moved on) is settled, and what it carried in the views of
    /// `epoch` strictly between (nullified, since the chain went from `parent_view` to `view`
    /// without a block in them) is returned. A view at or below `parent_view` is settled without
    /// a strike: its block was this chain's or an earlier gap's.
    pub fn nullified(&mut self, epoch: u64, parent_view: u64, view: u64) -> Vec<Carried> {
        let later = self.proposed.split_off(&(epoch, view.saturating_add(1)));
        let settled = std::mem::replace(&mut self.proposed, later);
        settled.into_iter().filter(|((e, v), _)| *e == epoch && *v > parent_view && *v < view).flat_map(|(_, carried)| carried).collect()
    }

    /// One more strike for `carried`: how many it has now. Counted in the metric.
    pub fn strike(&mut self, carried: &Carried) -> u32 {
        UNCERTIFIED.fetch_add(1, Relaxed);
        let until = carried.valid_until.saturating_add(1);
        let n = self.struck.iter().find(|(id, _, _)| *id == carried.id).map_or(0, |(_, n, _)| *n).saturating_add(1);
        self.struck.retain(|(id, _, _)| *id != carried.id);
        self.struck.push_back((carried.id.clone(), n, until));
        while self.struck.len() > MAX_STRUCK_IDS {
            self.struck.pop_front();
        }
        n
    }

    /// Forgets what can no longer matter at `next`: strikes of transactions no block can carry.
    pub fn expire(&mut self, next: u64) {
        self.struck.retain(|(_, _, until)| next < *until);
    }

    /// How many strikes `id` has.
    #[cfg(test)]
    pub fn strikes_of(&self, id: &str) -> u32 {
        self.struck.iter().find(|(i, _, _)| i == id).map_or(0, |(_, n, _)| *n)
    }

    /// How many views' proposals are waiting to be settled.
    #[cfg(test)]
    pub fn pending_views(&self) -> usize {
        self.proposed.len()
    }
}

/// The most digests remembered as verified clean.
pub const MAX_VERIFIED_CLEAN: usize = 64;

/// The blocks this node's own `verify` applied without a fault, newest last (§6.2): the last
/// [`MAX_VERIFIED_CLEAN`] digests (a block's hash as `Chain::apply_block` gets it, hex), in memory
/// only. A `Tx` fault while executing a block in it is this node's doing (a transient panic, an
/// engine or instantiation error under memory pressure), not a sign it differs from the quorum:
/// `halt_for` classes it with `Setup`, exit 14, which a restart most likely cures. The set doesn't
/// survive the restart, so a second fault on the same block exits 13.
#[derive(Debug, Default)]
pub struct VerifiedClean(VecDeque<String>);

impl VerifiedClean {
    pub fn record(&mut self, digest: &str) {
        if self.contains(digest) {
            return;
        }
        self.0.push_back(digest.to_string());
        while self.0.len() > MAX_VERIFIED_CLEAN {
            self.0.pop_front();
        }
    }

    pub fn contains(&self, digest: &str) -> bool {
        self.0.iter().any(|d| d == digest)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Serializes the tests that read the strikes metric ([`UNCERTIFIED`] is process-wide).
#[cfg(test)]
pub(super) static STRIKE_METRIC: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[cfg(test)]
mod tests;
