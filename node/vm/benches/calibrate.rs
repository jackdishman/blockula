//! Calibration (docs/PROGRAMS-PLAN.md S3; docs/PROGRAMS.md 4.5 and 12.2): what the engine costs
//! on this machine, against the fuel schedule at the design's 2.5 ns-per-fuel rule.
//!
//! **Measurements, never prices.** The floor machines (an x86 Fly `performance-4x` and GitHub's
//! `ubuntu-24.04-arm` runners, `.github/workflows/calibrate-vm.yml`) check the schedule; this
//! binary is the benchmark they run (C19's gate starts from it).
//!
//! ```text
//! cargo bench --locked --bench calibrate -- [--quick] [--only ops,host] [--out FILE]
//! ```
//!
//! With `--features ref038` every table gains a wasmi 0.38 column, for comparison only.
//!
//! Sections: `ops` (ns per fuel by operator class), `translate` (validation and translation ns
//! per byte), `instantiate`, `host` (every ABI v1 function), `block` (a `max_block_fuel` block of
//! the worst patterns), `warm` (cold versus warm, an engine's cost per block), `stack` (the
//! smallest native stack for the deepest call chain, bisected in child processes).
//!
//! Method: plain `Instant` timing with warmup and repetitions; every sample is a batch long
//! enough for the clock (at least `sample` ns), setup outside the clock, outputs dropped after it.
//! Reported: the median, and the min and max over the repetitions. Loop costs are per iteration,
//! from the difference between N iterations and none, so instantiation and the transaction's
//! fixed cost cancel. Everything runs through the real executor (`exec::execute`) and host
//! (`MemHost`) unless a row says otherwise, on a thread with the executor's 64 MiB stack.

use blockula_vm::engine::{Address, BlockEngine, CodeHash, EngineKind, Host, Instance, Trap, new_block_engine};
use blockula_vm::exec::{Call, Entry, Receipt, TxLimits, execute};
use blockula_vm::meter::V1;
use blockula_vm::testing::{MemHost, addr};
use blockula_vm::validate::validate;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The design's calibration rule: no path slower than this (4.5).
const NS_PER_FUEL: f64 = 2.5;
const BLOCK_FUEL: u64 = 60_000_000;
const TX_FUEL: u64 = 20_000_000;
const BLOCK_MS: f64 = 150.0;
/// A fuel limit no measured loop reaches.
const UNLIMITED: u64 = 1 << 50;

// ---- configuration, timing, statistics -----------------------------------------------------

struct Cfg {
    reps: usize,
    warmup: usize,
    /// Shortest timed sample: short runs are batched up to this.
    sample: Duration,
    /// How long one measured loop run should take.
    run: Duration,
    only: Vec<String>,
    out: Option<String>,
}

static CFG: OnceLock<Cfg> = OnceLock::new();

fn cfg() -> &'static Cfg {
    CFG.get().expect("configured")
}

fn wanted(section: &str) -> bool {
    cfg().only.is_empty() || cfg().only.iter().any(|s| s == section)
}

#[derive(Clone, Copy, Debug)]
struct Stats {
    med: f64,
    min: f64,
    max: f64,
}

impl Stats {
    fn of(mut v: Vec<f64>) -> Stats {
        v.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
        let n = v.len();
        let med = if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 };
        Stats { med, min: v[0], max: v[n - 1] }
    }
    /// `f` applied to each figure (`f` must be non-decreasing).
    fn map(self, f: impl Fn(f64) -> f64) -> Stats {
        Stats { med: f(self.med), min: f(self.min), max: f(self.max) }
    }
}

/// Times `run` on inputs `setup` makes outside the clock; ns per run. Batches runs so a sample
/// lasts at least `cfg().sample`; what `run` returns is dropped after the clock stops.
fn time_with<S, T>(mut setup: impl FnMut() -> S, mut run: impl FnMut(S) -> T) -> Stats {
    let s = setup();
    let t = Instant::now();
    let out = run(s);
    let probe = t.elapsed().as_nanos().max(1) as f64;
    drop(black_box(out));
    let batch = ((cfg().sample.as_nanos() as f64 / probe).ceil() as usize).clamp(1, 20_000);
    let mut samples = Vec::with_capacity(cfg().reps);
    for i in 0..cfg().warmup + cfg().reps {
        let inputs: Vec<S> = (0..batch).map(|_| setup()).collect();
        let mut outs = Vec::with_capacity(batch);
        let t = Instant::now();
        for s in inputs {
            outs.push(run(black_box(s)));
        }
        let elapsed = t.elapsed().as_nanos() as f64;
        drop(black_box(outs));
        if i >= cfg().warmup {
            samples.push(elapsed / batch as f64);
        }
    }
    Stats::of(samples)
}

fn time<T>(mut run: impl FnMut() -> T) -> Stats {
    time_with(|| (), |()| run())
}

/// One run, timed once (for sizing).
fn once<T>(run: impl FnOnce() -> T) -> f64 {
    let t = Instant::now();
    drop(black_box(run()));
    t.elapsed().as_nanos() as f64
}

fn short(kind: EngineKind) -> &'static str {
    match kind {
        EngineKind::Soroban => "soroban-wasmi",
        #[cfg(feature = "ref038")]
        EngineKind::Wasmi038 => "wasmi 0.38",
    }
}

fn engines() -> &'static [EngineKind] {
    EngineKind::ALL
}

// ---- formatting ----------------------------------------------------------------------------

fn n0(x: f64) -> String {
    let s = format!("{:.0}", x.abs());
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if x < 0.0 { format!("−{out}") } else { out }
}

fn f2(x: f64) -> String {
    if x.abs() >= 100.0 {
        n0(x)
    } else if x.abs() >= 10.0 {
        format!("{x:.1}")
    } else {
        format!("{x:.2}")
    }
}

/// `median [min–max]`.
fn range(s: Stats) -> String {
    format!("{} [{}–{}]", f2(s.med), f2(s.min), f2(s.max))
}

fn us(ns: f64) -> String {
    f2(ns / 1_000.0)
}

static OUT: Mutex<String> = Mutex::new(String::new());

fn emit(s: impl AsRef<str>) {
    let s = s.as_ref();
    println!("{s}");
    let mut out = OUT.lock().expect("not poisoned");
    out.push_str(s);
    out.push('\n');
}

fn progress(s: impl AsRef<str>) {
    eprintln!("  .. {}", s.as_ref());
}

fn table(header: &[&str], rows: &[Vec<String>]) {
    emit(format!("| {} |", header.join(" | ")));
    emit(format!("|{}|", header.iter().enumerate().map(|(i, _)| if i == 0 { "---" } else { "---:" }).collect::<Vec<_>>().join("|")));
    for r in rows {
        emit(format!("| {} |", r.join(" | ")));
    }
    emit("");
}

// ---- modules -------------------------------------------------------------------------------

/// WAT to a program's bytes: custom sections stripped (the `wat` crate writes a `name` section
/// whenever the text uses `$ids`, and `validate` refuses custom sections, as uploads would).
fn wat(src: &str) -> Vec<u8> {
    let wasm = wat::parse_str(src).unwrap_or_else(|e| panic!("bad WAT: {e}"));
    strip(&wasm)
}

fn strip(wasm: &[u8]) -> Vec<u8> {
    let mut out = wasm[..8].to_vec();
    let mut at = 8;
    while at < wasm.len() {
        let id = wasm[at];
        let (len, n) = leb(&wasm[at + 1..]);
        let end = at + 1 + n + len as usize;
        if id != 0 {
            out.extend_from_slice(&wasm[at..end]);
        }
        at = end;
    }
    out
}

fn leb(b: &[u8]) -> (u64, usize) {
    let (mut v, mut shift) = (0u64, 0);
    for (i, byte) in b.iter().enumerate() {
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return (v, i + 1);
        }
        shift += 7;
    }
    panic!("bad LEB")
}

/// A program that must pass upload validation, or the measurement means nothing.
fn program(what: &str, src: &str) -> Vec<u8> {
    let wasm = wat(src);
    if let Err(e) = validate(&wasm) {
        panic!("{what}: validate refused it: {e}");
    }
    wasm
}

/// The largest `n` (near enough) for which `make(n)` stays within `limit` bytes; sizes grow about
/// linearly in `n`. `make` is only ever called with `n ≥ 256`, so builders needn't be valid for
/// tiny `n`.
fn fit(limit: usize, make: impl Fn(usize) -> Vec<u8>) -> (usize, Vec<u8>) {
    let (lo, hi) = (256usize, 1024usize);
    let (a, b) = (make(lo).len() as f64, make(hi).len() as f64);
    let per = ((b - a) / (hi - lo) as f64).max(0.25);
    let base = a - per * lo as f64;
    let mut n = (((limit as f64 - base) / per) as usize).max(lo);
    loop {
        let wasm = make(n);
        if wasm.len() <= limit {
            return (n, wasm);
        }
        let step = ((wasm.len() - limit) as f64 / per).ceil().max(1.0) as usize;
        n = n.saturating_sub(step).max(1);
    }
}

const IMPORTS: &str = r#"
  (import "blockula" "input_len_v1" (func $input_len (result i32)))
  (import "blockula" "input_read_v1" (func $input_read (param i32)))
  (import "blockula" "return_data_v1" (func $return_data (param i32 i32)))
  (import "blockula" "abort_v1" (func $abort (param i32 i32 i32)))
  (import "blockula" "debug_log_v1" (func $debug_log (param i32 i32)))
  (import "blockula" "storage_read_v1" (func $storage_read (param i32 i32 i32 i32) (result i32)))
  (import "blockula" "storage_write_v1" (func $storage_write (param i32 i32 i32 i32 i32)))
  (import "blockula" "storage_remove_v1" (func $storage_remove (param i32 i32) (result i32)))
  (import "blockula" "self_address_v1" (func $self_address (param i32)))
  (import "blockula" "caller_v1" (func $caller (param i32)))
  (import "blockula" "value_v1" (func $value (result i64)))
  (import "blockula" "block_height_v1" (func $block_height (result i64)))
  (import "blockula" "self_balance_v1" (func $self_balance (result i64)))
  (import "blockula" "transfer_v1" (func $transfer (param i32 i64 i32) (result i32)))
  (import "blockula" "call_v1" (func $call (param i32 i32 i32 i32 i32 i32 i64 i64) (result i32)))
  (import "blockula" "try_call_v1" (func $try_call (param i32 i32 i32 i32 i32 i32 i64 i64) (result i32)))
  (import "blockula" "last_return_v1" (func $last_return (param i32)))
  (import "blockula" "last_return_len_v1" (func $last_return_len (result i32)))
  (import "blockula" "last_abort_code_v1" (func $last_abort_code (result i32)))
  (import "blockula" "emit_v1" (func $emit (param i32 i32 i32 i32)))
"#;

/// How many host functions `IMPORTS` declares: all of ABI v1, so module labels can't drift from it.
fn abi_len() -> usize {
    blockula_vm::abi::HOST_FNS.len()
}

// ---- the world -----------------------------------------------------------------------------

fn signer() -> Address {
    addr("signer")
}

fn world() -> MemHost {
    let mut host = MemHost::new();
    host.fund(signer(), 100_000_000_000_000_000);
    host
}

fn call(program: Address, method: &str, input: Vec<u8>) -> Call {
    Call { program, method: method.into(), input, value: 0 }
}

fn run_tx(engine: &dyn BlockEngine, host: &MemHost, calls: &[Call], declared: &[Address], fuel: u64) -> Receipt {
    execute(engine, host, signer(), calls, declared, TxLimits { fuel_limit: fuel, storage_limit: 1 << 60 }, &V1)
}

