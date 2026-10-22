//! Execution of one transaction's calls (docs/PROGRAMS.md 4.4, 4.5, 8.1, 8.2): frames over the
//! transaction's engine, one fuel meter, the overlay, and the commit checks. Never mutates the
//! base state: a successful transaction returns a [`Diff`] for the caller to apply.
//!
//! * **Frames.** Every call, top-level or nested, is a frame: an overlay layer, a store and an
//!   instance (`engine/mod.rs`). Value moves from payer to program in the frame's layer before
//!   the program runs, so a failed frame takes its value back with everything else.
//! * **One meter.** The batch shares `fuel_limit`. Each top-level call gets what's left; a nested
//!   call gets what its caller has left, or the smaller share the caller passes, and what it
//!   used is spent either way. Instantiation (`Schedule::instantiate_fuel`) is charged to the
//!   caller (the transaction, for a top-level call) before the callee's store exists.
//! * **Out of fuel.** The engine leaves the remainder as it was before the charge that failed,
//!   so a frame that runs out counts its whole share as used, and a transaction that fails out of
//!   fuel reports `fuel_used = fuel_limit` (4.3), nested or not: a callee that runs out of a
//!   smaller share its caller passed to `call_v1` fails the transaction with the whole limit
//!   used. (A `try_call_v1` on the way up absorbs it: its caller runs on, the failed frame's
//!   share counted as used.)
//! * **The declared set.** A transaction reaches its calls' programs and its bindings, nothing
//!   else (`UnboundProgram`). No program twice on the stack (`Reentrancy`); at most
//!   `MAX_CALL_DEPTH` on it (`CallDepth`).
//! * **Versions.** A call runs only the version its `version_digest` names; a binding's digest
//!   and `call_v1`'s `expect` (zeros for any) are checked each time a frame of that program opens
//!   (8.1, 8.2). Digests come from the [`View`]: execution only compares them.
//! * **Method kinds** (11.1), from the [`View`]: a top-level call runs only a method of the kind
//!   the action runs ([`Call::kind`]); `call_v1` never reaches an `Init` method; a `View` frame is
//!   read-only, and so is everything it calls; value goes only to payable methods.
//! * **Two kinds of failure.** A program's doing (a trap, an abort, a limit, the commit checks)
//!   fails the transaction with a numbered [`FailCode`] in its [`Receipt`]. The node's trouble (an
//!   engine error, code that isn't loaded, a broken host invariant, a panic) is a [`NodeFault`]:
//!   `execute` returns it instead of a receipt, `try_call` never absorbs it, and the node must not
//!   commit the block. Panics are caught (`catch_unwind`); after any node fault the caller drops
//!   the engine.

use crate::abi::{self, MAX_SUPPLY};
use crate::config::{MAX_BINDINGS, MAX_CALL_DEPTH, MAX_CALLS, MAX_INPUT};
use crate::engine::{Address, BlockEngine, CodeHash, CodeInfo, Digest, NodeFault, Trap, TrapKind};
use crate::host::{MethodKind, View};
use crate::meter::Schedule;
use crate::overlay::Overlay;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};

pub use crate::host::MethodInfo;
pub use crate::overlay::{CommitError, Diff, Entry, Event, ProgramStorage, StorageChange};

/// A binding's or `call_v1`'s digest that accepts whatever version is in force (8.1).
pub const ANY_VERSION: Digest = [0; 32];

/// One call of a batch (8.1's `ProgramCall`, with the address and digest already decoded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub program: Address,
    /// The version the signer previewed: the call runs only if it's `program`'s version in
    /// force (`View::version_digest`). No wildcard: [`ANY_VERSION`] matches nothing here.
    pub version_digest: Digest,
    /// Runs export `bk_<method>`.
    pub method: String,
    pub input: Vec<u8>,
    /// Drops moved from the signer to `program` before the method runs.
    pub value: u64,
    /// The kind of method the action runs: `Call` for `CallProgram`, `Init` for `CreateProgram`'s
    /// initializer, `View` for a read-only call (8.5). A method of another kind is `NotCallable`.
    pub kind: MethodKind,
}

