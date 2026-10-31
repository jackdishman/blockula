//! Running a program transaction's VM work (docs/PROGRAMS-V0.md §6.1 steps 3–5, A3, A4), and the
//! two ways a program transaction stops short (§6.2, §8.1): it **fails** ([`Failure`]: the same on
//! every node, charged, its action rolled back), or the node **faults** ([`Faulted`]: something no
//! valid transaction can cause, never an outcome).
//!
//! **A thread and an engine, per block pass or per transaction.** Each program transaction's VM
//! work (a create's `init`, a call batch, an upload's checks on new code) runs under
//! `catch_unwind` on a thread with a [`PROGRAM_STACK_BYTES`] stack (host calls nest native frames up
//! to `MAX_CALL_DEPTH` deep), which the action phase waits for. Which thread and which engine is
//! the node's business, not consensus (A3):
//! - **in a block pass** that holds a program transaction (`pass::block_pass`: executing a block,
//!   replaying an ancestor, a proposer's packing, a verification, the startup replay), the whole
//!   pass runs on one such thread, so each transaction's work runs inline on it, and on the pass's
//!   one engine (`pass::with_engine`): every distinct declared code loaded in hash order before
//!   anything runs, as always, with code the engine already holds a no-op. The engine is dropped
//!   after any fault, and when it has taken in too much code, so no fault, and no earlier
//!   transaction, can change what the next one does (`pass.rs`);
//! - **anywhere else** (`/view`, a lone `State::apply`, a pass the OS refused a thread for), each
//!   transaction spawns a thread inside `std::thread::scope` and joins it, and on it makes a fresh
//!   engine (`new_block_engine`, under `meter::schedule` of the protocol version in force), loads
//!   the declared code in hash order and then runs `vm::exec::execute`; the engine is dropped with
//!   the thread.
//!
//! Either way the same function loads and executes ([`load_and_execute`]).
//!
//! **Faults, by class** (§6.2, [`FaultClass`]):
//! - `Setup`, this node couldn't make what every program transaction needs: a thread the OS
//!   refused three times in a row (a refusal is retried twice at once), or an engine that wouldn't
//!   build (`new_block_engine`'s `Err`, or `upload::check`'s `NodeFault::Engine`, its throwaway
//!   engine);
//! - `Tx`, tied to the transaction: every other `NodeFault` from `execute` or `upload::check`, a
//!   `LoadError` on stored code, a panic on the program thread (caught here, or past every
//!   backstop, at `join`), stored records that don't hold together.
//!
//! The mempool never gets here (§6.4): test builds count the threads started and the engines built
//! (`pass::counts`), so tests can check the mempool's paths never do.

use super::pass;
use super::receipt::Notes;
use super::state::FaultClass;
use super::view::ProgramView;
use crate::chain::{short, State};
use crate::types::Tx;
use crate::upgrade;
use blockula_vm::engine::{Address, BlockEngine, CodeHash, NodeFault};
use blockula_vm::exec::{Binding, Call, FailCode, Receipt, TxLimits};
use blockula_vm::interface::Mismatch;
use blockula_vm::meter::{self, Schedule};
use blockula_vm::upload::{self, Refusal};
use blockula_vm::validate::{ModuleInfo, ValidateError};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// The program thread's stack: room for `MAX_CALL_DEPTH` nested frames of native host calls, the
/// engines' own recursion included (docs/PROGRAMS.md 4.3).
pub const PROGRAM_STACK_BYTES: usize = 64 << 20;

/// A spawn the OS refuses is tried twice more at once (§6.1 step 3): three in all.
pub(super) const SPAWN_ATTEMPTS: u32 = 3;

/// One program transaction's run: which transaction (its id, for a fault's record and logs), the
/// height it executes at, the protocol version in force there, whose fuel schedule it runs under,
/// and what it did, for its receipt.
pub struct Run {
    pub tx: String,
    pub height: u64,
    pub version: u32,
    /// What the transaction's handler did, noted as it goes, for its receipt (§8.1,
    /// `receipt::record`). The handlers take the run by reference, so the notes sit in a cell,
    /// written only on the thread that runs the action.
    pub notes: RefCell<Notes>,
}