fn ok(r: &Receipt, what: &str) {
    assert!(r.ok, "{what}: {:?}", r.fail);
}

/// A host for raw `BlockEngine::invoke` timing: programs that call it are refused.
struct NullHost;

impl Host for NullHost {
    fn call(&mut self, _: usize, _: &[i64], _: &mut dyn Instance) -> Result<i64, Trap> {
        Err(Trap::Fault("no host here".into()))
    }
}

// ---- 1. ns per fuel by operator class -------------------------------------------------------

struct Op {
    class: &'static str,
    name: String,
    extra: String,
    locals: String,
    setup: String,
    pre: String,
    unit: String,
    post: String,
    k: usize,
    pages: u32,
}

impl Op {
    fn new(class: &'static str, name: impl Into<String>, unit: impl Into<String>, k: usize) -> Op {
        Op {
            class,
            name: name.into(),
            extra: String::new(),
            locals: String::new(),
            setup: String::new(),
            pre: String::new(),
            unit: unit.into(),
            post: String::new(),
            k,
            pages: 1,
        }
    }
    fn chain(mut self, pre: &str, post: &str) -> Op {
        self.pre = pre.into();
        self.post = post.into();
        self
    }
    fn extra(mut self, extra: impl Into<String>) -> Op {
        self.extra = extra.into();
        self
    }
    fn setup(mut self, setup: impl Into<String>) -> Op {
        self.setup = setup.into();
        self
    }
    fn locals(mut self, locals: impl Into<String>) -> Op {
        self.locals = locals.into();
        self
    }
    fn pages(mut self, pages: u32) -> Op {
        self.pages = pages;
        self
    }

    /// `bk_run` reads N (u32 LE) and runs `pre (unit × k) post` N times.
    fn module(&self) -> Vec<u8> {
        let body = self.unit.repeat(self.k);
        let src = format!(
            r#"(module
  (import "blockula" "input_read_v1" (func $input_read (param i32)))
  (memory (export "memory") {pages} {pages})
  {extra}
  (func (export "bk_run")
    (local $n i32) (local $a i32) (local $b i32) (local $c i32) (local $d i32) (local $one i32) (local $zero i32)
    (local $r i32) (local $p i32) (local $i i32)
    (local $x i64) (local $y i64) (local $z i64) (local $w i64) (local $one64 i64) {locals}
    (call $input_read (i32.const 0))
    (local.set $n (i32.load (i32.const 0)))
    (local.set $one (i32.const 1)) (local.set $one64 (i64.const 1))
    (local.set $a (i32.const -1)) (local.set $b (i32.const 3)) (local.set $c (i32.const 0x7fffffff)) (local.set $d (i32.const 5))
    (local.set $x (i64.const -1)) (local.set $y (i64.const 3)) (local.set $z (i64.const 0x7fffffffffffffff)) (local.set $w (i64.const 5))
    (local.set $r (i32.const 12345)) (local.set $p (i32.const 8))
    (i32.store (i32.const 8) (i32.const 8))
    {setup}
    (block $done
      (loop $l
        (br_if $done (i32.eqz (local.get $n)))
        {pre} {body} {post}
        (local.set $n (i32.sub (local.get $n) (i32.const 1)))
        (br $l)))))"#,
            pages = self.pages,
            extra = self.extra,
            locals = self.locals,
            setup = self.setup,
            pre = self.pre,
            post = self.post,
        );
        program(&self.name, &src)
    }
}

/// An LCG step on `$r`, leaving the new value on the stack (9 fuel of base operators).
const LCG: &str = "(local.tee $r (i32.add (i32.mul (local.get $r) (i32.const 1103515245)) (i32.const 12345)))";

fn random_index(mask: u32) -> String {
    format!("(i32.and (i32.shr_u {LCG} (i32.const 16)) (i32.const {mask}))")
}

/// `t` nested blocks and a `br_table` to every one of them (the last also the default).
fn br_table_unit(t: usize, index: &str) -> String {
    let targets: Vec<String> = (0..t).map(|i| i.to_string()).collect();
    format!("{}(br_table {} {index}){}", "(block ".repeat(t), targets.join(" "), ")".repeat(t))
}

/// A cyclic pointer chain through `lines` 64-byte lines from address 0, in LCG order (full
/// period: Hull–Dobell), so `p = load(p)` visits every line.
fn chase_setup(lines: u32) -> String {
    format!(
        "(local.set $i (i32.const 0))
         (loop $s
           (i32.store (i32.shl (local.get $i) (i32.const 6))
             (i32.shl (i32.and (i32.add (i32.mul (local.get $i) (i32.const 1103515245)) (i32.const 12345)) (i32.const {})) (i32.const 6)))
           (local.set $i (i32.add (local.get $i) (i32.const 1)))
           (br_if $s (i32.lt_u (local.get $i) (i32.const {lines}))))
         (local.set $p (i32.const 0))",
        lines - 1
    )
}

fn i64s(n: usize) -> String {
    "i64 ".repeat(n)
}

fn ops() -> Vec<Op> {
    let callees = format!(
        "(type $t0 (func))
         (func $f0)
         (func $l8 (local {}))
         (func $l64 (local {}))
         (func $l256 (local {}))
         (func $l1024 (local {}))
         (func $p32 (param {}))
         (func $p32l992 (param {}) (local {}))
         (func $ret (param i64) (result i64) (i64.add (local.get 0) (i64.const 1)))
         (func $rec (param i32) (local {}) (if (local.get 0) (then (call $rec (i32.sub (local.get 0) (i32.const 1))))))",
        i64s(8),
        i64s(64),
        i64s(256),
        i64s(1024),
        "i32 ".repeat(32),
        "i32 ".repeat(32),
        i64s(992),
        i64s(1023)
    );
    let table = |f: &str| format!("{callees} (table 4096 4096 funcref) (elem (i32.const 0) func {})", format!("{f} ").repeat(4096));
    let args32 = "(local.get $a) ".repeat(32);
    let globals = "(global $sp (mut i32) (i32.const 65536)) (global $g (mut i32) (i32.const 0)) (global $h (mut i32) (i32.const 0))";
    let bin32 = ["add", "sub", "mul", "and", "or", "xor", "shl", "shr_u", "shr_s", "rotl", "rotr"];
    let bin32_local: String = bin32.iter().map(|o| format!("local.get $b i32.{o} ")).collect();
    let bin64_local: String = bin32.iter().map(|o| format!("local.get $y i64.{o} ")).collect();
    let bin32_const: String = bin32.iter().map(|o| format!("i32.const 7 i32.{o} ")).collect();
    vec![
        // Control.
        Op::new("control", "empty loop (the loop's own overhead)", "", 0),
        Op::new("control", "br to the next block end: (block (br 0))", "(block (br 0)) ", 64),
        Op::new("control", "br_if not taken", "(br_if 0 (local.get $zero)) ", 64),
        Op::new("control", "br_if to the function's end, not taken", "(br_if 2 (local.get $zero)) ", 64),
        Op::new("control", "empty loops: (loop)", "(loop) ", 64),
        Op::new("control", "if/else, unpredictable", format!("(if (i32.lt_s {LCG} (i32.const 0)) (then (local.set $a (local.get $b))) (else (local.set $a (local.get $c)))) "), 16),
        Op::new("br_table", "br_table 8 targets, fixed index", br_table_unit(8, "(local.get $zero)"), 16),
        Op::new("br_table", "br_table 64 targets, random index", br_table_unit(64, &random_index(63)), 4),
        Op::new("br_table", "br_table 1,024 targets, random index", br_table_unit(1024, &random_index(1023)), 1),
        Op::new("br_table", "br_table 4,096 targets, random index", br_table_unit(4096, &random_index(4095)), 1),
        Op::new("br_table", "br_table 4,096 entries to one label, random index", format!("(block (br_table {} {})) ", "0 ".repeat(4096), random_index(4095)), 1),
        // Calls.
        Op::new("call", "call () -> (), no locals", "(call $f0) ", 16).extra(callees.clone()),
        Op::new("call", "call, 8 locals", "(call $l8) ", 16).extra(callees.clone()),
        Op::new("call", "call, 64 locals", "(call $l64) ", 16).extra(callees.clone()),
        Op::new("call", "call, 256 locals", "(call $l256) ", 16).extra(callees.clone()),
        Op::new("call", "call, 1,024 locals", "(call $l1024) ", 16).extra(callees.clone()),
        Op::new("call", "call, 32 params", format!("(call $p32 {args32}) "), 8).extra(callees.clone()),
        Op::new("call", "call, 32 params + 992 locals", format!("(call $p32l992 {args32}) "), 8).extra(callees.clone()),
        Op::new("call", "call i64 -> i64", "(local.set $x (call $ret (local.get $x))) ", 16).extra(callees.clone()),
        Op::new("call", "recursion 64 deep, 1,024 locals a frame", "(call $rec (i32.const 64)) ", 1).extra(callees.clone()),
        Op::new("call", "call_indirect, fixed index", "(call_indirect (type $t0) (local.get $zero)) ", 16).extra(table("$f0")),
        Op::new("call", "call_indirect, random index of 4,096", format!("(call_indirect (type $t0) {}) ", random_index(4095)), 8).extra(table("$f0")),
        Op::new("call", "call_indirect, 1,024 locals", "(call_indirect (type $t0) (local.get $zero)) ", 16).extra(table("$l1024")),
        // Integer arithmetic.
        Op::new("i32 arith", "i32 unary chain (clz ctz popcnt eqz)", "i32.clz i32.ctz i32.popcnt i32.eqz ", 16).chain("local.get $a", "local.set $a"),
        Op::new("i64 arith", "i64 unary chain (clz ctz popcnt)", "i64.clz i64.ctz i64.popcnt ", 16).chain("local.get $x", "local.set $x"),
        Op::new("i32 arith", "i32 binary chain, local operand", bin32_local, 4).chain("local.get $a", "local.set $a"),
        Op::new("i64 arith", "i64 binary chain, local operand", bin64_local, 4).chain("local.get $x", "local.set $x"),
        Op::new("i32 arith", "i32 binary chain, constant operand", bin32_const, 4).chain("local.get $a", "local.set $a"),
        Op::new("i32 arith", "i32 3-address: a = b op c", "(local.set $a (i32.add (local.get $b) (local.get $c))) (local.set $d (i32.mul (local.get $a) (local.get $b))) ", 16),
        Op::new("i64 arith", "i64.mul chain", "local.get $y i64.mul ", 64).chain("local.get $x", "local.set $x"),
        Op::new("div/rem", "i32.div_u by 1, chain", "local.get $one i32.div_u ", 64).chain("local.get $a", "local.set $a"),
        Op::new("div/rem", "i32.div_s by 1, chain", "local.get $one i32.div_s ", 64).chain("local.get $c", "local.set $c"),
        Op::new("div/rem", "i32.rem_u, xor chain", "local.get $c i32.rem_u i32.const -1 i32.xor ", 32).chain("local.get $a", "local.set $a"),
        Op::new("div/rem", "i64.div_u by 1, chain", "local.get $one64 i64.div_u ", 64).chain("local.get $x", "local.set $x"),
        Op::new("div/rem", "i64.div_s by 1, chain", "local.get $one64 i64.div_s ", 64).chain("local.get $z", "local.set $z"),
        Op::new("div/rem", "i64.div_u by 3, xor chain", "local.get $y i64.div_u i64.const -1 i64.xor ", 32).chain("local.get $x", "local.set $x"),
        Op::new("div/rem", "i64.rem_u, xor chain", "local.get $z i64.rem_u i64.const -1 i64.xor ", 32).chain("local.get $x", "local.set $x"),
        Op::new("div/rem", "i64.rem_s, xor chain", "local.get $z i64.rem_s i64.const -1 i64.xor ", 32).chain("local.get $x", "local.set $x"),
        Op::new("compare", "i32 compare chain", "local.get $b i32.lt_u local.get $b i32.ge_s local.get $b i32.eq local.get $b i32.ne ", 16).chain("local.get $a", "local.set $a"),
        Op::new("compare", "i64 compare chain", "local.get $y i64.lt_u i64.extend_i32_u local.get $y i64.ge_s i64.extend_i32_u ", 16).chain("local.get $x", "local.set $x"),
        Op::new("compare", "i32.eqz chain", "i32.eqz ", 64).chain("local.get $a", "local.set $a"),
        Op::new("compare", "wrap/extend chain", "i32.wrap_i64 i64.extend_i32_s i32.wrap_i64 i64.extend_i32_u ", 16).chain("local.get $x", "local.set $x"),
        Op::new("select", "select chain", "local.get $b local.get $c select ", 32).chain("local.get $a", "local.set $a"),
        Op::new("select", "select, unpredictable condition", format!("(local.set $a (select (local.get $b) (local.get $d) (i32.lt_s {LCG} (i32.const 0)))) "), 16),
        // Locals.
        Op::new("locals", "local.get/local.set copies", "local.get $a local.set $b local.get $b local.set $c local.get $c local.set $a ", 16),
        Op::new("locals", "local.tee chain", "local.tee $b local.tee $c local.tee $d local.tee $a ", 16).chain("local.get $a", "local.set $a"),
        Op::new("locals", "far locals (1,024 declared)", "local.get 1000 local.set 600 local.get 600 local.set 1000 ", 16).locals(format!("(local {})", i64s(1009))),
        // Globals.
        Op::new("globals", "global.get/global.set copies", "global.get $g global.set $h global.get $h global.set $g ", 16).extra(globals),
        Op::new("globals", "global increment", "global.get $g i32.const 1 i32.add global.set $g ", 16).extra(globals),
        Op::new("globals", "global.get; drop", "global.get $g drop ", 32).extra(globals),
        // Memory.
        Op::new("load", "i32.load chain, one address", "i32.load ", 64).chain("local.get $p", "local.set $p"),
        Op::new("load", "narrow loads chain, one address", "i32.load i32.load8_u i32.load16_u i32.load8_s i32.load16_s ", 16).chain("local.get $p", "local.set $p"),
        Op::new("load", "pointer chase, 64 KiB, random", "i32.load ", 64).chain("local.get $p", "local.set $p").setup(chase_setup(1024)),
        Op::new("load", "pointer chase, 2 MiB, random", "i32.load ", 64).chain("local.get $p", "local.set $p").setup(chase_setup(32_768)).pages(32),
        Op::new("load", "random independent i64 loads, 2 MiB", format!("(local.set $x (i64.load (i32.and {LCG} (i32.const 0x1ffff8)))) "), 16).pages(32),
        Op::new("store", "i64.store, one address", "(i64.store (local.get $p) (local.get $x)) ", 32),
        Op::new("store", "narrow stores, one address", "(i32.store8 (local.get $p) (local.get $a)) (i32.store16 (local.get $p) (local.get $a)) (i64.store32 (local.get $p) (local.get $x)) ", 16),
        Op::new("store", "random i64 stores, 2 MiB", format!("(i64.store (i32.and {LCG} (i32.const 0x1ffff8)) (local.get $x)) "), 16).pages(32),
        Op::new("memory", "memory.size; drop", "memory.size drop ", 32),
    ]
}

