//! `/api/tx`'s `program` section (WBS S6b's acceptance): a unit test of its exact JSON, then
//! real receipts through the router on a dev chain with programs on (`probe::Dev`): an upload, a
//! create, a call batch with return data, an event and FANG, a failing call, a block whose claims
//! pass the budget, a pending program transaction and a v1 transfer, each read from `/api/tx/{id}`
//! (and the block's and the account's views of the same transaction).

use super::*;
use crate::api::{router, ClientIpPolicy, Node};
use crate::crypto;
use crate::programs::digest;
use crate::programs::probe::{calls, create, keys, probe_bytes, probe_interface, upload, Dev, ALICE, BOB, FANG, FUEL, PROBE_WASM};
use crate::programs::receipt::{CallResult, FeeParts, FuelUse, Omitted, OutsideVm, ProgramReceipt, ReceiptEvent, ReceiptKind, ReceiptOutcome, StorageSummary, UploadResult};
use crate::types::{fmt_fang, Action, HexBytes};
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt as _;

/// A receipt with every field set, as the block log would hold it.
fn full() -> ProgramReceipt {
    ProgramReceipt {
        version: 1,
        kind: ReceiptKind::Call,
        outcome: ReceiptOutcome::Aborted { program: "boo1p".into(), abort_code: 7, untrusted_message: "e".into() },
        fuel: FuelUse { limit: 400_000, load: 657_360, used: 2_043, reserved: 1_057_360, rate: 250 },
        fee: FeeParts { bytes: 67_000, fuel: 264_340, tip: 5 },
        calls: vec![CallResult { program: "boo1p".into(), method: "get".into(), value: 9_007_199_254_740_993, return_data: HexBytes(vec![1, 2]) }],
        events: vec![ReceiptEvent { program: "boo1p".into(), name: "released".into(), data: HexBytes(vec![0xab]) }],
        omitted: Some(Omitted { events: 3, event_bytes: 40, return_bytes: 9 }),
        balance_changes: vec![crate::programs::receipt::BalanceChange { account: "boo1a".into(), change: -9_007_199_254_740_993 }],
        storage: StorageSummary { bytes_delta: -64, bonded: 7, refunded: 1 },
        outside_vm: OutsideVm { record_deposit: 5, upload_deposit: 9_007_199_254_740_993, account_creation_fee: 100_000_000 },
        burned_by_programs: 9_007_199_254_740_995,
        created: Some("boo1new".into()),
        upload: Some(UploadResult { wasm_hash: "aa".repeat(32), interface_hash: "bb".repeat(32), new_code: true, new_interface: false, deposit: 3 }),
    }
}

/// The section's exact JSON: facts apart from claims; amounts as strings (a balance change and a
/// call's value past 2^53 keep every digit), fuel and counts as numbers, a method as
/// `untrusted_method` and an event's name as `untrusted_name` (never `method` or `name`), the
/// explicit nulls, and none of it present for a transaction with no receipt.
#[test]
fn the_section_serves_facts_apart_from_claims() {
    let j = section(Some(&full()));
    assert_eq!(
        j,
        json!({
            "version": 1,
            "kind": "call",
            "outcome": { "ok": false, "number": 2, "code": "aborted", "program": "boo1p", "abort_code": 7, "untrusted_message": "e" },
            "facts": {
                "balance_changes": [{ "account": "boo1a", "change": "-9007199254740993" }],
                "storage": { "bytes_delta": -64, "bonded": "7", "refunded": "1" },
                "outside_vm": { "record_deposit": "5", "upload_deposit": "9007199254740993", "account_creation_fee": "100000000", "total": "9007199354740998" },
                "burned_by_programs": "9007199254740995",
                "fuel": { "limit": 400_000, "load": 657_360, "used": 2_043, "reserved": 1_057_360, "rate": 250 },
                "fee": { "bytes": "67000", "fuel": "264340", "tip": "5" },
                "created": "boo1new",
                "upload": { "wasm_hash": "aa".repeat(32), "interface_hash": "bb".repeat(32), "new_code": true, "new_interface": false, "deposit": "3" },
            },
            "claims": {
                "return_data": [{ "program": "boo1p", "untrusted_method": "get", "value": "9007199254740993", "data": "0102" }],
                "events": [{ "program": "boo1p", "untrusted_name": "released", "data": "ab" }],
                "omitted": { "events": 3, "event_bytes": 40, "return_bytes": 9 },
            },
        })
    );
    let bare = ProgramReceipt { outcome: ReceiptOutcome::Ok, calls: vec![], events: vec![], omitted: None, balance_changes: vec![], created: None, upload: None, ..full() };
    let j = section(Some(&bare));
    assert_eq!((&j["facts"]["created"], &j["facts"]["upload"], &j["claims"]["omitted"]), (&Value::Null, &Value::Null, &Value::Null), "absent is an explicit null");
    assert_eq!((j["claims"]["events"].clone(), j["claims"]["return_data"].clone(), j["facts"]["balance_changes"].clone()), (json!([]), json!([]), json!([])));
    assert_eq!(section(None), Value::Null);
    // What the top-level `fee` sums.
    assert_eq!(charged(&full()), 67_000 + 264_340 + 5);
    assert_eq!(charged(&ProgramReceipt { fee: FeeParts { bytes: u64::MAX, fuel: 1, tip: 1 }, ..full() }), u64::MAX, "saturating");
}

