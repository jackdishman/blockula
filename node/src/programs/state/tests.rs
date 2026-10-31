//! Program state on its own (WBS S2): the records' formats, the round trips through records and a
//! snapshot, the journal, and one test per line of docs/PROGRAMS-V0.md §4.4's invariants, each on
//! a hand-built state (no program action exists yet to build one).

use super::*;
use crate::chain::{Account, Allocation, Genesis, ValidatorInfo};
use crate::crypto::{render_address, Keys};
use crate::state::codec::RecordKey;
use crate::types::{DECIMALS, DROPS_PER_FANG};
use crate::upgrade::{ScheduledUpgrade, PROGRAMS_FEATURE};

const FANG: u64 = DROPS_PER_FANG;
/// One of W1's guests, which passes validation: what a stored code record holds.
const WASM: &[u8] = include_bytes!("../../../vm/tests/programs/counter.wasm");
/// The program's 32 address bytes, and its two entries' keys.
const PROGRAM: [u8; 32] = [7; 32];
const KEY_OWN: &[u8] = b"sp";
const KEY_SIGNER: &[u8] = b"c";

fn keys(key_seed: u8) -> Keys {
    Keys::from_seed([key_seed; 32])
}

fn program() -> String {
    render_address("boo", &PROGRAM)
}

fn alice() -> String {
    keys(1).address()
}

fn validator() -> String {
    keys(31).address()
}

/// A chain born at v2 with programs on from genesis: one validator, one funded user.
fn genesis() -> Genesis {
    let alloc = |address: String, amount: u64| Allocation { address, amount, vesting: None, label: None };
    Genesis {
        chain_id: "programs-state-1".into(),
        token: "FANG".into(),
        decimals: DECIMALS,
        block_time_ms: 1_000,
        validators: vec![ValidatorInfo { address: validator(), public_key: hex::encode(keys(31).public_key()), p2p_addr: "127.0.0.1:7101".into(), http_addr: "127.0.0.1:7201".into() }],
        faucet: false,
        created_at_ms: 1_750_000_000_000,
        allocations: vec![alloc(alice(), 1_000 * FANG), alloc(validator(), 1_000 * FANG)],
        epoch_blocks: 600,
        validator_admins: vec![],
        contribution: None,
        address_prefix: "boo".into(),
        param_overrides: Default::default(),
        operators: vec![],
        reserved_names: vec![],
        soul_ledger: vec![],
        features: vec![PROGRAMS_FEATURE.into()],
        upgrades: vec![ScheduledUpgrade { version: 2, activation_height: 1 }],
        bootstrap_excluded: vec![],
        pool_repos: Default::default(),
    }
}

/// Every hash and size the populated state's records hold.
struct Built {
    wasm_hash: [u8; 32],
    interface_hash: [u8; 32],
}

fn interface_bytes() -> Vec<u8> {
    b"\x01an interface's bytes (decoded by the codec, not by the state)".to_vec()
}

/// The genesis state with what an upload, a create, a write the signer bonded, a write the program
/// sponsored, a transfer to it and a pending pause signature would have left, every total
/// consistent, committed. Each invariant test breaks one thing in it.
fn populated() -> (State, Built) {
    let mut st = genesis().initial_state();
    let alice_bytes = crate::crypto::address_bytes(&alice()).unwrap();
    let (wasm_hash, interface_hash) = (*blake3::hash(WASM).as_bytes(), [9; 32]);
    let interface = interface_bytes();
    let exports = blockula_vm::validate::validate(WASM).unwrap().exports;
    let own = Entry { value: vec![0xab; 100], payer: PROGRAM, deposit: 2_000, capacity: capacity(KEY_OWN.len(), 100) };
    let signer = Entry { value: 5u64.to_le_bytes().to_vec(), payer: alice_bytes, deposit: 1_000, capacity: capacity(KEY_SIGNER.len(), 8) };
    let entry_bytes = u64::from(own.capacity + signer.capacity);
    let p = &mut st.programs;
    p.code.insert(wasm_hash, Versioned(CodeRecord { wasm: WASM.to_vec(), exports }));
    p.interfaces.insert(interface_hash, Versioned(InterfaceRecord::new(interface.clone())));
    let blob = |deposit: u64, size: usize| Versioned(BlobRef { uploader: alice(), deposit, size: size as u32, refs: 1, uploaded_at: 1, last_used_at: 1 });
    p.blob_refs.insert(blob_key(BLOB_CODE, &wasm_hash), blob(300_000, WASM.len()));
    p.blob_refs.insert(blob_key(BLOB_INTERFACE, &interface_hash), blob(20_000, interface.len()));
    p.storage.insert(storage_key(&PROGRAM, KEY_OWN), Versioned(own));
    p.storage.insert(storage_key(&PROGRAM, KEY_SIGNER), Versioned(signer));
    let record = ProgramRecord {
        creator: alice(),
        created_at: 2,
        wasm_hash,
        interface_hash,
        version: 1,
        authority: None,
        upgrade_delay_blocks: 0,
        policy: 0,
        accepts_transfers: true,
        items: 2,
        bytes: entry_bytes,
        deposit: 3_000,
        sponsored: 2_000,
        record_deposit: 50_000,
        record_payer: alice(),
    };
    p.records.insert(program(), Versioned(record));
    st.accounts.insert(program(), Account { balance: 500, ..Default::default() });
    let escrow = 3_000 + 300_000 + 20_000 + 50_000;
    let m = st.programs.meta.as_mut().expect("on at genesis");
    m.programs = 1;
    m.state_bytes = (WASM.len() + interface.len()) as u64 + entry_bytes + RECORD_BYTES_FOR_TESTS;
    m.value_held = 500;
    m.sponsored = 2_000;
    m.escrow = escrow;
    m.votes.insert(validator(), 1);
    st.supply += 500 + escrow;
    st.commit_block().unwrap();
    st.check_invariants().unwrap();
    (st, Built { wasm_hash, interface_hash })
}