struct OpResult {
    class: &'static str,
    name: String,
    /// Per engine: fuel per iteration, fuel per unit, ns per fuel.
    per: Vec<(EngineKind, u64, f64, Stats)>,
}

fn op_world(wasm: &[u8]) -> (MemHost, Address) {
    let mut host = world();
    let p = addr("op");
    host.deploy(p, wasm);
    (host, p)
}

fn op_input(n: u32) -> Vec<u8> {
    n.to_le_bytes().to_vec()
}

/// Runs a loop case: fuel per iteration, and ns per fuel (from N iterations minus none).
fn measure_loop(engine: &dyn BlockEngine, host: &MemHost, p: Address, method: &str, input: impl Fn(u32) -> Vec<u8>, declared: &[Address], what: &str) -> (u64, Stats) {
    let rx = |n: u32| run_tx(engine, host, &[call(p, method, input(n))], declared, UNLIMITED);
    let (r0, r1) = (rx(0), rx(1));
    ok(&r0, what);
    ok(&r1, what);
    let per = r1.fuel_used - r0.fuel_used;
    let mut n = (2_000_000 / per.max(1)).max(1) as u32;
    let t = once(|| rx(n));
    n = ((n as f64 * cfg().run.as_nanos() as f64 / t).max(1.0)) as u32;
    let rn = rx(n);
    ok(&rn, what);
    assert_eq!(rn.fuel_used, r0.fuel_used + u64::from(n) * per, "{what}: fuel not linear in N");
    let base = time(|| rx(0)).med;
    let tn = time(|| rx(n));
    (per, tn.map(|t| (t - base) / f64::from(n) / per as f64))
}

fn section_ops() -> Vec<OpResult> {
    emit("## 1. ns per fuel by operator class\n");
    emit("Each case is a loop whose body is `k` copies of one unit dominated by one class (plus the loop's own fuel: `br_if`, a decrement, `br`). ns per fuel = (time of N iterations − time of 0) / N / (fuel per iteration), so instantiation and the transaction's fixed cost cancel. Fuel per unit = fuel per iteration / k, under each engine's mapping of schedule v1 (wasmi 0.38: its own IR fuel). Cells: **ns/fuel median [min–max]** (fuel per unit). Values above 2.5 ns/fuel are marked **bold**.\n");
    let mut results = Vec::new();
    for op in ops() {
        progress(format!("ops: {}", op.name));
        let wasm = op.module();
        let (host, p) = op_world(&wasm);
        let mut per = Vec::new();
        for &kind in engines() {
            let engine = host.engine(kind, &V1).expect("loads");
            let (fuel, s) = measure_loop(engine.as_ref(), &host, p, "run", op_input, &[], &op.name);
            let units = if op.k == 0 { 1.0 } else { op.k as f64 };
            per.push((kind, fuel, fuel as f64 / units, s));
        }
        results.push(OpResult { class: op.class, name: op.name, per });
    }
    let mut header = vec!["class", "case"];
    let names: Vec<String> = engines().iter().map(|k| short(*k).to_string()).collect();
    header.extend(names.iter().map(String::as_str));
    let rows: Vec<Vec<String>> = results
        .iter()
        .map(|r| {
            let mut row = vec![r.class.to_string(), r.name.clone()];
            for (_, _, unit, s) in &r.per {
                let cell = format!("{} ({})", range(*s), f2(*unit));
                row.push(if s.med > NS_PER_FUEL { format!("**{cell}**") } else { cell });
            }
            row
        })
        .collect();
    table(&header, &rows);
    emit("**Worst cases per engine** (median ns/fuel; the five highest):\n");
    let mut rows = Vec::new();
    for (i, &kind) in engines().iter().enumerate() {
        let mut v: Vec<(&str, Stats, f64)> = results.iter().map(|r| (r.name.as_str(), r.per[i].3, r.per[i].2)).collect();
        v.sort_by(|a, b| b.1.med.partial_cmp(&a.1.med).expect("no NaN"));
        for (rank, (name, s, unit)) in v.iter().take(5).enumerate() {
            rows.push(vec![
                if rank == 0 { short(kind).to_string() } else { String::new() },
                name.to_string(),
                range(*s),
                f2(*unit),
                format!("{:.1}×", s.med / NS_PER_FUEL),
            ]);
        }
    }
    table(&["engine", "case", "ns/fuel", "fuel/unit", "vs 2.5 ns"], &rows);
    // The reference engine more than 2× apart from the node's (with `--features ref038`).
    if let [first, second, ..] = engines() {
        let mut rows = Vec::new();
        for r in &results {
            let (a, b) = (r.per[0].3.med, r.per[1].3.med);
            if a.max(b) / a.min(b).max(1e-9) > 2.0 && a.max(b) > 0.5 {
                rows.push(vec![r.name.clone(), f2(a), f2(b), format!("{:.1}×", a.max(b) / a.min(b))]);
            }
        }
        if !rows.is_empty() {
            emit(format!("**{} and {} more than 2× apart** (ns/fuel, where either is above 0.5):\n", short(*first), short(*second)));
            table(&["case", short(*first), short(*second), "ratio"], &rows);
        }
    }
    results
}

// ---- 2. translation and validation ----------------------------------------------------------

