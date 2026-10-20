//! The executor end to end, on every engine (`EngineKind::ALL`): each scenario runs cold on a
//! fresh block engine and again warm on the same one (identical receipts and fuel), and every
//! engine must agree on the outcome, return data, events and diff (fuel may differ: each engine
//! has its own cost model). `cargo test --test exec fuel_table -- --nocapture` prints the fuel.

use blockula_vm::abi::{flags, status};
use blockula_vm::engine::{Address, BlockEngine, EngineKind, Limit, Misuse, Trap, WasmTrap};
use blockula_vm::exec::{Call, Entry, FailCode, Receipt, TxLimits, execute};
use blockula_vm::host::View;
use blockula_vm::meter::V1;
use blockula_vm::testing::{MemHost, UploadError, addr, strip_custom_sections};
use blockula_vm::validate::Rule;

const PRELUDE: &str = r#"
  (import "blockula" "input_len_v1" (func $input_len (result i32)))
  (import "blockula" "input_read_v1" (func $input_read (param i32)))
  (import "blockula" "return_data_v1" (func $return_data (param i32 i32)))
  (import "blockula" "abort_v1" (func $abort (param i32 i32 i32)))
  (import "blockula" "debug_log_v1" (func $debug_log (param i32 i32)))
  (import "blockula" "storage_remove_v1" (func $storage_remove (param i32 i32) (result i32)))
  (import "blockula" "self_address_v1" (func $self_address (param i32)))
  (import "blockula" "block_height_v1" (func $block_height (result i64)))
  (import "blockula" "storage_read_v1" (func $storage_read (param i32 i32 i32 i32) (result i32)))
  (import "blockula" "storage_write_v1" (func $storage_write (param i32 i32 i32 i32 i32)))
  (import "blockula" "value_v1" (func $value (result i64)))
  (import "blockula" "caller_v1" (func $caller (param i32)))
  (import "blockula" "self_balance_v1" (func $self_balance (result i64)))
  (import "blockula" "transfer_v1" (func $transfer (param i32 i64 i32) (result i32)))
  (import "blockula" "call_v1" (func $call (param i32 i32 i32 i32 i32 i32 i64 i64) (result i32)))
  (import "blockula" "try_call_v1" (func $try_call (param i32 i32 i32 i32 i32 i32 i64 i64) (result i32)))
  (import "blockula" "last_return_v1" (func $last_return (param i32)))
  (import "blockula" "last_return_len_v1" (func $last_return_len (result i32)))
  (import "blockula" "last_abort_code_v1" (func $last_abort_code (result i32)))
  (import "blockula" "emit_v1" (func $emit (param i32 i32 i32 i32)))
  (memory (export "memory") 1 1)
"#;

/// A test program: the prelude and `body`, without the `name` section `wat` writes (uploads have
/// no custom sections: `MemHost` deploys through upload validation).
fn module(body: &str) -> Vec<u8> {
    strip_custom_sections(&wat::parse_str(format!("(module {PRELUDE} {body})")).expect("valid wat"))
}

/// A counter: `incr` adds 1 to the u64 under key "n" and returns it; `reset` removes it;
/// `sponsor` writes key "s" with the program paying the deposit; `ctx` returns its context.
fn counter() -> Vec<u8> {
    module(
        r#"
        (data (i32.const 0) "n")
        (data (i32.const 16) "s")
        (func (export "bk_reset")
          (i32.store (i32.const 32) (call $storage_remove (i32.const 0) (i32.const 1)))
          (call $return_data (i32.const 32) (i32.const 4)))
        (func (export "bk_sponsor")
          (call $storage_write (i32.const 16) (i32.const 1) (i32.const 8) (i32.const 8) (i32.const 1)))
        (func (export "bk_ctx")
          (call $self_address (i32.const 100))
          (call $caller (i32.const 132))
          (i64.store (i32.const 164) (call $value))
          (i64.store (i32.const 172) (call $block_height))
          (i64.store (i32.const 180) (call $self_balance))
          (call $debug_log (i32.const 100) (i32.const 88))
          (call $return_data (i32.const 100) (i32.const 88)))
        (func (export "bk_incr")
          (if (i32.eq (call $storage_read (i32.const 0) (i32.const 1) (i32.const 8) (i32.const 8)) (i32.const -1))
            (then (i64.store (i32.const 8) (i64.const 0))))
          (i64.store (i32.const 8) (i64.add (i64.load (i32.const 8)) (i64.const 1)))
          (call $storage_write (i32.const 0) (i32.const 1) (i32.const 8) (i32.const 8) (i32.const 0))
          (call $return_data (i32.const 8) (i32.const 8)))"#,
    )
}

/// A vault: `pay` transfers from its own balance (input: to32 ‖ amount u64 ‖ flags i32), emits
/// "paid" with the status and returns it.
fn vault() -> Vec<u8> {
    module(
        r#"
        (data (i32.const 200) "paid")
        (func (export "bk_pay")
          (call $input_read (i32.const 0))
          (i32.store (i32.const 100) (call $transfer (i32.const 0) (i64.load (i32.const 32)) (i32.load (i32.const 40))))
          (call $emit (i32.const 200) (i32.const 4) (i32.const 100) (i32.const 4))
          (call $return_data (i32.const 100) (i32.const 4)))"#,
    )
}

