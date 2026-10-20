//! Execution of one transaction's calls (docs/PROGRAMS.md 4.4, 4.5, 8.1, 8.2): frames over the
//! block's engine, one fuel meter, the overlay, and the commit checks. Never mutates the base
//! state: a successful transaction returns a [`Diff`] for the caller to apply.
//!
//! * **Frames.** Every call, top-level or nested, is a frame: an overlay layer, a store and an
//!   instance (`engine/mod.rs`). Value moves from payer to program in the frame's layer before
//!   the program runs, so a failed frame takes its value back with everything else.
//! * **One meter.** The batch shares `fuel_limit`. Each top-level call gets what's left; a nested
//!   call gets what its caller has left, or the smaller share the caller passes, and what it
//!   used is spent either way. Instantiation (`Schedule::instantiate_fuel`) is charged to the
//!   caller (the transaction, for a top-level call) before the callee's store exists.
//! * **Out of fuel.** Both engines leave the remainder as it was before the charge that failed,
//!   so a frame that runs out counts its whole share as used, and a transaction that fails out of
//!   fuel reports `fuel_used = fuel_limit` (4.3).
//! * **The declared set.** A transaction reaches its calls' programs and `declared` (its
//!   bindings), nothing else (`UnboundProgram`). No program twice on the stack (`Reentrancy`); at
//!   most `MAX_CALL_DEPTH` on it (`CallDepth`).
//! * **Panics** are caught (`catch_unwind`) and become `RuntimeFault`. The caller must then drop
//!   the block's engine and build a fresh one for the rest of the block (4.3).

use crate::abi::{self, MAX_SUPPLY};
use crate::config::{MAX_CALL_DEPTH, MAX_INPUT};
use crate::engine::{Address, BlockEngine, CodeHash, CodeInfo, Misuse, Trap};
use crate::host::View;
use crate::meter::Schedule;
use crate::overlay::Overlay;
use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

pub use crate::overlay::{Diff, Entry, Event, StorageChange};

/// One call of a batch (8.1's `ProgramCall`, with the address already decoded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub program: Address,
    /// Runs export `bk_<method>`.
    pub method: String,
    pub input: Vec<u8>,
    /// Drops moved from the signer to `program` before the method runs.
    pub value: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxLimits {
    /// Shared by every call of the batch and every frame they open.
    pub fuel_limit: u64,
    /// Most deposit the signer's writes may bond, in drops (6.3).
    pub storage_limit: u64,
}

/// Why a transaction failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailCode {
    /// `program` trapped with `trap`: the innermost frame where the failure began (a `call`er
    /// that a failing callee traps doesn't replace it).
    Trapped { program: Address, trap: Trap },
    /// The overlay failed the commit checks (4.4): a host bug, never a program's doing.
    Commit(&'static str),
    /// A panic, caught by the backstop. The block's engine must be rebuilt.
    RuntimeFault,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub ok: bool,
    pub fail: Option<FailCode>,
    pub fuel_used: u64,
    /// Each call's return data (from its top frame), in order.
    pub returns: Vec<Vec<u8>>,
    pub events: Vec<Event>,
    /// Empty unless `ok`: nothing of a failed transaction applies.
    pub diff: Diff,
}

impl Receipt {
    fn failed(fail: FailCode, fuel_used: u64) -> Receipt {
        Receipt { ok: false, fail: Some(fail), fuel_used, returns: Vec::new(), events: Vec::new(), diff: Diff::default() }
    }
}

/// Runs `calls` in order, atomically, against `base` on the block's `engine`, which must already
/// hold every reachable program's code. `declared` is every other program the transaction may
/// reach (its bindings); a call's own program is always reachable.
pub fn execute(
    engine: &dyn BlockEngine,
    base: &dyn View,
    signer: Address,
    calls: &[Call],
    declared: &[Address],
    limits: TxLimits,
    schedule: &Schedule,
) -> Receipt {
    catch_unwind(AssertUnwindSafe(|| run(engine, base, signer, calls, declared, limits, schedule)))
        .unwrap_or_else(|_| Receipt::failed(FailCode::RuntimeFault, limits.fuel_limit))
}

fn run(
    engine: &dyn BlockEngine,
    base: &dyn View,
    signer: Address,
    calls: &[Call],
    declared: &[Address],
    limits: TxLimits,
    schedule: &Schedule,
) -> Receipt {
    let mut reachable: BTreeSet<Address> = declared.iter().copied().collect();
    reachable.extend(calls.iter().map(|c| c.program));
    let mut run = Run {
        engine,
        base,
        schedule,
        signer,
        reachable,
        storage_limit: limits.storage_limit,
        overlay: Overlay::new(signer),
        frames: Vec::new(),
        ran: BTreeSet::new(),
        origin: None,
    };
    let mut fuel = limits.fuel_limit;
    let mut returns = Vec::with_capacity(calls.len());
    let mut sent: u128 = 0;
    for call in calls {
        match run.top_call(call, &mut fuel) {
            Ok(ret) => returns.push(ret),
            Err(trap) => {
                let fuel_used = if trap == Trap::OutOfFuel { limits.fuel_limit } else { limits.fuel_limit - fuel };
                let program = run.origin.unwrap_or(call.program);
                return Receipt::failed(FailCode::Trapped { program, trap }, fuel_used);
            }
        }
        sent += u128::from(call.value);
    }
    let fuel_used = limits.fuel_limit - fuel;
    let allowance = sent + run.overlay.signer_bonded();
    let Run { overlay, ran, .. } = run;
    let (diff, events) = overlay.finish(base);
    if let Err(why) = diff.check(&signer, &ran, allowance) {
        return Receipt::failed(FailCode::Commit(why), fuel_used);
    }
    Receipt { ok: true, fail: None, fuel_used, returns, events, diff }
}

