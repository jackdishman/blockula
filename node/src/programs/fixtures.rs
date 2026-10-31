//! Test fixtures for programs' admission, fee and mempool tests (WBS S3): a state with programs on,
//! records installed by hand as an upload and a create would leave them (with every total
//! consistent, so `State::check_invariants` holds), program transactions to admit, and a dev chain
//! with programs on at genesis.

use super::digest;
use super::state::{blob_key, BlobRef, CodeRecord, InterfaceRecord, Pause, ProgramRecord, Versioned, BLOB_CODE, BLOB_INTERFACE};
use super::{status, Status};
use crate::chain::{now_ms, Account, Allocation, Chain, Genesis, KeyRecord, State, SummonOpts, ValidatorInfo};
use crate::crypto::{self, Keys};
use crate::governance::Multisig;
use crate::types::*;
use crate::upgrade::{ScheduledUpgrade, PROGRAMS_FEATURE, PROGRAMS_VERSION};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub const FANG: u64 = DROPS_PER_FANG;
pub const CHAIN: &str = "programs-fees-1";
pub const ALICE: u8 = 1;
pub const BOB: u8 = 2;
pub const VALIDATOR: u8 = 31;

/// W1's guests, with stage V's interfaces: `(wasm, interface)`.
pub const COUNTER: (&[u8], &[u8]) = (include_bytes!("../../vm/tests/programs/counter.wasm"), include_bytes!("../../vm/tests/programs/counter.iface"));
pub const TOKEN: (&[u8], &[u8]) = (include_bytes!("../../vm/tests/programs/token.wasm"), include_bytes!("../../vm/tests/programs/token.iface"));
pub const ESCROW: (&[u8], &[u8]) = (include_bytes!("../../vm/tests/programs/escrow.wasm"), include_bytes!("../../vm/tests/programs/escrow.iface"));

pub fn keys(key_seed: u8) -> Keys {
    Keys::from_seed([key_seed; 32])
}

pub fn addr(key_seed: u8) -> String {
    keys(key_seed).address()
}

/// A chain born at v2 (from block 1) whose genesis lists `features`: one validator, and Alice and
/// Bob with 1,000 FANG each.
pub fn genesis(features: &[&str]) -> Genesis {
    let alloc = |key_seed: u8| Allocation { address: addr(key_seed), amount: 1_000 * FANG, vesting: None, label: None };
    Genesis {
        chain_id: CHAIN.into(),
        token: "FANG".into(),
        decimals: DECIMALS,
        block_time_ms: 1_000,
        validators: vec![ValidatorInfo { address: addr(VALIDATOR), public_key: hex::encode(keys(VALIDATOR).public_key()), p2p_addr: "127.0.0.1:7101".into(), http_addr: "127.0.0.1:7201".into() }],
        faucet: false,
        created_at_ms: 1_750_000_000_000,
        allocations: [ALICE, BOB, VALIDATOR].map(alloc).to_vec(),
        epoch_blocks: 600,
        validator_admins: vec![],
        contribution: None,
        address_prefix: "boo".into(),
        param_overrides: Default::default(),
        operators: vec![],
        reserved_names: vec![],
        soul_ledger: vec![],
        features: features.iter().map(|f| f.to_string()).collect(),
        upgrades: vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 1 }],
        bootstrap_excluded: vec![],
        pool_repos: Default::default(),
    }
}

/// [`genesis`]'s state with programs on from block 0 (META `programs` at its defaults).
pub fn state_on() -> State {
    let g = genesis(&[PROGRAMS_FEATURE]);
    g.check().unwrap();
    g.initial_state()
}

/// Stores `wasm` and `interface` as an upload in block `at` would: their records and blob
/// references, and META's `state_bytes` (with no deposit, so escrow and supply stay as they are).
/// Returns their hashes. A blob stored already is left as it is.
pub fn store(st: &mut State, wasm: &[u8], interface: &[u8], at: u64) -> ([u8; 32], [u8; 32]) {
    let (w, i) = (digest::wasm_hash(wasm), digest::interface_hash(interface));
    let blob = |size: usize| Versioned(BlobRef { uploader: addr(ALICE), deposit: 0, size: size as u32, refs: 0, uploaded_at: at, last_used_at: at });
    let p = &mut st.programs;
    let mut added = 0;
    if !p.code.contains_key(&w) {
        // Exports as validation reads them; code that doesn't validate (a test's stand-in for a large
        // module) exports nothing.
        let exports = blockula_vm::validate::validate(wasm).map(|m| m.exports).unwrap_or_default();
        p.code.insert(w, Versioned(CodeRecord { wasm: wasm.to_vec(), exports }));
        p.blob_refs.insert(blob_key(BLOB_CODE, &w), blob(wasm.len()));
        added += wasm.len() as u64;
    }
    if !p.interfaces.contains_key(&i) {
        p.interfaces.insert(i, Versioned(InterfaceRecord::new(interface.to_vec())));
        p.blob_refs.insert(blob_key(BLOB_INTERFACE, &i), blob(interface.len()));
        added += interface.len() as u64;
    }
    st.programs.meta.as_mut().expect("programs exist").state_bytes += added;
    (w, i)
}

