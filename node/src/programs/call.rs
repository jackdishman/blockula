//! `CallProgram` (tag 64) in a block (docs/PROGRAMS-V0.md §2.4, "Execution", and §6.1): runs the
//! calls in order, atomically, with the bindings as the declared set besides the calls' own
//! programs (a program outside it traps, `UnboundProgram`), then commits the diff and holds it to
//! the FANG programs control's rules (exit-only, the value cap). On any failure only the fee and
//! the nonce stick.
//!
//! Each call runs only the version of its program it names (the VM refuses any other,
//! `VersionMismatch`), a method of kind `Call`, and takes value only if the method is payable
//! (`NotCallable`, `NotPayable`): admission refuses all three first (§2.4), and the VM checks them
//! again as it opens each frame.

use super::exec::{self, Failure, Run, Stop};
use super::{commit, receipt};
use super::view::ProgramView;
use crate::chain::{Journal, State};
use crate::crypto;
use crate::types::{self, Action, Tx};
use blockula_vm::engine::Address;
use blockula_vm::exec::{Binding, Call, TxLimits};
use blockula_vm::host::MethodKind;

/// Applies `tx`, a `CallProgram` from `sender`, in the block at `run.height`.
pub fn apply(state: &mut State, j: &mut Journal, run: &Run, tx: &Tx, sender: &str) -> Result<(), Stop> {
    let Action::CallProgram { calls, fuel_limit, storage_limit, bindings } = &tx.action else {
        return Err(Failure::native("not a call").into());
    };
    let before = commit::control(state);
    let signer = address(sender)?;
    let bindings = self::bindings(bindings)?;
    let calls = calls
        .iter()
        .map(|c| Ok(Call { program: address(&c.program)?, version_digest: hash(&c.version_digest)?, method: c.method.clone(), input: c.input.0.clone(), value: c.value, kind: MethodKind::Call }))
        .collect::<Result<Vec<Call>, Failure>>()?;
    let declared: Vec<Address> = calls.iter().map(|c| c.program).chain(bindings.iter().map(|b| b.program)).collect();
    let limits = TxLimits { fuel_limit: *fuel_limit, storage_limit: *storage_limit };
    let receipt = {
        let view = ProgramView::new(state, run, declared, None)?;
        exec::run_calls(run, &view, signer, &calls, &bindings, limits)?
    };
    run.notes.borrow_mut().ran(&receipt);
    if let Some(fail) = receipt.fail {
        return Err(Failure::Vm(fail).into());
    }
    run.notes.borrow_mut().stored(receipt::storage_change(state, &receipt.diff));
    commit::apply_diff(state, j, &receipt.diff)?;
    commit::check_control(state, before, run.height)?;
    Ok(())
}

/// An address's 32 bytes (the codec checked it's one).
fn address(a: &str) -> Result<Address, Failure> {
    crypto::address_bytes(a).ok_or_else(|| Failure::native(format!("{a} isn't an address")))
}

/// A `hex64` field's 32 bytes (the codec checked its form).
pub(super) fn hash(h: &str) -> Result<[u8; 32], Failure> {
    hex::decode(h).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| Failure::native(format!("{h} isn't a 32-byte hash")))
}

/// A program transaction's bindings, as the VM takes them (64 zeros: any version).
pub(super) fn bindings(bindings: &[types::Binding]) -> Result<Vec<Binding>, Failure> {
    bindings.iter().map(|b| Ok(Binding { program: address(&b.program)?, version_digest: hash(&b.version_digest)? })).collect()
}
