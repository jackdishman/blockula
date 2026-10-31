//! The node's benches (docs/PROGRAMS-V0.md §5.2 item 3; WBS S4c): what a block of program
//! transactions costs through the node's own path, which W1's `cargo bench --bench calibrate`
//! can't see because it runs the VM over an in-memory host. Each pattern is one block executed on a
//! dev chain with programs on, timed through `State::run_block` and `State::commit_block` (the
//! state tree's updates), then the writer's `persist` (the block logs and the database write,
//! fsynced as in production) apart from them, against the fuel the block counted:
//! - **minimal calls**: a block of `MAX_BLOCK_TXS` (512) calls of one `nop` to the smallest valid
//!   code, so the per-transaction setup fuel doesn't meter (a thread, an engine and its linker, a
//!   translation) stays within what the transaction paid;
//! - **write storm, small keys**: calls that each write 256 distinct one-byte keys (the most one
//!   transaction may), each to a program of its own so every write is a new entry;
//! - **write storm, large values**: calls that each write 48 values of 16 KiB (a transaction may
//!   write at most 1 MiB).
//!
//! PRG 4.5's rule is no pattern slower than **2.5 ns per counted fuel** on either floor runner
//! (x86-64 and the aarch64 GitHub runner), so a `CALIBRATED_BLOCK_FUEL` block runs in 150 ms.
//! "Counted fuel" is what the schedule says the work was: for each transaction the load fuel of its
//! code plus the fuel the VM metered (`FuelUse::load + used`). What the block's budget counts is
//! the sum of the reservations (`limit + load`), which is printed too.
//!
//! What the rule is applied to differs by pattern ([`Rule`], §5.2 item 3):
//! - the **minimal calls** are judged on the VM's share of `run_block` + `commit_block`: the block's,
//!   less the same block of as many native transfers (the work every transaction pays whatever it
//!   does: `Tx::check_format`, the fee phase, the account writes, none of it priced in fuel).
//!   `persist` is printed apart, since on a laptop it is mostly `fsync`;
//! - the **write storms** are judged on the whole block, `persist` (the database write) included,
//!   which is printed apart as well.
//!
//! The native cost of a transaction is printed beside every pattern, and the context table times
//! `Tx::check_format` and `Tx::id` of a call on their own.
//!
//! Since A3 (one thread and one engine per block pass, `pass.rs`) the blocks run as passes; with
//! `BLOCKULA_BENCH_PER_TX=1` they run the way they did before it, a thread and an engine per
//! program transaction (`pass::per_transaction`), so one binary times both back to back. The
//! context tables also time the pieces of a minimal call that aren't a thread, an engine or a load
//! (the program rules of admission, the run's notes, the view, the executor's instantiation and
//! `nop` on a warm engine, the commit, the receipt), so what a per-call cap or `LOAD_BASE` would
//! price is visible, and a thread by stack size.
//!
//! `#[ignore]`d, with a verdict (`GO` or `NO-GO`, which fails the test) per pattern. Run it on the
//! machine whose answer matters, in release, one at a time:
//!
//! ```text
//! cd node && cargo test --release --locked programs::bench -- --ignored --nocapture --test-threads=1
//! ```
//!
//! A `NO-GO` takes the pattern's fallback before the rules freeze, in the order of spec §5.2: make
//! the native work cheaper (no consensus effect), reuse one thread and one engine per block pass
//! (A3: no outcome or fuel changes, so no version), cap the program transactions in a block, and
//! only then raise `LOAD_BASE`; the write storms raise `storage_write`, its per-byte price and
//! `storage_remove`. Results go beside W1's `CALIBRATION-*.md` (`node/vm/CALIBRATION-node.md`).

use super::exec::Run as ProgramRun;
use super::params::Param;
use super::probe::*;
use super::view::ProgramView;
use crate::chain::{ApplyMode, Journal};
use crate::consensus::MAX_BLOCK_TXS;
use crate::types::{Action, Tx};
use blockula_vm::host::MethodKind;
use blockula_vm::interface::{Interface, Method};
use std::time::{Duration, Instant};

/// The rule: nanoseconds per counted fuel, at most.
const MAX_NS_PER_FUEL: f64 = 2.5;