/// A program at `address` from stored code and interface, created in block `at` as a create
/// without `init` leaves it (its record, with no deposit, and an empty account), counted in META
/// and in its blobs' references. Returns its version digest as a call names it (hex).
pub fn install(st: &mut State, address: &str, (wasm_hash, interface_hash): ([u8; 32], [u8; 32]), at: u64) -> String {
    let record = ProgramRecord {
        creator: addr(ALICE),
        created_at: at,
        wasm_hash,
        interface_hash,
        version: digest::FIRST_VERSION,
        authority: None,
        upgrade_delay_blocks: 0,
        policy: 0,
        accepts_transfers: false,
        items: 0,
        bytes: 0,
        deposit: 0,
        sponsored: 0,
        record_deposit: 0,
        record_payer: addr(ALICE),
    };
    st.programs.records.insert(address.to_string(), Versioned(record));
    st.accounts.insert(address.to_string(), Account::default());
    for key in [blob_key(BLOB_CODE, &wasm_hash), blob_key(BLOB_INTERFACE, &interface_hash)] {
        st.programs.blob_refs.get_mut(&key).expect("stored").refs += 1;
    }
    let m = st.programs.meta.as_mut().expect("programs exist");
    m.programs += 1;
    m.state_bytes += super::RECORD_BYTES;
    hex::encode(digest::version_digest(address, &wasm_hash, &interface_hash, digest::FIRST_VERSION))
}

/// The address of the program the `n`th create of `key_seed`'s account (at nonce `n`) makes.
pub fn program_of(key_seed: u8, nonce: u64) -> String {
    digest::program_address("boo", &crypto::address_bytes(&addr(key_seed)).unwrap(), nonce)
}

/// `action` from `key_seed`'s account at its next nonce on `st`, valid from `height` for a while, with
/// a fee of 1 FANG (only the charge is debited) and a placeholder signature (the state machine
/// doesn't check signatures: admission and verification do, before it).
pub fn tx(st: &State, key_seed: u8, action: Action, height: u64) -> Tx {
    let from = addr(key_seed);
    Tx { chain_id: CHAIN.into(), genesis_hash: "00".repeat(32), nonce: st.account(&from).nonce, from, fee: FANG, tip: 0, valid_until: height + 10, action, memo: String::new(), public_key: hex::encode(keys(key_seed).public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) }
}

/// An upload of `wasm` and `interface`, bonding at most `storage_limit`.
pub fn upload(wasm: &[u8], interface: &[u8], storage_limit: u64) -> Action {
    Action::UploadCode { wasm: HexBytes(wasm.to_vec()), interface: HexBytes(interface.to_vec()), storage_limit }
}

/// The largest code the codec takes: the Wasm header, then zeros (it fails validation, which only
/// a proposer's trial runs).
pub fn largest_wasm() -> Vec<u8> {
    let mut w = WASM_HEADER.to_vec();
    w.resize(blockula_vm::config::MAX_CODE_BYTES, 0);
    w
}

/// An immutable create from stored blobs, without `init` and its fields at their one encoding.
pub fn create(wasm_hash: &[u8; 32], interface_hash: &[u8; 32]) -> Action {
    Action::CreateProgram { wasm_hash: hex::encode(wasm_hash), interface_hash: hex::encode(interface_hash), authority: None, upgrade_delay_blocks: 0, policy: ProgramPolicy::Open, accepts_transfers: false, init: None, fuel_limit: 0, storage_limit: FANG, bindings: vec![] }
}

/// One call of `method` on `program` at `version_digest`, sending `value`.
pub fn call(program: &str, version_digest: &str, method: &str, value: u64) -> ProgramCall {
    ProgramCall { program: program.into(), version_digest: version_digest.into(), method: method.into(), input: HexBytes(vec![]), value }
}

/// A `CallProgram` of `calls`, with `fuel_limit`, no deposits and `bindings`.
pub fn calls(calls: Vec<ProgramCall>, fuel_limit: u64, bindings: Vec<Binding>) -> Action {
    Action::CallProgram { calls, fuel_limit, storage_limit: 0, bindings }
}

