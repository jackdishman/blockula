//! Program execution end to end (WBS S4a's acceptance): W1's four guests (`node/vm/tests/programs`)
//! and three small programs of this file's own, uploaded, created and called through `Chain` on a
//! dev chain born at v2 with programs on at genesis (`produce_block_with`, as a lone validator
//! finalizes blocks), every block ending with `State::debug_check` (the incremental root equals a
//! full rebuild, and every invariant holds, programs' included: `value_held`, `sponsored`,
//! `escrow`, `state_bytes` and each record's totals against the records). Where a rule is the
//! action phase's own re-check and admission (`programs::admit`, WBS S3) refuses the same
//! transaction first, the test shows admission's refusal, then runs the action phase directly
//! (`programs::apply`).
//!
//! Signed fees carry a margin for the fuel the transaction reserves (`programs::fuel_fee`, at the
//! fuel rate, here its floor), and tests compare balances against what was actually charged.

use super::super::state::{blob_key, storage_key, FaultClass, BLOB_CODE, BLOB_INTERFACE};
use super::super::view::inject::{self, Injected};
use super::super::{digest, params::Param, status, Status, RECORD_BYTES};
use super::super::pass::{counts, Counts};
use crate::chain::{Allocation, ApplyMode, Chain, Genesis, Journal, Outcome, State, SummonOpts, Vesting};
use crate::crypto::{self, Keys};
use crate::types::*;
use crate::upgrade::{ScheduledUpgrade, PROGRAMS_FEATURE, PROGRAMS_VERSION};
use blockula_vm::interface::{Interface, Method};
use std::path::PathBuf;

const FANG: u64 = DROPS_PER_FANG;
/// What a signed fee adds for fuel: a reservation's fee at the floor rate (250 milli-drops per
/// fuel) is at most `MAX_RESERVATION / 4` drops, about 16.8M.
const FUEL_MARGIN: u64 = 20_000_000;
/// The calls' `fuel_limit`.
const FUEL: u64 = 2_000_000;

// ---- the programs ----

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

const GUESTS: [&str; 4] = ["counter", "token", "escrow", "router"];

/// An interface of `Call` methods (none payable but `payable`), ascending by name.
fn calls_only(names: &[&str], payable: &[&str]) -> Vec<u8> {
    let mut names = names.to_vec();
    names.sort_unstable();
    let methods = names.iter().map(|n| Method { name: n.to_string(), kind: blockula_vm::host::MethodKind::Call, payable: payable.contains(n), args: vec![], returns: None }).collect();
    blockula_vm::interface::encode(&Interface { methods }).unwrap()
}

/// A program that keeps FANG in storage it pays for itself. Its methods: `fund` (payable: takes
/// value, does nothing), `put` (writes one byte under key `s` with `SPONSOR`: the program bonds the
/// deposit from its own balance) and `drop` (removes it: the deposit goes back to the program).
/// Assembled from:
///
/// ```wat
/// (module
///   (import "blockula" "storage_write_v1" (func (param i32 i32 i32 i32 i32)))
///   (import "blockula" "storage_remove_v1" (func (param i32 i32) (result i32)))
///   (memory (export "memory") 1 1)
///   (data (i32.const 0) "s")
///   (func (export "bk_drop") (drop (call 1 (i32.const 0) (i32.const 1))))
///   (func (export "bk_fund"))
///   (func (export "bk_put") (call 0 (i32.const 0) (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 1))))
/// ```
///
/// with `wat` 1.262.0 (node/vm's dev-dependency): no names, so no custom section.
const SPONSOR_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x12, 0x03, 0x60, 0x05, 0x7f, 0x7f, 0x7f, //
    0x7f, 0x7f, 0x00, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, 0x60, 0x00, 0x00, 0x02, 0x3a, 0x02, 0x08, //
    0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x75, 0x6c, 0x61, 0x10, 0x73, 0x74, 0x6f, 0x72, 0x61, 0x67, 0x65, //
    0x5f, 0x77, 0x72, 0x69, 0x74, 0x65, 0x5f, 0x76, 0x31, 0x00, 0x00, 0x08, 0x62, 0x6c, 0x6f, 0x63, //
    0x6b, 0x75, 0x6c, 0x61, 0x11, 0x73, 0x74, 0x6f, 0x72, 0x61, 0x67, 0x65, 0x5f, 0x72, 0x65, 0x6d, //
    0x6f, 0x76, 0x65, 0x5f, 0x76, 0x31, 0x00, 0x01, 0x03, 0x04, 0x03, 0x02, 0x02, 0x02, 0x05, 0x04, //
    0x01, 0x01, 0x01, 0x01, 0x07, 0x27, 0x04, 0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00, //
    0x07, 0x62, 0x6b, 0x5f, 0x64, 0x72, 0x6f, 0x70, 0x00, 0x02, 0x07, 0x62, 0x6b, 0x5f, 0x66, 0x75, //
    0x6e, 0x64, 0x00, 0x03, 0x06, 0x62, 0x6b, 0x5f, 0x70, 0x75, 0x74, 0x00, 0x04, 0x0a, 0x1d, 0x03, //
    0x09, 0x00, 0x41, 0x00, 0x41, 0x01, 0x10, 0x01, 0x1a, 0x0b, 0x02, 0x00, 0x0b, 0x0e, 0x00, 0x41, //
    0x00, 0x41, 0x01, 0x41, 0x00, 0x41, 0x01, 0x41, 0x01, 0x10, 0x00, 0x0b, 0x0b, 0x07, 0x01, 0x00, //
    0x41, 0x00, 0x0b, 0x01, 0x73,
];

/// A method using a float: `(func (export "bk_x") (drop (f32.const 1)))` beside an exported
/// one-page memory, assembled as [`SPONSOR_WASM`] is. Validation refuses it (`Rule::Float`).
const FLOAT_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03, 0x02, //
    0x01, 0x00, 0x05, 0x04, 0x01, 0x01, 0x01, 0x01, 0x07, 0x11, 0x02, 0x06, 0x6d, 0x65, 0x6d, 0x6f, //
    0x72, 0x79, 0x02, 0x00, 0x04, 0x62, 0x6b, 0x5f, 0x78, 0x00, 0x00, 0x0a, 0x0a, 0x01, 0x08, 0x00, //
    0x43, 0x00, 0x00, 0x80, 0x3f, 0x1a, 0x0b,
];

/// `memory.grow`: `(func (export "bk_x") (drop (memory.grow (i32.const 0))))`, likewise
/// (`Rule::MemoryGrow`).
const GROW_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03, 0x02, //
    0x01, 0x00, 0x05, 0x04, 0x01, 0x01, 0x01, 0x01, 0x07, 0x11, 0x02, 0x06, 0x6d, 0x65, 0x6d, 0x6f, //
    0x72, 0x79, 0x02, 0x00, 0x04, 0x62, 0x6b, 0x5f, 0x78, 0x00, 0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, //
    0x41, 0x00, 0x40, 0x00, 0x1a, 0x0b,
];

fn le(n: u64) -> Vec<u8> {
    n.to_le_bytes().to_vec()
}

// ---- the chain ----

const CHAIN: &str = "programs-dev-1";

fn keys(user: u8) -> Keys {
    Keys::from_seed([user; 32])
}

/// Who signs: three users, funded at genesis.
const ALICE: u8 = 71;
const BOB: u8 = 72;
const CAROL: u8 = 73;

/// A dev chain (one validator, `boo` addresses, short governance periods) born at v2 with programs
/// on at genesis, its users funded with 1,000 FANG each; `edit` adjusts the genesis further.
struct Dev {
    home: PathBuf,
    c: Chain,
}