const RECORD_BYTES_FOR_TESTS: u64 = super::super::RECORD_BYTES;

fn all_records(st: &State) -> Vec<(u8, Vec<u8>, Vec<u8>)> {
    let mut v = vec![];
    st.for_each_record(|n, k, val| v.push((n, k.to_vec(), val.to_vec())));
    v
}

/// `Err`'s text from programs' own invariants (the line under test, before anything else in the
/// state's checks sees the change).
fn broken(st: &State) -> String {
    check_invariants(st).unwrap_err().to_string()
}

/// Every value starts with its format byte, 1; decoding refuses any other (0 and 2 named), and a
/// value with nothing after it.
#[test]
fn records_carry_format_1_and_refuse_any_other() {
    let (st, b) = populated();
    let p = &st.programs;
    let values: Vec<(&str, Vec<u8>)> = vec![
        ("program", encode(&p.records[&program()])),
        ("code", encode(&p.code[&b.wasm_hash])),
        ("interface", encode(&p.interfaces[&b.interface_hash])),
        ("entry", encode(&p.storage[&storage_key(&PROGRAM, KEY_OWN)])),
        ("blob", encode(&p.blob_refs[&blob_key(BLOB_CODE, &b.wasm_hash)])),
        ("meta", st.record(ns::META, meta::PROGRAMS).unwrap()),
    ];
    for (what, bytes) in values {
        assert_eq!(bytes[0], FORMAT, "{what}");
        let body = &bytes[1..];
        for format in [0u8, 2, 255] {
            let other = [&[format][..], body].concat();
            let e = match what {
                "program" => decode::<Versioned<ProgramRecord>>(&other).map(drop),
                "code" => decode::<Versioned<CodeRecord>>(&other).map(drop),
                "interface" => decode::<Versioned<InterfaceRecord>>(&other).map(drop),
                "entry" => decode::<Versioned<Entry>>(&other).map(drop),
                "blob" => decode::<Versioned<BlobRef>>(&other).map(drop),
                _ => decode::<Versioned<ProgramsMeta>>(&other).map(drop),
            };
            assert!(e.unwrap_err().to_string().contains(&format!("program record format {format}, this binary reads 1")), "{what} in format {format}");
        }
    }
    assert!(decode::<Versioned<BlobRef>>(&[]).is_err() && decode::<Versioned<BlobRef>>(&[1]).is_err(), "nothing, or a format byte alone");
    // Loading refuses them too, as records (the snapshot and the store load through it).
    let mut records = all_records(&st);
    let i = records.iter().position(|(n, _, _)| *n == ns::PROGRAM_BLOB_REFS).unwrap();
    records[i].2[0] = 2;
    assert!(State::from_records(records.into_iter().map(Ok)).err().unwrap().to_string().contains("program record format 2, this binary reads 1"));
}

/// Format 1's layouts are frozen with v2's other program rules (§9.2): field order is encoding
/// order. This pins each record's bytes; a change that moves them is a new format, never an edit.
#[test]
fn format_1_layouts_are_pinned() {
    let record = ProgramRecord {
        creator: "boo1c".into(),
        created_at: 2,
        wasm_hash: [1; 32],
        interface_hash: [2; 32],
        version: 1,
        authority: Some("boo1a".into()),
        upgrade_delay_blocks: 3,
        policy: 1,
        accepts_transfers: true,
        items: 4,
        bytes: 5,
        deposit: 6,
        sponsored: 7,
        record_deposit: 8,
        record_payer: "boo1p".into(),
    };
    let entry = Entry { value: vec![0xaa, 0xbb], payer: [3; 32], deposit: 9, capacity: 128 };
    let blob = BlobRef { uploader: "boo1u".into(), deposit: 10, size: 11, refs: 12, uploaded_at: 13, last_used_at: 14 };
    let meta = ProgramsMeta {
        params: BTreeMap::from([("program_value_cap".to_string(), 15)]),
        programs: 16,
        state_bytes: 17,
        value_held: 18,
        sponsored: 19,
        escrow: 20,
        fuel_rate: 21,
        pause: Some(Pause { from: 22, until: 23, started: 22, signers: vec!["boo1v".into()] }),
        votes: BTreeMap::from([("boo1v".to_string(), 24)]),
        pause_signed: BTreeMap::from([("boo1v".to_string(), 25)]),
    };
    let u64s = |v: &[u64]| v.iter().map(|x| hex::encode(x.to_le_bytes())).collect::<String>();
    let s = |t: &str| format!("{}{}", hex::encode((t.len() as u32).to_le_bytes()), hex::encode(t));
    let authority = format!("01{}", s("boo1a"));
    assert_eq!(
        hex::encode(encode(&Versioned(&record))),
        format!("01{}{}{}{}01000000{authority}{}0101{}{}", s("boo1c"), u64s(&[2]), "01".repeat(32), "02".repeat(32), u64s(&[3]), u64s(&[4, 5, 6, 7, 8]), s("boo1p"))
    );
    assert_eq!(hex::encode(encode(&Versioned(&CodeRecord { wasm: vec![0, 0x61], exports: vec!["get".into()] }))), format!("01{}0061{}{}", hex::encode(2u32.to_le_bytes()), hex::encode(1u32.to_le_bytes()), s("get")));
    assert_eq!(hex::encode(encode(&Versioned(&InterfaceRecord::new(vec![1])))), "010100000001");
    assert_eq!(hex::encode(encode(&Versioned(&entry))), format!("0102000000aabb{}{}80000000", "03".repeat(32), u64s(&[9])));
    assert_eq!(hex::encode(encode(&Versioned(&blob))), format!("01{}{}0b0000000c000000{}", s("boo1u"), u64s(&[10]), u64s(&[13, 14])));
    let map1 = |k: &str, v: u64| format!("01000000{}{}", s(k), u64s(&[v]));
    assert_eq!(
        hex::encode(encode(&Versioned(&meta))),
        format!("01{}{}01{}01000000{}{}{}", map1("program_value_cap", 15), u64s(&[16, 17, 18, 19, 20, 21]), u64s(&[22, 23, 22]), s("boo1v"), map1("boo1v", 24), map1("boo1v", 25))
    );
}

