//! The `program_receipt` target (docs/PROGRAMS-V0.md §8.1, §10.4): a program transaction's receipt
//! as JSON, in the forms the node stores and serves it.
//!
//! A receipt is display only (no root commits to it, A10), but a node writes it into the block log
//! with its block (`types::Block::programs`, a map by index into the block's transactions), reads it
//! back at startup, and serves it as `/api/tx`'s `program` section and in `/api/block`, which the SDK
//! and the explorer read. So the target takes JSON of a receipt (`ProgramReceipt`), of a block's map
//! of them, or of a whole block, whichever it parses as, and holds every receipt in it to:
//!
//! - **the round trip**: its JSON reads back as the receipt it came from, through bytes and through
//!   a `Value`, and writing that again gives the same bytes (one encoding of each receipt);
//! - **its outcome's JSON**: `ok`, and for a failure the number and code its variant has;
//! - **the API's section** (`tx_json::section`): never a panic, and each of its facts and claims
//!   the receipt's own field, as the SDK reads them; and `charged`, the receipt's total, the sum
//!   of the fee's three parts, saturating;
//! - **the claims budget** (`receipt::apply_claims_budget`), held to an independent model of the
//!   rule: in the block's order each receipt whose claims fit what's left of
//!   [`MAX_BLOCK_CLAIM_BYTES`] keeps them and spends that much, one whose claims don't loses them
//!   all and says what it lost, and doing it again changes nothing;
//! - **`for_block`**, which hands a block its receipts by transaction index: a failed transaction's
//!   receipt that says it applied becomes `native` with nothing of its action left, a receipt of a
//!   transaction the block doesn't hold is dropped, and the budget applies.
//!
//! Accepted: the input parsed as one of the three.

use super::oracle;
use super::*;
use crate::programs::receipt::{self, apply_claims_budget, BalanceChange, CallResult, FeeParts, FuelUse, Numbered, Omitted, OutsideVm, ProgramReceipt, ReceiptEvent, ReceiptKind, ReceiptOutcome, StorageSummary, UploadResult, EVENT_CLAIM_BYTES, MAX_BLOCK_CLAIM_BYTES};
use crate::programs::state::BlockRun;
use crate::programs::tx_json;
use crate::types::{Block, HexBytes};

pub fn program_receipt(data: &[u8]) -> bool {
    if let Ok(r) = serde_json::from_slice::<ProgramReceipt>(data) {
        check_receipt(&r);
        check_block(&BTreeMap::from([(0, r)]));
        return true;
    }
    if let Ok(receipts) = serde_json::from_slice::<BTreeMap<usize, ProgramReceipt>>(data) {
        receipts.values().for_each(check_receipt);
        check_block(&receipts);
        return true;
    }
    let Ok(block) = serde_json::from_slice::<Block>(data) else { return false };
    // The block log's line: what the node writes reads back, and a block without receipts has no
    // `programs` key (a v1 block's record is what it always was, §10.5).
    let json = serde_json::to_value(&block).expect("a block serializes");
    assert_eq!(json.get("programs").is_some(), !block.programs.is_empty(), "a block's `programs` key is there exactly when it has receipts");
    let back: Block = serde_json::from_value(json.clone()).expect("a block must read back from its JSON");
    assert_eq!(serde_json::to_value(&back).expect("a block serializes"), json, "a block's JSON round trip changed it");
    assert_eq!(back.programs, block.programs, "a block's receipts changed in its JSON round trip");
    block.programs.values().for_each(check_receipt);
    check_block(&block.programs);
    true
}