fn client() -> MockConnectInfo<SocketAddr> {
    MockConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap())
}

async fn get(n: &Node, uri: &str) -> Value {
    let app = router(n.clone()).layer(client());
    let res = app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "{uri}");
    serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 32 << 20).await.unwrap()).unwrap()
}

/// What the dev chain's blocks held, with the balances around the transactions that matter.
struct Chainwork {
    n: Node,
    _d: Dev,
    program: String,
    upload: crate::types::Tx,
    create: crate::types::Tx,
    batch: crate::types::Tx,
    failing: crate::types::Tx,
    /// The nine transactions of the block whose claims pass the budget.
    crowd: Vec<crate::types::Tx>,
    transfer: crate::types::Tx,
    /// Alice's balance before and after each of upload, create, batch, failing.
    alice: [(u64, u64); 4],
    creation_fee: u64,
    record_deposit: u64,
}

fn chainwork(tag: &str) -> Chainwork {
    let mut d = Dev::new(tag);
    let alice = keys(ALICE);
    let balance = |d: &Dev| d.balance(ALICE);
    let iface = probe_interface();
    let mut spans = vec![];
    let up = d.tx(&alice, upload(PROBE_WASM, &iface));
    let before = balance(&d);
    d.block(vec![up.clone()]);
    spans.push((before, balance(&d)));
    let cr = d.tx(&alice, create(PROBE_WASM, &iface));
    let program = digest::program_address(&d.prefix(), &crypto::address_bytes(&cr.from).unwrap(), cr.nonce);
    let before = balance(&d);
    d.block(vec![cr.clone()]);
    spans.push((before, balance(&d)));
    // A batch: return data (4 KiB), an event (4 KiB of data, named `e`) and 5 FANG into `fund`.
    let batch = d.tx(&alice, calls(vec![d.call(&program, "ret", vec![], 0), d.call(&program, "emit", vec![], 0), d.call(&program, "fund", vec![], 5 * FANG)], vec![]));
    let before = balance(&d);
    d.block(vec![batch.clone()]);
    spans.push((before, balance(&d)));
    // A call that aborts (code 7, message `e`): charged, included, no claims.
    let failing = d.tx(&alice, calls(vec![d.call(&program, "no", vec![], 0)], vec![]));
    let before = balance(&d);
    let b = d.block(vec![failing.clone()]);
    assert_eq!(b.failed.len(), 1);
    spans.push((before, balance(&d)));
    // Nine transactions of eight calls that each return 4 KiB: the block's claims budget (256 KiB)
    // holds the first eight and leaves the ninth's out.
    let crowd = d.txs(&keys(BOB), vec![calls(vec![d.call(&program, "ret", vec![], 0); 8], vec![]); 9]);
    d.block(crowd.clone());
    let transfer = d.tx(&alice, Action::Transfer { to: keys(BOB).address_with(&d.prefix()), amount: FANG });
    d.block(vec![transfer.clone()]);
    let creation_fee = d.c.state.chain_params.account_creation_fee;
    let record_deposit = d.c.state.programs.records[&program].record_deposit;
    let chain = Arc::new(Mutex::new(d.take()));
    let n = Node::new(chain, None, vec![], ClientIpPolicy::default());
    Chainwork { n, _d: d, program, upload: up, create: cr, batch, failing, crowd, transfer, alice: spans.try_into().unwrap(), creation_fee, record_deposit }
}