impl Run {
    /// `id` is `tx`'s id ([`Tx::id`]), a hash of the whole transaction that `programs::apply`
    /// has from its caller or computes once: it names the run in a fault's record and in logs,
    /// and keys its receipt (`receipt::record`).
    pub fn new(state: &State, tx: &Tx, id: String, height: u64) -> Run {
        Run { tx: id, height, version: upgrade::version_at(state, height), notes: RefCell::new(Notes::new(state, tx, height)) }
    }

    /// The fuel schedule blocks at this version run programs under (`meter::schedule`: none
    /// below v2). Programs run only from v2 (`programs::status`), so its absence is a node fault.
    fn schedule(&self) -> Result<&'static Schedule, Faulted> {
        meter::schedule(self.version).ok_or_else(|| Faulted::tx(format!("no fuel schedule for protocol v{}", self.version)))
    }
}

/// How a program transaction's action stopped short.
#[derive(Debug)]
pub enum Stop {
    /// It failed: charged, its action rolled back (`Outcome::Failed`).
    Failed(Failure),
    /// The node faulted: never the transaction's outcome (§6.2).
    Faulted(Faulted),
}

impl From<Failure> for Stop {
    fn from(f: Failure) -> Stop {
        Stop::Failed(f)
    }
}

impl From<Faulted> for Stop {
    fn from(f: Faulted) -> Stop {
        Stop::Faulted(f)
    }
}

/// Why a program transaction failed: §8.1's outcomes, the same on every node. Numbers and the
/// chain's own words only, never an engine's or the host's text (a program's abort message is in
/// its `FailCode`, for the receipt, and in no reason). `super::receipt` numbers them for the
/// receipt and renders each one's reason (`Failure::reason`), side by side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The VM's failed receipt: trapped (1), aborted (2), or its commit checks (3).
    Vm(FailCode),
    /// The FANG programs control would rise past `program_value_cap` (10, §7.1).
    ValueCap { cap: u64, control: u128 },
    /// Programs are exit-only, and the control would rise (11, §7.3).
    ExitOnly,
    /// An upload's or a create's own deposits pass its `storage_limit` (12).
    StorageLimit { deposits: u128, limit: u64 },
    /// An upload or a create would take program state past its ceiling (13, §4.6).
    StateFull,
    /// Upload validation refused the code (14): its rule, never its message.
    InvalidCode(ValidateError),
    /// The test translation refused the code (15).
    TranslationRefused,
    /// The interface's methods aren't the code's `bk_` exports (16).
    InterfaceMismatch(Mismatch),
    /// A native check failed (20): funds for deposits, an account that can't be adopted, a record
    /// that doesn't add up. Its wording is the reason.
    Native(String),
}

impl Failure {
    pub fn native(e: impl fmt::Display) -> Failure {
        Failure::Native(e.to_string())
    }
}

/// A node fault (§6.2): its class, and what happened, for logs. `programs::apply` records it as
/// the state's `programs.fault`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Faulted {
    pub class: FaultClass,
    pub what: String,
}

impl Faulted {
    pub fn tx(what: impl fmt::Display) -> Faulted {
        Faulted { class: FaultClass::Tx, what: what.to_string() }
    }

    pub fn setup(what: impl fmt::Display) -> Faulted {
        Faulted { class: FaultClass::Setup, what: what.to_string() }
    }
}

/// Runs a call batch (a `CallProgram`'s calls, or a create's `init`) for `signer` against `view`
/// (§6.1 step 4): on the transaction's thread, or the pass's, an engine loaded with the view's
/// declared code in hash order, then `vm::exec::execute`. The receipt, failed or not, is the
/// transaction's; any `NodeFault` is a fault.
pub fn run_calls(run: &Run, view: &ProgramView, signer: Address, calls: &[Call], bindings: &[Binding], limits: TxLimits) -> Result<Receipt, Faulted> {
    run_batch(&run.tx, run.schedule()?, view, signer, calls, bindings, limits)
}

/// [`run_calls`]' work, for a caller that has no transaction to name (`/view`, which runs one
/// batch of a read-only call, [`super::view_api`]): `tx` names the run in a fault's message and for
/// the tests' injected faults. The same thread, engine, loading and `execute` as a transaction's.
pub(super) fn run_batch(tx: &str, schedule: &'static Schedule, view: &ProgramView, signer: Address, calls: &[Call], bindings: &[Binding], limits: TxLimits) -> Result<Receipt, Faulted> {
    let codes = view.codes();
    on_program_thread(tx, || pass::with_engine(schedule, &codes, |engine| load_and_execute(engine, &codes, view, signer, calls, bindings, limits, schedule)))
}

