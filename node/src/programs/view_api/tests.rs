//! `POST /api/program/{address}/view` (WBS S6b's acceptance), through the router on a node over a
//! dev chain born at v2 with programs on (`probe::Dev`), called with `oneshot` as a client at
//! `peer` (loopback has no per-IP budget). Two programs: the counter guest (a `get` view, an
//! `increment` call) and the probe with a views interface (`ret`, `no`, `spin` and `go` are views;
//! `emit` and `fund` stay calls), made on chain by real transactions.

use super::*;
use crate::api::{router, ClientIpPolicy};
use crate::programs::fixtures::{self as fx, COUNTER};
use crate::programs::probe::{calls, create, keys, upload, Dev, ALICE, BOB, PROBE_WASM};
use crate::programs::view::inject::{self, Injected};
use crate::types::{Action, ProgramCall, Tx};
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::Request;
use blockula_vm::interface::{Interface, Method};
use parking_lot::Mutex;
use std::sync::Arc;
use tower::ServiceExt as _;

const LOOPBACK: &str = "127.0.0.1:1";
const STRANGER: &str = "203.0.113.9:4000";

/// The probe's interface with its read methods as views: `go` (calls `spin` on the program whose
/// address is its input), `no` (aborts with code 7), `ret` (returns 4 KiB) and `spin` (never
/// returns); `emit` and `fund` stay calls, `fund` payable.
fn views_interface() -> Vec<u8> {
    let methods = ["emit", "fund", "go", "no", "ret", "spin"].map(|name| {
        let kind = if matches!(name, "emit" | "fund") { MethodKind::Call } else { MethodKind::View };
        Method { name: name.into(), kind, payable: name == "fund", args: vec![], returns: None }
    });
    blockula_vm::interface::encode(&Interface { methods: methods.into() }).unwrap()
}

/// Alice uploads `wasm` with `interface`, then creates it `n` times: the programs' addresses.
fn deploy(d: &mut Dev, wasm: &[u8], interface: &[u8], n: usize) -> Vec<String> {
    let alice = keys(ALICE);
    let up = d.tx(&alice, upload(wasm, interface));
    d.block(vec![up]);
    let creates = d.txs(&alice, vec![create(wasm, interface); n]);
    let programs: Vec<String> = creates.iter().map(|t| digest::program_address(&d.prefix(), &crypto::address_bytes(&t.from).unwrap(), t.nonce)).collect();
    let b = d.block(creates);
    assert!(b.failed.is_empty(), "{:?}", b.failed);
    programs
}

/// A dev chain with a counter and two probes deployed, and a node over it that serves views (a
/// validator's node doesn't by default, `an_off_validator_refuses`).
struct Harness {
    d: Dev,
    n: Node,
    counter: String,
    probes: Vec<String>,
}

/// How many harnesses this process has made.
static HARNESSES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Harness {
    /// A harness whose programs have addresses of its own: Alice, who makes them, first spends as many
    /// nonces as harnesses were made before it. The tests run as threads of one process, and what a
    /// test arms for a program (`view::inject`: a fault for a run `view <address> <method>`) and the
    /// views it counts are the process's: with the same addresses in every test, one test's armed
    /// fault would meet another's views of the same program.
    fn new(tag: &str) -> Harness {
        let mut d = Dev::new(tag);
        let skip = HARNESSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if skip > 0 {
            let spends = d.txs(&keys(ALICE), vec![Action::Transfer { to: d.address(BOB), amount: 1 }; skip]);
            d.block(spends);
        }
        let counter = deploy(&mut d, COUNTER.0, COUNTER.1, 1).remove(0);
        let probes = deploy(&mut d, PROBE_WASM, &views_interface(), 2);
        let chain = Arc::new(Mutex::new(d.take()));
        let n = Node::new(chain, None, vec![], ClientIpPolicy::default()).with_program_views(true, None);
        Harness { d, n, counter, probes }
    }

    /// Alice calls the counter's `increment` `times` times, in one block of this node's chain.
    fn increment(&self, times: usize) {
        let mut c = self.n.chain.lock();
        let alice = keys(ALICE);
        let from = alice.address_with(c.state.params.prefix());
        let record = &c.state.programs.records[&self.counter];
        let version = hex::encode(digest::version_digest(&self.counter, &record.wasm_hash, &record.interface_hash, record.version));
        let nonce = c.state.account(&from).nonce;
        let txs: Vec<Tx> = (0..times as u64)
            .map(|i| {
                let call = ProgramCall { program: self.counter.clone(), version_digest: version.clone(), method: "increment".into(), input: HexBytes(vec![]), value: 0 };
                let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), nonce: nonce + i, from: from.clone(), fee: 0, tip: 0, valid_until: c.height() + 100, action: calls(vec![call], vec![]), memo: String::new(), public_key: hex::encode(alice.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
                tx.fee = crate::econ::max_fee(&c.state, tx.size()) + crate::programs::probe::FUEL_MARGIN;
                tx.sign(&alice)
            })
            .collect();
        let mut f = c.test_block(txs);
        f.timestamp_ms = 1_790_000_000_000 + f.height * 1_000;
        c.execute(f).unwrap();
    }

    fn height(&self) -> u64 {
        self.n.live.view().height
    }
}