/// A program the transaction may reach besides its calls' (8.1's `Binding`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub program: Address,
    /// Checked each time a frame of `program` opens; [`ANY_VERSION`] accepts any.
    pub version_digest: Digest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxLimits {
    /// Shared by every call of the batch and every frame they open.
    pub fuel_limit: u64,
    /// Most deposit the signer's writes may bond, in drops (6.3).
    pub storage_limit: u64,
}

/// Why a transaction failed (6.5): numbers and the program's own bytes, never the engine's or the
/// host's text. Each variant's [`number`](FailCode::number) is as stable as `TrapKind`'s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailCode {
    /// `program` trapped with `kind`: the innermost frame where the failure began (a `call`er
    /// that a failing callee traps doesn't replace it; a `call_v1` refused before its callee's
    /// frame exists is its caller's). For a top-level call refused before its frame exists (any
    /// of the checks on it, or the signer short of its value), the program it called.
    Trapped { program: Address, kind: TrapKind },
    /// `program` called `abort_v1(code, message)`; the message is sanitized and at most
    /// `MAX_ABORT_MESSAGE` bytes (`host::sanitize`).
    Aborted { program: Address, code: u32, message: String },
    /// Every call returned, but the diff failed the commit checks (4.4): a host bug, never a
    /// program's doing, yet the same on every node, so the transaction fails rather than halts.
    Commit(CommitError),
}

impl FailCode {
    /// The variant's number in receipts.
    pub fn number(&self) -> u8 {
        match self {
            FailCode::Trapped { .. } => 1,
            FailCode::Aborted { .. } => 2,
            FailCode::Commit(_) => 3,
        }
    }
}

/// What a transaction did, as consensus records it: outcome, fuel, return data, events and the
/// diff to apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    /// `None` if every call returned and the diff passed the commit checks.
    pub fail: Option<FailCode>,
    /// The fuel the calls used (load fuel is the node's, 4.5). After any `OutOfFuel` that fails
    /// the transaction, nested or not, exactly `fuel_limit`, even when only a callee's smaller
    /// share ran out.
    pub fuel_used: u64,
    /// Each call's return data (from its top frame), in order. Empty unless ok.
    pub returns: Vec<Vec<u8>>,
    /// Empty unless ok: a failed transaction emits nothing.
    pub events: Vec<Event>,
    /// Empty unless ok: nothing of a failed transaction applies.
    pub diff: Diff,
}

impl Receipt {
    /// Whether the transaction succeeded.
    pub fn ok(&self) -> bool {
        self.fail.is_none()
    }

    fn failed(fail: FailCode, fuel_used: u64) -> Receipt {
        Receipt { fail: Some(fail), fuel_used, returns: Vec::new(), events: Vec::new(), diff: Diff::default() }
    }
}

/// Runs `calls` in order, atomically, against `view` on `engine`, which must already hold every
/// reachable program's code. `bindings` are the other programs the transaction may reach; a call's
/// own program is always reachable. The batch bounds (1–8 calls, at most 16 bindings, none a
/// call's program, Σ inputs ≤ 32 KiB, `MAX_TX_CODE_BYTES`) are the tx codec's and admission's.
/// Execution checks each call's own bounds again (a failed receipt), and the batch's counts: no
/// valid transaction has a batch without calls, more than `MAX_CALLS` of them or more than
/// `MAX_BINDINGS` bindings, so one is a node fault, never a receipt (an empty batch would
/// otherwise succeed having done nothing).
///
/// Returns the transaction's receipt, failed or not, or a [`NodeFault`] if the node can't execute
/// it: then nothing of the transaction (or the block) may be committed, and the engine must be
/// dropped. Must run on a large-stack thread (the node gives each program transaction one of
/// 64 MiB, which also makes and drops its engine): host calls nest native frames up to
/// `MAX_CALL_DEPTH` deep.
pub fn execute(
    engine: &dyn BlockEngine,
    view: &dyn View,
    signer: Address,
    calls: &[Call],
    bindings: &[Binding],
    limits: TxLimits,
    schedule: &Schedule,
) -> Result<Receipt, NodeFault> {
    catch_unwind(AssertUnwindSafe(|| run(engine, view, signer, calls, bindings, limits, schedule))).unwrap_or(Err(NodeFault::Panic))
}