/// A program on the call stack.
pub(crate) struct Frame {
    pub program: Address,
    /// The signer for a top-level call, else the calling program.
    pub caller: Address,
    pub value: u64,
    pub input: Vec<u8>,
    /// What `return_data_v1` set.
    pub ret: Vec<u8>,
    /// The last callee's return data (or its abort message, after `try_call` returned
    /// `ERR_ABORT`): `abi::HOST_FNS`, "After a call".
    pub last_return: Vec<u8>,
    /// The last callee's abort code, after `try_call` returned `ERR_ABORT`; else 0.
    pub last_abort_code: i32,
}

/// One transaction's execution state: what the host functions (`host.rs`) work on.
pub(crate) struct Run<'a> {
    pub engine: &'a dyn BlockEngine,
    pub base: &'a dyn View,
    pub schedule: &'a Schedule,
    pub signer: Address,
    pub reachable: BTreeSet<Address>,
    pub storage_limit: u64,
    pub overlay: Overlay,
    pub frames: Vec<Frame>,
    /// Programs that ran (who-was-debited check).
    pub ran: BTreeSet<Address>,
    /// The program where a failure in flight began; cleared when `try_call` absorbs it.
    pub origin: Option<Address>,
}

impl Run<'_> {
    pub fn frame(&self) -> &Frame {
        self.frames.last().expect("a host call runs in a frame")
    }

    pub fn frame_mut(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("a host call runs in a frame")
    }

    fn top_call(&mut self, call: &Call, fuel: &mut u64) -> Result<Vec<u8>, Trap> {
        // Admission's `check_format` refuses these; execution checks again (8.1).
        if !abi::is_ident(&call.method) {
            return Err(Trap::Misuse(Misuse::Name));
        }
        if call.input.len() > MAX_INPUT {
            return Err(Trap::Misuse(Misuse::Length));
        }
        if call.value > MAX_SUPPLY {
            return Err(Trap::Misuse(Misuse::Amount));
        }
        let (code, info) = self.resolve(&call.program, &[0; 32])?;
        *fuel = fuel.checked_sub(self.schedule.instantiate_fuel(info.pages, info.data_bytes)).ok_or(Trap::OutOfFuel)?;
        let (result, used) = self.enter(call.program, code, self.signer, call.value, &call.method, call.input.clone(), *fuel);
        *fuel -= used;
        result
    }

    /// Whether `program` may be called now, and its code: in the declared set, not on the stack,
    /// within the depth limit, a program, at the version `expect` names (zeros: any).
    pub fn resolve(&self, program: &Address, expect: &[u8; 32]) -> Result<(CodeHash, CodeInfo), Trap> {
        if !self.reachable.contains(program) {
            return Err(Trap::UnboundProgram);
        }
        if self.frames.iter().any(|f| f.program == *program) {
            return Err(Trap::Reentrancy);
        }
        if self.frames.len() >= MAX_CALL_DEPTH {
            return Err(Trap::CallDepth);
        }
        let code = self.base.code_of(program).ok_or(Trap::NotAProgram)?;
        // The spike's version digest is the code hash (7.3's digest also covers the interface).
        if *expect != [0; 32] && *expect != code {
            return Err(Trap::VersionMismatch);
        }
        let info = self.engine.info(&code).ok_or_else(|| Trap::Fault("code not loaded".into()))?;
        Ok((code, info))
    }

    /// Runs `bk_<method>` of `program` in a new frame with `fuel`: moves `value` from `payer`,
    /// invokes, then merges or drops the frame's layer. Returns the frame's return data and the
    /// fuel it used (all of it if it ran out).
    #[allow(clippy::too_many_arguments)]
    pub fn enter(
        &mut self,
        program: Address,
        code: CodeHash,
        payer: Address,
        value: u64,
        method: &str,
        input: Vec<u8>,
        fuel: u64,
    ) -> (Result<Vec<u8>, Trap>, u64) {
        self.overlay.push();
        if let Err(trap) = self.overlay.move_value(self.base, payer, program, value) {
            // The payer is short: the caller's failure, not the callee's (no origin set here).
            self.overlay.revert();
            return (Err(trap), 0);
        }
        self.frames.push(Frame { program, caller: payer, value, input, ret: Vec::new(), last_return: Vec::new(), last_abort_code: 0 });
        self.ran.insert(program);
        let engine = self.engine;
        let invoked = engine.invoke(&code, &format!("{}{method}", abi::EXPORT_PREFIX), fuel, self);
        let frame = self.frames.pop().expect("the frame pushed above");
        let used = match invoked.result {
            Err(Trap::OutOfFuel) => fuel,
            _ => fuel.saturating_sub(invoked.fuel_left),
        };
        match invoked.result {
            Ok(()) => {
                self.overlay.commit();
                (Ok(frame.ret), used)
            }
            Err(trap) => {
                self.overlay.revert();
                self.origin.get_or_insert(program);
                (Err(trap), used)
            }
        }
    }
}