/// POSTs `body` to the view route of `address` as a client at `peer`.
async fn post(n: &Node, peer: &str, address: &str, body: &str) -> (StatusCode, axum::http::HeaderMap, Value) {
    let app = router(n.clone()).layer(MockConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    let req = Request::builder().method("POST").uri(format!("/api/program/{address}/view")).header("content-type", "application/json").body(Body::from(body.to_string())).unwrap();
    let res = app.oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = axum::body::to_bytes(res.into_body(), 8 << 20).await.unwrap();
    (status, headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn view(n: &Node, address: &str, body: Value) -> (StatusCode, Value) {
    let (s, _, j) = post(n, LOOPBACK, address, &body.to_string()).await;
    (s, j)
}

fn u64_of(j: &Value) -> u64 {
    let data = j["return_data"].as_str().unwrap_or_else(|| panic!("the view returned no data: {j}"));
    u64::from_le_bytes(hex::decode(data).unwrap().try_into().unwrap())
}

/// `/view` runs a guest's `View` method: the counter's `get` reads what its `increment` calls
/// wrote, at the executed state, and the answer is `{ ok, outcome, return_data, fuel_used,
/// height }`. Nothing changes: the state root, the height and the mempool are what they were.
#[tokio::test]
async fn a_view_runs_a_view_method_and_changes_nothing() {
    let h = Harness::new("view-run");
    let (s, j) = view(&h.n, &h.counter, json!({ "method": "get" })).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!((j["ok"].as_bool(), &j["outcome"], u64_of(&j), j["height"].as_u64()), (Some(true), &json!({ "ok": true }), 0, Some(h.height())));
    assert!(j["fuel_used"].as_u64().unwrap() > 0, "{j}");
    assert_eq!(j.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(), ["fuel_used", "height", "ok", "outcome", "return_data"]);
    h.increment(3);
    let before = (h.n.chain.lock().state.full_root(), h.height(), h.n.live.mempool().len());
    let (s, j) = view(&h.n, &h.counter, json!({ "method": "get", "input": "", "bindings": [], "fuel": 1_000_000 })).await;
    assert_eq!((s, u64_of(&j), j["height"].as_u64()), (StatusCode::OK, 3, Some(h.height())), "{j}");
    // Twice more: a view leaves nothing behind.
    for _ in 0..2 {
        let (_, j) = view(&h.n, &h.counter, json!({ "method": "get" })).await;
        assert_eq!(u64_of(&j), 3);
    }
    assert_eq!((h.n.chain.lock().state.full_root(), h.height(), h.n.live.mempool().len()), before);
    assert_eq!(h.n.views.slots.available_permits(), MAX_CONCURRENT_VIEWS, "every run gave its slot back");
}

/// A method that isn't a `View` is refused before anything runs (a `Call` method: 400; the
/// initializer likewise): a fault armed for the call would answer 500 if the VM were reached.
#[tokio::test]
async fn a_call_method_is_refused_and_no_vm_runs() {
    let h = Harness::new("view-call");
    for (p, method) in [(&h.counter, "increment"), (&h.probes[0], "emit"), (&h.probes[0], "fund")] {
        inject::arm(&label(p, method), Injected::Panic);
        let (s, j) = view(&h.n, p, json!({ "method": method })).await;
        inject::disarm(&label(p, method));
        assert_eq!(s, StatusCode::BAD_REQUEST, "{method}: {j}");
        let e = j["error"].as_str().unwrap();
        assert!(e.contains("is a call, which only a transaction runs"), "{method}: {e}");
        assert!(!e.contains(method), "a refusal doesn't quote the method");
    }
}

/// The fuel cap holds: a view that never returns runs out of `min(fuel, cap)` and says so, whatever
/// the request asks (a huge `fuel` is clamped to the node's `--view-max-fuel`, and by default to
/// `program_max_tx_fuel`); a smaller `fuel` is honoured.
#[tokio::test]
async fn the_fuel_cap_holds() {
    let h = Harness::new("view-fuel");
    let spin = &h.probes[0];
    let out_of_fuel = |j: &Value| (j["ok"].as_bool(), j["outcome"]["code"].as_str().map(String::from), j["outcome"]["trap"]["name"].as_str().map(String::from), j["return_data"].is_null());
    let expected = (Some(false), Some("trapped".to_string()), Some("out_of_fuel".to_string()), true);
    // The default cap is the code's own bound on a transaction, read from the state.
    let max_tx = param(&h.n.chain.lock().state, Param::MaxTxFuel);
    let (s, j) = view(&h.n, spin, json!({ "method": "spin" })).await;
    assert_eq!((s, out_of_fuel(&j), j["fuel_used"].as_u64()), (StatusCode::OK, expected.clone(), Some(max_tx)), "{j}");
    let (_, j) = view(&h.n, spin, json!({ "method": "spin", "fuel": u64::MAX >> 12 })).await;
    assert_eq!((out_of_fuel(&j), j["fuel_used"].as_u64()), (expected.clone(), Some(max_tx)), "a huge request is clamped to the cap: {j}");
    let (_, j) = view(&h.n, spin, json!({ "method": "spin", "fuel": 123_456 })).await;
    assert_eq!((out_of_fuel(&j), j["fuel_used"].as_u64()), (expected.clone(), Some(123_456)), "a smaller request is honoured: {j}");
    // `--view-max-fuel`: a lower cap for this node.
    let capped = Node::new(h.n.chain.clone(), None, vec![], ClientIpPolicy::default()).with_program_views(true, Some(77_777));
    for fuel in [None, Some(1_000_000_000u64), Some(77_777)] {
        let body = fuel.map_or(json!({ "method": "spin" }), |f| json!({ "method": "spin", "fuel": f }));
        let (_, j) = view(&capped, spin, body).await;
        assert_eq!((out_of_fuel(&j), j["fuel_used"].as_u64()), (expected.clone(), Some(77_777)), "{fuel:?}: {j}");
    }
    // `fuel: 0` can't run anything.
    let (s, j) = view(&h.n, spin, json!({ "method": "spin", "fuel": 0 })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{j}");
    // A view that does its job in its fuel isn't charged more than it used.
    let (_, j) = view(&capped, &h.counter, json!({ "method": "get", "fuel": 77_777 })).await;
    assert!(j["ok"] == true && j["fuel_used"].as_u64().unwrap() < 77_777, "{j}");
    // The chain's parameter bounds the flag: governance lowers `program_max_tx_fuel` to 1,500,000 (its
    // lowest is 1,000,000), and the very next view of a node with no flag, or a higher one, is held
    // to it; a lower flag still is the cap.
    {
        let mut c = h.n.chain.lock();
        c.state.programs.meta.as_mut().unwrap().params.insert(Param::MaxTxFuel.key().into(), 1_500_000);
        c.produce_block_with(vec![]).unwrap();
    }
    let flagged = |flag: Option<u64>| Node::new(h.n.chain.clone(), None, vec![], ClientIpPolicy::default()).with_program_views(true, flag);
    for (what, node, request, cap) in [("no flag", flagged(None), None, 1_500_000), ("a higher flag", flagged(Some(2_000_000)), None, 1_500_000), ("a higher flag and a bigger request", flagged(Some(2_000_000)), Some(1_000_000_000u64), 1_500_000), ("a lower flag", flagged(Some(1_200_000)), None, 1_200_000)] {
        let body = request.map_or(json!({ "method": "spin" }), |f| json!({ "method": "spin", "fuel": f }));
        let (_, j) = view(&node, spin, body).await;
        assert_eq!((out_of_fuel(&j), j["fuel_used"].as_u64()), (expected.clone(), Some(cap)), "{what}: {j}");
    }
}

/// A program's own failure is a result, not an error: status 200, `ok: false`, the receipt's outcome
/// numbers and the program's sanitized abort message, no return data.
#[tokio::test]
async fn a_programs_failure_is_a_result() {
    let h = Harness::new("view-fail");
    let (s, j) = view(&h.n, &h.probes[0], json!({ "method": "no" })).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["ok"], false);
    assert!(j["return_data"].is_null());
    assert_eq!(j["outcome"], json!({ "ok": false, "number": 2, "code": "aborted", "program": h.probes[0], "abort_code": 7, "untrusted_message": "e" }));
    // `ret` returns 4 KiB: hex, the program's claim.
    let (_, j) = view(&h.n, &h.probes[0], json!({ "method": "ret" })).await;
    assert_eq!((j["ok"].as_bool(), j["return_data"].as_str().map(str::len)), (Some(true), Some(8192)));
    assert_eq!(&j["return_data"].as_str().unwrap()[128..136], hex::encode(b"spin"), "the probe's memory");
}

/// Bindings: the programs a view may reach. `go` calls `spin` on the program in its input: unbound,
/// it traps; bound (with or without a version digest), `spin` runs in its share and runs out.
#[tokio::test]
async fn bindings_let_a_view_reach_other_programs() {
    let h = Harness::new("view-bind");
    let (a, b) = (&h.probes[0], &h.probes[1]);
    let input = hex::encode(crypto::address_bytes(b).unwrap());
    let trap = |j: &Value| j["outcome"]["trap"]["name"].as_str().map(String::from);
    let (s, j) = view(&h.n, a, json!({ "method": "go", "input": input })).await;
    assert_eq!((s, trap(&j)), (StatusCode::OK, Some("unbound_program".to_string())), "{j}");
    let in_force = hex::encode(digest::version_digest(b, &h.n.live.view().state.programs.records[b].wasm_hash, &h.n.live.view().state.programs.records[b].interface_hash, 1));
    for binding in [json!({ "program": b }), json!({ "program": b, "version_digest": in_force }), json!({ "program": b, "version_digest": "00".repeat(32) })] {
        let (s, j) = view(&h.n, a, json!({ "method": "go", "input": input, "bindings": [binding] })).await;
        assert_eq!((s, trap(&j)), (StatusCode::OK, Some("out_of_fuel".to_string())), "{binding}: {j}");
    }
}

/// What a view's request can get wrong, each its own refusal and none a run.
#[tokio::test]
async fn a_bad_request_is_refused_in_words() {
    let h = Harness::new("view-bad");
    let (a, b) = (h.probes[0].clone(), h.probes[1].clone());
    let name = format!("{}.boo", "alice");
    let cases: Vec<(&str, String, Value, StatusCode, &str)> = vec![
        ("a name for the program", name, json!({ "method": "get" }), StatusCode::BAD_REQUEST, "named by its address"),
        ("no such program", keys(BOB).address_with("boo"), json!({ "method": "get" }), StatusCode::NOT_FOUND, "program not found"),
        ("a method that isn't an identifier", a.clone(), json!({ "method": "Get!" }), StatusCode::BAD_REQUEST, "`method` is a method's name"),
        ("a method the interface lacks", a.clone(), json!({ "method": "nothing" }), StatusCode::BAD_REQUEST, "has no method by that name"),
        ("input that isn't hex", a.clone(), json!({ "method": "ret", "input": "zz" }), StatusCode::UNPROCESSABLE_ENTITY, "bad view request JSON"),
        ("uppercase hex", a.clone(), json!({ "method": "ret", "input": "AB" }), StatusCode::UNPROCESSABLE_ENTITY, "lowercase hex"),
        ("an unknown field", a.clone(), json!({ "method": "ret", "value": "5" }), StatusCode::UNPROCESSABLE_ENTITY, "unknown field"),
        ("no method", a.clone(), json!({}), StatusCode::UNPROCESSABLE_ENTITY, "missing field"),
        ("input past MAX_INPUT", a.clone(), json!({ "method": "ret", "input": "00".repeat(MAX_INPUT + 1) }), StatusCode::BAD_REQUEST, "past the 32768"),
        // `chain::short` once sliced these at byte 10 and 6 from the end, inside the `é`: a panic, remotely.
        ("a binding that isn't an address (multi-byte text)", a.clone(), json!({ "method": "ret", "bindings": [{ "program": format!("aaaaaaaaaé{}", "b".repeat(10)) }] }), StatusCode::BAD_REQUEST, "a binding is a program's address"),
        ("a binding that isn't an address (a name)", a.clone(), json!({ "method": "ret", "bindings": [{ "program": "alice.boo" }] }), StatusCode::BAD_REQUEST, "a binding is a program's address"),
        ("a binding that isn't a program", a.clone(), json!({ "method": "ret", "bindings": [{ "program": keys(BOB).address_with("boo") }] }), StatusCode::BAD_REQUEST, "isn't a program here"),
        ("a binding that is the program", a.clone(), json!({ "method": "ret", "bindings": [{ "program": a }] }), StatusCode::BAD_REQUEST, "bound twice, or is the program being called"),
        ("a binding twice", a.clone(), json!({ "method": "ret", "bindings": [{ "program": b }, { "program": b }] }), StatusCode::BAD_REQUEST, "bound twice"),
        ("a binding at another version", a.clone(), json!({ "method": "ret", "bindings": [{ "program": b, "version_digest": "11".repeat(32) }] }), StatusCode::BAD_REQUEST, "whose version in force is"),
        ("a binding digest that isn't hex64", a.clone(), json!({ "method": "ret", "bindings": [{ "program": b, "version_digest": "11" }] }), StatusCode::BAD_REQUEST, "64 lowercase hex"),
        ("too many bindings", a.clone(), json!({ "method": "ret", "bindings": vec![json!({ "program": b }); MAX_BINDINGS + 1] }), StatusCode::BAD_REQUEST, "past the 16"),
    ];
    for (what, address, body, status, phrase) in cases {
        let (s, j) = view(&h.n, &address, body).await;
        assert_eq!(s, status, "{what}: {j}");
        assert!(j["error"].as_str().unwrap_or_default().contains(phrase), "{what}: {j}");
    }
    // Not JSON at all.
    let (s, _, j) = post(&h.n, LOOPBACK, &a, "not json").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{j}");
    assert_eq!(h.n.views.slots.available_permits(), MAX_CONCURRENT_VIEWS, "a refusal never holds a slot");
}

/// An initializer is refused as a call is, and a view method of the same program runs: the token
/// guest's `init` (an `Init` method), `transfer` (a `Call`) and `balance_of` (a `View`), checked on a
/// state where the token is installed as a create would leave it.
#[test]
fn an_initializer_is_refused_as_a_call_is() {
    let mut st = fx::state_on();
    let blobs = fx::store(&mut st, fx::TOKEN.0, fx::TOKEN.1, 1);
    let token = fx::program_of(BOB, 0);
    fx::install(&mut st, &token, blobs, 2);
    let req = |method: &str| ViewRequest { method: method.into(), input: HexBytes(vec![]), bindings: vec![], fuel: None };
    let refused = |method: &str| check(&st, &token, req(method), 1_000_000).err().map(|r| (r.status, r.message));
    let (status, message) = refused("init").expect("an initializer is refused");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(message.contains("the program's initializer, which runs only when it's created"), "{message}");
    let (status, message) = refused("transfer").expect("a call method is refused");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(message.contains("is a call, which only a transaction runs"), "{message}");
    assert!(refused("balance_of").is_none() && refused("total_supply").is_none());
}

/// Declared code past what one transaction may load is refused, as admission refuses it: a program
/// and its bindings whose distinct code is past `MAX_TX_CODE_BYTES` (two programs of the largest
/// code are exactly the bound; a third passes it). A binding of the same code twice is declared
/// once.
#[test]
fn code_past_a_transactions_load_bound_is_refused() {
    let mut w = fx::world();
    let mut wasm = fx::largest_wasm();
    *wasm.last_mut().unwrap() = 9;
    let blobs = fx::store(&mut w.st, &wasm, fx::COUNTER.1, 1);
    let third = fx::program_of(BOB, 12);
    fx::install(&mut w.st, &third, blobs, 2);
    let (first, second) = (w.big[0].0.clone(), w.big[1].0.clone());
    assert_eq!(2 * blockula_vm::config::MAX_CODE_BYTES, MAX_TX_CODE_BYTES, "two of the largest are the bound");
    let req = |bindings: &[&str]| ViewRequest { method: "get".into(), input: HexBytes(vec![]), bindings: bindings.iter().map(|p| BindingRequest { program: p.to_string(), version_digest: None }).collect(), fuel: None };
    assert!(check(&w.st, &first, req(&[]), 1_000).is_ok());
    assert!(check(&w.st, &first, req(&[&second]), 1_000).is_ok(), "exactly the bound");
    let r = check(&w.st, &first, req(&[&second, &third]), 1_000).err().expect("past the bound");
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.message.contains(&format!("is {} bytes, past the {MAX_TX_CODE_BYTES} one run may load", 3 * blockula_vm::config::MAX_CODE_BYTES)), "{}", r.message);
}

/// Off on a validator unless asked: a node over a validator's chain refuses (403, with the way to
/// turn it on) before it runs or counts anything; a full node's gate is on without the flag, and
/// `with_program_views` turns one on and never off.
#[tokio::test]
async fn an_off_validator_refuses() {
    let h = Harness::new("view-off");
    let chain = h.n.chain.clone();
    let validator = Node::new(chain.clone(), None, vec![], ClientIpPolicy::default());
    assert!(!validator.views.enabled(), "a validator's node serves no views by default");
    let (s, j) = view(&validator, &h.counter, json!({ "method": "get" })).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{j}");
    assert!(j["error"].as_str().unwrap().contains("--program-views"), "{j}");
    // Whatever the request, it's the gate that answers (not a 422 for a bad body, nor a 404).
    let (s, _, j) = post(&validator, LOOPBACK, &keys(BOB).address_with("boo"), "not json").await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{j}");
    let on = Node::new(chain.clone(), None, vec![], ClientIpPolicy::default()).with_program_views(true, None);
    assert!(on.views.enabled());
    let (s, j) = view(&on, &h.counter, json!({ "method": "get" })).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert!(on.clone().with_program_views(false, None).views.enabled(), "the flag only turns views on");
    assert!(ViewGate::for_role(Role::FullNode).enabled() && !ViewGate::for_role(Role::Validator).enabled());
    // A node over a chain with no validator key is a full node: views on, with no flag.
    let home = Dev::fresh_home("view-full");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::copy(h.d.home.join("genesis.json"), home.join("genesis.json")).unwrap();
    let full = Node::new(Arc::new(Mutex::new(crate::chain::Chain::open(&home).unwrap())), None, vec![], ClientIpPolicy::default());
    assert!(full.views.enabled(), "a full node serves views by default");
    std::fs::remove_dir_all(home).ok();
}

/// Dormant: while programs have never been switched on, the route answers 404 before doing anything
/// else, on a v1 chain and on a chain born at v2 without programs, views on or off, whatever the body.
#[tokio::test]
async fn a_dormant_chain_answers_404_first() {
    for (tag, programs) in [("view-v1", None), ("view-v2", Some(false))] {
        let home = Dev::fresh_home(tag);
        let o = crate::chain::SummonOpts { address_prefix: "boo".into(), ..crate::chain::SummonOpts::new("view-dormant", 1) };
        crate::chain::Chain::summon(&home, &o).unwrap();
        if programs.is_some() {
            let path = home.join("genesis.json");
            let mut g: crate::chain::Genesis = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            g.upgrades = vec![crate::upgrade::ScheduledUpgrade { version: crate::upgrade::PROGRAMS_VERSION, activation_height: 1 }];
            std::fs::write(&path, serde_json::to_string_pretty(&g).unwrap()).unwrap();
        }
        let chain = Arc::new(Mutex::new(crate::chain::Chain::open(&home).unwrap()));
        chain.lock().produce_block_with(vec![]).unwrap();
        for on in [true, false] {
            let n = Node::new(chain.clone(), None, vec![], ClientIpPolicy::default()).with_program_views(on, None);
            let (s, _, j) = post(&n, STRANGER, &keys(BOB).address_with("boo"), "not json").await;
            assert_eq!(s, StatusCode::NOT_FOUND, "{tag}, views {on}: {j}");
            assert!(j["error"].as_str().unwrap().contains("no programs exist"), "{j}");
        }
        std::fs::remove_dir_all(home).ok();
    }
}

/// The per-IP budget: [`VIEWS_PER_IP_PER_MINUTE`] views a minute from one client, then 429 with a
/// `Retry-After`; another client has its own, and a loopback caller (no budget) isn't limited.
#[tokio::test]
async fn a_client_gets_a_budget_of_views_a_minute() {
    let h = Harness::new("view-rate");
    let body = json!({ "method": "get" }).to_string();
    for i in 0..VIEWS_PER_IP_PER_MINUTE {
        let (s, _, j) = post(&h.n, STRANGER, &h.counter, &body).await;
        assert_eq!(s, StatusCode::OK, "view {i}: {j}");
    }
    let (s, headers, j) = post(&h.n, STRANGER, &h.counter, &body).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{j}");
    let wait: u64 = headers.get(RETRY_AFTER).unwrap().to_str().unwrap().parse().unwrap();
    assert!((1..=60).contains(&wait), "{wait}");
    assert!(j["error"].as_str().unwrap().contains("too many program views from your IP"), "{j}");
    // Another IP, and this one's other budgets (reads, txs) are untouched.
    let (s, _, _) = post(&h.n, "203.0.113.77:1", &h.counter, &body).await;
    assert_eq!(s, StatusCode::OK);
    // Loopback has no budget.
    for _ in 0..VIEWS_PER_IP_PER_MINUTE + 5 {
        let (s, _, j) = post(&h.n, LOOPBACK, &h.counter, &body).await;
        assert_eq!(s, StatusCode::OK, "{j}");
    }
    assert!(count(Outcome::Limited) >= 1);
}

/// At most [`MAX_CONCURRENT_VIEWS`] run at once on a node: with every slot held a view waits
/// [`VIEW_SLOT_WAIT`] for one and is then 429 (with `Retry-After: 1`), after only the checks that need
/// no VM, and a request that was refused for anything else never takes one (or waits for one).
#[tokio::test]
async fn at_most_two_views_run_at_once() {
    let h = Harness::new("view-slots");
    let body = json!({ "method": "get" }).to_string();
    let held: Vec<_> = (0..MAX_CONCURRENT_VIEWS).map(|_| h.n.views.slots.clone().try_acquire_owned().unwrap()).collect();
    let started = std::time::Instant::now();
    let (s, headers, j) = post(&h.n, LOOPBACK, &h.counter, &body).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{j}");
    assert!(started.elapsed() >= VIEW_SLOT_WAIT.mul_f32(0.9), "it waited for a slot first: {:?}", started.elapsed());
    assert_eq!(headers.get(RETRY_AFTER).unwrap(), "1");
    assert!(j["error"].as_str().unwrap().contains("running 2 program views already"), "{j}");
    // A bad request is a 400 (not 429) while every slot is held, and at once: it asked for nothing
    // to run, so it doesn't wait for a slot (a node that would make it wait a minute answers it
    // in the time it takes to run to the check).
    let mut patient = h.n.clone();
    patient.views = ViewGate::with_slot_wait(true, None, Duration::from_secs(60));
    let _taken: Vec<_> = (0..MAX_CONCURRENT_VIEWS).map(|_| patient.views.slots.clone().try_acquire_owned().unwrap()).collect();
    let bad = json!({ "method": "increment" }).to_string();
    let (s, _, j) = tokio::time::timeout(Duration::from_secs(10), post(&patient, LOOPBACK, &h.counter, &bad)).await.expect("a request that asked for nothing to run doesn't wait for a slot");
    assert_eq!(s, StatusCode::BAD_REQUEST, "{j}");
    let (s, _, j) = post(&h.n, LOOPBACK, &h.counter, &bad).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{j}");
    drop(held);
    let (s, _, j) = post(&h.n, LOOPBACK, &h.counter, &body).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    // One slot held: one view still runs.
    let one = h.n.views.slots.clone().try_acquire_owned().unwrap();
    let (s, _, _) = post(&h.n, LOOPBACK, &h.counter, &body).await;
    assert_eq!(s, StatusCode::OK);
    drop(one);
}

/// A node fault in a view is a 500 and a count, never a halt: a panic inside the VM (a `Tx` fault)
/// and a thread the OS refuses three times (a `Setup` fault) both answer 500 with no engine text,
/// leave the state, the height and the halt signal alone, record nothing in the state, and the next
/// view runs as if nothing happened.
#[tokio::test]
async fn a_fault_in_a_view_is_a_500_and_never_a_halt() {
    let h = Harness::new("view-fault");
    let root = h.n.chain.lock().state.full_root();
    let faults = count(Outcome::Fault);
    for (what, fault) in [("a panic in the VM", Injected::Panic), ("a spawn the OS refuses", Injected::RefuseSpawns(3))] {
        let l = label(&h.counter, "get");
        inject::arm(&l, fault);
        let (s, j) = view(&h.n, &h.counter, json!({ "method": "get" })).await;
        inject::disarm(&l);
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{what}: {j}");
        let e = j["error"].as_str().unwrap();
        assert!(e.contains("a node fault, logged") && !e.contains("panic") && !e.contains("refused"), "{what}: no engine text in the answer: {e}");
        assert!(h.n.halt.get().is_none(), "{what}: no halt");
        {
            let c = h.n.chain.lock();
            assert_eq!((c.state.full_root(), c.state.programs.fault.is_none()), (root, true), "{what}: nothing recorded");
        }
        let (s, j) = view(&h.n, &h.counter, json!({ "method": "get" })).await;
        assert_eq!((s, j["ok"].as_bool()), (StatusCode::OK, Some(true)), "{what}: the next view runs: {j}");
    }
    assert_eq!(count(Outcome::Fault) - faults, 2);
    assert_eq!(h.n.views.slots.available_permits(), MAX_CONCURRENT_VIEWS, "a fault gives its slot back");
}

/// `/metrics` carries `blockula_program_views_total{outcome}` once programs exist, and nothing of it
/// on a dormant chain.
#[tokio::test]
async fn metrics_count_views_by_outcome() {
    let h = Harness::new("view-metrics");
    let _ = view(&h.n, &h.counter, json!({ "method": "get" })).await;
    let text = metrics(&h.n.live.view());
    assert!(text.contains("# TYPE blockula_program_views_total counter"), "{text}");
    for o in Outcome::ALL {
        assert!(text.contains(&format!("blockula_program_views_total{{outcome=\"{}\"}} ", o.label())), "{text}");
    }
    let app = router(h.n.clone()).layer(MockConnectInfo(LOOPBACK.parse::<SocketAddr>().unwrap()));
    let res = app.oneshot(Request::builder().uri("/metrics").body(Body::empty()).unwrap()).await.unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 8 << 20).await.unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("blockula_program_views_total{outcome=\"ok\"}"));
}