/// The modules translation is timed on: the guests, then generated ones near the limits.
fn translation_corpus() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/programs/");
    let mut corpus: Vec<(String, Vec<u8>)> = ["counter", "token", "escrow", "router"]
        .iter()
        .map(|g| (format!("guest {g}.wasm"), std::fs::read(format!("{dir}{g}.wasm")).expect("guest committed")))
        .collect();
    const MAX: usize = blockula_vm::config::MAX_CODE_BYTES;
    let mem = r#"(memory (export "memory") 1 1)"#;
    let module = |body: &str| format!("(module {mem} {body} (func (export \"bk_run\")))");
    let mut add = |name: &str, wasm: Vec<u8>| {
        if let Err(e) = validate(&wasm) {
            panic!("{name}: validate refused it: {e}");
        }
        corpus.push((name.to_string(), wasm));
    };
    add("minimal program", wat(&module("")));
    add(&format!("minimal program + {} imports", abi_len()), wat(&format!("(module {IMPORTS} {mem} (func (export \"bk_run\")))")));
    let unit = "local.get 0 i64.const 1234567 i64.add local.set 0 ";
    let (_, w) = fit(MAX, |n| wat(&module(&format!("(func (local i64) {})", unit.repeat(n)))));
    add("one function, straight-line i64 arithmetic", w);
    let small = format!("(func {})", "i64.const 0 i64.const 0 i64.add drop ".repeat(8));
    let (n, w) = fit(MAX, |n| wat(&module(&small.repeat(n))));
    add(&format!("{n} small functions (≈40 B bodies)"), w);
    let locals = format!("(func (local {}) {})", i64s(1024), "i64.const 0 i64.const 0 i64.add drop ".repeat(6));
    let (n, w) = fit(MAX, |n| wat(&module(&locals.repeat(n))));
    add(&format!("{n} functions with 1,024 locals each"), w);
    let (n, w) = fit(MAX, |n| wat(&module(&format!("(func (block (br_table {} (i32.const 0))))", "0 ".repeat(n)).repeat(2))));
    add(&format!("br_table, 2 × {n} targets to one label"), w);
    let tbl = br_table_unit(4096, "(i32.const 0)");
    let (n, w) = fit(MAX, |n| wat(&module(&format!("(func {})", tbl.repeat(n)))));
    add(&format!("br_table, {n} × 4,096 distinct nested targets"), w);
    let (n, w) = fit(MAX, |n| wat(&module(&format!("(func {}{})", "block ".repeat(n), "end ".repeat(n)))));
    add(&format!("{n} nested blocks"), w);
    let (n, w) = fit(MAX, |n| wat(&module(&format!("(func (param i32) {})", "(if (local.get 0) (then nop) (else nop)) ".repeat(n)))));
    add(&format!("{n} if/else"), w);
    let (n, w) = fit(MAX, |n| wat(&module(&format!("(func {})", (0..n).map(|i| format!("i64.const {} drop ", 1_000_000_007u64 * (i as u64 + 1))).collect::<String>()))));
    add(&format!("{n} distinct i64 constants"), w);
    // A big data segment needs the pages to hold it (32 pages = 2 MiB), plus matching code.
    let data32 = |body: &str| format!("(module (memory (export \"memory\") 32 32) {body} (func (export \"bk_run\")))");
    let data = |n: usize| format!("(data (i32.const 0) \"{}\")", "\\00".repeat(n));
    let filler = |n: usize| format!("(func {})", "i64.const 0 drop ".repeat(n / 3 + 1));
    let (n, w) = fit(MAX, |n| wat(&data32(&format!("{} {}", data(n), filler(n)))));
    add(&format!("{n} B of data + as much code (32 pages)"), w);
    let (n, w) = fit(MAX, |n| {
        let mut types = String::new();
        let mut count = 0;
        'all: for len in 0..=32usize {
            for bits in 0..(1u64 << len.min(20)) {
                if count == n {
                    break 'all;
                }
                let params: String = (0..len).map(|i| if bits >> (i % 20) & 1 == 1 { "i64 " } else { "i32 " }).collect();
                types.push_str(&format!("(type (func (param {params})))"));
                count += 1;
            }
        }
        wat(&module(&types))
    });
    add(&format!("{n} distinct function types"), w);
    let (n, w) = fit(MAX, |n| wat(&module(&"(type (func))".repeat(n))));
    add(&format!("{n} identical function types"), w);
    let (n, w) = fit(MAX, |n| wat(&format!("(module {mem} (func $f) {})", (0..n).map(|i| format!("(export \"bk_e{i}\" (func $f))")).collect::<String>())));
    add(&format!("{n} exports of one function"), w);
    let callees = format!("(func $f {})", "i64.const 0 drop ".repeat(14));
    let (n, w) = fit(MAX, |n| wat(&module(&format!("{callees} (func {})", "call $f ".repeat(n)))));
    add(&format!("{n} calls"), w);
    let (n, w) = fit(MAX, |n| wat(&module(&format!("(func (param i32) (local {}) {})", i64s(1022), "local.get 1000 local.set 999 ".repeat(n)))));
    add(&format!("{n} far local.get/set pairs (1,024 locals)"), w);
    // Deep operand stacks of local references and local.sets over them.
    let pattern = format!("{}{}{}drop ", "local.get 0 ".repeat(4000), "local.get 1 local.set 0 ", "i32.add ".repeat(3999));
    let (n, w) = fit(MAX, |n| wat(&module(&format!("(func (param i32 i32) {})", pattern.repeat(n)))));
    add(&format!("{n} × (4,000 local.gets, local.set over them, 3,999 adds)"), w);
    let pattern = format!("{}{}{}drop ", "local.get 0 ".repeat(4000), "local.get 1 local.set 1 ".repeat(1000), "i32.add ".repeat(3999));
    let (n, w) = fit(MAX, |n| wat(&module(&format!("(func (param i32 i32) {})", pattern.repeat(n)))));
    add(&format!("{n} × (4,000-deep stack, 1,000 local.sets)"), w);
    let (n, w) = fit(MAX, |n| wat(&module(&format!("(func unreachable {})", "i64.const 0 i64.const 0 i64.add drop ".repeat(n)))));
    add(&format!("{n} dead-code units after unreachable"), w);
    let globals = "(global (mut i64) (i64.const 0))".repeat(256);
    let (n, w) = fit(MAX, |n| wat(&module(&format!("{globals} (func {})", "global.get 200 global.set 100 ".repeat(n)))));
    add(&format!("256 globals, {n} global.get/set pairs"), w);
    corpus
}

struct LoadResult {
    name: String,
    size: usize,
    validate: Stats,
    /// Per engine: load (`BlockEngine::load` on a fresh engine), then the engine's drop.
    load: Vec<(EngineKind, Stats, Stats)>,
}

fn time_load(kind: EngineKind, wasm: &[u8]) -> (Stats, Stats) {
    let hash: CodeHash = *blake3::hash(wasm).as_bytes();
    let load = time_with(
        || new_block_engine(kind, &V1),
        |mut e| {
            e.load(hash, wasm).expect("loads");
            e
        },
    );
    // The drop of an engine holding the module, less an empty engine's.
    let empty = time_with(|| new_block_engine(kind, &V1), drop);
    let full = time_with(
        || {
            let mut e = new_block_engine(kind, &V1);
            e.load(hash, wasm).expect("loads");
            e
        },
        drop,
    );
    (load, full.map(|t| t - empty.med))
}

fn section_translate() -> Vec<LoadResult> {
    emit("## 2. Translation and validation, ns per byte\n");
    emit("Cold: each load is `BlockEngine::load` (a structural read of memory and data, then the engine's `Module::new`: its own parse, validation and translation) on a fresh block engine made outside the clock, as a block loads every declared code into a fresh engine (4.3). `validate` is upload validation (`validate.rs`, wasmparser 0.228 plus our rules). CPU caches are warm (the same bytes are loaded repeatedly), so these are lower bounds for a cold machine. *Implied fuel* = time / 2.5 ns. **Implied LOAD_PER_BYTE** = (load time / 2.5 ns − LOAD_BASE 2,000) / bytes, against schedule v1's 10; **implied LOAD_BASE** comes from the smallest modules.\n");
    let mut results = Vec::new();
    for (name, wasm) in translation_corpus() {
        progress(format!("translate: {name} ({} B)", wasm.len()));
        let v = time(|| validate(&wasm).expect("valid"));
        let mut load = Vec::new();
        for &kind in engines() {
            let (l, d) = time_load(kind, &wasm);
            load.push((kind, l, d));
        }
        results.push(LoadResult { name, size: wasm.len(), validate: v, load });
    }
    let mut header = vec!["module".to_string(), "bytes".into(), "validate µs (ns/B)".into()];
    for k in engines() {
        header.push(format!("{} load µs (ns/B)", short(*k)));
    }
    for k in engines() {
        header.push(format!("{} implied LOAD_PER_BYTE", short(*k)));
    }
    let rows: Vec<Vec<String>> = results
        .iter()
        .map(|r| {
            let b = r.size as f64;
            let mut row = vec![r.name.clone(), n0(b), format!("{} ({})", us(r.validate.med), f2(r.validate.med / b))];
            for (_, l, _) in &r.load {
                row.push(format!("{} ({})", us(l.med), f2(l.med / b)));
            }
            for (_, l, _) in &r.load {
                let per = (l.med / NS_PER_FUEL - V1.load.load_base as f64) / b;
                let cell = f2(per.max(0.0));
                row.push(if per > V1.load.load_per_byte as f64 { format!("**{cell}**") } else { cell });
            }
            row
        })
        .collect();
    let h: Vec<&str> = header.iter().map(String::as_str).collect();
    table(&h, &rows);
    // Spread and drop costs.
    let mut header = vec!["module".to_string()];
    for k in engines() {
        header.push(format!("{} load [min–max] µs", short(*k)));
        header.push(format!("{} drop µs", short(*k)));
    }
    header.push("validate [min–max] µs".into());
    let rows: Vec<Vec<String>> = results
        .iter()
        .map(|r| {
            let mut row = vec![r.name.clone()];
            for (_, l, d) in &r.load {
                row.push(format!("[{}–{}]", us(l.min), us(l.max)));
                row.push(us(d.med));
            }
            row.push(format!("[{}–{}]", us(r.validate.min), us(r.validate.max)));
            row
        })
        .collect();
    emit("Spread, and the drop of an engine holding the module (less an empty engine's drop): part of the block's cost, unpriced.\n");
    let h: Vec<&str> = header.iter().map(String::as_str).collect();
    table(&h, &rows);
    // Summary: the worst per byte, and the fixed cost.
    let mut rows = Vec::new();
    for (i, &kind) in engines().iter().enumerate() {
        let worst = results
            .iter()
            .max_by(|a, b| (a.load[i].1.med / a.size as f64).partial_cmp(&(b.load[i].1.med / b.size as f64)).expect("no NaN"))
            .expect("a corpus");
        let big: Vec<&LoadResult> = results.iter().filter(|r| r.size > 100_000).collect();
        let worst_big = big.iter().max_by(|a, b| (a.load[i].1.med / a.size as f64).partial_cmp(&(b.load[i].1.med / b.size as f64)).expect("no NaN")).expect("big modules");
        let minimal = results.iter().find(|r| r.name == "minimal program").expect("minimal");
        let per = (worst_big.load[i].1.med / NS_PER_FUEL - V1.load.load_base as f64) / worst_big.size as f64;
        // Upload's worst is its own maximum (validation's cost differs by shape), not the worst load's.
        let upload_per_byte = |r: &LoadResult| (r.validate.med + r.load[i].1.med) / r.size as f64;
        let worst_upload = big.iter().max_by(|a, b| upload_per_byte(a).partial_cmp(&upload_per_byte(b)).expect("no NaN")).expect("big modules");
        rows.push(vec![
            short(kind).to_string(),
            format!("{} ({} ns/B)", worst.name, f2(worst.load[i].1.med / worst.size as f64)),
            format!("{} ({} ns/B)", worst_big.name, f2(worst_big.load[i].1.med / worst_big.size as f64)),
            f2(per),
            format!("{} µs = {} fuel", us(minimal.load[i].1.med), n0(minimal.load[i].1.med / NS_PER_FUEL)),
            format!("{} ({} ns/B = {} fuel/B)", worst_upload.name, f2(upload_per_byte(worst_upload)), f2(upload_per_byte(worst_upload) / NS_PER_FUEL)),
        ]);
    }
    emit("**Summary.** Schedule v1: LOAD_BASE 2,000, LOAD_PER_BYTE 10; upload 50,000 + 20 per byte.\n");
    table(&["engine", "worst ns/B, any size", "worst ns/B, modules > 100 KB", "implied LOAD_PER_BYTE (worst > 100 KB)", "minimal module's load (implied LOAD_BASE)", "worst upload (validate + load), modules > 100 KB"], &rows);
    results
}

// ---- 3. instantiation -----------------------------------------------------------------------

