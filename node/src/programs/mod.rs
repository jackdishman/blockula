//! Programs v0 in the node (protocol v2; docs/PROGRAMS-V0.md, which amends docs/PROGRAMS.md):
//! third-party WebAssembly programs that anyone can upload (`UploadCode`, tag 62), create
//! (`CreateProgram`, 63) and call (`CallProgram`, 64), and that f + 1 validators can pause
//! (`ProgramsPause`, 69). Tags 65–68 and 70 keep the meanings docs/PROGRAMS.md gives them but
//! aren't decoded in v0. The VM itself (upload validation, the engine, the host ABI, metering and
//! the storage overlay) is the `blockula-vm` crate in `node/vm`; this module is the node's side of
//! it: admission, fees, the program state, execution and its faults, and the training wheels.
//! Programs, never contracts.
//!
//! **v2 = v1 + dormant programs.** Protocol v2 changes nothing in state or behaviour until the
//! `programs` feature first switches on: when an `ActivateFeature { programs }` executes (on
//! `blockula-1` only from protocol v3, `upgrade::check_may_activate`), or at block 0 of a chain
//! born at v2 whose genesis lists the feature. That creates the program state (META `programs`),
//! which is never deleted. Until then:
//! - [`status`] is [`Status::Dormant`] at every height, so admission refuses every program tag
//!   (`Err`: the transaction can't be included and nobody is charged), and a block carrying one
//!   fails verification;
//! - every hook this module has elsewhere in the node is inert (a lookup that finds nothing, a term
//!   that adds 0, a step that returns at once), and each is gated on [`status`], never on "no
//!   program exists", so a later change can't wake one early;
//! - no program record exists, so states, roots, snapshots and the store are exactly v1's.
//!
//! So one binary runs a v1 chain (testnet-2 has no upgrade schedule: `Dormant` forever), a chain
//! born at v2 with programs off, and the activation later. `blockula replay` checks the first
//! against testnet-2's recorded history (docs/REPLAY.md).
//!
//! **One gate.** Every program rule asks [`status`] at the height the transaction executes at:
//!
//! | [`Status`] | When | Program transactions admitted |
//! |---|---|---|
//! | `Dormant` | below v2, or programs never switched on (no META `programs`) | none |
//! | `On` | the feature on, no pause running | all four tags |
//! | `ExitOnly` | switched on once, off since (META kept, the feature off) | `ProgramsPause`, and calls carrying no value (one whose commit would raise the FANG programs control fails) |
//! | `Paused { until }` | f + 1 validators signed `ProgramsPause`; lapses on its own at `until` | `ProgramsPause` only, which renews it |
//!
//! State (namespaces `state::ns` 66–69 and 72; 70, 71 and 73–79 are reserved for later program
//! actions), all of it absent until programs first switch on:
//!
//! | Namespace | Key → value |
//! |---|---|
//! | 66 `PROGRAMS` | program address → its record: creator, code and interface, storage totals, deposits |
//! | 67 `PROGRAM_CODE` | code hash → the Wasm and its method names, written once |
//! | 68 `PROGRAM_INTERFACES` | interface hash → the interface, written once |
//! | 69 `PROGRAM_STORAGE` | program address bytes ‖ key → an entry: value, payer, deposit, capacity |
//! | 72 `PROGRAM_BLOB_REFS` | kind ‖ hash → a blob's uploader, deposit, size and references |
//! | META `programs` | the parameters, the totals (state bytes, value held, sponsored, escrow), the fuel rate, the pause |

use crate::chain::State;
use crate::types::{Action, Tx};
use crate::upgrade;
use blockula_vm::meter::MAX_RESERVATION;

// The codec's side (WBS S1; the tags themselves are `types::Action`'s): admission's refusals while
// programs are dormant, the digests that name code, interfaces, versions, programs and methods,
// and the transactions' summaries.
mod admit;
mod describe;
pub mod digest;
pub use admit::{admit, refuse_early};
pub use describe::describe;

