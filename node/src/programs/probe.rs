//! What the tests of receipts and node faults run on (WBS S4b): the probe program, a dev chain
//! born at v2 with programs on, a second node of the same chain, and the consensus scaffolding
//! that drives `App::propose` and `App::verify`.

use super::digest;
// For the tests outside `programs`, which can't reach `view`: the fault injection.
pub use super::view::inject::{self, Injected};
use crate::chain::{Allocation, Chain, Finalized, Genesis, SummonOpts, Vesting};
use crate::consensus::{hex_32, App, CBlock, ReplayCache, Shared, SUPPORTED_PROTOCOL_VERSION};
use crate::crypto::{self, Keys};
use crate::pq::PrivateKey;
use crate::types::*;
use crate::upgrade::{ScheduledUpgrade, PROGRAMS_FEATURE, PROGRAMS_VERSION};
use blockula_vm::host::MethodKind;
use blockula_vm::interface::{Interface, Method};
use commonware_consensus::simplex::types::Context as SimplexContext;
use commonware_consensus::types::{Epoch, Round, View};
use commonware_consensus::{marshal, Application};
use commonware_cryptography::{blake3::Digest, Digestible as _, Signer as _};
use commonware_runtime::{deterministic, Clock as _, Runner as _, Supervisor as _};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub const FANG: u64 = DROPS_PER_FANG;
/// What a signed fee adds for fuel: a reservation's fee at the floor rate is at most
/// `MAX_RESERVATION / 4` drops, about 16.8M.
pub const FUEL_MARGIN: u64 = 20_000_000;
/// A call's `fuel_limit`.
pub const FUEL: u64 = 2_000_000;
/// The share of fuel the probe's `go` passes the `spin` it calls.
pub const SHARE: u64 = 100_000;

/// Who signs: users funded at genesis. Dave's 100 FANG vest from block 1,000 on, so all of it is
/// locked here.
pub const ALICE: u8 = 81;
pub const BOB: u8 = 82;
pub const CAROL: u8 = 83;
pub const DAVE: u8 = 84;

pub fn keys(user: u8) -> Keys {
    Keys::from_seed([user; 32])
}