/// A proxy: `go` calls (mode 0) or try_calls (mode 1) another program, per its input (see
/// `proxy_input`), then writes key "after" = the call's result, so tests see it ran on. Call mode
/// returns the callee's return data; try mode returns status ‖ `last_abort_code` ‖
/// `last_return_len` ‖ the first 8 bytes of `last_return` (each 4 bytes LE but the last).
fn proxy() -> Vec<u8> {
    module(
        r#"
        (data (i32.const 300) "after")
        (func (export "bk_go") (local $len i32) (local $m i32) (local $r i32)
          (local.set $len (call $input_len))
          (call $input_read (i32.const 1024))
          (local.set $m (i32.load (i32.const 1076)))
          (if (i32.eqz (i32.load (i32.const 1072)))
            (then
              (local.set $r (call $call (i32.const 1024) (i32.const 0) (i32.const 1080) (local.get $m)
                (i32.add (i32.const 1080) (local.get $m)) (i32.sub (local.get $len) (i32.add (i32.const 56) (local.get $m)))
                (i64.load (i32.const 1056)) (i64.load (i32.const 1064)))))
            (else
              (local.set $r (call $try_call (i32.const 1024) (i32.const 0) (i32.const 1080) (local.get $m)
                (i32.add (i32.const 1080) (local.get $m)) (i32.sub (local.get $len) (i32.add (i32.const 56) (local.get $m)))
                (i64.load (i32.const 1056)) (i64.load (i32.const 1064))))))
          (i32.store (i32.const 39988) (local.get $r))
          (i32.store (i32.const 39992) (call $last_abort_code))
          (i32.store (i32.const 39996) (call $last_return_len))
          (call $last_return (i32.const 40000))
          (call $storage_write (i32.const 300) (i32.const 5) (i32.const 39988) (i32.const 4) (i32.const 0))
          (if (i32.eqz (i32.load (i32.const 1072)))
            (then (call $return_data (i32.const 40000) (local.get $r)))
            (else (call $return_data (i32.const 39988) (i32.const 20)))))"#,
    )
}

/// A leaf with one method per behavior under test.
fn leaf() -> Vec<u8> {
    module(
        r#"
        (data (i32.const 500) "got")
        (data (i32.const 510) "x")
        (data (i32.const 520) "big")
        (data (i32.const 600) "boom\e2\80\ae")
        (data (i32.const 620) "dying")
        (func (export "bk_recv")
          (i64.store (i32.const 0) (call $value))
          (call $caller (i32.const 8))
          (call $storage_write (i32.const 500) (i32.const 3) (i32.const 0) (i32.const 40) (i32.const 0))
          (call $return_data (i32.const 0) (i32.const 8)))
        (func (export "bk_abort")
          (call $storage_write (i32.const 510) (i32.const 1) (i32.const 510) (i32.const 1) (i32.const 0))
          (call $emit (i32.const 620) (i32.const 5) (i32.const 0) (i32.const 0))
          (call $abort (i32.const 7) (i32.const 600) (i32.const 7)))
        (func (export "bk_trap")
          (call $storage_write (i32.const 510) (i32.const 1) (i32.const 510) (i32.const 1) (i32.const 0))
          unreachable)
        (func (export "bk_burn")
          (call $storage_write (i32.const 510) (i32.const 1) (i32.const 510) (i32.const 1) (i32.const 0))
          (loop $l (br $l)))
        (func (export "bk_write")
          (call $storage_write (i32.const 520) (i32.const 3) (i32.const 1000) (i32.const 1000) (i32.const 0)))
        (func (export "bk_noop"))"#,
    )
}

/// Pure Wasm work, for the engines' fuel side by side: a loop of arithmetic, calls to an empty
/// function, calls to a function with 200 locals, a 64-way `br_table`, loads and stores, and
/// unbounded recursion (the 1,024-frame limit).
fn compute() -> Vec<u8> {
    let locals = "i64 ".repeat(200);
    let blocks: String = (0..64).map(|i| format!("(block $b{i} ")).collect();
    let targets: String = (0..64).map(|i| format!("$b{i} ")).collect();
    let ends = ")".repeat(64);
    module(&format!(
        r#"
        (func $empty)
        (func $many (param i64) (result i64) (local {locals}) (local.get 0))
        (func $recurse (call $recurse))
        (func (export "bk_loop") (local $i i32) (local $acc i64)
          (loop $l
            (local.set $acc (i64.add (local.get $acc) (i64.extend_i32_u (local.get $i))))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br_if $l (i32.lt_u (local.get $i) (i32.const 10000)))))
        (func (export "bk_calls") (local $i i32)
          (loop $l
            (call $empty)
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br_if $l (i32.lt_u (local.get $i) (i32.const 1000)))))
        (func (export "bk_locals") (local $i i32)
          (loop $l
            (drop (call $many (i64.const 1)))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br_if $l (i32.lt_u (local.get $i) (i32.const 1000)))))
        (func (export "bk_brtable") (local $i i32)
          (loop $l
            {blocks} (br_table {targets} (i32.and (local.get $i) (i32.const 63))) {ends}
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br_if $l (i32.lt_u (local.get $i) (i32.const 1000)))))
        (func (export "bk_mem") (local $i i32)
          (loop $l
            (i64.store (i32.const 0) (i64.add (i64.load (i32.const 0)) (i64.const 1)))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br_if $l (i32.lt_u (local.get $i) (i32.const 1000)))))
        (func (export "bk_recurse") (call $recurse))"#
    ))
}

fn proxy_input(callee: Address, value: u64, fuel: u64, try_call: bool, method: &str, inner: &[u8]) -> Vec<u8> {
    let mut v = callee.to_vec();
    v.extend(value.to_le_bytes());
    v.extend(fuel.to_le_bytes());
    v.extend(u32::from(try_call).to_le_bytes());
    v.extend((method.len() as u32).to_le_bytes());
    v.extend(method.as_bytes());
    v.extend(inner);
    v
}