/// `(module (memory (export "memory") 1 1) (func (export "bk_nop") nop))`, assembled with `wat`
/// 1.262.0 (node/vm's dev-dependency): the smallest valid code, one instruction in its one method.
const NOP_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03, 0x02, //
    0x01, 0x00, 0x05, 0x04, 0x01, 0x01, 0x01, 0x01, 0x07, 0x13, 0x02, 0x06, 0x6d, 0x65, 0x6d, 0x6f, //
    0x72, 0x79, 0x02, 0x00, 0x06, 0x62, 0x6b, 0x5f, 0x6e, 0x6f, 0x70, 0x00, 0x00, 0x0a, 0x05, 0x01, //
    0x03, 0x00, 0x01, 0x0b,
];

/// Writes 256 distinct one-byte keys with one-byte values:
///
/// ```wat
/// (module
///   (import "blockula" "storage_write_v1" (func (param i32 i32 i32 i32 i32)))
///   (memory (export "memory") 1 1)
///   (func (export "bk_keys") (local i32)
///     (loop
///       (i32.store8 (i32.const 0) (local.get 0))
///       (i32.store8 (i32.const 1) (i32.const 1))
///       (call 0 (i32.const 0) (i32.const 1) (i32.const 1) (i32.const 1) (i32.const 0))
///       (local.set 0 (i32.add (local.get 0) (i32.const 1)))
///       (br_if 0 (i32.lt_u (local.get 0) (i32.const 256))))))
/// ```
const KEYS_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x0c, 0x02, 0x60, 0x05, 0x7f, 0x7f, 0x7f, //
    0x7f, 0x7f, 0x00, 0x60, 0x00, 0x00, 0x02, 0x1d, 0x01, 0x08, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x75, //
    0x6c, 0x61, 0x10, 0x73, 0x74, 0x6f, 0x72, 0x61, 0x67, 0x65, 0x5f, 0x77, 0x72, 0x69, 0x74, 0x65, //
    0x5f, 0x76, 0x31, 0x00, 0x00, 0x03, 0x02, 0x01, 0x01, 0x05, 0x04, 0x01, 0x01, 0x01, 0x01, 0x07, //
    0x14, 0x02, 0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00, 0x07, 0x62, 0x6b, 0x5f, 0x6b, //
    0x65, 0x79, 0x73, 0x00, 0x01, 0x0a, 0x32, 0x01, 0x30, 0x01, 0x01, 0x7f, 0x03, 0x40, 0x41, 0x00, //
    0x20, 0x00, 0x3a, 0x00, 0x00, 0x41, 0x01, 0x41, 0x01, 0x3a, 0x00, 0x00, 0x41, 0x00, 0x41, 0x01, //
    0x41, 0x01, 0x41, 0x01, 0x41, 0x00, 0x10, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6a, 0x21, 0x00, 0x20, //
    0x00, 0x41, 0x80, 0x02, 0x49, 0x0d, 0x00, 0x0b, 0x0b,
];

/// Writes 48 values of 16 KiB (the first 16,384 bytes of its memory) under 48 distinct one-byte
/// keys, 768 KiB of the 1 MiB a transaction may write:
///
/// ```wat
/// (module
///   (import "blockula" "storage_write_v1" (func (param i32 i32 i32 i32 i32)))
///   (memory (export "memory") 1 1)
///   (func (export "bk_values") (local i32)
///     (loop
///       (i32.store8 (i32.const 65000) (local.get 0))
///       (call 0 (i32.const 65000) (i32.const 1) (i32.const 0) (i32.const 16384) (i32.const 0))
///       (local.set 0 (i32.add (local.get 0) (i32.const 1)))
///       (br_if 0 (i32.lt_u (local.get 0) (i32.const 48))))))
/// ```
const VALUES_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x0c, 0x02, 0x60, 0x05, 0x7f, 0x7f, 0x7f, //
    0x7f, 0x7f, 0x00, 0x60, 0x00, 0x00, 0x02, 0x1d, 0x01, 0x08, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x75, //
    0x6c, 0x61, 0x10, 0x73, 0x74, 0x6f, 0x72, 0x61, 0x67, 0x65, 0x5f, 0x77, 0x72, 0x69, 0x74, 0x65, //
    0x5f, 0x76, 0x31, 0x00, 0x00, 0x03, 0x02, 0x01, 0x01, 0x05, 0x04, 0x01, 0x01, 0x01, 0x01, 0x07, //
    0x16, 0x02, 0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00, 0x09, 0x62, 0x6b, 0x5f, 0x76, //
    0x61, 0x6c, 0x75, 0x65, 0x73, 0x00, 0x01, 0x0a, 0x30, 0x01, 0x2e, 0x01, 0x01, 0x7f, 0x03, 0x40, //
    0x41, 0xe8, 0xfb, 0x03, 0x20, 0x00, 0x3a, 0x00, 0x00, 0x41, 0xe8, 0xfb, 0x03, 0x41, 0x01, 0x41, //
    0x00, 0x41, 0x80, 0x80, 0x01, 0x41, 0x00, 0x10, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6a, 0x21, 0x00, //
    0x20, 0x00, 0x41, 0x30, 0x49, 0x0d, 0x00, 0x0b, 0x0b,
];