/// The probe: a program whose methods each do one thing a receipt or a fault test needs. Assembled
/// from this, with `wat` 1.262.0 (node/vm's dev-dependency; no names, so no custom section):
///
/// ```wat
/// (module
///   (import "blockula" "return_data_v1" (func (param i32 i32)))
///   (import "blockula" "emit_v1" (func (param i32 i32 i32 i32)))
///   (import "blockula" "abort_v1" (func (param i32 i32 i32)))
///   (import "blockula" "input_read_v1" (func (param i32)))
///   (import "blockula" "call_v1" (func (param i32 i32 i32 i32 i32 i32 i64 i64) (result i32)))
///   (memory (export "memory") 1 1)
///   (data (i32.const 64) "spin")
///   (data (i32.const 100) "e")
///   ;; emits event `e` with 4 KiB of data
///   (func (export "bk_emit") (call 1 (i32.const 100) (i32.const 1) (i32.const 0) (i32.const 4096)))
///   ;; takes the value it's sent (payable) and does nothing
///   (func (export "bk_fund"))
///   ;; calls `spin` on the program whose 32 address bytes are its input, any version, with a
///   ;; share of 100,000 fuel
///   (func (export "bk_go")
///     (call 3 (i32.const 0))
///     (drop (call 4 (i32.const 0) (i32.const 32) (i32.const 64) (i32.const 4) (i32.const 0) (i32.const 0) (i64.const 0) (i64.const 100000))))
///   ;; aborts with code 7 and message "e"
///   (func (export "bk_no") (call 2 (i32.const 7) (i32.const 100) (i32.const 1)))
///   ;; returns 4 KiB (`MAX_RETURN`)
///   (func (export "bk_ret") (call 0 (i32.const 0) (i32.const 4096)))
///   ;; never returns: runs out of whatever fuel it has
///   (func (export "bk_spin") (loop (br 0))))
/// ```
pub const PROBE_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x26, 0x06, 0x60, 0x02, 0x7f, 0x7f, 0x00, //
    0x60, 0x04, 0x7f, 0x7f, 0x7f, 0x7f, 0x00, 0x60, 0x03, 0x7f, 0x7f, 0x7f, 0x00, 0x60, 0x01, 0x7f, //
    0x00, 0x60, 0x08, 0x7f, 0x7f, 0x7f, 0x7f, 0x7f, 0x7f, 0x7e, 0x7e, 0x01, 0x7f, 0x60, 0x00, 0x00, //
    0x02, 0x6e, 0x05, 0x08, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x75, 0x6c, 0x61, 0x0e, 0x72, 0x65, 0x74, //
    0x75, 0x72, 0x6e, 0x5f, 0x64, 0x61, 0x74, 0x61, 0x5f, 0x76, 0x31, 0x00, 0x00, 0x08, 0x62, 0x6c, //
    0x6f, 0x63, 0x6b, 0x75, 0x6c, 0x61, 0x07, 0x65, 0x6d, 0x69, 0x74, 0x5f, 0x76, 0x31, 0x00, 0x01, //
    0x08, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x75, 0x6c, 0x61, 0x08, 0x61, 0x62, 0x6f, 0x72, 0x74, 0x5f, //
    0x76, 0x31, 0x00, 0x02, 0x08, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x75, 0x6c, 0x61, 0x0d, 0x69, 0x6e, //
    0x70, 0x75, 0x74, 0x5f, 0x72, 0x65, 0x61, 0x64, 0x5f, 0x76, 0x31, 0x00, 0x03, 0x08, 0x62, 0x6c, //
    0x6f, 0x63, 0x6b, 0x75, 0x6c, 0x61, 0x07, 0x63, 0x61, 0x6c, 0x6c, 0x5f, 0x76, 0x31, 0x00, 0x04, //
    0x03, 0x07, 0x06, 0x05, 0x05, 0x05, 0x05, 0x05, 0x05, 0x05, 0x04, 0x01, 0x01, 0x01, 0x01, 0x07, //
    0x41, 0x07, 0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00, 0x07, 0x62, 0x6b, 0x5f, 0x65, //
    0x6d, 0x69, 0x74, 0x00, 0x05, 0x07, 0x62, 0x6b, 0x5f, 0x66, 0x75, 0x6e, 0x64, 0x00, 0x06, 0x05, //
    0x62, 0x6b, 0x5f, 0x67, 0x6f, 0x00, 0x07, 0x05, 0x62, 0x6b, 0x5f, 0x6e, 0x6f, 0x00, 0x08, 0x06, //
    0x62, 0x6b, 0x5f, 0x72, 0x65, 0x74, 0x00, 0x09, 0x07, 0x62, 0x6b, 0x5f, 0x73, 0x70, 0x69, 0x6e, //
    0x00, 0x0a, 0x0a, 0x4e, 0x06, 0x0e, 0x00, 0x41, 0xe4, 0x00, 0x41, 0x01, 0x41, 0x00, 0x41, 0x80, //
    0x20, 0x10, 0x01, 0x0b, 0x02, 0x00, 0x0b, 0x1c, 0x00, 0x41, 0x00, 0x10, 0x03, 0x41, 0x00, 0x41, //
    0x20, 0x41, 0xc0, 0x00, 0x41, 0x04, 0x41, 0x00, 0x41, 0x00, 0x42, 0x00, 0x42, 0xa0, 0x8d, 0x06, //
    0x10, 0x04, 0x1a, 0x0b, 0x0b, 0x00, 0x41, 0x07, 0x41, 0xe4, 0x00, 0x41, 0x01, 0x10, 0x02, 0x0b, //
    0x09, 0x00, 0x41, 0x00, 0x41, 0x80, 0x20, 0x10, 0x00, 0x0b, 0x07, 0x00, 0x03, 0x40, 0x0c, 0x00, //
    0x0b, 0x0b, 0x0b, 0x12, 0x02, 0x00, 0x41, 0xc0, 0x00, 0x0b, 0x04, 0x73, 0x70, 0x69, 0x6e, 0x00, //
    0x41, 0xe4, 0x00, 0x0b, 0x01, 0x65,
];