/// Without META `programs` there's nothing: no record in a program namespace, no META key, the
/// same record count and root as a state that never had the module (`state::tests::golden_encodings`
/// pins that root). A v2 chain born without programs is such a state.
#[test]
fn without_meta_there_are_no_program_records() {
    let empty = State::default();
    assert!(empty.programs.meta.is_none());
    let mut v2 = genesis();
    v2.features.clear();
    v2.check().unwrap();
    for st in [empty, v2.initial_state()] {
        let records = all_records(&st);
        assert!(records.iter().all(|(n, k, _)| !(66..=79).contains(n) && !(*n == ns::META && k == meta::PROGRAMS)));
        assert_eq!(escrow(&st), 0);
        st.check_invariants().unwrap();
    }
}

/// A state with program records survives `for_each_record` → `State::from_records` and a snapshot
/// (`write_state` → `read_state`) with the same root and records, META `programs` loaded back
/// (`load_meta`'s hand-written list), every map's records in its namespace.
#[test]
fn program_state_round_trips_through_records_and_a_snapshot() {
    let (st, _) = populated();
    let records = all_records(&st);
    for n in [ns::PROGRAMS, ns::PROGRAM_CODE, ns::PROGRAM_INTERFACES, ns::PROGRAM_STORAGE, ns::PROGRAM_BLOB_REFS] {
        assert!(records.iter().any(|(m, _, _)| *m == n), "namespace {n} has a record");
    }
    assert!(records.iter().any(|(n, k, _)| *n == ns::META && k == meta::PROGRAMS));
    assert_eq!(st.root_hash(), st.full_root());
    let back = State::from_records(records.iter().cloned().map(Ok)).unwrap();
    assert_eq!((back.root(), all_records(&back)), (st.root(), records.clone()));
    assert_eq!(back.programs.meta, st.programs.meta, "META programs loads back");
    back.check_invariants().unwrap();
    let mut bytes = vec![];
    crate::snapshot::write_state(&st, &mut bytes).unwrap();
    let snap = crate::snapshot::read_state(&mut bytes.as_slice()).unwrap();
    assert_eq!((snap.root(), all_records(&snap)), (st.root(), records));
    assert_eq!(snap.programs.meta, st.programs.meta);
    snap.check_invariants().unwrap();
    // A snapshot taken before programs switched on restores into a state that switches them on
    // later (§4.7): no program record, then META once `on_activate` runs.
    let mut v2 = genesis();
    v2.features.clear();
    let before = v2.initial_state();
    let mut bytes = vec![];
    crate::snapshot::write_state(&before, &mut bytes).unwrap();
    let mut restored = crate::snapshot::read_state(&mut bytes.as_slice()).unwrap();
    assert_eq!(restored.root(), before.root());
    let mut j = Journal::default();
    crate::upgrade::exec_activate_feature(&mut restored, &mut j, "programs-state-1", PROGRAMS_FEATURE, 1).unwrap();
    assert!(restored.programs.meta.is_some());
    restored.commit_block().unwrap();
    assert_eq!(restored.root_hash(), restored.full_root());
    assert_ne!(restored.root(), before.root());
}

/// `(height, fuel)` are journaled together: a rolled-back trial restores both. A first trial at a
/// new height that rolls back leaves the older height's run, which a reader at the new height
/// takes as empty: the counted fuel there is 0. Nested journals keep the oldest values.
#[test]
fn a_rolled_back_first_trial_leaves_the_counted_fuel_at_0() {
    let (mut st, _) = populated();
    st.programs.block = BlockRun { height: 5, fuel: 100, ..BlockRun::default() };
    // The first trial at height 6 resets the run, then rolls back.
    let mut j = Journal::default();
    block_mut(&mut st, &mut j, 6).fuel += 7;
    assert_eq!((st.programs.block.fuel_at(6), st.programs.block.fuel_at(5)), (7, 0));
    st.rollback(j);
    assert_eq!(st.programs.block, BlockRun { height: 5, fuel: 100, ..BlockRun::default() });
    assert_eq!(st.programs.block.fuel_at(6), 0, "the counted fuel at the new height");
    // One that sticks, then a trial at the same height that rolls back: back to what stuck.
    let mut j = Journal::default();
    block_mut(&mut st, &mut j, 6).fuel += 7;
    let mut later = Journal::default();
    block_mut(&mut st, &mut later, 6).fuel += 1_000;
    assert_eq!(st.programs.block.fuel_at(6), 1_007);
    st.rollback(later);
    assert_eq!(st.programs.block, BlockRun { height: 6, fuel: 7, ..BlockRun::default() });
    // A nested journal folded into its parent: rolling the parent back undoes both.
    let mut nested = Journal::default();
    block_mut(&mut st, &mut nested, 6).fuel += 3;
    j.absorb_pub(nested);
    st.rollback(j);
    assert_eq!(st.programs.block, BlockRun { height: 5, fuel: 100, ..BlockRun::default() });
    // The run is no record: nothing to commit, the root unmoved.
    let root = st.root();
    assert!(st.commit_block().unwrap().records.is_empty() && st.root() == root);
}

