//! Node faults on every path (docs/PROGRAMS-V0.md §6.2, §6.3): what a node does when it can't run
//! a program transaction as every other node does.
//!
//! A node fault (`state::Fault`, recorded by `programs::apply` in `state.programs.fault`) is
//! anything no valid transaction can cause: a panic, an engine error, stored code that won't load,
//! a thread the OS won't start. It's never a transaction's outcome (A11), and it sticks to the
//! `State` value that hit it, so each path below reads it once its run is over:
//!
//! | Path | On a fault |
//! |---|---|
//! | **Proposing** ([`Packing`]) | Leave the transaction out (its trial rolled back, the fault taken off the proposer's own copy) and go on packing, holding its sender's later transactions back. `Tx`: drop it, with its sender's later transactions, from this node's mempool once packing ends. `Setup`: skip the proposal's other program transactions. A fault replaying the parent's unexecuted ancestors: no proposal ([`faulted`]) |
//! | **Verifying** ([`faulted`]) | Abstain: after the ancestors' replay, on an unadmittable transaction while a fault is set (an earlier one's fault may be why), and after the loop |
//! | **Executing** a certified block, **replaying** the block log at startup ([`halt_on_fault`]) | Halt: `Tx` → `Halt::ProgramFault`, exit 13 (stays down); `Setup` → `Halt::ProgramResources`, exit 14 (restarts) ([`halt_for`]) |
//! | **Committing** ([`refuse_commit`]) | Refuse: the backstop of every path that commits a state |
//!
//! Each fault a path meets is logged with the transaction, the height and what happened, and
//! counted in `blockula_program_node_faults_total{path, class}` ([`metrics`]).
//!
//! The node-local safeguards beside them (WBS S4c; none of them consensus, §9.2): the quarantine
//! ([`Quarantine`]: what a fault in a proposer's trial keeps out of this node's mempool and out of
//! its votes), the trial marker ([`TrialMarker`]: what a crash remembers), strikes ([`Strikes`]:
//! proposals that never certified), the blocks this node verified clean ([`VerifiedClean`], which
//! [`halt_for`] consults), and the totals check ([`check_totals`]).

use super::is_program_tx;
use super::state::{Fault, FaultClass};
use crate::chain::{Chain, State};
use crate::types::Tx;
use crate::upgrade::{Halt, HaltSignal};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

mod marker;
mod quarantine;
mod totals;
pub use marker::{recover, Record, Trial, TrialMarker};
pub use quarantine::{Carried, Quarantine, Strikes, VerifiedClean, STRIKES_TO_DROP};
pub use totals::{accounting_drift, check_totals};

/// Where a node fault was met, for the metric and the logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// A proposer's packing trials, or its replay of the parent's unexecuted ancestors.
    Propose,
    /// Verification: the replay of the block's unexecuted ancestors, or the block's own transactions.
    Verify,
    /// Executing a certified block (`Chain::apply_block`).
    Execute,
    /// Replaying the block log at startup (`Chain::replay`).
    Startup,
}

impl Path {
    const ALL: [Path; 4] = [Path::Propose, Path::Verify, Path::Execute, Path::Startup];

    /// Its label in the metric, and its row in [`FAULTS`].
    fn label(self) -> (&'static str, usize) {
        match self {
            Path::Propose => ("propose", 0),
            Path::Verify => ("verify", 1),
            Path::Execute => ("execute", 2),
            Path::Startup => ("startup", 3),
        }
    }
}

const CLASSES: [FaultClass; 2] = [FaultClass::Tx, FaultClass::Setup];

/// A class's label in the metric, and its column in [`FAULTS`].
fn class_label(class: FaultClass) -> (&'static str, usize) {
    match class {
        FaultClass::Tx => ("tx", 0),
        FaultClass::Setup => ("setup", 1),
    }
}

/// This process's node faults, by path and class (a node runs one chain). Node-local, never state.
static FAULTS: [[AtomicU64; CLASSES.len()]; Path::ALL.len()] = [const { [const { AtomicU64::new(0) }; CLASSES.len()] }; Path::ALL.len()];

fn counter(path: Path, class: FaultClass) -> &'static AtomicU64 {
    &FAULTS[path.label().1][class_label(class).1]
}

/// How many node faults of `class` this process has met on `path`.
pub fn count(path: Path, class: FaultClass) -> u64 {
    counter(path, class).load(Relaxed)
}