fn signer() -> Address {
    addr("signer")
}

/// A host with the signer funded and the four programs deployed at their labels.
fn world() -> MemHost {
    let mut host = MemHost::new();
    host.fund(signer(), 1_000_000_000_000).fund(addr("alice"), 5);
    host.deploy(addr("counter"), &counter());
    host.deploy(addr("vault"), &vault());
    host.deploy(addr("a"), &proxy());
    host.deploy(addr("b"), &proxy());
    host.deploy(addr("leaf"), &leaf());
    host.deploy(addr("compute"), &compute());
    host
}

const LIMITS: TxLimits = TxLimits { fuel_limit: 10_000_000, storage_limit: 1_000_000 };

fn call(program: &str, method: &str, input: Vec<u8>, value: u64) -> Call {
    Call { program: addr(program), method: method.into(), input, value }
}

struct Scenario {
    name: &'static str,
    host: MemHost,
    calls: Vec<Call>,
    declared: Vec<Address>,
    limits: TxLimits,
}

impl Scenario {
    fn new(name: &'static str, host: MemHost, calls: Vec<Call>, declared: &[&str]) -> Self {
        Scenario { name, host, calls, declared: declared.iter().map(|d| addr(d)).collect(), limits: LIMITS }
    }
    fn limits(mut self, limits: TxLimits) -> Self {
        self.limits = limits;
        self
    }
}

/// Runs `f` on a thread with the executor thread's stack (4.3: 64 MiB).
fn on_executor_thread<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    std::thread::scope(|s| std::thread::Builder::new().stack_size(64 << 20).spawn_scoped(s, f).unwrap().join().unwrap())
}

fn exec(engine: &dyn BlockEngine, s: &Scenario) -> Receipt {
    execute(engine, &s.host, signer(), &s.calls, &s.declared, s.limits, &V1)
}

/// The scenario on one engine: cold on a fresh block engine, then warm on the same engine. Both
/// must be identical, fuel included (4.3: no fuel may depend on any cache).
fn run_on(kind: EngineKind, s: &Scenario) -> Receipt {
    on_executor_thread(|| {
        let engine = s.host.engine(kind, &V1).unwrap();
        let cold = exec(engine.as_ref(), s);
        let warm = exec(engine.as_ref(), s);
        assert_eq!(cold, warm, "{}: cold and warm differ on {}", s.name, kind.name());
        cold
    })
}

/// The scenario on every engine. Every engine must agree on everything but fuel; a successful
/// diff must conserve FANG when applied. Returns each engine's receipt, the default first.
fn run(s: &Scenario) -> Vec<Receipt> {
    let receipts: Vec<Receipt> = EngineKind::ALL.iter().map(|k| run_on(*k, s)).collect();
    let first = &receipts[0];
    for (kind, r) in EngineKind::ALL.iter().zip(&receipts).skip(1) {
        let same = Receipt { fuel_used: first.fuel_used, ..r.clone() };
        assert_eq!(first, &same, "{}: {} disagrees with {}", s.name, kind.name(), EngineKind::ALL[0].name());
    }
    if first.ok {
        let mut after = s.host.clone();
        after.apply(&first.diff);
        assert_eq!(after.total(), s.host.total(), "{}: FANG not conserved", s.name);
    } else {
        assert_eq!(first.diff, Default::default(), "{}: a failed transaction changes nothing", s.name);
    }
    receipts
}

fn trapped(r: &Receipt) -> (Address, Trap) {
    match &r.fail {
        Some(FailCode::Trapped { program, trap }) => (*program, trap.clone()),
        other => panic!("expected a trap, got {other:?}"),
    }
}

fn balance(r: &Receipt, account: Address) -> i128 {
    r.diff.balances.iter().find(|(a, _)| *a == account).map_or(0, |(_, d)| *d)
}

fn stored(host: &MemHost, program: &str, key: &[u8]) -> Option<Entry> {
    host.storage(&addr(program), key)
}

// --- the scenarios (also the fuel table's rows) ---

fn counter_tx() -> Scenario {
    Scenario::new("counter", world(), vec![call("counter", "incr", vec![], 0)], &[])
}

fn pay(to: Address, value: u64, amount: u64, flags: i32) -> Vec<Call> {
    let mut input = to.to_vec();
    input.extend(amount.to_le_bytes());
    input.extend(flags.to_le_bytes());
    vec![call("vault", "pay", input, value)]
}

fn pay_existing() -> Scenario {
    Scenario::new("value + transfer", world(), pay(addr("alice"), 1_000, 400, 0), &[])
}

fn pay_missing() -> Scenario {
    Scenario::new("transfer to a missing account", world(), pay(addr("newbie"), 1_000, 400, 0), &[])
}

fn pay_missing_deduct() -> Scenario {
    let fee = world().creation_fee;
    Scenario::new("transfer opening (OPEN_DEDUCT)", world(), pay(addr("newbie"), fee + 1_000, fee + 500, flags::OPEN_DEDUCT), &[])
}

fn nested_value() -> Scenario {
    let input = proxy_input(addr("leaf"), 300, 0, false, "recv", &[]);
    Scenario::new("nested call with value", world(), vec![call("a", "go", input, 1_000)], &["leaf"])
}

fn reentrancy() -> Scenario {
    let inner = proxy_input(addr("a"), 0, 0, false, "go", &proxy_input(addr("leaf"), 0, 0, false, "noop", &[]));
    let input = proxy_input(addr("b"), 0, 0, false, "go", &inner);
    Scenario::new("reentrancy A -> B -> A", world(), vec![call("a", "go", input, 0)], &["b", "leaf"])
}

