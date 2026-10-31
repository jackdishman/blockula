//! One thread and one engine per block pass (docs/PROGRAMS-V0.md §5.2 item 3's first fallback, A3):
//! how the node meets the consensus rule that every transaction pays its own declared code's load
//! without paying for a thread and an engine apiece.
//!
//! **A block pass** is one run of a block's transactions: executing a certified block, replaying an
//! unexecuted ancestor, a verification's apply loop, a proposer's packing, the startup replay of
//! the block log. Its caller wraps the loop in [`block_pass`], which, when the transactions hold a
//! program transaction (tags 62-64), runs the whole loop on **one thread** with the
//! [`PROGRAM_STACK_BYTES`](super::exec::PROGRAM_STACK_BYTES) stack, started in a `thread::scope`
//! and joined when the loop ends. The loop is the node's own code, so what it captures crosses to
//! that thread (`State` is `Send` and `Sync`), and a panic in it is re-raised on the caller's
//! thread at the join, as it would have unwound there. A pass with no program transaction (every v1
//! block) runs on the caller's thread, with no thread and no engine, exactly as before.
//!
//! On that thread each program transaction's VM work runs inline, under the `catch_unwind` it had
//! on a thread of its own (`exec::on_program_thread`), and on **one engine**, built when the first
//! transaction that needs one runs. A transaction loads its declared code in hash order, as it
//! always did; code the engine already holds is W1's documented no-op (`BlockEngine::load`:
//! "loading a hash again is a no-op"), so a code several transactions declare is translated once
//! per pass. What a transaction's load costs in *fuel* is never read off the engine: admission
//! counted it from the transaction's own declaration (§5.4), so no outcome, fuel figure, receipt or
//! state root depends on what an engine held (the equivalence argument is in
//! `node/vm/CALIBRATION-node.md`, "A3"; the differential test in `pass/tests.rs` holds it).
//!
//! **When the engine is dropped.** The pass's engine is dropped
//! - **after any node fault or caught panic** (spec §6.1 step 4's parenthesis): the transaction's
//!   code runs with the engine taken out of its slot, and goes back only if the run returned; and
//!   `programs::apply` drops it too when it records a fault, whichever step raised it
//!   ([`discard_engine`]). The next program transaction gets a fresh one;
//! - **when the code loaded into it would pass [`ENGINE_CODE_BYTES`]**: soroban-wasmi keeps every
//!   translated function until its engine is dropped, so a block of transactions that each declare
//!   other code would otherwise grow one engine without bound, where a thread per transaction held
//!   one transaction's code at a time. The next transaction gets a fresh engine (a rebuild costs
//!   microseconds; translating what it loads costs more);
//! - **when a transaction runs under a schedule that differs** from the one it was built from,
//!   compared by value, not by address (it can't within one pass: a pass is one block, and an
//!   upgrade activates at least `MIN_UPGRADE_LEAD` blocks after the block that schedules it; the
//!   check keeps an engine from ever being shared across schedules whatever a later release does);
//! - **when the pass ends**, on the thread that built it (a `Box<dyn BlockEngine>` isn't `Send`,
//!   which is why the engine lives in a thread-local of the pass thread and not in the `State`).
//!
//! `upload::check` keeps its own throwaway engine (its test translation is of code the chain
//! doesn't hold yet), but runs inline on the pass thread. `/view` and anything else that runs the
//! VM outside a pass keeps a thread and an engine per call, as before.
//!
//! **If the OS refuses the pass thread** three times, the pass runs on the caller's thread, and
//! each program transaction on a thread of its own, exactly as it did before A3: the first one
//! then meets the refusals and the `Setup` fault as it always did.
//!
//! **What crosses to the pass thread** is the loop's captures, so `State` must be `Send` and `Sync`
//! (asserted below). Other than this module's own two (the pass mark and the engine, both the pass
//! thread's), the node has no `thread_local!` in production code. The ones test builds have are
//! `upgrade::test_gate`'s record, read in `State::end_block`, which stays on the caller's thread; the
//! state tree's visit count, which the staking and API tests read around reads of their own; the
//! counters here, which a pass thread shares with its caller; and `replay::diverge`'s switches, read
//! by admission and the transfer handler, which run on the pass thread: a test build copies the
//! caller's setting to the pass thread when it starts it, so a divergence set for a block with a
//! program transaction diverges there too and never drops out silently.
//!
//! **Test builds** count what the mempool's tests must see not happen ([`counts`]): pass threads
//! and transaction threads started, engines built and upload checks run, and
//! [`per_transaction`] runs a closure's passes the old way, one thread and one engine per
//! transaction, for the differential test.