fn run(
    engine: &dyn BlockEngine,
    view: &dyn View,
    signer: Address,
    calls: &[Call],
    bindings: &[Binding],
    limits: TxLimits,
    schedule: &Schedule,
) -> Result<Receipt, NodeFault> {
    if calls.is_empty() {
        return Err(NodeFault::Host("a batch without calls"));
    }
    if calls.len() > MAX_CALLS {
        return Err(NodeFault::Host("a batch of more than MAX_CALLS calls"));
    }
    if bindings.len() > MAX_BINDINGS {
        return Err(NodeFault::Host("more than MAX_BINDINGS bindings"));
    }
    // A call's program is reached under its call's digest, checked when that call runs; reached
    // from another program first, it's under no digest of its own (the call's check fails the
    // whole transaction later if it doesn't match).
    let mut reachable: BTreeMap<Address, Digest> = bindings.iter().map(|b| (b.program, b.version_digest)).collect();
    for call in calls {
        reachable.entry(call.program).or_insert(ANY_VERSION);
    }
    let mut run = Run {
        engine,
        view,
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
                let program = run.origin.unwrap_or(call.program);
                let fail = match trap {
                    Trap::Fault(fault) => return Err(fault),
                    Trap::Kind(kind) => FailCode::Trapped { program, kind },
                    Trap::Abort { code, message } => FailCode::Aborted { program, code, message },
                };
                let out_of_fuel = matches!(fail, FailCode::Trapped { kind: TrapKind::OutOfFuel, .. });
                let fuel_used = if out_of_fuel { limits.fuel_limit } else { spent(limits.fuel_limit, fuel)? };
                return Ok(Receipt::failed(fail, fuel_used));
            }
        }
        sent += u128::from(call.value);
    }
    let fuel_used = spent(limits.fuel_limit, fuel)?;
    let allowance = sent + run.overlay.signer_bonded();
    let Run { overlay, ran, .. } = run;
    let (diff, events) = overlay.finish(view)?;
    if let Err(why) = diff.check(&signer, &ran, allowance) {
        return Ok(Receipt::failed(FailCode::Commit(why), fuel_used));
    }
    Ok(Receipt { fail: None, fuel_used, returns, events, diff })
}

/// Fuel used out of `limit` with `left` remaining: the meter only ever goes down.
fn spent(limit: u64, left: u64) -> Result<u64, NodeFault> {
    limit.checked_sub(left).ok_or(NodeFault::Host("more fuel left than the limit"))
}

/// A program on the call stack.
pub(crate) struct Frame {
    pub program: Address,
    /// The signer for a top-level call, else the calling program.
    pub caller: Address,
    pub value: u64,
    pub input: Vec<u8>,
    /// Running a `View` method (4.4): it may not write, remove, transfer, emit, send value or call
    /// a non-`View` method. So every frame it opens is read-only too.
    pub read_only: bool,
    /// What `return_data_v1` set.
    pub ret: Vec<u8>,
    /// The last callee's return data (or its abort message, after `try_call` returned
    /// `ERR_ABORT`): `abi::HOST_FNS`, "After a call".
    pub last_return: Vec<u8>,
    /// The last callee's abort code, after `try_call` returned `ERR_ABORT`; else 0.
    pub last_abort_code: u32,
}

