//! A3's tests (docs/PROGRAMS-V0.md §5.2 item 3, §6.1 steps 3-4; `pass.rs`): one thread and one
//! engine per block pass changes no outcome, fuel figure, receipt, fault record or state root.
//!
//! - **Differential** (`DIFF-*`): the same blocks and trials run the way they did before A3, a
//!   thread and an engine per program transaction (`per_transaction`), and as a pass, and must read
//!   byte for byte alike: blocks executed by two nodes of one chain (every outcome, receipt, fuel
//!   figure, the block's `programs` field and the state root, over uploads, creates, call batches,
//!   payable calls, payouts, every way a call fails and every per-transaction bound, native
//!   transfers between them); a node fault at the first, a middle and the last program
//!   transaction of a block; proposals and verifications with a fault (what they include, the
//!   quarantine, what the trial marker named while each transaction ran).
//! - **What an engine holds decides nothing**: a transaction reaches the programs it declares and
//!   no others, whatever an earlier transaction loaded; a fault means the next transaction gets a
//!   new engine; the memory bound rebuilds one; an engine is never shared across schedules.
//! - **Where it runs**: a block without a program transaction has no thread; a pass thread the OS
//!   refuses three times falls back to a thread per transaction; a panic comes out on the caller's
//!   thread with the trial marker as it was; the end-of-block steps stay on the caller's thread.

use super::super::digest;
use super::super::params::Param;
use super::super::probe::*;
use super::super::fault::{Record, COUNTING};
use super::super::receipt;
use super::super::state::{Fault, FaultClass};
use super::*;
use crate::chain::{Outcome, State};
use crate::crypto;
use crate::types::{Action, Binding, Block, HexBytes, InitCall, ProgramCall, ProgramPolicy, Tx};
use blockula_vm::host::MethodKind;
use blockula_vm::interface::{Interface, Method};
use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

fn le(n: u64) -> Vec<u8> {
    n.to_le_bytes().to_vec()
}

fn bytes_of(address: &str) -> Vec<u8> {
    crypto::address_bytes(address).unwrap().to_vec()
}

// ---- the programs ----

const GUESTS: [&str; 4] = ["counter", "token", "escrow", "router"];

/// A guest of W1's (`node/vm/tests/programs`): its code and its interface (format 1).
fn guest(name: &str) -> (Vec<u8>, Vec<u8>) {
    let (wasm, iface): (&[u8], &[u8]) = match name {
        "counter" => (include_bytes!("../../../vm/tests/programs/counter.wasm"), include_bytes!("../../../vm/tests/programs/counter.iface")),
        "token" => (include_bytes!("../../../vm/tests/programs/token.wasm"), include_bytes!("../../../vm/tests/programs/token.iface")),
        "escrow" => (include_bytes!("../../../vm/tests/programs/escrow.wasm"), include_bytes!("../../../vm/tests/programs/escrow.iface")),
        "router" => (include_bytes!("../../../vm/tests/programs/router.wasm"), include_bytes!("../../../vm/tests/programs/router.iface")),
        _ => panic!("no guest {name}"),
    };
    (wasm.to_vec(), iface.to_vec())
}