/// A dev chain's home (one validator, `boo` addresses, `params` overriding its parameters) born at
/// v2 with programs on at genesis, and the chain opened there as its validator.
pub fn dev_chain(tag: &str, params: &[(&str, u64)]) -> (PathBuf, Chain) {
    let home = std::env::temp_dir().join(format!("blockula-programs-s3-{tag}-{}-{}", now_ms(), std::process::id()));
    let mut o = SummonOpts::new("programs-dev-1", 1);
    o.address_prefix = crypto::ADDRESS_HRP.into();
    for (k, v) in params {
        o.params.insert(k.to_string(), *v);
    }
    Chain::summon(&home, &o).unwrap();
    let path = home.join("genesis.json");
    let mut g: Genesis = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    g.upgrades = vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 1 }];
    g.features = vec![PROGRAMS_FEATURE.to_string()];
    g.check().unwrap();
    std::fs::write(&path, serde_json::to_string_pretty(&g).unwrap()).unwrap();
    let c = Chain::open(&home).unwrap();
    (home, c)
}

/// `action` from `k`'s account on `c` at nonce `nonce`, paying `fee`, signed.
pub fn signed(c: &Chain, k: &Keys, nonce: u64, fee: u64, action: Action) -> Tx {
    let from = k.address_with(c.state.params.prefix());
    let tx = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), nonce, from, fee, tip: 0, valid_until: c.height() + 100, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
    tx.sign(k)
}

// ---- spec 10.1's admission matrix (WBS T), shared by the tests that must agree with it ----

/// The height the matrix's transactions are checked at.
pub const H: u64 = 5;

/// The state a create's and a call's checks run against: programs on; the counter, token and
/// escrow uploaded at block 1 and created at block 2 (by Bob's account, nonces 0 to 2); two
/// programs whose code is as large as the codec takes (the load tests'); and the height checked,
/// [`H`].
pub struct World {
    pub st: State,
    pub counter: (String, String),
    pub token: (String, String),
    pub escrow: (String, String),
    pub big: [(String, String); 2],
    pub blobs: [([u8; 32], [u8; 32]); 3],
}

pub fn world() -> World {
    let mut st = state_on();
    let mut made = vec![];
    let mut blobs = vec![];
    for (n, guest) in [COUNTER, TOKEN, ESCROW].into_iter().enumerate() {
        let b = store(&mut st, guest.0, guest.1, 1);
        let address = program_of(BOB, n as u64);
        let digest = install(&mut st, &address, b, 2);
        made.push((address, digest));
        blobs.push(b);
    }
    let mut big = vec![];
    for n in 0..2u8 {
        let mut wasm = largest_wasm();
        *wasm.last_mut().unwrap() = n;
        let b = store(&mut st, &wasm, COUNTER.1, 1);
        let address = program_of(BOB, 10 + u64::from(n));
        let digest = install(&mut st, &address, b, 2);
        big.push((address, digest));
    }
    let [counter, token, escrow]: [(String, String); 3] = made.try_into().unwrap();
    World { st, counter, token, escrow, big: big.try_into().unwrap(), blobs: blobs.try_into().unwrap() }
}

/// The kinds of signer of §10.1's matrix, each as a tx of `action` on `st`: an owner key on an
/// ordinary account (Alice's own), a session key on it, a validator's consensus key acting for its
/// owner (Alice), a multisig's owner key (Bob's account is a 1-of-1 multisig), and a program's
/// address as `from`; and a sixth, since §2.5 tells it apart: the consensus key on the validator's
/// own account, which is the original key of an ordinary account. `st` gets the keys and the
/// multisig each needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signer {
    Owner,
    Session,
    ConsensusForOwner,
    ConsensusOnOwnAccount,
    MultisigOwner,
    Program,
}