/// What one receipt is held to (the module docs).
fn check_receipt(r: &ProgramReceipt) {
    let json = serde_json::to_vec(r).expect("a receipt serializes");
    let back: ProgramReceipt = serde_json::from_slice(&json).expect("a receipt must read back from the JSON it wrote");
    assert_eq!(&back, r, "a receipt's JSON round trip changed it");
    assert_eq!(serde_json::to_vec(&back).expect("a receipt serializes"), json, "a receipt has more than one encoding");
    let value = serde_json::to_value(r).expect("a receipt serializes");
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&json).expect("JSON"), value, "a receipt's bytes and its value differ");
    assert_eq!(&serde_json::from_value::<ProgramReceipt>(value.clone()).expect("a receipt must read back from its value"), r, "a receipt's value round trip changed it");

    // Its outcome: `ok`, and for a failure its number and code, which are §8.1's table's (written
    // out in `oracle::spec::OUTCOMES`, not read back from the code that writes the JSON).
    let outcome = &value["outcome"];
    assert_eq!(outcome["ok"], r.outcome.is_ok(), "the outcome's `ok`");
    let table = oracle::spec::OUTCOMES.iter().find(|(code, _)| *code == r.outcome.code()).map(|(_, n)| *n);
    assert_eq!(table, Some(r.outcome.number()), "the outcome {} is numbered {}, §8.1's table numbers it {table:?}", r.outcome.code(), r.outcome.number());
    if !r.outcome.is_ok() {
        assert_eq!((&outcome["number"], &outcome["code"]), (&json!(table), &json!(r.outcome.code())), "a failure's number and code");
    }

    // The API's section reads each from the receipt.
    assert_eq!(tx_json::section(None), serde_json::Value::Null, "no receipt, no section");
    let section = tx_json::section(Some(r));
    assert_eq!((&section["version"], &section["kind"], &section["outcome"]), (&value["version"], &value["kind"], &value["outcome"]), "the section's version, kind and outcome");
    let facts = &section["facts"];
    for field in ["balance_changes", "storage", "outside_vm", "burned_by_programs", "fuel", "fee", "created", "upload"] {
        assert_eq!(facts[field], value[field], "the section's fact `{field}`");
    }
    let claims = &section["claims"];
    assert_eq!(claims["omitted"], value["omitted"], "the section's `omitted`");
    let (returns, events) = (claims["return_data"].as_array().expect("an array"), claims["events"].as_array().expect("an array"));
    assert_eq!((returns.len(), events.len()), (r.calls.len(), r.events.len()), "the section's claims");
    for (c, j) in r.calls.iter().zip(returns) {
        assert_eq!((&j["program"], &j["untrusted_method"], &j["value"], &j["data"]), (&json!(c.program), &json!(c.method), &json!(c.value.to_string()), &json!(hex::encode(&c.return_data.0))), "a call's return data in the section");
    }
    for (e, j) in r.events.iter().zip(events) {
        assert_eq!((&j["program"], &j["untrusted_name"], &j["data"]), (&json!(e.program), &json!(e.name), &json!(hex::encode(&e.data.0))), "an event in the section");
    }
    let total = (u128::from(r.fee.bytes) + u128::from(r.fee.fuel) + u128::from(r.fee.tip)).min(u128::from(u64::MAX));
    assert_eq!(u128::from(tx_json::charged(r)), total, "what the receipt says it charged");
    assert_eq!(r.claim_bytes(), claims_of(r).0, "the claims a receipt counts against its block's budget");
}

/// A receipt's claims in bytes (events: data, name and the program's address; return data: its
/// length), and what `omitted` says of them: events, event bytes and return bytes.
fn claims_of(r: &ProgramReceipt) -> (u64, Omitted) {
    let event_bytes: u64 = r.events.iter().map(|e| (e.data.0.len() + e.name.len()) as u64).fold(0, u64::saturating_add);
    let return_bytes: u64 = r.calls.iter().map(|c| c.return_data.0.len() as u64).fold(0, u64::saturating_add);
    let total = event_bytes.saturating_add(EVENT_CLAIM_BYTES.saturating_mul(r.events.len() as u64)).saturating_add(return_bytes);
    (total, Omitted { events: r.events.len() as u64, event_bytes, return_bytes })
}

/// `r` as a block that ran it hands it on when it didn't apply after all: `native`, with nothing of
/// its action left (what it paid and the fuel it used stay).
fn failed_native(r: &ProgramReceipt) -> ProgramReceipt {
    let calls = r.calls.iter().map(|c| CallResult { return_data: HexBytes::default(), ..c.clone() }).collect();
    ProgramReceipt { outcome: ReceiptOutcome::Native, calls, events: vec![], omitted: None, balance_changes: vec![], storage: StorageSummary::default(), outside_vm: OutsideVm::default(), burned_by_programs: 0, created: None, upload: None, ..r.clone() }
}