/// A program with a method for each way a transaction can stop, and a few it can succeed, assembled
/// from this with `wat` 1.262.0 (node/vm's dev-dependency), custom sections (the names) stripped:
///
/// ```wat
/// (module
///   (import "blockula" "input_read_v1" (func $input_read (param i32)))
///   (import "blockula" "abort_v1" (func $abort (param i32 i32 i32)))
///   (import "blockula" "storage_write_v1" (func $write (param i32 i32 i32 i32 i32)))
///   (import "blockula" "self_balance_v1" (func $balance (result i64)))
///   (import "blockula" "transfer_v1" (func $transfer (param i32 i64 i32) (result i32)))
///   (import "blockula" "emit_v1" (func $emit (param i32 i32 i32 i32)))
///   (memory (export "memory") 1 1)
///   (data (i32.const 100) "e")
///   ;; aborts with code 9 and message "e"
///   (func (export "bk_abort") (call $abort (i32.const 9) (i32.const 100) (i32.const 1)))
///   ;; writes 65 values of 16 KiB under distinct keys: one past MAX_WRITTEN_BYTES (1 MiB)
///   (func (export "bk_bytes") (local i32)
///     (loop
///       (i32.store8 (i32.const 65000) (local.get 0))
///       (call $write (i32.const 65000) (i32.const 1) (i32.const 0) (i32.const 16384) (i32.const 0))
///       (local.set 0 (i32.add (local.get 0) (i32.const 1)))
///       (br_if 0 (i32.lt_u (local.get 0) (i32.const 65)))))
///   ;; divides by zero
///   (func (export "bk_div") (drop (i32.div_u (i32.const 1) (i32.const 0))))
///   ;; emits two events of 9,000 bytes: past MAX_EVENT_BYTES (16 KiB) together
///   (func (export "bk_ebytes")
///     (call $emit (i32.const 100) (i32.const 1) (i32.const 1000) (i32.const 9000))
///     (call $emit (i32.const 100) (i32.const 1) (i32.const 1000) (i32.const 9000)))
///   ;; emits 65 events: one past MAX_EVENTS
///   (func (export "bk_events") (local i32)
///     (loop
///       (call $emit (i32.const 100) (i32.const 1) (i32.const 0) (i32.const 0))
///       (local.set 0 (i32.add (local.get 0) (i32.const 1)))
///       (br_if 0 (i32.lt_u (local.get 0) (i32.const 65)))))
///   ;; takes the value it's sent (payable) and does nothing
///   (func (export "bk_fund"))
///   ;; writes 257 distinct two-byte keys: one past MAX_KEYS_WRITTEN
///   (func (export "bk_keys") (local i32)
///     (loop
///       (i32.store16 (i32.const 0) (local.get 0))
///       (i32.store8 (i32.const 2) (i32.const 1))
///       (call $write (i32.const 0) (i32.const 2) (i32.const 2) (i32.const 1) (i32.const 0))
///       (local.set 0 (i32.add (local.get 0) (i32.const 1)))
///       (br_if 0 (i32.lt_u (local.get 0) (i32.const 257)))))
///   ;; loads past its memory
///   (func (export "bk_oob") (drop (i32.load (i32.const 70000))))
///   ;; pays its whole balance to the account whose 32 address bytes are its input
///   (func (export "bk_payout")
///     (call $input_read (i32.const 0))
///     (drop (call $transfer (i32.const 0) (call $balance) (i32.const 0))))
///   ;; never returns: runs out of whatever fuel it has
///   (func (export "bk_spin") (loop (br 0)))
///   ;; unreachable
///   (func (export "bk_trap") unreachable)
///   ;; writes one key and emits one event
///   (func (export "bk_write")
///     (i32.store8 (i32.const 0) (i32.const 7))
///     (call $write (i32.const 0) (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 0))
///     (call $emit (i32.const 100) (i32.const 1) (i32.const 0) (i32.const 1))))
/// ```
const BOUNDS_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x28, 0x07, 0x60, 0x01, 0x7f, 0x00, 0x60, //
    0x03, 0x7f, 0x7f, 0x7f, 0x00, 0x60, 0x05, 0x7f, 0x7f, 0x7f, 0x7f, 0x7f, 0x00, 0x60, 0x00, 0x01, //
    0x7e, 0x60, 0x03, 0x7f, 0x7e, 0x7f, 0x01, 0x7f, 0x60, 0x04, 0x7f, 0x7f, 0x7f, 0x7f, 0x00, 0x60, //
    0x00, 0x00, 0x02, 0x8f, 0x01, 0x06, 0x08, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x75, 0x6c, 0x61, 0x0d, //
    0x69, 0x6e, 0x70, 0x75, 0x74, 0x5f, 0x72, 0x65, 0x61, 0x64, 0x5f, 0x76, 0x31, 0x00, 0x00, 0x08, //
    0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x75, 0x6c, 0x61, 0x08, 0x61, 0x62, 0x6f, 0x72, 0x74, 0x5f, 0x76, //
    0x31, 0x00, 0x01, 0x08, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x75, 0x6c, 0x61, 0x10, 0x73, 0x74, 0x6f, //
    0x72, 0x61, 0x67, 0x65, 0x5f, 0x77, 0x72, 0x69, 0x74, 0x65, 0x5f, 0x76, 0x31, 0x00, 0x02, 0x08, //
    0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x75, 0x6c, 0x61, 0x0f, 0x73, 0x65, 0x6c, 0x66, 0x5f, 0x62, 0x61, //
    0x6c, 0x61, 0x6e, 0x63, 0x65, 0x5f, 0x76, 0x31, 0x00, 0x03, 0x08, 0x62, 0x6c, 0x6f, 0x63, 0x6b, //
    0x75, 0x6c, 0x61, 0x0b, 0x74, 0x72, 0x61, 0x6e, 0x73, 0x66, 0x65, 0x72, 0x5f, 0x76, 0x31, 0x00, //
    0x04, 0x08, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x75, 0x6c, 0x61, 0x07, 0x65, 0x6d, 0x69, 0x74, 0x5f, //
    0x76, 0x31, 0x00, 0x05, 0x03, 0x0d, 0x0c, 0x06, 0x06, 0x06, 0x06, 0x06, 0x06, 0x06, 0x06, 0x06, //
    0x06, 0x06, 0x06, 0x05, 0x04, 0x01, 0x01, 0x01, 0x01, 0x07, 0x89, 0x01, 0x0d, 0x06, 0x6d, 0x65, //
    0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00, 0x08, 0x62, 0x6b, 0x5f, 0x61, 0x62, 0x6f, 0x72, 0x74, 0x00, //
    0x06, 0x08, 0x62, 0x6b, 0x5f, 0x62, 0x79, 0x74, 0x65, 0x73, 0x00, 0x07, 0x06, 0x62, 0x6b, 0x5f, //
    0x64, 0x69, 0x76, 0x00, 0x08, 0x09, 0x62, 0x6b, 0x5f, 0x65, 0x62, 0x79, 0x74, 0x65, 0x73, 0x00, //
    0x09, 0x09, 0x62, 0x6b, 0x5f, 0x65, 0x76, 0x65, 0x6e, 0x74, 0x73, 0x00, 0x0a, 0x07, 0x62, 0x6b, //
    0x5f, 0x66, 0x75, 0x6e, 0x64, 0x00, 0x0b, 0x07, 0x62, 0x6b, 0x5f, 0x6b, 0x65, 0x79, 0x73, 0x00, //
    0x0c, 0x06, 0x62, 0x6b, 0x5f, 0x6f, 0x6f, 0x62, 0x00, 0x0d, 0x09, 0x62, 0x6b, 0x5f, 0x70, 0x61, //
    0x79, 0x6f, 0x75, 0x74, 0x00, 0x0e, 0x07, 0x62, 0x6b, 0x5f, 0x73, 0x70, 0x69, 0x6e, 0x00, 0x0f, //
    0x07, 0x62, 0x6b, 0x5f, 0x74, 0x72, 0x61, 0x70, 0x00, 0x10, 0x08, 0x62, 0x6b, 0x5f, 0x77, 0x72, //
    0x69, 0x74, 0x65, 0x00, 0x11, 0x0a, 0x83, 0x02, 0x0c, 0x0b, 0x00, 0x41, 0x09, 0x41, 0xe4, 0x00, //
    0x41, 0x01, 0x10, 0x01, 0x0b, 0x2f, 0x01, 0x01, 0x7f, 0x03, 0x40, 0x41, 0xe8, 0xfb, 0x03, 0x20, //
    0x00, 0x3a, 0x00, 0x00, 0x41, 0xe8, 0xfb, 0x03, 0x41, 0x01, 0x41, 0x00, 0x41, 0x80, 0x80, 0x01, //
    0x41, 0x00, 0x10, 0x02, 0x20, 0x00, 0x41, 0x01, 0x6a, 0x21, 0x00, 0x20, 0x00, 0x41, 0xc1, 0x00, //
    0x49, 0x0d, 0x00, 0x0b, 0x0b, 0x08, 0x00, 0x41, 0x01, 0x41, 0x00, 0x6e, 0x1a, 0x0b, 0x1e, 0x00, //
    0x41, 0xe4, 0x00, 0x41, 0x01, 0x41, 0xe8, 0x07, 0x41, 0xa8, 0xc6, 0x00, 0x10, 0x05, 0x41, 0xe4, //
    0x00, 0x41, 0x01, 0x41, 0xe8, 0x07, 0x41, 0xa8, 0xc6, 0x00, 0x10, 0x05, 0x0b, 0x21, 0x01, 0x01, //
    0x7f, 0x03, 0x40, 0x41, 0xe4, 0x00, 0x41, 0x01, 0x41, 0x00, 0x41, 0x00, 0x10, 0x05, 0x20, 0x00, //
    0x41, 0x01, 0x6a, 0x21, 0x00, 0x20, 0x00, 0x41, 0xc1, 0x00, 0x49, 0x0d, 0x00, 0x0b, 0x0b, 0x02, //
    0x00, 0x0b, 0x30, 0x01, 0x01, 0x7f, 0x03, 0x40, 0x41, 0x00, 0x20, 0x00, 0x3b, 0x01, 0x00, 0x41, //
    0x02, 0x41, 0x01, 0x3a, 0x00, 0x00, 0x41, 0x00, 0x41, 0x02, 0x41, 0x02, 0x41, 0x01, 0x41, 0x00, //
    0x10, 0x02, 0x20, 0x00, 0x41, 0x01, 0x6a, 0x21, 0x00, 0x20, 0x00, 0x41, 0x81, 0x02, 0x49, 0x0d, //
    0x00, 0x0b, 0x0b, 0x0a, 0x00, 0x41, 0xf0, 0xa2, 0x04, 0x28, 0x02, 0x00, 0x1a, 0x0b, 0x0f, 0x00, //
    0x41, 0x00, 0x10, 0x00, 0x41, 0x00, 0x10, 0x03, 0x41, 0x00, 0x10, 0x04, 0x1a, 0x0b, 0x07, 0x00, //
    0x03, 0x40, 0x0c, 0x00, 0x0b, 0x0b, 0x03, 0x00, 0x00, 0x0b, 0x20, 0x00, 0x41, 0x00, 0x41, 0x07, //
    0x3a, 0x00, 0x00, 0x41, 0x00, 0x41, 0x01, 0x41, 0x00, 0x41, 0x01, 0x41, 0x00, 0x10, 0x02, 0x41, //
    0xe4, 0x00, 0x41, 0x01, 0x41, 0x00, 0x41, 0x01, 0x10, 0x05, 0x0b, 0x0b, 0x08, 0x01, 0x00, 0x41, //
    0xe4, 0x00, 0x0b, 0x01, 0x65, //
];

/// The bounds program's interface: its twelve methods, all of kind `Call`, `fund` the only payable one.
fn bounds_interface() -> Vec<u8> {
    let names = ["abort", "bytes", "div", "ebytes", "events", "fund", "keys", "oob", "payout", "spin", "trap", "write"];
    let methods = names.map(|name| Method { name: name.into(), kind: MethodKind::Call, payable: name == "fund", args: vec![], returns: None });
    blockula_vm::interface::encode(&Interface { methods: methods.into() }).unwrap()
}

/// A create of the uploaded `wasm` and `iface` with an `init` call.
fn create_with_init(wasm: &[u8], iface: &[u8], method: &str, input: Vec<u8>) -> Action {
    Action::CreateProgram {
        wasm_hash: hex::encode(digest::wasm_hash(wasm)),
        interface_hash: hex::encode(digest::interface_hash(iface)),
        authority: None,
        upgrade_delay_blocks: 0,
        policy: ProgramPolicy::Open,
        accepts_transfers: false,
        init: Some(InitCall { method: method.into(), input: HexBytes(input), value: 0 }),
        fuel_limit: FUEL,
        storage_limit: FANG,
        bindings: vec![],
    }
}

/// A `CallProgram` with its own limits.
fn call_with(calls: Vec<ProgramCall>, bindings: Vec<Binding>, fuel_limit: u64, storage_limit: u64) -> Action {
    Action::CallProgram { calls, fuel_limit, storage_limit, bindings }
}

fn transfer(to: &str, fangs: u64) -> Action {
    Action::Transfer { to: to.into(), amount: fangs * FANG }
}

// ---- two nodes of one chain ----

/// Two nodes of one chain (the same genesis and validator key, so the same fee sink), `a` executing
/// blocks as passes and `b` the way it did before A3, a thread and an engine per program
/// transaction. Every block goes to both, and must come out the same.
struct Pair {
    a: Dev,
    b: Dev,
}