fn inst_corpus() -> Vec<(String, Vec<u8>)> {
    const MAX: usize = blockula_vm::config::MAX_CODE_BYTES;
    let noop = r#"(func $noop (export "bk_noop"))"#;
    let touch = r#"(func (export "bk_touch") (local $i i32)
        (loop $l (i32.store (local.get $i) (local.get $i))
          (local.set $i (i32.add (local.get $i) (i32.const 4096)))
          (br_if $l (i32.lt_u (local.get $i) (i32.const 2097152)))))"#;
    let m = |pages: u32, imports: bool, body: &str| {
        format!("(module {} (memory (export \"memory\") {pages} {pages}) {noop} {body})", if imports { IMPORTS } else { "" })
    };
    let filler = |n: usize| format!("(func {})", "i64.const 0 drop ".repeat(n / 3 + 1));
    let mut c: Vec<(String, Vec<u8>)> = Vec::new();
    let mut add = |name: &str, src: String| c.push((name.to_string(), program(name, &src)));
    add("1 page, no imports", m(1, false, ""));
    add(&format!("1 page, {} imports", abi_len()), m(1, true, ""));
    add("32 pages", m(32, false, ""));
    add(&format!("32 pages, {} imports, + touch every 4 KiB", abi_len()), m(32, true, touch));
    add("data: 1 segment of 120,000 B", m(2, false, &format!("(data (i32.const 0) \"{}\") {}", "\\01".repeat(120_000), filler(120_000))));
    add("data: 1,000 segments of 1 B", m(1, false, &format!("{} {}", (0..1000).map(|i| format!("(data (i32.const {i}) \"x\")")).collect::<String>(), filler(1_000))));
    add("data: 1,000 segments of 120 B", m(2, false, &format!("{} {}", (0..1000).map(|i| format!("(data (i32.const {}) \"{}\")", i * 120, "\\01".repeat(120))).collect::<String>(), filler(120_000))));
    add("256 globals", m(1, false, &"(global (mut i64) (i64.const 7))".repeat(256)));
    add("table of 4,096, 1,000 element segments", m(1, false, &format!("(table 4096 4096 funcref) {}", (0..1000).map(|i| format!("(elem (i32.const {}) func $noop $noop $noop $noop)", i * 4)).collect::<String>())));
    let small = format!("(func {})", "i64.const 0 i64.const 0 i64.add drop ".repeat(8));
    let (n, w) = fit(MAX, |n| wat(&m(1, false, &small.repeat(n))));
    validate(&w).expect("valid");
    c.push((format!("{n} functions"), w));
    let (n, w) = fit(MAX, |n| wat(&m(1, false, &(0..n).map(|i| format!("(export \"bk_e{i}\" (func $noop))")).collect::<String>())));
    validate(&w).expect("valid");
    c.push((format!("{n} exports"), w));
    // Everything at once, within the size limit: imports, 32 pages, globals, the table, functions
    // and as many exports as fit.
    let base = format!(
        "{} {} {} {}",
        "(global (mut i64) (i64.const 7))".repeat(256),
        format_args!("(table 4096 4096 funcref) {}", (0..1000).map(|i| format!("(elem (i32.const {}) func $noop $noop $noop $noop)", i * 4)).collect::<String>()),
        small.repeat(2_000),
        (0..1000).map(|i| format!("(data (i32.const {i}) \"x\")")).collect::<String>(),
    );
    let (n, w) = fit(MAX, |n| wat(&m(32, true, &format!("{base} {}", (0..n).map(|i| format!("(export \"bk_e{i}\" (func $noop))")).collect::<String>()))));
    validate(&w).expect("valid");
    c.push((format!("all: {} imports, 32 pages, 256 globals, table + 1,000 elems, 2,001 funcs, 1,000 data, {n} exports", abi_len()), w));
    c
}

fn caller_program() -> Vec<u8> {
    program(
        "caller",
        &format!(
            r#"(module {IMPORTS} (memory (export "memory") 1 1)
  (data (i32.const 60000) "noop")
  (func (export "bk_run") (local $n i32)
    (call $input_read (i32.const 1024))
    (local.set $n (i32.load (i32.const 1024)))
    (block $d (loop $l (br_if $d (i32.eqz (local.get $n)))
      ;; program32 at 1028 (the input's callee), expect32 at 0 (32 zero bytes: any version), method "noop" at 60000.
      (drop (call $call (i32.const 1028) (i32.const 0) (i32.const 60000) (i32.const 4) (i32.const 0) (i32.const 0) (i64.const 0) (i64.const 0)))
      (local.set $n (i32.sub (local.get $n) (i32.const 1))) (br $l)))))"#
        ),
    )
}

/// [n u32][callee 32]
fn caller_input(callee: Address) -> impl Fn(u32) -> Vec<u8> {
    move |n| {
        let mut v = n.to_le_bytes().to_vec();
        v.extend(callee);
        v
    }
}

fn section_instantiate() {
    emit("## 3. Instantiation\n");
    emit("Per module, three ways: **raw** (`BlockEngine::invoke` of an empty `bk_noop` with a host that refuses every import: the engine's store, linking, instance and drop), **tx** (a top-level transaction calling `bk_noop` through `execute`: also the executor's fixed cost) and **call_v1** (each call of a loop `call_v1`s `bk_noop` on the module: the repeatable pattern, charged `call` 20,100 + `instantiate_fuel`). *Schedule* = `instantiate_fuel(pages, data)` (1,000 + 2,000 per page + 1 per data byte), which counts no imports, functions, exports, globals, tables or segments. Time ÷ 2.5 ns is the fuel it would need.\n");
    let corpus = inst_corpus();
    let caller = caller_program();
    let mark = |ns: f64, fuel: f64| {
        let cell = format!("{} ({})", us(ns), f2(ns / fuel));
        if ns / fuel > NS_PER_FUEL { format!("**{cell}**") } else { cell }
    };
    let (mut rows, mut rows_call, mut rows_touch) = (Vec::new(), Vec::new(), Vec::new());
    for (name, wasm) in &corpus {
        progress(format!("instantiate: {name}"));
        let info = blockula_vm::engine::CodeInfo::read(wasm).expect("reads");
        let sched = V1.instantiate_fuel(info.pages, info.data_bytes);
        let mut host = world();
        let (p, c) = (addr("inst"), addr("caller"));
        let hash = host.deploy(p, wasm);
        host.deploy(c, &caller);
        let mut row = vec![name.clone(), n0(wasm.len() as f64), n0(sched as f64)];
        let mut row_call = vec![name.clone(), n0((V1.host.base + V1.host.call + sched) as f64)];
        let mut row_touch = vec![name.clone()];
        for &kind in engines() {
            let engine = host.engine(kind, &V1).expect("loads");
            let raw = time(|| engine.invoke(&hash, "bk_noop", UNLIMITED, &mut NullHost));
            let r = run_tx(engine.as_ref(), &host, &[call(p, "noop", vec![])], &[], UNLIMITED);
            ok(&r, name);
            let tx = time(|| run_tx(engine.as_ref(), &host, &[call(p, "noop", vec![])], &[], UNLIMITED));
            let (fuel, per) = measure_loop(engine.as_ref(), &host, c, "run", caller_input(p), &[p], name);
            let call_ns = per.map(|x| x * fuel as f64);
            row.push(us(raw.med));
            row.push(mark(tx.med, r.fuel_used as f64));
            row_call.push(format!("{}: {}", n0(fuel as f64), mark(call_ns.med, fuel as f64)));
            if name.contains("touch") {
                let t = run_tx(engine.as_ref(), &host, &[call(p, "touch", vec![])], &[], UNLIMITED);
                ok(&t, "touch");
                let touch = time(|| run_tx(engine.as_ref(), &host, &[call(p, "touch", vec![])], &[], UNLIMITED));
                row_touch.push(format!("{}: {} (+{} µs over the empty tx)", n0(t.fuel_used as f64), mark(touch.med, t.fuel_used as f64), us(touch.med - tx.med)));
            }
        }
        rows.push(row);
        rows_call.push(row_call);
        if row_touch.len() > 1 {
            rows_touch.push(row_touch);
        }
    }
    let mut header = vec!["module".to_string(), "bytes".into(), "schedule fuel".into()];
    for k in engines() {
        header.push(format!("{} raw µs", short(*k)));
        header.push(format!("{} tx µs (ns/fuel)", short(*k)));
    }
    let h: Vec<&str> = header.iter().map(String::as_str).collect();
    table(&h, &rows);
    let mut header = vec!["callee".to_string(), "schedule fuel per call".into()];
    for k in engines() {
        header.push(format!("{} fuel: µs (ns/fuel)", short(*k)));
    }
    emit("**Repeated instantiation through `call_v1`** (per call: the measured fuel, then time and ns per fuel):\n");
    let h: Vec<&str> = header.iter().map(String::as_str).collect();
    table(&h, &rows_call);
    let mut header = vec!["module".to_string()];
    for k in engines() {
        header.push(format!("{} tx fuel: µs (ns/fuel)", short(*k)));
    }
    emit("**Touching the memory**: a transaction whose method stores to every 4 KiB of its 2 MiB, so the zeroed pages the allocator handed out lazily are faulted in (the cost an instantiation's per-page price must cover):\n");
    let h: Vec<&str> = header.iter().map(String::as_str).collect();
    table(&h, &rows_touch);
}

// ---- 4. host functions ----------------------------------------------------------------------

const HB: &str = "hb";

fn host_bench_program() -> Vec<u8> {
    let methods: &[(&str, &str, &str)] = &[
        ("input_len", "", "(drop (call $input_len))"),
        ("input_read", "", "(call $input_read (i32.const 0))"),
        ("return_data", "", "(call $return_data (i32.const 40000) (local.get $L))"),
        ("debug_log", "", "(call $debug_log (i32.const 0) (local.get $L))"),
        ("storage_read", "", "(drop (call $storage_read (i32.const 70000) (local.get $K) (i32.const 80000) (i32.const 16384)))"),
        ("storage_write", "", "(call $storage_write (i32.const 70000) (local.get $K) (i32.const 80000) (local.get $L) (i32.const 0))"),
        ("storage_write_new", "", "(i32.store (i32.const 70000) (local.get $i)) (call $storage_write (i32.const 70000) (local.get $K) (i32.const 80000) (local.get $L) (i32.const 0))"),
        ("storage_remove", "", "(drop (call $storage_remove (i32.const 70000) (local.get $K)))"),
        ("storage_remove_present", "", "(i32.store (i32.const 70000) (local.get $i)) (drop (call $storage_remove (i32.const 70000) (local.get $K)))"),
        ("self_address", "", "(call $self_address (i32.const 40000))"),
        ("caller", "", "(call $caller (i32.const 40000))"),
        ("value", "", "(drop (call $value))"),
        ("block_height", "", "(drop (call $block_height))"),
        ("self_balance", "", "(drop (call $self_balance))"),
        ("transfer", "", "(drop (call $transfer (i32.const 12) (i64.const 1) (i32.const 0)))"),
        ("transfer_open", "(i32.store (i32.const 100004) (i32.const 0x5eed))", "(i32.store (i32.const 100000) (local.get $i)) (drop (call $transfer (i32.const 100000) (i64.const 100000000) (i32.const 1)))"),
        ("call_noop", "", "(drop (call $call (i32.const 12) (i32.const 90000) (i32.const 60000) (i32.const 4) (i32.const 40000) (local.get $L) (i64.const 0) (i64.const 0)))"),
        ("try_call_trap", "", "(drop (call $try_call (i32.const 12) (i32.const 90000) (i32.const 60008) (i32.const 4) (i32.const 40000) (i32.const 0) (i64.const 0) (i64.const 0)))"),
        // L: the callee method's name pointer, K: its length ("noop" returns nothing, "ret4k" 4 KiB).
        ("last_return", "(drop (call $call (i32.const 12) (i32.const 90000) (local.get $L) (local.get $K) (i32.const 0) (i32.const 0) (i64.const 0) (i64.const 0)))", "(call $last_return (i32.const 40000))"),
        ("last_return_len", "", "(drop (call $last_return_len))"),
        ("last_abort_code", "", "(drop (call $last_abort_code))"),
        ("emit", "", "(call $emit (i32.const 60100) (local.get $K) (i32.const 80000) (local.get $L))"),
    ];
    let mut funcs = String::new();
    for (name, setup, body) in methods {
        write!(
            funcs,
            r#"(func (export "bk_{name}") (local $i i32) (local $L i32) (local $K i32)
  (call $input_read (i32.const 0))
  (local.set $i (i32.load (i32.const 0))) (local.set $L (i32.load (i32.const 4))) (local.set $K (i32.load (i32.const 8)))
  {setup}
  (block $d (loop $l (br_if $d (i32.eqz (local.get $i)))
    {body}
    (local.set $i (i32.sub (local.get $i) (i32.const 1))) (br $l))))
"#
        )
        .expect("a string");
    }
    funcs.push_str(
        r#"(func (export "bk_abort") (local $L i32)
  (call $input_read (i32.const 0))
  (local.set $L (i32.load (i32.const 4)))
  (call $abort (i32.const 7) (i32.const 80000) (local.get $L)))"#,
    );
    program(
        "host bench",
        &format!(
            r#"(module {IMPORTS} (memory (export "memory") 2 2)
  (data (i32.const 60000) "noop")
  (data (i32.const 60008) "trap")
  (data (i32.const 60016) "ret4k")
  (data (i32.const 60100) "abcdefghijklmnopqrstuvwxyzabcdef")
  (data (i32.const 80000) "message: something went wrong in the program")
  {funcs})"#
        ),
    )
}