/// An interface of `method` alone: a `Call`, not payable, no arguments.
fn interface_of(method: &str) -> Vec<u8> {
    let methods = [method].map(|name| Method { name: name.into(), kind: MethodKind::Call, payable: false, args: vec![], returns: None });
    blockula_vm::interface::encode(&Interface { methods: methods.into() }).unwrap()
}

/// What a pattern's block time is judged on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rule {
    /// The VM's share of `run_block` + `commit_block`: the block's less a block of as many native
    /// transfers'; `persist` apart.
    VmShare,
    /// The whole block: `run_block`, `commit_block` and `persist`.
    Whole,
}

/// One block of calls, and how to time it.
struct Pattern {
    name: &'static str,
    wasm: &'static [u8],
    method: &'static str,
    /// Calls in the block, each in its own transaction (and, if `own_programs`, to its own program).
    calls: usize,
    own_programs: bool,
    /// The calls' `fuel_limit`: what each needs, with a margin.
    fuel_limit: u64,
    /// Blocks to time on fresh chains (the median is reported).
    reps: usize,
    rule: Rule,
}

const PATTERNS: [Pattern; 3] = [
    Pattern { name: "minimal calls", wasm: NOP_WASM, method: "nop", calls: MAX_BLOCK_TXS, own_programs: false, fuel_limit: 6_000, reps: 5, rule: Rule::VmShare },
    Pattern { name: "write storm, 256 one-byte keys a call", wasm: KEYS_WASM, method: "keys", calls: 24, own_programs: true, fuel_limit: 1_600_000, reps: 3, rule: Rule::Whole },
    Pattern { name: "write storm, 48 values of 16 KiB a call", wasm: VALUES_WASM, method: "values", calls: 4, own_programs: true, fuel_limit: 9_500_000, reps: 3, rule: Rule::Whole },
];

/// The process's minor page faults so far (Linux: `/proc/self/stat`, field 10), `None` elsewhere. A
/// fresh thread's stack and its first allocations are first-touch faults; the bench prints how many
/// a block's `run_block` takes, apart from its time, to say how much of a call is that.
fn minor_faults() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // After the command's closing parenthesis: state, ppid, pgrp, session, tty, tpgid, flags, minflt.
    stat.rsplit_once(')')?.1.split_whitespace().nth(7)?.parse().ok()
}

fn faults_per(faults: Option<u64>, n: usize) -> String {
    faults.map_or_else(|| "n/a".to_string(), |f| format!("{f} ({:.1} a transaction)", f as f64 / n as f64))
}

/// What one block cost.
#[derive(Clone, Debug)]
struct Run {
    txs: usize,
    /// Σ `load + used` over the block's receipts: the fuel the schedule says the work was.
    counted: u64,
    /// Σ `reserved`: what the block's fuel budget counts.
    reserved: u64,
    /// The chain's `program_max_block_fuel`.
    budget: u64,
    run_block: Duration,
    commit_block: Duration,
    persist: Duration,
    /// `run_block` + `commit_block` of `txs` native transfers on the same state.
    native: Duration,
    /// Minor page faults the process took in `run_block` of the block and of the native transfers
    /// (Linux).
    faults: Option<u64>,
    native_faults: Option<u64>,
}

impl Run {
    /// What the state transition cost, the writer's `persist` apart.
    fn work(&self) -> Duration {
        self.run_block + self.commit_block
    }

    /// The block's work less the same number of native transfers': what the programs added.
    fn vm_share(&self) -> Duration {
        self.work().saturating_sub(self.native)
    }

    fn total(&self) -> Duration {
        self.work() + self.persist
    }

    /// What `rule` judges.
    fn judged(&self, rule: Rule) -> Duration {
        match rule {
            Rule::VmShare => self.vm_share(),
            Rule::Whole => self.total(),
        }
    }