impl Pair {
    fn new(tag: &str) -> Pair {
        let mut a = Dev::new(tag);
        let mut b = a.node(&format!("{tag}-per-tx"));
        for d in [&mut a, &mut b] {
            // Room for the per-transaction bounds the failing calls pass: a transaction's fuel up to
            // its parameter's ceiling, and a block of them.
            let meta = d.c.state.programs.meta.as_mut().unwrap();
            meta.params.insert(Param::MaxTxFuel.key().into(), 20_000_000);
            meta.params.insert(Param::MaxBlockFuel.key().into(), 60_000_000);
        }
        Pair { a, b }
    }

    /// `plan`'s actions, each from the account of the user it names, at that account's next nonces.
    fn txs(&self, plan: Vec<(u8, Action)>) -> Vec<Tx> {
        let mut used: BTreeMap<u8, u64> = BTreeMap::new();
        plan.into_iter()
            .map(|(user, action)| {
                let k = keys(user);
                let base = self.a.c.state.account(&k.address_with(&self.a.prefix())).nonce;
                let n = used.entry(user).or_default();
                let tx = self.a.txs_at(&k, Some(base + *n), 0, vec![action]).remove(0);
                *n += 1;
                tx
            })
            .collect()
    }

    fn nonce(&self, user: u8) -> u64 {
        self.a.c.state.account(&self.a.address(user)).nonce
    }

    /// The program `user`'s account creates with its transaction at `nonce`.
    fn program(&self, user: u8, nonce: u64) -> String {
        digest::program_address(&self.a.prefix(), &crypto::address_bytes(&self.a.address(user)).unwrap(), nonce)
    }

    /// `plan` as the next block on both nodes; the block, which the two must have made alike.
    fn block(&mut self, plan: Vec<(u8, Action)>) -> Block {
        let txs = self.txs(plan);
        self.execute(txs)
    }

    /// `txs` as the next block on both nodes, `a` as a pass and `b` as before A3: the block, and
    /// what each node started and built.
    fn run(&mut self, txs: Vec<Tx>) -> (Block, Counts, Counts) {
        let (fa, fb) = (self.a.finalized(txs.clone()), self.b.finalized(txs));
        let before = counts();
        self.a.c.execute(fa).unwrap();
        let pass = counts().since(before);
        let before = counts();
        per_transaction(|| self.b.c.execute(fb)).unwrap();
        let per_tx = counts().since(before);
        let (a, b) = (self.a.c.last_block().unwrap().clone(), self.b.c.last_block().unwrap().clone());
        assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap(), "block {} differs between the pass and a thread per transaction", a.height);
        assert_eq!(self.a.c.state.root(), self.b.c.state.root());
        (a, pass, per_tx)
    }

    /// [`Pair::run`], and that the two took the paths they are meant to.
    fn execute(&mut self, txs: Vec<Tx>) -> Block {
        let program_txs = txs.iter().filter(|t| is_program_tx(t)).count() as u64;
        let (block, pass, per_tx) = self.run(txs);
        assert_eq!((pass.pass_threads, pass.tx_threads), (u64::from(program_txs > 0), 0), "a block with a program tx is one pass, and starts no other thread: {pass:?}");
        assert_eq!((per_tx.pass_threads, per_tx.engines + per_tx.upload_checks), (0, per_tx.tx_threads), "the reference starts a thread for each program tx's engine or check: {per_tx:?}");
        assert!(pass.engines <= 1 && pass.engines <= per_tx.engines && pass.upload_checks == per_tx.upload_checks, "{pass:?} against {per_tx:?}");
        block
    }
}

/// Everything the differential blocks need, deployed on both nodes of a pair.
struct World {
    p: Pair,
    probe: [String; 2],
    bounds: [String; 2],
    counter: String,
    token: String,
    escrow: String,
    router: String,
}

impl World {
    /// Uploads, creates (the token with an `init`) and funded users, in the first two blocks.
    fn new(tag: &str) -> World {
        let mut p = Pair::new(tag);
        let [(counter_w, counter_i), (token_w, token_i), (escrow_w, escrow_i), (router_w, router_i)] = GUESTS.map(guest);
        let bounds_i = bounds_interface();
        let (bob, carol) = (p.a.address(BOB), p.a.address(CAROL));
        // New code and code the chain holds already (the probe, uploaded twice), with transfers
        // between.
        p.block(vec![
            (ALICE, transfer(&bob, 5)),
            (ALICE, upload(PROBE_WASM, &probe_interface())),
            (BOB, upload(BOUNDS_WASM, &bounds_i)),
            (CAROL, upload(&counter_w, &counter_i)),
            (ALICE, upload(&token_w, &token_i)),
            (BOB, upload(&escrow_w, &escrow_i)),
            (CAROL, upload(&router_w, &router_i)),
            (ALICE, upload(PROBE_WASM, &probe_interface())),
            (BOB, transfer(&carol, 1)),
        ]);
        let (a0, b0, c0) = (p.nonce(ALICE), p.nonce(BOB), p.nonce(CAROL));
        let (probe, bounds, escrow, counter, router, token) = ([p.program(ALICE, a0), p.program(ALICE, a0 + 1)], [p.program(BOB, b0), p.program(BOB, b0 + 1)], p.program(BOB, b0 + 2), p.program(CAROL, c0), p.program(CAROL, c0 + 1), p.program(ALICE, a0 + 3));
        // Creates with `init` (the token's, giving the router a supply) and without.
        let b = p.block(vec![
            (ALICE, create_probe()),
            (ALICE, create_probe()),
            (BOB, create(BOUNDS_WASM, &bounds_i)),
            (BOB, create(BOUNDS_WASM, &bounds_i)),
            (ALICE, transfer(&carol, 1)),
            (ALICE, create_with_init(&token_w, &token_i, "init", [bytes_of(&router), le(1_000)].concat())),
            (BOB, create(&escrow_w, &escrow_i)),
            (CAROL, create(&counter_w, &counter_i)),
            (CAROL, create(&router_w, &router_i)),
        ]);
        assert!(b.failed.is_empty(), "{:?}", b.failed);
        World { p, probe, bounds, counter, token, escrow, router }
    }

    fn call(&self, program: &str, method: &str, input: Vec<u8>, value: u64) -> ProgramCall {
        self.p.a.call(program, method, input, value)
    }

    fn one(&self, program: &str, method: &str) -> Action {
        calls(vec![self.call(program, method, vec![], 0)], vec![])
    }

    /// Calls that succeed: batches over code several transactions share and code they don't,
    /// payable calls, native transfers between them.
    /// The escrow's deal `deal` is opened (ids can't repeat).
    fn successes(&self, deal: u64) -> Vec<(u8, Action)> {
        let (a, bob, carol) = (&self.p.a, self.p.a.address(BOB), self.p.a.address(CAROL));
        vec![
            (ALICE, self.one(&self.counter, "increment")),
            (ALICE, self.one(&self.counter, "increment")),
            (BOB, transfer(&carol, 1)),
            (BOB, calls(vec![self.call(&self.router, "send", [bytes_of(&self.token), bytes_of(&carol), le(250)].concat(), 0), self.call(&self.escrow, "open", [le(deal), bytes_of(&carol)].concat(), 5 * FANG)], vec![a.bind(&self.token)])),
            (CAROL, calls(vec![self.call(&self.probe[0], "fund", vec![], FANG)], vec![])),
            (ALICE, calls(vec![self.call(&self.probe[0], "ret", vec![], 0), self.call(&self.probe[1], "emit", vec![], 0)], vec![])),
            (BOB, calls(vec![self.call(&self.bounds[0], "write", vec![], 0), self.call(&self.bounds[1], "fund", vec![], 2 * FANG)], vec![])),
            (CAROL, transfer(&bob, 1)),
            (ALICE, self.one(&self.counter, "increment")),
            (BOB, self.one(&self.bounds[0], "write")),
        ]
    }

