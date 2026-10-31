//! `/api/tx`'s `program` section (docs/PROGRAMS-V0.md §8.1, §8.2): a program transaction's receipt as
//! the API serves it, **facts apart from claims** (PRG 8.4), and the fee it was charged.
//!
//! A transaction of tag 62, 63 or 64 that a block ran carries `"program"` in its JSON, the receipt
//! the block logged (`types::Block::programs`, by index into `txs`); one with no receipt to show
//! (still pending, or its block pruned from this node) carries `"program": null`. Every other
//! transaction has no such key at all, so a v1 transaction's JSON is what it always was.
//!
//! ```json
//! "program": {
//!   "version": 1, "kind": "call",
//!   "outcome": { "ok": true },
//!   "facts": {
//!     "balance_changes": [{ "account": "boo1…", "change": "-5000000000" }],
//!     "storage": { "bytes_delta": 64, "bonded": "7000000", "refunded": "0" },
//!     "outside_vm": { "record_deposit": "0", "upload_deposit": "0", "account_creation_fee": "0", "total": "0" },
//!     "burned_by_programs": "0",
//!     "fuel": { "limit": 400000, "load": 657360, "used": 2043, "reserved": 1057360, "rate": 250 },
//!     "fee": { "bytes": "67000", "fuel": "264340", "tip": "0" },
//!     "created": null, "upload": null },
//!   "claims": {
//!     "return_data": [{ "program": "boo1…", "untrusted_method": "get", "value": "0", "data": "…hex…" }],
//!     "events": [{ "program": "boo1…", "untrusted_name": "released", "data": "…hex…" }],
//!     "omitted": null } }
//! ```
//!
//! **Facts** are what the chain did and the state holds too (provable through `/api/proof`): the
//! outcome (numbers, never engine text; an abort's own sanitized message is `untrusted_message`),
//! `balance_changes` (the VM's diff only, in drops as decimal strings, the signer's own write bonds
//! among it), `storage` (informational: everything the transaction bonded, which `balance_changes`
//! already counts in part), `outside_vm` (what the signer was debited outside the VM's diff: a
//! create's record deposit, an upload's deposit and the account creation fee a create burns, none
//! of them in `balance_changes`), `burned_by_programs` (what the VM's diff burned: the creation
//! fees of the accounts its programs' own sends opened, out of the programs' balances, never the
//! signer's), `fuel`, `fee` (in drops as strings: what the top-level `fee` sums), the program a
//! create made and what an upload stored. **The signer's balance reconciles exactly: Δbalance(signer)
//! = −(`fee` total + `outside_vm.total`) + `balance_changes[signer]`** (0 for a signer the VM's diff
//! doesn't list), **and the facts conserve FANG: Σ `balance_changes` + (`storage.bonded` −
//! `outside_vm.record_deposit` − `outside_vm.upload_deposit` − `storage.refunded`) +
//! `burned_by_programs` = 0.**
//!
//! **Claims** are what the programs reported, which the chain checks nothing of: the return data of
//! each call (with the program and `value` the call was signed with, and its method, which is the
//! deployer's identifier: `untrusted_method`) and the events (`untrusted_name`, the program's own
//! identifier). `omitted` is `null`, or says how many events and bytes the block's claims budget
//! left out of this receipt (it keeps every fact, §8.1). Nothing absent is left out of the JSON:
//! `omitted`, `created` and `upload` are explicit nulls.
//!
//! `fee` and `fee_display`, at the transaction's top level, are the receipt's sum
//! ([`charged`]): the block's base fee for its bytes, its fuel fee (the reservation at the rate it
//! was charged, never refunded) and its tip.

use super::is_program_tx;
use super::receipt::ProgramReceipt;
use crate::types::{Block, Tx};
use crate::view::ReadView;
use serde_json::{json, Value};

/// The receipt of `tx`, which ran in block `height` as its transaction number `idx`, when it's a
/// program transaction the block has one for. `block` is the block if the caller holds it already
/// (`/api/block`); otherwise it's read here, by index and cache (`ReadView::block`), only for a
/// program transaction: every other `/api/tx` answer keeps its cheap path (one transaction's span
/// of the block log). `None` for any other transaction, and for one whose block this node no longer
/// holds.
pub fn receipt_of(v: &ReadView, tx: &Tx, height: u64, idx: Option<usize>, block: Option<&Block>) -> Option<ProgramReceipt> {
    if !is_program_tx(tx) {
        return None;
    }
    let idx = idx?;
    match block {
        Some(b) => b.programs.get(&idx).cloned(),
        None => v.block(height)?.programs.get(&idx).cloned(),
    }
}

/// What the receipt says the transaction was charged, in drops: its bytes' base fee, its fuel fee
/// and its tip (§8.1's `Σ`; `api::charged` reads it).
pub fn charged(r: &ProgramReceipt) -> u64 {
    r.fee.bytes.saturating_add(r.fee.fuel).saturating_add(r.fee.tip)
}

/// The `program` section for `receipt`, or `null` when there's none to show (the module docs).
pub fn section(receipt: Option<&ProgramReceipt>) -> Value {
    let Some(r) = receipt else { return Value::Null };
    json!({
        "version": r.version,
        "kind": r.kind,
        "outcome": r.outcome,
        "facts": {
            "balance_changes": r.balance_changes,
            "storage": r.storage,
            "outside_vm": r.outside_vm,
            "burned_by_programs": r.burned_by_programs.to_string(),
            "fuel": r.fuel,
            "fee": r.fee,
            "created": r.created,
            "upload": r.upload,
        },
        "claims": {
            "return_data": r.calls.iter().map(|c| json!({ "program": c.program, "untrusted_method": c.method, "value": c.value.to_string(), "data": hex::encode(&c.return_data.0) })).collect::<Vec<_>>(),
            "events": r.events.iter().map(|e| json!({ "program": e.program, "untrusted_name": e.name, "data": hex::encode(&e.data.0) })).collect::<Vec<_>>(),
            "omitted": r.omitted,
        },
    })
}

#[cfg(test)]
mod tests;