/// The key set of `j`, sorted.
fn keys_of(j: &Value) -> BTreeSet<&str> {
    j.as_object().unwrap().keys().map(String::as_str).collect()
}

fn drops(j: &Value) -> u64 {
    j.as_str().unwrap().parse().unwrap()
}

/// The fee a transaction was charged, from its section's parts, which the top-level `fee` and
/// `fee_display` are the sum of.
fn fee_of(tx: &Value) -> u64 {
    let parts = &tx["program"]["facts"]["fee"];
    let sum = drops(&parts["bytes"]) + drops(&parts["fuel"]) + drops(&parts["tip"]);
    assert_eq!((drops(&tx["fee"]), tx["fee_display"].as_str().unwrap()), (sum, fmt_fang(sum).as_str()), "fee is the receipt's sum: {tx}");
    sum
}

/// An upload and a create: what they stored and bonded is in `storage` and `upload`/`created`, and
/// what they debited from the sender outside the VM (the deposit, a create's record deposit and the
/// creation fee of the program's account) is `outside_vm`, not `balance_changes` (the VM never ran for
/// either): the sender's balance is down by exactly the fee and `outside_vm.total`. Nothing is claimed.
#[tokio::test]
async fn an_upload_and_a_create_report_their_deposits_outside_the_vm_not_in_balance_changes() {
    let w = chainwork("txjson-deposits");
    let up = get(&w.n, &format!("/api/tx/{}", w.upload.id())).await;
    assert_eq!((up["status"].as_str(), up["kind"].as_str(), up["program"]["kind"].as_str(), &up["program"]["outcome"]), (Some("finalized"), Some("upload_code"), Some("upload"), &json!({ "ok": true })));
    let facts = &up["program"]["facts"];
    let (wasm_hash, interface_hash) = (hex::encode(digest::wasm_hash(PROBE_WASM)), hex::encode(digest::interface_hash(&probe_interface())));
    let deposit = drops(&facts["upload"]["deposit"]);
    assert_eq!(facts["upload"], json!({ "wasm_hash": wasm_hash, "interface_hash": interface_hash, "new_code": true, "new_interface": true, "deposit": deposit.to_string() }));
    assert!(deposit > 0);
    assert_eq!((facts["balance_changes"].clone(), facts["created"].clone()), (json!([]), Value::Null), "the deposit is debited outside the VM");
    assert_eq!(drops(&facts["storage"]["bonded"]), deposit);
    assert_eq!(facts["outside_vm"], json!({ "record_deposit": "0", "upload_deposit": deposit.to_string(), "account_creation_fee": "0", "total": deposit.to_string() }));
    assert_eq!(facts["storage"]["bytes_delta"].as_i64(), Some((PROBE_WASM.len() + probe_interface().len()) as i64));
    assert_eq!(facts["fuel"]["limit"], facts["fuel"]["used"], "an upload's limit and used are its upload_fuel");
    let fee = fee_of(&up);
    let (before, after) = w.alice[0];
    assert_eq!(before - after, fee + deposit, "balance down by the fee and the deposit");
    assert_eq!((up["program"]["claims"]["events"].clone(), up["program"]["claims"]["omitted"].clone()), (json!([]), Value::Null));

    let cr = get(&w.n, &format!("/api/tx/{}", w.create.id())).await;
    let facts = &cr["program"]["facts"];
    assert_eq!((cr["program"]["kind"].as_str(), facts["created"].as_str(), facts["upload"].clone()), (Some("create"), Some(w.program.as_str()), Value::Null));
    assert_eq!((drops(&facts["storage"]["bonded"]), facts["balance_changes"].clone()), (w.record_deposit, json!([])), "a create's record deposit is bonded, and debited outside the VM");
    let total = w.record_deposit + w.creation_fee;
    assert_eq!(facts["outside_vm"], json!({ "record_deposit": w.record_deposit.to_string(), "upload_deposit": "0", "account_creation_fee": w.creation_fee.to_string(), "total": total.to_string() }));
    let fee = fee_of(&cr);
    let (before, after) = w.alice[1];
    assert_eq!(before - after, fee + total, "the fee, the record deposit and the creation fee of the program's account");
    // No init: nothing ran.
    assert_eq!((facts["fuel"]["limit"].as_u64(), facts["fuel"]["used"].as_u64()), (Some(0), Some(0)));
}