/// Who opens a frame ([`Run::admit`]).
#[derive(Clone, Copy)]
pub(crate) enum Opener {
    /// The transaction, for a top-level call: only a method of `kind`, and only at exactly
    /// `version` (no wildcard).
    Top { kind: MethodKind, version: Digest },
    /// The running frame's `call_v1` or `try_call_v1`: at any version `expect` accepts (zeros:
    /// any), never an `Init` method, and from a read-only frame only a `View` method.
    Nested { expect: Digest },
}

/// A frame [`Run::admit`] lets open: its code, what instantiating it costs to know, and whether
/// it runs read-only.
pub(crate) struct Admitted {
    pub code: CodeHash,
    pub info: CodeInfo,
    pub read_only: bool,
}

/// One transaction's execution state: what the host functions (`host.rs`) work on.
pub(crate) struct Run<'a> {
    pub engine: &'a dyn BlockEngine,
    pub view: &'a dyn View,
    pub schedule: &'a Schedule,
    pub signer: Address,
    /// The declared program set, each with the digest it's bound at ([`ANY_VERSION`]: any).
    pub reachable: BTreeMap<Address, Digest>,
    pub storage_limit: u64,
    pub overlay: Overlay,
    pub frames: Vec<Frame>,
    /// Programs that ran (who-was-debited check).
    pub ran: BTreeSet<Address>,
    /// The program where a failure in flight began; cleared when `try_call` absorbs it.
    pub origin: Option<Address>,
}