    fn ns_per_fuel(&self, d: Duration) -> f64 {
        d.as_nanos() as f64 / self.counted as f64
    }

    fn per_tx(&self, d: Duration) -> f64 {
        d.as_secs_f64() * 1e6 / self.txs as f64
    }
}

/// A fresh dev chain with the pattern's code uploaded and its programs created, storage priced at
/// its floor (so deposits don't bound the block: they change no work), and the block's calls.
fn build(p: &Pattern) -> (Dev, Vec<Tx>) {
    let mut d = Dev::new("bench");
    d.c.state.programs.meta.as_mut().unwrap().params.insert(Param::StoragePrice.key().into(), 1_000);
    let alice = keys(ALICE);
    let iface = interface_of(p.method);
    let upload = d.tx(&alice, upload(p.wasm, &iface));
    d.block(vec![upload]);
    let n = if p.own_programs { p.calls } else { 1 };
    let creates = d.txs(&alice, vec![create(p.wasm, &iface); n]);
    let programs: Vec<String> = creates.iter().map(|t| super::digest::program_address(&d.prefix(), &crate::crypto::address_bytes(&t.from).unwrap(), t.nonce)).collect();
    let b = d.block(creates);
    assert!(b.failed.is_empty(), "{:?}", b.failed);
    let actions = (0..p.calls).map(|i| Action::CallProgram { calls: vec![d.call(&programs[if p.own_programs { i } else { 0 }], p.method, vec![], 0)], fuel_limit: p.fuel_limit, storage_limit: 50 * FANG, bindings: vec![] }).collect();
    let txs = d.txs(&alice, actions);
    (d, txs)
}

/// `n` native transfers of Alice's to Bob, run as the next block on a copy of `d`'s state (never
/// executed): `run_block` and `commit_block` together, what every transaction costs whatever its
/// action. The first run warms what is lazy (page faults, the allocator) and isn't counted.
fn native_work(d: &Dev, n: usize) -> (Duration, Option<u64>) {
    let chain_id = d.c.genesis.chain_id.clone();
    let mut took = Duration::ZERO;
    let mut faults = None;
    for _ in 0..2 {
        let transfers = d.txs(&keys(ALICE), (0..n).map(|_| Action::Transfer { to: d.address(BOB), amount: 1 }).collect());
        let f = d.finalized(transfers);
        let mut copy = d.c.state.clone();
        let before = minor_faults();
        let started = Instant::now();
        copy.run_block(&chain_id, f.height, &f.hash, &f.proposer, f.view, f.parent_view, &f.txs, |_, _, _| {});
        copy.commit_block().unwrap();
        took = started.elapsed();
        faults = minor_faults().zip(before).map(|(after, before)| after - before);
    }
    (took, faults)
}

/// Executes `txs` as the next block of `d`, timing its parts: `run_block` and `commit_block` on a
/// copy of the state (the same work `apply_block` does), then the real execution in two halves, in
/// memory (`execute_begin`) and written (`execute_end`: the block logs, the database commit).
fn time(d: &mut Dev, txs: Vec<Tx>) -> Run {
    let (native, native_faults) = native_work(d, txs.len());
    let budget = Param::MaxBlockFuel.default(&d.c.state.params);
    let f = d.finalized(txs);
    let chain_id = d.c.genesis.chain_id.clone();
    let mut copy = d.c.state.clone();
    let before = minor_faults();
    let started = Instant::now();
    copy.run_block(&chain_id, f.height, &f.hash, &f.proposer, f.view, f.parent_view, &f.txs, |_, _, _| {});
    let run_block = started.elapsed();
    let faults = minor_faults().zip(before).map(|(after, before)| after - before);
    assert!(copy.programs.fault.is_none());
    let started = Instant::now();
    copy.commit_block().unwrap();
    let commit_block = started.elapsed();
    let txs = f.txs.len();
    let e = d.c.execute_begin(f).unwrap();
    let started = Instant::now();
    d.c.execute_end(e).unwrap();
    let persist = started.elapsed();
    assert_eq!(d.c.state.root(), copy.root(), "both paths reach the same root");
    let b = d.c.last_block().unwrap().clone();
    assert!(b.failed.is_empty(), "{:?}", b.failed.iter().take(3).collect::<Vec<_>>());
    assert_eq!(b.programs.len(), txs, "a receipt for every call");
    let (counted, reserved) = b.programs.values().fold((0, 0), |(c, r), p| (c + p.fuel.load + p.fuel.used, r + p.fuel.reserved));
    Run { txs, counted, reserved, budget, run_block, commit_block, persist, native, faults, native_faults }
}

