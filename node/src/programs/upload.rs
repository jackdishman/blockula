//! `UploadCode` (tag 62) in a block (docs/PROGRAMS-V0.md §2.2, "Execution"): stores a program's
//! code and its interface, each once, under its hash, bonding a deposit for each that isn't stored
//! yet. Code is usable from the block after its upload (A5).
//!
//! 1. The hashes (§2.1).
//! 2. Code that's stored already isn't validated again: its methods are its `CodeRecord`'s.
//! 3. New code goes through W1's `upload::check` on the transaction's thread (validation, then a
//!    test translation): a refusal fails the upload (`invalid_code` with the rule's number, or
//!    `translation_refused`); a panic or an engine that won't build is a node fault.
//! 4. The pair must match, whichever half is new: the interface's methods are exactly the code's
//!    `bk_` exports (`interface_mismatch`).
//! 5. Deposits, code first, then interface, for each blob not stored yet: its size × the price at
//!    program state's bytes once it lands (§4.6). No price past the ceiling (`state_full`); the
//!    sum may not pass `storage_limit`.
//! 6. The sum is debited from the sender and held in escrow, the sizes join `state_bytes`, and each
//!    new blob is written with its `BlobRef`. A blob that was stored already only moves its
//!    `last_used_at` to this block.

use super::digest;
use super::exec::{self, Failure, Run, Stop};
use super::receipt::{OutsideVm, StorageSummary, UploadResult};
use super::state::{blob_key, meta_mut, BlobRef, CodeRecord, InterfaceRecord, Versioned, BLOB_CODE, BLOB_INTERFACE};
use super::view::storage_price;
use crate::chain::{Journal, State};
use crate::types::{Action, Tx};
use blockula_vm::interface;
use blockula_vm::upload::Refusal;

/// Applies `tx`, an `UploadCode` from `sender`, in the block at `run.height`.
pub fn apply(state: &mut State, j: &mut Journal, run: &Run, tx: &Tx, sender: &str) -> Result<(), Stop> {
    let Action::UploadCode { wasm, interface: iface, storage_limit } = &tx.action else {
        return Err(Failure::native("not an upload").into());
    };
    let (wasm, iface) = (&wasm.0, &iface.0);
    let height = run.height;
    // 1.
    let (code_hash, iface_hash) = (digest::wasm_hash(wasm), digest::interface_hash(iface));
    let (new_code, new_iface) = (!state.programs.code.contains_key(&code_hash), !state.programs.interfaces.contains_key(&iface_hash));
    // 2–3. The code's methods.
    let exports = match state.programs.code.get(&code_hash) {
        Some(stored) => stored.exports.clone(),
        None => match exec::check_upload(run, wasm)? {
            Ok(info) => info.exports,
            Err(Refusal::Invalid(e)) => return Err(Failure::InvalidCode(e).into()),
            Err(Refusal::Translate(_)) => return Err(Failure::TranslationRefused.into()),
        },
    };
    // 4. `check_format` decoded the interface already, so only the match can fail here.
    let decoded = interface::decode(iface).map_err(|e| Failure::native(format!("the interface doesn't decode: {e}")))?;
    interface::check_exports(&decoded, &exports).map_err(Failure::InterfaceMismatch)?;
    // 5. Deposits, each priced at program state's bytes once that blob lands.
    let mut used = state.programs.meta.as_ref().map_or(0, |m| m.state_bytes);
    let mut deposit = |new: bool, size: usize| -> Result<u64, Failure> {
        if !new {
            return Ok(0);
        }
        let size = size as u64;
        used = used.checked_add(size).ok_or(Failure::StateFull)?;
        let price = storage_price(state, used).ok_or(Failure::StateFull)?;
        // No sender could cover a deposit past u64.
        price.checked_mul(size).ok_or(Failure::StorageLimit { deposits: u128::from(price) * u128::from(size), limit: *storage_limit })
    };
    let (code_deposit, iface_deposit) = (deposit(new_code, wasm.len())?, deposit(new_iface, iface.len())?);
    let deposits = u128::from(code_deposit) + u128::from(iface_deposit);
    if deposits > u128::from(*storage_limit) {
        return Err(Failure::StorageLimit { deposits, limit: *storage_limit }.into());
    }
    // 6. Bond them, and store what's new. Together they're within `storage_limit`, a u64.
    let total = u64::try_from(deposits).map_err(|_| Failure::StorageLimit { deposits, limit: *storage_limit })?;
    state.j_debit(j, sender, total).map_err(Failure::native)?;
    let added = (if new_code { wasm.len() as u64 } else { 0 }) + if new_iface { iface.len() as u64 } else { 0 };
    let m = meta_mut(state, j).ok_or_else(|| Failure::native("an upload before programs exist"))?;
    m.escrow = m.escrow.checked_add(total).ok_or_else(|| Failure::native("programs' escrow out of range"))?;
    m.state_bytes = m.state_bytes.checked_add(added).ok_or(Failure::StateFull)?;
    run.notes.borrow_mut().stored(StorageSummary { bytes_delta: added as i64, bonded: total, refunded: 0 });
    run.notes.borrow_mut().debited(OutsideVm { upload_deposit: total, ..OutsideVm::default() });
    run.notes.borrow_mut().uploaded(UploadResult { wasm_hash: hex::encode(code_hash), interface_hash: hex::encode(iface_hash), new_code, new_interface: new_iface, deposit: total });
    if new_code {
        j.programs.code.touch(&state.programs.code, &code_hash);
        state.programs.code.insert(code_hash, Versioned(CodeRecord { wasm: wasm.clone(), exports }));
    }
    if new_iface {
        j.programs.interfaces.touch(&state.programs.interfaces, &iface_hash);
        state.programs.interfaces.insert(iface_hash, Versioned(InterfaceRecord::new(iface.clone())));
    }
    for (kind, hash, new, deposit, size) in [(BLOB_CODE, code_hash, new_code, code_deposit, wasm.len()), (BLOB_INTERFACE, iface_hash, new_iface, iface_deposit, iface.len())] {
        let key = blob_key(kind, &hash);
        j.programs.blob_refs.touch(&state.programs.blob_refs, &key);
        if new {
            // Sizes are within the codec's bounds (256 KiB, 16 KiB).
            let size = u32::try_from(size).map_err(|_| Failure::native("a blob past u32"))?;
            state.programs.blob_refs.insert(key, Versioned(BlobRef { uploader: sender.to_string(), deposit, size, refs: 0, uploaded_at: height, last_used_at: height }));
        } else if let Some(r) = state.programs.blob_refs.get_mut(&key) {
            r.last_used_at = height;
        }
    }
    Ok(())
}
