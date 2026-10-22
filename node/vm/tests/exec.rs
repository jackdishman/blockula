//! The executor end to end, on every engine (`EngineKind::ALL`): each scenario runs cold on a
//! fresh block engine and again warm on the same one (identical receipts and fuel), and every
//! engine must agree on the outcome, return data, events and diff (fuel may differ: each engine
//! has its own cost model). `cargo test --test exec fuel_table -- --nocapture` prints the fuel.

use blockula_vm::abi::{flags, status};
use blockula_vm::engine::{Address, BlockEngine, CodeInfo, Digest, EngineKind, NodeFault, TrapKind};
use blockula_vm::exec::{ANY_VERSION, Binding, Call, CommitError, Entry, FailCode, MethodInfo, ProgramStorage, Receipt, TxLimits, execute};
use blockula_vm::host::{MethodKind, View};
use blockula_vm::meter::SCHEDULE_V0;
use blockula_vm::storage::capacity;
use blockula_vm::testing::{MemHost, addr, strip_custom_sections};
use blockula_vm::upload::Refusal;
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

/// Drops per byte a deposit bonds (`MemHost`'s price, far below its soft cap).
const PRICE: i128 = 100;

/// The deposit an entry of `key_len` and `value_len` bytes bonds: its capacity at `PRICE`.
fn deposit(key_len: usize, value_len: usize) -> i128 {
    PRICE * i128::from(capacity(key_len, value_len))
}

/// A `CallProgram` call, its version digest left for [`Scenario::new`] to bind.
fn call(program: &str, method: &str, input: Vec<u8>, value: u64) -> Call {
    Call { program: addr(program), version_digest: ANY_VERSION, method: method.into(), input, value, kind: MethodKind::Call }
}

struct Scenario {
    name: &'static str,
    host: MemHost,
    calls: Vec<Call>,
    declared: Vec<Binding>,
    limits: TxLimits,
}