/// A chain of `proxies` proxies ending at the leaf: `proxies + 1` frames.
fn chain(name: &'static str, proxies: usize) -> Scenario {
    let mut host = world();
    let names: Vec<String> = (1..=proxies).map(|i| format!("p{i}")).collect();
    for n in &names {
        host.deploy(addr(n), &proxy());
    }
    let mut input = proxy_input(addr("leaf"), 0, 0, false, "noop", &[]);
    for n in names.iter().skip(1).rev() {
        input = proxy_input(addr(n), 0, 0, false, "go", &input);
    }
    let mut declared: Vec<&str> = names.iter().map(String::as_str).collect();
    declared.push("leaf");
    Scenario::new(name, host, vec![call("p1", "go", input, 0)], &declared)
}

fn unbound(try_call: bool) -> Scenario {
    let input = proxy_input(addr("leaf"), 0, 0, try_call, "noop", &[]);
    let name = if try_call { "try_call an undeclared program" } else { "call an undeclared program" };
    Scenario::new(name, world(), vec![call("a", "go", input, 0)], &[])
}

fn out_of_fuel() -> Scenario {
    Scenario::new("out of fuel", world(), vec![call("leaf", "burn", vec![], 0)], &[])
        .limits(TxLimits { fuel_limit: 100_000, storage_limit: 1_000_000 })
}

/// The leaf's `write` with fuel for its instantiation and its entry block, not its
/// `storage_write`: the host function runs out.
fn host_out_of_fuel() -> Scenario {
    let leaf_info = blockula_vm::engine::CodeInfo::read(&leaf()).unwrap();
    let fuel_limit = V1.instantiate_fuel(leaf_info.pages, leaf_info.data_bytes) + 500;
    Scenario::new("host function out of fuel", world(), vec![call("leaf", "write", vec![], 0)], &[])
        .limits(TxLimits { fuel_limit, storage_limit: 1_000_000 })
}

fn try_host_out_of_fuel(share: u64) -> Scenario {
    let input = proxy_input(addr("leaf"), 0, share, true, "write", &[]);
    Scenario::new("try_call: callee's host function out of its share", world(), vec![call("a", "go", input, 0)], &["leaf"])
}

fn try_fail(method: &'static str) -> Scenario {
    let input = proxy_input(addr("leaf"), 50, 0, true, method, &[]);
    let name = if method == "abort" { "try_call an aborting callee" } else { "try_call a trapping callee" };
    Scenario::new(name, world(), vec![call("a", "go", input, 100)], &["leaf"])
}

fn batch() -> Scenario {
    let calls = vec![call("counter", "incr", vec![], 0), call("counter", "incr", vec![], 0), call("leaf", "noop", vec![], 0)];
    Scenario::new("a batch of three calls", world(), calls, &[])
}

fn work(method: &'static str) -> Scenario {
    Scenario::new(method, world(), vec![call("compute", method, vec![], 0)], &[])
}

fn all_scenarios() -> Vec<Scenario> {
    vec![
        work("loop"),
        work("calls"),
        work("locals"),
        work("brtable"),
        work("mem"),
        work("recurse"),
        counter_tx(),
        batch(),
        pay_existing(),
        pay_missing(),
        pay_missing_deduct(),
        nested_value(),
        reentrancy(),
        chain("depth 8 (7 proxies + leaf)", 7),
        chain("depth 9 (8 proxies + leaf)", 8),
        unbound(false),
        unbound(true),
        out_of_fuel(),
        host_out_of_fuel(),
        try_host_out_of_fuel(1_000),
        try_fail("abort"),
        try_fail("trap"),
    ]
}

// --- the tests ---

#[test]
fn counter_persists_across_two_transactions() {
    let first = counter_tx();
    let r1 = &run(&first)[0];
    assert!(r1.ok, "{r1:?}");
    assert_eq!(r1.returns, vec![1u64.to_le_bytes().to_vec()]);
    // The signer bonded the entry's deposit: 100 drops × (1 key byte + 8 value bytes).
    assert_eq!(balance(r1, signer()), -900);
    assert_eq!(r1.diff.escrow, 900);
    let mut host = first.host.clone();
    host.apply(&r1.diff);
    assert_eq!(stored(&host, "counter", b"n").unwrap().value, 1u64.to_le_bytes());

    let second = Scenario { host: host.clone(), ..counter_tx() };
    let r2 = &run(&second)[0];
    assert!(r2.ok, "{r2:?}");
    assert_eq!(r2.returns, vec![2u64.to_le_bytes().to_vec()]);
    // The overwrite fits the entry's capacity: no deposit moves.
    assert!(r2.diff.balances.is_empty(), "{:?}", r2.diff.balances);
    host.apply(&r2.diff);
    let entry = stored(&host, "counter", b"n").unwrap();
    assert_eq!((entry.value, entry.payer, entry.deposit), (2u64.to_le_bytes().to_vec(), signer(), 900));
}

#[test]
fn removing_refunds_the_payer() {
    let mut host = world();
    host.apply(&run(&counter_tx())[0].diff);
    let reset = Scenario::new("reset", host.clone(), vec![call("counter", "reset", vec![], 0)], &[]);
    let r = &run(&reset)[0];
    assert!(r.ok, "{r:?}");
    assert_eq!(r.returns, vec![1i32.to_le_bytes().to_vec()]);
    assert_eq!((balance(r, signer()), r.diff.escrow), (900, -900));
    host.apply(&r.diff);
    assert!(stored(&host, "counter", b"n").is_none());
    assert_eq!(host.escrow, 0);
    let again = Scenario::new("reset again", host, vec![call("counter", "reset", vec![], 0)], &[]);
    assert_eq!(run(&again)[0].returns, vec![0i32.to_le_bytes().to_vec()]);
}