/// The run in the middle by what `rule` judges.
fn median(mut runs: Vec<Run>, rule: Rule) -> Run {
    runs.sort_by_key(|r| r.judged(rule));
    runs.swap_remove(runs.len() / 2)
}

fn ms(d: Duration) -> String {
    format!("{:.2}", d.as_secs_f64() * 1e3)
}

/// Runs one pattern, prints it, and says whether it holds the rule.
fn bench(p: &Pattern) -> bool {
    let runs: Vec<Run> = (0..p.reps)
        .map(|_| {
            let (mut d, txs) = build(p);
            time(&mut d, txs)
        })
        .collect();
    let r = median(runs.clone(), p.rule);
    let worst = runs.iter().map(|r| r.ns_per_fuel(r.judged(p.rule))).fold(0.0, f64::max);
    let ns = r.ns_per_fuel(r.judged(p.rule));
    let go = worst <= MAX_NS_PER_FUEL;
    println!("\n### {}\n", p.name);
    println!("The rule is judged on {}.\n", match p.rule {
        Rule::VmShare => "the VM's share of run_block + commit_block (the block's, less the same number of native transfers'); persist is apart",
        Rule::Whole => "the whole block: run_block, commit_block and persist",
    });
    println!("| txs | counted fuel | reserved fuel | run_block | commit_block | block work | {} native transfers | VM share | persist | ns per counted fuel, judged (median, worst of {}) |", r.txs, p.reps);
    println!("|---|---|---|---|---|---|---|---|---|---|");
    println!("| {} | {} | {} | {} ms | {} ms | {} ms | {} ms | {} ms | {} ms | {:.2}, {:.2} |", r.txs, r.counted, r.reserved, ms(r.run_block), ms(r.commit_block), ms(r.work()), ms(r.native), ms(r.vm_share()), ms(r.persist), ns, worst);
    println!(
        "\nper transaction: block work {:.1} us, a native transfer {:.1} us, the VM's share {:.1} us; ns per counted fuel: block work {:.2}, VM share {:.2}, persist {:.2}, whole block {:.2}",
        r.per_tx(r.work()),
        r.per_tx(r.native),
        r.per_tx(r.vm_share()),
        r.ns_per_fuel(r.work()),
        r.ns_per_fuel(r.vm_share()),
        r.ns_per_fuel(r.persist),
        r.ns_per_fuel(r.total()),
    );
    if r.faults.is_some() {
        println!("minor page faults in run_block (Linux): the block {}, the same number of native transfers {}", faults_per(r.faults, r.txs), faults_per(r.native_faults, r.txs));
    }
    if p.calls >= MAX_BLOCK_TXS {
        // The block is full of transactions long before it is full of fuel: a block of the budget's
        // worth isn't reachable with this pattern, and what bounds its time is the cost of 512
        // transactions (whatever fuel they count), which `Tx::check_format`, the fee phase and a
        // thread each cost before any program runs.
        println!(
            "this pattern is bound by MAX_BLOCK_TXS ({MAX_BLOCK_TXS} txs), not by fuel: the block counts {} fuel, {:.0}% of the default budget ({}), so its time is {MAX_BLOCK_TXS} x the per-transaction cost above, {} ms of block work",
            r.counted,
            r.counted as f64 * 100.0 / r.budget as f64,
            r.budget,
            ms(r.work()),
        );
    } else {
        println!(
            "a block of the default budget ({} fuel) at the judged rate: {:.0} ms, of CALIBRATED_BLOCK_FUEL ({}): {:.0} ms (the rule: {:.1} ns per fuel, 150 ms); persist's fixed cost is scaled with it, so these overstate",
            r.budget,
            ns * r.budget as f64 / 1e6,
            blockula_vm::meter::CALIBRATED_BLOCK_FUEL,
            ns * blockula_vm::meter::CALIBRATED_BLOCK_FUEL as f64 / 1e6,
            MAX_NS_PER_FUEL,
        );
    }
    println!("{}: {}", p.name, if go { "GO" } else { "NO-GO" });
    go
}