pub fn signers(st: &mut State, action: &Action, height: u64) -> Vec<(Signer, Tx)> {
    let (session, owner_key) = (keys(51), keys(52));
    // Alice's account: her original key and a session key.
    let original = hex::encode(crypto::address_bytes(&addr(ALICE)).unwrap());
    let record = |role| KeyRecord { label: "k".into(), role, added_at: 0, spent: 0, window_start: 0 };
    let alice = st.accounts.get_mut(&addr(ALICE)).unwrap();
    alice.keys = BTreeMap::from([(original, record(KeyRole::Owner)), (key_id(&session.public_key()), record(KeyRole::Session { limit: 100 * FANG, period_blocks: 100, expires_at: 1_000_000 }))]);
    // The validator is Alice's.
    st.validators.get_mut(&addr(VALIDATOR)).unwrap().owner = addr(ALICE);
    // Bob's account is a 1-of-1 multisig.
    st.accounts.get_mut(&addr(BOB)).unwrap().keys = BTreeMap::from([(key_id(&owner_key.public_key()), record(KeyRole::Owner))]);
    st.governance.multisigs.insert(addr(BOB), Multisig { threshold: 1, created_at: 0, pending: BTreeSet::new() });
    // A program's account if the state has one (programs exist from the feature's first activation).
    let program = st.programs.records.keys().next().cloned().unwrap_or_else(|| program_of(ALICE, 0));
    let base = tx(st, ALICE, action.clone(), height);
    let signed_by = |from: String, k: &Keys| Tx { nonce: st.account(&from).nonce, from, public_key: hex::encode(k.public_key()), ..base.clone() };
    vec![
        (Signer::Owner, base.clone()),
        (Signer::Session, signed_by(addr(ALICE), &session)),
        (Signer::ConsensusForOwner, signed_by(addr(ALICE), &keys(VALIDATOR))),
        (Signer::ConsensusOnOwnAccount, signed_by(addr(VALIDATOR), &keys(VALIDATOR))),
        (Signer::MultisigOwner, signed_by(addr(BOB), &owner_key)),
        (Signer::Program, signed_by(program, &keys(ALICE))),
    ]
}

/// The five statuses of §10.1's matrix, each as a state ([`column_state`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    /// Programs on (META, the feature, programs created), with v2 scheduled above [`H`]: only the
    /// version keeps them dormant, `status`'s first rule. (A v1 chain with no programs, testnet-2
    /// forever, is dormant by its second rule too; `every_program_tag_is_refused_while_dormant_whoever_signs`
    /// runs those.)
    DormantBelowV2,
    /// Born at v2, programs never switched on (`blockula-1` at launch): no META.
    DormantAtV2,
    On,
    ExitOnly,
    /// A pause running at [`H`], written into META by hand (`pause_signed` empty): the status gate's
    /// verdict only. Tag 69's own limits under a pause a validator really signed (§7.2's window,
    /// which `pause_signed` records) are `admit::tests::tag_69_while_paused_follows_the_window`.
    Paused,
}

pub const COLUMNS: [Column; 5] = [Column::DormantBelowV2, Column::DormantAtV2, Column::On, Column::ExitOnly, Column::Paused];

impl Column {
    /// The `status` its state reads at [`H`].
    pub fn status(self) -> Status {
        match self {
            Column::DormantBelowV2 | Column::DormantAtV2 => Status::Dormant,
            Column::On => Status::On,
            Column::ExitOnly => Status::ExitOnly,
            Column::Paused => Status::Paused { until: H + 10 },
        }
    }
}

/// `column`'s state, from the [`world`]: asserts it reads `column.status()` at [`H`].
pub fn column_state(w: &World, column: Column) -> State {
    let mut st = w.st.clone();
    match column {
        Column::DormantBelowV2 => st.upgrade.schedule = vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: H + 1 }],
        Column::DormantAtV2 => st = genesis(&[]).initial_state(),
        Column::On => {}
        Column::ExitOnly => {
            st.upgrade.features.remove(PROGRAMS_FEATURE);
        }
        Column::Paused => st.programs.meta.as_mut().unwrap().pause = Some(Pause { from: H, until: H + 10, started: H, signers: vec![addr(VALIDATOR)] }),
    }
    // Bob's account made the world's programs at nonces 0 to 2 and 10, 11: a create of his
    // multisig key's would otherwise make the address of one of them.
    if let Some(bob) = st.accounts.get_mut(&addr(BOB)).filter(|_| column != Column::DormantAtV2) {
        bob.nonce = 20;
    }
    assert_eq!(status(&st, H), column.status(), "{column:?}");
    st
}

/// The program actions of the matrix, each a valid one on the [`world`]'s state: an upload, a create,
/// a value-free call, a call carrying FANG, a call batch in which only a later call carries FANG
/// (exit-only refuses a call "carrying value" as a whole, §7.3: `any`, not the first), and the pause.
pub fn matrix_actions(w: &World) -> Vec<Action> {
    let at = |method: &str, value: u64| call(&w.escrow.0, &w.escrow.1, method, value);
    vec![
        upload(COUNTER.0, COUNTER.1, FANG),
        create(&w.blobs[2].0, &w.blobs[2].1),
        calls(vec![at("refund", 0)], 100_000, vec![]),
        calls(vec![at("open", FANG)], 100_000, vec![]),
        calls(vec![at("refund", 0), at("open", FANG)], 100_000, vec![]),
        Action::ProgramsPause {},
    ]
}