    /// Calls that fail, each a different way, and transactions that never run: charged, rolled back,
    /// and the same on every node.
    fn failures(&self) -> Vec<(u8, Action)> {
        let (a, alice, bob) = (&self.p.a, self.p.a.address(ALICE), self.p.a.address(BOB));
        let on = |program: &str, method: &str| calls(vec![self.call(program, method, vec![], 0)], vec![]);
        let limited = |method: &str, fuel: u64, storage: u64| call_with(vec![self.call(&self.bounds[0], method, vec![], 0)], vec![], fuel, storage);
        vec![
            // Upload refusals (the check runs on the pass's thread).
            (ALICE, upload(FLOAT_WASM, &probe_interface())),
            (ALICE, upload(PROBE_WASM, &bounds_interface())),
            // A program's doing.
            (BOB, limited("abort", 200_000, FANG)),
            (CAROL, limited("trap", 200_000, FANG)),
            (ALICE, limited("div", 200_000, FANG)),
            (BOB, limited("oob", 200_000, FANG)),
            (CAROL, limited("spin", 300_000, FANG)),
            // Each per-transaction bound.
            (ALICE, limited("keys", 3_000_000, 50 * FANG)),
            (BOB, limited("bytes", 13_000_000, 300 * FANG)),
            (CAROL, limited("events", 2_000_000, FANG)),
            (ALICE, limited("ebytes", 2_000_000, FANG)),
            // The probe's: an abort, and a nested call that runs out of the share it was given.
            (BOB, on(&self.probe[0], "no")),
            (CAROL, calls(vec![self.call(&self.probe[0], "go", bytes_of(&self.probe[1]), 0)], vec![a.bind(&self.probe[1])])),
            // A call that reaches a program it doesn't declare.
            (ALICE, calls(vec![self.call(&self.router, "send", [bytes_of(&self.token), bytes_of(&alice), le(1)].concat(), 0)], vec![])),
            // A payout of what the bounds program holds (its `fund` took 2 FANG), then a transfer.
            (BOB, calls(vec![self.call(&self.bounds[1], "payout", bytes_of(&alice), 0)], vec![])),
            (ALICE, transfer(&bob, 1)),
            // A call whose value its sender can't cover: charged, failed.
            (CAROL, calls(vec![self.call(&self.probe[0], "fund", vec![], 100_000 * FANG)], vec![])),
        ]
    }
}

/// The reasons a block's failed transactions give, joined.
fn reasons(b: &Block) -> String {
    b.failed.values().cloned().collect::<Vec<_>>().join("\n")
}

/// DIFF-1. Whole blocks on two nodes of one chain, one executing them as passes and one as it did
/// before A3: the block (every outcome, the failure texts, the receipts in its `programs` field,
/// the fuel figures, the fee base) and the state root are alike for every block, over every kind of
/// program transaction and every way a call fails.
#[test]
fn diff_whole_blocks_read_the_same_as_a_pass_and_as_a_thread_per_transaction() {
    let mut w = World::new("diff-blocks");
    let b = w.p.block(w.successes(1));
    assert!(b.failed.is_empty() && b.skipped.is_empty(), "{:?}", b.failed);
    assert_eq!(b.programs.len(), 8, "a receipt for each program transaction");
    // Shared code: the three counter calls and the two bounds writes ran.
    assert_eq!(w.p.a.c.state.programs.storage.len(), w.p.b.c.state.programs.storage.len());

    let mut plan = w.failures();
    // A program transaction that never runs: a nonce far past the sender's next. It is skipped
    // identically (not included, nothing charged).
    let mut txs = w.p.txs(std::mem::take(&mut plan));
    let late = w.p.a.txs_at(&keys(BOB), Some(w.p.nonce(BOB) + 70), 0, vec![w.one(&w.counter, "increment")]).remove(0);
    txs.insert(3, late);
    let b = w.p.execute(txs);
    assert_eq!(b.skipped.len(), 1, "the one that never runs");
    let why = reasons(&b);
    for expected in [
        "upload validation rule",
        "the interface doesn't match the code",
        "aborted with code 9",
        "(trap 2)",
        "(trap 3)",
        "(trap 6)",
        "(trap 1)",
        "(trap 27)",
        "(trap 28)",
        "(trap 29)",
        "(trap 30)",
        "(trap 15)",
    ] {
        assert!(why.contains(expected), "no failure says {expected:?}:\n{why}");
    }
    // A block of native transfers has no pass at all, and the next block of calls reads the same.
    let carol = w.p.a.address(CAROL);
    let before = counts();
    w.p.block(vec![(ALICE, transfer(&carol, 1)), (BOB, transfer(&carol, 1))]);
    assert_eq!(counts(), before, "no thread for a block without a program transaction");
    let b = w.p.block(w.successes(2));
    assert!(b.failed.is_empty(), "{:?}", b.failed);
}

// ---- DIFF-2: a node fault at the first, a middle and the last program transaction ----

/// What running a block through `State::run_block` and committing it leaves behind.
#[derive(Debug, PartialEq)]
struct Ran {
    /// Each transaction's names and result (`Err`: skipped).
    results: Vec<(Vec<String>, Result<Outcome, String>)>,
    fault: Option<Fault>,
    /// The state root of the commit, or why the commit was refused.
    committed: Result<String, String>,
    /// The block's `programs` field, if it committed.
    receipts: Option<String>,
}

/// `txs` as the next block on a copy of `base` (a state of `d`'s chain), a pass or the old way.
fn ran(base: &State, d: &Dev, txs: &[Tx], per_tx: bool) -> (Ran, Counts) {
    let f = d.finalized(txs.to_vec());
    let chain_id = d.c.genesis.chain_id.clone();
    let mut st = base.clone();
    let mut results = vec![];
    let before = counts();
    go(per_tx, || st.run_block(&chain_id, f.height, &f.hash, &f.proposer, f.view, f.parent_view, &f.txs, |_, named, r| results.push((named, r.map_err(|e| e.to_string())))));
    let started = counts().since(before);
    let fault = st.programs.fault.clone();
    let committed = st.commit_block().map(|c| hex::encode(c.root)).map_err(|e| e.to_string());
    let receipts = committed.is_ok().then(|| {
        let (mut kept, mut failed) = (vec![], BTreeMap::new());
        for (tx, (_, r)) in f.txs.iter().zip(&results) {
            match r {
                Ok(Outcome::Applied) => kept.push(tx.clone()),
                Ok(Outcome::Failed(why)) => {
                    failed.insert(kept.len(), why.clone());
                    kept.push(tx.clone());
                }
                Err(_) => {}
            }
        }
        format!("{:?}", receipt::for_block(&mut st, f.height, &kept, &failed))
    });
    (Ran { results, fault, committed, receipts }, started)
}

/// DIFF-2. A block of five program transactions between native ones, run on copies of one state
/// (`State::run_block`), with a node fault injected at the first, a middle and the last program
/// transaction: a panic inside the VM (for the last, an upload's check), and a thread the OS
/// refuses (twice, so the retry works, and three times, a `Setup` fault; the pass thread meets the
/// refusal first and the block runs as it did before A3). Every result, the fault the state
/// records, the commit and the receipts read the same as a pass and as a thread per transaction.
#[test]
fn diff_a_fault_at_the_first_a_middle_and_the_last_program_tx_reads_the_same() {
    let mut w = World::new("diff-faults");
    w.p.block(w.successes(1));
    let [alice, bob, carol] = [ALICE, BOB, CAROL].map(|s| w.p.a.address(s));
    let plan = vec![
        (ALICE, transfer(&bob, 1)),
        (BOB, w.one(&w.counter, "increment")),
        (CAROL, w.one(&w.probe[0], "ret")),
        (BOB, transfer(&carol, 1)),
        (ALICE, w.one(&w.bounds[0], "write")),
        (BOB, w.one(&w.counter, "increment")),
        (CAROL, transfer(&alice, 1)),
        (ALICE, upload(FLOAT_WASM, &probe_interface())),
    ];
    let txs = w.p.txs(plan);
    let program_txs: Vec<&Tx> = txs.iter().filter(|t| is_program_tx(t)).collect();
    assert_eq!(program_txs.len(), 5);
    let base = w.p.a.c.state.clone();

    // Nothing injected: five program transactions, one pass and one engine, against five threads.
    let (pass, started) = ran(&base, &w.p.a, &txs, false);
    let (old, then) = ran(&base, &w.p.a, &txs, true);
    assert_eq!(pass, old);
    assert!(pass.fault.is_none() && pass.committed.is_ok() && pass.receipts.is_some());
    assert_eq!((started.pass_threads, started.tx_threads, started.engines, started.upload_checks), (1, 0, 1, 1));
    assert_eq!((then.pass_threads, then.tx_threads, then.engines, then.upload_checks), (0, 5, 4, 1));

    for (kind, class) in [(Injected::Panic, Some(FaultClass::Tx)), (Injected::RefuseSpawns(3), Some(FaultClass::Setup)), (Injected::RefuseSpawns(2), None)] {
        for at in [0, 2, 4] {
            let id = program_txs[at].id();
            inject::arm(&id, kind);
            let (pass, started) = ran(&base, &w.p.a, &txs, false);
            inject::arm(&id, kind);
            let (old, _) = ran(&base, &w.p.a, &txs, true);
            inject::disarm(&id);
            assert_eq!(pass, old, "{kind:?} at program tx {at}");
            assert_eq!(pass.fault.as_ref().map(|f| (f.class, f.tx.clone())), class.map(|c| (c, id.clone())), "{kind:?} at program tx {at}");
            assert_eq!(pass.committed.is_err(), class.is_some(), "a faulted block is never committed");
            // The OS refusing a spawn is met by the pass thread first, and then by the
            // transaction's own: the block runs as before A3, a thread each for the program
            // transactions that get one (all five, or those before the one refused three times).
            let (pass_threads, tx_threads) = match kind {
                Injected::Panic => (1, 0),
                Injected::RefuseSpawns(3) => (0, at as u64),
                _ => (0, 5),
            };
            assert_eq!((started.pass_threads, started.tx_threads), (pass_threads, tx_threads), "{kind:?} at {at}: {started:?}");
        }
    }
}