fn leaf_program(pages: u32) -> Vec<u8> {
    program(
        "leaf",
        &format!(
            r#"(module (import "blockula" "return_data_v1" (func $return_data (param i32 i32)))
  (memory (export "memory") {pages} {pages})
  (func (export "bk_noop"))
  (func (export "bk_trap") unreachable)
  (func (export "bk_ret4k") (call $return_data (i32.const 0) (i32.const 4096)))
  (func (export "bk_ret0") (call $return_data (i32.const 0) (i32.const 0))))"#
        ),
    )
}

#[derive(Clone)]
struct HostCase {
    func: &'static str,
    case: String,
    method: &'static str,
    /// L, K, padding to make the input this long, the callee.
    l: u32,
    k: u32,
    input_len: usize,
    callee: &'static str,
    /// Most iterations the per-transaction bounds allow.
    bound: Option<u32>,
    /// Storage entries in the base state: (key, value length).
    base: Vec<(Vec<u8>, usize)>,
}

fn hc(func: &'static str, case: impl Into<String>, method: &'static str, l: u32, k: u32) -> HostCase {
    HostCase { func, case: case.into(), method, l, k, input_len: 44, callee: "leaf", bound: None, base: Vec::new() }
}

impl HostCase {
    fn bound(mut self, n: u32) -> Self {
        self.bound = Some(n);
        self
    }
    fn input_len(mut self, n: usize) -> Self {
        self.input_len = n;
        self
    }
    fn callee(mut self, c: &'static str) -> Self {
        self.callee = c;
        self
    }
    fn base(mut self, entries: Vec<(Vec<u8>, usize)>) -> Self {
        self.base = entries;
        self
    }
    fn input(&self, n: u32) -> Vec<u8> {
        let mut v = n.to_le_bytes().to_vec();
        v.extend(self.l.to_le_bytes());
        v.extend(self.k.to_le_bytes());
        v.extend(addr(self.callee));
        v.resize(self.input_len.max(44), 0);
        v
    }
}

fn host_cases() -> Vec<HostCase> {
    let key = |k: usize| vec![0u8; k];
    let numbered = |i: u32| {
        let mut k = vec![0u8; 32];
        k[..4].copy_from_slice(&i.to_le_bytes());
        k
    };
    vec![
        hc("input_len_v1", "", "input_len", 0, 0),
        hc("input_read_v1", "44 B input", "input_read", 0, 0),
        hc("input_read_v1", "32 KiB input", "input_read", 0, 0).input_len(32 * 1024),
        hc("return_data_v1", "0 B", "return_data", 0, 0),
        hc("return_data_v1", "4 KiB", "return_data", 4096, 0),
        hc("debug_log_v1", "0 B", "debug_log", 0, 0),
        hc("debug_log_v1", "64 KiB", "debug_log", 65536, 0),
        hc("abort_v1", "256 B message (one per tx: tx time − an empty tx's)", "abort", 256, 0),
        hc("storage_read_v1", "absent, 32 B key", "storage_read", 0, 32),
        hc("storage_read_v1", "32 B key, 8 B value", "storage_read", 0, 32).base(vec![(key(32), 8)]),
        hc("storage_read_v1", "128 B key, 8 B value", "storage_read", 0, 128).base(vec![(key(128), 8)]),
        hc("storage_read_v1", "32 B key, 16 KiB value", "storage_read", 0, 32).base(vec![(key(32), 16384)]),
        hc("storage_write_v1", "overwrite, 32 B key, 0 B value", "storage_write", 0, 32).base(vec![(key(32), 64)]),
        hc("storage_write_v1", "overwrite, 128 B key, 8 B value", "storage_write", 8, 128).base(vec![(key(128), 64)]).bound(4_000),
        hc("storage_write_v1", "overwrite, 32 B key, 16 KiB value (64 a tx)", "storage_write", 16384, 32).base(vec![(key(32), 16384)]).bound(64),
        hc("storage_write_v1", "new keys, 32 B key, 8 B value (256 a tx)", "storage_write_new", 8, 32).bound(256),
        hc("storage_remove_v1", "absent", "storage_remove", 0, 32),
        hc("storage_remove_v1", "present, 16 KiB value (256 a tx)", "storage_remove_present", 0, 32).base((1..=256).map(|i| (numbered(i), 16384)).collect()).bound(256),
        hc("self_address_v1", "", "self_address", 0, 0),
        hc("caller_v1", "", "caller", 0, 0),
        hc("value_v1", "", "value", 0, 0),
        hc("block_height_v1", "", "block_height", 0, 0),
        hc("self_balance_v1", "", "self_balance", 0, 0),
        hc("transfer_v1", "to an existing account", "transfer", 0, 0).callee("rcpt"),
        hc("transfer_v1", "opening an account (OPEN_DEDUCT)", "transfer_open", 0, 0),
        hc("call_v1", "callee 1 page, empty method", "call_noop", 0, 0),
        hc("call_v1", "callee 1 page, 32 KiB input", "call_noop", 32 * 1024, 0),
        hc("call_v1", "callee 32 pages, empty method", "call_noop", 0, 0).callee("leaf32"),
        hc("try_call_v1", "callee traps (layer dropped)", "try_call_trap", 0, 0),
        hc("last_return_v1", "0 B", "last_return", 60000, 4),
        hc("last_return_v1", "4 KiB", "last_return", 60016, 5),
        hc("last_return_len_v1", "", "last_return_len", 0, 0),
        hc("last_abort_code_v1", "", "last_abort_code", 0, 0),
        hc("emit_v1", "8 B name, 0 B data (64 a tx)", "emit", 0, 8).bound(64),
        hc("emit_v1", "32 B name, 16,352 B data (1 a tx)", "emit", 16352, 32).bound(1),
    ]
}

fn host_world(case: &HostCase, hb: &[u8], leaf: &[u8], leaf32: &[u8]) -> MemHost {
    let mut host = world();
    host.deploy(addr(HB), hb);
    host.deploy(addr("leaf"), leaf);
    host.deploy(addr("leaf32"), leaf32);
    host.fund(addr(HB), 10_000_000_000_000_000);
    host.fund(addr("rcpt"), 1);
    for (key, len) in &case.base {
        let entry = Entry { value: vec![1; *len], payer: signer(), deposit: 0, capacity: (key.len() + len) as u32 };
        host.storage.insert((addr(HB), key.clone()), entry);
    }
    host
}

fn section_host() {
    emit("## 4. Host functions (ABI v1)\n");
    emit("Each case loops one host function N times inside a program (`bk_<case>`) through the real executor and `MemHost`; per call = (time of N − time of 0) / N, so the call's Wasm side (its arguments, the `call` itself at 10 fuel, the loop's ~9) is included, as a program would pay it. Cases bounded per transaction (events, keys written, bytes written) run that many calls a transaction. Fuel per call is measured (the schedule's charge plus that Wasm side). `MemHost` is a `BTreeMap`, not the node's state: storage costs here are the overlay's and the host's, not redb's or the tree's (those are commit-time).\n");
    let (hb, leaf, leaf32) = (host_bench_program(), leaf_program(1), leaf_program(32));
    let mut rows = Vec::new();
    // case name -> (case label, byte size, per-engine (fuel/call, ns/call)).
    type Slope = (String, u32, Vec<(f64, f64)>);
    let mut slopes: BTreeMap<&str, Vec<Slope>> = BTreeMap::new();
    for case in host_cases() {
        progress(format!("host: {} {}", case.func, case.case));
        let host = host_world(&case, &hb, &leaf, &leaf32);
        let declared = [addr("leaf"), addr("leaf32")];
        let mut row = vec![format!("`{}` {}", case.func, case.case)];
        let mut per_engine = Vec::new();
        let mut fuel_cell = String::new();
        for &kind in engines() {
            let engine = host.engine(kind, &V1).expect("loads");
            let (fuel, ns_call) = if case.method == "abort" {
                let r = run_tx(engine.as_ref(), &host, &[call(addr(HB), "abort", case.input(0))], &declared, UNLIMITED);
                assert!(!r.ok);
                let empty = run_tx(engine.as_ref(), &host, &[call(addr(HB), "input_len", case.input(0))], &declared, UNLIMITED);
                let t = time(|| run_tx(engine.as_ref(), &host, &[call(addr(HB), "abort", case.input(0))], &declared, UNLIMITED));
                let t0 = time(|| run_tx(engine.as_ref(), &host, &[call(addr(HB), "input_len", case.input(0))], &declared, UNLIMITED));
                // Out of the abort's fuel: the receipt reports what it used (the abort traps, not OutOfFuel).
                (r.fuel_used.saturating_sub(empty.fuel_used), t.map(|x| x - t0.med))
            } else if let Some(bound) = case.bound {
                let rx = |n: u32| run_tx(engine.as_ref(), &host, &[call(addr(HB), case.method, case.input(n))], &declared, UNLIMITED);
                let (r0, rn) = (rx(0), rx(bound));
                ok(&r0, &case.case);
                ok(&rn, &case.case);
                let fuel = (rn.fuel_used - r0.fuel_used) / u64::from(bound);
                let t0 = time(|| rx(0)).med;
                let tn = time(|| rx(bound));
                (fuel, tn.map(|t| (t - t0) / f64::from(bound)))
            } else {
                let (fuel, per) = measure_loop(engine.as_ref(), &host, addr(HB), case.method, |n| case.input(n), &declared, &case.case);
                (fuel, per.map(|x| x * fuel as f64))
            };
            if fuel_cell.is_empty() {
                fuel_cell = n0(fuel as f64);
            } else if fuel_cell != n0(fuel as f64) {
                fuel_cell = format!("{fuel_cell} / {}", n0(fuel as f64));
            }
            let nsf = ns_call.med / fuel.max(1) as f64;
            let cell = format!("{} ({})", f2(ns_call.med), f2(nsf));
            row.push(if nsf > NS_PER_FUEL { format!("**{cell}**") } else { cell });
            per_engine.push((fuel as f64, ns_call.med));
        }
        row.insert(1, fuel_cell);
        rows.push(row);
        slopes.entry(case.func).or_default().push((case.case.clone(), case.l.max(case.input_len as u32), per_engine));
    }
    let mut header = vec!["function, case".to_string(), "fuel per call".into()];
    for k in engines() {
        header.push(format!("{} ns/call (ns/fuel)", short(*k)));
    }
    let h: Vec<&str> = header.iter().map(String::as_str).collect();
    table(&h, &rows);
    // Per-byte slopes from the pairs that differ only in size.
    emit("**Per byte**, from the two sizes of each function (Δtime / Δbytes, against Δfuel / Δbytes):\n");
    let pairs: &[(&str, &str, &str, f64)] = &[
        ("input_read_v1", "44 B input", "32 KiB input", (32 * 1024 - 44) as f64),
        ("return_data_v1", "0 B", "4 KiB", 4096.0),
        ("debug_log_v1", "0 B", "64 KiB", 65536.0),
        ("storage_read_v1", "32 B key, 8 B value", "32 B key, 16 KiB value", (16384 - 8) as f64),
        ("storage_read_v1", "32 B key, 8 B value", "128 B key, 8 B value", 96.0),
        ("storage_write_v1", "overwrite, 32 B key, 0 B value", "overwrite, 32 B key, 16 KiB value (64 a tx)", 16384.0),
        ("call_v1", "callee 1 page, empty method", "callee 1 page, 32 KiB input", 32768.0),
        ("last_return_v1", "0 B", "4 KiB", 4096.0),
        ("emit_v1", "8 B name, 0 B data (64 a tx)", "32 B name, 16,352 B data (1 a tx)", (16352 + 24) as f64),
    ];
    let mut rows = Vec::new();
    for (func, small, large, bytes) in pairs {
        let cases = &slopes[func];
        let a = cases.iter().find(|c| c.0 == *small).expect("small case");
        let b = cases.iter().find(|c| c.0 == *large).expect("large case");
        let fuel_per_byte = (b.2[0].0 - a.2[0].0) / bytes;
        let mut row = vec![format!("`{func}`: {small} → {large}"), f2(fuel_per_byte)];
        for (ea, eb) in a.2.iter().zip(&b.2) {
            let ns_b = (eb.1 - ea.1) / bytes;
            let nsf = ns_b / fuel_per_byte.max(1e-9);
            let cell = format!("{} ({})", f2(ns_b), f2(nsf));
            row.push(if nsf > NS_PER_FUEL { format!("**{cell}**") } else { cell });
        }
        rows.push(row);
    }
    let mut header = vec!["function: sizes".to_string(), "fuel per byte".into()];
    for k in engines() {
        header.push(format!("{} ns/B (ns/fuel)", short(*k)));
    }
    let h: Vec<&str> = header.iter().map(String::as_str).collect();
    table(&h, &rows);
}

