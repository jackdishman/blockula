//! `blockula program …` (WBS S6b's acceptance): its arguments, what it works out for itself from the
//! node's answers (deposits, reservations, whether a validator may pause) checked against what the
//! chain does, what it prints from the node's real JSON, and the whole of it against a chain served
//! over HTTP in this process, with a block every 100 ms: upload, create, call, view, the reads and
//! the pause, each as a person would run them. (`scripts/test-program-cli.sh` runs the same against
//! a `blockula dev --programs` process.)

use super::*;
use crate::api::{router, ClientIpPolicy, Node};
use crate::chain::{Chain, SummonOpts};
use crate::crypto::Keys;
use crate::programs::params::{param, Param};
use crate::programs::probe::{calls, create as create_action, keys, probe_interface, upload as upload_action, Dev, ALICE, BOB, FANG, FUEL, PROBE_WASM};
use crate::programs::state::Pause;
use std::io::IsTerminal as _;
use blockula_vm::meter::SCHEDULE_V0;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

/// A dev chain's numbers as the CLI reads them from `/api/fees` and `/api/params`.
fn bounds_of(c: &Chain) -> Bounds {
    let p = |k: Param| param(&c.state, k);
    Bounds { schedule: &SCHEDULE_V0, max_tx_fuel: p(Param::MaxTxFuel), max_block_fuel: p(Param::MaxBlockFuel), max_tx_code_bytes: blockula_vm::config::MAX_TX_CODE_BYTES, state_bytes: c.state.programs.meta.as_ref().unwrap().state_bytes, storage_price_base: p(Param::StoragePrice), state_soft_bytes: p(Param::StateSoftBytes), min_upgrade_delay: p(Param::MinUpgradeDelayBlocks) }
}

/// What the arguments accept and refuse, in words that say what to pass instead.
#[test]
fn arguments_are_checked() {
    let h = "ab".repeat(32);
    assert_eq!(hash_arg(&h).unwrap(), h);
    for bad in ["", "ab", &h[..63], &h.to_uppercase(), &format!("{h}0"), "zz".repeat(32).as_str()] {
        assert!(hash_arg(bad).unwrap_err().to_string().contains("isn't a hash"), "{bad}");
    }
    let addr = keys(ALICE).address_with("boo");
    assert_eq!(address_arg(&addr).unwrap(), addr);
    assert_eq!(address_arg(&keys(ALICE).address_with("tboo")).unwrap(), keys(ALICE).address_with("tboo"));
    for name in ["alice.boo", "alice", &addr.to_uppercase(), "boo1xyz"] {
        let e = address_arg(name).err().unwrap_or_else(|| panic!("{name} was taken for a program's address")).to_string();
        assert!(e.contains("never by a .boo name"), "{name}: {e}");
    }
    assert_eq!(input_arg(None).unwrap(), Vec::<u8>::new());
    assert_eq!(input_arg(Some("00ff")).unwrap(), vec![0, 255]);
    for bad in ["0", "00FF", "0x00", "zz"] {
        assert!(input_arg(Some(bad)).unwrap_err().to_string().contains("lowercase hex of whole bytes"), "{bad}");
    }
    assert!(input_arg(Some(&"00".repeat(blockula_vm::config::MAX_INPUT + 1))).unwrap_err().to_string().contains("past the 32768"));
    assert_eq!(input_arg(Some(&"00".repeat(blockula_vm::config::MAX_INPUT))).unwrap().len(), blockula_vm::config::MAX_INPUT);
    // A binding: the address, and the version if said; never a name, never a malformed digest.
    assert_eq!(binding_arg(&addr).unwrap(), (addr.clone(), None));
    assert_eq!(binding_arg(&format!("{addr}@{h}")).unwrap(), (addr.clone(), Some(h.clone())));
    assert_eq!(binding_arg(&format!("{addr}@{}", "00".repeat(32))).unwrap().1, Some("00".repeat(32)), "any version, but only if said");
    assert!(binding_arg("alice.boo").is_err() && binding_arg(&format!("{addr}@xyz")).is_err() && binding_arg(&format!("alice.boo@{h}")).is_err());
    assert_eq!(fang_arg("value", None).unwrap(), 0);
    assert_eq!(fang_arg("value", Some("1.5")).unwrap(), 1_500_000_000);
    assert!(fang_arg("value", Some("1.5.5")).unwrap_err().to_string().contains("--value"));
}

/// What `upload` and `create` print as the deposit is what the chain bonds: the upload's receipt and
/// the created record say the same, for both blobs new, for a new interface over stored code, and for
/// the record.
#[test]
fn deposits_are_what_the_chain_bonds() {
    let mut d = Dev::new("cli-deposit");
    let alice = keys(ALICE);
    let iface = probe_interface();
    let predicted = bounds_of(&d.c).upload_deposit(&[(PROBE_WASM.len(), true), (iface.len(), true)]).unwrap();
    let up = d.tx(&alice, upload_action(PROBE_WASM, &iface));
    let b = d.block(vec![up]);
    let bonded = |b: &crate::types::Block| b.programs[&0].upload.as_ref().unwrap().deposit;
    assert_eq!(bonded(&b), predicted);
    assert!(predicted > 0);
    // Another interface over the stored code: only the interface is new, priced after the first landed.
    let other = {
        let methods = ["emit", "fund", "go", "no", "ret", "spin"].map(|name| blockula_vm::interface::Method { name: name.into(), kind: blockula_vm::host::MethodKind::View, payable: false, args: vec![], returns: None });
        blockula_vm::interface::encode(&blockula_vm::interface::Interface { methods: methods.into() }).unwrap()
    };
    let predicted = bounds_of(&d.c).upload_deposit(&[(PROBE_WASM.len(), false), (other.len(), true)]).unwrap();
    let up = d.tx(&alice, upload_action(PROBE_WASM, &other));
    let b = d.block(vec![up]);
    assert_eq!(bonded(&b), predicted);
    // Both stored: nothing is bonded.
    assert_eq!(bounds_of(&d.c).upload_deposit(&[(PROBE_WASM.len(), false), (iface.len(), false)]), Some(0));
    // The record.
    let predicted = bounds_of(&d.c).record_deposit().unwrap();
    let created = d.tx(&alice, create_action(PROBE_WASM, &iface));
    let program = digest::program_address(&d.prefix(), &crypto::address_bytes(&created.from).unwrap(), created.nonce);
    d.block(vec![created]);
    assert_eq!(d.c.state.programs.records[&program].record_deposit, predicted);
    // Past the ceiling there's no price: no deposit.
    let full = Bounds { state_bytes: bounds_of(&d.c).state_soft_bytes.saturating_mul(1 << 20), ..bounds_of(&d.c) };
    assert_eq!(full.upload_deposit(&[(10, true)]), None);
    assert_eq!(full.record_deposit(), None);
}