/// A call batch: the facts (balance changes net and exact, storage, fuel, the fee parts) and the
/// claims (return data and events), where every program-chosen name is `untrusted_*`: no `method` or
/// `name` key in the claims at all. The sender's balance is down by exactly the fee and the FANG it
/// sent.
#[tokio::test]
async fn a_call_reports_facts_apart_from_claims() {
    let w = chainwork("txjson-call");
    let tx = get(&w.n, &format!("/api/tx/{}", w.batch.id())).await;
    let p = &tx["program"];
    assert_eq!((tx["status"].as_str(), p["kind"].as_str(), &p["outcome"]), (Some("finalized"), Some("call"), &json!({ "ok": true })));
    assert_eq!(keys_of(p), BTreeSet::from(["claims", "facts", "kind", "outcome", "version"]));
    assert_eq!(keys_of(&p["facts"]), BTreeSet::from(["balance_changes", "burned_by_programs", "created", "fee", "fuel", "outside_vm", "storage", "upload"]));
    assert_eq!(p["facts"]["burned_by_programs"], json!("0"), "a call that opens no account burns nothing");
    assert_eq!(keys_of(&p["claims"]), BTreeSet::from(["events", "omitted", "return_data"]));
    // Claims: each call's return data and the event, under `untrusted_` names.
    let returns = p["claims"]["return_data"].as_array().unwrap();
    assert_eq!(returns.len(), 3);
    let alice = keys(ALICE).address_with("boo");
    for (r, (method, value, data)) in returns.iter().zip([("ret", 0, hex::encode(probe_bytes())), ("emit", 0, String::new()), ("fund", 5 * FANG, String::new())]) {
        assert_eq!(keys_of(r), BTreeSet::from(["data", "program", "untrusted_method", "value"]), "no `method` key: {r}");
        assert_eq!((r["program"].as_str(), r["untrusted_method"].as_str(), r["value"].as_str(), r["data"].as_str()), (Some(w.program.as_str()), Some(method), Some(value.to_string().as_str()), Some(data.as_str())));
    }
    let events = p["claims"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(keys_of(&events[0]), BTreeSet::from(["data", "program", "untrusted_name"]), "no `name` key: {}", events[0]);
    assert_eq!((events[0]["program"].as_str(), events[0]["untrusted_name"].as_str(), events[0]["data"].as_str()), (Some(w.program.as_str()), Some("e"), Some(hex::encode(probe_bytes()).as_str())));
    // Facts: 5 FANG from Alice to the program, in the order of the accounts' bytes, in drops.
    let changes = p["facts"]["balance_changes"].as_array().unwrap();
    let net: i128 = changes.iter().map(|c| c["change"].as_str().unwrap().parse::<i128>().unwrap()).sum();
    assert_eq!(net, 0, "the VM's diff conserves FANG: {changes:?}");
    let change_of = |a: &str| changes.iter().find(|c| c["account"] == a).map(|c| c["change"].as_str().unwrap().to_string());
    assert_eq!((change_of(&alice), change_of(&w.program)), (Some(format!("-{}", 5 * FANG)), Some((5 * FANG).to_string())));
    let order: Vec<[u8; 32]> = changes.iter().map(|c| crypto::address_bytes(c["account"].as_str().unwrap()).unwrap()).collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "sorted by the accounts' 32 bytes");
    assert_eq!(p["facts"]["storage"], json!({ "bytes_delta": 0, "bonded": "0", "refunded": "0" }));
    assert_eq!(p["facts"]["outside_vm"], json!({ "record_deposit": "0", "upload_deposit": "0", "account_creation_fee": "0", "total": "0" }), "a call debits nothing outside the VM");
    let fuel = &p["facts"]["fuel"];
    let (limit, load, reserved, rate) = (fuel["limit"].as_u64().unwrap(), fuel["load"].as_u64().unwrap(), fuel["reserved"].as_u64().unwrap(), fuel["rate"].as_u64().unwrap());
    assert_eq!((limit, reserved, load > 0), (FUEL, limit + load, true));
    assert!(fuel["used"].as_u64().unwrap() > 0 && fuel["used"].as_u64().unwrap() < limit);
    let fee = fee_of(&tx);
    assert_eq!(drops(&p["facts"]["fee"]["fuel"]), rate * reserved / 1000, "the fuel fee: the reservation at the rate it was charged");
    assert!(drops(&p["facts"]["fee"]["fuel"]) > 0 && drops(&p["facts"]["fee"]["tip"]) == 0);
    let (before, after) = w.alice[2];
    assert_eq!(before - after, fee + 5 * FANG, "balance down by the fee and the FANG it sent");
    // The fee the node shows is not just bytes: it includes the fuel part.
    assert!(fee > drops(&p["facts"]["fee"]["bytes"]));
}