// ---- 5. an adversarial block ----------------------------------------------------------------

/// A row projecting a 60M-fuel block from a per-fuel cost: ms = 60M × ns/fuel.
fn block_row(kind: EngineKind, label: &str, ns_per_fuel: Stats) -> Vec<String> {
    let ms = ns_per_fuel.map(|x| x * BLOCK_FUEL as f64 / 1e6);
    let cell = format!("{:.0}×", ms.med / BLOCK_MS);
    vec![
        short(kind).to_string(),
        label.to_string(),
        format!("{} [{}–{}]", f2(ns_per_fuel.med), f2(ns_per_fuel.min), f2(ns_per_fuel.max)),
        format!("{:.0} [{:.0}–{:.0}]", ms.med, ms.min, ms.max),
        if ms.med > BLOCK_MS { format!("**{cell}**") } else { cell },
    ]
}

fn section_block(worst: &[OpResult]) {
    emit("## 5. An adversarial block\n");
    emit(format!("A `max_block_fuel` block is {} fuel ({} transactions of `program_max_tx_fuel`, {} each). The cost of a block made entirely of one pattern is **60M × the pattern's ns/fuel**: that is how the design sets the budget (60M × 2.5 ns = 150 ms). Each ns/fuel below is measured as in sections 1–4 (a bounded loop, not a literal 60M-fuel run, which would just be the same number × 60M); the projection is then checked against one real end-to-end 60M block per engine at the end. 150 ms is the budget; a block over it is **bold**.\n", n0(BLOCK_FUEL as f64), BLOCK_FUEL / TX_FUEL, n0(TX_FUEL as f64)));
    let mut rows = Vec::new();
    let all = ops();
    // Compute: each engine's two worst operator patterns (from section 1).
    for (i, &kind) in engines().iter().enumerate() {
        let mut ranked: Vec<&OpResult> = worst.iter().filter(|r| r.per[i].1 >= 1).collect();
        ranked.sort_by(|a, b| b.per[i].3.med.partial_cmp(&a.per[i].3.med).expect("no NaN"));
        for r in ranked.iter().take(2) {
            rows.push(block_row(kind, &format!("compute: {}", r.name), r.per[i].3));
        }
    }
    // Instantiation: call_v1 loops to the worst callees.
    let inst = inst_corpus();
    let caller = caller_program();
    for (label, wasm) in [
        ("instantiation: call_v1 loop to the many-exports callee", inst.iter().find(|(n, _)| n.ends_with(" exports") && !n.starts_with("all")).expect("exports").1.clone()),
        ("instantiation: call_v1 loop to the 5,040-function callee", inst.iter().find(|(n, _)| n.ends_with(" functions")).expect("functions").1.clone()),
        ("instantiation: call_v1 loop to a 32-page callee", leaf_program(32)),
    ] {
        let mut host = world();
        let (p, c) = (addr("inst"), addr("caller"));
        host.deploy(p, &wasm);
        host.deploy(c, &caller);
        for &kind in engines() {
            progress(format!("block: {} {label}", short(kind)));
            let engine = host.engine(kind, &V1).expect("loads");
            let (_, per) = measure_loop(engine.as_ref(), &host, c, "run", caller_input(p), &[p], label);
            rows.push(block_row(kind, label, per));
        }
    }
    // Storage-write storm: 16 KiB values, 64 a transaction (the 1 MiB write bound).
    {
        let (hb, leaf, leaf32) = (host_bench_program(), leaf_program(1), leaf_program(32));
        let case = hc("storage_write_v1", "", "storage_write", 16384, 32).base(vec![(vec![0u8; 32], 16384)]);
        let host = host_world(&case, &hb, &leaf, &leaf32);
        let calls = [call(addr(HB), "storage_write", case.input(64))];
        for &kind in engines() {
            progress(format!("block: {} storage storm", short(kind)));
            let engine = host.engine(kind, &V1).expect("loads");
            let r = run_tx(engine.as_ref(), &host, &calls, &[], UNLIMITED);
            ok(&r, "storm");
            let fuel = r.fuel_used;
            let t = time(|| run_tx(engine.as_ref(), &host, &calls, &[], UNLIMITED));
            rows.push(block_row(kind, "storage_write storm: 64 × 16 KiB overwrites a tx", t.map(|x| x / fuel as f64)));
        }
    }
    // Load: distinct maximum-size codes of the worst shape, and distinct minimal codes.
    let corpus = translation_corpus();
    let shapes: Vec<&(String, Vec<u8>)> = corpus.iter().filter(|(_, w)| w.len() > 100_000).collect();
    let tiny = corpus.iter().find(|(n, _)| n == "minimal program").expect("minimal").1.clone();
    for &kind in engines() {
        let mut worst_shape = (0.0f64, &shapes[0].1, shapes[0].0.clone());
        for (name, wasm) in &shapes {
            let ns = time_load(kind, wasm).0.med / wasm.len() as f64;
            if ns > worst_shape.0 {
                worst_shape = (ns, wasm, name.clone());
            }
        }
        for (label, wasm) in [(format!("load: distinct \"{}\"", worst_shape.2), worst_shape.1.clone()), ("load: distinct minimal programs".to_string(), tiny.clone())] {
            let count = (BLOCK_FUEL / V1.load_fuel(wasm.len())).max(1);
            progress(format!("block: {} {label}", short(kind)));
            let load_fuel = V1.load_fuel(wasm.len());
            let per = time_load(kind, &wasm).0.map(|t| t / load_fuel as f64);
            rows.push(block_row(kind, &format!("{label} × {count} ({} B each)", n0(wasm.len() as f64)), per));
        }
    }
    table(&["engine", "block of this pattern", "ns/fuel [min–max]", "projected ms [min–max]", "vs 150 ms"], &rows);

    // End-to-end check: one real 60M-fuel block (three 20M txs) of each engine's worst compute
    // pattern, timed, against the projection above.
    emit("**End-to-end check**: one real `max_block_fuel` block (three 20M-fuel transactions, each run until out of fuel) of each engine's worst compute pattern, measured whole, against the projection:\n");
    let mut rows = Vec::new();
    for (i, &kind) in engines().iter().enumerate() {
        let r = worst.iter().filter(|r| r.per[i].1 >= 1).max_by(|a, b| a.per[i].3.med.partial_cmp(&b.per[i].3.med).expect("no NaN")).expect("a case");
        let op = all.iter().find(|o| o.name == r.name).expect("the op");
        progress(format!("block e2e: {} {}", short(kind), op.name));
        let (host, p) = op_world(&op.module());
        let calls = [call(p, "run", op_input(u32::MAX))];
        let engine = host.engine(kind, &V1).expect("loads");
        let burn = || {
            let mut used = 0u64;
            for _ in 0..BLOCK_FUEL / TX_FUEL {
                used += run_tx(engine.as_ref(), &host, &calls, &[], TX_FUEL).fuel_used;
            }
            used
        };
        assert_eq!(burn(), BLOCK_FUEL, "the block burns exactly max_block_fuel");
        let t = time(burn).map(|x| x / 1e6);
        let projected = r.per[i].3.med * BLOCK_FUEL as f64 / 1e6;
        rows.push(vec![
            short(kind).to_string(),
            r.name.clone(),
            format!("{:.0} [{:.0}–{:.0}]", t.med, t.min, t.max),
            format!("{projected:.0}"),
            format!("{:.0}×", t.med / BLOCK_MS),
        ]);
    }
    table(&["engine", "worst compute pattern", "measured ms [min–max]", "projected ms", "vs 150 ms"], &rows);
}

// ---- 6. cold versus warm, an engine's cost per block ----------------------------------------