/// Every table and META roll back with the action journal, dirty marks included, so a rolled-back
/// trial leaves nothing to commit. A rolled-back first activation leaves no META.
#[test]
fn program_records_roll_back_with_the_journal() {
    let (mut st, b) = populated();
    let root = st.root();
    let mut j = Journal::default();
    let p = &mut st.programs;
    j.programs.records.touch(&p.records, &program());
    p.records.get_mut(&program()).unwrap().items += 1;
    j.programs.code.touch(&p.code, &[1; 32]);
    p.code.insert([1; 32], Versioned(CodeRecord { wasm: vec![], exports: vec![] }));
    j.programs.interfaces.touch(&p.interfaces, &b.interface_hash);
    p.interfaces.remove(&b.interface_hash);
    let key = storage_key(&PROGRAM, KEY_OWN);
    j.programs.storage.touch(&p.storage, &key);
    p.storage.remove(&key);
    let blob = blob_key(BLOB_CODE, &b.wasm_hash);
    j.programs.blob_refs.touch(&p.blob_refs, &blob);
    p.blob_refs.get_mut(&blob).unwrap().refs += 1;
    meta_mut(&mut st, &mut j).unwrap().escrow += 1;
    j.programs.opened.insert(program());
    st.rollback(j);
    assert!(st.commit_block().unwrap().records.is_empty(), "nothing dirty");
    assert_eq!(st.root(), root);
    st.check_invariants().unwrap();
    // A first activation, rolled back: no META, and nothing to commit.
    let mut v2 = genesis();
    v2.features.clear();
    let mut st = v2.initial_state();
    let root = st.root();
    let mut j = Journal::default();
    crate::upgrade::exec_activate_feature(&mut st, &mut j, "programs-state-1", PROGRAMS_FEATURE, 1).unwrap();
    assert!(st.programs.meta.is_some());
    st.rollback(j);
    assert!(st.programs.meta.is_none() && !crate::upgrade::is_active(&st, PROGRAMS_FEATURE));
    st.commit_block().unwrap();
    assert_eq!(st.root(), root);
}

/// A nested journal folded into its parent (`Journal::absorb`, as `apply_j` folds an action's
/// journal into its transaction's, which a proposer's trial may then roll back): when the nested
/// one was the first to touch program state, rolling the parent back undoes what it recorded (the
/// block run, META, a record); when both touched it, the parent's older values win. `opened` is
/// the union of both.
#[test]
fn nested_journals_fold_into_their_parent() {
    let (mut st, _) = populated();
    st.programs.block = BlockRun { height: 5, fuel: 100, ..BlockRun::default() };
    let as_it_was = |st: &State| (st.programs.block.clone(), st.programs.meta.clone(), st.programs.records.get(&program()).cloned());
    let (before, root) = (as_it_was(&st), st.root());
    // What a commit at block 6 might write: the block's fuel, META's escrow, a program's record.
    let write = |st: &mut State, j: &mut Journal, fuel: u64| {
        block_mut(st, j, 6).fuel += fuel;
        meta_mut(st, j).unwrap().escrow += 1;
        j.programs.records.touch(&st.programs.records, &program());
        st.programs.records.get_mut(&program()).unwrap().items += 1;
    };
    // The parent has touched nothing of programs'; the nested journal is the first.
    let mut j = Journal::default();
    let mut nested = Journal::default();
    write(&mut st, &mut nested, 3);
    nested.programs.opened.insert(alice());
    j.absorb_pub(nested);
    assert_eq!(j.programs.opened, BTreeSet::from([alice()]));
    st.rollback(j);
    assert_eq!(as_it_was(&st), before, "the parent's rollback undid the nested journal's writes");
    assert!(st.commit_block().unwrap().records.is_empty() && st.root() == root, "nothing dirty");
    // Both touched them, the parent first: its values are the older ones, and they win.
    let mut j = Journal::default();
    write(&mut st, &mut j, 3);
    j.programs.opened.insert(alice());
    let mut nested = Journal::default();
    write(&mut st, &mut nested, 4);
    nested.programs.opened.insert(program());
    assert_eq!(st.programs.block, BlockRun { height: 6, fuel: 7, ..BlockRun::default() });
    j.absorb_pub(nested);
    assert_eq!(j.programs.opened, BTreeSet::from([alice(), program()]));
    st.rollback(j);
    assert_eq!(as_it_was(&st), before, "back to before the parent's first write");
    assert!(st.commit_block().unwrap().records.is_empty() && st.root() == root, "nothing dirty");
}

/// A node fault is sticky for the life of its `State` value: into clones, and through a rollback
/// (it's never journaled, §6.2).
#[test]
fn a_fault_is_never_journaled() {
    let (mut st, _) = populated();
    let mut j = Journal::default();
    meta_mut(&mut st, &mut j).unwrap().fuel_rate += 1;
    st.programs.fault = Some(Fault { height: 3, tx: "ab".repeat(32), class: FaultClass::Tx, what: "an injected fault".into() });
    let copy = st.clone();
    st.rollback(j);
    assert_eq!(st.programs.fault.as_ref().map(|f| f.class), Some(FaultClass::Tx));
    assert_eq!(copy.programs.fault, st.programs.fault);
    assert_eq!(st.programs.fault.take().map(|f| f.height), Some(3), "only a proposer's trial takes it");
    // A setup fault (a thread or an engine this node couldn't make) is just as sticky.
    st.programs.fault = Some(Fault { height: 4, tx: "cd".repeat(32), class: FaultClass::Setup, what: "no thread".into() });
    assert_eq!(st.clone().programs.fault.map(|f| f.class), Some(FaultClass::Setup));
}