/// A matrix action as a cell's label shows it: its kind, and a call batch's values.
pub fn describe(action: &Action) -> String {
    match action {
        Action::CallProgram { calls, .. } => format!("{} (values {:?})", action.kind(), calls.iter().map(|c| c.value).collect::<Vec<_>>()),
        _ => action.kind().to_string(),
    }
}

/// What the spec says of `action` at `column`, for a signer who may sign it at all (§2.1: an owner
/// key on an ordinary account): `None` if it's admitted, else a phrase its refusal carries.
/// §1.2 (dormant: every program tag refused), §2.2–2.5 (`status` is `On`; in exit-only a call that
/// carries no FANG, and the pause), §7.2 (a running pause refuses 62–64 and lets the pause renew:
/// that is the status gate's, tag 69's own limits are `pause::may_sign`'s) and §7.3 (exit-only
/// refuses uploads, creates and a call batch with any value).
pub fn spec_refusal(action: &Action, column: Column) -> Option<&'static str> {
    match (column, action) {
        (Column::DormantBelowV2 | Column::DormantAtV2, _) => Some("programs are off"),
        (Column::On, _) => None,
        (Column::ExitOnly | Column::Paused, Action::ProgramsPause {}) => None,
        (Column::ExitOnly, Action::UploadCode { .. } | Action::CreateProgram { .. }) => Some("programs are exit-only"),
        (Column::ExitOnly, Action::CallProgram { calls, .. }) => calls.iter().any(|c| c.value > 0).then_some("a call can't send a program FANG"),
        (Column::Paused, Action::UploadCode { .. } | Action::CreateProgram { .. } | Action::CallProgram { .. }) => Some("programs are paused until block"),
        _ => panic!("{} isn't a program action", action.kind()),
    }
}

/// The verdict of `programs::admit` alone (the last check of `State::admit`): the status's, except
/// that no transaction acts from a program, and that tag 69's signer must resolve to a validator of
/// the epoch's active set (`staking::validator_for_signer`): any key of the validator's owner
/// account does, a session key and a consensus key acting for the owner included, since the rules
/// that keep those from signing it are `State::admit`'s own, earlier ones ([`state_refusal`], §2.1);
/// a multisig's owner key and a program's address do not.
pub fn programs_refusal(signer: Signer, action: &Action, column: Column) -> Option<&'static str> {
    let dormant = matches!(column, Column::DormantBelowV2 | Column::DormantAtV2);
    match signer {
        // The program check is gated on the status like every hook (§1.2): dormant, it has no programs to find.
        Signer::Program if !dormant => Some("programs have no keys"),
        Signer::MultisigOwner if !dormant && matches!(action, Action::ProgramsPause {}) => Some("only a validator in epoch 0's active set"),
        _ => spec_refusal(action, column),
    }
}

/// The verdict of the whole of `State::admit` (what `apply_j` refuses with an `Err`, and what
/// `submit_inner`'s early refusal must match, §8.2): §2.1's signers. Only an owner key on an ordinary
/// account signs a program action (a session key signs transfers, a consensus key acting for its
/// owner signs `staking::system_tx_key`'s list, a multisig's owner keys propose and approve, and no
/// program action nests), and a program has no keys; so the status decides for the owner key, and for
/// the consensus key on the validator's own account, which is an ordinary account's original key.
pub fn state_refusal(signer: Signer, action: &Action, column: Column) -> Option<&'static str> {
    match signer {
        Signer::Owner | Signer::ConsensusOnOwnAccount => spec_refusal(action, column),
        Signer::Session => Some("session keys can only send transfers"),
        Signer::ConsensusForOwner | Signer::Program => Some("this key isn't authorized on"),
        Signer::MultisigOwner => Some("-of-1 multisig: propose this with multisig_propose"),
    }
}

/// Checks `got` against a verdict from [`spec_refusal`], [`programs_refusal`] or [`state_refusal`]:
/// admitted exactly where it's `None`, else refused in words containing the phrase.
pub fn expect_verdict(what: &str, got: Result<(), String>, refusal: Option<&str>) {
    match (got, refusal) {
        (Ok(()), None) => {}
        (Err(e), Some(phrase)) => assert!(e.contains(phrase), "{what}: refused in other words than {phrase:?}: {e}"),
        (Ok(()), Some(phrase)) => panic!("{what}: admitted, but the spec refuses it ({phrase:?})"),
        (Err(e), None) => panic!("{what}: refused ({e}), but the spec admits it"),
    }
}