// ---- DIFF-3: proposals and verifications ----

/// Everything a proposal and two verifications leave that a differential must see.
#[derive(Debug, PartialEq)]
struct Observed {
    packed: Vec<Vec<u8>>,
    /// The quarantine's sizes (ids, senders, upload code), and for each candidate what holds it.
    quarantine: [usize; 3],
    held: Vec<[Option<u64>; 3]>,
    queued: Vec<bool>,
    pending_views: usize,
    /// What the trial marker's file held when each candidate's view was made, in the proposal and in
    /// a verification of a block carrying them all.
    marker_proposing: Vec<Option<Vec<u8>>>,
    marker_verifying: Vec<Option<Vec<u8>>>,
    /// The verdicts on a block carrying all the candidates, and on one carrying those that did not
    /// fault.
    verdicts: [Option<bool>; 2],
    marker_after: Vec<u8>,
    clean: usize,
}

/// Five funded senders, the probe deployed, on both nodes of a pair; and five calls to it, the
/// first with the highest tip, so a proposal tries them in that order.
fn candidates(tag: &str) -> (Pair, Vec<Tx>) {
    let mut p = Pair::new(tag);
    let senders: Vec<u8> = (90..95).collect();
    let funds = senders.iter().map(|s| (ALICE, transfer(&keys(*s).address_with(&p.a.prefix()), 20))).chain([(ALICE, upload(PROBE_WASM, &probe_interface()))]).collect();
    p.block(funds);
    let a0 = p.nonce(ALICE);
    let probe = p.program(ALICE, a0);
    p.block(vec![(ALICE, create_probe())]);
    let size = p.a.tx(&keys(senders[0]), calls(vec![p.a.call(&probe, "ret", vec![], 0)], vec![])).size();
    let base = crate::econ::base_fee(&p.a.c.state, p.a.next() + 1, size);
    let txs = senders.iter().enumerate().map(|(i, s)| p.a.txs_at(&keys(*s), None, base * (5 - i as u64) / 2, vec![calls(vec![p.a.call(&probe, "ret", vec![], 0)], vec![])]).remove(0)).collect();
    (p, txs)
}

/// `f`, the old way (a thread and an engine per program transaction) if `per_tx`.
fn go<R>(per_tx: bool, f: impl FnOnce() -> R) -> R {
    if per_tx {
        per_transaction(f)
    } else {
        f()
    }
}

/// The trial marker's file `marker` is read when each of `txs` builds its view.
fn look_all(txs: &[Tx], marker: &std::path::Path) {
    for t in txs {
        inject::look(&t.id(), marker.to_path_buf());
    }
}

/// `d`'s node proposes on `txs` (all in its mempool), and verifies two blocks, with `kind` injected
/// at `txs[at]`; the old way if `per_tx`.
fn observe(d: &mut Dev, txs: &[Tx], per_tx: bool, kind: Injected, at: usize) -> (Observed, Counts) {
    let marker = d.home.join("program-trial");
    for t in txs {
        d.c.submit(t.clone()).unwrap();
    }
    inject::arm(&txs[at].id(), kind);
    look_all(txs, &marker);
    let r = Round2::new(d, &[]);
    let before = counts();
    let packed = go(per_tx, || r.propose()).expect("a proposal").txs;
    let started = counts().since(before);
    let marker_proposing = txs.iter().map(|t| inject::looked(&t.id())).collect();
    look_all(txs, &marker);
    let everyone = go(per_tx, || r.verify(r.block(txs)));
    let marker_verifying = txs.iter().map(|t| inject::looked(&t.id())).collect();
    let others: Vec<Tx> = txs.iter().enumerate().filter(|(i, _)| *i != at).map(|(_, t)| t.clone()).collect();
    let clean = go(per_tx, || r.verify(r.block(&others)));
    inject::disarm(&txs[at].id());
    let observed = {
        let c = r.chain.lock();
        let next = c.height() + 1;
        Observed {
            packed,
            quarantine: c.quarantine.lens(),
            held: txs.iter().map(|t| c.quarantine.held_until(&Record::of(t), next)).collect(),
            queued: txs.iter().map(|t| c.in_mempool(&t.id())).collect(),
            pending_views: c.strikes.pending_views(),
            marker_proposing,
            marker_verifying,
            verdicts: [everyone, clean],
            marker_after: std::fs::read(&marker).unwrap_or_default(),
            clean: c.verified_clean.len(),
        }
    };
    r.give_back(d);
    (observed, started)
}

/// DIFF-3. A proposal and the verifications of blocks carrying its candidates, with a fault at the
/// first, a middle and the last of five program transactions (a panic in the VM, a `Tx` fault,
/// quarantined; a thread the OS refuses three times, a `Setup` fault that holds the rest of the
/// proposal's program transactions back): what the proposal includes, what it quarantines and
/// leaves queued, what the trial marker named while each transaction ran, the verdicts (a block
/// carrying a transaction that faults is abstained on) and the digests recorded as verified clean
/// are alike as a pass and as a thread per transaction.
#[test]
fn diff_proposals_and_verifications_read_the_same_with_a_fault_at_the_first_a_middle_and_the_last_program_tx() {
    // The faults counted per path are the process's: the tests that read them hold this.
    let _counting = COUNTING.lock();
    for (kind, name) in [(Injected::Panic, "panic"), (Injected::RefuseSpawns(3), "setup")] {
        for at in [0usize, 2, 4] {
            let (mut p, txs) = candidates(&format!("diff-propose-{name}-{at}"));
            let (pass, started) = observe(&mut p.a, &txs, false, kind, at);
            let (old, then) = observe(&mut p.b, &txs, true, kind, at);
            assert_eq!(pass, old, "{name} at {at}");
            if name == "panic" {
                // Every other transaction is packed, the faulting one quarantined and dropped, and a
                // block carrying it abstained on; the one without it is verified clean.
                assert_eq!(pass.packed.len(), 4, "{name} at {at}");
                assert_eq!(pass.quarantine, [1, 1, 0]);
                assert_eq!(pass.queued, (0..5).map(|i| i != at).collect::<Vec<_>>(), "the faulting one and its sender's later ones leave the mempool");
                assert_eq!(pass.verdicts, [None, Some(true)]);
                // The whole proposal is one pass; an engine for the transactions before the fault and
                // one for those after it (the faulting one's went with it).
                assert_eq!((started.pass_threads, started.tx_threads), (1, 0), "{started:?}");
                assert_eq!(started.engines, if at == 4 { 1 } else { 2 }, "{started:?}");
                assert_eq!(then.engines, 5, "an engine for each of the five: {then:?}");
            } else {
                // The packing stops at the fault: the transactions before it are packed, none
                // quarantined, and the rest wait. The OS that refuses a transaction's thread refused
                // the pass thread first, so the proposal ran as before A3.
                assert_eq!(pass.packed.len(), at, "{name} at {at}");
                assert_eq!(pass.quarantine, [0, 0, 0]);
                assert_eq!(pass.queued, vec![true; 5], "nothing is dropped for the node's own trouble");
                assert_eq!(pass.verdicts, [Some(true), Some(true)], "nothing is armed any more once the refusals are used");
                assert_eq!((started.pass_threads, then.pass_threads), (0, 0));
                assert_eq!((started.tx_threads, then.tx_threads), (at as u64, at as u64), "a thread for each transaction before the fault");
            }
        }
    }
}