#[test]
fn a_sponsored_write_is_paid_by_the_program() {
    // Unfunded, the program can't bond.
    let r = &run(&Scenario::new("sponsor, unfunded", world(), vec![call("counter", "sponsor", vec![], 0)], &[]))[0];
    assert_eq!(trapped(r), (addr("counter"), Trap::Insufficient));
    // Paid from value received in the same transaction; storage_limit 0 doesn't apply to it.
    let s = Scenario::new("sponsor", world(), vec![call("counter", "sponsor", vec![], 1_000)], &[])
        .limits(TxLimits { fuel_limit: 1_000_000, storage_limit: 0 });
    let r = &run(&s)[0];
    assert!(r.ok, "{r:?}");
    assert_eq!((balance(r, signer()), balance(r, addr("counter")), r.diff.escrow), (-1_000, 100, 900));
    assert_eq!(r.diff.storage[0].entry.as_ref().unwrap().payer, addr("counter"));
}

#[test]
fn context_reads() {
    let r = &run(&Scenario::new("context", world(), vec![call("counter", "ctx", vec![], 77)], &[]))[0];
    assert!(r.ok, "{r:?}");
    let mut expect = addr("counter").to_vec();
    expect.extend(signer());
    expect.extend(77u64.to_le_bytes());
    expect.extend(1u64.to_le_bytes());
    expect.extend(77u64.to_le_bytes());
    assert_eq!(r.returns, vec![expect]);
}

#[test]
fn a_batch_shares_one_meter_and_one_overlay() {
    let r = &run(&batch())[0];
    assert!(r.ok, "{r:?}");
    assert_eq!(r.returns, vec![1u64.to_le_bytes().to_vec(), 2u64.to_le_bytes().to_vec(), vec![]]);
}

#[test]
fn storage_past_the_limit_traps() {
    let s = counter_tx().limits(TxLimits { fuel_limit: 1_000_000, storage_limit: 899 });
    let r = &run(&s)[0];
    assert_eq!(trapped(r), (addr("counter"), Trap::StorageLimit));
}

#[test]
fn value_and_transfer_conserve() {
    let s = pay_existing();
    let r = &run(&s)[0];
    assert!(r.ok, "{r:?}");
    assert_eq!(r.returns, vec![status::OK.to_le_bytes().to_vec()]);
    assert_eq!(balance(r, signer()), -1_000);
    assert_eq!(balance(r, addr("vault")), 600);
    assert_eq!(balance(r, addr("alice")), 400);
    assert_eq!(r.diff.balances.iter().map(|(_, d)| d).sum::<i128>(), 0);
    assert_eq!(r.events.len(), 1);
    assert_eq!((r.events[0].program, r.events[0].name.as_str()), (addr("vault"), "paid"));
}

#[test]
fn transfer_to_a_missing_account_needs_a_flag() {
    let r = &run(&pay_missing())[0];
    assert!(r.ok, "{r:?}");
    assert_eq!(r.returns, vec![status::ERR_TRANSFER.to_le_bytes().to_vec()]);
    assert_eq!(balance(r, addr("vault")), 1_000);
    assert!(r.diff.opened.is_empty());

    let s = pay_missing_deduct();
    let fee = s.host.creation_fee;
    let r = &run(&s)[0];
    assert!(r.ok, "{r:?}");
    assert_eq!(r.returns, vec![status::OK.to_le_bytes().to_vec()]);
    assert_eq!(r.diff.opened, vec![addr("newbie")]);
    assert_eq!(r.diff.burned, u128::from(fee));
    assert_eq!(balance(r, addr("newbie")), 500);
    assert_eq!(balance(r, addr("vault")), 500);
    let mut host = s.host.clone();
    host.apply(&r.diff);
    assert_eq!(host.balance(&addr("newbie")), Some(500));
}

#[test]
fn nested_call_moves_value_and_returns_data() {
    let r = &run(&nested_value())[0];
    assert!(r.ok, "{r:?}");
    // The leaf returned its value_v1; the proxy passed it through.
    assert_eq!(r.returns, vec![300u64.to_le_bytes().to_vec()]);
    let leaf_deposit = 100 * (3 + 40);
    let a_deposit = 100 * (5 + 4);
    assert_eq!(balance(r, signer()), -1_000 - leaf_deposit - a_deposit);
    assert_eq!(balance(r, addr("a")), 700);
    assert_eq!(balance(r, addr("leaf")), 300);
    let mut host = world();
    host.apply(&r.diff);
    let got = stored(&host, "leaf", b"got").unwrap().value;
    assert_eq!(got[..8], 300u64.to_le_bytes());
    assert_eq!(got[8..], addr("a"), "the callee sees its caller");
    assert_eq!(stored(&host, "a", b"after").unwrap().value, 8i32.to_le_bytes(), "call returned the return length");
}

#[test]
fn reentrancy_traps() {
    let r = &run(&reentrancy())[0];
    assert_eq!(trapped(r), (addr("b"), Trap::Reentrancy));
}

#[test]
fn depth_eight_runs_and_nine_traps() {
    let r = &run(&chain("depth 8", 7))[0];
    assert!(r.ok, "{r:?}");
    let r = &run(&chain("depth 9", 8))[0];
    assert_eq!(trapped(r), (addr("p8"), Trap::CallDepth));
}