/// A call that aborts: charged and included, the outcome says how in numbers and the program's own
/// sanitized message (`untrusted_message`), no claims, no balance changes; the top-level `fee` is
/// what the receipt says and what Alice lost.
#[tokio::test]
async fn a_failed_call_reports_its_outcome_and_what_it_cost() {
    let w = chainwork("txjson-failed");
    let tx = get(&w.n, &format!("/api/tx/{}", w.failing.id())).await;
    assert_eq!((tx["status"].as_str(), tx["error"].as_str().map(|e| e.contains("aborted with code 7"))), (Some("failed"), Some(true)), "{tx}");
    let p = &tx["program"];
    assert_eq!(p["outcome"], json!({ "ok": false, "number": 2, "code": "aborted", "program": w.program, "abort_code": 7, "untrusted_message": "e" }));
    assert_eq!((p["facts"]["balance_changes"].clone(), p["claims"]["events"].clone()), (json!([]), json!([])));
    assert_eq!(p["claims"]["return_data"][0]["data"], "", "return data is empty unless the call applied");
    let fee = fee_of(&tx);
    let (before, after) = w.alice[3];
    assert_eq!(before - after, fee, "only the fee is charged");
}

/// The claims budget: the block's ninth transaction of 32 KiB of return data meets the 256 KiB
/// budget spent, and says so (`omitted`), with every fact kept; the first eight record theirs.
#[tokio::test]
async fn claims_past_the_blocks_budget_are_reported_omitted() {
    let w = chainwork("txjson-omitted");
    let txs: Vec<Value> = crowd_txs(&w).await;
    for (i, tx) in txs.iter().enumerate() {
        let p = &tx["program"];
        let returns: Vec<usize> = p["claims"]["return_data"].as_array().unwrap().iter().map(|r| r["data"].as_str().unwrap().len() / 2).collect();
        if i < 8 {
            assert_eq!((returns, p["claims"]["omitted"].clone()), (vec![4096; 8], Value::Null), "tx {i}");
        } else {
            assert_eq!((returns, &p["claims"]["omitted"]), (vec![0; 8], &json!({ "events": 0, "event_bytes": 0, "return_bytes": 8 * 4096 })), "tx {i}");
            assert_eq!(p["outcome"], json!({ "ok": true }), "its facts are kept");
            assert!(p["facts"]["fuel"]["used"].as_u64().unwrap() > 0);
            fee_of(tx);
        }
    }
}