/// Logs and counts a node fault met on `path` while judging, proposing or executing block
/// `height`, with what the node does about it (`then`).
fn met(path: Path, height: u64, fault: &Fault, then: &str) {
    counter(path, fault.class).fetch_add(1, Relaxed);
    tracing::error!(path = path.label().0, height, tx = %fault.tx, at = fault.height, class = ?fault.class, fault = %fault.what, "a program execution fault on this node: {then}");
}

/// `/metrics`' program safeguard series (`programs::api::metrics` prints them once programs exist):
/// `blockula_program_node_faults_total{path, class}`, this process's counts; the quarantine's size
/// by kind (`blockula_program_quarantined{kind}`); the strikes proposals have taken
/// (`blockula_program_uncertified_proposals_total`); whether the last totals check found a drift
/// (`blockula_program_accounting_drift`); and the replay cache's hits (`blockula_replay_cache_hits_total`).
pub fn metrics() -> String {
    use std::fmt::Write as _;
    let mut out = String::from("# HELP blockula_program_node_faults_total Program execution faults on this node (never an outcome), by path and class\n# TYPE blockula_program_node_faults_total counter\n");
    for path in Path::ALL {
        for class in CLASSES {
            let _ = writeln!(out, "blockula_program_node_faults_total{{path=\"{}\",class=\"{}\"}} {}", path.label().0, class_label(class).0, count(path, class));
        }
    }
    out.push_str("# HELP blockula_program_quarantined What this node's quarantine holds after program execution faults, by kind (tx ids, senders, upload code hashes)\n# TYPE blockula_program_quarantined gauge\n");
    for (kind, gauge) in ["tx", "sender", "code"].into_iter().zip(&quarantine::QUARANTINED) {
        let _ = writeln!(out, "blockula_program_quarantined{{kind=\"{kind}\"}} {}", gauge.load(Relaxed));
    }
    for (name, kind, help, value) in [
        ("blockula_program_uncertified_proposals_total", "counter", "Strikes: program transactions this node proposed in a view that never certified", quarantine::UNCERTIFIED.load(Relaxed)),
        ("blockula_program_accounting_drift", "gauge", "1 if the last epoch-boundary check found a program total (value held, sponsored, escrow) that differs from its sum", u64::from(accounting_drift())),
        ("blockula_replay_cache_hits_total", "counter", "Ancestor replays that started from a cached post-state instead of running the block", crate::consensus::replay_cache_hits()),
    ] {
        let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}");
    }
    out
}

/// The halt a fault calls for while executing block `height` (§6.2): `Tx` → `ProgramFault` (exit
/// 13, an operator looks first: a quorum executed the block without a fault, so this node differs
/// from them), `Setup` → `ProgramResources` (exit 14: a restart may cure it).
///
/// The one exception is a block this node's own verification ran clean: `executing` is the
/// verified-clean set and the executed block's digest, `None` where the set can't know the block
/// (startup replay: the set doesn't survive a restart, so a second fault on the same block exits
/// 13). A `Tx` fault on a block in it is this node's doing (a transient panic, an engine or
/// instantiation error under memory pressure), not a sign it differs from the quorum, and a
/// restart most likely executes the block: it's classed with `Setup`.
pub fn halt_for(fault: &Fault, height: u64, executing: Option<(&VerifiedClean, &str)>) -> Halt {
    match fault.class {
        FaultClass::Tx if executing.is_some_and(|(clean, digest)| clean.contains(digest)) => Halt::ProgramResources { height, fault: format!("this node verified block {height} without a fault, then faulted executing it (tx {}: {}): restarting once", fault.tx, fault.what) },
        FaultClass::Tx => Halt::ProgramFault { height, tx: fault.tx.clone(), fault: fault.what.clone() },
        FaultClass::Setup => Halt::ProgramResources { height, fault: fault.what.clone() },
    }
}

/// After block `height` ran on `state` on `path` (executing a certified block, or replaying the
/// block log at startup): if a program faulted, the node halts (§6.2). The halt goes into `signal`
/// (which `node::watch_halts` turns into the exit, as for every halt) and is the error, so the
/// block is never committed. `Ok` otherwise. `executing` is as for [`halt_for`].
pub fn halt_on_fault(state: &State, path: Path, height: u64, signal: &HaltSignal, executing: Option<(&VerifiedClean, &str)>) -> anyhow::Result<()> {
    let Some(fault) = &state.programs.fault else { return Ok(()) };
    let halt = halt_for(fault, height, executing);
    met(path, height, fault, &format!("halting with exit code {}", halt.exit_code()));
    signal.set(halt.clone());
    Err(halt.into())
}

