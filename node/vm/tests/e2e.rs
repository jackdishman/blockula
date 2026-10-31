//! The committed guests (`tests/programs/*.wasm`, built by `guests/build.sh`) end to end on the
//! real host, on every engine (`EngineKind::ALL`, wasmi 0.38 too with `--features ref038`):
//!
//! * **Upload.** Code reaches `MemHost` only through `MemHost::upload`: upload validation (4.2),
//!   then a test translation on a throwaway soroban-wasmi engine (`upload::check`), then storage
//!   by hash.
//! * **Execution.** Every transaction runs through `exec::execute` on a fresh block engine loaded
//!   with exactly its declared programs' code (cold), then again on that engine (warm): the two
//!   receipts must be identical, fuel included. The engines must agree on everything but fuel:
//!   outcome, return data, events and diff.
//! * **Audit.** Every receipt is checked from the outside (`execute` runs `Diff::check` itself):
//!   a failure changes nothing; a success conserves FANG, debits only the signer (at most what it
//!   sent and bonded) and programs the transaction could reach, and burns one creation fee per
//!   account it opened. Then its diff is applied, as commit would.
//!
//! These are the guests agent's stand-in host checks (once in `tests/guests.rs`) on the real
//! host, plus the router's cross-program calls and an engine's lifecycle (per block as 4.3 has
//! it, and per transaction as the node runs it).
//! `cargo test --test e2e fuel_table -- --nocapture` prints each call's fuel on every engine.

use std::collections::BTreeSet;
use std::path::Path;

use blockula_vm::abi::status;
use blockula_vm::config::MIN_NEW_ACCOUNT;
use blockula_vm::engine::{Address, EngineKind, TrapKind, new_block_engine};
use blockula_vm::exec::{ANY_VERSION, Call, Diff, Event, FailCode, Receipt, TxLimits, execute};
use blockula_vm::host::{MethodKind, View};
use blockula_vm::meter::SCHEDULE_V0;
use blockula_vm::storage::capacity;
use blockula_vm::testing::{MemHost, addr, strip_custom_sections};
use blockula_vm::upload::Refusal;
use blockula_vm::validate::{Rule, validate};

const FANG: u64 = 1_000_000_000;
const LIMITS: TxLimits = TxLimits { fuel_limit: 10_000_000, storage_limit: FANG };
const PROGRAMS: &[&str] = &["counter", "token", "escrow", "router"];
/// Drops per byte a deposit bonds (`MemHost`'s price, far below its soft cap).
const PRICE: i128 = 100;

fn wasm(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/programs/{name}.wasm"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (run guests/build.sh)", path.display()))
}

fn le(n: u64) -> Vec<u8> {
    n.to_le_bytes().to_vec()
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// A `CallProgram` call; `Chain::tx` binds it to its program's version in force, as a wallet
/// would after previewing it.
fn call(program: &str, method: &str, input: Vec<u8>, value: u64) -> Call {
    Call { program: addr(program), version_digest: ANY_VERSION, method: method.into(), input, value, kind: MethodKind::Call }
}

/// The deposit `key` ‖ a `value_len`-byte value bonds: its capacity (`storage::capacity`) at
/// `PRICE`.
fn deposit(key_len: usize, value_len: usize) -> i128 {
    PRICE * i128::from(capacity(key_len, value_len))
}

/// Runs `f` on a thread with the executor thread's stack (4.3: 64 MiB).
fn on_executor_thread<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    std::thread::scope(|s| std::thread::Builder::new().stack_size(64 << 20).spawn_scoped(s, f).unwrap().join().unwrap())
}

fn trapped(r: &Receipt) -> (Address, TrapKind) {
    match &r.fail {
        Some(FailCode::Trapped { program, kind }) => (*program, *kind),
        other => panic!("expected a trap, got {other:?}"),
    }
}

/// The program and code of an abort.
fn aborted(r: &Receipt) -> (Address, u32) {
    match &r.fail {
        Some(FailCode::Aborted { program, code, .. }) => (*program, *code),
        other => panic!("expected an abort, got {other:?}"),
    }
}

/// An abort's program, code and message.
fn abort(program: &str, code: u32, message: &str) -> Option<FailCode> {
    Some(FailCode::Aborted { program: addr(program), code, message: message.into() })
}

fn delta(r: &Receipt, account: Address) -> i128 {
    r.diff.balances.iter().find(|(a, _)| *a == account).map_or(0, |(_, d)| *d)
}

fn outcome(r: &Receipt) -> String {
    match &r.fail {
        None => "ok".into(),
        Some(FailCode::Aborted { code, .. }) => format!("Abort({code})"),
        Some(FailCode::Trapped { kind, .. }) => format!("{kind:?}"),
        Some(f) => format!("{f:?}"),
    }
}