use super::exec::{Faulted, PROGRAM_STACK_BYTES, SPAWN_ATTEMPTS};
use super::is_program_tx;
use crate::types::Tx;
use blockula_vm::engine::{new_block_engine, BlockEngine, CodeHash, EngineKind};
use blockula_vm::meter::Schedule;
use parking_lot::Mutex;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
#[cfg(test)]
use std::sync::Arc;

/// The most bytes of code one pass's engine holds before the next transaction that needs code it
/// doesn't hold gets a new engine: 1 MiB, twice what one transaction may declare
/// (`MAX_TX_CODE_BYTES`, 512 KiB), so a new engine always fits a transaction's whole declared set
/// (the assert below). A block of transactions that each declare a maximal set of other code
/// rebuilds the engine about every second one: microseconds, and the load it re-translates was
/// charged to each of them already.
///
/// **What a full engine holds.** Measured with a counting allocator over code that passes
/// `blockula_vm::validate::validate` (`node/vm/CALIBRATION-node.md`, "The engine's memory bound"),
/// in bytes of heap per byte of code:
/// - 4 to 11 for ordinary code and for the most functions validation allows (6,250 of 40 bytes);
/// - 24 for element segments (a 24-byte entry for each one-byte function index): 23 MiB at the bound;
/// - 16 to 21 for `br_table`s of up to 20,000 targets, repeated, the densest code (two 8-byte
///   instructions for each one-byte target): 16 to 20 MiB at the bound, by how it splits into
///   functions;
/// - under 32 whatever the code. The engine keeps every translated function in one
///   `Vec<Instruction>` for all its modules, at most two instructions per code byte, and a `Vec`'s
///   capacity is under twice what it holds, so code whose last function just crosses a step of that
///   capacity holds twice what it needs: **31.7 MiB at the bound, the most found** (four codes of
///   260 KB, then any other code that fits).
///
/// So a full engine holds under 32 MiB (about 20 MiB of it written, on Linux), where a thread per
/// transaction held one transaction's code, at most 16 MiB (the same shape in 512 KiB). On top of
/// that, while one function translates the engine holds up to 44 bytes per byte of it more (11 MB
/// for 250 KB), freed when it is done. A validator runs at most two passes at once: a pass blocks the
/// runtime worker that runs it and the runtime has two (`CALIBRATION-node.md`, "How many passes at
/// once"; no limiter of ours is behind that), so the code held is under 64 MiB, and about 105 MiB
/// with the transients: a twentieth of a 2 GB machine. A bound doesn't change an outcome, only when
/// an engine is rebuilt.
pub const ENGINE_CODE_BYTES: usize = 1 << 20;
const _: () = assert!(ENGINE_CODE_BYTES >= blockula_vm::config::MAX_TX_CODE_BYTES);

/// The state moves to the pass thread by `&mut` and is read there through shared references.
const _: () = {
    const fn send_and_sync<T: Send + Sync>() {}
    send_and_sync::<crate::chain::State>();
};

/// A pass's engine, with what it needs to be reused or replaced.
struct Held {
    engine: Box<dyn BlockEngine>,
    /// The schedule it was configured from.
    schedule: &'static Schedule,
    /// Bytes of code loaded into it so far (each distinct code once).
    loaded: usize,
}