/// The deposit is priced at program state's bytes once each blob lands, in the order the chain
/// stores them (code, then interface), so an upload that crosses a step of the curve pays the higher
/// price from the byte that crosses it: written out from §4.6 here, not from the node's code.
#[test]
fn an_upload_that_crosses_a_price_step_pays_the_higher_price() {
    let (base, soft) = (100_000u64, 64u64 << 20);
    let step = soft / 8;
    let at = |state_bytes: u64| Bounds { schedule: &SCHEDULE_V0, max_tx_fuel: 1, max_block_fuel: 1, max_tx_code_bytes: 1, state_bytes, storage_price_base: base, state_soft_bytes: soft, min_upgrade_delay: 1 };
    // The code (1,000 bytes) lands 50 bytes short of the step, the interface (50) exactly on it.
    assert_eq!(at(step - 1_050).upload_deposit(&[(1_000, true), (50, true)]), Some(1_000 * base + 50 * 2 * base));
    // Both under it, both over it.
    assert_eq!(at(step - 2_000).upload_deposit(&[(1_000, true), (50, true)]), Some(1_050 * base));
    assert_eq!(at(step).upload_deposit(&[(1_000, true), (50, true)]), Some(1_050 * 2 * base));
    // A blob stored already isn't counted, and doesn't move the bytes the next one lands at.
    assert_eq!(at(step - 40).upload_deposit(&[(1_000, false), (50, true)]), Some(50 * 2 * base));
    assert_eq!(at(step - 50).upload_deposit(&[(1_000, false), (50, true)]), Some(50 * 2 * base));
    assert_eq!(at(step - 51).upload_deposit(&[(1_000, false), (50, true)]), Some(50 * base));
    // The record: 256 bytes, priced where they land.
    assert_eq!(at(step - 256).record_deposit(), Some(256 * 2 * base));
    assert_eq!(at(step - 257).record_deposit(), Some(256 * base));
    // The ceiling: 2 × soft is the last byte there's a price for.
    assert_eq!(at(2 * soft - 256).record_deposit(), Some(256 * (base << 16)));
    assert_eq!(at(2 * soft - 255).record_deposit(), None);
}

/// What a call reserves is what the chain reserves: fuel and the load of the distinct code, once per
/// hash; and what admission would refuse is refused before signing (fuel past a transaction's
/// maximum, code past what one may load, a reservation past a block's).
#[test]
fn reservations_are_what_the_chain_reserves() {
    let mut d = Dev::new("cli-reserve");
    let probes = d.deploy(2);
    let bounds = bounds_of(&d.c);
    let record = &d.c.state.programs.records[&probes[0]];
    let size = d.c.state.programs.code[&record.wasm_hash].wasm.len();
    let hash = hex::encode(record.wasm_hash);
    let call = d.tx(&keys(ALICE), calls(vec![d.call(&probes[0], "ret", vec![], 0), d.call(&probes[1], "ret", vec![], 0)], vec![]));
    d.block(vec![call]);
    let receipt = d.c.last_block().unwrap().programs[&0].clone();
    // Both programs are one code: declared once.
    let predicted = bounds.reservation(FUEL, &BTreeMap::from([(hash.clone(), size)])).unwrap();
    assert_eq!((predicted, receipt.fuel.reserved), (receipt.fuel.reserved, receipt.fuel.limit + receipt.fuel.load));
    assert_eq!(predicted, FUEL + SCHEDULE_V0.load_fuel(size));
    // Refused as admission refuses.
    let e = bounds.reservation(bounds.max_tx_fuel + 1, &BTreeMap::new()).unwrap_err().to_string();
    assert!(e.contains("above program_max_tx_fuel"), "{e}");
    let e = bounds.reservation(1, &BTreeMap::from([("a".into(), bounds.max_tx_code_bytes), ("b".into(), 1)])).unwrap_err().to_string();
    assert!(e.contains("past the") && e.contains("one transaction may load"), "{e}");
    let tight = Bounds { max_block_fuel: 1_000, ..bounds_of(&d.c) };
    let e = tight.reservation(1_000, &BTreeMap::from([(hash, size)])).unwrap_err().to_string();
    assert!(e.contains("a block counts at most 1000"), "{e}");
    assert_eq!(tight.reservation(1, &BTreeMap::new()).unwrap(), 1);
}

/// What the CLI reads of `/api/params` for a pause is what META holds, and `may_sign_now` (the check
/// `blockula program pause` makes before it signs) over it says what `pause::may_sign` says over the
/// state, word for word, in every case §7.2 has: nothing
/// signed yet, a live vote, a run at its cap, a lapsed run too soon after, the window with and without
/// a fix pending.
#[test]
fn a_validator_is_told_what_the_chain_will_say() {
    let mut d = Dev::new("cli-pause");
    let validator = d.c.me().unwrap();
    let mut c = d.take();
    let spans = pause::Spans::of(&c.genesis.params());
    let now = |c: &Chain| crate::econ::now(&c.state);
    let verdicts = |c: &Chain| {
        let h = now(c);
        let section = crate::programs::api::params_section(&c.live.view()).unwrap();
        let facts = pause_facts(&section).unwrap();
        let cli = may_sign_now(&facts, &spans, &validator, h).map_err(|e| e.to_string());
        let chain = pause::may_sign(&c.state, &validator, h).map_err(|e| e.to_string());
        let meta = c.state.programs.meta.as_ref().unwrap();
        let run = |p: &Option<Pause>| p.as_ref().map(|p| (p.from, p.until, p.started));
        assert_eq!((run(&facts.pause), &facts.votes, &facts.pause_signed), (run(&meta.pause), &meta.votes, &meta.pause_signed), "what the node serves is META (a run's signers aren't read: no rule looks at them)");
        assert_eq!(facts.fix_pending, pause::fix_pending(&c.state, h));
        assert_eq!(cli, chain, "the CLI and the chain agree at block {h}");
        chain
    };
    // META changed by hand, then a block, which publishes the view `/api/params` reads.
    let set = |c: &mut Chain, f: &dyn Fn(&mut crate::programs::state::ProgramsMeta)| {
        f(c.state.programs.meta.as_mut().unwrap());
        c.produce_block_with(vec![]).unwrap();
    };
    assert!(verdicts(&c).is_ok(), "nothing signed yet");
    // A live vote of its own.
    set(&mut c, &|m| {
        m.votes.insert(validator.clone(), 0);
    });
    assert!(verdicts(&c).unwrap_err().contains("already signed a pause at block 0"));
    set(&mut c, &|m| m.votes.clear());
    // A running pause that has reached its cap: it ends where its run's 30 days do.
    set(&mut c, &|m| m.pause = Some(Pause { from: 1, until: 1 + spans.max_span, started: 1, signers: vec![validator.clone()] }));
    assert!(verdicts(&c).unwrap_err().contains("has reached its 30-day cap"));
    // A run that lapsed: the next can't start before 60 days from its start.
    set(&mut c, &|m| m.pause = Some(Pause { from: 1, until: 2, started: 1, signers: vec![validator.clone()] }));
    assert!(verdicts(&c).unwrap_err().contains("programs can be paused again from block"));
    set(&mut c, &|m| m.pause = None);
    assert!(verdicts(&c).is_ok());
    // The window: it started or renewed a pause within the last 30 days.
    set(&mut c, &|m| {
        m.pause_signed.insert(validator.clone(), 0);
    });
    assert!(verdicts(&c).unwrap_err().contains("may sign again from block"));
    // ... unless a fix is pending: an upgrade scheduled above the height waives it.
    let h = now(&c);
    c.state.upgrade.schedule.push(crate::upgrade::ScheduledUpgrade { version: 3, activation_height: h + 1_000_000 });
    c.produce_block_with(vec![]).unwrap();
    assert!(verdicts(&c).is_ok(), "a pending fix waives the window");
}

