//! The mempool's side of programs (docs/PROGRAMS-V0.md §6.4): **the mempool never runs the VM.**
//!
//! A program transaction (tags 62–64) in the mempool is judged by admission alone, the same
//! function a block runs (`super::admit`, in `ApplyMode::Mempool`: everything but the block's fuel
//! budget), and its action phase runs nothing: [`mempool_apply`] debits the most it could cost its
//! sender beyond the fee, and `econ::check_locks` holds that to transferable FANG. The mempool's
//! simulations are never committed, so this changes nothing real; it means a sender's later queued
//! transactions are judged against the worst its program transaction could cost, and one whose
//! worst case its sender can't cover never enters the mempool. A proposer runs it; it's included
//! and charged if it fails (§6.3).
//!
//! At the door ([`mempool_admit`], node policy rather than consensus): at most
//! [`MAX_MEMPOOL_UPLOADS`] pending uploads, none larger than the next block may carry, and nothing
//! queued behind its sender's own pending upload or create that needs what it stores (code is
//! usable from the block after its upload, a program from the block after its creation, A5; the
//! simulations write no records, so the refusal says when to send it instead).

use super::digest;
use super::fuel::parse_hash;
use crate::chain::{short, Journal, State};
use crate::types::{Action, Tx};
use anyhow::{anyhow, bail, Result};

/// The most uploads a node holds pending (§2.2, A38). Each is admittable at the current rate, so
/// proposers include and charge it within blocks (two maximal uploads fit a default block's
/// fuel): a slot can't be held for free. Node policy, not consensus.
pub const MAX_MEMPOOL_UPLOADS: usize = 16;

/// `State::apply_j`'s action phase for a program transaction in `ApplyMode::Mempool` (§6.4): it
/// runs nothing, and debits from `sender` (and burns, in the simulation) the most the transaction
/// could cost beyond its fee ([`worst_case`]). `Err` if the sender can't cover it, which makes the
/// transaction `Failed` in the simulation, and the mempool refuses (or drops) it.
pub fn mempool_apply(state: &mut State, j: &mut Journal, tx: &Tx, sender: &str) -> Result<()> {
    let worst = worst_case(state, tx)?;
    if worst > 0 {
        state.j_debit(j, sender, worst).map_err(|e| anyhow!("its worst case is {} beyond the fee: {e}", crate::types::fmt_fang(worst)))?;
        state.j_burn(j, worst);
    }
    Ok(())
}

/// The most `tx` could cost its sender beyond its fee (§6.4), what a block's execution may debit:
/// for a call, every call's `value` plus `storage_limit`; for a create, `init.value` plus
/// `storage_limit`, plus the account creation fee when its address is new; for an upload,
/// `storage_limit`; 0 for anything else.
pub fn worst_case(state: &State, tx: &Tx) -> Result<u64> {
    let total = match &tx.action {
        Action::UploadCode { storage_limit, .. } => Some(*storage_limit),
        Action::CreateProgram { init, storage_limit, .. } => {
            let creator = crate::crypto::address_bytes(&tx.from).ok_or_else(|| anyhow!("`from` isn't an address"))?;
            let opening = state.creation_fee_for(&digest::program_address(state.params.prefix(), &creator, tx.nonce));
            init.as_ref().map_or(0, |i| i.value).checked_add(*storage_limit).and_then(|v| v.checked_add(opening))
        }
        Action::CallProgram { calls, storage_limit, .. } => calls.iter().try_fold(*storage_limit, |sum, c| sum.checked_add(c.value)),
        _ => Some(0),
    };
    total.ok_or_else(|| anyhow!("the FANG this transaction could move overflows"))
}

/// The mempool's door for program transactions (`Chain::admit_to_mempool`, beside Ghost Mode's),
/// given the queued txs and the height the next block has: refuses the 17th pending upload, an
/// upload the next block couldn't carry, and a create or call that needs what its sender's own
/// pending upload or create would store. Every other tx passes. Node policy, not consensus.
pub fn mempool_admit(state: &State, queued: &[Tx], tx: &Tx, next: u64) -> Result<()> {
    match &tx.action {
        Action::UploadCode { .. } => {
            if queued.iter().filter(|q| matches!(q.action, Action::UploadCode { .. })).count() >= MAX_MEMPOOL_UPLOADS {
                bail!("too many pending uploads; try again shortly");
            }
            // The size limit can be a parameter below an upload's own bound (`MAX_CODE_TX_BYTES`):
            // no proposer could include one past it (`consensus.rs`'s packing).
            let limit = crate::econ::slot(state, next).map_or(state.chain_params.max_block_bytes, |s| s.max_block_bytes);
            if tx.size() > limit {
                bail!("this upload is {} bytes, more than block {next} may carry ({limit} bytes)", tx.size());
            }
        }
        Action::CreateProgram { wasm_hash, interface_hash, bindings, .. } => {
            // What the create needs that isn't stored yet (a blob stored already is usable whatever
            // a pending upload of it does: an upload of a stored blob only refreshes it, §2.2).
            let code = parse_hash(wasm_hash).filter(|h| !state.programs.code.contains_key(h));
            let interface = parse_hash(interface_hash).filter(|h| !state.programs.interfaces.contains_key(h));
            if code.is_some() || interface.is_some() {
                for q in own(queued, tx) {
                    let Action::UploadCode { wasm: w, interface: i, .. } = &q.action else { continue };
                    if code == Some(digest::wasm_hash(&w.0)) || interface == Some(digest::interface_hash(&i.0)) {
                        bail!("your pending upload (nonce {}) stores code this create needs, and code is usable from the block after its upload: from block {} at the earliest. Send the create once the upload is final", q.nonce, next + 1);
                    }
                }
            }
            behind_create(state, queued, tx, bindings.iter().map(|b| b.program.as_str()), next)?;
        }
        Action::CallProgram { calls, bindings, .. } => {
            behind_create(state, queued, tx, calls.iter().map(|c| c.program.as_str()).chain(bindings.iter().map(|b| b.program.as_str())), next)?;
        }
        _ => {}
    }
    Ok(())
}

/// Refuses `tx` if a program it names is one its sender's own pending create would make.
fn behind_create<'a>(state: &State, queued: &[Tx], tx: &Tx, named: impl Iterator<Item = &'a str> + Clone, next: u64) -> Result<()> {
    let Some(creator) = crate::crypto::address_bytes(&tx.from) else { return Ok(()) };
    for q in own(queued, tx).filter(|q| matches!(q.action, Action::CreateProgram { .. })) {
        let address = digest::program_address(state.params.prefix(), &creator, q.nonce);
        if named.clone().any(|p| p == address) {
            bail!("your pending create (nonce {}) makes program {}, and a program is usable from the block after its creation: from block {} at the earliest. Send this once the create is final", q.nonce, short(&address), next + 1);
        }
    }
    Ok(())
}

/// The queued txs of `tx`'s sender (signed ones: unsigned ghost txs have no sender).
fn own<'a>(queued: &'a [Tx], tx: &'a Tx) -> impl Iterator<Item = &'a Tx> {
    queued.iter().filter(move |q| !q.is_unsigned() && q.from == tx.from)
}

#[cfg(test)]
mod tests;