// ---- what an engine holds decides nothing ----

/// The state after the World's successes, a block's finalized shell, and its transactions:
/// `w.p.a`'s node is not executed further, `State::run_block` runs on copies.
fn run_pass_with(w: &World, txs: &[Tx], per_tx: bool, each: &mut (dyn FnMut(&Tx) + Send)) -> Vec<Result<Outcome, String>> {
    let f = w.p.a.finalized(txs.to_vec());
    let chain_id = w.p.a.c.genesis.chain_id.clone();
    let mut st = w.p.a.c.state.clone();
    let mut results = vec![];
    let run = || st.run_block(&chain_id, f.height, &f.hash, &f.proposer, f.view, f.parent_view, &f.txs, |tx, _, r| {
        results.push(r.map_err(|e| e.to_string()));
        each(tx);
    });
    go(per_tx, run);
    results
}

/// The declared set decides what a transaction reaches, not the engine. Transaction A calls the
/// counter, so the pass's engine holds the counter's code; transaction B, in the same pass, declares
/// the probe alone and has it call the counter. The engine has the code, and B still can't reach
/// it: it fails exactly as it does with an engine of its own, with the unbound program trap, the
/// probe's.
#[test]
fn the_declared_set_decides_what_a_transaction_reaches_not_the_engine() {
    let w = World::new("declared");
    let counter_code = digest::wasm_hash(&guest("counter").0);
    let txs = w.p.txs(vec![(ALICE, w.one(&w.counter, "increment")), (BOB, calls(vec![w.call(&w.probe[0], "go", bytes_of(&w.counter), 0)], vec![]))]);

    let held_by_the_engine = std::sync::atomic::AtomicBool::new(false);
    let before = counts();
    let in_pass = run_pass_with(&w, &txs, false, &mut |tx| {
        // After A: the pass's engine holds the counter's code.
        if tx.id() == txs[0].id() {
            let held = ENGINE.with(|e| e.borrow().as_ref().is_some_and(|held| held.engine.info(&counter_code).is_some()));
            held_by_the_engine.store(held, std::sync::atomic::Ordering::SeqCst);
        }
    });
    let pass = counts().since(before);
    let before = counts();
    let per_tx = run_pass_with(&w, &txs, true, &mut |_| ());
    let old = counts().since(before);

    assert!(held_by_the_engine.load(std::sync::atomic::Ordering::SeqCst), "A's code is in the engine B runs on");
    assert_eq!((pass.pass_threads, pass.engines, old.engines), (1, 1, 2), "B ran on A's engine: {pass:?} against {old:?}");
    assert_eq!(in_pass, per_tx);
    assert_eq!(in_pass[0], Ok(Outcome::Applied));
    let Ok(Outcome::Failed(why)) = &in_pass[1] else { panic!("B should fail: {:?}", in_pass[1]) };
    assert!(why.ends_with("trapped: unbound program (trap 15)"), "{why}");
}

/// A fault means the next program transaction gets a new engine. A proposal's packing is the one
/// pass that goes on after a fault (the faulted trial is rolled back and left out): five calls with
/// a panic injected in the first, a middle and the last of them. Without a fault the five share one
/// engine; with one, the engine that met it is dropped and the transactions after it get another.
#[test]
fn a_fault_means_the_next_program_transaction_gets_a_new_engine() {
    let _counting = COUNTING.lock();
    for (at, engines) in [(None, 1), (Some(0usize), 2), (Some(2), 2), (Some(4), 1)] {
        let (mut p, txs) = candidates("fresh-engine");
        let d = &mut p.a;
        for t in &txs {
            d.c.submit(t.clone()).unwrap();
        }
        if let Some(at) = at {
            inject::arm(&txs[at].id(), Injected::Panic);
        }
        let r = Round2::new(d, &[]);
        let before = counts();
        let block = r.propose().expect("a proposal");
        let started = counts().since(before);
        if let Some(at) = at {
            inject::disarm(&txs[at].id());
        }
        r.give_back(d);
        assert_eq!(block.txs.len(), if at.is_some() { 4 } else { 5 });
        assert_eq!((started.pass_threads, started.tx_threads, started.engines), (1, 0, engines), "a fault at {at:?}: {started:?}");
    }
}

/// A fault raised outside the VM's run drops the engine too (`programs::apply` discards it when it
/// records the fault, whichever step raised it): the first call builds the pass's engine; the second
/// meets stored records that don't hold together (its program's code is gone), a `Tx` fault before
/// the VM is reached; the third, once the proposer's rollback took the fault off, gets a new engine.
#[test]
fn a_fault_outside_the_vms_run_drops_the_engine_too() {
    let w = World::new("outside");
    let txs = w.p.txs(vec![(BOB, w.one(&w.counter, "increment")), (BOB, w.one(&w.probe[0], "ret")), (BOB, w.one(&w.counter, "increment"))]);
    let probe_code = digest::wasm_hash(PROBE_WASM);
    let mut st = w.p.a.c.state.clone();
    let (chain_id, sink, height) = (w.p.a.c.genesis.chain_id.clone(), w.p.a.c.me().unwrap(), w.p.a.next());
    let stored = st.programs.code.get(&probe_code).cloned().expect("the probe's code is stored");
    let before = counts();
    block_pass(txs.iter(), || {
        assert_eq!(st.apply(&txs[0], &chain_id, &sink, height).unwrap(), Outcome::Applied);
        assert!(ENGINE.with(|e| e.borrow().is_some()), "the first call left its engine");
        st.programs.code.remove(&probe_code);
        let failed = st.apply(&txs[1], &chain_id, &sink, height).unwrap();
        assert!(matches!(&failed, Outcome::Failed(why) if why.starts_with("program execution faulted on this node")), "{failed:?}");
        let fault = st.programs.fault.take().expect("a fault");
        assert_eq!(fault.class, FaultClass::Tx);
        assert!(ENGINE.with(|e| e.borrow().is_none()), "the fault took the engine with it");
        st.programs.code.insert(probe_code, stored);
        assert_eq!(st.apply(&txs[2], &chain_id, &sink, height).unwrap(), Outcome::Applied);
    });
    assert_eq!(counts().since(before).engines, 2, "the call after the fault built a new engine");
}

/// A code module of exactly `size` bytes: the smallest valid module and a custom section (which
/// the engine keeps; upload validation would refuse it) named by `tag`.
fn module_of(size: usize, tag: u8) -> Vec<u8> {
    let mut wasm = b"\0asm\x01\0\0\0\x01\x04\x01\x60\0\0\x03\x02\x01\0\x05\x04\x01\x01\x01\x01\x07\x0a\x01\x06memory\x02\0\x0a\x04\x01\x02\0\x0b".to_vec();
    let section = size - wasm.len() - 1 - 3;
    wasm.push(0);
    // A three-byte size: below 2^21.
    wasm.extend([0x80 | (section & 0x7f) as u8, 0x80 | ((section >> 7) & 0x7f) as u8, (section >> 14) as u8]);
    wasm.extend([1, b'a' + tag]);
    wasm.resize(size, 0);
    assert_eq!(wasm.len(), size);
    wasm
}

/// Declares `ids` (indexes into `codes`) to the pass's engine as one transaction does (`with_engine`,
/// loading each), and says which of `codes` the engine holds after. Call it from inside a pass.
fn declare(codes: &[(CodeHash, Vec<u8>)], ids: &[usize]) -> Vec<bool> {
    let set: BTreeMap<CodeHash, &[u8]> = ids.iter().map(|&i| (codes[i].0, codes[i].1.as_slice())).collect();
    with_engine(blockula_vm::meter::schedule(2).unwrap(), &set, |engine| {
        for (hash, wasm) in &set {
            engine.load(*hash, wasm).expect("loads");
        }
        Ok(codes.iter().map(|(hash, _)| engine.info(hash).is_some()).collect::<Vec<_>>())
    })
    .unwrap()
}