/// What programs may do at a height (docs/PROGRAMS-V0.md §1.1). Every program rule reads it
/// through [`status`], so the answer is computed in one place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Below protocol v2, or programs were never switched on (no META `programs`): every program
    /// tag is refused, and every hook is inert.
    Dormant,
    /// The `programs` feature is on and no pause is running.
    On,
    /// Programs were switched on once and are off now (META `programs` kept, the feature off): only
    /// what lets FANG leave programs runs (§7.3).
    ExitOnly,
    /// A `ProgramsPause` reached its threshold: nothing new runs from its first block until `until`
    /// (exclusive), whether the feature is on or not (§7.2).
    Paused { until: u64 },
}

/// The [`Status`] of programs for a transaction that executes at `height` on `state`. `Dormant`
/// below [`upgrade::PROGRAMS_VERSION`] and wherever META `programs` is absent, whatever else the
/// state holds: the feature flag alone wakes nothing. Then a pause running at `height` (`from ≤
/// height < until`) wins, whether the feature is on or not (§7.2); past it, the feature decides
/// between `On` and `ExitOnly` (§1.4). The `ActivateFeature` that creates META runs at the end of
/// its block, so its own block's transactions still see `Dormant` and the next block's see `On`.
pub fn status(state: &State, height: u64) -> Status {
    if upgrade::version_at(state, height) < upgrade::PROGRAMS_VERSION {
        return Status::Dormant;
    }
    // From v2, programs exist once they've been switched on, which created META `programs`.
    let Some(meta) = &state.programs.meta else { return Status::Dormant };
    // The last run of pauses stays in META after it lapses (§7.2): only a running one counts.
    if let Some(pause) = meta.pause.as_ref().filter(|p| p.from <= height && height < p.until) {
        return Status::Paused { until: pause.until };
    }
    if upgrade::is_active(state, upgrade::PROGRAMS_FEATURE) {
        Status::On
    } else {
        Status::ExitOnly
    }
}

/// Whether `tx` is a program transaction: one that runs program code or bonds program deposits,
/// and pays for the fuel it reserves. That's tags 62, 63 and 64 (§5.1, §6.3); not
/// `ProgramsPause` (69), a validator duty that reserves no fuel and that a proposer leaves out
/// when it fails, like any other.
pub fn is_program_tx(tx: &Tx) -> bool {
    matches!(tx.action, Action::UploadCode { .. } | Action::CreateProgram { .. } | Action::CallProgram { .. })
}

/// Whether `address` is a program's account (namespace 66 has its record). A program has no keys,
/// so admission refuses any transaction from one (§2.1). Records are keyed by the address as
/// accounts are, so this is one lookup; while programs are dormant there are none
/// (`state::check_invariants`: no META, no records).
pub fn is_program(state: &State, address: &str) -> bool {
    state.programs.records.contains_key(address)
}

/// What a program record (namespace 66) adds to program state's bytes, and so what its creator
/// bonds a deposit for at the storage price of the moment (§2.3, §4.6; docs/PROGRAMS.md 6.3).
pub const RECORD_BYTES: u64 = 256;

/// The codec's size bound on an `UploadCode` transaction, in wire bytes (§2.2, §5.3;
/// `Tx::max_bytes`): every other non-ghost transaction has `types::MAX_TX_BYTES`.
pub const MAX_CODE_TX_BYTES: usize = 320 * 1024;
/// The largest upload fits: its code and interface at their limits, the action's tag, two u32
/// lengths and `storage_limit`, and a transaction's envelope with every field at its bound (the
/// domain, chain id, genesis hash, sender, four u64s, memo, key and signature).
const _: () = {
    use crate::crypto::{PUBLIC_KEY_LEN, SIGNATURE_LEN};
    use crate::types::{MAX_ADDRESS, MAX_CHAIN_ID, MAX_MEMO, TX_DOMAIN};
    use blockula_vm::config::{MAX_CODE_BYTES, MAX_INTERFACE_BYTES};
    let action = 1 + 4 + MAX_CODE_BYTES + 4 + MAX_INTERFACE_BYTES + 8;
    let envelope = TX_DOMAIN.len() + (2 + MAX_CHAIN_ID) + 32 + (2 + MAX_ADDRESS) + 4 * 8 + (2 + MAX_MEMO) + PUBLIC_KEY_LEN + SIGNATURE_LEN;
    assert!(action + envelope <= MAX_CODE_TX_BYTES);
    // And the paths that carry one already take it: the wire decoder's first bound (also a
    // consensus block's per-transaction one), and `POST /api/tx`'s body as JSON, where the bytes
    // travel as hex.
    assert!(MAX_CODE_TX_BYTES <= crate::types::MAX_GHOST_TX_BYTES);
    assert!(2 * MAX_CODE_TX_BYTES <= crate::api::MAX_TX_BODY_BYTES);
};