/// 4.4's commit checks from the outside, plus what `Diff::check` can't see (see the module doc).
fn audit(host: &MemHost, signer: Address, calls: &[Call], reachable: &BTreeSet<Address>, r: &Receipt) {
    if !r.ok() {
        assert_eq!((&r.diff, r.events.len(), r.returns.len()), (&Diff::default(), 0, 0), "a failed transaction changes nothing");
        return;
    }
    let d = &r.diff;
    let sum = d.balances.iter().map(|(_, x)| x).sum::<i128>() + d.escrow + d.burned as i128;
    assert_eq!(sum, 0, "FANG not conserved: {d:?}");
    let sent: u128 = calls.iter().map(|c| u128::from(c.value)).sum();
    // An upper bound on what the signer bonded: every entry it pays for that this diff writes.
    let bonded: u128 = d.storage.iter().filter_map(|c| c.entry.as_ref()).filter(|e| e.payer == signer).map(|e| u128::from(e.deposit)).sum();
    for (account, change) in &d.balances {
        if *change >= 0 {
            continue;
        }
        if *account == signer {
            assert!(change.unsigned_abs() <= sent + bonded, "the signer paid {change}, more than it sent ({sent}) and bonded ({bonded})");
        } else {
            assert!(reachable.contains(account) && host.programs.contains_key(account), "debited an account that may not be: {d:?}");
        }
    }
    for account in &d.opened {
        assert_eq!(host.balance(account), None, "opened an account that existed");
    }
    assert_eq!(d.burned, d.opened.len() as u128 * u128::from(host.creation_fee), "one creation fee burned per account opened");
    let mut after = host.clone();
    after.apply(d);
    assert_eq!(after.total(), host.total(), "FANG not conserved once applied");
}

/// A `MemHost` with the four guests uploaded and created at their names, and a log of each
/// transaction's fuel on every engine.
struct Chain {
    host: MemHost,
    log: Vec<(String, String, Vec<u64>)>,
}

impl Chain {
    fn new(accounts: &[(&str, u64)]) -> Self {
        let mut host = MemHost::new();
        for p in PROGRAMS {
            // `deploy` is `upload` (validation, test translations) then `create`.
            host.deploy(addr(p), &wasm(p));
        }
        for (who, balance) in accounts {
            host.fund(addr(who), *balance);
        }
        Chain { host, log: Vec::new() }
    }

    /// One transaction in a block of its own, on every engine: see the module doc.
    fn tx(&mut self, label: &str, signer: &str, calls: Vec<Call>, declared: &[&str]) -> Receipt {
        let signer = addr(signer);
        let calls: Vec<Call> = calls.into_iter().map(|c| self.host.bind(c)).collect();
        let declared: Vec<Address> = declared.iter().map(|d| addr(d)).collect();
        let reachable: BTreeSet<Address> = calls.iter().map(|c| c.program).chain(declared.iter().copied()).collect();
        let programs: Vec<Address> = reachable.iter().copied().collect();
        let bindings = MemHost::any_version(&declared);
        let host = &self.host;
        let receipts: Vec<Receipt> = EngineKind::ALL
            .iter()
            .map(|&kind| {
                on_executor_thread(|| {
                    let (engine, _) = host.engine_for(kind, &SCHEDULE_V0, &programs).expect("uploaded code loads");
                    let cold = execute(engine.as_ref(), host, signer, &calls, &bindings, LIMITS, &SCHEDULE_V0).expect("no node fault");
                    let warm = execute(engine.as_ref(), host, signer, &calls, &bindings, LIMITS, &SCHEDULE_V0).expect("no node fault");
                    assert_eq!(cold, warm, "{label}: cold and warm differ on {}", kind.name());
                    cold
                })
            })
            .collect();
        let first = receipts[0].clone();
        for (kind, r) in EngineKind::ALL.iter().zip(&receipts).skip(1) {
            let same = Receipt { fuel_used: first.fuel_used, ..r.clone() };
            assert_eq!(same, first, "{label}: {} disagrees with {}", kind.name(), EngineKind::ALL[0].name());
        }
        audit(host, signer, &calls, &reachable, &first);
        self.log.push((label.into(), outcome(&first), receipts.iter().map(|r| r.fuel_used).collect()));
        if first.ok() {
            self.host.apply(&first.diff);
        }
        first
    }

    fn balance(&self, who: &str) -> Option<u64> {
        self.host.balance(&addr(who))
    }

    /// `program`'s stored value under `key`.
    fn stored(&self, program: &str, key: &[u8]) -> Option<Vec<u8>> {
        self.host.storage(&addr(program), key).map(|e| e.value)
    }

    /// Every key `program` has stored.
    fn keys(&self, program: &str) -> Vec<Vec<u8>> {
        self.host.storage.keys().filter(|(p, _)| *p == addr(program)).map(|(_, k)| k.clone()).collect()
    }

    fn table(&self) -> String {
        let kinds = EngineKind::ALL;
        let mut out = format!(
            "| call | outcome | {} |\n|---|---|{}\n",
            kinds.iter().map(|k| k.name()).collect::<Vec<_>>().join(" | "),
            "---:|".repeat(kinds.len())
        );
        for (label, outcome, fuel) in &self.log {
            out += &format!("| {label} | {outcome} | {} |\n", fuel.iter().map(u64::to_string).collect::<Vec<_>>().join(" | "));
        }
        out
    }
}

// --- upload ---