/// A view runs on the state of the newest block this node executed, at the height after it, and a
/// later block's changes show in the next view.
#[tokio::test]
async fn a_view_reads_the_newest_executed_state() {
    let h = Harness::new("view-fresh");
    let (_, j) = view(&h.n, &h.counter, json!({ "method": "get" })).await;
    let before = (u64_of(&j), j["height"].as_u64().unwrap());
    h.increment(2);
    let (_, j) = view(&h.n, &h.counter, json!({ "method": "get" })).await;
    assert_eq!((u64_of(&j), j["height"].as_u64().unwrap()), (before.0 + 2, before.1 + 1));
}

/// `chain::short` is safe on any string, since a client's text can reach it: an address (ASCII) comes
/// out as it always has, and a string whose 10th byte or 6th-from-last byte is inside a multi-byte
/// character comes out cut at characters, never a panic.
#[test]
fn short_is_safe_on_any_string() {
    let address = keys(ALICE).address_with("boo");
    assert_eq!(short(&address), format!("{}…{}", &address[..10], &address[address.len() - 6..]), "an address, as ever");
    assert_eq!(short("boo1short"), "boo1short");
    assert_eq!(short("0123456789abcdef"), "0123456789abcdef", "16 bytes is whole");
    assert_eq!(short("0123456789abcdefg"), "0123456789…bcdefg");
    // The 10th byte inside `é` (bytes 9 and 10), more than 16 bytes in all.
    let a = format!("aaaaaaaaaé{}", "b".repeat(10));
    assert_eq!(short(&a), format!("aaaaaaaaaé…{}", "b".repeat(6)));
    // The 6th byte from the end inside one (`é` is bytes 20 and 21, 27 bytes in all).
    let b = format!("{}ézzzzz", "b".repeat(20));
    assert_eq!(short(&b), "bbbbbbbbbb…ézzzzz");
    // Wide characters, 3 bytes each: 12 of them (36 bytes) cut inside one, but are no more than 16
    // characters, so they stay whole; 17 are cut by characters, 10 and 6.
    let c = "界".repeat(12);
    assert_eq!(short(&c), c);
    let d = "界".repeat(17);
    assert_eq!(short(&d), format!("{}…{}", "界".repeat(10), "界".repeat(6)));
    // Where the byte cuts happen to fall between characters they are the cuts, as they always were.
    assert_eq!(short(&"é".repeat(9)), "ééééé…ééé");
}