/// The memory bound: an engine takes in at most [`ENGINE_CODE_BYTES`] of code, and the transaction
/// whose declared code would take it past that gets a new engine, which holds none of the earlier
/// code, with the engine it replaces already gone. The codes are the largest an upload may be
/// (`MAX_CODE_BYTES`), as many as fill the bound exactly (`fit`): the first `fit` fill the engine,
/// the next rebuilds it, the one after fits beside that; declaring code the engine holds adds
/// nothing. And a code one byte over the engine's last share rebuilds it too: `fit - 1` codes, then
/// one of `MAX_CODE_BYTES + 1` (the bound plus one), where `MAX_CODE_BYTES` exactly would fit.
#[test]
fn the_memory_bound_rebuilds_the_engine() {
    let size = blockula_vm::config::MAX_CODE_BYTES;
    let fit = ENGINE_CODE_BYTES / size;
    assert!(fit >= 2 && fit * size == ENGINE_CODE_BYTES, "the bound is a whole number of uploads, at least two: {ENGINE_CODE_BYTES} / {size}");
    // `fit + 2` of `size`, and one more of one byte more.
    let over = fit + 2;
    let codes: Vec<(CodeHash, Vec<u8>)> = (0..=over as u8).map(|i| ([i + 1; 32], module_of(size + usize::from(usize::from(i) == over), i))).collect();
    let held_only = |ids: &[usize]| (0..=over).map(|i| ids.contains(&i)).collect::<Vec<_>>();
    let tx = bare_tx("a", 0, 10, bare_call());
    let before = counts();
    block_pass([&tx].into_iter(), || {
        for i in 0..fit {
            declare(&codes, &[i]);
            assert_eq!(counts().since(before).engines, 1, "code {i} fits the first engine");
        }
        // Declaring what it holds takes nothing more.
        assert_eq!(declare(&codes, &[0, fit - 1]).iter().filter(|h| **h).count(), fit);
        assert_eq!(counts().since(before).engines, 1);
        assert_eq!(peak_alive(), 1, "one engine beside the work so far");
        // The next would pass the bound: a new engine, with it alone, and the full one gone before it
        // translates anything.
        assert_eq!(declare(&codes, &[fit]), held_only(&[fit]), "the new engine holds only what was just loaded");
        assert_eq!(counts().since(before).engines, 2);
        assert_eq!(peak_alive(), 1, "the engine it replaces was dropped before the work ran");
        // The one after fits beside it.
        assert_eq!(declare(&codes, &[fit + 1]).iter().filter(|h| **h).count(), 2);
        assert_eq!(declare(&codes, &[fit, fit + 1]).iter().filter(|h| **h).count(), 2);
        assert_eq!(counts().since(before).engines, 2);
        // One transaction's declared set always fits an empty engine: `fit` at once fill a new one.
        let all: Vec<usize> = (0..fit).collect();
        assert_eq!(declare(&codes, &all).iter().filter(|h| **h).count(), fit);
        assert_eq!(counts().since(before).engines, 3);
        assert_eq!(peak_alive(), 1);
    });
    assert!(!in_pass() && ENGINE.with(|e| e.borrow().is_none()), "nothing outlives the pass");

    // The bound to the byte. `fit - 1` codes of `size` hold ENGINE_CODE_BYTES - size; one more of
    // `size` still fits (the first pass above), and one `size + 1` long would make ENGINE_CODE_BYTES + 1.
    let before = counts();
    block_pass([&tx].into_iter(), || {
        for i in 0..fit - 1 {
            declare(&codes, &[i]);
        }
        assert_eq!(counts().since(before).engines, 1);
        assert_eq!(declare(&codes, &[over]), held_only(&[over]), "the new engine holds the one code");
        assert_eq!(counts().since(before).engines, 2, "the bound plus one byte is a rebuild");
    });
}

/// An engine isn't shared across schedules, compared by value: a transaction that runs under another
/// schedule than the one the held engine was configured from gets a new one (within a pass the version is one, so this
/// is a guard for whatever a later release adds).
#[test]
fn an_engine_is_never_shared_across_schedules() {
    let first: &'static blockula_vm::meter::Schedule = blockula_vm::meter::schedule(2).unwrap();
    let mut other = *first;
    other.ops.base += 1;
    let other: &'static blockula_vm::meter::Schedule = Box::leak(Box::new(other));
    let tx = bare_tx("a", 0, 10, bare_call());
    let none = BTreeMap::new();
    let before = counts();
    // The schedule's value, at another address: `&SCHEDULE_V0` is a promoted constant, and nothing
    // promises one address for it, so what shares an engine is equality of value.
    let copy: &'static blockula_vm::meter::Schedule = Box::leak(Box::new(*first));
    assert!(!std::ptr::eq(first, copy) && *first == *copy);
    block_pass([&tx].into_iter(), || {
        let run = |schedule| with_engine(schedule, &none, |_| Ok(())).unwrap();
        run(first);
        run(first);
        assert_eq!(counts().since(before).engines, 1, "the same schedule shares");
        run(copy);
        assert_eq!(counts().since(before).engines, 1, "so does an equal one at another address");
        run(other);
        assert_eq!(counts().since(before).engines, 2, "another schedule doesn't");
        run(other);
        run(first);
        assert_eq!(counts().since(before).engines, 3);
    });
}

/// `with_engine` itself puts its engine back only when the run returned `Ok`: a run that faults, or
/// panics, leaves the slot empty and the next one builds another, whether or not `programs::apply`
/// discards it as well (`a_fault_outside_the_vms_run_drops_the_engine_too` pins apply's route; this
/// is the pass-level one, called directly so that apply's can't cover for it).
#[test]
fn a_run_that_faults_or_panics_leaves_no_engine_for_the_next() {
    let schedule = blockula_vm::meter::schedule(2).unwrap();
    let tx = bare_tx("a", 0, 10, bare_call());
    let none = BTreeMap::new();
    let before = counts();
    block_pass([&tx].into_iter(), || {
        let held = || ENGINE.with(|e| e.borrow().is_some());
        let built = || counts().since(before).engines;
        let ok = || with_engine(schedule, &none, |_| Ok(())).unwrap();
        // A run that returns leaves its engine, which the next shares.
        ok();
        assert!(held());
        ok();
        assert_eq!(built(), 1);
        // One that faults takes the engine with it.
        let faulted = with_engine(schedule, &none, |_| Err::<(), _>(Faulted::tx("a fault in the run")));
        assert_eq!(faulted, Err(Faulted::tx("a fault in the run")));
        assert!(!held(), "the engine is not put back after a fault");
        ok();
        assert_eq!(built(), 2, "the next run builds another");
        // And one that panics: the engine unwinds with it.
        let panicked = catch_unwind(AssertUnwindSafe(|| with_engine(schedule, &none, |_| -> Result<(), Faulted> { panic!("a panic in the run") })));
        assert!(panicked.is_err());
        assert!(!held(), "the engine is not put back after a panic");
        ok();
        assert_eq!(built(), 3);
    });
}

/// A pass that mixes creates with an `init` call and plain calls builds one engine: every call site
/// (create.rs, call.rs) asks for the schedule of the version in force, and what shares the engine is
/// that schedule's value, however each site came by its reference. The reference path, a thread and an
/// engine for each program transaction, builds six.
#[test]
fn a_pass_of_creates_with_init_and_calls_builds_one_engine() {
    let mut w = World::new("one-engine");
    let (token_w, token_i) = guest("token");
    let init = |supply: u64| create_with_init(&token_w, &token_i, "init", [bytes_of(&w.router), le(supply)].concat());
    let txs = w.p.txs(vec![
        (ALICE, init(10)),
        (BOB, w.one(&w.counter, "increment")),
        (CAROL, init(20)),
        (BOB, w.one(&w.probe[0], "ret")),
        (ALICE, init(30)),
        (CAROL, w.one(&w.counter, "increment")),
    ]);
    let (block, pass, per_tx) = w.p.run(txs);
    assert!(block.failed.is_empty() && block.programs.len() == 6, "{:?}", block.failed);
    assert_eq!((pass.pass_threads, pass.tx_threads, pass.engines), (1, 0, 1), "three creates with init and three calls, one engine: {pass:?}");
    assert_eq!((per_tx.pass_threads, per_tx.tx_threads, per_tx.engines), (0, 6, 6), "{per_tx:?}");
}