impl Dev {
    fn new(tag: &str, edit: impl FnOnce(&mut Genesis)) -> Dev {
        let home = std::env::temp_dir().join(format!("blockula-exec-{tag}-{}-{}", crate::chain::now_ms(), std::process::id()));
        let mut o = SummonOpts::new(CHAIN, 1);
        o.address_prefix = crypto::ADDRESS_HRP.into();
        for (k, v) in [("proposal_deposit", 10 * FANG), ("voting_blocks", 10), ("timelock_blocks", 3), ("lock_period_blocks", 5), ("quorum_floor", 0)] {
            o.params.insert(k.into(), v);
        }
        Chain::summon(&home, &o).unwrap();
        let path = home.join("genesis.json");
        let mut g: Genesis = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        g.upgrades = vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 1 }];
        g.features = vec![PROGRAMS_FEATURE.into()];
        for seed in [ALICE, BOB, CAROL] {
            g.allocations.push(Allocation { address: keys(seed).address(), amount: 1_000 * FANG, vesting: None, label: None });
        }
        edit(&mut g);
        g.check().unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&g).unwrap()).unwrap();
        let c = Chain::open(&home).unwrap();
        Dev { home, c }
    }

    fn prefix(&self) -> String {
        self.c.state.params.prefix().to_string()
    }

    /// The height the next block executes at.
    fn next(&self) -> u64 {
        self.c.height() + 1
    }

    /// `actions` from `k`'s account at consecutive nonces from its next one, each paying the
    /// highest base fee already fixed plus the fuel margin.
    fn txs(&self, k: &Keys, actions: Vec<Action>) -> Vec<Tx> {
        let from = k.address_with(&self.prefix());
        let nonce = self.c.state.account(&from).nonce;
        actions
            .into_iter()
            .enumerate()
            .map(|(i, action)| {
                let mut tx = Tx { chain_id: CHAIN.into(), genesis_hash: self.c.genesis_hash.clone(), nonce: nonce + i as u64, from: from.clone(), fee: 0, tip: 0, valid_until: self.c.height() + 100, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
                tx.fee = crate::econ::max_fee(&self.c.state, tx.size()) + FUEL_MARGIN;
                tx.sign(k)
            })
            .collect()
    }

    fn tx(&self, k: &Keys, action: Action) -> Tx {
        self.txs(k, vec![action]).remove(0)
    }

    /// Executes `txs` as the next block; each one's fate.
    fn block(&mut self, txs: Vec<Tx>) -> Vec<Fate> {
        self.c.produce_block_with(txs.clone()).unwrap();
        let b = self.c.last_block().unwrap().clone();
        txs.iter()
            .map(|tx| match b.txs.iter().position(|t| t.id() == tx.id()) {
                Some(i) => match b.failed.get(&i) {
                    Some(why) => Fate::Failed(why.clone()),
                    None => Fate::Applied,
                },
                None => Fate::Skipped(self.c.failure(&tx.id()).map(|f| f.0.clone()).unwrap_or_default()),
            })
            .collect()
    }

    /// [`Dev::block`], every transaction applying.
    fn applied(&mut self, txs: Vec<Tx>) {
        for (i, fate) in self.block(txs).into_iter().enumerate() {
            assert_eq!(fate, Fate::Applied, "tx {i}");
        }
    }

    fn address(&self, user: u8) -> String {
        keys(user).address_with(&self.prefix())
    }

    fn balance(&self, address: &str) -> u64 {
        self.c.state.account(address).balance
    }

    /// The program `user`'s account creates with its transaction of `nonce` (§2.1).
    fn program(&self, user: u8, nonce: u64) -> String {
        digest::program_address(&self.prefix(), &crypto::address_bytes(&self.address(user)).unwrap(), nonce)
    }

    /// The version digest in force of `program`, as a call names it.
    fn version(&self, program: &str) -> String {
        let r = &self.c.state.programs.records[program];
        hex::encode(digest::version_digest(program, &r.wasm_hash, &r.interface_hash, r.version))
    }

    fn meta(&self) -> &super::super::state::ProgramsMeta {
        self.c.state.programs.meta.as_ref().unwrap()
    }

    /// `program`'s stored value under `key`.
    fn stored(&self, program: &str, key: &[u8]) -> Option<Vec<u8>> {
        let bytes = crypto::address_bytes(program).unwrap();
        self.c.state.programs.storage.get(&storage_key(&bytes, key)).map(|e| e.value.clone())
    }

    /// What `txs`, applied in order on a copy of the state at the next height, are charged (the
    /// fee phase's debit).
    fn charges(&self, txs: &[Tx]) -> u64 {
        let mut st = self.c.state.clone();
        let h = self.next();
        txs.iter()
            .map(|tx| {
                let charge = crate::econ::check_fee(&st, tx, h).unwrap();
                st.apply(tx, CHAIN, &self.c.me().unwrap(), h).unwrap();
                charge
            })
            .sum()
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.home).ok();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Fate {
    Applied,
    Failed(String),
    Skipped(String),
}

impl Fate {
    fn failed(&self) -> &str {
        match self {
            Fate::Failed(why) => why,
            other => panic!("expected a failure, got {other:?}"),
        }
    }
}

fn upload(wasm: Vec<u8>, iface: Vec<u8>, storage_limit: u64) -> Action {
    Action::UploadCode { wasm: HexBytes(wasm), interface: HexBytes(iface), storage_limit }
}

fn upload_guest(name: &str) -> Action {
    let (wasm, iface) = guest(name);
    upload(wasm, iface, 10 * FANG)
}

/// A create of stored `wasm` and `iface`, immutable and not taking transfers, with `init` (a method
/// name, its input and value) if given.
fn create(wasm: &[u8], iface: &[u8], init: Option<(&str, Vec<u8>, u64)>) -> Action {
    let fuel_limit = if init.is_some() { FUEL } else { 0 };
    Action::CreateProgram {
        wasm_hash: hex::encode(digest::wasm_hash(wasm)),
        interface_hash: hex::encode(digest::interface_hash(iface)),
        authority: None,
        upgrade_delay_blocks: 0,
        policy: ProgramPolicy::Open,
        accepts_transfers: false,
        init: init.map(|(method, input, value)| InitCall { method: method.into(), input: HexBytes(input), value }),
        fuel_limit,
        storage_limit: FANG,
        bindings: vec![],
    }
}

fn create_guest(name: &str, init: Option<(&str, Vec<u8>, u64)>) -> Action {
    let (wasm, iface) = guest(name);
    create(&wasm, &iface, init)
}

/// One call of a `CallProgram`, at the version `d` reads in force.
fn call(d: &Dev, program: &str, method: &str, input: Vec<u8>, value: u64) -> ProgramCall {
    ProgramCall { program: program.into(), version_digest: d.version(program), method: method.into(), input: HexBytes(input), value }
}

fn calls(calls: Vec<ProgramCall>, bindings: Vec<Binding>) -> Action {
    Action::CallProgram { calls, fuel_limit: FUEL, storage_limit: FANG, bindings }
}

fn bind(d: &Dev, program: &str) -> Binding {
    Binding { program: program.into(), version_digest: d.version(program) }
}

/// The router's `send` input: hops ‖ token ‖ to ‖ amount (W1's `route`).
fn route(token: &str, to: &str, amount: u64) -> Vec<u8> {
    [crypto::address_bytes(token).unwrap().to_vec(), crypto::address_bytes(to).unwrap().to_vec(), le(amount)].concat()
}

/// The token's key for `holder`'s balance (W1's token: `b` ‖ the holder's 32 bytes).
fn holding(holder: &str) -> Vec<u8> {
    [b"b".to_vec(), crypto::address_bytes(holder).unwrap().to_vec()].concat()
}

/// What a blob or record of `size` bytes bonds when it lands on a state of `used` bytes, at the
/// default parameters (§4.6), computed apart from the node's code.
fn priced(used: u64, size: u64) -> u64 {
    blockula_vm::storage::price(100_000, 64 << 20, used + size).unwrap() * size
}

/// An entry's deposit for a `key`-byte key and a `value`-byte value, at the default price (far
/// below the first step).
fn entry(key: usize, value: usize) -> u64 {
    100_000 * u64::from(blockula_vm::storage::capacity(key, value))
}

// ---- the four guests, end to end ----

/// WBS S4a's main line: the four guests uploaded in one block, created in the next (the token with
/// its `init`), called in the next: the counter bonds an entry, a router batch composes the token
/// (through a binding) with the escrow, a router call that reaches an unbound token traps and is
/// charged, and the escrow pays out. Deposits are bonded at the price once each blob, record and
/// entry lands, `value_held` follows the FANG programs hold, and nothing is sponsored.
#[test]
fn the_four_guests_upload_create_and_run_end_to_end() {
    let mut d = Dev::new("guests", |_| {});
    let (alice, bob, carol) = (keys(ALICE), d.address(BOB), d.address(CAROL));
    assert_eq!(status(&d.c.state, d.next()), Status::On);

    // Block 1: the uploads, each code then its interface priced once it lands.
    let ups = d.txs(&alice, GUESTS.map(upload_guest).to_vec());
    let charged = d.charges(&ups);
    let (before, started) = (d.balance(&alice.address()), counts());
    d.applied(ups);
    assert_eq!(counts().since(started), Counts { pass_threads: 1, tx_threads: 0, engines: 0, upload_checks: 4 }, "the block is one pass, and each new code's checks ran on its thread");
    let h1 = d.c.height();
    let mut used = 0;
    let mut bonded = 0;
    for g in GUESTS {
        let (wasm, iface) = guest(g);
        let (code, interface) = (priced(used, wasm.len() as u64), priced(used + wasm.len() as u64, iface.len() as u64));
        used += (wasm.len() + iface.len()) as u64;
        bonded += code + interface;
        let p = &d.c.state.programs;
        let (w, i) = (digest::wasm_hash(&wasm), digest::interface_hash(&iface));
        assert_eq!(p.code[&w].wasm, wasm, "{g}");
        assert_eq!(p.code[&w].exports, blockula_vm::validate::validate(&wasm).unwrap().exports, "{g}");
        assert_eq!(p.interfaces[&i].bytes, iface, "{g}");
        let (cr, ir) = (&p.blob_refs[&blob_key(BLOB_CODE, &w)], &p.blob_refs[&blob_key(BLOB_INTERFACE, &i)]);
        assert_eq!((cr.uploader.as_str(), cr.deposit, cr.size as usize, cr.refs, cr.uploaded_at, cr.last_used_at), (alice.address().as_str(), code, wasm.len(), 0, h1, h1), "{g}");
        assert_eq!((ir.deposit, ir.size as usize, ir.refs), (interface, iface.len(), 0), "{g}");
    }
    assert_eq!((d.meta().state_bytes, d.meta().escrow, d.meta().programs), (used, bonded, 0));
    assert_eq!(d.balance(&alice.address()), before - charged - bonded, "the fee and the deposits, from the uploader");

    // Block 2: the creates. The token's `init` gives the router (created in the same block, its
    // address known from alice's nonce) a supply of 1,000.
    let nonce = d.c.state.account(&alice.address()).nonce;
    let [counter, token, escrow, router] = [0, 1, 2, 3].map(|i| d.program(ALICE, nonce + i));
    let creates = d.txs(&alice, vec![create_guest("counter", None), create_guest("token", Some(("init", [crypto::address_bytes(&router).unwrap().to_vec(), le(1_000)].concat(), 0))), create_guest("escrow", None), create_guest("router", None)]);
    let (charged, before, burned) = (d.charges(&creates), d.balance(&alice.address()), d.c.state.burned);
    let started = counts();
    d.applied(creates);
    assert_eq!(counts().since(started), Counts { pass_threads: 1, tx_threads: 0, engines: 1, upload_checks: 0 }, "only the token's create runs code (its init), on the pass's one engine");
    let h2 = d.c.height();
    let mut records = 0;
    for (g, address) in GUESTS.iter().zip([&counter, &token, &escrow, &router]) {
        let r = &d.c.state.programs.records[address.as_str()];
        let (wasm, iface) = guest(g);
        let record = priced(used, RECORD_BYTES);
        used += RECORD_BYTES;
        records += record;
        assert_eq!((r.creator.as_str(), r.created_at, r.wasm_hash, r.interface_hash, r.version), (alice.address().as_str(), h2, digest::wasm_hash(&wasm), digest::interface_hash(&iface), 1), "{g}");
        assert_eq!((r.record_deposit, r.record_payer.as_str(), r.authority.clone(), r.accepts_transfers), (record, alice.address().as_str(), None, false), "{g}");
        assert_eq!(d.c.state.programs.blob_refs[&blob_key(BLOB_CODE, &r.wasm_hash)].refs, 1, "{g}");
        assert!(d.c.state.accounts.contains_key(address.as_str()), "{g}: its account");
    }
    // The token's init bonded two entries (the supply, the router's holding), paid by alice;
    // each new program account burned a creation fee from her.
    let init_bonds = entry(1, 8) + entry(33, 8);
    used += u64::from(blockula_vm::storage::capacity(1, 8)) + u64::from(blockula_vm::storage::capacity(33, 8));
    let fees = 4 * d.c.state.chain_params.account_creation_fee;
    assert_eq!((d.meta().programs, d.meta().state_bytes, d.meta().escrow), (4, used, bonded + records + init_bonds));
    assert_eq!(d.balance(&alice.address()), before - charged - records - init_bonds - fees);
    assert_eq!(d.c.state.burned - burned, charged + fees, "the fees' base parts and the creation fees burn (no tip)");
    assert_eq!((d.stored(&token, b"s"), d.stored(&token, &holding(&router))), (Some(le(1_000)), Some(le(1_000))));
    assert_eq!(d.c.state.programs.records[token.as_str()].items, 2);

    // Block 3: the calls.
    let increment = d.tx(&alice, calls(vec![call(&d, &counter, "increment", vec![], 0)], vec![]));
    // Bob's batch: the router sends carol 250 of its tokens (a binding lets it reach the token),
    // and the escrow takes 5 FANG for a deal with carol as the seller.
    let batch = d.tx(&keys(BOB), calls(vec![call(&d, &router, "send", route(&token, &carol, 250), 0), call(&d, &escrow, "open", [le(1), crypto::address_bytes(&carol).unwrap().to_vec()].concat(), 5 * FANG)], vec![bind(&d, &token)]));
    // Carol's router call, without the binding: the token is out of reach.
    let unbound = d.tx(&keys(CAROL), calls(vec![call(&d, &router, "send", route(&token, &carol, 1), 0)], vec![]));
    let (bob_before, carol_before) = (d.balance(&bob), d.balance(&carol));
    let (bob_charge, carol_charge) = (d.charges(std::slice::from_ref(&batch)), d.charges(std::slice::from_ref(&unbound)));
    let started = counts();
    let fates = d.block(vec![increment, batch, unbound]);
    assert_eq!(counts().since(started), Counts { pass_threads: 1, tx_threads: 0, engines: 1, upload_checks: 0 }, "one thread and one engine for the pass's three program transactions (the failed one too)");
    assert_eq!(&fates[..2], &[Fate::Applied, Fate::Applied]);
    assert!(fates[2].failed().starts_with(&format!("program {} trapped: unbound program (trap 15)", crate::chain::short(&router))), "{:?}", fates[2]);
    assert_eq!(d.stored(&counter, b"n"), Some(le(1)));
    assert_eq!((d.stored(&token, &holding(&router)), d.stored(&token, &holding(&carol))), (Some(le(750)), Some(le(250))));
    assert!(d.stored(&escrow, &[b"d".to_vec(), le(1)].concat()).is_some(), "the deal");
    // Bob paid the fee, the escrow's 5 FANG and the two entries his batch wrote (carol's holding,
    // two frames down, and the deal); carol only her fee.
    let deal = entry(9, 72);
    assert_eq!(d.balance(&bob), bob_before - bob_charge - 5 * FANG - entry(33, 8) - deal);
    assert_eq!(d.balance(&carol), carol_before - carol_charge, "a failed call charges its fee, nothing else");
    assert_eq!((d.balance(&escrow), d.meta().value_held, d.meta().sponsored), (5 * FANG, 5 * FANG, 0));
    assert_eq!(d.meta().escrow, bonded + records + init_bonds + entry(1, 8) + entry(33, 8) + deal);

    // Block 4: the buyer releases the deal: carol gets the 5 FANG, bob his deposit back. Carol
    // sends alice her 250 tokens: her emptied holding's deposit goes back to bob, who bonded it,
    // never to her (the remover), and she bonds alice's new holding.
    let (bob_before, carol_before) = (d.balance(&bob), d.balance(&carol));
    let release = d.tx(&keys(BOB), calls(vec![call(&d, &escrow, "release", le(1), 0)], vec![]));
    let empties = d.tx(&keys(CAROL), calls(vec![call(&d, &token, "transfer", [crypto::address_bytes(&alice.address()).unwrap().to_vec(), le(250)].concat(), 0)], vec![]));
    let (charge, carol_charge) = (d.charges(std::slice::from_ref(&release)), d.charges(std::slice::from_ref(&empties)));
    d.applied(vec![release, empties]);
    assert_eq!(d.balance(&carol), carol_before + 5 * FANG - carol_charge - entry(33, 8));
    assert_eq!(d.balance(&bob), bob_before - charge + deal + entry(33, 8));
    assert_eq!((d.stored(&token, &holding(&carol)), d.stored(&token, &holding(&alice.address()))), (None, Some(le(250))));
    assert_eq!((d.balance(&escrow), d.meta().value_held), (0, 0));
    assert_eq!(d.stored(&escrow, &[b"d".to_vec(), le(1)].concat()), None);
}

/// Uploads that fail (§2.2): a float, `memory.grow`, an interface that isn't the code's, and
/// deposits past `storage_limit`. Each is included and charged its fee, and stores nothing; an
/// upload of a pair already stored bonds nothing and moves only `last_used_at`.
#[test]
fn failed_uploads_are_charged_and_store_nothing() {
    let mut d = Dev::new("uploads", |_| {});
    let alice = keys(ALICE);
    let x = calls_only(&["x"], &[]);
    let (counter, counter_iface) = guest("counter");
    let (_, token_iface) = guest("token");
    let bad = d.txs(&alice, vec![upload(FLOAT_WASM.to_vec(), x.clone(), FANG), upload(GROW_WASM.to_vec(), x.clone(), FANG), upload(counter.clone(), token_iface, FANG), upload(counter.clone(), counter_iface.clone(), 1)]);
    let (charged, before) = (d.charges(&bad), d.balance(&alice.address()));
    let fates = d.block(bad);
    let reasons: Vec<&str> = fates.iter().map(Fate::failed).collect();
    assert_eq!(reasons[0], "invalid code: upload validation rule 4 (Float)");
    assert_eq!(reasons[1], "invalid code: upload validation rule 7 (MemoryGrow)");
    assert!(reasons[2].starts_with("the interface doesn't match the code: "), "{}", reasons[2]);
    assert_eq!(reasons[3], format!("deposits of {} drops pass the transaction's storage limit of 1", priced(0, counter.len() as u64) + priced(counter.len() as u64, counter_iface.len() as u64)));
    assert_eq!(d.balance(&alice.address()), before - charged, "only the fees");
    let p = &d.c.state.programs;
    assert!(p.code.is_empty() && p.interfaces.is_empty() && p.blob_refs.is_empty());
    assert_eq!((d.meta().state_bytes, d.meta().escrow), (0, 0));

    // The pair, stored; then uploaded again, by bob, a block later.
    d.applied(vec![d.tx(&alice, upload(counter.clone(), counter_iface.clone(), FANG))]);
    let (h, escrow) = (d.c.height(), d.meta().escrow);
    let again = d.tx(&keys(BOB), upload(counter.clone(), counter_iface.clone(), 0));
    let (charged, before) = (d.charges(std::slice::from_ref(&again)), d.balance(&d.address(BOB)));
    d.applied(vec![again]);
    let r = &d.c.state.programs.blob_refs[&blob_key(BLOB_CODE, &digest::wasm_hash(&counter))];
    assert_eq!((r.uploader.as_str(), r.uploaded_at, r.last_used_at), (d.address(ALICE).as_str(), h, h + 1));
    assert_eq!((d.meta().escrow, d.balance(&d.address(BOB))), (escrow, before - charged), "nothing bonded twice");
}

/// Creates (§2.3): an `init` that aborts rolls the whole create back (record, account, deposits)
/// and leaves a balance someone sent ahead to its address where it was, a plain account; the
/// creator's next create gets another address. A create at an address someone funded adopts the
/// account and its balance (no creation fee: the account exists).
#[test]
fn creates_adopt_a_funded_address_and_roll_back_a_failed_init() {
    let mut d = Dev::new("creates", |_| {});
    let (alice, bob) = (keys(ALICE), keys(BOB));
    d.applied(d.txs(&alice, vec![upload_guest("token"), upload_guest("counter")]));
    let nonce = d.c.state.account(&alice.address()).nonce;
    let (first, second, third) = (d.program(ALICE, nonce), d.program(ALICE, nonce + 1), d.program(ALICE, nonce + 2));
    // Bob sends FANG ahead to the addresses alice's next creates get.
    d.applied(d.txs(&bob, vec![Action::Transfer { to: first.clone(), amount: FANG }, Action::Transfer { to: third.clone(), amount: 2 * FANG }]));
    let root_programs = d.meta().clone();
    // The token's init takes 40 bytes (an owner and a supply): with 3 it aborts (code 1).
    let aborts = d.tx(&alice, create_guest("token", Some(("init", vec![1, 2, 3], 0))));
    let (charged, before) = (d.charges(std::slice::from_ref(&aborts)), d.balance(&alice.address()));
    let fates = d.block(vec![aborts]);
    assert_eq!(fates[0].failed(), format!("program {} aborted with code 1", crate::chain::short(&first)));
    assert_eq!(d.balance(&alice.address()), before - charged, "only the fee sticks");
    assert!(!d.c.state.programs.records.contains_key(&first));
    let sent_ahead = d.c.state.account(&first);
    assert_eq!((sent_ahead.balance, sent_ahead.nonce, sent_ahead.keys.is_empty()), (FANG, 0, true), "left where it was");
    let m = d.meta();
    assert_eq!((m.programs, m.state_bytes, m.escrow, m.value_held), (root_programs.programs, root_programs.state_bytes, root_programs.escrow, 0));
    assert_eq!(d.c.state.programs.blob_refs[&blob_key(BLOB_CODE, &digest::wasm_hash(&guest("token").0))].refs, 0);

    // Her next create, at nonce + 1, gets another address; the one after adopts bob's 2 FANG.
    let next = d.txs(&alice, vec![create_guest("token", Some(("init", [crypto::address_bytes(&alice.address()).unwrap().to_vec(), le(7)].concat(), 0))), create_guest("counter", None)]);
    let (charged, before, burned) = (d.charges(&next), d.balance(&alice.address()), d.c.state.burned);
    d.applied(next);
    assert!(d.c.state.programs.records.contains_key(&second) && !d.c.state.programs.records.contains_key(&first));
    assert_eq!(d.stored(&second, &holding(&alice.address())), Some(le(7)));
    assert!(d.c.state.programs.records.contains_key(&third), "adopted");
    assert_eq!((d.balance(&third), d.meta().value_held), (2 * FANG, 2 * FANG), "its balance joins value_held");
    let fee = d.c.state.chain_params.account_creation_fee;
    assert_eq!(d.c.state.burned - burned, charged + fee, "one creation fee: the token's account was new, the adopted one wasn't");
    let records: u64 = [&second, &third].iter().map(|p| d.c.state.programs.records[p.as_str()].record_deposit).sum();
    assert_eq!(d.balance(&alice.address()), before - charged - fee - records - entry(1, 8) - entry(33, 8));
}

/// A create at an address that holds keys, locks or a nonce fails (§2.3 step 1): here a vesting
/// lock, from a genesis allocation to the address the creator's first create gets. Admission
/// refuses it first, so the action phase runs on its own here.
#[test]
fn a_create_never_adopts_an_account_with_locks() {
    let mut d = Dev::new("locked", |g| {
        let target = digest::program_address(crypto::ADDRESS_HRP, &crypto::address_bytes(&keys(CAROL).address()).unwrap(), 1);
        g.allocations.push(Allocation { address: target, amount: FANG, vesting: Some(Vesting { start: 0, cliff: 1_000, end: 2_000 }), label: Some("vesting".into()) });
    });
    let carol = keys(CAROL);
    d.applied(vec![d.tx(&carol, upload_guest("counter"))]);
    let target = d.program(CAROL, 1);
    assert!(!d.c.state.account(&target).locks.is_empty());
    let tx = d.tx(&carol, create_guest("counter", None));
    assert_eq!(tx.nonce, 1);
    let refused = crate::programs::admit(&d.c.state, &tx, d.next(), ApplyMode::Block).unwrap_err();
    assert_eq!(refused.to_string(), format!("the address this create makes, {}, holds an account a program can't adopt (it has keys, locks or a nonce)", crate::chain::short(&target)));
    let mut st = d.c.state.clone();
    let mut j = Journal::default();
    let e = super::super::apply(&mut st, &mut j, &tx, None, &carol.address(), &key_id(&carol.public_key()), d.next()).unwrap_err();
    assert_eq!(e.to_string(), format!("{} has keys, locks or a nonce: a program can only adopt an account FANG was sent to", crate::chain::short(&target)));
}

/// A create names code and an interface that may come from different uploads (§2.3): the action
/// phase re-checks that they match (§6.1 step 1), so a pair that doesn't fails and no record
/// cites it. Admission refuses it first, so the action phase runs on its own here.
#[test]
fn a_create_of_code_and_an_interface_that_dont_match_fails() {
    let mut d = Dev::new("mismatch", |_| {});
    let alice = keys(ALICE);
    d.applied(d.txs(&alice, vec![upload_guest("counter"), upload_guest("token")]));
    let ((counter, _), (_, token_iface)) = (guest("counter"), guest("token"));
    let tx = d.tx(&alice, create(&counter, &token_iface, None));
    let refused = crate::programs::admit(&d.c.state, &tx, d.next(), ApplyMode::Block).unwrap_err().to_string();
    assert!(refused.starts_with("interface ") && refused.contains(" doesn't match code "), "{refused}");
    let mut st = d.c.state.clone();
    let e = super::super::apply(&mut st, &mut Journal::default(), &tx, None, &alice.address(), &key_id(&alice.public_key()), d.next()).unwrap_err();
    assert!(e.to_string().starts_with("the interface doesn't match the code: "), "{e}");
    assert!(st.programs.records.is_empty() && st.programs.fault.is_none());
}

/// A5 for blobs, and `init` (§2.3): a create may use code and an interface from the block after
/// their upload, and carries `init` exactly when the interface declares an initializer. Admission
/// refuses a create that breaks either, uncharged; the create handler re-checks both in admission's
/// words (§6.1 step 1), so on a path that skips admission (the action phase on its own) such a
/// create fails and leaves no record. An upload and a create of its code in one block: on a copy
/// where the upload just applied, the handler fails the create at that height; through the chain
/// the upload applies and the create is left out; a block later the create applies.
#[test]
fn a_create_is_rechecked_for_its_blobs_age_and_its_init() {
    let mut d = Dev::new("recheck", |_| {});
    let alice = keys(ALICE);
    let (prefix, sink, signer) = (d.prefix(), d.c.me().unwrap(), key_id(&alice.public_key()));
    // `tx` on `st` at `h`: admission's refusal, and the handler's failure, which leaves no record
    // at the address the create makes.
    let both = |st: &State, tx: &Tx, h: u64| {
        let refused = crate::programs::admit(st, tx, h, ApplyMode::Block).unwrap_err().to_string();
        let mut st = st.clone();
        let failed = super::super::apply(&mut st, &mut Journal::default(), tx, None, &tx.from, &signer, h).unwrap_err().to_string();
        assert!(!st.programs.records.contains_key(&digest::program_address(&prefix, &crypto::address_bytes(&tx.from).unwrap(), tx.nonce)));
        (refused, failed)
    };
    let (counter, _) = guest("counter");
    let h = d.next();
    let txs = d.txs(&alice, vec![upload_guest("counter"), create_guest("counter", None)]);
    let program = d.program(ALICE, txs[1].nonce);
    let too_soon = format!("code {}… was uploaded at block {h}: it's usable from block {}", &hex::encode(digest::wasm_hash(&counter))[..10], h + 1);
    let mut st = d.c.state.clone();
    assert_eq!(st.apply(&txs[0], CHAIN, &sink, h).unwrap(), Outcome::Applied);
    assert_eq!(both(&st, &txs[1], h), (too_soon.clone(), too_soon.clone()));
    // Through the chain: the create is left out, uncharged (its nonce unused).
    assert_eq!(d.block(txs.clone()), vec![Fate::Applied, Fate::Skipped(format!("not included: {too_soon}"))]);
    assert_eq!(d.c.state.account(&alice.address()).nonce, txs[1].nonce);
    // A block later it applies, at the same nonce, so the same address; the token's blobs land
    // beside it.
    d.applied(d.txs(&alice, vec![create_guest("counter", None), upload_guest("token")]));
    assert!(d.c.state.programs.records.contains_key(&program));
    // The token's interface declares an initializer; the counter's doesn't.
    let h = d.next();
    let must = "the interface declares an initializer, so the create must run it (`init`)".to_string();
    assert_eq!(both(&d.c.state, &d.tx(&alice, create_guest("token", None)), h), (must.clone(), must));
    let cant = "the interface declares no initializer, so the create can't carry `init`".to_string();
    assert_eq!(both(&d.c.state, &d.tx(&alice, create_guest("counter", Some(("init", vec![], 0)))), h), (cant.clone(), cant));
}

/// The value cap (§7.1): value sent in past the cap fails and rolls back; a sponsored write at the
/// cap applies (neutral); an adoption past the cap fails and leaves the sent-ahead balance alone.
#[test]
fn the_value_cap_refuses_calls_and_adoption_but_allows_sponsored_writes() {
    let mut d = Dev::new("cap", |_| {});
    let (alice, bob) = (keys(ALICE), keys(BOB));
    let iface = calls_only(&["drop", "fund", "put"], &["fund"]);
    d.applied(vec![d.tx(&alice, upload(SPONSOR_WASM.to_vec(), iface.clone(), FANG))]);
    let program = d.program(ALICE, d.c.state.account(&alice.address()).nonce);
    d.applied(vec![d.tx(&alice, create(SPONSOR_WASM, &iface, None))]);
    d.applied(vec![d.tx(&alice, calls(vec![call(&d, &program, "fund", vec![], 2 * FANG)], vec![]))]);
    assert_eq!(d.meta().value_held, 2 * FANG);
    // Bob sends FANG ahead to the address alice's next create gets.
    let next_program = d.program(ALICE, d.c.state.account(&alice.address()).nonce);
    d.applied(vec![d.tx(&bob, Action::Transfer { to: next_program.clone(), amount: FANG })]);
    let (h, sink) = (d.next(), d.c.me().unwrap());
    let capped = |cap: u64| {
        let mut st = d.c.state.clone();
        st.programs.meta.as_mut().unwrap().params.insert(Param::ValueCap.key().into(), cap);
        st
    };
    // a) value past the cap fails, charged its fee only.
    let mut st = capped(2 * FANG + 1);
    let fund = d.tx(&alice, calls(vec![call(&d, &program, "fund", vec![], 2)], vec![]));
    let charge = crate::econ::check_fee(&st, &fund, h).unwrap();
    let before = st.account(&alice.address()).balance;
    let out = st.apply(&fund, CHAIN, &sink, h).unwrap();
    assert!(matches!(&out, Outcome::Failed(why) if why.contains("past the value cap")), "{out:?}");
    assert_eq!((st.account(&program).balance, st.programs.meta.as_ref().unwrap().value_held, st.account(&alice.address()).balance), (2 * FANG, 2 * FANG, before - charge));
    st.check_invariants().unwrap();
    // b) at the cap exactly, a sponsored write applies (the control doesn't move).
    let mut st = capped(2 * FANG);
    let put = d.tx(&alice, calls(vec![call(&d, &program, "put", vec![], 0)], vec![]));
    assert_eq!(st.apply(&put, CHAIN, &sink, h).unwrap(), Outcome::Applied);
    let m = st.programs.meta.as_ref().unwrap();
    assert_eq!(m.value_held + m.sponsored, 2 * FANG);
    st.check_invariants().unwrap();
    // c) an adoption past the cap fails; the balance sent ahead stays where it was.
    let mut st = capped(2 * FANG);
    let adopt = d.tx(&alice, create(SPONSOR_WASM, &iface, None));
    let charge = crate::econ::check_fee(&st, &adopt, h).unwrap();
    let before = st.account(&alice.address()).balance;
    let meta_before = st.programs.meta.clone();
    let refs_before = st.programs.blob_refs.clone();
    let out = st.apply(&adopt, CHAIN, &sink, h).unwrap();
    assert!(matches!(&out, Outcome::Failed(why) if why.contains("past the value cap")), "{out:?}");
    assert!(!st.programs.records.contains_key(&next_program));
    assert_eq!(st.account(&alice.address()).balance, before - charge, "only the fee sticks");
    assert_eq!(st.programs.meta, meta_before, "record deposits and totals roll back");
    assert_eq!(st.programs.blob_refs.iter().collect::<Vec<_>>(), refs_before.iter().collect::<Vec<_>>(), "blob references roll back");
    assert_eq!((st.account(&next_program).balance, st.programs.meta.as_ref().unwrap().value_held), (FANG, 2 * FANG));
    st.check_invariants().unwrap();
    // d) with room, the same adoption applies and joins value_held.
    let mut st = capped(3 * FANG);
    assert_eq!(st.apply(&adopt, CHAIN, &sink, h).unwrap(), Outcome::Applied);
    assert_eq!(st.programs.meta.as_ref().unwrap().value_held, 3 * FANG);
    st.check_invariants().unwrap();
}

/// `SPONSOR` (§6.1 step 6.1, §7.1): a program that bonds its own balance into an entry moves FANG
/// from `value_held` to `sponsored`, and the entry's removal moves it back; the FANG programs
/// control (their sum) stays where it was. Value sent in raises it.
#[test]
fn sponsored_deposits_move_between_value_held_and_sponsored() {
    let mut d = Dev::new("sponsor", |_| {});
    let alice = keys(ALICE);
    let iface = calls_only(&["drop", "fund", "put"], &["fund"]);
    d.applied(vec![d.tx(&alice, upload(SPONSOR_WASM.to_vec(), iface.clone(), FANG))]);
    let program = d.program(ALICE, d.c.state.account(&alice.address()).nonce);
    d.applied(vec![d.tx(&alice, create(SPONSOR_WASM, &iface, None))]);
    let control = |d: &Dev| (d.meta().value_held, d.meta().sponsored, d.c.state.programs.records[program.as_str()].sponsored);
    d.applied(vec![d.tx(&alice, calls(vec![call(&d, &program, "fund", vec![], FANG)], vec![]))]);
    assert_eq!(control(&d), (FANG, 0, 0), "value in raises the control");
    let alice_before = d.balance(&alice.address());
    let put = d.tx(&alice, calls(vec![call(&d, &program, "put", vec![], 0)], vec![]));
    let charge = d.charges(std::slice::from_ref(&put));
    d.applied(vec![put]);
    let deposit = entry(1, 1);
    assert_eq!(control(&d), (FANG - deposit, deposit, deposit), "the program paid its own deposit");
    assert_eq!((d.balance(&program), d.balance(&alice.address())), (FANG - deposit, alice_before - charge), "the signer bonded nothing");
    let e = d.c.state.programs.storage.get(&storage_key(&crypto::address_bytes(&program).unwrap(), b"s")).unwrap().clone();
    assert_eq!((e.payer, e.deposit), (crypto::address_bytes(&program).unwrap(), deposit));
    d.applied(vec![d.tx(&alice, calls(vec![call(&d, &program, "drop", vec![], 0)], vec![]))]);
    assert_eq!(control(&d), (FANG, 0, 0), "its removal refunds the program");
    assert_eq!(d.balance(&program), FANG);
}

// ---- the training wheels at commit (WBS S5's acceptance) ----

/// The sponsoring program (`SPONSOR_WASM`) created by alice, accepting plain transfers, and funded
/// with 2 FANG through its payable `fund`; then a copy of the state, for the next block's height,
/// with `program_value_cap` at `cap`. Returns the program and that state.
fn sponsor_funded(d: &mut Dev, cap: u64) -> (String, State) {
    let alice = keys(ALICE);
    let iface = calls_only(&["drop", "fund", "put"], &["fund"]);
    d.applied(vec![d.tx(&alice, upload(SPONSOR_WASM.to_vec(), iface.clone(), FANG))]);
    let program = d.program(ALICE, d.c.state.account(&alice.address()).nonce);
    let mut open = create(SPONSOR_WASM, &iface, None);
    if let Action::CreateProgram { accepts_transfers, .. } = &mut open {
        *accepts_transfers = true;
    }
    d.applied(vec![d.tx(&alice, open)]);
    d.applied(vec![d.tx(&alice, calls(vec![call(d, &program, "fund", vec![], 2 * FANG)], vec![]))]);
    let mut st = d.c.state.clone();
    st.programs.meta.as_mut().unwrap().params.insert(Param::ValueCap.key().into(), cap);
    (program, st)
}

/// The cap on the control through a call's commit and a plain transfer (§7.1, A40): a program at
/// the cap that bonds its balance into an entry it sponsors (which leaves the control where it was,
/// so it applies at the cap) can't then take in more, neither as a call's value nor as a transfer,
/// though its `value_held` is under the cap; and the entry's removal at the cap, which refunds it,
/// applies.
#[test]
fn a_program_at_the_cap_that_parks_its_balance_takes_in_no_more() {
    let mut d = Dev::new("parked", |_| {});
    let (program, mut st) = sponsor_funded(&mut d, 2 * FANG);
    let (h, sink, alice) = (d.next(), d.c.me().unwrap(), keys(ALICE));
    let held = |st: &State| {
        let m = st.programs.meta.as_ref().unwrap();
        (m.value_held, m.sponsored)
    };
    let deposit = entry(1, 1);
    let txs = d.txs(&alice, vec![
        calls(vec![call(&d, &program, "put", vec![], 0)], vec![]),
        calls(vec![call(&d, &program, "fund", vec![], 1)], vec![]),
        Action::Transfer { to: program.clone(), amount: 1 },
        calls(vec![call(&d, &program, "drop", vec![], 0)], vec![]),
    ]);
    assert_eq!(st.apply(&txs[0], CHAIN, &sink, h).unwrap(), Outcome::Applied, "a sponsored write at the cap");
    assert_eq!(held(&st), (2 * FANG - deposit, deposit), "the control at the cap, its value_held under it");
    let fund = st.apply(&txs[1], CHAIN, &sink, h).unwrap();
    assert!(matches!(&fund, Outcome::Failed(why) if why.contains("past the value cap")), "{fund:?}");
    let past = format!("programs may control at most {} (program_value_cap), and this would take them to {}", fmt_fang(2 * FANG), fmt_fang(2 * FANG + 1));
    assert_eq!(st.apply(&txs[2], CHAIN, &sink, h).unwrap(), Outcome::Failed(past));
    assert_eq!((held(&st), st.account(&program).balance), ((2 * FANG - deposit, deposit), 2 * FANG - deposit), "nothing came in");
    assert_eq!(st.apply(&txs[3], CHAIN, &sink, h).unwrap(), Outcome::Applied, "the entry's removal at the cap");
    assert_eq!((held(&st), st.account(&program).balance), ((2 * FANG, 0), 2 * FANG));
    st.check_invariants().unwrap();
}

/// The cap on the control for an adoption (§7.1, A40): with programs at the cap once a program
/// bonds its balance into an entry it sponsors, a create can't adopt FANG sent ahead to its
/// address, though `value_held` alone would have room for it (the entry's deposit is more than the
/// least a payment may open an account with, and that's what was sent); the balance stays where it
/// was. The payment that sent it ahead, to an account that isn't a program yet, applies at the cap.
#[test]
fn an_adoption_at_the_cap_is_measured_on_the_control() {
    let mut d = Dev::new("parked-adopt", |_| {});
    let (program, mut st) = sponsor_funded(&mut d, 2 * FANG);
    let (h, sink, alice) = (d.next(), d.c.me().unwrap(), keys(ALICE));
    let deposit = entry(1, 1);
    assert!(deposit > MIN_NEW_ACCOUNT, "value_held alone has room for the adoption");
    let iface = calls_only(&["drop", "fund", "put"], &["fund"]);
    let adoptee = d.program(ALICE, d.c.state.account(&alice.address()).nonce + 1);
    let txs = d.txs(&alice, vec![calls(vec![call(&d, &program, "put", vec![], 0)], vec![]), create(SPONSOR_WASM, &iface, None)]);
    let ahead = d.tx(&keys(BOB), Action::Transfer { to: adoptee.clone(), amount: MIN_NEW_ACCOUNT });
    assert_eq!(st.apply(&txs[0], CHAIN, &sink, h).unwrap(), Outcome::Applied, "a sponsored write at the cap");
    assert_eq!(st.apply(&ahead, CHAIN, &sink, h).unwrap(), Outcome::Applied, "a payment ahead, to an account that isn't a program yet");
    let meta_before = st.programs.meta.clone();
    let adopt = st.apply(&txs[1], CHAIN, &sink, h).unwrap();
    assert!(matches!(&adopt, Outcome::Failed(why) if why.contains("past the value cap")), "{adopt:?}");
    assert!(!st.programs.records.contains_key(&adoptee));
    assert_eq!((st.account(&adoptee).balance, &st.programs.meta), (MIN_NEW_ACCOUNT, &meta_before), "nothing adopted");
    st.check_invariants().unwrap();
}

/// Exit-only's measure is the control, not `value_held` (§7.3, A40): in exit-only a program that
/// bonds its balance into an entry it sponsors, and the entry's removal (which refunds it, raising
/// `value_held` by as much as `sponsored` falls), each leave the control where it was, so both
/// apply. A commit that raised the control would fail (`check_control`'s exit-only arm), though
/// no v0 call reaches it: admission refuses a call carrying value first, and nothing else raises it.
#[test]
fn exit_only_measures_the_control_not_value_held() {
    let mut d = Dev::new("exit-measure", |_| {});
    let (program, mut st) = sponsor_funded(&mut d, 10_000_000 * FANG);
    let (h, sink, alice) = (d.next(), d.c.me().unwrap(), keys(ALICE));
    st.upgrade.features.remove(PROGRAMS_FEATURE);
    assert_eq!(status(&st, h), Status::ExitOnly);
    let deposit = entry(1, 1);
    let txs = d.txs(&alice, vec![calls(vec![call(&d, &program, "put", vec![], 0)], vec![]), calls(vec![call(&d, &program, "drop", vec![], 0)], vec![])]);
    assert_eq!(st.apply(&txs[0], CHAIN, &sink, h).unwrap(), Outcome::Applied);
    let m = st.programs.meta.clone().unwrap();
    assert_eq!((m.value_held, m.sponsored), (2 * FANG - deposit, deposit));
    assert_eq!(st.apply(&txs[1], CHAIN, &sink, h).unwrap(), Outcome::Applied, "value_held rises, the control doesn't");
    let m = st.programs.meta.clone().unwrap();
    assert_eq!((m.value_held, m.sponsored), (2 * FANG, 0));
    st.check_invariants().unwrap();

    let control = super::super::commit::control(&st);
    let prefix = d.prefix();
    assert_eq!(super::super::commit::check_control(&st, control - 1, h).map_err(|f| f.reason(&prefix)), Err("programs are exit-only: a call may not raise the FANG programs control".to_string()));
    assert_eq!(super::super::commit::check_control(&st, control, h), Ok(()), "a commit that doesn't raise it");
    assert_eq!(super::super::commit::check_control(&d.c.state, control - 1, h), Ok(()), "while programs are on, a rise under the cap");
}

/// Exit-only lets programs pay each other (§7.3: what moves FANG between programs runs): a call in
/// which one program pays another's account leaves the control where it was, so it applies. Two
/// escrows: the first holds a deal for bob (so its account exists), the second one sold to the
/// first, released once programs are switched off.
#[test]
fn exit_only_lets_a_program_pay_another() {
    let mut d = Dev::new("exit-between", |_| {});
    let alice = keys(ALICE);
    d.applied(vec![d.tx(&alice, upload_guest("escrow"))]);
    let nonce = d.c.state.account(&alice.address()).nonce;
    let (first, second) = (d.program(ALICE, nonce), d.program(ALICE, nonce + 1));
    d.applied(d.txs(&alice, vec![create_guest("escrow", None), create_guest("escrow", None)]));
    let open = |d: &Dev, escrow: &str, seller: &str, value: u64| call(d, escrow, "open", [le(1), crypto::address_bytes(seller).unwrap().to_vec()].concat(), value);
    d.applied(vec![d.tx(&alice, calls(vec![open(&d, &first, &d.address(BOB), FANG), open(&d, &second, &first, 2 * FANG)], vec![]))]);
    assert_eq!((d.balance(&first), d.balance(&second), d.meta().value_held), (FANG, 2 * FANG, 3 * FANG));
    let (h, sink) = (d.next(), d.c.me().unwrap());
    let mut st = d.c.state.clone();
    st.upgrade.features.remove(PROGRAMS_FEATURE);
    assert_eq!(status(&st, h), Status::ExitOnly);
    let release = d.tx(&alice, calls(vec![call(&d, &second, "release", le(1), 0)], vec![]));
    assert_eq!(st.apply(&release, CHAIN, &sink, h).unwrap(), Outcome::Applied, "a program pays another");
    let m = st.programs.meta.as_ref().unwrap();
    assert_eq!((st.account(&first).balance, st.account(&second).balance, m.value_held, m.sponsored), (3 * FANG, 0, 3 * FANG, 0), "the control where it was");
    st.check_invariants().unwrap();
}

/// Accounts a program pays (§6.1 step 6.3, A30). The escrow releases a deal to a fresh account:
/// the payment opens it, the creation fee comes out of the deal (the escrow pays with
/// `OPEN_DEDUCT`) and burns once, and the signer isn't billed another (`econ::charge_new_accounts`
/// skips `j.programs.opened`). It releases a deal of 1 drop to a keyless system account that has
/// no record yet: the view reads that account as existing, so the payment opens nothing and burns
/// nothing (a new account would refuse 1 drop, and the release would abort).
#[test]
fn programs_open_accounts_once_and_pay_system_accounts_as_existing() {
    let mut d = Dev::new("opened", |_| {});
    let alice = keys(ALICE);
    d.applied(vec![d.tx(&alice, upload_guest("escrow"))]);
    let escrow = d.program(ALICE, d.c.state.account(&alice.address()).nonce);
    d.applied(vec![d.tx(&alice, create_guest("escrow", None))]);
    let prefix = d.prefix();
    let system = [crate::chain::treasury(&prefix), crate::chain::community(&prefix), crate::chain::endowment(&prefix), crate::chain::bootstrap_delegation(&prefix), crate::poc::pool_address(&prefix)]
        .into_iter()
        .find(|a| !d.c.state.accounts.contains_key(a))
        .expect("a dev chain has a system account without a record");
    let fresh = keys(75).address_with(&prefix);
    assert!(!d.c.state.accounts.contains_key(&fresh));
    let open = |id: u64, seller: &str, value: u64| call(&d, &escrow, "open", [le(id), crypto::address_bytes(seller).unwrap().to_vec()].concat(), value);
    d.applied(vec![d.tx(&alice, calls(vec![open(1, &fresh, 2 * FANG), open(2, &system, 1)], vec![]))]);

    let release = d.tx(&alice, calls(vec![call(&d, &escrow, "release", le(1), 0), call(&d, &escrow, "release", le(2), 0)], vec![]));
    let (charge, before, burned) = (d.charges(std::slice::from_ref(&release)), d.balance(&alice.address()), d.c.state.burned);
    d.applied(vec![release]);
    let fee = d.c.state.chain_params.account_creation_fee;
    assert_eq!((d.balance(&fresh), d.balance(&system)), (2 * FANG - fee, 1));
    assert_eq!(d.c.state.burned - burned, charge + fee, "one creation fee, out of the deal");
    assert_eq!(d.balance(&alice.address()), before - charge + 2 * entry(9, 72), "the fee, less the two deals' deposits back");
    assert_eq!((d.balance(&escrow), d.meta().value_held), (0, 0));
}

/// Exit-only (§7.3): once governance switches programs off, a call that pays out (the escrow's
/// release) and one whose signer bonds a new record (a token transfer to a new holder) apply; a call
/// carrying value, an upload and a create of stored code are each refused by admission, uncharged —
/// the valued one from its signer, the upload and the create from two others.
#[test]
fn exit_only_lets_a_signer_bonded_withdrawal_through() {
    let mut d = Dev::new("exit", |_| {});
    let (alice, bob) = (keys(ALICE), keys(BOB));
    let validator = d.c.keys.as_ref().unwrap().clone_keys();
    d.applied(d.txs(&alice, vec![upload_guest("token"), upload_guest("escrow")]));
    let nonce = d.c.state.account(&alice.address()).nonce;
    let (token, escrow) = (d.program(ALICE, nonce), d.program(ALICE, nonce + 1));
    d.applied(d.txs(&alice, vec![create_guest("token", Some(("init", [crypto::address_bytes(&alice.address()).unwrap().to_vec(), le(100)].concat(), 0))), create_guest("escrow", None)]));
    d.applied(vec![d.tx(&alice, calls(vec![call(&d, &escrow, "open", [le(1), crypto::address_bytes(&bob.address()).unwrap().to_vec()].concat(), 3 * FANG)], vec![]))]);
    assert_eq!(d.meta().value_held, 3 * FANG);

    // The council (this chain's one validator) switches programs off.
    let id = d.c.state.governance.next_proposal_id;
    d.applied(vec![d.tx(&alice, Action::Propose { kind: ProposalKind::DeactivateFeature { feature: PROGRAMS_FEATURE.into() }, deposit: 10 * FANG, title: "programs off".into(), url: String::new() })]);
    let digest_ = crate::governance::digest_of(&d.c.state.governance.proposals[&id]);
    d.applied(vec![d.tx(&validator, Action::Vote { proposal_id: id, aye: true, amount: FANG, conviction: 0, proposal_digest: digest_ })]);
    while status(&d.c.state, d.next()) != Status::ExitOnly {
        assert!(d.c.height() < 40, "the proposal never ran: {}", d.c.state.governance.proposals[&id].outcome);
        d.applied(vec![]);
    }

    // The buyer releases the deal (a payout), then sends dave tokens: dave holds none yet, so
    // alice bonds his entry, a deposit refundable only to her.
    let (dave, escrow_balance, bob_before) = (keys(74).address(), d.balance(&escrow), d.balance(&bob.address()));
    d.applied(vec![d.tx(&alice, calls(vec![call(&d, &escrow, "release", le(1), 0)], vec![]))]);
    d.applied(vec![d.tx(&alice, calls(vec![call(&d, &token, "transfer", [crypto::address_bytes(&dave).unwrap().to_vec(), le(10)].concat(), 0)], vec![]))]);
    assert_eq!((d.balance(&bob.address()), d.balance(&escrow), d.meta().value_held), (bob_before + escrow_balance, 0, 0), "the payout");
    assert_eq!(d.stored(&token, &holding(&dave)), Some(le(10)), "the signer-bonded write");

    let refused = d.txs(&alice, vec![calls(vec![call(&d, &escrow, "open", [le(2), crypto::address_bytes(&bob.address()).unwrap().to_vec()].concat(), 1)], vec![])]);
    // The escrow's code was uploaded at the first block, so a create of it by a third signer (carol)
    // reaches exit-only as its reason: admission refuses a create, uncharged.
    let carol = keys(CAROL);
    let others = vec![d.tx(&bob, upload_guest("counter")), d.tx(&carol, create_guest("escrow", None))];
    let before = (d.balance(&alice.address()), d.balance(&bob.address()), d.balance(&carol.address()));
    for fate in d.block([refused, others].concat()) {
        assert!(matches!(fate, Fate::Skipped(ref why) if why.contains("exit-only")), "{fate:?}");
    }
    assert_eq!((d.balance(&alice.address()), d.balance(&bob.address()), d.balance(&carol.address())), before, "nothing charged");
}

// ---- deposits, priced once they land ----

/// §4.6, A34: an upload's blobs (code, then interface), a create's record and an entry are each
/// priced at program state's bytes once it lands, so a blob, a record or an entry that crosses a
/// step of the curve pays the higher price. On a state machine with program state just under the
/// first step (an eighth of `program_state_soft_bytes`), the handlers run directly: the uploads at
/// one height, a create in the block after (A5), its calls in the one after that.
#[test]
fn deposits_are_priced_once_they_land() {
    let d = Dev::new("pricing", |_| {});
    let alice = keys(ALICE);
    let soft = 16u64 << 20;
    let step = soft / 8;
    let base = Param::StoragePrice.default(&d.c.state.params);
    let fresh = |below: u64| {
        let mut st = d.c.state.clone();
        let m = st.programs.meta.as_mut().unwrap();
        m.params.insert(Param::StateSoftBytes.key().into(), soft);
        m.state_bytes = step - below;
        st
    };
    let h = d.next();
    let (wasm, iface) = guest("counter");
    let run = |st: &mut State, tx: &Tx, h: u64| {
        let mut j = Journal::default();
        super::super::apply(st, &mut j, tx, None, &alice.address(), &key_id(&alice.public_key()), h).unwrap();
    };
    // The code crosses the step: both blobs at twice the price.
    let mut st = fresh(10);
    let up = d.tx(&alice, upload(wasm.clone(), iface.clone(), FANG));
    run(&mut st, &up, h);
    let refs = &st.programs.blob_refs;
    assert_eq!((refs[&blob_key(BLOB_CODE, &digest::wasm_hash(&wasm))].deposit, refs[&blob_key(BLOB_INTERFACE, &digest::interface_hash(&iface))].deposit), (2 * base * wasm.len() as u64, 2 * base * iface.len() as u64));
    // The code stays below; the interface, landing on top of it, crosses.
    let mut st = fresh(wasm.len() as u64 + 1);
    run(&mut st, &up, h);
    let refs = &st.programs.blob_refs;
    assert_eq!((refs[&blob_key(BLOB_CODE, &digest::wasm_hash(&wasm))].deposit, refs[&blob_key(BLOB_INTERFACE, &digest::interface_hash(&iface))].deposit), (base * wasm.len() as u64, 2 * base * iface.len() as u64));
    // A record of 256 bytes, landing across the step (the blobs stored first, just below it).
    let mut st = fresh(wasm.len() as u64 + iface.len() as u64 + 100);
    run(&mut st, &up, h);
    let program = digest::program_address(&d.prefix(), &crypto::address_bytes(&alice.address()).unwrap(), up.nonce + 1);
    let create = Tx { nonce: up.nonce + 1, ..d.tx(&alice, create_guest("counter", None)) };
    run(&mut st, &create, h + 1);
    assert_eq!(st.programs.records[&program].record_deposit, 2 * base * RECORD_BYTES);
    // An entry (128 bytes for the counter's) that crosses it, in the VM's overlay; one below it.
    for (below, price) in [(100, 2 * base), (129, base)] {
        let mut st = st.clone();
        st.programs.meta.as_mut().unwrap().state_bytes = step - below;
        let version = {
            let r = &st.programs.records[&program];
            hex::encode(digest::version_digest(&program, &r.wasm_hash, &r.interface_hash, r.version))
        };
        let increment = Tx { nonce: up.nonce + 2, ..d.tx(&alice, Action::CallProgram { calls: vec![ProgramCall { program: program.clone(), version_digest: version, method: "increment".into(), input: HexBytes(vec![]), value: 0 }], fuel_limit: FUEL, storage_limit: FANG, bindings: vec![] }) };
        run(&mut st, &increment, h + 2);
        let e = st.programs.storage.get(&storage_key(&crypto::address_bytes(&program).unwrap(), b"n")).unwrap();
        assert_eq!(e.deposit, price * u64::from(e.capacity), "{below} bytes below the step");
    }
}

// ---- the node's side of the VM ----

/// The mempool never runs the VM (§6.4): it takes program transactions in `ApplyMode::Mempool`
/// (its dry run, and its simulation of a sender's queue, each debiting the worst case instead:
/// `programs::mempool_apply`) and re-checks them after a block (`MempoolCheck::run`) without
/// starting a program thread, so no engine; executing the block that carries them starts one pass
/// thread, with one engine for its calls.
#[test]
fn the_mempool_starts_no_engine() {
    let mut d = Dev::new("mempool", |_| {});
    let alice = keys(ALICE);
    d.applied(vec![d.tx(&alice, upload_guest("counter"))]);
    let counter = d.program(ALICE, d.c.state.account(&alice.address()).nonce);
    d.applied(vec![d.tx(&alice, create_guest("counter", None))]);
    let started = counts();
    // A transfer first, so the program transactions behind it are judged on alice's simulated
    // queue (`simulate_queue`), each by the mempool's dry run. The mempool takes all three.
    let txs = d.txs(&alice, vec![Action::Transfer { to: d.address(BOB), amount: FANG }, calls(vec![call(&d, &counter, "increment", vec![], 0)], vec![]), upload_guest("token")]);
    for tx in &txs {
        d.c.submit(tx.clone()).unwrap();
    }
    // The mempool's re-check after a block, run directly and by an empty block's execution: it
    // keeps all three.
    assert_eq!(d.c.mempool_check().run().1, vec![]);
    d.applied(vec![]);
    assert!(txs.iter().all(|t| d.c.in_mempool(&t.id())), "the mempool keeps them");
    assert_eq!(counts(), started, "no thread and no engine in the mempool's paths");
    // The block that carries them runs each program transaction, on its one pass thread.
    d.c.produce_block().unwrap();
    let b = d.c.last_block().unwrap();
    assert_eq!((b.txs.len(), b.failed.len()), (3, 0));
    assert_eq!(counts().since(started), Counts { pass_threads: 1, tx_threads: 0, engines: 1, upload_checks: 1 }, "a block's execution does start them");
    assert_eq!(d.stored(&counter, b"n"), Some(le(1)));
}

/// Determinism: the same block of program transactions, on two copies of a state, reaches the
/// same outcomes and the same root, which is the one the chain commits for it.
#[test]
fn the_same_block_reaches_the_same_root_on_two_states() {
    let mut d = Dev::new("determinism", |_| {});
    let (alice, bob) = (keys(ALICE), keys(BOB));
    d.applied(d.txs(&alice, vec![upload_guest("token"), upload_guest("router")]));
    let nonce = d.c.state.account(&alice.address()).nonce;
    let (token, router) = (d.program(ALICE, nonce), d.program(ALICE, nonce + 1));
    d.applied(d.txs(&alice, vec![create_guest("token", Some(("init", [crypto::address_bytes(&router).unwrap().to_vec(), le(500)].concat(), 0))), create_guest("router", None)]));
    let block = vec![
        d.tx(&bob, calls(vec![call(&d, &router, "send", route(&token, &d.address(CAROL), 5), 0), call(&d, &router, "try_send", route(&token, &d.address(CAROL), 10_000), 0)], vec![bind(&d, &token)])),
        d.tx(&alice, upload_guest("counter")),
        // Carol holds 5 tokens by then: a transfer of 100 aborts.
        d.tx(&keys(CAROL), calls(vec![call(&d, &token, "transfer", [crypto::address_bytes(&d.address(BOB)).unwrap().to_vec(), le(100)].concat(), 0)], vec![])),
    ];
    let h = d.next();
    let run = |mut st: State| {
        let mut outcomes = vec![];
        st.run_block(CHAIN, h, "block", &d.c.me().unwrap(), h, h - 1, &block, |_, _, r| outcomes.push(r.map_err(|e| e.to_string())));
        st.commit_block().unwrap();
        (outcomes, hex::encode(st.root_hash()))
    };
    let (a, b) = (run(d.c.state.clone()), run(d.c.state.clone()));
    assert_eq!(a, b);
    assert!(matches!(&a.0[..], [Ok(Outcome::Applied), Ok(Outcome::Applied), Ok(Outcome::Failed(_))]), "{:?}", a.0);
    d.c.produce_block_with(block).unwrap();
    assert_eq!(d.c.last_block().unwrap().state_root, a.1);
}

/// The VM checks a call's version digest itself (`VersionMismatch`), though admission refuses a
/// wrong one first: the action phase, on its own.
#[test]
fn a_wrong_digest_fails_in_the_vm() {
    let mut d = Dev::new("digest", |_| {});
    let alice = keys(ALICE);
    d.applied(vec![d.tx(&alice, upload_guest("counter"))]);
    let counter = d.program(ALICE, d.c.state.account(&alice.address()).nonce);
    d.applied(vec![d.tx(&alice, create_guest("counter", None))]);
    let wrong = ProgramCall { version_digest: "11".repeat(32), ..call(&d, &counter, "increment", vec![], 0) };
    let tx = d.tx(&alice, calls(vec![wrong], vec![]));
    let refused = crate::programs::admit(&d.c.state, &tx, d.next(), ApplyMode::Block).unwrap_err().to_string();
    assert!(refused.starts_with(&format!("call 1 names version 1111111111… of program {}, whose version in force is ", crate::chain::short(&counter))), "{refused}");
    let mut st = d.c.state.clone();
    let mut j = Journal::default();
    let e = super::super::apply(&mut st, &mut j, &tx, None, &alice.address(), &key_id(&alice.public_key()), d.next()).unwrap_err();
    assert_eq!(e.to_string(), format!("program {} trapped: version mismatch (trap 18)", crate::chain::short(&counter)));
    assert!(st.programs.fault.is_none());
}

/// A run is named by its transaction's id (WBS stage N): the key of its receipt, a fault's record
/// and the logs. Handed over by a caller that already has it (a proposer's trial, a verification)
/// or computed when none does, it is the same, so the receipts, the fault and the state a run
/// leaves don't depend on who hashed the transaction.
#[test]
fn a_runs_id_names_its_receipt_and_its_fault_whoever_hashed_the_transaction() {
    let mut d = Dev::new("run-id", |_| {});
    let alice = keys(ALICE);
    d.applied(vec![d.tx(&alice, upload_guest("counter"))]);
    let counter = d.program(ALICE, d.c.state.account(&alice.address()).nonce);
    d.applied(vec![d.tx(&alice, create_guest("counter", None))]);
    let tx = d.tx(&alice, calls(vec![call(&d, &counter, "increment", vec![], 0)], vec![]));
    let (h, fee_sink, id) = (d.next(), d.c.me().unwrap(), tx.id());
    let run = |given: Option<&str>, fault: Option<Injected>| {
        if let Some(f) = fault {
            inject::arm(&id, f);
        }
        let mut st = d.c.state.clone();
        let outcome = st.apply_j_with_id(&mut Journal::default(), &tx, given, CHAIN, &fee_sink, h, ApplyMode::Block);
        inject::disarm(&id);
        (outcome.map_err(|e| e.to_string()), st)
    };
    let (computed, by_none) = run(None, None);
    let (handed, by_caller) = run(Some(&id), None);
    assert_eq!((&computed, &handed), (&Ok(Outcome::Applied), &Ok(Outcome::Applied)));
    let keyed: Vec<&str> = by_caller.programs.block.receipts.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keyed, [id.as_str()], "the receipt is keyed by the transaction's id");
    assert_eq!(by_none.programs.block, by_caller.programs.block, "the same receipt either way");
    assert_eq!(by_none.account(&alice.address()).balance, by_caller.account(&alice.address()).balance);
    for given in [None, Some(id.as_str())] {
        let (outcome, st) = run(given, Some(Injected::Panic));
        assert!(matches!(&outcome, Ok(Outcome::Failed(why)) if why.starts_with("program execution faulted on this node")), "{outcome:?}");
        assert_eq!(st.programs.fault.map(|f| (f.height, f.tx)), Some((h, id.clone())), "the fault names the transaction by its id");
    }
}