/// Where the minimal-call block's time goes, and what a block costs with no program in it: the
/// context for a verdict, not part of one. Each of a call's fixed costs is timed on its own over
/// `MAX_BLOCK_TXS` repetitions: the thread with the 64 MiB stack (`exec::PROGRAM_STACK_BYTES`), a
/// fresh engine (`new_block_engine`, which builds its linker), and loading the smallest code into
/// it (validation isn't repeated: it was done at upload; this is the translation); the two pure
/// functions of the transaction every path runs for every transaction, `Tx::check_format` (its
/// hex decoding of the key and the signature) and `Tx::id`, on a call; then 512 native transfers'
/// `run_block`, an empty block's (the end-of-block steps alone), and the persist of an empty block,
/// which is mostly the fsyncs.
fn context() {
    use blockula_vm::engine::{new_block_engine, EngineKind};
    let n = MAX_BLOCK_TXS as u32;
    let schedule = blockula_vm::meter::schedule(2).expect("schedule v0");
    let per = |d: Duration| format!("{:.1} us", d.as_secs_f64() * 1e6 / f64::from(n));
    let started = Instant::now();
    for _ in 0..n {
        std::thread::scope(|s| {
            std::thread::Builder::new().stack_size(super::exec::PROGRAM_STACK_BYTES).spawn_scoped(s, || std::hint::black_box(1)).unwrap().join().unwrap();
        });
    }
    let thread = started.elapsed();
    let started = Instant::now();
    for _ in 0..n {
        std::hint::black_box(new_block_engine(EngineKind::Soroban, schedule).unwrap());
    }
    let engine = started.elapsed();
    let hash = super::digest::wasm_hash(NOP_WASM);
    let started = Instant::now();
    for _ in 0..n {
        let mut e = new_block_engine(EngineKind::Soroban, schedule).unwrap();
        e.load(hash, NOP_WASM).unwrap();
        std::hint::black_box(e);
    }
    let engine_and_load = started.elapsed();
    let (mut d, calls) = build(&PATTERNS[0]);
    let call = &calls[0];
    let started = Instant::now();
    for _ in 0..n {
        std::hint::black_box(call).check_format().unwrap();
    }
    let check_format = started.elapsed();
    let started = Instant::now();
    for _ in 0..n {
        std::hint::black_box(call.id());
    }
    let id = started.elapsed();
    let alice = keys(ALICE);
    let transfers = d.txs(&alice, (0..MAX_BLOCK_TXS).map(|_| Action::Transfer { to: d.address(BOB), amount: 1 }).collect());
    let f = d.finalized(transfers);
    let mut copy = d.c.state.clone();
    let started = Instant::now();
    copy.run_block(&d.c.genesis.chain_id.clone(), f.height, &f.hash, &f.proposer, f.view, f.parent_view, &f.txs, |_, _, _| {});
    let transfers = started.elapsed();
    let empty = d.finalized(vec![]);
    let mut copy = d.c.state.clone();
    let started = Instant::now();
    copy.run_block(&d.c.genesis.chain_id.clone(), empty.height, &empty.hash, &empty.proposer, empty.view, empty.parent_view, &empty.txs, |_, _, _| {});
    let end_block = started.elapsed();
    let e = d.c.execute_begin(empty).unwrap();
    let started = Instant::now();
    d.c.execute_end(e).unwrap();
    let persist = started.elapsed();
    println!("\n### context (per call of the minimal-call block, each cost timed alone; not part of a verdict)\n");
    println!("| a thread with a 64 MiB stack | a new engine | a new engine and the load of the smallest code | Tx::check_format of a call | Tx::id of a call | {MAX_BLOCK_TXS} native transfers, run_block | an empty block, run_block | persist of an empty block |");
    println!("|---|---|---|---|---|---|---|---|");
    println!("| {} | {} | {} | {} | {} | {} ms ({}/tx) | {} ms | {} ms |", per(thread), per(engine), per(engine_and_load), per(check_format), per(id), ms(transfers), per(transfers), ms(end_block), ms(persist));
    remainder(&d, &calls[0]);
    threads_by_stack();
}