/// Test builds count the engines a pass keeps alive (the module docs: the memory bound).
#[cfg(test)]
impl Drop for Held {
    fn drop(&mut self) {
        tests_support::alive(-1);
    }
}

thread_local! {
    /// Whether this thread is a pass thread: it is running a [`block_pass`]'s loop.
    static IN_PASS: Cell<bool> = const { Cell::new(false) };
    /// The pass's engine, once a transaction has needed one. Only the pass thread's, which built it.
    static ENGINE: RefCell<Option<Held>> = const { RefCell::new(None) };
}

/// Whether the current thread is running a block pass: a program transaction's VM work then runs
/// here, on the pass's engine, not on a thread of its own.
pub fn in_pass() -> bool {
    IN_PASS.get()
}

/// Marks the thread as running a pass; on drop (the loop returned or unwound) drops the engine on
/// this thread, the one that built it, and clears the mark.
struct Active;

impl Active {
    fn begin() -> Active {
        IN_PASS.set(true);
        Active
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        discard_engine();
        IN_PASS.set(false);
    }
}

/// Runs `pass`, the loop over a block's transactions `txs`, on the pass thread if `txs` hold a
/// program transaction (the module docs), else on the caller's. `pass` captures the state and
/// whatever else the loop works on; it runs once.
///
/// A panic in `pass` unwinds the pass thread and is raised again here, on the caller's thread.
pub fn block_pass<'a, R: Send>(txs: impl Iterator<Item = &'a Tx> + Clone, pass: impl FnOnce() -> R + Send) -> R {
    if in_pass() || !txs.clone().any(is_program_tx) {
        return pass();
    }
    #[cfg(test)]
    if tests_support::PER_TRANSACTION.get() {
        return pass();
    }
    // Held here so that a spawn the OS refuses leaves it to run on this thread.
    let pass = Mutex::new(Some(pass));
    #[cfg(test)]
    let (counting, diverging) = (tests_support::counting(), tests_support::diverging());
    let spawned = std::thread::scope(|s| {
        for _ in 0..SPAWN_ATTEMPTS {
            #[cfg(test)]
            if tests_support::refuses_pass_thread(txs.clone()) {
                continue;
            }
            // It holds only references, so it's `Copy`: each attempt takes a copy.
            let thread = || {
                #[cfg(test)]
                {
                    tests_support::adopt(&counting);
                    crate::replay::diverge::set(diverging);
                }
                on_pass_thread(&pass)
            };
            if let Ok(thread) = std::thread::Builder::new().name("blockula-program-pass".into()).stack_size(PROGRAM_STACK_BYTES).spawn_scoped(s, thread) {
                #[cfg(test)]
                tests_support::count(|c| c.pass_threads += 1);
                return Some(thread.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)));
            }
        }
        None
    });
    spawned.unwrap_or_else(|| {
        tracing::warn!("the OS refused the program thread of a block pass {SPAWN_ATTEMPTS} times: running its program transactions each on a thread of its own");
        let pass = pass.lock().take().expect("a refused pass wasn't started");
        pass()
    })
}

/// The pass thread's body: takes `pass` out of its slot and runs it with the thread marked.
fn on_pass_thread<R, F: FnOnce() -> R>(pass: &Mutex<Option<F>>) -> R {
    let pass = pass.lock().take().expect("a pass runs once");
    let _active = Active::begin();
    pass()
}

/// A new engine for the transactions that run under `schedule`: an `Err` is a `Setup` fault (this
/// node couldn't make what every program transaction needs, §6.2).
pub(super) fn new_engine(schedule: &'static Schedule) -> Result<Box<dyn BlockEngine>, Faulted> {
    // Test builds: an engine a test made fail to build is a `Setup` fault and isn't counted as built.
    #[cfg(test)]
    if tests_support::engine_refused() {
        return Err(Faulted::setup("no engine: refused by a test"));
    }
    #[cfg(test)]
    tests_support::count(|c| c.engines += 1);
    new_block_engine(EngineKind::Soroban, schedule).map_err(|e| Faulted::setup(format!("no engine: {e}")))
}