#[test]
fn an_undeclared_callee_is_unbound() {
    let r = &run(&unbound(false))[0];
    assert_eq!(trapped(r), (addr("a"), Trap::UnboundProgram));
    let r = &run(&unbound(true))[0];
    assert!(r.ok, "{r:?}");
    assert_eq!(r.returns[0][..4], status::ERR_FAULT.to_le_bytes());
}

#[test]
fn out_of_fuel_uses_the_limit_and_applies_nothing() {
    let s = out_of_fuel();
    for r in run(&s) {
        assert_eq!(trapped(&r), (addr("leaf"), Trap::OutOfFuel));
        assert_eq!(r.fuel_used, s.limits.fuel_limit);
        assert!(!r.ok && r.diff == Default::default() && r.events.is_empty());
    }
}

#[test]
fn a_host_function_runs_out_of_fuel() {
    let s = host_out_of_fuel();
    for r in run(&s) {
        assert_eq!(trapped(&r), (addr("leaf"), Trap::OutOfFuel));
        assert_eq!(r.fuel_used, s.limits.fuel_limit);
    }
    // Under a try_call share: the callee's whole share is spent, the caller runs on.
    let small = run(&try_host_out_of_fuel(1_000));
    let large = run(&try_host_out_of_fuel(2_000));
    for (k, (a, b)) in EngineKind::ALL.iter().zip(small.iter().zip(&large)) {
        assert!(a.ok && b.ok, "{a:?} {b:?}");
        assert_eq!(a.returns[0][..4], status::ERR_FAULT.to_le_bytes());
        assert_eq!(b.fuel_used - a.fuel_used, 1_000, "{}: the share is spent either way", k.name());
    }
    let mut host = world();
    host.apply(&small[0].diff);
    assert!(stored(&host, "leaf", b"big").is_none());
    assert!(stored(&host, "a", b"after").is_some());
}

#[test]
fn try_call_drops_a_failing_callee_and_runs_on() {
    let r = &run(&try_fail("abort"))[0];
    assert!(r.ok, "{r:?}");
    // The message as the receipt would show it: "boom" and U+FFFD for the bidi override, 7 bytes.
    let message = "boom\u{FFFD}".as_bytes();
    let mut expect = status::ERR_ABORT.to_le_bytes().to_vec();
    expect.extend(7i32.to_le_bytes());
    expect.extend((message.len() as u32).to_le_bytes());
    expect.extend(message);
    expect.push(0);
    assert_eq!(r.returns, vec![expect], "status, last_abort_code, last_return_len, then the sanitized message");
    // The callee's write and the 50 drops it was sent are gone; the caller's write stays.
    assert_eq!(balance(r, addr("leaf")), 0);
    assert_eq!(balance(r, addr("a")), 100);
    let mut host = world();
    host.apply(&r.diff);
    assert!(stored(&host, "leaf", b"x").is_none());
    assert_eq!(stored(&host, "a", b"after").unwrap().value, status::ERR_ABORT.to_le_bytes());
    assert!(r.events.is_empty(), "a frame that rolls back drops its events");

    let r = &run(&try_fail("trap"))[0];
    assert!(r.ok, "{r:?}");
    assert_eq!(r.returns[0][..4], status::ERR_FAULT.to_le_bytes());
    assert_eq!(r.returns[0][4..], [0; 16], "no abort code and nothing to read after a fault");
}

/// `try_call` to a callee that returns: `OK`, no abort code, and its return data's length and
/// bytes through `last_return_len` and `last_return`.
#[test]
fn try_call_reports_the_return_length() {
    let input = proxy_input(addr("leaf"), 25, 0, true, "recv", &[]);
    let s = Scenario::new("try_call a returning callee", world(), vec![call("a", "go", input, 100)], &["leaf"]);
    let r = &run(&s)[0];
    assert!(r.ok, "{r:?}");
    let mut expect = status::OK.to_le_bytes().to_vec();
    expect.extend(0i32.to_le_bytes());
    expect.extend(8u32.to_le_bytes());
    expect.extend(25u64.to_le_bytes());
    assert_eq!(r.returns, vec![expect]);
    assert_eq!((balance(r, addr("a")), balance(r, addr("leaf"))), (75, 25));
}