/// A caller that hands over the wrong id would key the receipt, the fault and the logs to another
/// transaction, so test builds refuse it where it enters (`programs::apply`).
#[test]
#[should_panic(expected = "another transaction's id")]
fn an_id_that_is_not_the_transactions_is_refused_in_test_builds() {
    let mut d = Dev::new("wrong-id", |_| {});
    let alice = keys(ALICE);
    d.applied(vec![d.tx(&alice, upload_guest("counter"))]);
    let counter = d.program(ALICE, d.c.state.account(&alice.address()).nonce);
    d.applied(vec![d.tx(&alice, create_guest("counter", None))]);
    let tx = d.tx(&alice, calls(vec![call(&d, &counter, "increment", vec![], 0)], vec![]));
    let wrong = "ab".repeat(32);
    let mut st = d.c.state.clone();
    let _ = st.apply_j_with_id(&mut Journal::default(), &tx, Some(&wrong), CHAIN, &d.c.me().unwrap(), d.next(), ApplyMode::Block);
}

/// Node faults (§6.2) map to their class and stick to the state: a spawn the OS refuses is
/// retried (refused once, the call runs), a third refusal in a row is a `Setup` fault, and a panic
/// inside the VM (injected through the view) a `Tx` fault. Once one is recorded, no program runs
/// on that state.
#[test]
fn node_faults_are_classed_and_stick() {
    let mut d = Dev::new("faults", |_| {});
    let alice = keys(ALICE);
    d.applied(vec![d.tx(&alice, upload_guest("counter"))]);
    let counter = d.program(ALICE, d.c.state.account(&alice.address()).nonce);
    d.applied(vec![d.tx(&alice, create_guest("counter", None))]);
    let tx = d.tx(&alice, calls(vec![call(&d, &counter, "increment", vec![], 0)], vec![]));
    let (h, fee_sink) = (d.next(), d.c.me().unwrap());
    let id = tx.id();
    let run = |what: Option<Injected>| {
        if let Some(w) = what {
            inject::arm(&id, w);
        }
        let mut st = d.c.state.clone();
        let outcome = st.apply(&tx, CHAIN, &fee_sink, h);
        inject::disarm(&id);
        (outcome.map_err(|e| e.to_string()), st)
    };
    let (ok, st) = run(Some(Injected::RefuseSpawns(2)));
    assert_eq!((ok, st.programs.fault), (Ok(Outcome::Applied), None), "two refusals, then a thread");
    let (setup, st) = run(Some(Injected::RefuseSpawns(3)));
    assert!(matches!(&setup, Ok(Outcome::Failed(why)) if why.starts_with("program execution faulted on this node (the OS refused a program thread 3 times")), "{setup:?}");
    let fault = st.programs.fault.clone().unwrap();
    assert_eq!((fault.class, fault.height, fault.tx.as_str()), (FaultClass::Setup, h, id.as_str()));
    let (panicked, mut st) = run(Some(Injected::Panic));
    assert!(matches!(&panicked, Ok(Outcome::Failed(why)) if why == "program execution faulted on this node (a panic in program execution): never this transaction's outcome"), "{panicked:?}");
    assert_eq!(st.programs.fault.as_ref().map(|f| f.class), Some(FaultClass::Tx));
    // Sticky: the next program transaction on that state runs nothing.
    let next = Tx { nonce: tx.nonce + 1, ..d.tx(&alice, calls(vec![call(&d, &counter, "increment", vec![], 0)], vec![])) };
    let started = counts();
    let again = st.apply(&next, CHAIN, &fee_sink, h).unwrap();
    assert!(matches!(&again, Outcome::Failed(why) if why.starts_with("program execution faulted on this node at block")), "{again:?}");
    assert_eq!(counts(), started);
}