/// What the probe's `ret` returns and its `emit` emits: the first 4 KiB of its memory, zeros but
/// for its data segments ("spin" at 64, "e" at 100).
pub fn probe_bytes() -> Vec<u8> {
    let mut m = vec![0; 4096];
    m[64..68].copy_from_slice(b"spin");
    m[100] = b'e';
    m
}

/// The probe's interface: its six methods, all of kind `Call`, `fund` the only payable one.
pub fn probe_interface() -> Vec<u8> {
    let methods = ["emit", "fund", "go", "no", "ret", "spin"].map(|name| Method { name: name.into(), kind: MethodKind::Call, payable: name == "fund", args: vec![], returns: None });
    blockula_vm::interface::encode(&Interface { methods: methods.into() }).unwrap()
}

/// A method using a float: `(func (export "bk_x") (drop (f32.const 1)))` beside an exported
/// one-page memory (`exec::tests`' `FLOAT_WASM`). Validation refuses it (`Rule::Float`).
pub const FLOAT_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03, 0x02, //
    0x01, 0x00, 0x05, 0x04, 0x01, 0x01, 0x01, 0x01, 0x07, 0x11, 0x02, 0x06, 0x6d, 0x65, 0x6d, 0x6f, //
    0x72, 0x79, 0x02, 0x00, 0x04, 0x62, 0x6b, 0x5f, 0x78, 0x00, 0x00, 0x0a, 0x0a, 0x01, 0x08, 0x00, //
    0x43, 0x00, 0x00, 0x80, 0x3f, 0x1a, 0x0b,
];

pub fn upload(wasm: &[u8], interface: &[u8]) -> Action {
    Action::UploadCode { wasm: HexBytes(wasm.to_vec()), interface: HexBytes(interface.to_vec()), storage_limit: 10 * FANG }
}

/// A create of the probe, immutable, without `init` (it has no initializer).
pub fn create_probe() -> Action {
    create(PROBE_WASM, &probe_interface())
}

/// A create of the uploaded code `wasm` with `interface`, immutable, without `init`.
pub fn create(wasm: &[u8], interface: &[u8]) -> Action {
    Action::CreateProgram {
        wasm_hash: hex::encode(digest::wasm_hash(wasm)),
        interface_hash: hex::encode(digest::interface_hash(interface)),
        authority: None,
        upgrade_delay_blocks: 0,
        policy: ProgramPolicy::Open,
        accepts_transfers: false,
        init: None,
        fuel_limit: 0,
        storage_limit: FANG,
        bindings: vec![],
    }
}

/// A `CallProgram` of `calls` with [`FUEL`], a storage limit of 1 FANG and `bindings`.
pub fn calls(calls: Vec<ProgramCall>, bindings: Vec<Binding>) -> Action {
    Action::CallProgram { calls, fuel_limit: FUEL, storage_limit: FANG, bindings }
}

/// A dev chain's node (one validator, `boo` addresses) born at v2 with programs on at genesis,
/// Alice, Bob and Carol funded with 1,000 FANG each and Dave with 100 vesting ones.
pub struct Dev {
    pub home: PathBuf,
    pub c: Chain,
    /// Homes of the spare chains `c` held while this node's own was elsewhere ([`Dev::take`]).
    spares: Vec<PathBuf>,
}

impl Dev {
    pub fn new(tag: &str) -> Dev {
        Dev::with(tag, true, None)
    }