/// Runs `work` on an engine holding `codes` (the declared set's distinct codes, by hash; `work`
/// loads them): on a pass thread, the pass's engine; anywhere else a new one, dropped after.
///
/// On a pass thread the engine is taken out of its slot while `work` runs and goes back only if
/// `work` returned `Ok`: a fault, and a panic (which unwinds with the engine), leave the slot empty,
/// so the next transaction builds a new engine. A held engine is replaced instead when it was built
/// for another schedule, or when the code `work` is about to load would take it past
/// [`ENGINE_CODE_BYTES`].
pub(super) fn with_engine<T>(schedule: &'static Schedule, codes: &BTreeMap<CodeHash, &[u8]>, work: impl FnOnce(&mut dyn BlockEngine) -> Result<T, Faulted>) -> Result<T, Faulted> {
    if !in_pass() {
        let mut engine = new_engine(schedule)?;
        return work(&mut *engine);
    }
    // The schedule is compared by value: `&SCHEDULE_V0` is a promoted constant, and the language
    // doesn't promise one address for it across codegen units.
    let kept = ENGINE.with(|e| e.borrow_mut().take()).filter(|held| *held.schedule == *schedule);
    // What this transaction adds to the engine: the declared codes it doesn't hold yet.
    let adds = |engine: &dyn BlockEngine| codes.iter().filter(|(hash, _)| engine.info(hash).is_none()).fold(0usize, |sum, (_, wasm)| sum.saturating_add(wasm.len()));
    let mut held = match kept {
        Some(held) if held.loaded.saturating_add(adds(&*held.engine)) <= ENGINE_CODE_BYTES => held,
        // The engine this one replaces goes first: it is up to a bound's worth of translated code,
        // and `work` is about to translate the new one's.
        replaced => {
            drop(replaced);
            let engine = new_engine(schedule)?;
            #[cfg(test)]
            tests_support::alive(1);
            Held { engine, schedule, loaded: 0 }
        }
    };
    held.loaded = held.loaded.saturating_add(adds(&*held.engine));
    #[cfg(test)]
    tests_support::note_alive();
    let out = work(&mut *held.engine);
    if out.is_ok() {
        ENGINE.with(|e| *e.borrow_mut() = Some(held));
    }
    out
}

/// Drops the pass's engine, if this thread has one: after a node fault, wherever it was raised.
pub(super) fn discard_engine() {
    // Taken first, dropped after the borrow ends.
    let engine = ENGINE.with(|e| e.borrow_mut().take());
    drop(engine);
}

#[cfg(test)]
pub use tests_support::{adopt, count, counting, counts, fail_engines, peak_alive, per_transaction, refuse_pass_threads, Counts};

/// What test builds count and switch (the module docs).
#[cfg(test)]
mod tests_support {
    use super::*;