impl Scenario {
    /// A transaction as a wallet signs it: each call bound to its program's version in force
    /// (`MemHost::bind`), `declared` bound at any version.
    fn new(name: &'static str, host: MemHost, calls: Vec<Call>, declared: &[&str]) -> Self {
        let calls = calls.into_iter().map(|c| host.bind(c)).collect();
        let declared: Vec<Address> = declared.iter().map(|d| addr(d)).collect();
        Scenario { name, calls, declared: MemHost::any_version(&declared), host, limits: LIMITS }
    }
    /// A transaction with exactly these calls and bindings, digests as given.
    fn raw(name: &'static str, host: MemHost, calls: Vec<Call>, declared: Vec<Binding>) -> Self {
        Scenario { name, host, calls, declared, limits: LIMITS }
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
    execute(engine, &s.host, signer(), &s.calls, &s.declared, s.limits, &SCHEDULE_V0).unwrap_or_else(|f| panic!("{}: a node fault: {f}", s.name))
}

/// The scenario on one engine: cold on a fresh block engine, then warm on the same engine. Both
/// must be identical, fuel included (4.3: no fuel may depend on any cache).
fn run_on(kind: EngineKind, s: &Scenario) -> Receipt {
    on_executor_thread(|| {
        let engine = s.host.engine(kind, &SCHEDULE_V0).unwrap();
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
    if first.ok() {
        let mut after = s.host.clone();
        after.apply(&first.diff);
        assert_eq!(after.total(), s.host.total(), "{}: FANG not conserved", s.name);
    } else {
        assert_eq!(first.diff, Default::default(), "{}: a failed transaction changes nothing", s.name);
    }
    receipts
}

fn trapped(r: &Receipt) -> (Address, TrapKind) {
    match &r.fail {
        Some(FailCode::Trapped { program, kind }) => (*program, *kind),
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
    let leaf_info = CodeInfo::read(&leaf()).unwrap();
    let fuel_limit = SCHEDULE_V0.instantiate_fuel(&leaf_info) + 500;
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
    assert!(r1.ok(), "{r1:?}");
    assert_eq!(r1.returns, vec![1u64.to_le_bytes().to_vec()]);
    // The signer bonded the entry's deposit: 100 drops × its capacity, 1 key byte + 8 value bytes
    // + 96, rounded up to 128.
    assert_eq!(deposit(1, 8), 12_800);
    assert_eq!(balance(r1, signer()), -12_800);
    assert_eq!(r1.diff.escrow, 12_800);
    assert_eq!(r1.diff.state_bytes, 128);
    let mut host = first.host.clone();
    host.apply(&r1.diff);
    assert_eq!(stored(&host, "counter", b"n").unwrap().value, 1u64.to_le_bytes());

    let second = Scenario { host: host.clone(), ..counter_tx() };
    let r2 = &run(&second)[0];
    assert!(r2.ok(), "{r2:?}");
    assert_eq!(r2.returns, vec![2u64.to_le_bytes().to_vec()]);
    // The overwrite fits the entry's capacity: no deposit moves.
    assert!(r2.diff.balances.is_empty(), "{:?}", r2.diff.balances);
    host.apply(&r2.diff);
    let entry = stored(&host, "counter", b"n").unwrap();
    assert_eq!((entry.value, entry.payer, entry.deposit, entry.capacity), (2u64.to_le_bytes().to_vec(), signer(), 12_800, 128));
}

#[test]
fn removing_refunds_the_payer() {
    let mut host = world();
    host.apply(&run(&counter_tx())[0].diff);
    let reset = Scenario::new("reset", host.clone(), vec![call("counter", "reset", vec![], 0)], &[]);
    let r = &run(&reset)[0];
    assert!(r.ok(), "{r:?}");
    assert_eq!(r.returns, vec![1i32.to_le_bytes().to_vec()]);
    assert_eq!((balance(r, signer()), r.diff.escrow, r.diff.state_bytes), (12_800, -12_800, -128));
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
    assert_eq!(trapped(r), (addr("counter"), TrapKind::Insufficient));
    // Paid from value received in the same transaction; storage_limit 0 doesn't apply to it.
    let s = Scenario::new("sponsor", world(), vec![call("counter", "sponsor", vec![], 20_000)], &[])
        .limits(TxLimits { fuel_limit: 1_000_000, storage_limit: 0 });
    let r = &run(&s)[0];
    assert!(r.ok(), "{r:?}");
    assert_eq!((balance(r, signer()), balance(r, addr("counter")), r.diff.escrow), (-20_000, 7_200, 12_800));
    assert_eq!(r.diff.storage[0].entry.as_ref().unwrap().payer, addr("counter"));
}

#[test]
fn context_reads() {
    let r = &run(&Scenario::new("context", world(), vec![call("counter", "ctx", vec![], 77)], &[]))[0];
    assert!(r.ok(), "{r:?}");
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
    assert!(r.ok(), "{r:?}");
    assert_eq!(r.returns, vec![1u64.to_le_bytes().to_vec(), 2u64.to_le_bytes().to_vec(), vec![]]);
}

#[test]
fn storage_past_the_limit_traps() {
    let s = counter_tx().limits(TxLimits { fuel_limit: 1_000_000, storage_limit: 12_799 });
    let r = &run(&s)[0];
    assert_eq!(trapped(r), (addr("counter"), TrapKind::StorageLimit));
}

#[test]
fn value_and_transfer_conserve() {
    let s = pay_existing();
    let r = &run(&s)[0];
    assert!(r.ok(), "{r:?}");
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
    assert!(r.ok(), "{r:?}");
    assert_eq!(r.returns, vec![status::ERR_TRANSFER.to_le_bytes().to_vec()]);
    assert_eq!(balance(r, addr("vault")), 1_000);
    assert!(r.diff.opened.is_empty());

    let s = pay_missing_deduct();
    let fee = s.host.creation_fee;
    let r = &run(&s)[0];
    assert!(r.ok(), "{r:?}");
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
    assert!(r.ok(), "{r:?}");
    // The leaf returned its value_v1; the proxy passed it through.
    assert_eq!(r.returns, vec![300u64.to_le_bytes().to_vec()]);
    let leaf_deposit = deposit(3, 40);
    let a_deposit = deposit(5, 4);
    assert_eq!(balance(r, signer()), -1_000 - leaf_deposit - a_deposit);
    assert_eq!(balance(r, addr("a")), 700);
    assert_eq!(balance(r, addr("leaf")), 300);
    let mut host = world();
    host.apply(&r.diff);
    let got = stored(&host, "leaf", b"got").unwrap().value;
    assert_eq!(got[..8], 300u64.to_le_bytes());
    assert_eq!(got[8..], addr("a"), "the callee sees its caller");
    assert_eq!(stored(&host, "a", b"after").unwrap().value, 8i32.to_le_bytes(), "call returned the return length");
    // Each program's storage totals, for the node's records (`MemHost::apply` checked them
    // against the entries).
    let mut programs = vec![
        ProgramStorage { program: addr("a"), items: 1, bytes: 128, deposit: a_deposit },
        ProgramStorage { program: addr("leaf"), items: 1, bytes: 192, deposit: leaf_deposit },
    ];
    programs.sort_by_key(|p| p.program);
    assert_eq!((r.diff.programs.clone(), r.diff.state_bytes), (programs, 320));
}

#[test]
fn reentrancy_traps() {
    let r = &run(&reentrancy())[0];
    assert_eq!(trapped(r), (addr("b"), TrapKind::Reentrancy));
}

#[test]
fn depth_eight_runs_and_nine_traps() {
    let r = &run(&chain("depth 8", 7))[0];
    assert!(r.ok(), "{r:?}");
    let r = &run(&chain("depth 9", 8))[0];
    assert_eq!(trapped(r), (addr("p8"), TrapKind::CallDepth));
}

#[test]
fn an_undeclared_callee_is_unbound() {
    let r = &run(&unbound(false))[0];
    assert_eq!(trapped(r), (addr("a"), TrapKind::UnboundProgram));
    let r = &run(&unbound(true))[0];
    assert!(r.ok(), "{r:?}");
    assert_eq!(r.returns[0][..4], status::ERR_FAULT.to_le_bytes());
}

#[test]
fn out_of_fuel_uses_the_limit_and_applies_nothing() {
    let s = out_of_fuel();
    for r in run(&s) {
        assert_eq!(trapped(&r), (addr("leaf"), TrapKind::OutOfFuel));
        assert_eq!(r.fuel_used, s.limits.fuel_limit);
        assert!(!r.ok() && r.diff == Default::default() && r.events.is_empty());
    }
}

#[test]
fn a_host_function_runs_out_of_fuel() {
    let s = host_out_of_fuel();
    for r in run(&s) {
        assert_eq!(trapped(&r), (addr("leaf"), TrapKind::OutOfFuel));
        assert_eq!(r.fuel_used, s.limits.fuel_limit);
    }
    // Under a try_call share: the callee's whole share is spent, the caller runs on.
    let small = run(&try_host_out_of_fuel(1_000));
    let large = run(&try_host_out_of_fuel(2_000));
    for (k, (a, b)) in EngineKind::ALL.iter().zip(small.iter().zip(&large)) {
        assert!(a.ok() && b.ok(), "{a:?} {b:?}");
        assert_eq!(a.returns[0][..4], status::ERR_FAULT.to_le_bytes());
        assert_eq!(b.fuel_used - a.fuel_used, 1_000, "{}: the share is spent either way", k.name());
    }
    let mut host = world();
    host.apply(&small[0].diff);
    assert!(stored(&host, "leaf", b"big").is_none());
    assert!(stored(&host, "a", b"after").is_some());
}

// After any `OutOfFuel` that fails the transaction, nested or not, `fuel_used` is the whole
// `fuel_limit`: a callee out of the smaller share its caller passed to `call_v1` too.
#[test]
fn a_callee_out_of_its_share_fails_with_the_whole_limit() {
    let input = proxy_input(addr("leaf"), 0, 50_000, false, "burn", &[]);
    let s = Scenario::new("call_v1: callee out of its share", world(), vec![call("a", "go", input, 0)], &["leaf"]);
    for r in run(&s) {
        assert_eq!(trapped(&r), (addr("leaf"), TrapKind::OutOfFuel));
        assert_eq!(r.fuel_used, s.limits.fuel_limit);
    }
}

#[test]
fn try_call_drops_a_failing_callee_and_runs_on() {
    let r = &run(&try_fail("abort"))[0];
    assert!(r.ok(), "{r:?}");
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
    assert!(r.ok(), "{r:?}");
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
    assert!(r.ok(), "{r:?}");
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
    assert!(r.ok(), "{r:?}");
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
    assert_eq!(r.fail, Some(FailCode::Aborted { program: addr("leaf"), code: 7, message: "boom\u{FFFD}".into() }));
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
        Ok(Err(Refusal::Invalid(e))) => assert_eq!(e.rule, Rule::MemoryGrow, "{e}"),
        other => panic!("expected Rule::MemoryGrow, got {other:?}"),
    }
}

/// The executor's backstop: a panic under `execute` (here in a view read) is a node fault, never
/// a receipt; a rebuilt engine then runs like a fresh one.
#[test]
fn a_panic_is_a_node_fault() {
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
        fn version_digest(&self, p: &Address) -> Option<Digest> {
            self.0.version_digest(p)
        }
        fn method(&self, p: &Address, name: &str) -> Option<MethodInfo> {
            self.0.method(p, name)
        }
        fn storage(&self, _: &Address, _: &[u8]) -> Option<Entry> {
            panic!("injected fault")
        }
        fn state_bytes(&self) -> u64 {
            self.0.state_bytes()
        }
        fn storage_price(&self, used: u64) -> Option<u64> {
            self.0.storage_price(used)
        }
        fn account_creation_fee(&self) -> u64 {
            self.0.account_creation_fee()
        }
    }
    let s = counter_tx();
    for kind in EngineKind::ALL {
        on_executor_thread(|| {
            let engine = s.host.engine(*kind, &SCHEDULE_V0).unwrap();
            let r = execute(engine.as_ref(), &Panicky(s.host.clone()), signer(), &s.calls, &[], s.limits, &SCHEDULE_V0);
            assert_eq!(r, Err(NodeFault::Panic));
            let fresh = exec(s.host.engine(*kind, &SCHEDULE_V0).unwrap().as_ref(), &s);
            assert!(fresh.ok());
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
    assert!(r.ok(), "{r:?}");
}

#[test]
fn misuse_traps() {
    // Return data past MAX_RETURN.
    let body = r#"(func (export "bk_big") (call $return_data (i32.const 0) (i32.const 4097)))
                  (func (export "bk_oob") (call $return_data (i32.const 65535) (i32.const 2)))"#;
    let mut host = world();
    host.deploy(addr("bad"), &module(body));
    let r = &run(&Scenario::new("return too long", host.clone(), vec![call("bad", "big", vec![], 0)], &[]))[0];
    assert_eq!(trapped(r), (addr("bad"), TrapKind::Length));
    let r = &run(&Scenario::new("out of bounds", host.clone(), vec![call("bad", "oob", vec![], 0)], &[]))[0];
    assert_eq!(trapped(r), (addr("bad"), TrapKind::MemoryAccess));
    let r = &run(&Scenario::new("no such method", host, vec![call("bad", "nope", vec![], 0)], &[]))[0];
    assert_eq!(trapped(r), (addr("bad"), TrapKind::NoMethod));
}

#[test]
fn recursion_past_the_limit_traps() {
    let r = &run(&work("recurse"))[0];
    assert_eq!(trapped(r), (addr("compute"), TrapKind::StackOverflow));
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
                .spawn_scoped(scope, || exec(s.host.engine(*kind, &SCHEDULE_V0).unwrap().as_ref(), &s))
                .unwrap()
                .join()
                .unwrap()
        });
        assert_eq!(trapped(&r).1, TrapKind::OutOfFuel, "{}", kind.name());
    }
}

/// Cold versus warm, with history: one block engine runs every scenario in turn, and each
/// receipt must equal the one a fresh engine gives (fuel included).
#[test]
fn cold_and_warm_agree_with_history() {
    for kind in EngineKind::ALL {
        on_executor_thread(|| {
            let warm = world().engine(*kind, &SCHEDULE_V0).unwrap();
            for s in all_scenarios() {
                let cold = exec(s.host.engine(*kind, &SCHEDULE_V0).unwrap().as_ref(), &s);
                assert_eq!(exec(warm.as_ref(), &s), cold, "{}: {}", kind.name(), s.name);
            }
        });
    }
}

/// A program that writes and emits up to 4.4's per-transaction bounds, each method reading its
/// counts from its input (u32s, LE): `keys(from, to)` writes the 4-byte keys `from..to`, each
/// with its own 4 bytes as the value; `fill(n)` writes a 16 KiB value under key "w" `n` times;
/// `events(n)` emits `n` events "e" of 4 bytes (at least one); `blob(n)` emits one event "e" of
/// `n` bytes.
fn storm() -> Vec<u8> {
    module(
        r#"
        (data (i32.const 100) "e")
        (data (i32.const 104) "w")
        (func (export "bk_keys") (local $i i32) (local $n i32)
          (call $input_read (i32.const 0))
          (local.set $i (i32.load (i32.const 0)))
          (local.set $n (i32.load (i32.const 4)))
          (block $done
            (loop $l
              (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
              (i32.store (i32.const 8) (local.get $i))
              (call $storage_write (i32.const 8) (i32.const 4) (i32.const 8) (i32.const 4) (i32.const 0))
              (local.set $i (i32.add (local.get $i) (i32.const 1)))
              (br $l))))
        (func (export "bk_fill") (local $i i32) (local $n i32)
          (call $input_read (i32.const 0))
          (local.set $n (i32.load (i32.const 0)))
          (block $done
            (loop $l
              (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
              (call $storage_write (i32.const 104) (i32.const 1) (i32.const 1024) (i32.const 16384) (i32.const 0))
              (local.set $i (i32.add (local.get $i) (i32.const 1)))
              (br $l))))
        (func (export "bk_events") (local $i i32) (local $n i32)
          (call $input_read (i32.const 0))
          (local.set $n (i32.load (i32.const 0)))
          (loop $l
            (call $emit (i32.const 100) (i32.const 1) (i32.const 0) (i32.const 4))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br_if $l (i32.lt_u (local.get $i) (local.get $n)))))
        (func (export "bk_blob")
          (call $input_read (i32.const 0))
          (call $emit (i32.const 100) (i32.const 1) (i32.const 1024) (i32.load (i32.const 0))))"#,
    )
}

/// u32s, LE: a `storm` method's input.
fn counts(n: &[u32]) -> Vec<u8> {
    n.iter().flat_map(|n| n.to_le_bytes()).collect()
}

/// A world with `storm` deployed, and limits for its biggest batches: 64 writes of 16 KiB take
/// about 11M fuel, and 256 entries of 128 bytes bond 3.3M drops.
fn storm_world() -> (MemHost, TxLimits) {
    let mut host = world();
    host.deploy(addr("storm"), &storm());
    (host, TxLimits { fuel_limit: 20_000_000, storage_limit: 1 << 40 })
}

/// 4.4's per-transaction bounds: 256 distinct keys written, 64 events.
#[test]
fn per_transaction_bounds_trap() {
    let (host, limits) = storm_world();
    let go = |method: &'static str, input: Vec<u8>| run(&Scenario::new(method, host.clone(), vec![call("storm", method, input, 0)], &[]).limits(limits)).remove(0);
    let r = go("keys", counts(&[0, 256]));
    assert!(r.ok(), "{:?}", r.fail);
    assert_eq!(r.diff.storage.len(), 256);
    assert_eq!(trapped(&go("keys", counts(&[0, 257]))), (addr("storm"), TrapKind::KeysWritten));
    let r = go("events", counts(&[64]));
    assert!(r.ok() && r.events.len() == 64, "{:?}", r.fail);
    assert_eq!(trapped(&go("events", counts(&[65]))), (addr("storm"), TrapKind::Events));
}

/// The bounds are the transaction's, not a frame's: what one call (or a callee that returned)
/// wrote and emitted counts against every later call of the batch, and a key counts once however
/// often it's written. `a` is a proxy: it calls `storm` (`call_v1`, or `try_call_v1`), then writes
/// its own key "after" (4 bytes).
#[test]
fn per_transaction_bounds_span_calls_and_frames() {
    let (host, limits) = storm_world();
    let storm = |method: &str, n: &[u32]| call("storm", method, counts(n), 0);
    let via_a = |method: &str, n: &[u32], try_call: bool| call("a", "go", proxy_input(addr("storm"), 0, 0, try_call, method, &counts(n)), 0);
    let go = |calls: Vec<Call>, declared: &[&str]| run(&Scenario::new("bounds", host.clone(), calls, declared).limits(limits)).remove(0);
    let ok = |r: &Receipt| assert!(r.ok(), "{:?}", r.fail);
    let at = |kind| (addr("storm"), kind);

    // Distinct keys: the same 200 twice are 200; overlapping, they add up across the batch.
    let r = go(vec![storm("keys", &[0, 200]), storm("keys", &[0, 200])], &[]);
    ok(&r);
    assert_eq!(r.diff.storage.len(), 200);
    let r = go(vec![storm("keys", &[0, 200]), storm("keys", &[100, 256])], &[]);
    ok(&r);
    assert_eq!(r.diff.storage.len(), 256);
    assert_eq!(trapped(&go(vec![storm("keys", &[0, 200]), storm("keys", &[100, 257])], &[])), at(TrapKind::KeysWritten));
    // A callee's keys reach its caller's count, and the caller's own key counts: 200 + "after" +
    // 55 is 256, one more traps.
    let r = go(vec![via_a("keys", &[0, 200], false), storm("keys", &[200, 255])], &[]);
    ok(&r);
    assert_eq!(r.diff.storage.len(), 256);
    assert_eq!(trapped(&go(vec![via_a("keys", &[0, 200], false), storm("keys", &[200, 256])], &[])), at(TrapKind::KeysWritten));
    // Behind `try_call`: the second callee's 56th new key is the 257th, so it fails and is
    // dropped; `a` runs on, rewriting "after", which counts nothing new.
    let r = go(vec![via_a("keys", &[0, 200], true), via_a("keys", &[200, 300], true)], &["storm"]);
    ok(&r);
    assert_eq!(r.returns[0][..4], status::OK.to_le_bytes());
    assert_eq!(r.returns[1][..4], status::ERR_FAULT.to_le_bytes());
    assert_eq!(r.diff.storage.len(), 201);

    // Written bytes: every write counts, overwrites of one key included. 64 × 16 KiB is 1 MiB.
    let r = go(vec![storm("fill", &[40]), storm("fill", &[24])], &[]);
    ok(&r);
    assert_eq!(trapped(&go(vec![storm("fill", &[40]), storm("fill", &[25])], &[])), at(TrapKind::WrittenBytes));
    // Through `a`, its own 4 bytes count too: 63 × 16 KiB + 4 fits, 64 × 16 KiB + 4 doesn't.
    ok(&go(vec![via_a("fill", &[40], false), storm("fill", &[23])], &[]));
    assert_eq!(trapped(&go(vec![via_a("fill", &[40], false), storm("fill", &[24])], &[])), at(TrapKind::WrittenBytes));

    // Events and their bytes (name and data: 1 + n for a blob), across calls and frames.
    let r = go(vec![via_a("events", &[40], false), storm("events", &[24])], &[]);
    ok(&r);
    assert_eq!(r.events.len(), 64);
    assert_eq!(trapped(&go(vec![via_a("events", &[40], false), storm("events", &[25])], &[])), at(TrapKind::Events));
    let r = go(vec![storm("blob", &[8000]), via_a("blob", &[8382], false)], &[]);
    ok(&r);
    assert_eq!(r.events.iter().map(|e| e.name.len() + e.data.len()).sum::<usize>(), 16 * 1024);
    assert_eq!(trapped(&go(vec![storm("blob", &[8000]), via_a("blob", &[8383], false)], &[])), at(TrapKind::EventBytes));
    assert_eq!(trapped(&go(vec![via_a("blob", &[8000], false), storm("blob", &[8383])], &[])), at(TrapKind::EventBytes));
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
            Some(FailCode::Aborted { code, .. }) => format!("Abort({code})"),
            Some(FailCode::Trapped { kind, .. }) => format!("{kind:?}"),
            Some(f) => format!("{f:?}"),
        };
        let outcomes: Vec<String> = receipts.iter().map(outcome).collect();
        let outcome = if outcomes.iter().all(|o| *o == outcomes[0]) { outcomes[0].clone() } else { outcomes.join(" / ") };
        let fuel: Vec<String> = receipts.iter().map(|r| r.fuel_used.to_string()).collect();
        out += &format!("| {} | {} | {} |\n", s.name, outcome, fuel.join(" | "));
    }
    println!("{out}");
}

// --- receipts: numbered codes, and node faults apart from them ---

/// Receipts hold numbers (6.5): each kind keeps its number forever. This table is the record;
/// changing a row is a consensus change.
#[test]
fn trap_kinds_and_fail_codes_keep_their_numbers() {
    use TrapKind::*;
    let table = [
        (OutOfFuel, 1),
        (Unreachable, 2),
        (MemoryOutOfBounds, 3),
        (TableOutOfBounds, 4),
        (IndirectCallToNull, 5),
        (DivisionByZero, 6),
        (IntegerOverflow, 7),
        (StackOverflow, 8),
        (BadSignature, 9),
        (MemoryAccess, 10),
        (Length, 11),
        (Amount, 12),
        (Flags, 13),
        (Name, 14),
        (UnboundProgram, 15),
        (Reentrancy, 16),
        (CallDepth, 17),
        (VersionMismatch, 18),
        (NotAProgram, 19),
        (NoMethod, 20),
        (NotCallable, 21),
        (NotPayable, 22),
        (ReadOnly, 23),
        (Insufficient, 24),
        (StorageLimit, 25),
        (StateFull, 26),
        (KeysWritten, 27),
        (WrittenBytes, 28),
        (Events, 29),
        (EventBytes, 30),
    ];
    assert_eq!(TrapKind::ALL.len(), table.len(), "a kind without a row");
    for (kind, number) in table {
        assert_eq!(kind.number(), number, "{kind:?}");
        assert_eq!(TrapKind::from_number(number), Some(kind));
    }
    assert_eq!(TrapKind::from_number(0), None);
    assert_eq!(TrapKind::from_number(31), None);
    let a = addr("a");
    assert_eq!(FailCode::Trapped { program: a, kind: OutOfFuel }.number(), 1);
    assert_eq!(FailCode::Aborted { program: a, code: 0, message: String::new() }.number(), 2);
    assert_eq!(FailCode::Commit(CommitError::Overflow).number(), 3);
    let commits = [(CommitError::Overflow, 1), (CommitError::NotConserved, 2), (CommitError::Debited, 3)];
    for (e, number) in commits {
        assert_eq!(e.number(), number, "{e:?}");
    }
}

/// A program with one method per trap the engine and the host functions raise, each numbered in
/// the receipt.
fn traps() -> Vec<u8> {
    module(
        r#"
        (type $unary (func (param i32) (result i32)))
        (type $nullary (func))
        (table 3 3 funcref)
        (elem (i32.const 0) $id $nop)
        (data (i32.const 64) "Bad")
        (data (i32.const 80) "k")
        (func $id (type $unary) (local.get 0))
        (func $nop (type $nullary))
        (func (export "bk_unreachable") unreachable)
        (func (export "bk_out_of_bounds") (drop (i32.load (i32.const 65534))))
        (func (export "bk_table_out_of_bounds") (drop (call_indirect (type $unary) (i32.const 1) (i32.const 3))))
        (func (export "bk_null_entry") (drop (call_indirect (type $unary) (i32.const 1) (i32.const 2))))
        (func (export "bk_bad_signature") (drop (call_indirect (type $unary) (i32.const 1) (i32.const 1))))
        (func (export "bk_divide_by_zero") (drop (i32.div_u (i32.const 1) (i32.const 0))))
        (func (export "bk_overflow") (drop (i32.div_s (i32.const 0x80000000) (i32.const -1))))
        (func (export "bk_amount") (drop (call $transfer (i32.const 0) (i64.const -1) (i32.const 0))))
        (func (export "bk_flags") (call $storage_write (i32.const 80) (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 2)))
        (func (export "bk_transfer_flags") (drop (call $transfer (i32.const 0) (i64.const 1) (i32.const 3))))
        (func (export "bk_name") (call $emit (i32.const 64) (i32.const 3) (i32.const 0) (i32.const 0)))
        (func (export "bk_event_bytes") (call $emit (i32.const 80) (i32.const 1) (i32.const 0) (i32.const 16384)))
        (func (export "bk_written_bytes") (local $i i32)
          (loop $l
            (i32.store (i32.const 84) (local.get $i))
            (call $storage_write (i32.const 84) (i32.const 4) (i32.const 1024) (i32.const 16384) (i32.const 0))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br_if $l (i32.le_u (local.get $i) (i32.const 64)))))"#,
    )
}