/// What's left of a minimal call once the thread, the engine and the load are not paid per call
/// (A3), each piece timed alone over `MAX_BLOCK_TXS` repetitions on the state of the minimal-call
/// block (not part of a verdict): the program rules of admission (`programs::admit`), the run's
/// notes (`Run::new`: the fee parts and the fuel figures, with the transaction's id handed in), the
/// block's fuel count, the call's view of the chain, the executor on a warm engine holding the code
/// (instantiation, the frame, the `nop`, the overlay and the commit checks), the commit of its
/// (empty) diff, and the receipt (the run's facts noted and recorded).
fn remainder(d: &Dev, tx: &Tx) {
    use blockula_vm::engine::{new_block_engine, EngineKind};
    use blockula_vm::exec::{execute, Call, TxLimits};
    let n = MAX_BLOCK_TXS as u32;
    let per = |d: Duration| format!("{:.1} us", d.as_secs_f64() * 1e6 / f64::from(n));
    let schedule = blockula_vm::meter::schedule(2).expect("schedule v0");
    let state = d.c.state.clone();
    let height = d.next();
    let Action::CallProgram { calls, fuel_limit, storage_limit, .. } = &tx.action else { panic!("a call") };
    let sender = tx.sender().unwrap();
    let signer = crate::crypto::address_bytes(&sender).unwrap();
    let call = Call { program: crate::crypto::address_bytes(&calls[0].program).unwrap(), version_digest: super::call::hash(&calls[0].version_digest).unwrap(), method: calls[0].method.clone(), input: calls[0].input.0.clone(), value: 0, kind: MethodKind::Call };
    let limits = TxLimits { fuel_limit: *fuel_limit, storage_limit: *storage_limit };
    let id = tx.id();

    let started = Instant::now();
    for _ in 0..n {
        std::hint::black_box(super::admit(&state, std::hint::black_box(tx), height, ApplyMode::Block)).unwrap();
    }
    let admit = started.elapsed();
    // What `State::admit` and `run_block` do for the call that a native transfer doesn't: the fee
    // with fuel in it, the accounts the action names, and the call's fields decoded for the VM.
    let started = Instant::now();
    for _ in 0..n {
        std::hint::black_box(crate::econ::check_fee(&state, std::hint::black_box(tx), height)).unwrap();
    }
    let fee = started.elapsed();
    let started = Instant::now();
    for _ in 0..n {
        std::hint::black_box(state.named_by(std::hint::black_box(tx)));
    }
    let named = started.elapsed();
    let started = Instant::now();
    for _ in 0..n {
        let Action::CallProgram { calls, bindings, .. } = &std::hint::black_box(tx).action else { panic!("a call") };
        let decoded = calls.iter().map(|c| Call { program: crate::crypto::address_bytes(&c.program).unwrap(), version_digest: super::call::hash(&c.version_digest).unwrap(), method: c.method.clone(), input: c.input.0.clone(), value: c.value, kind: MethodKind::Call }).collect::<Vec<_>>();
        std::hint::black_box((crate::crypto::address_bytes(&sender).unwrap(), super::call::bindings(bindings).unwrap(), decoded));
    }
    let decode = started.elapsed();
    let started = Instant::now();
    for _ in 0..n {
        std::hint::black_box(ProgramRun::new(&state, std::hint::black_box(tx), id.clone(), height));
    }
    let notes = started.elapsed();
    let run = ProgramRun::new(&state, tx, id.clone(), height);
    let mut counted = state.clone();
    let started = Instant::now();
    for _ in 0..n {
        super::count_fuel(&mut counted, &mut Journal::default(), std::hint::black_box(tx), height, ApplyMode::Block);
    }
    let count = started.elapsed();
    let started = Instant::now();
    for _ in 0..n {
        std::hint::black_box(ProgramView::new(&state, &run, [call.program], None).unwrap());
    }
    let view = started.elapsed();
    let view_of = ProgramView::new(&state, &run, [call.program], None).unwrap();
    let mut engine = new_block_engine(EngineKind::Soroban, schedule).unwrap();
    for (hash, wasm) in view_of.codes() {
        engine.load(hash, wasm).unwrap();
    }
    let calls_vec = [call];
    let started = Instant::now();
    for _ in 0..n {
        std::hint::black_box(execute(&*engine, &view_of, signer, &calls_vec, &[], limits, schedule)).unwrap();
    }
    let executor = started.elapsed();
    let receipt = execute(&*engine, &view_of, signer, &calls_vec, &[], limits, schedule).unwrap();
    assert!(receipt.ok() && receipt.diff.storage.is_empty(), "the nop succeeds and writes nothing");
    let mut committed = state.clone();
    let started = Instant::now();
    for _ in 0..n {
        let before = super::commit::control(&committed);
        std::hint::black_box(super::receipt::storage_change(&committed, &receipt.diff));
        super::commit::apply_diff(&mut committed, &mut Journal::default(), &receipt.diff).unwrap();
        super::commit::check_control(&committed, before, height).unwrap();
    }
    let commit = started.elapsed();
    let mut recorded = state.clone();
    let started = Instant::now();
    for _ in 0..n {
        run.notes.borrow_mut().ran(&receipt);
        super::receipt::record(&mut recorded, tx, &sender, &run, &Ok(()));
    }
    let receipt_time = started.elapsed();
    println!("\n### context: what's left of a minimal call, each piece timed alone (per call; not part of a verdict)\n");
    println!("| the program rules of admission (`programs::admit`) | the fee check with fuel (`econ::check_fee`) | the accounts the action names (`named_by`) | the call decoded for the VM | the run's notes (`Run::new`) | the block's fuel count | the call's view (`ProgramView::new`) | the executor on a warm engine (instantiation, `nop`, checks) | the commit of its empty diff | the receipt |");
    println!("|---|---|---|---|---|---|---|---|---|---|");
    println!("| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |", per(admit), per(fee), per(named), per(decode), per(notes), per(count), per(view), per(executor), per(commit), per(receipt_time));
}