/// The claims budget by the rule alone (the module docs).
fn budgeted(receipts: &BTreeMap<usize, ProgramReceipt>) -> BTreeMap<usize, ProgramReceipt> {
    let mut left = MAX_BLOCK_CLAIM_BYTES;
    let mut out = BTreeMap::new();
    for (&i, r) in receipts {
        let (claims, omitted) = claims_of(r);
        if claims <= left {
            left -= claims;
            out.insert(i, r.clone());
        } else {
            let calls = r.calls.iter().map(|c| CallResult { return_data: HexBytes::default(), ..c.clone() }).collect();
            out.insert(i, ProgramReceipt { events: vec![], calls, omitted: Some(omitted), ..r.clone() });
        }
    }
    out
}

/// A receipt with its events repeated until they come to more than `target` bytes of claims (none if
/// it has no events): the claims budget is 256 KiB, which would take half a megabyte of JSON to
/// reach from the input alone.
fn inflated(r: &ProgramReceipt, target: u64) -> ProgramReceipt {
    let per: u64 = r.events.iter().map(|e| (e.data.0.len() + e.name.len()) as u64 + EVENT_CLAIM_BYTES).sum();
    if per == 0 {
        return r.clone();
    }
    let times = (target / per + 1) as usize;
    ProgramReceipt { events: r.events.iter().cloned().cycle().take(r.events.len() * times).collect(), ..r.clone() }
}

/// What a block's receipts are held to together (`check_together`), as they are, and with their
/// events repeated until the claims budget's edge is within reach: each receipt's to more than half
/// of it (so any two come to more than all of it, and a smaller one after still fits), and the
/// first to more than all of it.
fn check_block(receipts: &BTreeMap<usize, ProgramReceipt>) {
    check_together(receipts);
    if receipts.values().any(|r| !r.events.is_empty()) {
        let halves: BTreeMap<usize, ProgramReceipt> = receipts.iter().map(|(i, r)| (*i, inflated(r, MAX_BLOCK_CLAIM_BYTES / 2))).collect();
        check_together(&halves);
        let mut first = false;
        let one: BTreeMap<usize, ProgramReceipt> = receipts
            .iter()
            .map(|(i, r)| {
                let big = !std::mem::replace(&mut first, true) && !r.events.is_empty();
                (*i, if big { inflated(r, MAX_BLOCK_CLAIM_BYTES) } else { r.clone() })
            })
            .collect();
        check_together(&one);
    }
}