/// Calls `program` (input bytes 0..32) at `expect` (32..64), method `noop`.
fn dialer() -> Vec<u8> {
    module(
        r#"
        (data (i32.const 200) "noop")
        (func (export "bk_dial")
          (call $input_read (i32.const 0))
          (drop (call $call (i32.const 0) (i32.const 32) (i32.const 200) (i32.const 4) (i32.const 0) (i32.const 0) (i64.const 0) (i64.const 0))))"#,
    )
}

/// Every trap kind a program can cause this way gets its number in the receipt, on every engine,
/// with nothing applied.
#[test]
fn engine_and_host_traps_are_numbered() {
    let mut host = world();
    host.deploy(addr("traps"), &traps());
    host.deploy(addr("dialer"), &dialer());
    let one = |method: &'static str| {
        let s = Scenario::new(method, host.clone(), vec![call("traps", method, vec![], 0)], &[])
            .limits(TxLimits { fuel_limit: 20_000_000, storage_limit: 1 << 40 });
        trapped(&run(&s)[0])
    };
    let cases = [
        ("unreachable", TrapKind::Unreachable),
        ("out_of_bounds", TrapKind::MemoryOutOfBounds),
        ("table_out_of_bounds", TrapKind::TableOutOfBounds),
        ("null_entry", TrapKind::IndirectCallToNull),
        ("bad_signature", TrapKind::BadSignature),
        ("divide_by_zero", TrapKind::DivisionByZero),
        ("overflow", TrapKind::IntegerOverflow),
        ("amount", TrapKind::Amount),
        ("flags", TrapKind::Flags),
        ("transfer_flags", TrapKind::Flags),
        ("name", TrapKind::Name),
        ("event_bytes", TrapKind::EventBytes),
        ("written_bytes", TrapKind::WrittenBytes),
    ];
    for (method, kind) in cases {
        assert_eq!(one(method), (addr("traps"), kind), "{method}");
    }
    // A digest that isn't the callee's version, and a declared account that isn't a program.
    let dial = |to: &str, expect: [u8; 32]| {
        let input = [addr(to).as_slice(), &expect].concat();
        let s = Scenario::new("dial", host.clone(), vec![call("dialer", "dial", input, 0)], &[to]);
        trapped(&run(&s)[0])
    };
    assert_eq!(dial("leaf", [7; 32]), (addr("dialer"), TrapKind::VersionMismatch));
    assert_eq!(dial("alice", [0; 32]), (addr("dialer"), TrapKind::NotAProgram));
}