/// Loads `codes` (the declared set's distinct codes, in hash order) into `engine`, then executes
/// the batch on it: what every program transaction's VM work is, whichever thread and engine it
/// runs on. Code `engine` holds already is W1's no-op (`BlockEngine::load`), and execution reaches
/// only the view's declared set, so what an engine held before changes nothing here.
#[allow(clippy::too_many_arguments)]
fn load_and_execute(engine: &mut dyn BlockEngine, codes: &BTreeMap<CodeHash, &[u8]>, view: &ProgramView, signer: Address, calls: &[Call], bindings: &[Binding], limits: TxLimits, schedule: &'static Schedule) -> Result<Receipt, Faulted> {
    // Stored code passed validation and a test translation at upload: if it doesn't load now,
    // this node differs from the one that stored it.
    for (hash, wasm) in codes {
        engine.load(*hash, wasm).map_err(|e| Faulted::tx(NodeFault::Load(e)))?;
    }
    blockula_vm::exec::execute(&*engine, view, signer, calls, bindings, limits, schedule).map_err(Faulted::tx)
}

/// `UploadCode`'s checks on code the chain doesn't hold (§2.2 step 3): W1's `upload::check`,
/// validation then a test translation on a throwaway engine, on the transaction's thread (or the
/// pass's: the throwaway engine is its own either way). A refusal is the upload's failure; a panic
/// is a `Tx` fault, an engine that won't build `Setup`.
pub fn check_upload(run: &Run, wasm: &[u8]) -> Result<Result<ModuleInfo, Refusal>, Faulted> {
    let schedule = run.schedule()?;
    #[cfg(test)]
    let tx_id = run.tx.clone();
    on_program_thread(&run.tx, || {
        #[cfg(test)]
        pass::count(|c| c.upload_checks += 1);
        // Test builds: a panic a test injected for this upload, as the view's hook does for calls.
        #[cfg(test)]
        if super::view::inject::armed(&tx_id) == Some(super::view::inject::Injected::Panic) {
            panic!("a fault injected into an upload (test)");
        }
        upload::check(wasm, schedule).map_err(|f| match f {
            NodeFault::Engine(_) => Faulted::setup(f),
            _ => Faulted::tx(f),
        })
    })
}

/// Runs `work` on a fresh thread with a [`PROGRAM_STACK_BYTES`] stack, and waits for it; or, on a
/// block pass's thread (`pass.rs`: already such a thread), inline. A spawn the OS refuses is
/// retried at once, twice; a third refusal is a `Setup` fault. A panic in `work` is caught on its
/// thread, and one that escapes anyway shows at `join`: either is a `Tx` fault.
fn on_program_thread<T: Send>(tx: &str, work: impl Fn() -> Result<T, Faulted> + Sync) -> Result<T, Faulted> {
    // It holds only references, so it's `Copy`: each attempt takes a copy.
    #[cfg(test)]
    let counting = pass::counting();
    let work = || {
        #[cfg(test)]
        pass::adopt(&counting);
        catch_unwind(AssertUnwindSafe(&work)).unwrap_or_else(|_| Err(Faulted::tx("a panic on the program thread")))
    };
    if pass::in_pass() {
        return work();
    }
    std::thread::scope(|s| {
        let mut refused = None;
        for _ in 0..SPAWN_ATTEMPTS {
            #[cfg(test)]
            if super::view::inject::refuse_spawn(tx) {
                refused = Some(std::io::Error::other("refused by a test"));
                continue;
            }
            match std::thread::Builder::new().name("blockula-program".into()).stack_size(PROGRAM_STACK_BYTES).spawn_scoped(s, work) {
                Ok(thread) => {
                    #[cfg(test)]
                    pass::count(|c| c.tx_threads += 1);
                    return thread.join().unwrap_or_else(|_| Err(Faulted::tx("the program thread panicked past every backstop")));
                }
                Err(e) => refused = Some(e),
            }
        }
        let why = refused.map_or_else(String::new, |e| format!(": {e}"));
        Err(Faulted::setup(format!("the OS refused a program thread {SPAWN_ATTEMPTS} times for tx {}{why}", short(tx))))
    })
}

#[cfg(test)]
mod tests;