/// `State::commit_block`'s backstop: a state that hit a node fault ran a block otherwise than the
/// chain did, so it's never committed, whichever path got it there. The error is the fault's halt.
pub fn refuse_commit(state: &State) -> anyhow::Result<()> {
    match &state.programs.fault {
        Some(fault) => Err(halt_for(fault, fault.height, None).into()),
        None => Ok(()),
    }
}

/// Whether `state`, which `path` ran to judge or build on block `height`, hit a node fault: then
/// this node can't judge it, so verification abstains and a proposer doesn't propose on it (§6.2),
/// and the fault is logged and counted.
pub fn faulted(state: &State, path: Path, height: u64) -> bool {
    let Some(fault) = &state.programs.fault else { return false };
    met(path, height, fault, if path == Path::Propose { "not proposing on a parent this node can't replay" } else { "abstaining" });
    true
}

/// The most `Tx` faults one proposal's packing tries, after which it stops trying program
/// transactions as a `Setup` fault does (§6.3). A faulted trial is rolled back, so its fuel doesn't
/// count against the block's fuel budget, and the packing holds the chain lock while it runs: the
/// quarantine lets each faulting id be tried once and its sender's program transactions not be
/// tried again, but a flood of funded senders each with a transaction that faults would still make
/// one proposal try them all. This bounds that to eight trials a proposal; the rest wait in the
/// mempool, and the next proposals try eight more.
pub const MAX_FAULTED_TRIALS: usize = 8;

/// A proposal's packing, as far as node faults go (§6.2's first row, §6.3): the transactions whose
/// trial faulted, which it leaves out, whether a `Setup` fault, or too many `Tx` ones, hold the
/// rest of its program transactions back, and the program transactions it carries.
#[derive(Default)]
pub struct Packing {
    /// The transactions whose trial faulted, in packing order, with their faults.
    faulted: Vec<(Tx, Fault)>,
    /// A trial met a `Setup` fault: this node can't run programs for now.
    setup: bool,
    /// The program transactions the proposal carries, for strikes.
    carried: Vec<Carried>,
}

impl Packing {
    /// Whether packing skips `tx` untried: a program transaction, once a `Setup` fault showed this
    /// node can't run one, or [`MAX_FAULTED_TRIALS`] `Tx` faults have been met (the caller holds
    /// its sender's later transactions back too).
    pub fn skips(&self, tx: &Tx) -> bool {
        is_program_tx(tx) && (self.setup || self.faulted.iter().filter(|(_, f)| f.class == FaultClass::Tx).count() >= MAX_FAULTED_TRIALS)
    }

    /// After `tx`'s trial on `state`, the proposer's own copy, for block `height`: whether it met
    /// a node fault, which this takes off the state (`Option::take`), logs and counts. The caller
    /// then rolls the trial back and leaves `tx` out: this proposal commits nothing, so leaving
    /// it out claims nothing about it.
    pub fn take_fault(&mut self, state: &mut State, tx: &Tx, height: u64) -> bool {
        let Some(fault) = state.programs.fault.take() else { return false };
        met(Path::Propose, height, &fault, "leaving the transaction out of this proposal");
        self.setup |= fault.class == FaultClass::Setup;
        self.faulted.push((tx.clone(), fault));
        true
    }

    /// `tx` goes into the proposal: a program transaction is remembered for strikes ([`Strikes`]).
    pub fn carries(&mut self, tx: &Tx) {
        if is_program_tx(tx) {
            self.carried.push(Carried::of(tx));
        }
    }

    /// Once packing ends, with the chain lock no longer held for it: the transactions whose trial
    /// met a `Tx` fault are quarantined and dropped from `chain`'s mempool with their senders'
    /// later ones as of block `height` (`Chain::quarantine_faulted`; nothing to do, and no lock
    /// taken, when no trial faulted). A `Setup` fault is this node's trouble, not the
    /// transaction's: it stays queued. Returns the program transactions the proposal carries, for
    /// [`Packing::record`] once the proposal is made.
    pub fn finish(self, chain: &parking_lot::Mutex<Chain>, height: u64) -> Vec<Carried> {
        if !self.faulted.is_empty() {
            chain.lock().quarantine_faulted(&self.faulted, height);
        }
        self.carried
    }