/// What the CLI prints from the node's real answers: a program's record, a receipt (a call's, with
/// claims apart from facts, and a failed one's), a view's answer, an upload's and a create's, read
/// from `/api/program` and `/api/tx` as served, so a key the node doesn't have shows as `?`.
#[tokio::test]
async fn the_cli_reads_what_the_node_serves() {
    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::Request;
    use tower::ServiceExt as _;
    let mut d = Dev::new("cli-print");
    let alice = keys(ALICE);
    let iface = probe_interface();
    let up = d.tx(&alice, upload_action(PROBE_WASM, &iface));
    d.block(vec![up.clone()]);
    let cr = d.tx(&alice, create_action(PROBE_WASM, &iface));
    let program = digest::program_address(&d.prefix(), &crypto::address_bytes(&cr.from).unwrap(), cr.nonce);
    d.block(vec![cr.clone()]);
    let batch = d.tx(&alice, calls(vec![d.call(&program, "ret", vec![], 0), d.call(&program, "fund", vec![], FANG)], vec![]));
    d.block(vec![batch.clone()]);
    let failing = d.tx(&alice, calls(vec![d.call(&program, "no", vec![], 0)], vec![]));
    d.block(vec![failing.clone()]);
    let n = Node::new(Arc::new(Mutex::new(d.take())), None, vec![], ClientIpPolicy::default());
    let get = |uri: String| {
        let app = router(n.clone()).layer(MockConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));
        async move {
            let res = app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
            serde_json::from_slice::<Value>(&axum::body::to_bytes(res.into_body(), 8 << 20).await.unwrap()).unwrap()
        }
    };
    let no_gaps = |lines: &[String]| {
        for l in lines {
            assert!(!l.contains(" ? ") && !l.ends_with(" ?") && !l.contains("?:") && !l.contains("null"), "a key the CLI reads is missing from the node's JSON: {l}");
        }
    };
    // show
    let shown = show_lines(&get(format!("/api/program/{program}")).await);
    no_gaps(&shown);
    assert_eq!(shown[0], format!("program {program}"));
    assert!(shown.iter().any(|l| l.contains("immutable")) && shown.iter().any(|l| l.contains("takes plain transfers: no")));
    assert!(shown.iter().any(|l| l == "   methods (named by the deployer):"));
    assert!(shown.iter().any(|l| l.starts_with("   - fund() call, payable")), "{shown:?}");
    // an upload, a create
    let lines = receipt_lines(&get(format!("/api/tx/{}", up.id())).await);
    no_gaps(&lines);
    assert!(lines[0].starts_with("✓ ") && lines.iter().any(|l| l.contains("stored code") && l.contains("(new)")) && lines.iter().any(|l| l.contains("drops bonded")), "{lines:?}");
    // What the upload's deposit is debited as, outside the program's own balance changes, and not as a
    // note that it's "only counted" in `storage` (a create's initializer's write bonds are in both).
    let outside = get(format!("/api/tx/{}", up.id())).await["program"]["facts"]["outside_vm"].clone();
    let deposit = outside["upload_deposit"].as_str().unwrap();
    assert!(lines.iter().any(|l| l == &format!("   also debited from the signer: upload deposit {deposit} = {deposit} drops")), "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains("only counted here")), "{lines:?}");
    let lines = receipt_lines(&get(format!("/api/tx/{}", cr.id())).await);
    no_gaps(&lines);
    assert!(lines.iter().any(|l| l == &format!("   created program {program}")), "{lines:?}");
    let created = get(format!("/api/tx/{}", cr.id())).await["program"]["facts"]["outside_vm"].clone();
    let want = format!("   also debited from the signer: record deposit {} + account creation fee (burned) {} = {} drops", created["record_deposit"].as_str().unwrap(), created["account_creation_fee"].as_str().unwrap(), created["total"].as_str().unwrap());
    assert!(lines.iter().any(|l| l == &want), "{lines:?}");
    // A call debits nothing outside the VM: no such line.
    // a call: facts, then claims
    let lines = receipt_lines(&get(format!("/api/tx/{}", batch.id())).await);
    no_gaps(&lines);
    assert!(lines.iter().any(|l| l.starts_with("   fee ") && l.contains("fuel")) && lines.iter().any(|l| l.starts_with("   fuel: reserved ")), "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains("also debited")), "{lines:?}");
    let claims = lines.iter().position(|l| l.contains("the programs report (claims")).expect("claims are marked");
    assert!(lines[..claims].iter().all(|l| !l.contains("returned")) && lines[claims..].iter().any(|l| l.contains("- ret returned ")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains(&program) && l.contains("drops")), "the balance changes are facts: {lines:?}");
    // a failed call
    let lines = receipt_lines(&get(format!("/api/tx/{}", failing.id())).await);
    assert!(lines[0].contains("it ran and failed: program ") && lines[0].contains("aborted with code 7") && lines[0].contains("the program's words") && lines[0].contains("the fee was charged"), "{lines:?}");
    // a transaction that isn't a program's has no receipt to print.
    assert_eq!(receipt_lines(&json!({ "summary": "alice sent 1 FANG" })), vec!["✓ alice sent 1 FANG".to_string()]);
    // a view
    let ok = json!({ "ok": true, "outcome": { "ok": true }, "return_data": "0100", "fuel_used": 12, "height": 9 });
    assert_eq!(view_lines(&ok), ["the program reports: return data 0100", "   fuel used 12 · at block 9 (the newest this node executed)"]);
    assert!(view_lines(&json!({ "ok": true, "return_data": "", "fuel_used": 1, "height": 1 }))[0].contains("(no return data)"));
    let failed = json!({ "ok": false, "outcome": { "ok": false, "number": 1, "code": "trapped", "program": program, "trap": { "number": 1, "name": "out_of_fuel" } }, "return_data": null, "fuel_used": 5, "height": 9 });
    assert!(view_lines(&failed)[0].contains("trapped: out of fuel (trap 1)"));
}

/// `interface encode` is offline and exact: the guest's JSON encodes to the bytes and hash its
/// interface has, and a file that isn't an interface is refused in the codec's words.
#[test]
fn interface_encode_matches_the_committed_guest() {
    let dir = Dev::fresh_home("cli-iface");
    std::fs::create_dir_all(&dir).unwrap();
    let json = dir.join("counter.iface.json");
    std::fs::write(&json, include_str!("../../vm/tests/programs/counter.iface.json")).unwrap();
    let bytes = interface_bytes(&json).unwrap();
    assert_eq!(bytes, include_bytes!("../../vm/tests/programs/counter.iface"));
    assert!(interface("encode", Some(&json), "http://unused.invalid").is_ok(), "offline: it never asks a node");
    assert!(interface("encode", None, "http://unused.invalid").is_err());
    std::fs::write(&json, r#"{ "format": 1, "methods": [ { "name": "b", "kind": "call", "payable": false, "args": [], "returns": null }, { "name": "a", "kind": "call", "payable": false, "args": [], "returns": null } ] }"#).unwrap();
    let e = interface_bytes(&json).unwrap_err();
    assert!(format!("{e:#}").contains("isn't an interface"), "{e:#}");
    std::fs::write(&json, "nonsense").unwrap();
    assert!(interface_bytes(&json).is_err());
    assert!(interface(&"ab".repeat(32), Some(&json), "http://unused.invalid").unwrap_err().to_string().contains("takes no file"));
    std::fs::remove_dir_all(dir).ok();
}

// ------------------------------------------------------------- the whole CLI, against a served chain

/// A chain born with programs on (`SummonOpts::with_programs`: what `blockula summon --programs` and
/// `blockula dev --programs` write), served over HTTP in this process, with a block every 100 ms.
struct Served {
    rpc: String,
    home: std::path::PathBuf,
    dir: std::path::PathBuf,
    chain: Arc<Mutex<Chain>>,
    /// The validator's key file, and a funded user's.
    validator_key: std::path::PathBuf,
    user_key: std::path::PathBuf,
    user: Keys,
}

impl Drop for Served {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.home).ok();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

async fn served(tag: &str) -> Served {
    let home = Dev::fresh_home(tag);
    let dir = Dev::fresh_home(&format!("{tag}-cli"));
    std::fs::create_dir_all(&dir).unwrap();
    let opts = SummonOpts { block_time_ms: 50, address_prefix: "boo".into(), ..SummonOpts::new("programs-cli-1", 1).with_programs() };
    Chain::summon(&home, &opts).unwrap();
    let chain = Arc::new(Mutex::new(Chain::open(&home).unwrap()));
    let user = Keys::from_seed([91; 32]);
    chain.lock().faucet(&user.address_with("boo"), 500 * crate::types::DROPS_PER_FANG).unwrap();
    chain.lock().produce_block().unwrap();
    // The validator's node serves views here (as `blockula dev`'s does: it is its own RPC).
    let node = Node::new(chain.clone(), None, vec![], ClientIpPolicy::default()).with_program_views(true, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rpc = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router(node).into_make_service_with_connect_info::<SocketAddr>()).await.ok() });
    let ticker = chain.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = ticker.lock().produce_block();
        }
    });
    let user_key = dir.join("user.key");
    std::fs::write(&user_key, user.seed_hex()).unwrap();
    Served { rpc, validator_key: home.join("validator.key"), home, dir, chain, user_key, user }
}