    /// A dev chain with no programs: its genesis schedules no upgrade and lists no feature, so it
    /// runs v1 rules, as testnet-2 does (no META `programs`, every program tag refused).
    pub fn dormant(tag: &str) -> Dev {
        Dev::with(tag, false, None)
    }

    /// [`Dev::new`] with programs on or off, and epochs of `epoch_blocks` blocks (the summoned
    /// default: 600).
    pub fn with(tag: &str, programs: bool, epoch_blocks: Option<u64>) -> Dev {
        let home = Dev::fresh_home(tag);
        let mut o = SummonOpts::new("programs-probe-1", 1);
        o.address_prefix = crypto::ADDRESS_HRP.into();
        if let Some(blocks) = epoch_blocks {
            o.epoch_blocks = blocks;
        }
        Chain::summon(&home, &o).unwrap();
        let path = home.join("genesis.json");
        let mut g: Genesis = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        if programs {
            g.upgrades = vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 1 }];
            g.features = vec![PROGRAMS_FEATURE.into()];
        }
        for user in [ALICE, BOB, CAROL] {
            g.allocations.push(Allocation { address: keys(user).address(), amount: 1_000 * FANG, vesting: None, label: None });
        }
        g.allocations.push(Allocation { address: keys(DAVE).address(), amount: 100 * FANG, vesting: Some(Vesting { start: 0, cliff: 1_000, end: 2_000 }), label: Some("vesting".into()) });
        g.check().unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&g).unwrap()).unwrap();
        let c = Chain::open(&home).unwrap();
        Dev { home, c, spares: vec![] }
    }

    /// A home no other test of this process (they run as threads of one) or of another process
    /// has: the time and the process id, and a count of this process's homes, since two tests can
    /// ask in the same millisecond.
    pub fn fresh_home(tag: &str) -> PathBuf {
        static HOMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = HOMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("blockula-s4b-{tag}-{}-{}-{n}", crate::chain::now_ms(), std::process::id()))
    }

    /// Another node of the same chain, with the same validator key (so the same proposer and fee
    /// sink), that has executed nothing yet.
    pub fn node(&self, tag: &str) -> Dev {
        let home = Dev::fresh_home(tag);
        std::fs::create_dir_all(&home).unwrap();
        for file in ["genesis.json", "validator.key"] {
            std::fs::copy(self.home.join(file), home.join(file)).unwrap();
        }
        let c = Chain::open(&home).unwrap();
        Dev { home, c, spares: vec![] }
    }

    /// This node's chain, leaving a spare in its place (a fresh chain of the same genesis in a home
    /// of its own) until [`Dev::put`] gives it back.
    pub fn take(&mut self) -> Chain {
        let home = Dev::fresh_home("spare");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::copy(self.home.join("genesis.json"), home.join("genesis.json")).unwrap();
        let spare = Chain::open(&home).unwrap();
        self.spares.push(home);
        std::mem::replace(&mut self.c, spare)
    }

    pub fn put(&mut self, c: Chain) {
        self.c = c;
    }

    /// The node restarted: its chain dropped, then opened again from its home. On an error the
    /// node holds a spare until it's reopened.
    pub fn reopen(&mut self) -> anyhow::Result<()> {
        // The database is single-process: the old chain lets go of it first.
        drop(self.take());
        self.c = Chain::open(&self.home)?;
        Ok(())
    }

    pub fn prefix(&self) -> String {
        self.c.state.params.prefix().to_string()
    }

    /// The height the next block executes at.
    pub fn next(&self) -> u64 {
        self.c.height() + 1
    }

    pub fn address(&self, user: u8) -> String {
        keys(user).address_with(&self.prefix())
    }

    pub fn balance(&self, user: u8) -> u64 {
        self.c.state.account(&self.address(user)).balance
    }

    /// `actions` from `k`'s account at consecutive nonces from `nonce` (its next one if `None`),
    /// each paying the highest base fee already fixed plus the fuel margin, and tipping `tip`.
    pub fn txs_at(&self, k: &Keys, nonce: Option<u64>, tip: u64, actions: Vec<Action>) -> Vec<Tx> {
        let from = k.address_with(&self.prefix());
        let nonce = nonce.unwrap_or_else(|| self.c.state.account(&from).nonce);
        actions
            .into_iter()
            .enumerate()
            .map(|(i, action)| {
                let mut tx = Tx { chain_id: self.c.genesis.chain_id.clone(), genesis_hash: self.c.genesis_hash.clone(), nonce: nonce + i as u64, from: from.clone(), fee: 0, tip, valid_until: self.c.height() + 100, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
                tx.fee = crate::econ::max_fee(&self.c.state, tx.size()) + FUEL_MARGIN + tip;
                tx.sign(k)
            })
            .collect()
    }

    pub fn txs(&self, k: &Keys, actions: Vec<Action>) -> Vec<Tx> {
        self.txs_at(k, None, 0, actions)
    }

    pub fn tx(&self, k: &Keys, action: Action) -> Tx {
        self.txs(k, vec![action]).remove(0)
    }

    /// The next block with `txs`, as consensus would hand it over, at a fixed time (so every node
    /// of the chain gets the same one).
    pub fn finalized(&self, txs: Vec<Tx>) -> Finalized {
        let mut f = self.c.test_block(txs);
        f.timestamp_ms = 1_790_000_000_000 + f.height * 1_000;
        f
    }

    /// Executes `txs` as the next block.
    pub fn run(&mut self, txs: Vec<Tx>) -> anyhow::Result<()> {
        let f = self.finalized(txs);
        self.c.execute(f)
    }

    /// [`Dev::run`], which must succeed; the block.
    pub fn block(&mut self, txs: Vec<Tx>) -> Block {
        self.run(txs).unwrap();
        self.c.last_block().unwrap().clone()
    }

    /// Alice uploads the probe, then creates it `n` times in one block: the programs' addresses.
    pub fn deploy(&mut self, n: usize) -> Vec<String> {
        let alice = keys(ALICE);
        let upload = self.tx(&alice, upload(PROBE_WASM, &probe_interface()));
        self.block(vec![upload]);
        let creates = self.txs(&alice, vec![create_probe(); n]);
        let programs: Vec<String> = creates.iter().map(|t| digest::program_address(&self.prefix(), &crypto::address_bytes(&t.from).unwrap(), t.nonce)).collect();
        let b = self.block(creates);
        assert!(b.failed.is_empty(), "{:?}", b.failed);
        programs
    }

    /// The version digest in force of `program`, as a call names it.
    pub fn version(&self, program: &str) -> String {
        let r = &self.c.state.programs.records[program];
        hex::encode(digest::version_digest(program, &r.wasm_hash, &r.interface_hash, r.version))
    }

    /// One call of `method` on `program`, sending `value`, with `input`.
    pub fn call(&self, program: &str, method: &str, input: Vec<u8>, value: u64) -> ProgramCall {
        ProgramCall { program: program.into(), version_digest: self.version(program), method: method.into(), input: HexBytes(input), value }
    }

    /// A binding of `program` at its version in force.
    pub fn bind(&self, program: &str) -> Binding {
        Binding { program: program.into(), version_digest: self.version(program) }
    }
}