async fn crowd_txs(w: &Chainwork) -> Vec<Value> {
    let mut out = vec![];
    for t in &w.crowd {
        out.push(get(&w.n, &format!("/api/tx/{}", t.id())).await);
    }
    out
}

/// The same transaction through the other routes that show one: its block's `txs` and its sender's
/// recent transactions carry the same `program` and the same `fee`.
#[tokio::test]
async fn the_block_and_the_account_show_the_same_receipt() {
    let w = chainwork("txjson-routes");
    let tx = get(&w.n, &format!("/api/tx/{}", w.batch.id())).await;
    let height = tx["height"].as_u64().unwrap();
    let block = get(&w.n, &format!("/api/block/{height}")).await;
    let in_block = block["txs"].as_array().unwrap().iter().find(|t| t["id"] == tx["id"]).expect("in its block");
    assert_eq!((in_block["program"].clone(), in_block["fee"].clone(), in_block["fee_display"].clone()), (tx["program"].clone(), tx["fee"].clone(), tx["fee_display"].clone()));
    let account = get(&w.n, &format!("/api/account/{}", keys(ALICE).address_with("boo"))).await;
    let recent = account["recent_txs"].as_array().or_else(|| account["txs"].as_array()).expect("an account's recent transactions");
    let listed = recent.iter().find(|t| t["id"] == tx["id"]).expect("in its sender's history");
    assert_eq!((listed["program"].clone(), listed["fee"].clone()), (tx["program"].clone(), tx["fee"].clone()));
}

/// A v1 transaction's JSON is what it always was: a transfer has no `program` key, and exactly the
/// keys it had before this section existed; its fee is its base fee and its tip.
#[tokio::test]
async fn a_v1_transactions_json_is_unchanged() {
    let w = chainwork("txjson-v1");
    let tx = get(&w.n, &format!("/api/tx/{}", w.transfer.id())).await;
    assert_eq!(
        keys_of(&tx),
        BTreeSet::from(["action", "amount", "amount_display", "error", "fee", "fee_display", "from", "height", "id", "kind", "max_fee", "memo", "nonce", "signature_scheme", "signer_key", "size_bytes", "status", "summary", "tip", "to", "to_name", "valid_until"])
    );
    let height = tx["height"].as_u64().unwrap();
    let v = w.n.live.view();
    assert_eq!(drops(&tx["fee"]), v.base_fee(height).unwrap().fee(w.transfer.size()) + w.transfer.tip);
    // The block's listing of it too.
    let block = get(&w.n, &format!("/api/block/{height}")).await;
    assert!(block["txs"][0].get("program").is_none());
}

/// A program transaction that no block ran yet carries `"program": null` and is priced at the next
/// block's charge, fuel included; one that is still pending has no receipt to show.
#[tokio::test]
async fn a_pending_program_transaction_has_a_null_section_and_its_full_price() {
    let w = chainwork("txjson-pending");
    let alice = keys(ALICE);
    let (tx, charge) = {
        let mut c = w.n.chain.lock();
        let from = alice.address_with(c.state.params.prefix());
        let record = &c.state.programs.records[&w.program];
        let version = hex::encode(digest::version_digest(&w.program, &record.wasm_hash, &record.interface_hash, record.version));
        let call = crate::types::ProgramCall { program: w.program.clone(), version_digest: version, method: "ret".into(), input: HexBytes(vec![]), value: 0 };
        let mut tx = crate::types::Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), nonce: c.state.account(&from).nonce, from, fee: 0, tip: 0, valid_until: c.height() + 100, action: calls(vec![call], vec![]), memo: String::new(), public_key: hex::encode(alice.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        tx.fee = crate::econ::max_fee(&c.state, tx.size()) + crate::programs::probe::FUEL_MARGIN;
        let tx = tx.sign(&alice);
        c.submit(tx.clone()).unwrap();
        let charge = crate::econ::charge(&c.state, &tx, crate::econ::now(&c.state)).unwrap();
        (tx, charge)
    };
    let j = get(&w.n, &format!("/api/tx/{}", tx.id())).await;
    assert_eq!((j["status"].as_str(), j["program"].clone()), (Some("pending"), Value::Null), "{j}");
    assert!(j.as_object().unwrap().contains_key("program"), "a program tx always has the key");
    assert_eq!(drops(&j["fee"]), charge);
    let c = w.n.chain.lock();
    assert!(charge > crate::econ::base_fee(&c.state, crate::econ::now(&c.state), tx.size()), "the fuel is in the price");
}