/// A view onto `MemHost` with one program's code replaced (code no upload would accept, loaded
/// straight into an engine), one account's balance replaced, or the version digests gone: what a
/// node with a bug might hand the executor.
struct Patched<'a> {
    host: &'a MemHost,
    code: Option<(Address, [u8; 32])>,
    balance: Option<(Address, u64)>,
    no_digests: bool,
}

impl<'a> Patched<'a> {
    fn of(host: &'a MemHost) -> Self {
        Patched { host, code: None, balance: None, no_digests: false }
    }
}

impl View for Patched<'_> {
    fn height(&self) -> u64 {
        self.host.height()
    }
    fn balance(&self, a: &Address) -> Option<u64> {
        match self.balance {
            Some((account, balance)) if account == *a => Some(balance),
            _ => self.host.balance(a),
        }
    }
    fn code_of(&self, p: &Address) -> Option<[u8; 32]> {
        match self.code {
            Some((program, code)) if program == *p => Some(code),
            _ => self.host.code_of(p),
        }
    }
    fn version_digest(&self, p: &Address) -> Option<Digest> {
        if self.no_digests { None } else { self.host.version_digest(p) }
    }
    fn method(&self, p: &Address, name: &str) -> Option<MethodInfo> {
        self.host.method(p, name)
    }
    fn storage(&self, p: &Address, k: &[u8]) -> Option<Entry> {
        self.host.storage(p, k)
    }
    fn state_bytes(&self) -> u64 {
        self.host.state_bytes()
    }
    fn storage_price(&self, used: u64) -> Option<u64> {
        self.host.storage_price(used)
    }
    fn account_creation_fee(&self) -> u64 {
        self.host.account_creation_fee()
    }
}