/// Tests run as threads of one process and ask for homes in the same millisecond: each home is its
/// own, or two tests would write into (and delete) one directory.
#[test]
fn homes_asked_for_at_once_are_distinct() {
    let homes: std::collections::BTreeSet<PathBuf> = (0..64).map(|_| Dev::fresh_home("same")).collect();
    assert_eq!(homes.len(), 64);
}

impl Drop for Dev {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.home).ok();
        for home in &self.spares {
            std::fs::remove_dir_all(home).ok();
        }
    }
}

/// Consensus as a lone validator drives it, over a [`Dev`] whose executed tip is height `e`: an
/// unexecuted parent at `e + 1` (empty, or carrying `parent_txs`) and the proposals and
/// verifications at `e + 2` on it.
pub struct Round2 {
    pub chain: Shared,
    pub parent: CBlock,
    pub key_seed: [u8; 32],
    pub prefix: String,
    /// The executed tip's height.
    pub e: u64,
    /// What the proposals and verifications here share, as the consensus engine's clones of `App`
    /// do (§6.6): their replays cache the parent's post-state here.
    pub cache: ReplayCache,
}

impl Round2 {
    /// Takes `d`'s chain over (the [`Dev`] keeps a fresh placeholder chain of its own until it's
    /// given back with [`Round2::give_back`]).
    pub fn new(d: &mut Dev, parent_txs: &[Tx]) -> Round2 {
        let e = d.c.height();
        let version = crate::upgrade::version_at(&d.c.state, e + 1);
        let key_seed: [u8; 32] = hex::decode(std::fs::read_to_string(d.home.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
        let prefix = d.prefix();
        let leader = PrivateKey::from_seed_bytes(key_seed).public_key().address(&prefix);
        let parent = CBlock::new(Digest(hex_32(d.c.last_hash())), e + 1, e + 1, e, 0, leader, e, hex_32(&d.c.root_at(e).unwrap()), parent_txs.iter().map(Tx::to_bytes).collect()).with_versions(version, SUPPORTED_PROTOCOL_VERSION);
        Round2 { chain: Arc::new(Mutex::new(d.take())), parent, key_seed, prefix, e, cache: ReplayCache::default() }
    }

    /// The chain back into `d`.
    pub fn give_back(self, d: &mut Dev) {
        let c = Arc::try_unwrap(self.chain).ok().expect("no other owner").into_inner();
        d.put(c);
    }

    /// `App::propose` at `e + 2` on the parent: the block, or `None`.
    pub fn propose(&self) -> Option<CBlock> {
        self.propose_in(self.e + 2)
    }

    /// `App::propose` in consensus view `view` (above the parent's) on the parent: the block, or
    /// `None`.
    pub fn propose_in(&self, view: u64) -> Option<CBlock> {
        self.propose_on(vec![Arc::new(self.parent.clone())], view)
    }

    /// `App::propose` in consensus view `view` on `ancestry` (newest first: the parent, then the
    /// blocks above the executed tip it builds on, as [`unexecuted_chain`] makes them): the block,
    /// or `None`.
    pub fn propose_on(&self, ancestry: Vec<Arc<CBlock>>, view: u64) -> Option<CBlock> {
        let (chain, key_seed, prefix, cache) = (self.chain.clone(), self.key_seed, self.prefix.clone(), self.cache.clone());
        deterministic::Runner::timed(Duration::from_secs(60)).start(|context| async move {
            let leader = PrivateKey::from_seed_bytes(key_seed).public_key();
            let mut app = App { chain, block_time_ms: 1_000, address_prefix: prefix, replay_cache: cache };
            let parent = ancestry.first().expect("a parent").clone();
            let cctx = SimplexContext { round: Round::new(Epoch::zero(), View::new(view)), leader, parent: (View::new(parent.view), parent.digest()) };
            Application::<deterministic::Context>::propose(&mut app, (context.child("propose"), cctx), marshal::ancestry::from_iter(ancestry), ()).await
        })
    }

    /// A block at `e + 2` on the parent carrying `txs`, proposed by this validator a block time
    /// after the parent.
    pub fn block(&self, txs: &[Tx]) -> CBlock {
        let leader = PrivateKey::from_seed_bytes(self.key_seed).public_key();
        let version = crate::upgrade::version_at(&self.chain.lock().state, self.e + 2);
        CBlock::new(self.parent.digest(), self.e + 2, self.e + 2, self.parent.view, 1_000, leader.address(&self.prefix), self.e, self.parent.exec_root, txs.iter().map(Tx::to_bytes).collect()).with_versions(version, SUPPORTED_PROTOCOL_VERSION)
    }

    /// `App::verify` of `block` (at `e + 2`, on the parent): its verdict, or `None` if it's still
    /// pending after 10 simulated seconds (the verifier abstains).
    pub fn verify(&self, block: CBlock) -> Option<bool> {
        self.verify_parked(block, || ()).0
    }

    /// [`Round2::verify`], and what `observe` returns when the verdict is still pending after 10
    /// simulated seconds, run while the verification is still alive (parked, as the consensus
    /// engine keeps it until its view times out) and dropped only afterwards.
    pub fn verify_parked<T>(&self, block: CBlock, observe: impl FnOnce() -> T) -> (Option<bool>, Option<T>) {
        let (chain, parent, key_seed, prefix, cache) = (self.chain.clone(), self.parent.clone(), self.key_seed, self.prefix.clone(), self.cache.clone());
        deterministic::Runner::timed(Duration::from_secs(60)).start(|context| async move {
            let leader = PrivateKey::from_seed_bytes(key_seed).public_key();
            let mut app = App { chain, block_time_ms: 1_000, address_prefix: prefix, replay_cache: cache };
            let cctx = SimplexContext { round: Round::new(Epoch::zero(), View::new(block.view)), leader, parent: (View::new(parent.view), parent.digest()) };
            let ancestry = marshal::ancestry::from_iter([Arc::new(block), Arc::new(parent)]);
            let mut verdict = std::pin::pin!(Application::<deterministic::Context>::verify(&mut app, (context.child("verify"), cctx), ancestry));
            commonware_macros::select! {
                v = &mut verdict => (Some(v), None),
                _ = context.sleep(Duration::from_secs(10)) => (None, Some(observe())),
            }
        })
    }
}

/// An unexecuted chain on `d`'s executed tip, as `consensus::unexecuted` hands it to `replay`
/// (newest first): one block for each entry of `blocks`, the transactions it carries, at the next
/// heights, proposed by this chain's validator in a view of its own height. `stamp` goes into the
/// timestamps, so two chains over the same transactions are two forks (other digests).
pub fn unexecuted_chain(d: &Dev, blocks: &[Vec<Tx>], stamp: u64) -> Vec<Arc<CBlock>> {
    let e = d.c.height();
    let key_seed: [u8; 32] = hex::decode(std::fs::read_to_string(d.home.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
    let leader = PrivateKey::from_seed_bytes(key_seed).public_key().address(&d.prefix());
    let (mut parent, mut chain) = (Digest(hex_32(d.c.last_hash())), vec![]);
    for (i, txs) in blocks.iter().enumerate() {
        let height = e + 1 + i as u64;
        let version = crate::upgrade::version_at(&d.c.state, height);
        let block = CBlock::new(parent, height, height, height - 1, stamp + height, leader.clone(), e, hex_32(&d.c.root_at(e).unwrap()), txs.iter().map(Tx::to_bytes).collect()).with_versions(version, SUPPORTED_PROTOCOL_VERSION);
        parent = block.digest();
        chain.push(Arc::new(block));
    }
    chain.reverse();
    chain
}

/// A transaction that is well formed on paper and nothing else (its signature is zeros): for what
/// reads a transaction's fields and id, never its validity.
pub fn bare_tx(from: &str, nonce: u64, valid_until: u64, action: Action) -> Tx {
    Tx { chain_id: "bare".into(), genesis_hash: "00".repeat(32), from: from.into(), nonce, fee: 0, tip: 0, valid_until, action, memo: String::new(), public_key: "00".repeat(8), signature: "00".repeat(8) }
}

/// A call that names no program.
pub fn bare_call() -> Action {
    Action::CallProgram { calls: vec![], fuel_limit: 0, storage_limit: 0, bindings: vec![] }
}

/// An upload of `wasm` (no interface).
pub fn bare_upload(wasm: &[u8]) -> Action {
    Action::UploadCode { wasm: HexBytes(wasm.to_vec()), interface: HexBytes(vec![]), storage_limit: 0 }
}