/// What a block's receipts are held to together: the claims budget and `for_block`.
fn check_together(receipts: &BTreeMap<usize, ProgramReceipt>) {
    let mut cut = receipts.clone();
    apply_claims_budget(&mut cut);
    assert_eq!(cut, budgeted(receipts), "the claims budget cut a block's receipts otherwise than its rule says");
    let spent: u64 = cut.values().map(ProgramReceipt::claim_bytes).fold(0, u64::saturating_add);
    assert!(spent <= MAX_BLOCK_CLAIM_BYTES, "a block records {spent} bytes of claims, past {MAX_BLOCK_CLAIM_BYTES}");
    let mut again = cut.clone();
    apply_claims_budget(&mut again);
    assert_eq!(again, cut, "the claims budget isn't idempotent");

    // `for_block` on a state whose block run holds them: one transaction a receipt, and none, all
    // or the odd ones failed; plus a receipt of a transaction the block doesn't hold, one of
    // another height and a second one of a transaction that has one.
    if receipts.len() > 64 {
        return;
    }
    let height = 7;
    let tx = |i: usize| Tx { nonce: i as u64, ..harness_tx() };
    let txs: Vec<Tx> = (0..receipts.keys().next_back().map_or(0, |k| k + 1).min(64)).map(tx).collect();
    let none = BTreeMap::new();
    let all: BTreeMap<usize, String> = (0..txs.len()).map(|i| (i, "a native check failed".to_string())).collect();
    let odd: BTreeMap<usize, String> = all.iter().filter(|(i, _)| *i % 2 == 1).map(|(i, why)| (*i, why.clone())).collect();
    for failed in [&none, &all, &odd] {
        let mut state = State::default();
        let mut run: Vec<(String, ProgramReceipt)> = receipts.iter().filter(|(i, _)| **i < txs.len()).map(|(i, r)| (txs[*i].id(), r.clone())).collect();
        if let Some(r) = receipts.values().next() {
            run.insert(0, (tx(txs.len() + 1).id(), r.clone()));
            if let Some((id, first)) = run.get(1).cloned() {
                run.push((id, failed_native(&first)));
            }
        }
        state.programs.block = BlockRun { height, fuel: 0, receipts: run.clone() };
        assert!(receipt::for_block(&mut state, height + 1, &txs, failed).is_empty(), "receipts of another height were handed to a block");
        state.programs.block = BlockRun { height, fuel: 0, receipts: run };
        let got = receipt::for_block(&mut state, height, &txs, failed);
        assert!(state.programs.block.receipts.is_empty(), "for_block left the block's receipts behind");
        let expected: BTreeMap<usize, ProgramReceipt> = receipts.iter().filter(|(i, _)| **i < txs.len()).map(|(i, r)| (*i, if failed.contains_key(i) && r.outcome.is_ok() { failed_native(r) } else { r.clone() })).collect();
        assert_eq!(got, budgeted(&expected), "for_block handed a block other receipts than the rule says");
    }
}

/// A transaction to give receipts to: its id depends on its nonce (every other field fixed).
fn harness_tx() -> Tx {
    program_tx(Action::UploadCode { wasm: HexBytes(types::WASM_HEADER.to_vec()), interface: HexBytes(vec![1, 1]), storage_limit: 0 })
}

// ----------------------------------------------------------------------------------------- seeds

/// A receipt with every field set, as an applied call: `calls` returned data, events, balance
/// changes, storage, a created program and what an upload stored.
fn full() -> ProgramReceipt {
    let program = Keys::from_seed([0x61; 32]).address();
    ProgramReceipt {
        version: receipt::RECEIPT_VERSION,
        kind: ReceiptKind::Call,
        outcome: ReceiptOutcome::Ok,
        fuel: FuelUse { limit: 400_000, load: 657_360, used: 2_043, reserved: 1_057_360, rate: 250 },
        fee: FeeParts { bytes: 67_000, fuel: 264_340, tip: 3 },
        calls: vec![CallResult { program: program.clone(), method: "increment".into(), value: 5 * types::DROPS_PER_FANG, return_data: HexBytes(vec![1, 0, 0, 0, 0, 0, 0, 0]) }, CallResult { program: program.clone(), method: "get".into(), value: 0, return_data: HexBytes(vec![]) }],
        events: vec![ReceiptEvent { program: program.clone(), name: "transfer".into(), data: HexBytes(vec![7; 72]) }],
        omitted: None,
        balance_changes: vec![BalanceChange { account: program.clone(), change: 5_000_000_000 }, BalanceChange { account: Keys::from_seed([0x62; 32]).address(), change: -5_012_800_000 }],
        storage: StorageSummary { bytes_delta: 128, bonded: 12_800_000, refunded: 0 },
        outside_vm: OutsideVm { record_deposit: 25_600_000, upload_deposit: 338_900_000, account_creation_fee: 100_000_000 },
        burned_by_programs: 100_000_000,
        created: Some(program),
        upload: Some(UploadResult { wasm_hash: "ab".repeat(32), interface_hash: "cd".repeat(32), new_code: true, new_interface: false, deposit: 338_900_000 }),
    }
}