    /// The proposal for block `height` made in `view` carries `carried` ([`Packing::finish`]):
    /// remembered, with the epoch of `height`, until a finalized block shows whether the view was
    /// nullified (strikes). Called when the block is returned, so a proposal that is never made
    /// (the checkpoint wait gave up) strikes nothing.
    pub fn record(chain: &parking_lot::Mutex<Chain>, height: u64, view: u64, carried: Vec<Carried>) {
        if !carried.is_empty() {
            let mut c = chain.lock();
            let epoch = c.state.params.epoch_of(height);
            c.strikes.record(epoch, view, carried);
        }
    }
}

/// What executing the finalized block at `height`, proposed in `view` on a parent proposed in
/// `parent_view`, does to the safeguards (`Chain::apply_block`, after the block's own transactions
/// left the mempool). Entries that no longer hold go. Each program transaction this node proposed
/// in a view the chain skipped (`parent_view < v < view`) and that still waits in the mempool gets
/// a strike; at [`STRIKES_TO_DROP`] it is dropped (with its sender's later transactions) and its
/// sender's program transactions are held for [`quarantine::QUARANTINE_HOLD_BLOCKS`] (§6.2). The strike
/// counts stay until the transaction can no longer be carried, so one resubmitted after the hold is
/// dropped again at its next strike. The views are those of the epoch `height` is in: views
/// restart at each epoch.
pub fn on_block(chain: &mut Chain, height: u64, view: u64, parent_view: u64) {
    let next = height.saturating_add(1);
    chain.quarantine.expire(next);
    chain.strikes.expire(next);
    let epoch = chain.state.params.epoch_of(height);
    let mut dropped = vec![];
    for tx in chain.strikes.nullified(epoch, parent_view, view) {
        // Included elsewhere, expired or replaced since: nothing left to give up on.
        if !chain.in_mempool(&tx.id) {
            continue;
        }
        let strikes = chain.strikes.strike(&tx);
        tracing::warn!(tx = %tx.id, strikes, height, "a proposal carrying this program transaction never certified (its view was nullified)");
        if strikes >= STRIKES_TO_DROP {
            chain.quarantine.hold_sender(&tx.sender, next);
            dropped.push((tx.id, "not certified in two proposals: other validators may be faulting on it".to_string()));
        }
    }
    chain.drop_from_mempool((height, dropped));
}

/// Whether the block at `block_height`, which carries the transactions `in_block` says it does (by
/// id), carries one `chain`'s quarantine holds: this node abstains on it before replaying anything
/// (§6.2). Logged. What's walked, under the chain lock, is the quarantine's ids (at most
/// [`quarantine::MAX_QUARANTINED_IDS`]), each asked of the block, not each of the block's
/// transactions against all of them.
pub fn quarantined(chain: &parking_lot::Mutex<Chain>, in_block: impl Fn(&str) -> bool, block_height: u64) -> bool {
    let c = chain.lock();
    let Some(id) = c.quarantine.holds_id(in_block, c.height().saturating_add(1)) else { return false };
    tracing::warn!(tx = %id, height = block_height, "abstaining on a block that carries a transaction this node quarantined after a program execution fault");
    true
}

/// A verification of the block `digest` (hex) applied all of `txs` on `state`, the state it
/// verified on, without a fault: remembered as verified clean ([`VerifiedClean`], §6.2), but only
/// where programs exist (META in `state`), so a v1 node runs today's code, and only for a block
/// that carries a program transaction, the only kind a `Tx` fault can happen in (so blocks
/// without one don't push the digests that matter out of the last 64).
pub fn verified(chain: &parking_lot::Mutex<Chain>, state: &State, txs: &[Tx], digest: &str) {
    if state.programs.meta.is_some() && txs.iter().any(is_program_tx) {
        chain.lock().verified_clean.record(digest);
    }
}

#[cfg(test)]
mod tests;
// The tests of other modules that make faults the counters count take it too (`pass/tests.rs`).
#[cfg(test)]
pub(crate) use tests::COUNTING;
#[cfg(test)]
mod safeguards;