impl Run<'_> {
    /// The running frame: every host call runs in one.
    pub fn frame(&self) -> Result<&Frame, NodeFault> {
        self.frames.last().ok_or(NodeFault::Host("a host call outside a frame"))
    }

    pub fn frame_mut(&mut self) -> Result<&mut Frame, NodeFault> {
        self.frames.last_mut().ok_or(NodeFault::Host("a host call outside a frame"))
    }

    fn top_call(&mut self, call: &Call, fuel: &mut u64) -> Result<Vec<u8>, Trap> {
        // Admission's `check_format` refuses these; execution checks again (8.1).
        if !abi::is_ident(&call.method) {
            return Err(TrapKind::Name.into());
        }
        if call.input.len() > MAX_INPUT {
            return Err(TrapKind::Length.into());
        }
        if call.value > MAX_SUPPLY {
            return Err(TrapKind::Amount.into());
        }
        // Admission checks the version, the method's kind and payability too (8.1); a multisig's
        // stored or nested call meets only these.
        let opener = Opener::Top { kind: call.kind, version: call.version_digest };
        let frame = self.admit(opener, &call.program, &call.method, call.value)?;
        *fuel = fuel.checked_sub(self.schedule.instantiate_fuel(&frame.info)).ok_or(TrapKind::OutOfFuel)?;
        let input = call.input.clone();
        let (result, used) = self.enter(call.program, frame.code, self.signer, call.value, &call.method, input, *fuel, frame.read_only)?;
        *fuel = fuel.checked_sub(used).ok_or(NodeFault::Host("a frame used more fuel than it had"))?;
        result
    }

    /// Whether a frame of `program`'s `method`, sent `value`, may open now (8.1, 8.2): every frame
    /// opens through here. In order: `program` is reachable at the version the opener names
    /// ([`Run::resolve`]); its interface has `method`; the method is of a kind the opener may run
    /// (a top-level call: the action's kind; `call_v1`: never `Init`, and only `View` from a
    /// read-only frame); value goes only to a payable method, and never from or to a read-only
    /// frame.
    pub fn admit(&self, opener: Opener, program: &Address, method: &str, value: u64) -> Result<Admitted, Trap> {
        let expect = match opener {
            Opener::Top { .. } => ANY_VERSION,
            Opener::Nested { expect } => expect,
        };
        let (code, info, version) = self.resolve(program, &expect)?;
        if let Opener::Top { version: signed, .. } = opener
            && signed != version
        {
            return Err(TrapKind::VersionMismatch.into());
        }
        let m = self.view.method(program, method).ok_or(TrapKind::NoMethod)?;
        // Whether the opener is read-only: a transaction never is.
        let from_read_only = match opener {
            Opener::Top { kind, .. } => {
                if m.kind != kind {
                    return Err(TrapKind::NotCallable.into());
                }
                false
            }
            Opener::Nested { .. } => {
                let read_only = self.frame()?.read_only;
                match m.kind {
                    MethodKind::Init => return Err(TrapKind::NotCallable.into()),
                    MethodKind::Call if read_only => return Err(TrapKind::ReadOnly.into()),
                    MethodKind::Call | MethodKind::View => {}
                }
                read_only
            }
        };
        let read_only = m.kind == MethodKind::View;
        if value > 0 {
            if !m.payable {
                return Err(TrapKind::NotPayable.into());
            }
            if from_read_only || read_only {
                return Err(TrapKind::ReadOnly.into());
            }
        }
        Ok(Admitted { code, info, read_only })
    }

    /// Whether `program` may be reached now, and its code and version: in the declared set, not
    /// on the stack, within the depth limit, an active program, at the version its binding and
    /// `expect` name (zeros: any).
    pub fn resolve(&self, program: &Address, expect: &Digest) -> Result<(CodeHash, CodeInfo, Digest), Trap> {
        let bound = *self.reachable.get(program).ok_or(TrapKind::UnboundProgram)?;
        if self.frames.iter().any(|f| f.program == *program) {
            return Err(TrapKind::Reentrancy.into());
        }
        if self.frames.len() >= MAX_CALL_DEPTH {
            return Err(TrapKind::CallDepth.into());
        }
        let code = self.view.code_of(program).ok_or(TrapKind::NotAProgram)?;
        let version = self.view.version_digest(program).ok_or(NodeFault::Host("a program without a version digest"))?;
        if [bound, *expect].iter().any(|d| *d != ANY_VERSION && *d != version) {
            return Err(TrapKind::VersionMismatch.into());
        }
        let info = self.engine.info(&code).ok_or(NodeFault::NotLoaded)?;
        Ok((code, info, version))
    }

    /// Runs `bk_<method>` of `program` in a new frame with `fuel`: moves `value` from `payer`,
    /// invokes, then merges or drops the frame's layer. Returns the frame's outcome (its return
    /// data, or why it failed) and the fuel it used (all of it if it ran out), or the node fault
    /// that stopped it.
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
        read_only: bool,
    ) -> Result<(Result<Vec<u8>, Trap>, u64), NodeFault> {
        self.overlay.push();
        if let Err(trap) = self.overlay.move_value(self.view, payer, program, value) {
            // The payer is short: the caller's failure, not the callee's (no origin set here).
            self.overlay.revert()?;
            return Ok((Err(trap), 0));
        }
        let frame = Frame { program, caller: payer, value, input, read_only, ret: Vec::new(), last_return: Vec::new(), last_abort_code: 0 };
        self.frames.push(frame);
        self.ran.insert(program);
        let engine = self.engine;
        let invoked = engine.invoke(&code, &format!("{}{method}", abi::EXPORT_PREFIX), fuel, self);
        let frame = self.frames.pop().ok_or(NodeFault::Host("a frame gone before it returned"))?;
        if frame.program != program {
            return Err(NodeFault::Host("frames out of order"));
        }
        // The fuel left after `OutOfFuel` is from before the charge that failed: the frame's whole
        // share counts as used.
        let used = |left: u64| fuel.checked_sub(left).ok_or(NodeFault::Fuel);
        match invoked.result {
            Ok(()) => {
                self.overlay.commit()?;
                Ok((Ok(frame.ret), used(invoked.fuel_left)?))
            }
            Err(Trap::Fault(fault)) => Err(fault),
            Err(trap) => {
                let used = if trap == Trap::Kind(TrapKind::OutOfFuel) { fuel } else { used(invoked.fuel_left)? };
                self.overlay.revert()?;
                self.origin.get_or_insert(program);
                Ok((Err(trap), used))
            }
        }
    }
}