/// What no valid transaction can cause is the node's fault: `execute` returns it instead of a
/// receipt, `try_call` never absorbs it, and nothing is committed.
#[test]
fn node_faults_are_never_receipts() {
    let host = world();
    let s = |calls, declared: &[&str]| Scenario::new("fault", host.clone(), calls, declared);
    on_executor_thread(|| {
        let full = host.engine(EngineKind::Soroban, &SCHEDULE_V0).unwrap();
        // Code that isn't in the block's engine, at the top level and behind `call`/`try_call`.
        let (only_a, _) = host.engine_for(EngineKind::Soroban, &SCHEDULE_V0, &[addr("a")]).unwrap();
        let leaf = s(vec![call("leaf", "noop", vec![], 0)], &[]);
        let r = execute(only_a.as_ref(), &leaf.host, signer(), &leaf.calls, &[], LIMITS, &SCHEDULE_V0);
        assert_eq!(r, Err(NodeFault::NotLoaded));
        for try_call in [false, true] {
            let input = proxy_input(addr("leaf"), 0, 0, try_call, "noop", &[]);
            let nested = s(vec![call("a", "go", input, 0)], &["leaf"]);
            let r = execute(only_a.as_ref(), &nested.host, signer(), &nested.calls, &nested.declared, LIMITS, &SCHEDULE_V0);
            assert_eq!(r, Err(NodeFault::NotLoaded), "try_call: {try_call}");
            assert!(exec(full.as_ref(), &nested).ok(), "with the code loaded it runs");
        }
        // Code no upload accepts (a start section), instantiated: the engine refuses it.
        let started = wat::parse_str(r#"(module (memory (export "memory") 1 1) (func $s) (start $s) (func (export "bk_noop")))"#).unwrap();
        let mut engine = host.engine(EngineKind::Soroban, &SCHEDULE_V0).unwrap();
        engine.load([9; 32], &started).unwrap();
        let view = Patched { code: Some((addr("leaf"), [9; 32])), ..Patched::of(&host) };
        let r = execute(engine.as_ref(), &view, signer(), &leaf.calls, &[], LIMITS, &SCHEDULE_V0);
        assert!(matches!(r, Err(NodeFault::Instantiate(_))), "{r:?}");
        // A balance no account can have breaks the host's own invariant.
        let view = Patched { balance: Some((addr("counter"), u64::MAX)), ..Patched::of(&host) };
        let ctx = s(vec![call("counter", "ctx", vec![], 0)], &[]);
        let r = execute(full.as_ref(), &view, signer(), &ctx.calls, &[], LIMITS, &SCHEDULE_V0);
        assert!(matches!(r, Err(NodeFault::Host(_))), "{r:?}");
        // A program the view has code for but no version digest: the view contradicts itself.
        let view = Patched { no_digests: true, ..Patched::of(&host) };
        let r = execute(full.as_ref(), &view, signer(), &ctx.calls, &[], LIMITS, &SCHEDULE_V0);
        assert_eq!(r, Err(NodeFault::Host("a program without a version digest")));
    });
}

/// The tx codec bounds a batch to 1–8 calls and 16 bindings. `execute` given another shape is the
/// node's bug: a node fault, never a receipt (an empty batch would otherwise succeed, fuel 0).
#[test]
fn a_batch_no_transaction_has_is_a_node_fault() {
    let host = world();
    let counter = host.bind(call("counter", "incr", vec![], 0));
    let bindings = |n: usize| (0..n).map(|i| Binding { program: addr(&format!("p{i}")), version_digest: ANY_VERSION }).collect::<Vec<_>>();
    on_executor_thread(|| {
        let engine = host.engine(EngineKind::Soroban, &SCHEDULE_V0).unwrap();
        let go = |calls: &[Call], bindings: &[Binding]| execute(engine.as_ref(), &host, signer(), calls, bindings, LIMITS, &SCHEDULE_V0);
        assert_eq!(go(&[], &[]), Err(NodeFault::Host("a batch without calls")));
        assert_eq!(go(&[], &bindings(1)), Err(NodeFault::Host("a batch without calls")));
        assert_eq!(go(&vec![counter.clone(); 9], &[]), Err(NodeFault::Host("a batch of more than MAX_CALLS calls")));
        assert_eq!(go(std::slice::from_ref(&counter), &bindings(17)), Err(NodeFault::Host("more than MAX_BINDINGS bindings")));
        // At the bounds it runs.
        let r = go(&vec![counter.clone(); 8], &bindings(16)).unwrap();
        assert!(r.ok(), "{:?}", r.fail);
        assert_eq!(r.returns.last().unwrap(), &8u64.to_le_bytes());
    });
}

// --- transfers that open accounts ---

/// `OPEN_PAY`: the program pays the creation fee on top of the amount, which arrives whole; short
/// of both, it's a status and nothing moves. To an account that exists, no fee either way.
#[test]
fn open_pay_burns_the_fee_on_top() {
    let fee = world().creation_fee;
    let s = Scenario::new("transfer opening (OPEN_PAY)", world(), pay(addr("newbie"), fee + 1_000, 400, flags::OPEN_PAY), &[]);
    let r = &run(&s)[0];
    assert!(r.ok(), "{r:?}");
    assert_eq!(r.returns, vec![status::OK.to_le_bytes().to_vec()]);
    assert_eq!((r.diff.opened.clone(), r.diff.burned), (vec![addr("newbie")], u128::from(fee)));
    assert_eq!(balance(r, addr("newbie")), 400, "the amount arrives whole");
    assert_eq!(balance(r, addr("vault")), 600, "the program paid 400 and the fee");
    let mut host = s.host.clone();
    host.apply(&r.diff);
    assert_eq!(host.balance(&addr("newbie")), Some(400));

    let short = Scenario::new("OPEN_PAY, short of the fee", world(), pay(addr("newbie"), fee + 399, 400, flags::OPEN_PAY), &[]);
    let r = &run(&short)[0];
    assert_eq!(r.returns, vec![status::ERR_TRANSFER.to_le_bytes().to_vec()]);
    assert!(r.diff.opened.is_empty() && r.diff.burned == 0);
    assert_eq!(balance(r, addr("vault")), i128::from(fee + 399));

    let existing = Scenario::new("OPEN_PAY to an account that exists", world(), pay(addr("alice"), 1_000, 400, flags::OPEN_PAY), &[]);
    let r = &run(&existing)[0];
    assert_eq!((balance(r, addr("alice")), balance(r, addr("vault")), r.diff.burned), (400, 600, 0));
}

// --- method kinds (11.1): View frames, Init, payable ---

/// One method per thing a read-only frame may or may not do, an initializer, and `relay`, which
/// calls `program32 ‖ method_len u32 ‖ method ‖ input` (see [`relay`]) at any version (`expect`
/// points at zeros) and returns what it returned, writing nothing.
fn kinds() -> Vec<u8> {
    module(
        r#"
        (data (i32.const 0) "k")
        (data (i32.const 16) "e")
        (func (export "bk_read")
          (i32.store (i32.const 100) (call $storage_read (i32.const 0) (i32.const 1) (i32.const 200) (i32.const 8)))
          (call $return_data (i32.const 100) (i32.const 4)))
        (func (export "bk_write") (call $storage_write (i32.const 0) (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 0)))
        (func (export "bk_remove") (drop (call $storage_remove (i32.const 0) (i32.const 1))))
        (func (export "bk_transfer") (drop (call $transfer (i32.const 300) (i64.const 0) (i32.const 0))))
        (func (export "bk_emit") (call $emit (i32.const 16) (i32.const 1) (i32.const 0) (i32.const 0)))
        (func (export "bk_relay") (local $len i32) (local $m i32)
          (local.set $len (call $input_len))
          (call $input_read (i32.const 1024))
          (local.set $m (i32.load (i32.const 1056)))
          (drop (call $call (i32.const 1024) (i32.const 512) (i32.const 1060) (local.get $m)
            (i32.add (i32.const 1060) (local.get $m)) (i32.sub (local.get $len) (i32.add (i32.const 36) (local.get $m)))
            (i64.const 0) (i64.const 0)))
          (call $last_return (i32.const 4096))
          (call $return_data (i32.const 4096) (call $last_return_len)))
        (func (export "bk_setup") (call $storage_write (i32.const 0) (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 0)))
        (func (export "bk_noop"))"#,
    )
}

/// `relay`'s input: call `method` of `to` with `inner`.
fn relay(to: &str, method: &str, inner: &[u8]) -> Vec<u8> {
    [addr(to).as_slice(), &(method.len() as u32).to_le_bytes(), method.as_bytes(), inner].concat()
}

/// The world with `kinds` and `kinds2` (the same code) and their interfaces: `read` and `relay`
/// are non-payable views; `write`, `remove`, `transfer` and `emit` are payable views (so a view
/// that writes, or is sent value, meets the read-only rule rather than payability); `setup` is
/// the initializer; `noop` a non-payable call.
fn kinds_world() -> MemHost {
    let mut host = world();
    for p in ["kinds", "kinds2"] {
        let p = addr(p);
        host.deploy(p, &kinds());
        host.set_method(p, "read", MethodKind::View, false)
            .set_method(p, "relay", MethodKind::View, false)
            .set_method(p, "setup", MethodKind::Init, false)
            .set_method(p, "noop", MethodKind::Call, false);
        for m in ["write", "remove", "transfer", "emit"] {
            host.set_method(p, m, MethodKind::View, true);
        }
    }
    host
}

/// A call of `kind` (a read-only call is `View`, `CreateProgram`'s initializer `Init`).
fn call_as(kind: MethodKind, program: &str, method: &str, input: Vec<u8>, value: u64) -> Call {
    Call { kind, ..call(program, method, input, value) }
}

/// One transaction on `host`, every engine, its first receipt.
fn once(host: &MemHost, calls: Vec<Call>, declared: &[&str]) -> Receipt {
    run(&Scenario::new("kinds", host.clone(), calls, declared)).remove(0)
}

/// A `View` frame reads but never writes, removes, transfers, emits, moves value or calls a
/// non-`View` method (4.4, 8.2), whether it's a read-only call's top frame or a callee.
#[test]
fn view_frames_are_read_only() {
    let host = kinds_world();
    let view = |program: &str, method: &str, input: Vec<u8>| call_as(MethodKind::View, program, method, input, 0);
    let absent = (-1i32).to_le_bytes().to_vec();
    // A read-only call (8.5) runs a view, which reads.
    let r = once(&host, vec![view("kinds", "read", vec![])], &[]);
    assert!(r.ok(), "{r:?}");
    assert_eq!(r.returns, vec![absent.clone()]);
    // Every write it tries traps, before it does anything.
    for m in ["write", "remove", "transfer", "emit"] {
        let r = once(&host, vec![view("kinds", m, vec![])], &[]);
        assert_eq!(trapped(&r), (addr("kinds"), TrapKind::ReadOnly), "{m}");
    }
    // Called by a program, the view's frame is read-only: `call` traps the caller with the view's
    // trap, `try_call` absorbs it.
    let r = once(&host, vec![call("a", "go", proxy_input(addr("kinds"), 0, 0, false, "write", &[]), 0)], &["kinds"]);
    assert_eq!(trapped(&r), (addr("kinds"), TrapKind::ReadOnly));
    let r = once(&host, vec![call("a", "go", proxy_input(addr("kinds"), 0, 0, true, "write", &[]), 0)], &["kinds"]);
    assert!(r.ok(), "{r:?}");
    assert_eq!(r.returns[0][..4], status::ERR_FAULT.to_le_bytes());
    // A view may call views, two deep, and nothing else at any depth.
    let r = once(&host, vec![view("kinds", "relay", relay("kinds2", "read", &[]))], &["kinds2"]);
    assert_eq!((r.ok(), r.returns.clone()), (true, vec![absent]), "{r:?}");
    let r = once(&host, vec![view("kinds", "relay", relay("leaf", "noop", &[]))], &["leaf"]);
    assert_eq!(trapped(&r), (addr("kinds"), TrapKind::ReadOnly), "a view's call to a Call method");
    let r = once(&host, vec![view("kinds", "relay", relay("kinds2", "relay", &relay("leaf", "noop", &[])))], &["kinds2", "leaf"]);
    assert_eq!(trapped(&r), (addr("kinds2"), TrapKind::ReadOnly), "read-only all the way down");
    // No value to a view, from a program or from the signer.
    let r = once(&host, vec![call("a", "go", proxy_input(addr("kinds"), 1, 0, false, "emit", &[]), 10)], &["kinds"]);
    assert_eq!(trapped(&r), (addr("a"), TrapKind::ReadOnly));
    let r = once(&host, vec![call_as(MethodKind::View, "kinds", "emit", vec![], 1)], &[]);
    assert_eq!(trapped(&r), (addr("kinds"), TrapKind::ReadOnly));
    assert_eq!(r.diff, Default::default(), "the value never moved");
    // A view isn't a `CallProgram` call, and a call isn't a read-only one.
    assert_eq!(trapped(&once(&host, vec![call("kinds", "read", vec![], 0)], &[])), (addr("kinds"), TrapKind::NotCallable));
    assert_eq!(trapped(&once(&host, vec![view("kinds", "noop", vec![])], &[])), (addr("kinds"), TrapKind::NotCallable));
}

/// An initializer runs only as `CreateProgram`'s call (kind `Init`): never from `CallProgram`,
/// never from another program.
#[test]
fn init_is_never_callable() {
    let host = kinds_world();
    let r = once(&host, vec![call("kinds", "setup", vec![], 0)], &[]);
    assert_eq!(trapped(&r), (addr("kinds"), TrapKind::NotCallable));
    let r = once(&host, vec![call_as(MethodKind::Init, "kinds", "setup", vec![], 0)], &[]);
    assert!(r.ok(), "{r:?}");
    assert_eq!(r.diff.storage.len(), 1, "it ran and wrote");
    let r = once(&host, vec![call_as(MethodKind::Init, "kinds", "noop", vec![], 0)], &[]);
    assert_eq!(trapped(&r), (addr("kinds"), TrapKind::NotCallable), "an Init call runs only the initializer");
    let r = once(&host, vec![call("a", "go", proxy_input(addr("kinds"), 0, 0, false, "setup", &[]), 0)], &["kinds"]);
    assert_eq!(trapped(&r), (addr("a"), TrapKind::NotCallable));
    let r = once(&host, vec![call("a", "go", proxy_input(addr("kinds"), 0, 0, true, "setup", &[]), 0)], &["kinds"]);
    assert_eq!(r.returns[0][..4], status::ERR_FAULT.to_le_bytes());
}

/// Value goes only to payable methods (8.1, 8.2), so FANG can't be stranded in a program that
/// never expected it. Nothing moves when it's refused.
#[test]
fn value_only_to_payable_methods() {
    let host = kinds_world();
    let r = once(&host, vec![call("kinds", "noop", vec![], 1)], &[]);
    assert_eq!(trapped(&r), (addr("kinds"), TrapKind::NotPayable));
    assert!(once(&host, vec![call("kinds", "noop", vec![], 0)], &[]).ok());
    let r = once(&host, vec![call("a", "go", proxy_input(addr("kinds"), 5, 0, false, "noop", &[]), 5)], &["kinds"]);
    assert_eq!(trapped(&r), (addr("a"), TrapKind::NotPayable));
    let r = once(&host, vec![call("a", "go", proxy_input(addr("kinds"), 5, 0, true, "noop", &[]), 5)], &["kinds"]);
    assert_eq!(r.returns[0][..4], status::ERR_FAULT.to_le_bytes());
    assert_eq!((balance(&r, addr("a")), balance(&r, addr("kinds"))), (5, 0), "the value stayed with the caller");
}

// --- version digests (7.3, 8.1, 8.2) ---

/// A call runs only the version its digest names (no wildcard); a binding's digest is checked
/// when its program is reached, not before; `call_v1`'s `expect` is a version digest from the
/// view, zeros for any, never the bare code hash.
#[test]
fn calls_bind_the_version_in_force() {
    let host = world();
    let leaf = addr("leaf");
    let digest = host.version_digest(&leaf).unwrap();
    let noop = |version_digest: Digest| Call { version_digest, ..call("leaf", "noop", vec![], 0) };
    let raw = |host: &MemHost, calls: Vec<Call>, declared: Vec<Binding>| run(&Scenario::raw("version", host.clone(), calls, declared)).remove(0);
    assert!(raw(&host, vec![noop(digest)], vec![]).ok());
    for wrong in [[7; 32], ANY_VERSION, host.programs[&leaf]] {
        assert_eq!(trapped(&raw(&host, vec![noop(wrong)], vec![])), (leaf, TrapKind::VersionMismatch));
    }
    // An upgrade (the same code, version 2) makes the previewed digest stale.
    let mut upgraded = host.clone();
    upgraded.upgrade(leaf, host.programs[&leaf]);
    assert_ne!(upgraded.version_digest(&leaf), Some(digest));
    assert_eq!(trapped(&raw(&upgraded, vec![noop(digest)], vec![])), (leaf, TrapKind::VersionMismatch));
    assert!(raw(&upgraded, vec![upgraded.bind(noop(ANY_VERSION))], vec![]).ok());

    // Bindings: checked when the bound program's frame opens.
    let go = host.bind(call("a", "go", proxy_input(leaf, 0, 0, false, "noop", &[]), 0));
    let bound = |version_digest| vec![Binding { program: leaf, version_digest }];
    assert!(raw(&host, vec![go.clone()], bound(digest)).ok());
    assert!(raw(&host, vec![go.clone()], bound(ANY_VERSION)).ok());
    assert_eq!(trapped(&raw(&host, vec![go], bound([7; 32]))), (addr("a"), TrapKind::VersionMismatch));
    let unreached = host.bind(call("counter", "incr", vec![], 0));
    assert!(raw(&host, vec![unreached], bound([7; 32])).ok(), "a binding never reached is never checked");

    // `call_v1`'s `expect`.
    let mut host = host;
    host.deploy(addr("dialer"), &dialer());
    let dial = |expect: [u8; 32]| once(&host, vec![call("dialer", "dial", [leaf.as_slice(), &expect].concat(), 0)], &["leaf"]);
    assert!(dial(digest).ok());
    assert!(dial(ANY_VERSION).ok());
    assert_eq!(trapped(&dial(host.programs[&leaf])), (addr("dialer"), TrapKind::VersionMismatch), "the code hash isn't a version");
}

// --- storage economics (6.2, 6.3) ---

/// Deposits are priced on the curve at the bytes state holds once the write lands; past the
/// ceiling there's no price and the write traps.
#[test]
fn storage_is_priced_by_fill_and_capped() {
    let incr = |host: &MemHost| {
        let s = Scenario::new("incr", host.clone(), vec![call("counter", "incr", vec![], 0)], &[])
            .limits(TxLimits { fuel_limit: LIMITS.fuel_limit, storage_limit: 1 << 40 });
        run(&s).remove(0)
    };
    let mut host = world();
    // 128 accounted bytes against a soft cap of 1,024: an eighth, so twice the base price.
    host.state_soft_bytes = 1_024;
    let r = incr(&host);
    assert!(r.ok(), "{r:?}");
    assert_eq!(balance(&r, signer()), -2 * deposit(1, 8));
    assert_eq!(r.diff.storage[0].entry.as_ref().unwrap().deposit, 25_600);
    // A soft cap of 63: the ceiling, 126 bytes, is short of one entry.
    host.state_soft_bytes = 63;
    assert_eq!(trapped(&incr(&host)), (addr("counter"), TrapKind::StateFull));
    // A soft cap of 64: 128 bytes is exactly the ceiling, at 2^16 times the base price.
    host.state_soft_bytes = 64;
    let r = incr(&host);
    assert!(r.ok(), "{r:?}");
    assert_eq!(balance(&r, signer()), -(deposit(1, 8) << 16));
}

// --- instantiation pricing (4.5, schedule v0) ---

/// A frame's instantiation is charged from what the module holds: two programs whose `bk_noop` is
/// the same body differ in fuel by exactly the price of what one has more of.
#[test]
fn instantiation_is_priced_per_entity() {
    let lean = strip_custom_sections(&wat::parse_str(r#"(module (memory (export "memory") 1 1) (func (export "bk_noop")))"#).unwrap());
    let rich = strip_custom_sections(
        &wat::parse_str(
            r#"(module
              (import "blockula" "input_len_v1" (func (result i32)))
              (import "blockula" "value_v1" (func (result i64)))
              (import "blockula" "caller_v1" (func (param i32)))
              (memory (export "memory") 3 3)
              (table 8 8 funcref)
              (global i32 (i32.const 1)) (global i32 (i32.const 2)) (global (mut i64) (i64.const 3)) (global i64 (i64.const 4))
              (func (export "bk_noop"))
              (func $f1) (func $f2) (func $f3)
              (func (export "bk_a") (drop (call 0))) (func (export "bk_b") (drop (call 1)))
              (elem (i32.const 0) $f1 $f2 $f3)
              (elem (i32.const 4) $f1 $f2 $f3)
              (data (i32.const 0) "abcd") (data (i32.const 8) "efg") (data (i32.const 16) "hij"))"#,
        )
        .unwrap(),
    );
    let (lean_info, rich_info) = (CodeInfo::read(&lean).unwrap(), CodeInfo::read(&rich).unwrap());
    let expect = CodeInfo {
        size: rich.len(),
        pages: 3,
        data_bytes: 10,
        imports: 3,
        exports: 4,
        functions: 6,
        globals: 4,
        element_segments: 2,
        elements: 6,
        data_segments: 3,
    };
    assert_eq!(rich_info, expect);
    let more = 300 * 3 + 500 * 2 + 15 * 5 + 20 * 4 + 50 * 2 + 2 * 6 + 20 * 3 + 2_000 * 2 + 10;
    assert_eq!(SCHEDULE_V0.instantiate_fuel(&rich_info) - SCHEDULE_V0.instantiate_fuel(&lean_info), more);
    let mut host = world();
    host.deploy(addr("lean"), &lean);
    host.deploy(addr("rich"), &rich);
    let fuel = |p: &str| {
        let r = once(&host, vec![call(p, "noop", vec![], 0)], &[]);
        assert!(r.ok(), "{r:?}");
        r.fuel_used
    };
    assert_eq!(fuel("rich") - fuel("lean"), more);
    assert_eq!(fuel("lean"), SCHEDULE_V0.instantiate_fuel(&lean_info) + 2 * u64::from(SCHEDULE_V0.ops.base), "what soroban-wasmi charges an empty body: two `base`");
}

// --- every trap kind, numbered in a receipt ---

/// Every `TrapKind` a transaction can cause reaches a receipt with its number, each from a
/// scenario of its own; the table must cover `TrapKind::ALL`.
#[test]
fn every_trap_kind_reaches_a_receipt() {
    let mut host = kinds_world();
    host.deploy(addr("traps"), &traps());
    host.deploy(addr("dialer"), &dialer());
    host.deploy(addr("bad"), &module(r#"(func (export "bk_big") (call $return_data (i32.const 0) (i32.const 4097)))
                                         (func (export "bk_oob") (call $return_data (i32.const 65535) (i32.const 2)))"#));
    let wide = TxLimits { fuel_limit: 20_000_000, storage_limit: 1 << 40 };
    let on = |host: &MemHost, calls: Vec<Call>, declared: &[&str]| Scenario::new("trap", host.clone(), calls, declared).limits(wide);
    let traps = |m: &str| on(&host, vec![call("traps", m, vec![], 0)], &[]);
    let dial = |to: &str, expect: [u8; 32]| on(&host, vec![call("dialer", "dial", [addr(to).as_slice(), &expect].concat(), 0)], &[to]);
    let mut full = host.clone();
    full.state_soft_bytes = 1;
    let mut storm = host.clone();
    storm.deploy(addr("storm"), &module(r#"(func (export "bk_keys") (local $i i32)
          (loop $l
            (i32.store (i32.const 8) (local.get $i))
            (call $storage_write (i32.const 8) (i32.const 4) (i32.const 8) (i32.const 4) (i32.const 0))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br_if $l (i32.le_u (local.get $i) (i32.const 256)))))
        (data (i32.const 100) "e")
        (func (export "bk_events") (local $i i32)
          (loop $l
            (call $emit (i32.const 100) (i32.const 1) (i32.const 0) (i32.const 0))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br_if $l (i32.le_u (local.get $i) (i32.const 64)))))"#));
    use TrapKind::*;
    let cases: Vec<(TrapKind, Address, Scenario)> = vec![
        (OutOfFuel, addr("leaf"), out_of_fuel()),
        (Unreachable, addr("traps"), traps("unreachable")),
        (MemoryOutOfBounds, addr("traps"), traps("out_of_bounds")),
        (TableOutOfBounds, addr("traps"), traps("table_out_of_bounds")),
        (IndirectCallToNull, addr("traps"), traps("null_entry")),
        (DivisionByZero, addr("traps"), traps("divide_by_zero")),
        (IntegerOverflow, addr("traps"), traps("overflow")),
        (StackOverflow, addr("compute"), work("recurse")),
        (BadSignature, addr("traps"), traps("bad_signature")),
        (MemoryAccess, addr("bad"), on(&host, vec![call("bad", "oob", vec![], 0)], &[])),
        (Length, addr("bad"), on(&host, vec![call("bad", "big", vec![], 0)], &[])),
        (Amount, addr("traps"), traps("amount")),
        (Flags, addr("traps"), traps("flags")),
        (Name, addr("traps"), traps("name")),
        (UnboundProgram, addr("a"), unbound(false)),
        (Reentrancy, addr("b"), reentrancy()),
        (CallDepth, addr("p8"), chain("depth 9", 8)),
        (VersionMismatch, addr("dialer"), dial("leaf", [7; 32])),
        (NotAProgram, addr("dialer"), dial("alice", ANY_VERSION)),
        (NoMethod, addr("bad"), on(&host, vec![call("bad", "nope", vec![], 0)], &[])),
        (NotCallable, addr("kinds"), on(&host, vec![call("kinds", "setup", vec![], 0)], &[])),
        (NotPayable, addr("kinds"), on(&host, vec![call("kinds", "noop", vec![], 1)], &[])),
        (ReadOnly, addr("kinds"), on(&host, vec![call_as(MethodKind::View, "kinds", "write", vec![], 0)], &[])),
        (Insufficient, addr("counter"), on(&host, vec![call("counter", "sponsor", vec![], 0)], &[])),
        (StorageLimit, addr("counter"), counter_tx().limits(TxLimits { fuel_limit: 1_000_000, storage_limit: 0 })),
        (StateFull, addr("counter"), on(&full, vec![call("counter", "incr", vec![], 0)], &[])),
        (KeysWritten, addr("storm"), on(&storm, vec![call("storm", "keys", vec![], 0)], &[])),
        (WrittenBytes, addr("traps"), traps("written_bytes")),
        (Events, addr("storm"), on(&storm, vec![call("storm", "events", vec![], 0)], &[])),
        (EventBytes, addr("traps"), traps("event_bytes")),
    ];
    let mut covered = Vec::new();
    for (kind, program, s) in cases {
        for r in run(&s) {
            assert_eq!(trapped(&r), (program, kind), "{kind:?}");
            assert_eq!(r.fail.as_ref().map(FailCode::number), Some(1));
            assert!(r.diff == Default::default() && r.events.is_empty() && r.returns.is_empty(), "{kind:?}: a failure applies nothing");
        }
        covered.push(kind);
    }
    assert_eq!(covered, TrapKind::ALL, "every kind, in number order");
}