/// The fuel rate's ceiling, in milli-drops per fuel (§5.3, §5.6): the highest rate at which no
/// reservation (at most [`MAX_RESERVATION`] fuel) costs more than a u64 holds, as
/// `econ::MAX_RATE` bounds the byte rate. Sustained full blocks take the rate there from the
/// default floor in about 177 blocks.
pub const MAX_FUEL_RATE: u64 = u64::MAX / MAX_RESERVATION;
const _: () = assert!(MAX_FUEL_RATE as u128 * MAX_RESERVATION as u128 <= u64::MAX as u128);

// Program state and switching programs on (WBS S2): the records, their invariants, the
// parameters, and what creates them.
mod activate;
pub mod params;
pub mod state;
pub use activate::on_activate;
pub use params::{check_param, is_program_param, set_param, COUNCIL_PARAMS};
pub use state::{check_bounds, check_invariants, escrow, Programs, ProgramsJournal};

// The training wheels (WBS S5): the validators' pause, and the guard on native credits to programs
// with the value cap on the FANG programs control.
mod credit;
pub mod pause;
pub use credit::{check_credit, native_credit, Credit};

// Fees, the fuel rate and the block's budget, and the mempool's side (WBS S3): what a program
// transaction reserves and pays, the rate congestion moves, proposal order's share, and the
// worst-case debits that keep the mempool from ever running a program.
pub mod fuel;
mod mempool;
pub use fuel::{count_fuel, end_block, fuel_fee, lowest_fuel_fee, share};
pub use mempool::{mempool_admit, mempool_apply};
#[cfg(test)]
mod fixtures;
// The proposer's rule for program transactions that fail (`consensus.rs`'s `propose`).
#[cfg(test)]
mod proposer_tests;

// Execution (WBS S4a): a program transaction's action in a block. The view of the state its VM
// work reads, the thread and the engine it runs on, what a successful run commits, and the three
// handlers.
mod call;
pub mod commit;
mod create;
mod exec;
mod pass;
mod upload;
mod view;
pub use pass::block_pass;
// Receipts and node faults (WBS S4b): what a block's program transactions did, and what a node
// does on each path when it can't run one as every other node does.
pub mod fault;
pub use fault::check_totals;
pub mod receipt;
#[cfg(test)]
pub(crate) mod probe;
// The node's benches (WBS S4c), `#[ignore]`d.
#[cfg(test)]
mod bench;