impl Served {
    fn signer(&self) -> Signer {
        Signer { key: Some(self.user_key.clone()), from: None, rpc: self.rpc.clone(), pin: ChainPin::default() }
    }

    /// Runs `cmd` as a person would (on a thread of its own: the CLI makes its runtime), returning
    /// its result.
    async fn run(&self, cmd: ProgramCmd) -> Result<()> {
        let dir = self.dir.clone();
        let t = std::thread::spawn(move || program(cmd, &dir));
        tokio::task::spawn_blocking(move || t.join().expect("the command didn't panic")).await.unwrap()
    }

    async fn get(&self, path: &str) -> Value {
        reqwest::get(format!("{}/api{path}", self.rpc)).await.unwrap().json().await.unwrap()
    }
}

/// Upload, create, call and view a guest, as a person would, one command after the next with no
/// waiting in between (`create` and `call` wait until what they need is usable and say so), then the
/// reads, then a failing call, then the validator's pause. Every refusal on the way is before
/// anything is signed (nothing is charged for it).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_program_commands_run_against_a_served_chain() {
    let s = served("cli-served").await;
    let wasm = s.dir.join("counter.wasm");
    let iface = s.dir.join("counter.iface.json");
    std::fs::write(&wasm, include_bytes!("../../vm/tests/programs/counter.wasm")).unwrap();
    std::fs::write(&iface, include_str!("../../vm/tests/programs/counter.iface.json")).unwrap();
    let (wasm_hash, interface_hash) = (hex::encode(digest::wasm_hash(include_bytes!("../../vm/tests/programs/counter.wasm"))), hex::encode(digest::interface_hash(include_bytes!("../../vm/tests/programs/counter.iface"))));
    let user = s.user.address_with("boo");
    let balance = |s: &Served| s.chain.lock().state.account(&s.user.address_with("boo")).balance;

    // Refused before anything is signed: a name for a program, code the chain would refuse.
    let before = balance(&s);
    let e = s.run(ProgramCmd::Call { address: "counter.boo".into(), method: "get".into(), input: None, value: None, fuel: None, storage_limit: None, bindings: vec![], yes: true, signer: s.signer() }).await.unwrap_err().to_string();
    assert!(e.contains("never by a .boo name"), "{e}");
    let bad = s.dir.join("bad.wasm");
    std::fs::write(&bad, [0, 0x61, 0x73, 0x6d, 1, 0, 0, 0, 0xff]).unwrap();
    let e = s.run(ProgramCmd::Upload { code: bad, interface: iface.clone(), storage_limit: None, yes: true, signer: s.signer() }).await.unwrap_err().to_string();
    assert!(e.contains("the chain would refuse this code"), "{e}");
    let e = s.run(ProgramCmd::Upload { code: wasm.clone(), interface: iface.clone(), storage_limit: Some("0.000001".into()), yes: true, signer: s.signer() }).await.unwrap_err().to_string();
    assert!(e.contains("is below the deposit"), "{e}");
    assert_eq!(balance(&s), before, "nothing was signed, so nothing was charged");

    // An upload asks before it signs a deposit v0 never gives back: not at a terminal it says so, and
    // signs nothing (a terminal would be asked instead).
    if !std::io::stdin().is_terminal() {
        let e = s.run(ProgramCmd::Upload { code: wasm.clone(), interface: iface.clone(), storage_limit: None, yes: false, signer: s.signer() }).await.unwrap_err().to_string();
        assert!(e.contains("Upload ") && e.contains("v0 never gives it back") && e.contains("pass --yes"), "{e}");
        assert_eq!((balance(&s), s.get(&format!("/account/{user}")).await["next_nonce"].as_u64()), (before, Some(0)), "nothing was signed");
    }

    // Upload; the deposit it printed is what was bonded.
    s.run(ProgramCmd::Upload { code: wasm.clone(), interface: iface.clone(), storage_limit: None, yes: true, signer: s.signer() }).await.unwrap();
    let code = s.get(&format!("/code/{wasm_hash}")).await;
    assert_eq!(code["uploader"]["address"], user.as_str());
    assert_eq!(s.get(&format!("/interface/{interface_hash}")).await["uploader"]["address"], user.as_str());

    // Create straight away: it waits for the code to be usable, then makes the program at the
    // address from the account's next nonce.
    let nonce = s.get(&format!("/account/{user}")).await["next_nonce"].as_u64().unwrap();
    let program_address = digest::program_address("boo", &crypto::address_bytes(&user).unwrap(), nonce);
    // The creation fee it tells the creator before asking is what the chain burns (§8.1's `outside_vm`):
    // the chain's own, while nothing holds an account at the program's address.
    let chain_fee = s.get("/fees").await["account_creation_fee"].as_str().unwrap().parse::<u64>().unwrap();
    assert!(chain_fee > 0);
    assert_eq!(program_opening(&s.rpc, &program_address).await.unwrap(), Opening::Burns(chain_fee));
    assert_eq!(program_opening(&s.rpc, &user).await.unwrap(), Opening::Adopted, "an account that exists is adopted, not opened");
    if !std::io::stdin().is_terminal() {
        let create = |yes: bool| ProgramCmd::Create { wasm_hash: wasm_hash.clone(), interface_hash: interface_hash.clone(), init: None, input: None, value: None, fuel: None, storage_limit: None, authority: None, delay: None, policy: None, accepts_transfers: false, bindings: vec![], yes, signer: s.signer() };
        let nonce_now = s.get(&format!("/account/{user}")).await["next_nonce"].as_u64();
        let charged = balance(&s);
        let e = s.run(create(false)).await.unwrap_err().to_string();
        assert!(e.contains(&format!("Create {program_address}")) && e.contains("burned to open its account") && e.contains("bonded for good") && e.contains("pass --yes"), "{e}");
        assert_eq!((balance(&s), s.get(&format!("/account/{user}")).await["next_nonce"].as_u64()), (charged, nonce_now), "nothing was signed");
    }
    s.run(ProgramCmd::Create { wasm_hash: wasm_hash.clone(), interface_hash: interface_hash.clone(), init: None, input: None, value: None, fuel: None, storage_limit: None, authority: None, delay: None, policy: None, accepts_transfers: false, bindings: vec![], yes: true, signer: s.signer() }).await.unwrap();
    let shown = s.get(&format!("/program/{program_address}")).await;
    assert_eq!((shown["creator"]["address"].as_str(), shown["upgradability"].as_str()), (Some(user.as_str()), Some("immutable")));
    let burned = |s: &Served, program: String| {
        let rpc = s.rpc.clone();
        async move {
            let created_at = reqwest::get(format!("{rpc}/api/program/{program}")).await.unwrap().json::<Value>().await.unwrap()["created_at"].as_u64().unwrap();
            let block: Value = reqwest::get(format!("{rpc}/api/block/{created_at}")).await.unwrap().json().await.unwrap();
            let tx = block["txs"].as_array().unwrap().iter().find(|t| t["program"]["facts"]["created"] == program.as_str()).expect("the create").clone();
            tx["program"]["facts"]["outside_vm"]["account_creation_fee"].as_str().unwrap().parse::<u64>().unwrap()
        }
    };
    assert_eq!(burned(&s, program_address.clone()).await, chain_fee, "the chain burned what the CLI said it would");

    // A create with an authority needs a long enough notice; a name for it is refused.
    let e = s.run(ProgramCmd::Create { wasm_hash: wasm_hash.clone(), interface_hash: interface_hash.clone(), init: None, input: None, value: None, fuel: None, storage_limit: None, authority: Some(user.clone()), delay: Some(5), policy: None, accepts_transfers: false, bindings: vec![], yes: true, signer: s.signer() }).await.unwrap_err().to_string();
    assert!(e.contains("program_min_upgrade_delay_blocks"), "{e}");
    assert!(s.run(ProgramCmd::Create { wasm_hash: wasm_hash.clone(), interface_hash: interface_hash.clone(), init: None, input: None, value: None, fuel: None, storage_limit: None, authority: Some("alice.boo".into()), delay: None, policy: None, accepts_transfers: false, bindings: vec![], yes: true, signer: s.signer() }).await.is_err());

    // Call: increments, twice; the receipt is on the chain, with the fuel in its fee.
    let call = |method: &str, fuel: Option<u64>| ProgramCmd::Call { address: program_address.clone(), method: method.into(), input: None, value: None, fuel, storage_limit: Some("1".into()), bindings: vec![], yes: true, signer: s.signer() };
    s.run(call("increment", None)).await.unwrap();
    s.run(call("increment", None)).await.unwrap();
    // A binding without a version digest is filled with the version in force now, never with zeros
    // (the CLI never signs "any version" unless told to); with `@digest` it signs what it was given.
    let second_nonce = s.get(&format!("/account/{user}")).await["next_nonce"].as_u64().unwrap();
    let second = digest::program_address("boo", &crypto::address_bytes(&user).unwrap(), second_nonce);
    s.run(ProgramCmd::Create { wasm_hash: wasm_hash.clone(), interface_hash: interface_hash.clone(), init: None, input: None, value: None, fuel: None, storage_limit: None, authority: None, delay: None, policy: None, accepts_transfers: false, bindings: vec![], yes: true, signer: s.signer() }).await.unwrap();
    let second_digest = s.get(&format!("/program/{second}")).await["version_digest"].as_str().unwrap().to_string();
    let bound = |spec: String| ProgramCmd::Call { address: program_address.clone(), method: "increment".into(), input: None, value: None, fuel: None, storage_limit: Some("1".into()), bindings: vec![spec], yes: true, signer: s.signer() };
    let binding_of_latest = |s: &Served| {
        let user = user.clone();
        let rpc = s.rpc.clone();
        async move {
            let account: Value = reqwest::get(format!("{rpc}/api/account/{user}")).await.unwrap().json().await.unwrap();
            let txs = account["recent_txs"].as_array().or_else(|| account["txs"].as_array()).cloned().expect("recent transactions");
            let call = txs.iter().find(|t| t["kind"] == "call_program").expect("a call").clone();
            call["action"]["bindings"][0].clone()
        }
    };
    s.run(bound(second.clone())).await.unwrap();
    assert_eq!(binding_of_latest(&s).await, json!({ "program": second, "version_digest": second_digest }), "the version in force, filled in");
    s.run(bound(format!("{second}@{}", "00".repeat(32)))).await.unwrap();
    assert_eq!(binding_of_latest(&s).await["version_digest"], "00".repeat(32), "any version, because it was said");
    // FANG sent to the next create's address first opens its account: the create adopts it, and says
    // there's no creation fee, which the chain agrees with.
    let third_nonce = s.get(&format!("/account/{user}")).await["next_nonce"].as_u64().unwrap();
    let third = digest::program_address("boo", &crypto::address_bytes(&user).unwrap(), third_nonce);
    s.chain.lock().faucet(&third, FANG).unwrap();
    for _ in 0..100 {
        if program_opening(&s.rpc, &third).await.unwrap() == Opening::Adopted {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(program_opening(&s.rpc, &third).await.unwrap(), Opening::Adopted, "the account the faucet opened is the program's to adopt");
    s.run(ProgramCmd::Create { wasm_hash: wasm_hash.clone(), interface_hash: interface_hash.clone(), init: None, input: None, value: None, fuel: None, storage_limit: None, authority: None, delay: None, policy: None, accepts_transfers: false, bindings: vec![], yes: true, signer: s.signer() }).await.unwrap();
    assert_eq!(burned(&s, third).await, 0, "an adopted account pays no creation fee");
    assert!(s.run(bound("alice.boo".into())).await.is_err(), "a name isn't a program");
    // A view of a view method reads it; a call method isn't a view and a view isn't a call.
    let view = |method: &str| ProgramCmd::View { address: program_address.clone(), method: method.into(), input: None, bindings: vec![], fuel: None, rpc: Rpc { rpc: s.rpc.clone() } };
    s.run(view("get")).await.unwrap();
    let e = s.run(view("increment")).await.unwrap_err().to_string();
    assert!(e.contains("is a call, which only a transaction runs"), "{e}");
    let e = s.run(call("get", None)).await.unwrap_err().to_string();
    assert!(e.contains("is a view: read it with `blockula program view"), "{e}");
    let e = s.run(call("nothing", None)).await.unwrap_err().to_string();
    assert!(e.contains("has no method"), "{e}");
    let e = s.run(call("increment", Some(blockula_vm::meter::MAX_TX_FUEL + 1))).await.unwrap_err().to_string();
    assert!(e.contains("above program_max_tx_fuel"), "{e}");
    let (_, viewed) = {
        let r = reqwest::Client::new().post(format!("{}/api/program/{program_address}/view", s.rpc)).json(&json!({ "method": "get" })).send().await.unwrap();
        (r.status(), r.json::<Value>().await.unwrap())
    };
    assert_eq!(viewed["return_data"], "0400000000000000", "four increments: two plain, two bound");
    // A call that sends FANG to a method that isn't payable is refused before signing.
    let e = s.run(ProgramCmd::Call { address: program_address.clone(), method: "increment".into(), input: None, value: Some("1".into()), fuel: None, storage_limit: None, bindings: vec![], yes: true, signer: s.signer() }).await.unwrap_err().to_string();
    assert!(e.contains("isn't payable"), "{e}");

    // The reads.
    s.run(ProgramCmd::Show { address: program_address.clone(), rpc: Rpc { rpc: s.rpc.clone() } }).await.unwrap();
    s.run(ProgramCmd::Code { hash: wasm_hash.clone(), rpc: Rpc { rpc: s.rpc.clone() } }).await.unwrap();
    s.run(ProgramCmd::Interface { what: interface_hash.clone(), file: None, rpc: Rpc { rpc: s.rpc.clone() } }).await.unwrap();
    s.run(ProgramCmd::Interface { what: "encode".into(), file: Some(iface.clone()), rpc: Rpc { rpc: "http://unused.invalid".into() } }).await.unwrap();
    assert!(s.run(ProgramCmd::Show { address: keys(BOB).address_with("boo"), rpc: Rpc { rpc: s.rpc.clone() } }).await.is_err(), "no such program");

    // Pause: the validator's own key; its program transactions stop from the next block.
    let before = s.get("/params").await["programs"]["status"].clone();
    assert_eq!(before, "on");
    // A user's key isn't a validator's.
    let e = s.run(ProgramCmd::Pause { key: s.user_key.clone(), yes: true, rpc: s.rpc.clone(), pin: ChainPin::default() }).await.unwrap_err().to_string();
    assert!(e.contains("isn't a validator of the active set"), "{e}");
    s.run(ProgramCmd::Pause { key: s.validator_key.clone(), yes: true, rpc: s.rpc.clone(), pin: ChainPin::default() }).await.unwrap();
    let after = s.get("/params").await;
    assert_eq!(after["programs"]["status"], "paused", "{after}");
    // Paused: a call is refused (at the node, before its signature is checked) and costs nothing.
    let charged = balance(&s);
    let e = s.run(call("increment", None)).await.unwrap_err().to_string();
    assert!(e.contains("programs are paused until block"), "{e}");
    assert_eq!(balance(&s), charged);
    // ... and a second pause by the same validator is refused: it has a live signature / run.
    let e = s.run(ProgramCmd::Pause { key: s.validator_key.clone(), yes: true, rpc: s.rpc.clone(), pin: ChainPin::default() }).await.unwrap_err().to_string();
    assert!(e.contains("may not sign a pause now"), "{e}");
    // Views go on while paused.
    s.run(view("get")).await.unwrap();
    let _ = (&s.home, FUEL);
}

/// A chain with programs off has nothing for the program commands to sign: they say so, and the
/// reads say there are no programs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chain_without_programs_is_told_so() {
    let home = Dev::fresh_home("cli-off");
    let dir = Dev::fresh_home("cli-off-cli");
    std::fs::create_dir_all(&dir).unwrap();
    Chain::summon(&home, &SummonOpts { block_time_ms: 50, address_prefix: "boo".into(), ..SummonOpts::new("programs-cli-off", 1) }).unwrap();
    let chain = Arc::new(Mutex::new(Chain::open(&home).unwrap()));
    let user = Keys::from_seed([92; 32]);
    chain.lock().faucet(&user.address_with("boo"), 50 * crate::types::DROPS_PER_FANG).unwrap();
    chain.lock().produce_block().unwrap();
    let node = Node::new(chain.clone(), None, vec![], ClientIpPolicy::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rpc = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router(node).into_make_service_with_connect_info::<SocketAddr>()).await.ok() });
    let key = dir.join("user.key");
    std::fs::write(&key, user.seed_hex()).unwrap();
    let wasm = dir.join("c.wasm");
    let iface = dir.join("c.iface.json");
    std::fs::write(&wasm, include_bytes!("../../vm/tests/programs/counter.wasm")).unwrap();
    std::fs::write(&iface, include_str!("../../vm/tests/programs/counter.iface.json")).unwrap();
    let signer = Signer { key: Some(key), from: None, rpc: rpc.clone(), pin: ChainPin::default() };
    let d2 = dir.clone();
    let result = tokio::task::spawn_blocking(move || std::thread::spawn(move || program(ProgramCmd::Upload { code: wasm, interface: iface, storage_limit: None, yes: true, signer }, &d2)).join().unwrap()).await.unwrap();
    let e = result.unwrap_err().to_string();
    assert!(e.contains("programs aren't on") && e.contains("--programs"), "{e}");
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_dir_all(dir).ok();
}