/// The state's checks call programs': a broken program total fails `State::check_invariants`, and
/// the supply counts programs' escrow (a deposit not in the supply is caught there).
#[test]
fn the_state_checks_programs_too() {
    let (mut st, _) = populated();
    st.programs.meta.as_mut().unwrap().programs = 2;
    assert!(st.check_invariants().unwrap_err().to_string().contains("META counts 2 programs"));
    let (mut st, _) = populated();
    st.supply -= 1;
    assert!(st.check_invariants().unwrap_err().to_string().contains("balances and escrows hold"));
    assert_eq!(escrow(&st), 373_000);
}

// ---- §4.4, one test per line ----

/// META absent ⇒ every program table is empty and the `programs` feature is off.
#[test]
fn invariant_meta_absent_means_no_program_state() {
    let (st, b) = populated();
    let without_meta = |f: &dyn Fn(&mut State)| {
        let mut st = State { programs: Programs::default(), ..st.clone() };
        crate::upgrade::exec_deactivate_feature(&mut st, &mut Journal::default(), PROGRAMS_FEATURE).unwrap();
        f(&mut st);
        st
    };
    check_invariants(&without_meta(&|_| {})).unwrap();
    let rec = st.programs.records[&program()].clone();
    let wasm_hash = b.wasm_hash;
    fn case(f: impl Fn(&mut State) + 'static) -> Box<dyn Fn(&mut State)> {
        Box::new(f)
    }
    let cases = [
        ("a record", case(move |s| drop(s.programs.records.insert(program(), rec.clone())))),
        ("code", case(|s| drop(s.programs.code.insert([1; 32], Versioned(CodeRecord { wasm: vec![], exports: vec![] }))))),
        ("an interface", case(|s| drop(s.programs.interfaces.insert([1; 32], Versioned(InterfaceRecord::new(vec![])))))),
        ("an entry", case(|s| drop(s.programs.storage.insert(storage_key(&PROGRAM, b"k"), Versioned(Entry { value: vec![], payer: PROGRAM, deposit: 0, capacity: 128 }))))),
        ("a blob reference", case(move |s| drop(s.programs.blob_refs.insert(blob_key(BLOB_CODE, &wasm_hash), Versioned(BlobRef { uploader: alice(), deposit: 0, size: 0, refs: 0, uploaded_at: 0, last_used_at: 0 }))))),
    ];
    for (what, f) in cases {
        assert_eq!(broken(&without_meta(&*f)), "program records exist without META programs", "{what}");
    }
    let mut on = without_meta(&|_| {});
    crate::upgrade::exec_activate_feature(&mut on, &mut Journal::default(), "programs-state-1", PROGRAMS_FEATURE, 1).unwrap();
    on.programs.meta = None;
    assert_eq!(broken(&on), "the programs feature is on without META programs");
}

/// META present ⇒ the version the next block runs is ≥ 2 (and the feature on ⇒ META present, the
/// line above).
#[test]
fn invariant_meta_needs_v2_next_block() {
    let (mut st, _) = populated();
    crate::upgrade::exec_deactivate_feature(&mut st, &mut Journal::default(), PROGRAMS_FEATURE).unwrap();
    check_invariants(&st).unwrap();
    // v2 only from block 5, the next block being 1: META couldn't exist yet.
    st.upgrade.schedule = vec![ScheduledUpgrade { version: 2, activation_height: 5 }];
    assert_eq!(broken(&st), "META programs exists under protocol v1");
    st.econ.height = 4;
    check_invariants(&st).unwrap();
}

/// `escrow` = Σ entry deposits + Σ blob deposits + Σ `record_deposit`.
#[test]
fn invariant_escrow_is_every_deposit() {
    let (st, b) = populated();
    let check = |f: &dyn Fn(&mut State)| {
        let mut st = st.clone();
        f(&mut st);
        broken(&st)
    };
    let e = "programs' escrow is 373000, their deposits add up to";
    // An entry's deposit (its program's `deposit` moved with it, so only the escrow can see it).
    let entry = |s: &mut State| {
        s.programs.storage.get_mut(&storage_key(&PROGRAM, KEY_SIGNER)).unwrap().deposit += 1;
        s.programs.records.get_mut(&program()).unwrap().deposit += 1;
    };
    assert_eq!(check(&entry), format!("{e} 373001"));
    assert_eq!(check(&|s| s.programs.blob_refs.get_mut(&blob_key(BLOB_INTERFACE, &b.interface_hash)).unwrap().deposit += 1), format!("{e} 373001"));
    assert_eq!(check(&|s| s.programs.records.get_mut(&program()).unwrap().record_deposit -= 1), format!("{e} 372999"));
    assert_eq!(check(&|s| s.programs.meta.as_mut().unwrap().escrow += 1), "programs' escrow is 373001, their deposits add up to 373000");
}