/// Applies `tx`, a program transaction from `sender`, in the block at `height`
/// (docs/PROGRAMS-V0.md §6.1): `State::apply_action`'s arm for tags 62–64 and 69, which runs after
/// the transaction's fee phase, so on the state its VM work reads. Tags 62–64 get here only in
/// blocks (a proposer's run, verification, execution, replay): the mempool never runs a program
/// (§6.4). A `ProgramsPause` (69) runs no program: it counts its validator's signature (§7.2,
/// [`pause::sign`]), in the mempool's dry runs too.
///
/// `id`: `tx`'s id ([`Tx::id`]), which names a program transaction's run (its receipt's key, a
/// fault's record, the logs), if the caller already has it: a proposer's trial and a verification
/// hash it to name the transaction in the trial marker and hand it on. `None` (a block's run, the
/// replay) has it computed here, once. Test builds check a given one.
///
/// `Ok` once the action applied. Otherwise `Err`, which `apply_j` makes the transaction's
/// `Failed` (charged, the action journal rolled back), in one of two ways:
/// - the transaction **failed**, the same on every node: the reason says why (a trap, an abort, a
///   limit, a deposit past `storage_limit`, the value cap…);
/// - the node **faulted** (§6.2): the fault is recorded in `state.programs.fault`, where it stays
///   for the life of this state, so whatever outcome `apply_j` reports is never kept, and every
///   later program action on this state returns `Err` at once and runs nothing.
pub fn apply(state: &mut State, j: &mut crate::chain::Journal, tx: &Tx, id: Option<&str>, sender: &str, signer: &str, height: u64) -> anyhow::Result<()> {
    if let Some(f) = &state.programs.fault {
        anyhow::bail!("program execution faulted on this node at block {} (tx {}): no program runs on this state", f.height, crate::chain::short(&f.tx));
    }
    // Admission checked the signer and §7.2's limits a moment ago in the same `apply_j` (it can't
    // nest); `sign` checks them again, then counts the vote.
    if let Action::ProgramsPause {} = tx.action {
        return pause::sign(state, j, sender, signer, height);
    }
    // §6.1 step 1: admission's checks, on the state the action runs on. Admission ran them a
    // moment ago in the same `apply_j` (no program action nests, A8), so this never fails; it's
    // here so that a later path into the action phase can't skip them.
    if !may_run(state, &tx.action, height) {
        anyhow::bail!("programs don't run {} transactions at block {height} ({:?})", tx.action.kind(), status(state, height));
    }
    // Test builds hold every caller to what it says: a wrong id would key the receipt, the fault
    // and the logs to another transaction.
    #[cfg(test)]
    if let Some(id) = id {
        assert_eq!(id, tx.id(), "a caller handed programs::apply another transaction's id");
    }
    let run = exec::Run::new(state, tx, id.map_or_else(|| tx.id(), str::to_string), height);
    let ran = match &tx.action {
        Action::UploadCode { .. } => upload::apply(state, j, &run, tx, sender),
        Action::CreateProgram { .. } => create::apply(state, j, &run, tx, sender),
        Action::CallProgram { .. } => call::apply(state, j, &run, tx, sender),
        _ => anyhow::bail!("this release can't apply {} transactions yet", tx.action.kind()),
    };
    // Its receipt (§8.1), applied or failed: a node fault is no outcome and gets none.
    receipt::record(state, tx, sender, &run, &ran);
    match ran {
        Ok(()) => Ok(()),
        Err(exec::Stop::Failed(failure)) => Err(anyhow::anyhow!(failure.reason(state.params.prefix()))),
        Err(exec::Stop::Faulted(fault)) => {
            tracing::error!(height, tx = %run.tx, class = ?fault.class, fault = %fault.what, "program execution faulted on this node: never this transaction's outcome");
            let what = fault.what.clone();
            // Whatever raised it, the block pass's engine goes with it: the next program transaction
            // gets a new one (A3, §6.1 step 4).
            pass::discard_engine();
            state.programs.fault = Some(state::Fault { height, tx: run.tx, class: fault.class, what: fault.what });
            anyhow::bail!("program execution faulted on this node ({what}): never this transaction's outcome")
        }
    }
}

/// Whether `action` may run at `height` by programs' status (§1.1): an upload, a create or a call
/// while programs are on; in exit-only, a call carrying no value (§7.3).
fn may_run(state: &State, action: &Action, height: u64) -> bool {
    match status(state, height) {
        Status::On => matches!(action, Action::UploadCode { .. } | Action::CreateProgram { .. } | Action::CallProgram { .. }),
        Status::ExitOnly => matches!(action, Action::CallProgram { calls, .. } if calls.iter().all(|c| c.value == 0)),
        Status::Dormant | Status::Paused { .. } => false,
    }
}

// The read API (WBS S6a): the program routes, the sections they add to the node's existing
// endpoints once programs exist, and interfaces' JSON form.
pub mod api;
pub mod interface_json;
// `POST /api/program/{address}/view` (WBS S6b): a program's read-only methods, run on the node.
pub mod view_api;
// A program transaction's receipt as `/api/tx` serves it (WBS S6b).
pub mod tx_json;
#[cfg(test)]
mod summon_tests;

#[cfg(test)]
mod tests;