/// Every committed guest passes upload validation and the test translation; refused code is stored
/// nowhere. The float and `memory.grow` cases are ones both engines translate
/// (`tests/engines.rs`, `validate.rs`'s tests), so it's validation, run first, that keeps them
/// from the engines.
#[test]
fn code_arrives_only_through_upload_validation() {
    let mut host = MemHost::new();
    for p in PROGRAMS {
        let code = wasm(p);
        let hash = host.upload(&code).expect("no node fault").unwrap_or_else(|e| panic!("{p}: {e}"));
        assert_eq!(host.upload(&code), Ok(Ok(hash)), "{p}: uploading again is a no-op");
        assert_eq!(host.code(&hash), Some(&code[..]));
        assert_eq!(validate(&code).unwrap().exports.len(), match *p {
            "counter" | "router" => 2,
            "escrow" => 3,
            _ => 4,
        });
    }
    let refused = |host: &mut MemHost, code: &[u8], rule: Rule| {
        match host.upload(code) {
            Ok(Err(Refusal::Invalid(e))) => assert_eq!(e.rule, rule, "{e}"),
            other => panic!("expected {rule:?}, got {other:?}"),
        }
        assert_eq!(host.code(blake3::hash(code).as_bytes()), None, "refused code isn't stored");
    };
    // A guest with a `name` section appended: custom sections are refused, not ignored.
    let named = cat(&[&wasm("counter"), &[0x00, 0x05, 0x04], b"name"]);
    refused(&mut host, &named, Rule::CustomSection);
    let program = |body: &str| strip_custom_sections(&wat::parse_str(format!(r#"(module {body} (memory (export "memory") 1 1))"#)).unwrap());
    refused(&mut host, &program(r#"(func (export "bk_x") unreachable (drop (i32.trunc_f32_s)))"#), Rule::Float);
    refused(&mut host, &program(r#"(import "env" "f" (func))"#), Rule::Import);
    refused(&mut host, &program(r#"(func (export "bk_x") (drop (memory.grow (i32.const 0))))"#), Rule::MemoryGrow);
}

#[test]
#[should_panic(expected = "wasn't uploaded")]
fn a_program_needs_uploaded_code() {
    MemHost::new().create(addr("p"), [7; 32]);
}

// --- the guests agent's checks, on the real host ---

#[test]
fn counter_counts_across_transactions() {
    let mut c = Chain::new(&[("alice", FANG)]);
    let get = || vec![call("counter", "get", vec![], 0)];
    let increment = || vec![call("counter", "increment", vec![], 0)];
    assert_eq!(c.tx("counter.get", "alice", get(), &[]).returns, vec![le(0)]);
    let r = c.tx("counter.increment (new entry)", "alice", increment(), &[]);
    assert_eq!(r.returns, vec![le(1)]);
    // The signer bonds the entry: 1 key byte + 8 value bytes.
    assert_eq!((delta(&r, addr("alice")), r.diff.escrow), (-deposit(1, 8), deposit(1, 8)));
    let r = c.tx("counter.increment", "alice", increment(), &[]);
    assert_eq!(r.returns, vec![le(2)]);
    assert!(r.diff.balances.is_empty(), "the overwrite fits the entry: no deposit moves");
    assert_eq!(c.tx("counter.get", "alice", get(), &[]).returns, vec![le(2)]);
    assert_eq!((c.keys("counter"), c.stored("counter", b"n")), (vec![b"n".to_vec()], Some(le(2))));
    let r = c.tx("counter.increment (input)", "alice", vec![call("counter", "increment", b"x".to_vec(), 0)], &[]);
    assert_eq!(aborted(&r), (addr("counter"), 1), "input must be empty");
    assert_eq!(c.tx("counter.get", "alice", get(), &[]).returns, vec![le(2)]);
    assert_eq!(c.balance("alice"), Some(FANG - deposit(1, 8) as u64));
}

#[test]
fn token_moves_balances_and_emits_transfers() {
    let (alice, bob, carol) = (addr("alice"), addr("bob"), addr("carol"));
    let mut c = Chain::new(&[("alice", FANG), ("bob", FANG), ("carol", FANG)]);
    let entry = deposit(33, 8);
    let transfer = |to: &Address, amount: u64| vec![call("token", "transfer", cat(&[to, &le(amount)]), 0)];
    let balance_of = |c: &mut Chain, who: &Address| {
        let r = c.tx("token.balance_of", "bob", vec![call("token", "balance_of", who.to_vec(), 0)], &[]);
        u64::from_le_bytes(r.returns[0].clone().try_into().unwrap())
    };

    assert_eq!(c.tx("token.total_supply", "alice", vec![call("token", "total_supply", vec![], 0)], &[]).returns, vec![le(0)]);
    let r = c.tx("token.init", "bob", vec![call("token", "init", cat(&[&alice, &le(1_000)]), 0)], &[]);
    assert!(r.ok(), "anyone may init, for anyone: {r:?}");
    assert_eq!(delta(&r, bob), -(deposit(1, 8) + entry), "bob bonded the supply and alice's balance");
    let r = c.tx("token.init (again)", "alice", vec![call("token", "init", cat(&[&alice, &le(5)]), 0)], &[]);
    assert_eq!(aborted(&r), (addr("token"), 10));
    assert_eq!(c.tx("token.total_supply", "alice", vec![call("token", "total_supply", vec![], 0)], &[]).returns, vec![le(1_000)]);
    assert_eq!(balance_of(&mut c, &alice), 1_000, "through return data");

    let r = c.tx("token.transfer (new holder)", "alice", transfer(&bob, 300), &[]);
    let event = |from: &Address, to: &Address, amount: u64| Event { program: addr("token"), name: "transfer".into(), data: cat(&[from, to, &le(amount)]) };
    assert_eq!(r.events, vec![event(&alice, &bob, 300)]);
    assert_eq!(delta(&r, alice), -entry, "the signer bonds the new holder's entry");
    assert_eq!((balance_of(&mut c, &alice), balance_of(&mut c, &bob)), (700, 300));

    let r = c.tx("token.transfer (short)", "bob", transfer(&carol, 301), &[]);
    assert_eq!(r.fail, abort("token", 11, "balance short"));
    let r = c.tx("token.transfer (empties)", "bob", transfer(&carol, 300), &[]);
    assert!(r.ok(), "{r:?}");
    assert_eq!(c.stored("token", &cat(&[b"b", &bob])), None, "an emptied balance is removed");
    // Its deposit goes back to whoever paid it (alice, who sent the tokens), never the remover;
    // bob bonds carol's new entry.
    assert_eq!((delta(&r, alice), delta(&r, bob)), (entry, -entry));
    assert_eq!(balance_of(&mut c, &carol), 300);

    let r = c.tx("token.transfer (to oneself)", "alice", transfer(&alice, 700), &[]);
    assert_eq!(r.events, vec![event(&alice, &alice, 700)]);
    // The balance reaches 0 first and is removed (bob paid for it at init), then written anew.
    assert_eq!((delta(&r, alice), delta(&r, bob)), (-entry, entry));
    assert_eq!(balance_of(&mut c, &alice), 700);
    let r = c.tx("token.transfer (nothing)", "alice", transfer(&bob, 0), &[]);
    assert_eq!(r.events.len(), 1);
    assert!(r.diff.balances.is_empty() && r.diff.storage.len() == 1, "{:?}", r.diff);

    let before = c.balance("alice");
    let r = c.tx("token.transfer (value)", "alice", vec![call("token", "transfer", cat(&[&bob, &le(1)]), 5)], &[]);
    assert_eq!(aborted(&r), (addr("token"), 2), "not payable");
    assert_eq!(c.balance("alice"), before, "the value came back");
    let r = c.tx("token.transfer (short input)", "alice", vec![call("token", "transfer", bob.to_vec(), 0)], &[]);
    assert_eq!(aborted(&r), (addr("token"), 1));
    let r = c.tx("token.balance_of (long input)", "alice", vec![call("token", "balance_of", cat(&[&bob, b"x"]), 0)], &[]);
    assert_eq!(aborted(&r), (addr("token"), 1));
    // FANG: everyone's deposits are where the entries are.
    assert_eq!(c.host.escrow as i128, deposit(1, 8) + 2 * entry);
}

fn open(id: u64, seller: &Address) -> Vec<u8> {
    cat(&[&le(id), seller])
}

#[test]
fn escrow_releases_to_the_seller_and_refunds_the_buyer() {
    let (alice, bob, mallory, seller, fresh) = (addr("alice"), addr("bob"), addr("mallory"), addr("seller"), addr("fresh"));
    let mut c = Chain::new(&[("alice", 10 * FANG), ("bob", 0), ("mallory", FANG)]);
    let fee = c.host.creation_fee;
    let deal = deposit(9, 72);
    let escrow = |method: &str, input: Vec<u8>, value: u64| vec![call("escrow", method, input, value)];
    let event = |name: &str, data: Vec<u8>| Event { program: addr("escrow"), name: name.into(), data };

    let r = c.tx("escrow.open", "alice", escrow("open", open(1, &seller), 5 * FANG), &[]);
    assert_eq!(r.events, vec![event("open", cat(&[&le(1), &alice, &seller, &le(5 * FANG)]))]);
    assert_eq!((delta(&r, alice), delta(&r, addr("escrow"))), (-(5 * FANG as i128) - deal, 5 * FANG as i128), "the value and the deal's deposit");
    assert_eq!(c.balance("escrow"), Some(5 * FANG));
    assert_eq!(aborted(&c.tx("escrow.open (id taken)", "mallory", escrow("open", open(1, &mallory), 1), &[])), (addr("escrow"), 21));
    assert_eq!(aborted(&c.tx("escrow.open (no value)", "alice", escrow("open", open(2, &seller), 0), &[])), (addr("escrow"), 20));

    assert_eq!(aborted(&c.tx("escrow.release (not the buyer)", "mallory", escrow("release", le(1), 0), &[])), (addr("escrow"), 23));
    assert_eq!(aborted(&c.tx("escrow.refund (not the seller)", "bob", escrow("refund", le(1), 0), &[])), (addr("escrow"), 24));
    assert_eq!(aborted(&c.tx("escrow.release (value)", "alice", escrow("release", le(1), 1), &[])), (addr("escrow"), 2));
    let r = c.tx("escrow.release (opens the seller)", "alice", escrow("release", le(1), 0), &[]);
    assert_eq!(r.events, vec![event("release", cat(&[&le(1), &seller, &le(5 * FANG)]))]);
    // OPEN_DEDUCT: the new seller pays its own creation fee out of the deal, and it's burned. The
    // program pays exactly the deal; the buyer only gets its deposit back.
    assert_eq!((r.diff.opened.clone(), r.diff.burned), (vec![seller], u128::from(fee)));
    assert_eq!(delta(&r, seller), i128::from(5 * FANG - fee));
    assert_eq!(delta(&r, addr("escrow")), -(5 * FANG as i128));
    assert_eq!(delta(&r, alice), deal);
    assert_eq!((c.balance("seller"), c.balance("escrow"), c.host.burned), (Some(5 * FANG - fee), Some(0), u128::from(fee)));
    assert!(c.keys("escrow").is_empty(), "the settled deal is removed");
    assert_eq!(aborted(&c.tx("escrow.release (no deal)", "alice", escrow("release", le(1), 0), &[])), (addr("escrow"), 22));

    // Refund: the seller sends it back.
    c.tx("escrow.open", "alice", escrow("open", open(2, &bob), 3 * FANG), &[]);
    assert_eq!(aborted(&c.tx("escrow.refund (not the seller)", "alice", escrow("refund", le(2), 0), &[])), (addr("escrow"), 24));
    assert_eq!(aborted(&c.tx("escrow.release (not the buyer)", "bob", escrow("release", le(2), 0), &[])), (addr("escrow"), 23));
    let r = c.tx("escrow.refund", "bob", escrow("refund", le(2), 0), &[]);
    assert_eq!(r.events, vec![event("refund", cat(&[&le(2), &alice, &le(3 * FANG)]))]);
    assert_eq!(delta(&r, alice), 3 * FANG as i128 + deal);
    assert_eq!(c.balance("alice"), Some(5 * FANG));

    // A deal that would leave a new seller less than `MIN_NEW_ACCOUNT` once the creation fee is
    // out can't open it (docs/PROGRAMS-V0.md A30): the release aborts 4, the removal before it
    // rolls back, and the deal can still be refunded once the seller exists.
    c.tx("escrow.open (small)", "mallory", escrow("open", open(3, &fresh), fee + MIN_NEW_ACCOUNT - 1), &[]);
    let kept = c.stored("escrow", &cat(&[b"d", &le(3)]));
    let r = c.tx("escrow.release (smaller than the fee and the minimum)", "mallory", escrow("release", le(3), 0), &[]);
    assert_eq!(r.fail, abort("escrow", 4, "transfer failed"));
    assert_eq!((c.stored("escrow", &cat(&[b"d", &le(3)])), c.balance("fresh")), (kept, None));
    c.host.fund(fresh, 0);
    assert!(c.tx("escrow.refund (small)", "fresh", escrow("refund", le(3), 0), &[]).ok());
    assert_eq!(c.balance("mallory"), Some(FANG));

    assert_eq!(aborted(&c.tx("escrow.open (short input)", "mallory", escrow("open", vec![0; 39], 1), &[])), (addr("escrow"), 1));
    assert_eq!((c.host.escrow, c.host.total()), (0, u128::from(11 * FANG)));
}

// --- the router: cross-program calls ---

/// The router's `send` input: hops ‖ token ‖ to ‖ amount.
fn route(hops: &[&str], to: &str, amount: u64) -> Vec<u8> {
    let mut v: Vec<u8> = hops.iter().flat_map(|h| addr(h)).collect();
    v.extend(addr("token"));
    v.extend(addr(to));
    v.extend(le(amount));
    v
}

/// `try_send`'s return: status ‖ abort code ‖ length ‖ the callee's bytes.
fn tried(status: i32, code: i32, passed: &[u8]) -> Vec<u8> {
    cat(&[&status.to_le_bytes(), &code.to_le_bytes(), &(passed.len() as u32).to_le_bytes(), passed])
}

#[test]
fn router_moves_its_own_tokens_by_call_and_try_call() {
    let (router, carol) = (addr("router"), addr("carol"));
    let mut c = Chain::new(&[("alice", FANG)]);
    c.tx("token.init", "alice", vec![call("token", "init", cat(&[&router, &le(1_000)]), 0)], &[]);
    let transferred = |to: &Address, amount: u64| vec![Event { program: addr("token"), name: "transfer".into(), data: cat(&[&router, to, &le(amount)]) }];

    // `call_v1` with value 0: the token sees the router as its caller. The router reads
    // `balance_of`'s 8 bytes back through `last_return_len` and `last_return`.
    let r = c.tx("router.send (call)", "alice", vec![call("router", "send", route(&[], "carol", 250), 0)], &["token"]);
    assert_eq!(r.returns, vec![le(750)]);
    assert_eq!(r.events, transferred(&carol, 250));
    assert_eq!(delta(&r, addr("alice")), -deposit(33, 8), "the signer bonds carol's entry, two frames down");
    assert_eq!((c.stored("token", &cat(&[b"b", &router])), c.stored("token", &cat(&[b"b", &carol]))), (Some(le(750)), Some(le(250))));

    // The token must be in the declared set.
    let r = c.tx("router.send (token unbound)", "alice", vec![call("router", "send", route(&[], "carol", 1), 0)], &[]);
    assert_eq!(trapped(&r), (router, TrapKind::UnboundProgram));
    // `call` traps the caller with the callee's failure, reported where it began.
    let r = c.tx("router.send (balance short)", "alice", vec![call("router", "send", route(&[], "carol", 10_000), 0)], &["token"]);
    assert_eq!(r.fail, abort("token", 11, "balance short"));
    let r = c.tx("router.send (value)", "alice", vec![call("router", "send", route(&[], "carol", 1), 1)], &["token"]);
    assert_eq!(aborted(&r), (router, 2));
    let r = c.tx("router.send (bad input)", "alice", vec![call("router", "send", vec![0; 73], 0)], &["token"]);
    assert_eq!(aborted(&r), (router, 1));

    // `try_call`: OK, and nothing to read (transfer returns nothing).
    let r = c.tx("router.try_send (ok)", "alice", vec![call("router", "try_send", route(&[], "carol", 50), 0)], &["token"]);
    assert_eq!(r.returns, vec![tried(status::OK, 0, b"")]);
    assert_eq!(r.events, transferred(&carol, 50));
    // The token aborts: the router runs on with its code (`last_abort_code`) and its message
    // (`last_return`); the token's layer is dropped, its event with it.
    let r = c.tx("router.try_send (abort)", "alice", vec![call("router", "try_send", route(&[], "carol", 10_000), 0)], &["token"]);
    assert!(r.ok(), "{r:?}");
    assert_eq!(r.returns, vec![tried(status::ERR_ABORT, 11, b"balance short")]);
    assert!(r.events.is_empty() && r.diff == Diff::default(), "{r:?}");
    // Unbound: one code for every host-level failure, and nothing to read.
    let r = c.tx("router.try_send (unbound)", "alice", vec![call("router", "try_send", route(&[], "carol", 1), 0)], &[]);
    assert_eq!(r.returns, vec![tried(status::ERR_FAULT, 0, b"")]);
    assert_eq!((c.stored("token", &cat(&[b"b", &router])), c.stored("token", &cat(&[b"b", &carol]))), (Some(le(700)), Some(le(300))));
}

/// Routers forwarding to routers: depth 8 (seven routers and the token) runs, depth 9 traps, a
/// router twice on the stack traps, and every hop must be declared. Eight programs, one code.
#[test]
fn routers_forward_up_to_the_depth_limit() {
    let mut c = Chain::new(&[("alice", FANG)]);
    let code = c.host.programs[&addr("router")];
    let hops: Vec<String> = (1..=8).map(|i| format!("r{i}")).collect();
    for h in &hops {
        c.host.create(addr(h), code);
    }
    let r7 = addr("r7");
    c.tx("token.init", "alice", vec![call("token", "init", cat(&[&r7, &le(1_000)]), 0)], &[]);
    let names: Vec<&str> = hops.iter().map(String::as_str).collect();
    let declared = |n: usize| -> Vec<&str> { names[1..n].iter().copied().chain(["token"]).collect() };

    // r1 → … → r7 → token: 8 frames. Each router passes r7's balance back up through last_return.
    let r = c.tx("router.send (depth 8)", "alice", vec![call("r1", "send", route(&names[1..7], "carol", 100), 0)], &declared(7));
    assert_eq!(r.returns, vec![le(900)]);
    assert_eq!(r.events, vec![Event { program: addr("token"), name: "transfer".into(), data: cat(&[&r7, &addr("carol"), &le(100)]) }]);
    // r1 → … → r8 → token would be the ninth frame.
    let r = c.tx("router.send (depth 9)", "alice", vec![call("r1", "send", route(&names[1..8], "carol", 100), 0)], &declared(8));
    assert_eq!(trapped(&r), (addr("r8"), TrapKind::CallDepth));
    // r1 → r2 → r1.
    let r = c.tx("router.send (reentrant)", "alice", vec![call("r1", "send", route(&["r2", "r1"], "carol", 1), 0)], &["r2", "token"]);
    assert_eq!(trapped(&r), (addr("r2"), TrapKind::Reentrancy));
    // r2 isn't declared.
    let r = c.tx("router.send (hop unbound)", "alice", vec![call("r1", "send", route(&["r2"], "carol", 1), 0)], &["token"]);
    assert_eq!(trapped(&r), (addr("r1"), TrapKind::UnboundProgram));
}

// --- an engine per block (4.3), and per transaction ---

/// 4.3's lifecycle with real programs: the escrow's whole flow in two blocks, each on a fresh
/// block engine that loads exactly its transactions' declared code before any of them runs, and
/// is dropped after. Every receipt must equal a fresh engine's for that transaction alone (the
/// engine per transaction the node runs; fuel included: nothing may depend on what an engine ran
/// before), and every engine must end in the same state with the same receipts (fuel aside).
#[test]
fn escrow_flow_on_a_fresh_engine_per_block() {
    let (seller, bob, mallory, fresh) = (addr("seller"), addr("bob"), addr("mallory"), addr("fresh"));
    let start = Chain::new(&[("alice", 10 * FANG), ("bob", 0), ("mallory", FANG)]).host;
    let fee = start.creation_fee;
    let tx = |signer: &str, method: &str, input: Vec<u8>, value: u64| (addr(signer), call("escrow", method, input, value));
    let blocks = [
        vec![
            tx("alice", "open", open(1, &seller), 5 * FANG),
            tx("alice", "open", open(2, &bob), 3 * FANG),
            tx("mallory", "open", open(3, &fresh), fee - 1),
            tx("mallory", "open", open(1, &mallory), 1),
        ],
        vec![
            tx("alice", "release", le(1), 0),
            tx("bob", "refund", le(2), 0),
            tx("mallory", "release", le(3), 0),
            tx("alice", "release", le(1), 0),
        ],
    ];
    let expected = [Ok(()), Ok(()), Ok(()), Err(21), Ok(()), Ok(()), Err(4), Err(22)];
    let token_code = start.programs[&addr("token")];

    let runs: Vec<(MemHost, Vec<Receipt>)> = EngineKind::ALL
        .iter()
        .map(|&kind| {
            on_executor_thread(|| {
                let mut host = start.clone();
                let mut receipts = Vec::new();
                for (height, block) in blocks.iter().enumerate() {
                    host.height = height as u64 + 1;
                    let programs: Vec<Address> = block.iter().map(|(_, call)| call.program).collect();
                    let (engine, load_fuel) = host.engine_for(kind, &SCHEDULE_V0, &programs).unwrap();
                    assert_eq!(load_fuel, SCHEDULE_V0.load_fuel(wasm("escrow").len()), "one distinct code, loaded once");
                    assert!(engine.info(&token_code).is_none(), "undeclared code isn't loaded");
                    for (signer, call) in block {
                        let calls = &[host.bind(call.clone())];
                        let r = execute(engine.as_ref(), &host, *signer, calls, &[], LIMITS, &SCHEDULE_V0).expect("no node fault");
                        let alone = host.engine_for(kind, &SCHEDULE_V0, &programs).unwrap().0;
                        assert_eq!(Ok(&r), execute(alone.as_ref(), &host, *signer, calls, &[], LIMITS, &SCHEDULE_V0).as_ref(), "{}: block {height}", kind.name());
                        audit(&host, *signer, calls, &BTreeSet::from([call.program]), &r);
                        if r.ok() {
                            host.apply(&r.diff);
                        }
                        receipts.push(r);
                    }
                    drop(engine);
                }
                (host, receipts)
            })
        })
        .collect();

    let (host, receipts) = &runs[0];
    let got: Vec<Result<(), u32>> = receipts.iter().map(|r| if r.ok() { Ok(()) } else { Err(aborted(r).1) }).collect();
    assert_eq!(got, expected);
    for (kind, (other, theirs)) in EngineKind::ALL.iter().zip(&runs).skip(1) {
        assert_eq!(other, host, "{}: another end state", kind.name());
        for (a, b) in receipts.iter().zip(theirs) {
            assert_eq!(&Receipt { fuel_used: a.fuel_used, ..b.clone() }, a, "{}", kind.name());
        }
    }
    // Deal 1 released to a seller it opened, deal 2 refunded, deal 3 still open (and refundable).
    let deal = deposit(9, 72) as u64;
    assert_eq!(host.balance(&seller), Some(5 * FANG - fee));
    assert_eq!(host.balance(&addr("alice")), Some(5 * FANG));
    assert_eq!(host.balance(&bob), Some(0));
    assert_eq!(host.balance(&mallory), Some(FANG - (fee - 1) - deal));
    assert_eq!((host.balance(&addr("escrow")), host.escrow, host.burned), (Some(fee - 1), u128::from(deal), u128::from(fee)));
    assert_eq!(host.total(), start.total());
}

// --- measurements ---

/// Prints each call's fuel on every engine and what uploading each guest takes (validation and
/// a test translation per engine, wall time of one run: indicative only). Asserts nothing new.
#[test]
fn fuel_table() {
    let mut c = Chain::new(&[("alice", 10 * FANG), ("bob", FANG)]);
    let (alice, bob, router) = (addr("alice"), addr("bob"), addr("router"));
    c.tx("counter.increment (new entry)", "alice", vec![call("counter", "increment", vec![], 0)], &[]);
    c.tx("counter.increment", "alice", vec![call("counter", "increment", vec![], 0)], &[]);
    c.tx("counter.get", "alice", vec![call("counter", "get", vec![], 0)], &[]);
    c.tx("token.init", "alice", vec![call("token", "init", cat(&[&router, &le(1_000)]), 0)], &[]);
    c.tx("token.balance_of", "alice", vec![call("token", "balance_of", router.to_vec(), 0)], &[]);
    c.tx("router.send (new holder)", "alice", vec![call("router", "send", route(&[], "bob", 10), 0)], &["token"]);
    c.tx("router.send", "alice", vec![call("router", "send", route(&[], "bob", 10), 0)], &["token"]);
    c.tx("router.try_send (ok)", "alice", vec![call("router", "try_send", route(&[], "bob", 10), 0)], &["token"]);
    c.tx("router.try_send (abort)", "alice", vec![call("router", "try_send", route(&[], "bob", 10_000), 0)], &["token"]);
    c.tx("token.transfer", "bob", vec![call("token", "transfer", cat(&[&alice, &le(5)]), 0)], &[]);
    c.tx("escrow.open", "alice", vec![call("escrow", "open", open(1, &addr("seller")), 5 * FANG)], &[]);
    c.tx("escrow.release (opens the seller)", "alice", vec![call("escrow", "release", le(1), 0)], &[]);
    c.tx("escrow.open", "alice", vec![call("escrow", "open", open(2, &bob), FANG)], &[]);
    c.tx("escrow.refund", "bob", vec![call("escrow", "refund", le(2), 0)], &[]);
    let code = c.host.programs[&router];
    let hops: Vec<String> = (2..=7).map(|i| format!("r{i}")).collect();
    for h in &hops {
        c.host.create(addr(h), code);
    }
    // r2 → … → r7 → router (which holds the tokens) → token: 8 frames.
    let mut names: Vec<&str> = hops.iter().map(String::as_str).collect();
    names.push("router");
    let mut declared = names.clone();
    declared.push("token");
    c.tx("router.send (depth 8: 7 routers)", "alice", vec![call("r2", "send", route(&names[1..], "bob", 1), 0)], &declared);
    println!("{}", c.table());

    let mut out = String::from("| guest | bytes | validate µs | load fuel | instantiate fuel |");
    for k in EngineKind::ALL {
        out += &format!(" {} translate µs |", k.name());
    }
    out += &format!("\n|---|---:|---:|---:|---:|{}\n", "---:|".repeat(EngineKind::ALL.len()));
    for p in PROGRAMS {
        let code = wasm(p);
        let t = std::time::Instant::now();
        let info = validate(&code).unwrap();
        let validate_us = t.elapsed().as_micros();
        out += &format!("| {p} | {} | {validate_us} | {} | {} |", code.len(), SCHEDULE_V0.load_fuel(code.len()), SCHEDULE_V0.instantiate_fuel(&info.code));
        for &k in EngineKind::ALL {
            let mut engine = new_block_engine(k, &SCHEDULE_V0).unwrap();
            let t = std::time::Instant::now();
            engine.load([0; 32], &code).unwrap();
            out += &format!(" {} |", t.elapsed().as_micros());
        }
        out += "\n";
    }
    println!("{out}");

    // Wall time per call on a warm block engine: the median of 101 runs, 11 in debug builds
    // (indicative only, and only in release; S3 measures properly). Every frame instantiates a fresh memory: the guests' is 32 pages, and
    // two empty programs show what that costs by itself.
    for pages in [1, 32] {
        let noop = strip_custom_sections(&wat::parse_str(format!(r#"(module (memory (export "memory") {pages} {pages}) (func (export "bk_noop")))"#)).unwrap());
        c.host.deploy(addr(&format!("noop{pages}")), &noop);
    }
    let samples: [(&str, &str, Call, &[&str]); 5] = [
        ("noop, 1 page", "alice", call("noop1", "noop", vec![], 0), &[]),
        ("noop, 32 pages", "alice", call("noop32", "noop", vec![], 0), &[]),
        ("counter.get", "alice", call("counter", "get", vec![], 0), &[]),
        ("token.transfer", "bob", call("token", "transfer", cat(&[&alice, &le(1)]), 0), &[]),
        ("router.send (3 frames)", "alice", call("router", "send", route(&[], "bob", 1), 0), &["token"]),
    ];
    let mut out = format!("| call | {} |\n|---|{}\n", EngineKind::ALL.iter().map(|k| format!("{} µs", k.name())).collect::<Vec<_>>().join(" | "), "---:|".repeat(EngineKind::ALL.len()));
    for (label, signer, call, declared) in &samples {
        let declared = MemHost::any_version(&declared.iter().map(|d| addr(d)).collect::<Vec<_>>());
        let call = c.host.bind(call.clone());
        out += &format!("| {label} |");
        for &kind in EngineKind::ALL {
            let host = &c.host;
            let median = on_executor_thread(|| {
                let engine = host.engine(kind, &SCHEDULE_V0).unwrap();
                let runs = if cfg!(debug_assertions) { 11 } else { 101 };
                let mut times: Vec<u128> = (0..runs)
                    .map(|_| {
                        let t = std::time::Instant::now();
                        let r = execute(engine.as_ref(), host, addr(signer), std::slice::from_ref(&call), &declared, LIMITS, &SCHEDULE_V0).unwrap();
                        let elapsed = t.elapsed().as_nanos();
                        assert!(r.ok(), "{label}: {r:?}");
                        elapsed
                    })
                    .collect();
                times.sort_unstable();
                times[runs / 2]
            });
            out += &format!(" {:.1} |", median as f64 / 1_000.0);
        }
        out += "\n";
    }
    println!("{out}");
}