/// `value_held` = Σ balances of program accounts; `programs` = the number of records; `sponsored`
/// = Σ records' `sponsored`, and each record's `sponsored` = Σ the deposits of its entries whose
/// payer is that program (so ≤ its `deposit`).
#[test]
fn invariant_value_held_programs_and_sponsored() {
    let (st, _) = populated();
    let check = |f: &dyn Fn(&mut State)| {
        let mut st = st.clone();
        f(&mut st);
        broken(&st)
    };
    assert_eq!(check(&|s| s.accounts.get_mut(&program()).unwrap().balance += 1), "value_held is 500, program accounts hold 501");
    assert_eq!(check(&|s| s.programs.meta.as_mut().unwrap().value_held = 0), "value_held is 0, program accounts hold 500");
    assert_eq!(check(&|s| s.programs.meta.as_mut().unwrap().programs = 0), "META counts 0 programs, 1 records exist");
    // META's sponsored against the records', and a record's against its own entries.
    assert_eq!(check(&|s| s.programs.meta.as_mut().unwrap().sponsored = 2_001), "META's sponsored is 2001, the records' add up to 2000");
    assert!(check(&|s| s.programs.records.get_mut(&program()).unwrap().sponsored = 0).ends_with("'s sponsored is 0, its own entries' deposits 2000"));
    // An entry the signer paid isn't sponsored: paying it from the program makes the record's
    // `sponsored` short.
    let program_paid = |s: &mut State| s.programs.storage.get_mut(&storage_key(&PROGRAM, KEY_SIGNER)).unwrap().payer = PROGRAM;
    assert!(check(&program_paid).ends_with("'s sponsored is 2000, its own entries' deposits 3000"));
}

/// `state_bytes` = Σ code sizes + Σ interface sizes + Σ entry capacities + `RECORD_BYTES` ×
/// records, and ≤ 2 × `MAX_STATE_SOFT_BYTES`.
#[test]
fn invariant_state_bytes() {
    let (st, b) = populated();
    let held = st.programs.meta.as_ref().unwrap().state_bytes;
    let entries = u64::from(capacity(KEY_OWN.len(), 100) + capacity(KEY_SIGNER.len(), 8));
    assert_eq!((entries, held), (256 + 128, (WASM.len() + interface_bytes().len()) as u64 + entries + 256));
    let check = |f: &dyn Fn(&mut State)| {
        let mut st = st.clone();
        f(&mut st);
        broken(&st)
    };
    assert_eq!(check(&|s| s.programs.meta.as_mut().unwrap().state_bytes -= 1), format!("state_bytes is {}, program state holds {held}", held - 1));
    // A blob's size counts (its reference agrees, so only the sum sees it).
    let grown = check(&|s| {
        s.programs.interfaces.get_mut(&b.interface_hash).unwrap().bytes.push(0);
        s.programs.blob_refs.get_mut(&blob_key(BLOB_INTERFACE, &b.interface_hash)).unwrap().size += 1;
    });
    assert_eq!(grown, format!("state_bytes is {held}, program state holds {}", held + 1));
    let bound = 2 * super::super::params::MAX_STATE_SOFT_BYTES;
    assert_eq!(check(&|s| s.programs.meta.as_mut().unwrap().state_bytes = bound + 1), format!("program state holds {} bytes, past twice the soft cap's bound", bound + 1));
}

/// Each program's `items`, `bytes` and `deposit` match its entries; each entry's program exists;
/// each entry's capacity covers its rounded size (at least `storage::capacity` of its key and
/// value, a whole number of units, at most the largest) and its value is at most 16 KiB.
#[test]
fn invariant_entries_and_their_program() {
    let (st, _) = populated();
    let check = |f: &dyn Fn(&mut State)| {
        let mut st = st.clone();
        f(&mut st);
        broken(&st)
    };
    let totals = "'s storage totals drifted from its entries";
    assert!(check(&|s| s.programs.records.get_mut(&program()).unwrap().items += 1).ends_with(totals));
    assert!(check(&|s| s.programs.records.get_mut(&program()).unwrap().bytes -= 64).ends_with(totals));
    assert!(check(&|s| s.programs.records.get_mut(&program()).unwrap().deposit += 1).ends_with(totals));
    let orphan = Versioned(Entry { value: vec![], payer: PROGRAM, deposit: 0, capacity: 128 });
    let e = check(&|s| drop(s.programs.storage.insert(storage_key(&[8; 32], b"k"), orphan.clone())));
    assert_eq!(e, format!("a storage entry of {} has no program", render_address("boo", &[8; 32])));
    assert!(check(&|s| drop(s.programs.storage.insert(ByteKey(PROGRAM.to_vec()), orphan.clone()))).contains("has no key after the program's address"));
    // Capacity: below the rounded size, not a whole unit, past the largest. An overwrite that fits
    // keeps the larger capacity it was bonded for (W1's overlay), so above the rounded size is fine.
    let entry = |s: &mut State| s.programs.storage.get_mut(&storage_key(&PROGRAM, KEY_SIGNER)).unwrap().0.clone();
    for (capacity, ok) in [(64, false), (127, false), (128, true), (129, false), (192, true), (MAX_CAPACITY, true), (MAX_CAPACITY + 64, false)] {
        let mut s = st.clone();
        let mut e = entry(&mut s);
        let moved = |total: &mut u64| *total = (*total as i64 + i64::from(capacity) - i64::from(e.capacity)) as u64;
        // Keep the totals in step, so only the capacity rule can fail.
        moved(&mut s.programs.records.get_mut(&program()).unwrap().bytes);
        moved(&mut s.programs.meta.as_mut().unwrap().state_bytes);
        e.capacity = capacity;
        s.programs.storage.insert(storage_key(&PROGRAM, KEY_SIGNER), Versioned(e));
        assert_eq!(check_invariants(&s).is_ok(), ok, "capacity {capacity}: {:?}", check_invariants(&s).err());
        if !ok {
            assert!(broken(&s).contains(&format!("has capacity {capacity} for a rounded size of 128")));
        }
    }
    let e = check(&|s| s.programs.storage.get_mut(&storage_key(&PROGRAM, KEY_SIGNER)).unwrap().value = vec![0; MAX_VALUE_BYTES + 1]);
    assert!(e.ends_with(&format!("holds {} bytes (max {MAX_VALUE_BYTES})", MAX_VALUE_BYTES + 1)), "{e}");
}