/// The numbers §8.2 states for views: 60 a minute per client IP, two running at once, a wait of 250 ms
/// for a slot with 16 waiting at most. (The tests that use these constants would follow a change of
/// them: this is what holds them to the spec.)
#[test]
fn the_limits_are_the_numbers_the_spec_states() {
    assert_eq!((VIEWS_PER_IP_PER_MINUTE, VIEW_WINDOW), (60, Duration::from_secs(60)));
    assert_eq!((MAX_CONCURRENT_VIEWS, MAX_WAITING_VIEWS, VIEW_SLOT_WAIT), (2, 16, Duration::from_millis(250)));
}

/// A node's own two slots don't refuse a client's parallel views: a wallet reading a dozen balances
/// at once makes more views than there are slots, and each waits its turn (a view takes about a
/// millisecond; the node here would wait a minute for a slot, so a slow machine can't run any wait
/// out). Eight at once from one client all run, and cost one hit each of its budget.
#[tokio::test]
async fn parallel_views_from_one_client_wait_for_a_slot() {
    let h = Harness::new("view-parallel");
    let mut n = h.n.clone();
    n.views = ViewGate::with_slot_wait(true, None, Duration::from_secs(60));
    let body = json!({ "method": "get" }).to_string();
    let answers = futures::future::join_all((0..8).map(|_| post(&n, STRANGER, &h.counter, &body))).await;
    let statuses: Vec<StatusCode> = answers.iter().map(|(s, _, _)| *s).collect();
    assert_eq!(statuses, vec![StatusCode::OK; 8], "{:?}", answers.iter().map(|(_, _, j)| j).collect::<Vec<_>>());
    assert_eq!(n.views.slots.available_permits(), MAX_CONCURRENT_VIEWS);
    // Eight hits of its budget, no more: 52 more views fit in it, and the next is refused for it.
    for i in 0..VIEWS_PER_IP_PER_MINUTE - 8 {
        let (s, _, j) = post(&n, STRANGER, &h.counter, &body).await;
        assert_eq!(s, StatusCode::OK, "view {i}: {j}");
    }
    let (s, _, j) = post(&n, STRANGER, &h.counter, &body).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{j}");
    assert!(j["error"].as_str().unwrap().contains("too many program views from your IP"), "{j}");
}