/// `storage_read_v1`'s buffer rule (`abi::HOST_FNS`, "Reading into a buffer"): absent is −1 and
/// copies nothing; otherwise the first `min(len, cap)` bytes are copied and the full length
/// returned, whatever `cap` is (0 probes the length; a `cap` far past memory is fine when the
/// value fits). Also: nothing to read after the last call before a frame's first call.
#[test]
fn storage_read_copies_what_fits_and_returns_the_length() {
    let reader = module(
        r#"
        (data (i32.const 0) "k")
        (func (export "bk_put")
          (i64.store (i32.const 8) (i64.const 0x0807060504030201))
          (call $storage_write (i32.const 0) (i32.const 1) (i32.const 8) (i32.const 8) (i32.const 0)))
        (func (export "bk_get")
          (call $input_read (i32.const 32))
          (i64.store (i32.const 64) (i64.const -1))
          (i64.store (i32.const 72) (i64.const -1))
          (i32.store (i32.const 60) (call $storage_read (i32.const 0) (i32.const 1) (i32.const 64) (i32.load (i32.const 32))))
          (call $return_data (i32.const 60) (i32.const 20)))
        (func (export "bk_fresh")
          (i32.store (i32.const 0) (call $last_return_len))
          (i32.store (i32.const 4) (call $last_abort_code))
          (call $return_data (i32.const 0) (i32.const 8)))"#,
    );
    let mut host = world();
    host.deploy(addr("reader"), &reader);
    let get = |cap: u32| call("reader", "get", cap.to_le_bytes().to_vec(), 0);
    let calls = vec![get(8), call("reader", "put", vec![], 0), get(4), get(0), get(8), get(u32::MAX), call("reader", "fresh", vec![], 0)];
    let r = &run(&Scenario::new("storage_read into a buffer", host, calls, &[]))[0];
    assert!(r.ok, "{r:?}");
    let got = |len: i32, copied: usize| {
        let mut v = len.to_le_bytes().to_vec();
        v.extend((1..=8u8).take(copied));
        v.extend(std::iter::repeat_n(0xff, 16 - copied));
        v
    };
    assert_eq!(r.returns[0], [(-1i32).to_le_bytes().to_vec(), vec![0xff; 16]].concat(), "absent: nothing copied");
    assert_eq!(r.returns[1], Vec::<u8>::new());
    assert_eq!(r.returns[2], got(8, 4), "cap 4: the first 4 bytes, and the full length");
    assert_eq!(r.returns[3], got(8, 0), "cap 0: a length probe");
    assert_eq!(r.returns[4], got(8, 8));
    assert_eq!(r.returns[5], got(8, 8), "a cap past memory copies what's there");
    assert_eq!(r.returns[6], vec![0; 8], "no call yet: nothing to read, no abort code");
}

#[test]
fn call_traps_with_the_callees_failure() {
    let input = proxy_input(addr("leaf"), 0, 0, false, "abort", &[]);
    let s = Scenario::new("call an aborting callee", world(), vec![call("a", "go", input, 0)], &["leaf"]);
    let r = &run(&s)[0];
    // The abort message is sanitized: U+202E (a bidi override) is replaced.
    assert_eq!(trapped(r), (addr("leaf"), Trap::Abort { code: 7, message: "boom\u{FFFD}".into() }));
}

/// A program with `memory.grow` never reaches the executor: upload validation refuses it
/// (`Rule::MemoryGrow`), so no scenario has one. What the engines would do with one is an engine
/// fact, in `tests/engines.rs`.
#[test]
fn memory_grow_never_uploads() {
    let grower = module(
        r#"
        (func (export "bk_grow")
          (call $input_read (i32.const 0))
          (i32.store (i32.const 0) (memory.grow (i32.load (i32.const 0))))
          (call $return_data (i32.const 0) (i32.const 4)))"#,
    );
    match MemHost::new().upload(&grower) {
        Err(UploadError::Invalid(e)) => assert_eq!(e.rule, Rule::MemoryGrow, "{e}"),
        other => panic!("expected Rule::MemoryGrow, got {other:?}"),
    }
}

/// The executor's backstop: a panic under `execute` (here in a view read) is a `RuntimeFault`
/// with the full fuel charged; a rebuilt engine then runs like a fresh one.
#[test]
fn a_panic_is_a_runtime_fault() {
    struct Panicky(MemHost);
    impl View for Panicky {
        fn height(&self) -> u64 {
            self.0.height()
        }
        fn balance(&self, a: &Address) -> Option<u64> {
            self.0.balance(a)
        }
        fn code_of(&self, p: &Address) -> Option<[u8; 32]> {
            self.0.code_of(p)
        }
        fn storage(&self, _: &Address, _: &[u8]) -> Option<Entry> {
            panic!("injected fault")
        }
        fn storage_price(&self) -> u64 {
            self.0.storage_price()
        }
        fn account_creation_fee(&self) -> u64 {
            self.0.account_creation_fee()
        }
    }
    let s = counter_tx();
    for kind in EngineKind::ALL {
        on_executor_thread(|| {
            let engine = s.host.engine(*kind, &V1).unwrap();
            let r = execute(engine.as_ref(), &Panicky(s.host.clone()), signer(), &s.calls, &[], s.limits, &V1);
            assert_eq!((r.ok, r.fail.clone(), r.fuel_used), (false, Some(FailCode::RuntimeFault), s.limits.fuel_limit));
            let fresh = exec(s.host.engine(*kind, &V1).unwrap().as_ref(), &s);
            assert!(fresh.ok);
            // Informative: the engine that unwound still runs like a fresh one (the rule rebuilds it anyway).
            assert_eq!(exec(engine.as_ref(), &s), fresh, "{}", kind.name());
        });
    }
}

/// soroban-wasmi deadlocks if a module is translated on its engine while a call runs. Every
/// declared module is loaded before execution, so a cross-program call only instantiates and
/// calls inside the host call: it must complete, three deep, on every engine.
#[test]
fn cross_program_calls_with_every_module_loaded_up_front() {
    let input = proxy_input(addr("b"), 0, 0, false, "go", &proxy_input(addr("leaf"), 0, 0, false, "recv", &[]));
    let s = Scenario::new("a -> b -> leaf", world(), vec![call("a", "go", input, 0)], &["b", "leaf"]);
    let (done, wait) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = run_on(EngineKind::Soroban, &s);
        let _ = done.send(r);
    });
    let r = wait.recv_timeout(std::time::Duration::from_secs(30)).expect("no deadlock");
    assert!(r.ok, "{r:?}");
}