/// A thread started and joined by the size of its stack, per thread over `MAX_BLOCK_TXS`
/// repetitions: what the 64 MiB stack costs against the 4 KiB to 1 MiB the deepest scenario needs
/// (node/vm's `CALIBRATION.md` section 7: 16 to 40 KiB), and what the OS does with a mapping that
/// is never used (not part of a verdict; `PROGRAM_STACK_BYTES` is not changed by this stage).
fn threads_by_stack() {
    let n = MAX_BLOCK_TXS as u32;
    println!("\n### context: a thread started and joined, by stack size (per thread; not part of a verdict)\n");
    let sizes: [(usize, &str); 6] = [(64 << 20, "64 MiB (PROGRAM_STACK_BYTES)"), (16 << 20, "16 MiB"), (4 << 20, "4 MiB"), (1 << 20, "1 MiB"), (256 << 10, "256 KiB"), (64 << 10, "64 KiB")];
    println!("| {} |", sizes.map(|(_, name)| name).join(" | "));
    println!("|{}", "---|".repeat(sizes.len()));
    let cells: Vec<String> = sizes
        .iter()
        .map(|(bytes, _)| {
            let started = Instant::now();
            for _ in 0..n {
                std::thread::scope(|s| {
                    std::thread::Builder::new().stack_size(*bytes).spawn_scoped(s, || std::hint::black_box(1)).unwrap().join().unwrap();
                });
            }
            format!("{:.1} us", started.elapsed().as_secs_f64() * 1e6 / f64::from(n))
        })
        .collect();
    println!("| {} |", cells.join(" | "));
}

/// The three patterns, one after another (each a block on fresh chains, so the machine isn't asked
/// for more than one at a time). Fails on a `NO-GO`.
#[test]
#[ignore = "a benchmark: cd node && cargo test --release --locked programs::bench -- --ignored --nocapture --test-threads=1"]
fn node_patterns_hold_the_two_and_a_half_ns_per_fuel_rule() {
    println!("\n## node benches (docs/PROGRAMS-V0.md 5.2 item 3), {} {}", std::env::consts::ARCH, std::env::consts::OS);
    let per_tx = std::env::var_os("BLOCKULA_BENCH_PER_TX").is_some();
    println!("\nblocks run as {}", if per_tx { "a thread and an engine per program transaction (BLOCKULA_BENCH_PER_TX: the path before A3)" } else { "passes: one thread and one engine per block pass (A3)" });
    // `BLOCKULA_BENCH_ONLY=minimal` runs the minimal calls alone, without the context tables: what a
    // profiler (perf, sample) is pointed at.
    let only = std::env::var("BLOCKULA_BENCH_ONLY").ok();
    let run = || {
        if only.is_none() {
            context();
        }
        PATTERNS.iter().filter(|p| only.as_deref().is_none_or(|o| p.name.starts_with(o))).map(|p| (p.name, bench(p))).collect::<Vec<_>>()
    };
    let verdicts: Vec<(&str, bool)> = if per_tx { super::pass::per_transaction(run) } else { run() };
    let failed: Vec<&str> = verdicts.iter().filter(|(_, go)| !go).map(|(name, _)| *name).collect();
    assert!(failed.is_empty(), "NO-GO: {failed:?} ran slower than {MAX_NS_PER_FUEL} ns per counted fuel");
}