/// A view that waits and gets a slot runs; one that waits the full [`VIEW_SLOT_WAIT`] is refused, and
/// then it was the node's refusal, not the client's: its hit on the per-IP budget is given back, so
/// two such refusals leave the whole budget for views that run.
#[tokio::test]
async fn a_view_refused_for_want_of_a_slot_is_not_charged_to_its_ip() {
    let h = Harness::new("view-refund");
    let body = json!({ "method": "get" }).to_string();
    // A slot frees while a view waits for one (here a node that would wait a minute, so the wait
    // can't run out first): it runs.
    let mut patient = h.n.clone();
    patient.views = ViewGate::with_slot_wait(true, None, Duration::from_secs(60));
    let mut held: Vec<_> = (0..MAX_CONCURRENT_VIEWS).map(|_| patient.views.slots.clone().try_acquire_owned().unwrap()).collect();
    let (n, counter, b) = (patient.clone(), h.counter.clone(), body.clone());
    let waiting = tokio::spawn(async move { post(&n, STRANGER, &counter, &b).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!waiting.is_finished(), "it waits");
    held.pop();
    let (s, _, j) = tokio::time::timeout(Duration::from_secs(10), waiting).await.expect("it ran once a slot freed").unwrap();
    assert_eq!(s, StatusCode::OK, "{j}");
    drop(held);
    // No slot frees within the wait: refused, twice.
    for _ in 0..2 {
        let taken: Vec<_> = (0..MAX_CONCURRENT_VIEWS).map(|_| h.n.views.slots.clone().try_acquire_owned().unwrap()).collect();
        let (s, headers, j) = post(&h.n, STRANGER, &h.counter, &body).await;
        assert_eq!((s, headers.get(RETRY_AFTER).map(|v| v.to_str().unwrap().to_string())), (StatusCode::TOO_MANY_REQUESTS, Some("1".to_string())), "{j}");
        assert!(j["error"].as_str().unwrap().contains("running 2 program views already"), "{j}");
        drop(taken);
    }
    // The budget: 1 hit for the view that ran, none for the two refusals, so 59 more run and the
    // 61st view (60 hits taken) is the budget's refusal.
    for i in 0..VIEWS_PER_IP_PER_MINUTE - 1 {
        let (s, _, j) = post(&h.n, STRANGER, &h.counter, &body).await;
        assert_eq!(s, StatusCode::OK, "view {i}: {j}");
    }
    let (s, _, j) = post(&h.n, STRANGER, &h.counter, &body).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{j}");
    assert!(j["error"].as_str().unwrap().contains("too many program views from your IP"), "the budget's own refusal: {j}");
}

/// At most [`MAX_WAITING_VIEWS`] views wait for a slot, whatever the wait: the gate (waiting a minute
/// here, so none gives up) takes 16 waiters, refuses a 17th at once, hands a freed slot to a waiter,
/// and gets a place back however a wait ends (here, the request is cut off).
#[tokio::test]
async fn the_gate_takes_sixteen_waiters_and_refuses_a_seventeenth_at_once() {
    use futures::FutureExt as _;
    let gate = ViewGate::with_slot_wait(true, None, Duration::from_secs(60));
    let held: Vec<_> = (0..MAX_CONCURRENT_VIEWS).map(|_| gate.slots.clone().try_acquire_owned().unwrap()).collect();
    let waiters: Vec<_> = (0..MAX_WAITING_VIEWS)
        .map(|_| {
            let gate = gate.clone();
            tokio::spawn(async move { gate.slot().await })
        })
        .collect();
    for _ in 0..2_000 {
        if gate.waiting.available_permits() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(gate.waiting.available_permits(), 0, "sixteen are waiting");
    assert!(waiters.iter().all(|w| !w.is_finished()));
    // The 17th is refused without waiting at all: it's ready on its first poll.
    assert!(gate.slot().now_or_never().expect("a 17th doesn't wait").is_none());
    // A slot freed goes to a waiter, which keeps it: two finish with one each, the rest wait on.
    drop(held);
    for _ in 0..2_000 {
        if waiters.iter().filter(|w| w.is_finished()).count() == MAX_CONCURRENT_VIEWS {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let (done, waiting): (Vec<_>, Vec<_>) = waiters.into_iter().partition(|w| w.is_finished());
    assert_eq!((done.len(), waiting.len()), (MAX_CONCURRENT_VIEWS, MAX_WAITING_VIEWS - MAX_CONCURRENT_VIEWS));
    let mut slots = vec![];
    for w in done {
        slots.push(w.await.unwrap().expect("a waiter got the freed slot"));
    }
    assert_eq!(gate.slots.available_permits(), 0);
    // The others' requests are cut off: their places come back.
    for w in &waiting {
        w.abort();
    }
    for w in waiting {
        assert!(w.await.unwrap_err().is_cancelled());
    }
    assert_eq!(gate.waiting.available_permits(), MAX_WAITING_VIEWS);
    // A place is free again and no slot is: the next one waits (not ready, and not refused either).
    assert!(gate.slot().now_or_never().is_none());
    drop(slots);
    assert!(gate.slot().await.is_some());
}

/// The same at the route: sixteen requests wait for a slot (the gate holds them a minute here), and a
/// seventeenth gets its 429 while they all still wait, with `Retry-After: 1`.
#[tokio::test]
async fn a_seventeenth_waiting_view_is_refused_while_sixteen_wait() {
    let h = Harness::new("view-seventeen");
    let mut n = h.n.clone();
    n.views = ViewGate::with_slot_wait(true, None, Duration::from_secs(60));
    let body = json!({ "method": "get" }).to_string();
    let held: Vec<_> = (0..MAX_CONCURRENT_VIEWS).map(|_| n.views.slots.clone().try_acquire_owned().unwrap()).collect();
    let waiters: Vec<_> = (0..MAX_WAITING_VIEWS)
        .map(|_| {
            let (n, counter, body) = (n.clone(), h.counter.clone(), body.clone());
            tokio::spawn(async move { post(&n, LOOPBACK, &counter, &body).await })
        })
        .collect();
    for _ in 0..2_000 {
        if n.views.waiting.available_permits() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(n.views.waiting.available_permits(), 0, "sixteen requests wait");
    let (s, headers, j) = post(&n, LOOPBACK, &h.counter, &body).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{j}");
    assert_eq!(headers.get(RETRY_AFTER).unwrap(), "1");
    assert!(waiters.iter().all(|w| !w.is_finished()), "they all still wait: the 17th was refused without waiting for them");
    // Let them go: every waiter runs in the end, two slots at a time.
    drop(held);
    for w in waiters {
        let (s, _, j) = tokio::time::timeout(Duration::from_secs(60), w).await.expect("a waiter got its turn").unwrap();
        assert_eq!(s, StatusCode::OK, "{j}");
    }
}

/// A request that's dropped while its view runs (the client gone, the timeout layer cutting it off)
/// leaves its slot held until the VM's own task ends, and then gives it back: at most two VMs ever
/// run, whatever clients do. (The slot moves into the blocking task: held by the request's scope it
/// would drop with the request, and the VM would run on with no slot.) The VM is held inside its
/// task by the gate's latch for as long as the test needs, so the check doesn't race its end.
#[tokio::test]
async fn a_dropped_request_keeps_its_slot_until_the_vm_ends() {
    let h = Harness::new("view-permit");
    let spin = h.probes[0].clone();
    let n = Node::new(h.n.chain.clone(), None, vec![], ClientIpPolicy::default()).with_program_views(true, Some(20_000_000));
    n.views.latch.close();
    let app = router(n.clone()).layer(MockConnectInfo(LOOPBACK.parse::<SocketAddr>().unwrap()));
    let req = Request::builder().method("POST").uri(format!("/api/program/{spin}/view")).header("content-type", "application/json").body(Body::from(json!({ "method": "spin" }).to_string())).unwrap();
    let request = tokio::spawn(app.oneshot(req));
    // The request has its slot (and its VM waits on the latch) once the slot's gone from the pool.
    for _ in 0..2_000 {
        if n.views.slots.available_permits() < MAX_CONCURRENT_VIEWS {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(MAX_CONCURRENT_VIEWS - n.views.slots.available_permits(), 1, "the request took a slot to run its view");
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled(), "the request was cut off while its view ran");
    assert_eq!(MAX_CONCURRENT_VIEWS - n.views.slots.available_permits(), 1, "the cut-off request's slot is held by the VM's task, not given back with the request");
    // The VM is let go: it runs to its end, and only then is the slot back.
    n.views.latch.open();
    for _ in 0..2_000 {
        if n.views.slots.available_permits() == MAX_CONCURRENT_VIEWS {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(n.views.slots.available_permits(), MAX_CONCURRENT_VIEWS, "and gives it back when the VM's task ends");
}

/// The per-IP budget is taken before the body is parsed, so garbage costs a hit as a good request
/// does: a client's sixty malformed bodies (each refused 422) use up its minute, and the sixty-first
/// request, a good one, is the budget's 429. Over loopback, which has no budget, none of it counts.
#[tokio::test]
async fn a_malformed_body_costs_a_budget_hit() {
    let h = Harness::new("view-garbage");
    for i in 0..VIEWS_PER_IP_PER_MINUTE {
        let (s, _, j) = post(&h.n, STRANGER, &h.counter, "not json").await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "garbage {i}: {j}");
    }
    let (s, _, j) = post(&h.n, STRANGER, &h.counter, &json!({ "method": "get" }).to_string()).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{j}");
    assert!(j["error"].as_str().unwrap().contains("too many program views from your IP"), "{j}");
    for _ in 0..VIEWS_PER_IP_PER_MINUTE + 1 {
        let (s, _, _) = post(&h.n, LOOPBACK, &h.counter, "not json").await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    }
}