#[test]
fn misuse_traps() {
    // Return data past MAX_RETURN.
    let body = r#"(func (export "bk_big") (call $return_data (i32.const 0) (i32.const 4097)))
                  (func (export "bk_oob") (call $return_data (i32.const 65535) (i32.const 2)))"#;
    let mut host = world();
    host.deploy(addr("bad"), &module(body));
    let r = &run(&Scenario::new("return too long", host.clone(), vec![call("bad", "big", vec![], 0)], &[]))[0];
    assert_eq!(trapped(r), (addr("bad"), Trap::Misuse(Misuse::Length)));
    let r = &run(&Scenario::new("out of bounds", host.clone(), vec![call("bad", "oob", vec![], 0)], &[]))[0];
    assert_eq!(trapped(r), (addr("bad"), Trap::Misuse(Misuse::MemoryAccess)));
    let r = &run(&Scenario::new("no such method", host, vec![call("bad", "nope", vec![], 0)], &[]))[0];
    assert_eq!(trapped(r), (addr("bad"), Trap::NoMethod));
}

#[test]
fn recursion_past_the_limit_traps() {
    let r = &run(&work("recurse"))[0];
    assert_eq!(trapped(r), (addr("compute"), Trap::Wasm(WasmTrap::StackOverflow)));
}

/// Interpreted loops must not grow the native stack: ten million fuel of a tight loop on a
/// 2 MiB thread, every engine.
#[test]
fn a_long_loop_runs_on_a_small_stack() {
    let s = out_of_fuel().limits(TxLimits { fuel_limit: 10_000_000, storage_limit: 1_000_000 });
    for kind in EngineKind::ALL {
        let r = std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(2 << 20)
                .spawn_scoped(scope, || exec(s.host.engine(*kind, &V1).unwrap().as_ref(), &s))
                .unwrap()
                .join()
                .unwrap()
        });
        assert_eq!(trapped(&r).1, Trap::OutOfFuel, "{}", kind.name());
    }
}

/// Cold versus warm, with history: one block engine runs every scenario in turn, and each
/// receipt must equal the one a fresh engine gives (fuel included).
#[test]
fn cold_and_warm_agree_with_history() {
    for kind in EngineKind::ALL {
        on_executor_thread(|| {
            let warm = world().engine(*kind, &V1).unwrap();
            for s in all_scenarios() {
                let cold = exec(s.host.engine(*kind, &V1).unwrap().as_ref(), &s);
                assert_eq!(exec(warm.as_ref(), &s), cold, "{}: {}", kind.name(), s.name);
            }
        });
    }
}

/// 4.4's per-transaction bounds: 256 distinct keys written, 64 events.
#[test]
fn per_transaction_bounds_trap() {
    let storm = module(
        r#"
        (data (i32.const 100) "e")
        (func (export "bk_keys") (local $i i32) (local $n i32)
          (call $input_read (i32.const 0))
          (local.set $n (i32.load (i32.const 0)))
          (loop $l
            (i32.store (i32.const 8) (local.get $i))
            (call $storage_write (i32.const 8) (i32.const 4) (i32.const 8) (i32.const 4) (i32.const 0))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br_if $l (i32.lt_u (local.get $i) (local.get $n)))))
        (func (export "bk_events") (local $i i32) (local $n i32)
          (call $input_read (i32.const 0))
          (local.set $n (i32.load (i32.const 0)))
          (loop $l
            (call $emit (i32.const 100) (i32.const 1) (i32.const 0) (i32.const 4))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br_if $l (i32.lt_u (local.get $i) (local.get $n)))))"#,
    );
    let mut host = world();
    host.deploy(addr("storm"), &storm);
    let go = |method: &'static str, n: u32| {
        let s = Scenario::new(method, host.clone(), vec![call("storm", method, n.to_le_bytes().to_vec(), 0)], &[]);
        run(&s).remove(0)
    };
    let r = go("keys", 256);
    assert!(r.ok, "{:?}", r.fail);
    assert_eq!(r.diff.storage.len(), 256);
    assert_eq!(trapped(&go("keys", 257)), (addr("storm"), Trap::Limit(Limit::KeysWritten)));
    let r = go("events", 64);
    assert!(r.ok && r.events.len() == 64, "{:?}", r.fail);
    assert_eq!(trapped(&go("events", 65)), (addr("storm"), Trap::Limit(Limit::Events)));
}

#[test]
fn every_scenario_agrees_across_engines() {
    for s in all_scenarios() {
        run(&s);
    }
}

/// Prints each engine's fuel for every scenario (`-- --nocapture`).
#[test]
fn fuel_table() {
    let kinds = EngineKind::ALL;
    let mut out = format!("| scenario | outcome | {} |\n|---|---|{}\n", kinds.iter().map(|k| k.name()).collect::<Vec<_>>().join(" | "), "---:|".repeat(kinds.len()));
    for s in &all_scenarios() {
        let receipts: Vec<Receipt> = kinds.iter().map(|k| run_on(*k, s)).collect();
        let outcome = |r: &Receipt| match &r.fail {
            None => "ok".to_string(),
            Some(FailCode::Trapped { trap: Trap::Abort { code, .. }, .. }) => format!("Abort({code})"),
            Some(FailCode::Trapped { trap, .. }) => format!("{trap:?}"),
            Some(f) => format!("{f:?}"),
        };
        let outcomes: Vec<String> = receipts.iter().map(outcome).collect();
        let outcome = if outcomes.iter().all(|o| *o == outcomes[0]) { outcomes[0].clone() } else { outcomes.join(" / ") };
        let fuel: Vec<String> = receipts.iter().map(|r| r.fuel_used.to_string()).collect();
        out += &format!("| {} | {} | {} |\n", s.name, outcome, fuel.join(" | "));
    }
    println!("{out}");
}