/// Every outcome a receipt can carry, each with its detail.
fn outcomes() -> Vec<(&'static str, ReceiptOutcome)> {
    let program = Keys::from_seed([0x61; 32]).address();
    let rule = Numbered { number: 8, name: "memory_limit".into() };
    vec![
        ("ok", ReceiptOutcome::Ok),
        ("trapped", ReceiptOutcome::Trapped { program: program.clone(), trap: Numbered { number: 1, name: "out_of_fuel".into() } }),
        ("aborted", ReceiptOutcome::Aborted { program, abort_code: 11, untrusted_message: "balance short".into() }),
        ("commit", ReceiptOutcome::Commit { commit_error: 2 }),
        ("value-cap", ReceiptOutcome::ValueCap { cap: 500 * types::DROPS_PER_FANG, control: 530 * u128::from(types::DROPS_PER_FANG) }),
        ("exit-only", ReceiptOutcome::ExitOnly),
        ("storage-limit", ReceiptOutcome::StorageLimit),
        ("state-full", ReceiptOutcome::StateFull),
        ("invalid-code", ReceiptOutcome::InvalidCode { rule: rule.clone(), func: Some(3), offset: Some(1_234) }),
        ("invalid-code-without-place", ReceiptOutcome::InvalidCode { rule, func: None, offset: None }),
        ("translation-refused", ReceiptOutcome::TranslationRefused),
        ("interface-mismatch", ReceiptOutcome::InterfaceMismatch),
        ("native", ReceiptOutcome::Native),
    ]
}

/// A failed receipt's shape: what it paid and the fuel it used, its calls without return data.
fn failed(outcome: ReceiptOutcome) -> ProgramReceipt {
    ProgramReceipt { outcome, ..failed_native(&full()) }
}

/// The `program_receipt` seeds: a receipt of each outcome, the receipts the v2 fixture's own seeds
/// make (what the node wrote, each kind and outcome they reach, the first of each), a block's map
/// of receipts and a whole block carrying them.
pub(super) fn seeds() -> Vec<(String, Vec<u8>)> {
    let json = |r: &ProgramReceipt| serde_json::to_vec(r).expect("a receipt serializes");
    let mut out: Vec<(String, Vec<u8>)> = vec![("applied-call-with-every-field".into(), json(&full()))];
    for (name, outcome) in outcomes() {
        if name != "ok" {
            out.push((format!("failed-{name}"), json(&failed(outcome))));
        }
    }
    // An applied create and an applied upload, as the node writes them.
    let upload = ProgramReceipt { kind: ReceiptKind::Upload, calls: vec![], events: vec![], balance_changes: vec![], created: None, ..full() };
    let create = ProgramReceipt { kind: ReceiptKind::Create, upload: None, ..full() };
    out.push(("applied-upload".into(), json(&upload)));
    out.push(("applied-create".into(), json(&create)));
    // What the fixture's own runs wrote, by kind and outcome.
    let mut real: BTreeMap<(&'static str, &'static str), ProgramReceipt> = BTreeMap::new();
    for (_, seed) in v2::seeds() {
        for (key, r) in v2::run_for_receipts(&seed) {
            real.entry(key).or_insert(r);
        }
    }
    for ((kind, code), r) in &real {
        out.push((format!("real-{kind}-{code}").replace('_', "-"), json(r)));
    }
    // A block's receipts, by index into its transactions, and a block that carries them. (The
    // claims budget is reached by repeating their events, `check_block`.)
    let more = ProgramReceipt { events: vec![ReceiptEvent { program: Keys::from_seed([0x61; 32]).address(), name: "more".into(), data: HexBytes(vec![9; 100]) }; 3], ..full() };
    let map = BTreeMap::from([(0usize, full()), (2, more), (3, failed(ReceiptOutcome::Native))]);
    out.push(("block-receipts".into(), serde_json::to_vec(&map).expect("a map serializes")));
    let proposer = Keys::from_seed([0x63; 32]).address();
    let block = Block { height: 3, prev_hash: "p".into(), timestamp_ms: 1, view: 3, parent_view: 2, version: 2, signal: 2, proposer, state_root: "r".into(), txs: vec![], hash: "h".into(), signature: String::new(), failed: BTreeMap::from([(3, "native".to_string())]), skipped: vec![], base_fee: None, programs: map };
    out.push(("block-with-receipts".into(), serde_json::to_vec(&block).expect("a block serializes")));
    out
}