/// Each record's blobs exist; each `BlobRef.refs` is the number of records citing it; its `size` is
/// its blob's length; each `CodeRecord.exports` matches its code (and every blob has its reference).
#[test]
fn invariant_blobs_and_their_references() {
    let (st, b) = populated();
    let check = |f: &dyn Fn(&mut State)| {
        let mut st = st.clone();
        f(&mut st);
        broken(&st)
    };
    let code_ref = blob_key(BLOB_CODE, &b.wasm_hash);
    assert!(check(&|s| s.programs.records.get_mut(&program()).unwrap().interface_hash = [1; 32]).ends_with("cites a blob that isn't stored"));
    assert_eq!(check(&|s| s.programs.blob_refs.get_mut(&code_ref).unwrap().refs = 2), format!("blob reference {} counts 2 programs, 1 cite it", hex::encode(&code_ref.0)));
    assert_eq!(check(&|s| s.programs.blob_refs.get_mut(&code_ref).unwrap().size += 1), format!("blob reference {} doesn't match a stored blob of its size", hex::encode(&code_ref.0)));
    assert!(check(&|s| s.programs.code.get_mut(&b.wasm_hash).unwrap().exports.push("extra".into())).ends_with("'s exports aren't its code's"));
    assert!(check(&|s| s.programs.code.get_mut(&b.wasm_hash).unwrap().exports.clear()).ends_with("'s exports aren't its code's"));
    // A blob without its reference, and a reference without its blob or under a bad key.
    assert!(check(&|s| drop(s.programs.blob_refs.remove(&code_ref))).ends_with("has no blob reference"));
    let stray = Versioned(BlobRef { uploader: alice(), deposit: 0, size: 0, refs: 0, uploaded_at: 0, last_used_at: 0 });
    assert!(check(&|s| drop(s.programs.blob_refs.insert(blob_key(BLOB_INTERFACE, &[1; 32]), stray.clone()))).ends_with("doesn't match a stored blob of its size"));
    assert!(check(&|s| drop(s.programs.blob_refs.insert(blob_key(2, &[1; 32]), stray.clone()))).starts_with("a blob reference is misfiled"));
    assert!(check(&|s| drop(s.programs.blob_refs.insert(ByteKey(vec![BLOB_CODE; 5]), stray.clone()))).starts_with("a blob reference is misfiled"));
}

/// Program accounts have no keys, no locks and nonce 0, and every record has an account.
#[test]
fn invariant_program_accounts() {
    let (st, _) = populated();
    let check = |f: &dyn Fn(&mut Account)| {
        let mut st = st.clone();
        f(st.accounts.get_mut(&program()).unwrap());
        broken(&st)
    };
    let e = format!("program {}'s account has keys, locks or a nonce", program());
    assert_eq!(check(&|a| a.nonce = 1), e);
    assert_eq!(check(&|a| drop(a.keys.insert("ab".repeat(32), crate::chain::KeyRecord { label: "k".into(), role: crate::types::KeyRole::Owner, added_at: 0, spent: 0, window_start: 0 }))), e);
    let lock = crate::chain::Lock { id: "l".into(), amount: crate::chain::LockAmount::Until { amount: 1, until: 9 }, reason: "r".into() };
    assert_eq!(check(&|a| a.locks.push(lock.clone())), e);
    let mut st = st.clone();
    st.accounts.remove(&program());
    assert_eq!(broken(&st), format!("program {} has no account", program()));
    // And a record keyed by anything but one of this chain's addresses.
    let (mut st, _) = populated();
    let rec = st.programs.records.remove(&program()).unwrap();
    st.programs.records.insert(render_address("tboo", &PROGRAM), rec);
    assert!(broken(&st).ends_with("isn't an address of this chain"));
}

/// `params` has only known keys, each within its bounds; `program_max_tx_fuel ≤ program_max_block_fuel`.
#[test]
fn invariant_params_known_and_within_bounds() {
    let (st, _) = populated();
    let check = |key: &str, value: u64| {
        let mut st = st.clone();
        st.programs.meta.as_mut().unwrap().params.insert(key.into(), value);
        check_invariants(&st).map_err(|e| e.to_string())
    };
    assert!(check("program_warp", 1).unwrap_err().starts_with("unknown parameter `program_warp`"));
    assert_eq!(check("program_fuel_floor_rate", 0).unwrap_err(), "parameter `program_fuel_floor_rate` must be 1–1000000, got 0");
    assert_eq!(check("program_max_block_fuel", 60_000_001).unwrap_err(), "parameter `program_max_block_fuel` must be 20000000–60000000, got 60000001");
    check("program_max_tx_fuel", 20_000_000).unwrap();
    // A map without a key reads its default: still valid.
    let mut st = st.clone();
    st.programs.meta.as_mut().unwrap().params.clear();
    check_invariants(&st).unwrap();
}