    /// What this thread has started or built.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Counts {
        /// Block passes' threads.
        pub pass_threads: u64,
        /// Threads of one program transaction's own (outside a pass).
        pub tx_threads: u64,
        /// Engines built for a transaction's calls (`upload::check`'s throwaway ones aren't).
        pub engines: u64,
        /// `upload::check`s run.
        pub upload_checks: u64,
    }

    impl Counts {
        pub fn since(self, before: Counts) -> Counts {
            Counts { pass_threads: self.pass_threads - before.pass_threads, tx_threads: self.tx_threads - before.tx_threads, engines: self.engines - before.engines, upload_checks: self.upload_checks - before.upload_checks }
        }
    }

    thread_local! {
        /// What this thread counts into: its own, unless it adopted its caller's (`adopt`).
        static COUNTS: RefCell<Arc<Mutex<Counts>>> = RefCell::new(Arc::default());
        /// How many pass-thread spawns the OS refuses next (this thread's).
        static REFUSE: Cell<u32> = const { Cell::new(0) };
        /// How many engine builds fail next (this thread's: set on the pass thread, which builds them).
        static FAIL_ENGINES: Cell<u32> = const { Cell::new(0) };
        /// The engines this (pass) thread's `with_engine` holds alive now, and the most it held when
        /// a transaction's `work` ran.
        static ALIVE: Cell<i64> = const { Cell::new(0) };
        static PEAK_ALIVE: Cell<i64> = const { Cell::new(0) };
        pub(super) static PER_TRANSACTION: Cell<bool> = const { Cell::new(false) };
    }

    /// What this thread has counted, and what the threads it started counted into the same cell.
    pub fn counts() -> Counts {
        *counting().lock()
    }

    /// The cell this thread counts into: handed to a thread it starts, which `adopt`s it, so what
    /// that thread does is counted for this one.
    pub fn counting() -> Arc<Mutex<Counts>> {
        COUNTS.with(|c| c.borrow().clone())
    }

    pub fn adopt(counting: &Arc<Mutex<Counts>>) {
        COUNTS.with(|c| *c.borrow_mut() = counting.clone());
    }

    pub fn count(f: impl FnOnce(&mut Counts)) {
        f(&mut counting().lock());
    }

    /// The OS refuses the next `n` pass-thread spawns on this thread (three in a row send the pass
    /// back to a thread per transaction).
    pub fn refuse_pass_threads(n: u32) {
        REFUSE.set(n);
    }

    /// The next `n` engines this thread builds fail (a `Setup` fault, `new_engine`): call it on the
    /// thread that builds them, a pass thread from inside the pass.
    pub fn fail_engines(n: u32) {
        FAIL_ENGINES.set(n);
    }

    pub(super) fn alive(by: i64) {
        ALIVE.set(ALIVE.get() + by);
    }

    /// `work` is about to run: the engines alive beside it count.
    pub(super) fn note_alive() {
        PEAK_ALIVE.set(PEAK_ALIVE.get().max(ALIVE.get()));
    }

    /// The most engines this thread held alive while a transaction's work ran.
    pub fn peak_alive() -> i64 {
        PEAK_ALIVE.get()
    }

    /// Whether this engine build fails: the countdown.
    pub(super) fn engine_refused() -> bool {
        let left = FAIL_ENGINES.get();
        FAIL_ENGINES.set(left.saturating_sub(1));
        left > 0
    }

    /// The divergence from v1 (`replay::diverge`) this thread has switched on, if any. The pass
    /// thread gets the same, since admission and the transfer handler read it where they run.
    pub(super) fn diverging() -> Option<crate::replay::diverge::Divergence> {
        use crate::replay::diverge::{self, Divergence::{Admission, EndOfEpoch, Handler}};
        [Admission, Handler, EndOfEpoch].into_iter().find(|d| {
            // Exhaustive on purpose: a divergence added later fails to compile here until it is listed.
            let (Admission | Handler | EndOfEpoch) = d;
            diverge::on(*d)
        })
    }

    /// Whether a test has the OS refuse this attempt at a pass thread: its countdown, or a
    /// transaction of the pass armed to have its own thread refused (`Injected::RefuseSpawns`):
    /// the pass thread meets the same OS, and the transaction then meets its refusals itself.
    pub(super) fn refuses_pass_thread<'a>(mut txs: impl Iterator<Item = &'a Tx>) -> bool {
        if REFUSE.get() > 0 {
            REFUSE.set(REFUSE.get() - 1);
            return true;
        }
        txs.any(|tx| is_program_tx(tx) && super::super::view::inject::refusing(&tx.id()))
    }

    /// Runs `f` with this thread's block passes done the way they were before A3: no pass thread, a
    /// thread and an engine per program transaction. The differential test's reference.
    pub fn per_transaction<R>(f: impl FnOnce() -> R) -> R {
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) {
                PER_TRANSACTION.set(self.0);
            }
        }
        let _restore = Restore(PER_TRANSACTION.replace(true));
        f()
    }
}

#[cfg(test)]
mod tests;