/// An engine the pass cannot build is a `Setup` fault for the transaction that needed it, as it is
/// with a thread of its own: the transaction fails on this node, nothing is built or kept, and the
/// pass's next transaction gets a new engine. (Test builds make the build fail: `fail_engines`.)
#[test]
fn an_engine_that_cannot_be_built_is_a_setup_fault_and_the_next_call_gets_a_new_one() {
    let w = World::new("no-engine");
    let txs = w.p.txs(vec![(BOB, w.one(&w.counter, "increment")), (BOB, w.one(&w.counter, "increment")), (BOB, w.one(&w.counter, "increment"))]);
    let mut st = w.p.a.c.state.clone();
    let (chain_id, sink, height) = (w.p.a.c.genesis.chain_id.clone(), w.p.a.c.me().unwrap(), w.p.a.next());
    let schedule = blockula_vm::meter::schedule(2).unwrap();
    let before = counts();
    block_pass(txs.iter(), || {
        // The first call needs an engine, and the build fails.
        fail_engines(1);
        let failed = st.apply(&txs[0], &chain_id, &sink, height).unwrap();
        assert!(matches!(&failed, Outcome::Failed(why) if why.starts_with("program execution faulted on this node (no engine:")), "{failed:?}");
        let fault = st.programs.fault.take().expect("a fault");
        assert_eq!((fault.class, fault.tx.as_str()), (FaultClass::Setup, txs[0].id().as_str()));
        assert!(ENGINE.with(|e| e.borrow().is_none()), "no engine was kept");
        assert_eq!(counts().since(before).engines, 0, "none was built");
        // The next gets a new one.
        assert_eq!(st.apply(&txs[1], &chain_id, &sink, height).unwrap(), Outcome::Applied);
        assert!(ENGINE.with(|e| e.borrow().is_some()));
        assert_eq!(counts().since(before).engines, 1);
        // A held engine is taken out of the slot before the replacement is built, so a build that
        // fails leaves it empty (here a rebuild asked for by another schedule).
        let mut other = *schedule;
        other.ops.base += 1;
        let other: &'static blockula_vm::meter::Schedule = Box::leak(Box::new(other));
        fail_engines(1);
        let refused = with_engine(other, &BTreeMap::new(), |_| Ok(()));
        assert!(matches!(&refused, Err(f) if f.class == FaultClass::Setup), "{refused:?}");
        assert!(ENGINE.with(|e| e.borrow().is_none()), "the held engine is gone with the failed rebuild");
        assert_eq!(st.apply(&txs[2], &chain_id, &sink, height).unwrap(), Outcome::Applied);
        assert_eq!(counts().since(before).engines, 2, "and the call after it built another");
    });
}

/// A divergence from v1 a test switched on for its thread (`replay::diverge`) holds on the pass
/// thread too, where admission and the transfer handler read it; the pass thread would otherwise
/// leave it unset, and a corpus with a program transaction would stop diverging without a sound.
#[test]
fn a_divergence_the_caller_set_holds_on_the_pass_thread_too() {
    use crate::replay::diverge::{self, Divergence};
    struct Unset;
    impl Drop for Unset {
        fn drop(&mut self) {
            diverge::set(None);
        }
    }
    let (native, program) = (bare_tx("a", 0, 10, Action::Transfer { to: "b".into(), amount: 1 }), bare_tx("a", 1, 10, bare_call()));
    let on = |d| (block_pass([&native].into_iter(), || diverge::on(d)), block_pass([&program].into_iter(), || diverge::on(d)));
    assert_eq!(on(Divergence::Handler), (false, false));
    let _unset = Unset;
    for d in [Divergence::Admission, Divergence::Handler, Divergence::EndOfEpoch] {
        diverge::set(Some(d));
        assert_eq!(on(d), (true, true), "{d:?}: on the caller's thread, and on the pass thread");
        for other in [Divergence::Admission, Divergence::Handler, Divergence::EndOfEpoch].into_iter().filter(|o| *o != d) {
            assert_eq!(on(other), (false, false), "only the one that was set");
        }
    }
    diverge::set(None);
    assert_eq!(on(Divergence::Admission), (false, false), "none set, none on the pass thread");
}

// ---- where it runs ----

/// Only a pass with a program transaction gets a thread, and it runs there: marked as a pass,
/// named, and nested passes are the outer one. A block of native transactions (every v1 block)
/// runs on the caller's thread with no engine.
#[test]
fn only_a_pass_with_a_program_tx_gets_a_thread() {
    let (native, program) = (bare_tx("a", 0, 10, Action::Transfer { to: "b".into(), amount: 1 }), bare_tx("a", 1, 10, bare_call()));
    let here = std::thread::current().id();
    let before = counts();
    let (inside, same) = block_pass([&native, &native].into_iter(), || (in_pass(), std::thread::current().id() == here));
    assert_eq!((inside, same), (false, true), "no program tx, no thread");
    assert_eq!(counts(), before);
    let (inside, name, same) = block_pass([&native, &program].into_iter(), || (in_pass(), std::thread::current().name().map(str::to_string), std::thread::current().id() == here));
    assert!(inside && !same && name.as_deref() == Some("blockula-program-pass"), "{inside} {name:?} {same}");
    assert_eq!(counts().since(before).pass_threads, 1);
    assert!(!in_pass(), "the mark is the pass thread's");
    let nested = block_pass([&program].into_iter(), || block_pass([&program].into_iter(), || std::thread::current().name().map(str::to_string)));
    assert_eq!(nested.as_deref(), Some("blockula-program-pass"));
    assert_eq!(counts().since(before).pass_threads, 2, "a pass inside a pass is the outer one");
}

/// A pass thread the OS refuses is tried twice more at once; refused three times, the pass runs on
/// the caller's thread and each program transaction on a thread of its own, as before A3 (and the
/// block comes out the same).
#[test]
fn a_pass_thread_the_os_refuses_three_times_falls_back_to_a_thread_per_transaction() {
    let mut w = World::new("refused");
    let three = |w: &World| w.p.txs(vec![(ALICE, w.one(&w.counter, "increment")), (BOB, w.one(&w.counter, "increment")), (CAROL, w.one(&w.counter, "increment"))]);
    refuse_pass_threads(2);
    let txs = three(&w);
    let (_, pass, _) = w.p.run(txs);
    assert_eq!((pass.pass_threads, pass.tx_threads, pass.engines), (1, 0, 1), "two refusals, then the pass thread: {pass:?}");
    refuse_pass_threads(3);
    let txs = three(&w);
    let (_, fell_back, per_tx) = w.p.run(txs);
    assert_eq!((fell_back.pass_threads, fell_back.tx_threads, fell_back.engines), (0, 3, 3), "three refusals, a thread each: {fell_back:?}");
    assert_eq!(fell_back, per_tx, "exactly the old path");
}

/// A panic in a pass (node-side code, outside the VM's backstop) comes out on the caller's thread
/// with its payload, as it would have unwound there; the trial marker, whose guard is the caller's,
/// is left as it was (S4c: the panic probably ends the process, and the file is the only thing that
/// remembers what it was trying).
#[test]
fn a_panic_on_the_pass_thread_comes_out_on_the_callers_and_leaves_the_marker_as_it_was() {
    let d = Dev::new("pass-panic");
    let marker = d.c.trial.clone();
    let file = d.home.join("program-trial");
    let tx = bare_tx(&d.address(ALICE), 0, 100, bare_call());
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let mut trial = marker.begin();
        block_pass([&tx].into_iter(), || {
            trial.mark(&tx);
            panic!("a node-side panic on the pass thread");
        })
    }));
    let payload = outcome.expect_err("the panic comes out");
    assert_eq!(payload.downcast_ref::<&str>().copied(), Some("a node-side panic on the pass thread"));
    assert!(!in_pass());
    let left = std::fs::read(&file).expect("the marker's file");
    assert!(String::from_utf8_lossy(&left).contains(&tx.id()), "the file still names the transaction the process died trying");
    // A pass that returns leaves the marker empty, as before.
    let mut trial = marker.begin();
    block_pass([&tx].into_iter(), || trial.mark(&tx));
    drop(trial);
    assert!(std::fs::read(&file).unwrap().iter().all(|b| *b == 0));
}

/// The block's end-of-block steps stay on the caller's thread, so a test that records on it (the
/// test-only version gate) still sees them for a block whose transactions ran as a pass.
#[test]
fn the_end_of_block_steps_stay_on_the_callers_thread() {
    let mut w = World::new("end-of-block");
    crate::upgrade::test_gate::record();
    let txs = w.p.txs(vec![(ALICE, w.one(&w.counter, "increment"))]);
    let f = w.p.a.finalized(txs);
    let (height, chain_id) = (f.height, w.p.a.c.genesis.chain_id.clone());
    let before = counts();
    w.p.a.c.execute(f).unwrap();
    assert_eq!(counts().since(before).pass_threads, 1);
    let seen = crate::upgrade::test_gate::take();
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].0.as_str(), seen[0].1), (chain_id.as_str(), height));
}