/// `fuel_rate` in [1, `MAX_FUEL_RATE`]: the bound, not the floor parameter, which can move above it.
#[test]
fn invariant_fuel_rate_within_its_bound() {
    let (st, _) = populated();
    let check = |rate: u64| {
        let mut st = st.clone();
        st.programs.meta.as_mut().unwrap().fuel_rate = rate;
        check_invariants(&st).map_err(|e| e.to_string())
    };
    let max = super::super::MAX_FUEL_RATE;
    assert_eq!(check(0).unwrap_err(), format!("the fuel rate 0 is outside [1, {max}]"));
    assert_eq!(check(max + 1).unwrap_err(), format!("the fuel rate {} is outside [1, {max}]", max + 1));
    check(1).unwrap();
    check(max).unwrap();
    // Below the floor parameter is still valid: readers take max(fuel_rate, floor).
    let mut st = st.clone();
    st.programs.meta.as_mut().unwrap().params.insert("program_fuel_floor_rate".into(), 1_000);
    check_invariants(&st).unwrap();
}

/// `pause`: `from < until` (a run starts where its first pause did, and is signed by distinct
/// validators); `votes` and `pause_signed` name validators, at heights at or below the one the
/// state's next transaction runs at.
#[test]
fn invariant_pause_votes_and_signatures() {
    let (st, _) = populated();
    let pause = |from, until, started, signers: Vec<String>| {
        let mut st = st.clone();
        st.programs.meta.as_mut().unwrap().pause = Some(Pause { from, until, started, signers });
        check_invariants(&st).map_err(|e| e.to_string())
    };
    pause(2, 10, 2, vec![validator()]).unwrap();
    pause(2, 10, 1, vec![validator()]).unwrap();
    assert_eq!(pause(5, 5, 5, vec![validator()]).unwrap_err(), "the programs pause runs from 5 to 5 (started 5)");
    assert_eq!(pause(5, 9, 6, vec![validator()]).unwrap_err(), "the programs pause runs from 5 to 9 (started 6)");
    for signers in [vec![], vec![alice()], vec![validator(), validator()]] {
        assert_eq!(pause(2, 10, 2, signers.clone()).unwrap_err(), "the programs pause's signers aren't distinct validators", "{signers:?}");
    }
    let now = crate::econ::now(&st);
    let mark = |votes: bool, who: String, at: u64| {
        let mut st = st.clone();
        let m = st.programs.meta.as_mut().unwrap();
        (if votes { &mut m.votes } else { &mut m.pause_signed }).insert(who, at);
        check_invariants(&st).map_err(|e| e.to_string())
    };
    for votes in [true, false] {
        let what = if votes { "vote" } else { "signature" };
        mark(votes, validator(), now).unwrap();
        assert_eq!(mark(votes, validator(), now + 1).unwrap_err(), format!("a programs pause {what} by {} at block {} (a validator, at block {now} or before)", validator(), now + 1));
        assert!(mark(votes, alice(), 0).unwrap_err().starts_with(&format!("a programs pause {what} by {}", alice())));
    }
}

/// `check_bounds` (part of `State::check_bounds`) holds every program record to the caps that keep
/// it within a snapshot's record limits.
#[test]
fn program_records_stay_within_their_caps() {
    let (st, b) = populated();
    check_bounds(&st).unwrap();
    let check = |f: &dyn Fn(&mut State)| {
        let mut st = st.clone();
        f(&mut st);
        check_bounds(&st).map_err(|e| e.to_string())
    };
    check(&|s| s.programs.code.get_mut(&b.wasm_hash).unwrap().wasm = vec![0; MAX_CODE_BYTES]).unwrap();
    assert!(check(&|s| s.programs.code.get_mut(&b.wasm_hash).unwrap().wasm = vec![0; MAX_CODE_BYTES + 1]).is_err());
    assert!(check(&|s| s.programs.code.get_mut(&b.wasm_hash).unwrap().exports = vec!["m".into(); MAX_METHODS + 1]).is_err());
    assert!(check(&|s| s.programs.code.get_mut(&b.wasm_hash).unwrap().exports.push("Not_an_ident".into())).is_err());
    assert!(check(&|s| s.programs.interfaces.get_mut(&b.interface_hash).unwrap().bytes = vec![0; MAX_INTERFACE_BYTES + 1]).is_err());
    let entry = Versioned(Entry { value: vec![], payer: PROGRAM, deposit: 0, capacity: 0 });
    check(&|s| drop(s.programs.storage.insert(storage_key(&PROGRAM, &[1; MAX_KEY_BYTES]), entry.clone()))).unwrap();
    assert!(check(&|s| drop(s.programs.storage.insert(storage_key(&PROGRAM, &[1; MAX_KEY_BYTES + 1]), entry.clone()))).is_err());
    assert!(check(&|s| drop(s.programs.storage.insert(storage_key(&PROGRAM, &[]), entry.clone()))).is_err());
    assert!(check(&|s| s.programs.storage.get_mut(&storage_key(&PROGRAM, KEY_OWN)).unwrap().value = vec![0; MAX_VALUE_BYTES + 1]).is_err());
    assert!(check(&|s| drop(s.programs.blob_refs.insert(ByteKey(vec![0; 34]), Versioned(BlobRef { uploader: alice(), deposit: 0, size: 0, refs: 0, uploaded_at: 0, last_used_at: 0 })))).is_err());
}

/// The keys the maps use: an entry's is its program's bytes then its key (so a program's entries
/// are one range), a blob reference's is its kind then its hash.
#[test]
fn storage_and_blob_keys() {
    assert_eq!(storage_key(&[7; 32], b"ab").key_bytes().as_ref(), [[7u8; 32].as_slice(), b"ab"].concat().as_slice());
    assert_eq!(blob_key(BLOB_INTERFACE, &[9; 32]).0, [&[1u8][..], &[9u8; 32][..]].concat());
    assert!(blob_key(BLOB_CODE, &[0xff; 32]) < blob_key(BLOB_INTERFACE, &[0; 32]), "every code reference sorts before every interface's");
}