/// What a client can check from `/api/tx` alone (§8.1): the signer's balance is down by exactly
/// `fee + outside_vm.total`, less what the VM's diff credits it (`balance_changes`, the signer's own
/// write bonds among it): Δbalance(signer) = −(fee + outside_vm.total) + balance_changes[signer].
/// Held for an upload, a create that opens its program's account, a create whose initializer writes
/// (the token's `init` bonds entries for its creator: counting `storage.bonded` as well would count
/// them twice), a create at an address FANG was sent to ahead (adopted: no creation fee), a call that
/// sends FANG, a call that writes, and a call that fails.
#[tokio::test]
async fn a_signers_balance_reconciles_from_the_receipt_alone() {
    use crate::programs::fixtures::{COUNTER, TOKEN};
    use crate::types::{InitCall, Tx};
    fn step(d: &mut Dev, spans: &mut Vec<(&'static str, Tx, u64, u64)>, what: &'static str, tx: Tx) {
        let before = d.balance(ALICE);
        let b = d.block(vec![tx.clone()]);
        assert_eq!(b.failed.len(), usize::from(what == "a failing call"), "{what}: {:?}", b.failed);
        spans.push((what, tx, before, d.balance(ALICE)));
    }
    let mut d = Dev::new("txjson-reconcile");
    let alice = keys(ALICE);
    let (iface, prefix) = (probe_interface(), d.prefix());
    let made = |t: &Tx| digest::program_address(&prefix, &crypto::address_bytes(&t.from).unwrap(), t.nonce);
    let mut spans = vec![];

    let tx = d.tx(&alice, upload(PROBE_WASM, &iface));
    step(&mut d, &mut spans, "an upload", tx);
    let tx = d.tx(&alice, create(PROBE_WASM, &iface));
    let probe = made(&tx);
    step(&mut d, &mut spans, "a create", tx);
    // The token: its `init(owner, supply)` writes the owner's balance and the supply.
    let tx = d.tx(&alice, upload(TOKEN.0, TOKEN.1));
    step(&mut d, &mut spans, "an upload of another code", tx);
    let mut token = create(TOKEN.0, TOKEN.1);
    if let Action::CreateProgram { init, fuel_limit, .. } = &mut token {
        let input = [crypto::address_bytes(&d.address(ALICE)).unwrap().to_vec(), 1_000u64.to_le_bytes().to_vec()].concat();
        (*init, *fuel_limit) = (Some(InitCall { method: "init".into(), input: HexBytes(input), value: 0 }), FUEL);
    }
    let tx = d.tx(&alice, token);
    step(&mut d, &mut spans, "a create whose initializer writes", tx);
    // FANG sent ahead to the address the next create makes: it adopts the account.
    let nonce = d.c.state.account(&d.address(ALICE)).nonce;
    let adopted = digest::program_address(&prefix, &crypto::address_bytes(&d.address(ALICE)).unwrap(), nonce + 1);
    let pair = d.txs_at(&alice, Some(nonce), 0, vec![Action::Transfer { to: adopted, amount: FANG }, create(PROBE_WASM, &iface)]);
    d.block(vec![pair[0].clone()]);
    step(&mut d, &mut spans, "a create at an adopted address", pair[1].clone());
    // The counter, and calls: one that writes, one that sends FANG, one that fails.
    let tx = d.tx(&alice, upload(COUNTER.0, COUNTER.1));
    d.block(vec![tx]);
    let tx = d.tx(&alice, create(COUNTER.0, COUNTER.1));
    let counter = made(&tx);
    d.block(vec![tx]);
    let tx = d.tx(&alice, calls(vec![d.call(&counter, "increment", vec![], 0)], vec![]));
    step(&mut d, &mut spans, "a call that writes", tx);
    let tx = d.tx(&alice, calls(vec![d.call(&probe, "fund", vec![], 5 * FANG)], vec![]));
    step(&mut d, &mut spans, "a call that sends FANG", tx);
    let tx = d.tx(&alice, calls(vec![d.call(&probe, "no", vec![], 0)], vec![]));
    step(&mut d, &mut spans, "a failing call", tx);
    let creation_fee = d.c.state.chain_params.account_creation_fee;

    let n = Node::new(Arc::new(Mutex::new(d.take())), None, vec![], ClientIpPolicy::default());
    let alice_address = keys(ALICE).address_with("boo");
    let mut outsides = BTreeMap::new();
    for (what, tx, before, after) in &spans {
        let j = get(&n, &format!("/api/tx/{}", tx.id())).await;
        let facts = &j["program"]["facts"];
        let fee = fee_of(&j);
        let parts = &facts["outside_vm"];
        let outside = drops(&parts["total"]);
        assert_eq!(outside, ["record_deposit", "upload_deposit", "account_creation_fee"].iter().map(|k| drops(&parts[k])).sum::<u64>(), "{what}: the total is the parts' sum");
        let credited: i128 = facts["balance_changes"].as_array().unwrap().iter().filter(|c| c["account"] == alice_address.as_str()).map(|c| c["change"].as_str().unwrap().parse::<i128>().unwrap()).sum();
        assert_eq!(i128::from(*after) - i128::from(*before), -i128::from(fee + outside) + credited, "{what}: Δbalance = −(fee + outside_vm.total) + balance_changes[signer]");
        // The facts conserve FANG, burned_by_programs included: Σ balance_changes + (bonded − record − upload − refunded) + burned = 0.
        let moved: i128 = facts["balance_changes"].as_array().unwrap().iter().map(|c| c["change"].as_str().unwrap().parse::<i128>().unwrap()).sum();
        let held = i128::from(drops(&facts["storage"]["bonded"])) - i128::from(drops(&parts["record_deposit"])) - i128::from(drops(&parts["upload_deposit"])) - i128::from(drops(&facts["storage"]["refunded"]));
        assert_eq!(moved + held + i128::from(drops(&facts["burned_by_programs"])), 0, "{what}: the receipt's facts conserve FANG");
        outsides.insert(*what, (drops(&parts["record_deposit"]), drops(&parts["upload_deposit"]), drops(&parts["account_creation_fee"]), credited, drops(&facts["storage"]["bonded"])));
    }
    // Who paid what, as the receipts say it: (record deposit, upload deposit, creation fee, the VM's
    // credit to the signer, everything bonded).
    let record = outsides["a create"].0;
    assert!(record > 0 && outsides["an upload"].1 > 0 && outsides["an upload of another code"].1 > 0);
    assert_eq!(outsides["an upload"], (0, outsides["an upload"].1, 0, 0, outsides["an upload"].1));
    assert_eq!(outsides["a create"], (record, 0, creation_fee, 0, record));
    assert_eq!(outsides["a create at an adopted address"], (record, 0, 0, 0, record), "an adopted address pays no creation fee");
    let (r, u, f, credited, bonded) = outsides["a create whose initializer writes"];
    assert_eq!((u, f), (0, creation_fee));
    assert!(credited < 0 && bonded == r + credited.unsigned_abs() as u64, "the initializer's write bonds are in `balance_changes` and in `storage.bonded`: {r} {credited} {bonded}");
    let (r, u, f, credited, bonded) = outsides["a call that writes"];
    assert_eq!((r, u, f), (0, 0, 0));
    assert!(credited < 0 && bonded == credited.unsigned_abs() as u64, "{credited} {bonded}");
    assert_eq!(outsides["a call that sends FANG"].3, -i128::from(5 * FANG));
    assert_eq!(outsides["a failing call"], (0, 0, 0, 0, 0));
}