/// A node that serves `/api/status` at a height that grows with every look, and a stored blob that
/// shows up on the second look: what `wait_usable` polls.
async fn scripted_node() -> (String, Arc<std::sync::atomic::AtomicU64>) {
    use axum::routing::get as route;
    use std::sync::atomic::Ordering::SeqCst;
    let looks = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (a, b) = (looks.clone(), looks.clone());
    let app = axum::Router::new()
        .route("/api/status", route(move || {
            let height = 2 + a.fetch_add(1, SeqCst) + 1;
            async move { axum::Json(json!({ "chain_id": "mock-1", "genesis_hash": "00".repeat(32), "address_prefix": "boo", "height": height })) }
        }))
        .route("/api/code/{hash}", route(move || {
            let looks = b.load(SeqCst);
            async move {
                if looks < 2 {
                    (axum::http::StatusCode::NOT_FOUND, axum::Json(json!({ "error": "code not found" })))
                } else {
                    (axum::http::StatusCode::OK, axum::Json(json!({ "uploaded_at": 5 })))
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rpc = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    (rpc, looks)
}

/// `create` and `call` wait for what they name: not stored (a 404), then stored in block 5 but not
/// yet usable until block 6 has been executed, and only then do they go on, saying so; if it never
/// shows up they say so after the polls they have.
#[tokio::test]
async fn waiting_for_what_a_transaction_needs_is_waiting_for_the_block_after() {
    use std::sync::atomic::Ordering::SeqCst;
    let (rpc, looks) = scripted_node().await;
    let remote = Remote { rpc: rpc.clone(), chain_id: "mock-1".into(), genesis_hash: "00".repeat(32), prefix: "boo".into() };
    let found = wait_usable(&remote, "the code", &format!("/code/{}", "ab".repeat(32)), "uploaded_at", "upload it first", 10).await.unwrap();
    assert_eq!(found["uploaded_at"], 5);
    // Heights 3 and 4: not stored then stored in block 5, not usable; height 5: the block that stored
    // it; height 6, the first after it: usable. Four looks, no fewer.
    assert_eq!(looks.load(SeqCst), 4);
    // Never stored: it says so after the polls it was given, and what to do.
    let (rpc, _) = (rpc, ());
    let remote = Remote { rpc, ..remote };
    let e = wait_usable(&remote, "the program", &format!("/program/{}", "x"), "created_at", "create it first", 2).await.unwrap_err().to_string();
    assert!(e.contains("isn't there after 2 seconds: create it first"), "{e}");
}

/// A `create` started while the upload it needs is still in flight waits for it and then makes the
/// program (`wait_usable`, as above, against a real chain): both commands succeed whichever comes
/// first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_create_started_before_its_upload_lands_waits_for_it() {
    let s = served("cli-wait").await;
    let wasm = s.dir.join("counter.wasm");
    let iface = s.dir.join("counter.iface.json");
    std::fs::write(&wasm, include_bytes!("../../vm/tests/programs/counter.wasm")).unwrap();
    std::fs::write(&iface, include_str!("../../vm/tests/programs/counter.iface.json")).unwrap();
    let (wasm_hash, interface_hash) = (hex::encode(digest::wasm_hash(include_bytes!("../../vm/tests/programs/counter.wasm"))), hex::encode(digest::interface_hash(include_bytes!("../../vm/tests/programs/counter.iface"))));
    let create = ProgramCmd::Create { wasm_hash, interface_hash, init: None, input: None, value: None, fuel: None, storage_limit: None, authority: None, delay: None, policy: None, accepts_transfers: false, bindings: vec![], yes: true, signer: s.signer() };
    let upload = ProgramCmd::Upload { code: wasm, interface: iface, storage_limit: None, yes: true, signer: s.signer() };
    let (created, uploaded) = tokio::join!(s.run(create), async {
        tokio::time::sleep(Duration::from_millis(400)).await;
        s.run(upload).await
    });
    uploaded.unwrap();
    created.expect("a create started before its upload landed waits for the code to be usable");
    assert_eq!(s.chain.lock().state.programs.records.len(), 1, "the program was made");
}

// ----------------------------------------------------------- what is said before anything is signed

/// What `create` prints and asks before it signs (§8.3): the address and its warning, the record
/// deposit, the account creation fee burned to open the program's account (or that FANG sent ahead
/// opened it already, so none burns), the most the fee can be, and in the question the whole cost.
#[test]
fn a_create_says_what_it_costs_before_it_asks() {
    let cost = |opening, init: bool, value, authority: Option<&str>| CreateCost { address: "boo1program".into(), nonce: 7, record_deposit: 25_600_000, opening, fee: 66_000, reserved: if init { 10_000_000 } else { 0 }, init, value, authority: authority.map(String::from), delay: 100_800 };
    let opened = create_lines(&cost(Opening::Burns(100_000_000), false, 0, None));
    assert_eq!(opened[0], "program   boo1program  (what this create makes, at nonce 7)");
    assert!(opened[1].contains("send nothing to that address before this create is final"));
    assert!(opened[2].contains("deposit 0.0256 FANG for the record (bonded, and v0 has no way to take it back)") && !opened[2].contains("initializer"), "{opened:?}");
    assert!(opened[3].contains("opening the program's account burns 0.1 FANG too") && opened[3].contains("not a deposit"), "{opened:?}");
    assert_eq!(opened[4], "   fees    at most 0.000066 FANG");
    let adopted = create_lines(&cost(Opening::Adopted, true, 0, None));
    assert!(adopted[2].contains("the initializer's writes bond more, up to --storage-limit"), "{adopted:?}");
    assert!(adopted[3].contains("already opened its account") && adopted[3].contains("no creation fee burns") && !adopted[3].contains("burns 0"), "{adopted:?}");
    assert!(adopted[4].contains("10000000 fuel reserved"), "{adopted:?}");
    // A chain that charges nothing for a new account says so, and burns nothing.
    let free = create_lines(&cost(Opening::Burns(0), false, 0, None));
    assert!(free[3].contains("charges no account creation fee") && !free[3].contains("adopts"), "{free:?}");
    assert!(!create_question(&cost(Opening::Burns(0), false, 0, None)).contains("burned"));
    // The question has the whole cost: the deposit, the burn, the fee, what's sent and who may upgrade.
    assert_eq!(create_question(&cost(Opening::Burns(100_000_000), false, 0, None)), "Create boo1program: 0.0256 FANG of deposit bonded for good, 0.1 FANG burned to open its account, at most 0.000066 FANG in fees?");
    let q = create_question(&cost(Opening::Adopted, true, 3 * FANG, Some("boo1boss")));
    assert!(q.contains("(and what the initializer's writes bond)") && !q.contains("burned") && q.contains("3 FANG sent to it with its initializer") && q.contains("upgradable by boo1boss with 100800 blocks' notice"), "{q}");
}

/// What `upload` asks: the deposit, which stays bonded for good, or that nothing new is bonded.
#[test]
fn an_upload_asks_for_its_deposit_and_fee() {
    let hash = "ab".repeat(32);
    assert_eq!(upload_question(&hash, 59_100_000, 66_000), "Upload ababababab…, with 0.0591 FANG of deposit bonded for good (v0 never gives it back) and at most 0.000066 FANG in fees?");
    let refresh = upload_question(&hash, 0, 66_000);
    assert!(refresh.contains("only refreshes what's stored") && !refresh.contains("bonded for good"), "{refresh}");
}

/// What `call` prints before it signs: the program and the version digest whole (all 64 hex, never a
/// prefix), the most the fee can be, and without `--storage-limit` that the call can't bond new storage
/// (a method that writes then traps on it and still pays its fee); with one, nothing of the sort.
#[test]
fn a_call_says_its_digest_its_fee_and_its_storage_limit() {
    let digest = "ab12".repeat(16);
    let said = call_lines("boo1program", &digest, 2_593_570, 10_032_600, false);
    assert_eq!(said[0], format!("calling boo1program at version {digest}"), "the whole digest it signs");
    assert!(said[1].contains("fees at most 0.00259357 FANG") && said[1].contains("10032600 fuel reserved"), "{said:?}");
    assert!(said[2].contains("storage limit 0: this call can't bond new storage; if the method writes, pass --storage-limit FANG"), "{said:?}");
    assert_eq!(said.len(), 3);
    let given = call_lines("boo1program", &digest, 2_593_570, 10_032_600, true);
    assert_eq!(given.len(), 2, "no advisory once a limit was given: {given:?}");
    // `call --help` says the same about the default.
    use clap::CommandFactory;
    let mut cli = crate::Cli::command();
    let help = cli.find_subcommand_mut("program").unwrap().find_subcommand_mut("call").unwrap().render_long_help().to_string();
    assert!(help.contains("Default 0, which consents to no new storage") && help.contains("traps `storage_limit` and still pays its fee"), "{help}");
}

/// A transaction that left no receipt (dropped by the node, or still pending after the wait) is only
/// "submitted": no check mark, which would claim it ran.
#[test]
fn a_transaction_with_no_receipt_is_only_submitted() {
    assert_eq!(outcome_lines("abc123", None), vec!["submitted abc123 (not final yet)".to_string()]);
    assert!(outcome_lines("abc123", None).iter().all(|l| !l.contains('✓')));
    let ran = json!({ "summary": "alice called counter", "program": { "outcome": { "ok": true }, "facts": { "fee": {}, "fuel": {}, "storage": { "bonded": "0", "refunded": "0", "bytes_delta": 0 }, "balance_changes": [] }, "claims": {} }, "fee_display": "0.1 FANG" });
    assert!(outcome_lines("abc123", Some(&ran))[0].starts_with("✓ alice called counter"));
}

// ------------------------------------------------------------------------------- a node that lies

/// A node in front of `upstream` that changes what it answers: every request is forwarded as it
/// came, and `lie` may rewrite an answer's JSON (it's given the path). What a hostile or broken RPC
/// could say, in front of a real chain, so the rest of the answers hold together (the genesis the
/// CLI pins is the real one).
async fn lying_node(upstream: String, lie: impl Fn(&str, &mut Value) + Send + Sync + 'static) -> String {
    use axum::body::Body;
    use axum::extract::Request;
    use axum::http::StatusCode;
    use axum::response::IntoResponse as _;
    let lie = Arc::new(lie);
    let app = axum::Router::new().fallback(move |req: Request| {
        let (upstream, lie) = (upstream.clone(), lie.clone());
        async move {
            let (parts, body) = req.into_parts();
            let path = parts.uri.path().to_string();
            let target = format!("{upstream}{}", parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or(""));
            let bytes = axum::body::to_bytes(body, 8 << 20).await.unwrap_or_default();
            let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes()).unwrap();
            let res = reqwest::Client::new().request(method, target).header("content-type", "application/json").body(bytes.to_vec()).send().await;
            let Ok(res) = res else { return StatusCode::BAD_GATEWAY.into_response() };
            let status = StatusCode::from_u16(res.status().as_u16()).unwrap();
            let mut answer = res.bytes().await.unwrap_or_default().to_vec();
            if let Ok(mut v) = serde_json::from_slice::<Value>(&answer) {
                lie(&path, &mut v);
                answer = serde_json::to_vec(&v).unwrap();
            }
            (status, [("content-type", "application/json")], Body::from(answer)).into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rpc = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    rpc
}

/// A node that lies to `blockula program` gets a refusal in words and no signature, never a panic:
/// `pause` against a status height of 2^64 − 1 or one the chain can't have reached by the clock (the
/// height is checked before any arithmetic on it); `call` and `create --bind` against a version
/// digest that's short, long, uppercase, not hex, or multi-byte (it's 64 lowercase hex before it's
/// printed or signed). Each command is also run through a node that tells the truth, so what
/// refuses is the lie.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_that_lies_gets_a_refusal_and_no_signature() {
    let s = served("cli-lies").await;
    let wasm = s.dir.join("counter.wasm");
    let iface = s.dir.join("counter.iface.json");
    std::fs::write(&wasm, include_bytes!("../../vm/tests/programs/counter.wasm")).unwrap();
    std::fs::write(&iface, include_str!("../../vm/tests/programs/counter.iface.json")).unwrap();
    let (wasm_hash, interface_hash) = (hex::encode(digest::wasm_hash(include_bytes!("../../vm/tests/programs/counter.wasm"))), hex::encode(digest::interface_hash(include_bytes!("../../vm/tests/programs/counter.iface"))));
    s.run(ProgramCmd::Upload { code: wasm, interface: iface, storage_limit: None, yes: true, signer: s.signer() }).await.unwrap();
    let user = s.user.address_with("boo");
    let nonce = s.get(&format!("/account/{user}")).await["next_nonce"].as_u64().unwrap();
    let program = digest::program_address("boo", &crypto::address_bytes(&user).unwrap(), nonce);
    let create = |signer: Signer, bindings: Vec<String>| ProgramCmd::Create { wasm_hash: wasm_hash.clone(), interface_hash: interface_hash.clone(), init: None, input: None, value: None, fuel: None, storage_limit: None, authority: None, delay: None, policy: None, accepts_transfers: false, bindings, yes: true, signer };
    s.run(create(s.signer(), vec![])).await.unwrap();
    let via = |rpc: &str| Signer { key: Some(s.user_key.clone()), from: None, rpc: rpc.to_string(), pin: ChainPin::default() };
    let call = |signer: Signer| ProgramCmd::Call { address: program.clone(), method: "increment".into(), input: None, value: None, fuel: None, storage_limit: Some("1".into()), bindings: vec![], yes: true, signer };
    let nonce_of = |s: &Served| {
        let (rpc, user) = (s.rpc.clone(), user.clone());
        async move { reqwest::get(format!("{rpc}/api/account/{user}")).await.unwrap().json::<Value>().await.unwrap()["next_nonce"].as_u64().unwrap() }
    };

    // The control: a node that tells the truth, through the same kind of forwarder, is served.
    let honest = lying_node(s.rpc.clone(), |_, _| {}).await;
    s.run(call(via(&honest))).await.unwrap();
    let signed = nonce_of(&s).await;

    // A status height no chain has reached, to `pause`: refused by the clock rule, before `+ 1` could
    // overflow. (A validator's key is asked for first, so the right one is given.)
    for (what, height) in [("2^64 - 1", json!(u64::MAX)), ("5,000,000 past the clock", json!(5_000_000_000u64))] {
        let lie = lying_node(s.rpc.clone(), move |path, v| {
            if path == "/api/status" {
                v["height"] = height.clone();
            }
        })
        .await;
        let e = s.run(ProgramCmd::Pause { key: s.validator_key.clone(), yes: true, rpc: lie.clone(), pin: ChainPin::default() }).await.unwrap_err().to_string();
        assert!(e.contains(&format!("{lie} reports height")) && e.contains("can't be past block") && e.contains("Nothing was signed"), "{what}: {e}");
        // `call` refuses the same lie the same way.
        let e = s.run(call(via(&lie))).await.unwrap_err().to_string();
        assert!(e.contains("reports height") && e.contains("can't be past block"), "{what}: {e}");
    }
    assert_eq!(nonce_of(&s).await, signed, "nothing was signed");

    // A version digest that isn't 64 lowercase hex, reported for the program being called or bound.
    let program_path = format!("/api/program/{program}");
    for bad in ["é".to_string(), "é".repeat(40), "ab".repeat(31) + "a", "ab".repeat(33), "AB".repeat(32), "xy".repeat(32), String::new()] {
        let (p, b) = (program_path.clone(), bad.clone());
        let lie = lying_node(s.rpc.clone(), move |path, v| {
            if path == p {
                v["version_digest"] = json!(b);
            }
        })
        .await;
        let e = s.run(call(via(&lie))).await.unwrap_err().to_string();
        assert!(e.contains("version digest as") && e.contains("isn't 64 lowercase hex characters") && e.contains("Nothing was signed"), "call, digest {bad:?}: {e}");
        let e = s.run(create(via(&lie), vec![program.clone()])).await.unwrap_err().to_string();
        assert!(e.contains("isn't 64 lowercase hex characters"), "create --bind, digest {bad:?}: {e}");
        assert!(e.len() < 400, "what the node said is cut short: {} bytes", e.len());
    }
    // No digest at all.
    let p = program_path.clone();
    let lie = lying_node(s.rpc.clone(), move |path, v| {
        if path == p {
            v.as_object_mut().unwrap().remove("version_digest");
        }
    })
    .await;
    let e = s.run(call(via(&lie))).await.unwrap_err().to_string();
    assert!(e.contains("doesn't report") && e.contains("version digest"), "{e}");
    assert_eq!(nonce_of(&s).await, signed, "nothing was signed for any of them");
}