fn section_warm() {
    emit("## 6. Cold versus warm, and an engine's cost per block\n");
    emit("**Cold** = the first transaction on a fresh block engine that has just loaded the code (engine and load outside the clock); **warm** = the same transaction again on an engine that has run it 10 times. Fuel must be identical (asserted for every repetition).\n");
    let counter = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/programs/counter.wasm")).expect("guest");
    let token = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/programs/token.wasm")).expect("guest");
    let escrow = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/programs/escrow.wasm")).expect("guest");
    let (hb, leaf, leaf32) = (host_bench_program(), leaf_program(1), leaf_program(32));
    let mut scenarios: Vec<(String, MemHost, Vec<Call>, Vec<Address>)> = Vec::new();
    {
        let mut host = world();
        host.deploy(addr("counter"), &counter);
        scenarios.push(("counter increment".into(), host, vec![call(addr("counter"), "increment", vec![])], vec![]));
    }
    {
        let mut host = world();
        host.deploy(addr("token"), &token);
        let mut init = signer().to_vec();
        init.extend(1_000_000u64.to_le_bytes());
        let e = host.engine(EngineKind::Soroban, &V1).expect("loads");
        let r = run_tx(e.as_ref(), &host, &[call(addr("token"), "init", init)], &[], 10_000_000);
        ok(&r, "token init");
        host.apply(&r.diff);
        let mut t = addr("bob").to_vec();
        t.extend(5u64.to_le_bytes());
        scenarios.push(("token transfer".into(), host, vec![call(addr("token"), "transfer", t)], vec![]));
    }
    {
        let mut host = world();
        host.deploy(addr("escrow"), &escrow);
        let mut input = 1u64.to_le_bytes().to_vec();
        input.extend(addr("seller"));
        let c = Call { program: addr("escrow"), method: "open".into(), input, value: 1_000_000_000 };
        scenarios.push(("escrow open (payable)".into(), host, vec![c], vec![]));
    }
    for name in ["call, 1,024 locals", "i64.div_u by 1, chain", "pointer chase, 2 MiB, random"] {
        let op = ops().into_iter().find(|o| o.name == name).expect("op");
        let (host, p) = op_world(&op.module());
        scenarios.push((format!("loop: {name} × 1,000"), host, vec![call(p, "run", op_input(1_000))], vec![]));
    }
    {
        let case = hc("call_v1", "", "call_noop", 0, 0).callee("leaf32");
        let host = host_world(&case, &hb, &leaf, &leaf32);
        scenarios.push(("call_v1 × 100 to a 32-page callee".into(), host, vec![call(addr(HB), "call_noop", case.input(100))], vec![addr("leaf32")]));
    }
    let mut rows = Vec::new();
    for (name, host, calls, declared) in &scenarios {
        progress(format!("warm: {name}"));
        let mut row = vec![name.clone()];
        let mut fuels = Vec::new();
        for &kind in engines() {
            let mut cold_fuel = Vec::new();
            let cold = time_with(
                || host.engine(kind, &V1).expect("loads"),
                |e| {
                    let r = run_tx(e.as_ref(), host, calls, declared, 100_000_000);
                    cold_fuel.push(r.fuel_used);
                    (e, r)
                },
            );
            let engine = host.engine(kind, &V1).expect("loads");
            let first = run_tx(engine.as_ref(), host, calls, declared, 100_000_000);
            ok(&first, name);
            for _ in 0..10 {
                assert_eq!(run_tx(engine.as_ref(), host, calls, declared, 100_000_000), first, "{name}: warm differs");
            }
            let warm = time(|| run_tx(engine.as_ref(), host, calls, declared, 100_000_000));
            assert!(cold_fuel.iter().all(|f| *f == first.fuel_used), "{name}: cold and warm fuel differ on {}", short(kind));
            fuels.push(n0(first.fuel_used as f64));
            row.push(format!("{} / {} ({:+.0}%)", us(cold.med), us(warm.med), (cold.med / warm.med - 1.0) * 100.0));
        }
        row.insert(1, fuels.join(" / "));
        rows.push(row);
    }
    let mut header = vec!["transaction".to_string(), "fuel (identical cold and warm)".into()];
    for k in engines() {
        header.push(format!("{} cold / warm µs", short(*k)));
    }
    let h: Vec<&str> = header.iter().map(String::as_str).collect();
    table(&h, &rows);

    emit(format!("**An engine's cost per block**: creating a block engine (the engine, its linker with the {} host functions), loading N codes (each a distinct hash: translated anew) and dropping it.\n", abi_len()));
    let corpus = translation_corpus();
    let big = corpus.iter().find(|(n, _)| n.starts_with("one function, straight-line")).expect("big").1.clone();
    let mut rows = Vec::new();
    for &kind in engines() {
        let create = time(|| new_block_engine(kind, &V1));
        let drop_empty = time_with(|| new_block_engine(kind, &V1), drop);
        rows.push(vec![short(kind).to_string(), "create an empty engine".into(), us(create.med), format!("drop {}", us(drop_empty.med))]);
        for (label, wasm, n) in [("guest token.wasm", &token, 1usize), ("guest token.wasm", &token, 10), ("guest token.wasm", &token, 100), ("guest token.wasm", &token, 1_000), ("256 KB straight-line", &big, 1), ("256 KB straight-line", &big, 10)] {
            let hashes: Vec<CodeHash> = (0..n).map(|i| *blake3::hash(&(i as u64).to_le_bytes()).as_bytes()).collect();
            let load = time_with(
                || new_block_engine(kind, &V1),
                |mut e| {
                    for h in &hashes {
                        e.load(*h, wasm).expect("loads");
                    }
                    e
                },
            );
            let dropped = time_with(
                || {
                    let mut e = new_block_engine(kind, &V1);
                    for h in &hashes {
                        e.load(*h, wasm).expect("loads");
                    }
                    e
                },
                drop,
            );
            rows.push(vec![
                String::new(),
                format!("{n} × {label}"),
                format!("{} ({} per code)", us(load.med), us(load.med / n as f64)),
                format!("drop {} ({} per code)", us(dropped.med), us(dropped.med / n as f64)),
            ]);
        }
    }
    table(&["engine", "what", "create + load µs", "drop µs"], &rows);
}

// ---- 7. native stack ------------------------------------------------------------------------

/// The chain program: `bk_go` recurses `rec` Wasm frames (the input's last 4 bytes), then calls
/// the input's first program with the rest of the input (`call_v1`), until no program is left.
fn chain_program() -> Vec<u8> {
    program(
        "chain",
        &format!(
            r#"(module {IMPORTS} (memory (export "memory") 1 1)
  (data (i32.const 60000) "go")
  (func $rec (param $d i32) (param $len i32)
    (if (local.get $d)
      (then (call $rec (i32.sub (local.get $d) (i32.const 1)) (local.get $len)))
      (else (if (i32.ge_u (local.get $len) (i32.const 36))
        (then (drop (call $call (i32.const 1024) (i32.const 0) (i32.const 60000) (i32.const 2)
                (i32.const 1056) (i32.sub (local.get $len) (i32.const 32)) (i64.const 0) (i64.const 0))))))))
  (func (export "bk_go") (local $len i32)
    (local.set $len (call $input_len))
    (call $input_read (i32.const 1024))
    (call $rec (i32.load (i32.sub (i32.add (i32.const 1024) (local.get $len)) (i32.const 4))) (local.get $len))))"#
        ),
    )
}

/// `programs` deployed at p1..pn; the top-level call goes to p1 and each calls the next.
fn chain_tx(programs: usize, rec: u32) -> (MemHost, Vec<Call>, Vec<Address>) {
    let mut host = world();
    let wasm = chain_program();
    let names: Vec<Address> = (1..=programs).map(|i| addr(&format!("p{i}"))).collect();
    for a in &names {
        host.deploy(*a, &wasm);
    }
    let mut input: Vec<u8> = names[1..].iter().flat_map(|a| a.to_vec()).collect();
    input.extend(rec.to_le_bytes());
    (host, vec![call(names[0], "go", input)], names[1..].to_vec())
}

fn stack_scenarios() -> Vec<(&'static str, usize, u32, bool)> {
    vec![
        ("one program, no recursion", 1, 0, true),
        ("depth 8, no Wasm recursion", 8, 0, true),
        ("depth 8, 1,000 Wasm frames per program", 8, 1_000, true),
        ("depth 9 attempted (CallDepth), 1,000 Wasm frames per program", 9, 1_000, false),
    ]
}

/// The child's side: run the scenario on a thread of `bytes` stack; exit 0 if it ran as expected.
fn stack_probe(kind: usize, scenario: usize, bytes: usize) -> ! {
    let kind = engines()[kind];
    let (_, programs, rec, expect_ok) = stack_scenarios()[scenario];
    let handle = std::thread::Builder::new()
        .stack_size(bytes)
        .spawn(move || {
            let (host, calls, declared) = chain_tx(programs, rec);
            let engine = host.engine(kind, &V1).expect("loads");
            let r = run_tx(engine.as_ref(), &host, &calls, &declared, 100_000_000);
            r.ok == expect_ok && (expect_ok || matches!(&r.fail, Some(blockula_vm::exec::FailCode::Trapped { trap: Trap::CallDepth, .. })))
        })
        .expect("spawns");
    let ok = handle.join().unwrap_or(false);
    std::process::exit(if ok { 0 } else { 3 });
}

fn probe_ok(kind: usize, scenario: usize, bytes: usize) -> bool {
    std::process::Command::new(std::env::current_exe().expect("this binary"))
        .args(["--stack-probe", &kind.to_string(), &scenario.to_string(), &bytes.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("runs")
        .success()
}

fn section_stack() {
    emit("## 7. Native stack\n");
    emit("The smallest thread stack on which the scenario runs as expected, bisected to 4 KiB (the OS rounds a thread's stack up to its page: 16 KiB on this Mac) in child processes (a stack overflow aborts the process). The thread both loads the code (a fresh block engine) and executes the transaction, as the executor thread will. Programs p1→…→p8 each `call_v1` the next; with recursion, each first recurses 1,000 Wasm frames (wasmi keeps Wasm frames off the native stack: the numbers should not move). Release build.\n");
    let mut rows = Vec::new();
    for (si, (name, ..)) in stack_scenarios().iter().enumerate() {
        let mut row = vec![name.to_string()];
        for (ki, &kind) in engines().iter().enumerate() {
            progress(format!("stack: {} {name}", short(kind)));
            let (mut lo, mut hi) = (16 * 1024usize, 16 * 1024 * 1024usize);
            assert!(probe_ok(ki, si, hi), "{name} fails even on 16 MiB");
            if probe_ok(ki, si, lo) {
                hi = lo;
            }
            while hi - lo > 4096 {
                let mid = (lo + hi) / 2 / 4096 * 4096;
                if probe_ok(ki, si, mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            row.push(format!("{} KiB", hi / 1024));
        }
        rows.push(row);
    }
    let mut header = vec!["scenario".to_string()];
    for k in engines() {
        header.push(short(*k).to_string());
    }
    let h: Vec<&str> = header.iter().map(String::as_str).collect();
    table(&h, &rows);
}

// ---- main -----------------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn prefer_performance_cores() {
    unsafe extern "C" {
        fn pthread_set_qos_class_self_np(qos: u32, relative_priority: i32) -> i32;
    }
    // QOS_CLASS_USER_INTERACTIVE: the scheduler keeps the thread on performance cores.
    // SAFETY: a plain libc call on the current thread.
    unsafe {
        pthread_set_qos_class_self_np(0x21, 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn prefer_performance_cores() {}

fn overflow_checks() -> bool {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let on = std::panic::catch_unwind(|| black_box(black_box(255u8) + black_box(1u8))).is_err();
    std::panic::set_hook(hook);
    on
}

fn command(cmd: &str, args: &[&str]) -> String {
    std::process::Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| a != "--bench").collect();
    let quick = args.iter().any(|a| a == "--quick");
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    CFG.set(Cfg {
        reps: if quick { 3 } else { 9 },
        warmup: if quick { 1 } else { 2 },
        sample: Duration::from_millis(if quick { 2 } else { 5 }),
        run: Duration::from_millis(if quick { 10 } else { 30 }),
        only: value("--only").map(|s| s.split(',').map(String::from).collect()).unwrap_or_default(),
        out: value("--out"),
    })
    .ok()
    .expect("once");
    if let Some(i) = args.iter().position(|a| a == "--stack-probe") {
        let n = |j: usize| args[i + j].parse::<usize>().expect("a number");
        stack_probe(n(1), n(2), n(3));
    }
    let machine = if cfg!(target_os = "macos") { command("sysctl", &["-n", "machdep.cpu.brand_string"]) } else { command("uname", &["-m"]) };
    emit("# Calibration run\n");
    emit(format!(
        "- Machine: {machine} ({}, {}); development numbers, never prices\n- rustc: {}\n- Build: `{}` profile, overflow checks {}, debug assertions {}\n- Engines: {}\n- Repetitions: {} (+{} warmup), samples ≥ {} ms, loop runs ≈ {} ms\n",
        std::env::consts::ARCH,
        std::env::consts::OS,
        command("rustc", &["-V"]),
        if cfg!(debug_assertions) { "debug" } else { "release/bench" },
        if overflow_checks() { "on" } else { "off" },
        if cfg!(debug_assertions) { "on" } else { "off" },
        engines().iter().map(|k| k.name()).collect::<Vec<_>>().join(", "),
        cfg().reps,
        cfg().warmup,
        cfg().sample.as_millis(),
        cfg().run.as_millis(),
    ));
    let started = Instant::now();
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            prefer_performance_cores();
            let ops_results = if wanted("ops") || wanted("block") {
                let r = section_ops();
                Some(r)
            } else {
                None
            };
            if wanted("translate") {
                section_translate();
            }
            if wanted("instantiate") {
                section_instantiate();
            }
            if wanted("host") {
                section_host();
            }
            if wanted("block") {
                section_block(ops_results.as_deref().expect("ops ran"));
            }
            if wanted("warm") {
                section_warm();
            }
        })
        .expect("spawns")
        .join()
        .expect("the run");
    if wanted("stack") {
        section_stack();
    }
    emit(format!("_Total run time: {:.0} s._", started.elapsed().as_secs_f64()));
    if let Some(path) = &cfg().out {
        std::fs::write(path, OUT.lock().expect("not poisoned").as_str()).expect("writes the report");
    }
}
