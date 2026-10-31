//! Receipts (docs/PROGRAMS-V0.md §8.1; WBS S4b's acceptance): their numbers and JSON, what each
//! program transaction's receipt says (applied, failed, a nested out-of-fuel, the `native`
//! override), the claims budget (unit, and the same cut on two nodes), a v1 block's record byte
//! for byte without a `programs` key, and receipts rebuilt from the logs after the database is
//! gone.

use super::super::probe::*;
use super::*;
use crate::chain::Chain;
use crate::store;
use crate::types::{Action, Block, InitCall};
use blockula_vm::engine::TrapKind;
use blockula_vm::validate::{Rule, ValidateError};

/// Two of W1's guests: the counter (`increment` bonds an entry, then overwrites it) and the token
/// (whose `init` aborts on an input of the wrong length).
const COUNTER: (&[u8], &[u8]) = (include_bytes!("../../../vm/tests/programs/counter.wasm"), include_bytes!("../../../vm/tests/programs/counter.iface"));
const TOKEN: (&[u8], &[u8]) = (include_bytes!("../../../vm/tests/programs/token.wasm"), include_bytes!("../../../vm/tests/programs/token.iface"));
/// The escrow (`open` takes a deal's id and seller and the buyer's FANG; `release` pays the seller).
const ESCROW: (&[u8], &[u8]) = (include_bytes!("../../../vm/tests/programs/escrow.wasm"), include_bytes!("../../../vm/tests/programs/escrow.iface"));

/// A receipt with no claims and default facts, for the budget's unit test.
fn bare() -> ProgramReceipt {
    ProgramReceipt {
        version: RECEIPT_VERSION,
        kind: ReceiptKind::Call,
        outcome: ReceiptOutcome::Ok,
        fuel: FuelUse::default(),
        fee: FeeParts::default(),
        calls: vec![],
        events: vec![],
        omitted: None,
        balance_changes: vec![],
        storage: StorageSummary::default(),
        outside_vm: OutsideVm::default(),
        burned_by_programs: 0,
        created: None,
        upload: None,
    }
}

/// A receipt whose claims are `returns` (each a call's return data length) and `events` (each an
/// event's data length, named `e`).
fn claiming(returns: &[usize], events: &[usize]) -> ProgramReceipt {
    let calls = returns.iter().map(|n| CallResult { program: "boo1p".into(), method: "ret".into(), value: 0, return_data: HexBytes(vec![7; *n]) }).collect();
    let events = events.iter().map(|n| ReceiptEvent { program: "boo1p".into(), name: "e".into(), data: HexBytes(vec![9; *n]) }).collect();
    ProgramReceipt { calls, events, ..bare() }
}

/// §8.1's table, pinned: every outcome's number, code and JSON, read back exactly; a failure's
/// JSON that doesn't hold together is refused. Trap kinds and validation rules carry W1's numbers
/// and names, and a refused upload never its validator's message.
#[test]
fn outcomes_keep_their_numbers_codes_and_json() {
    let trap = Numbered::of(TrapKind::OutOfFuel.number(), TrapKind::OutOfFuel);
    let float = Numbered::of(Rule::Float.number(), Rule::Float);
    let cases = [
        (ReceiptOutcome::Ok, 0, "ok", r#"{"ok":true}"#),
        (ReceiptOutcome::Trapped { program: "boo1p".into(), trap: trap.clone() }, 1, "trapped", r#"{"ok":false,"number":1,"code":"trapped","program":"boo1p","trap":{"number":1,"name":"out_of_fuel"}}"#),
        (ReceiptOutcome::Aborted { program: "boo1p".into(), abort_code: 7, untrusted_message: "e".into() }, 2, "aborted", r#"{"ok":false,"number":2,"code":"aborted","program":"boo1p","abort_code":7,"untrusted_message":"e"}"#),
        (ReceiptOutcome::Commit { commit_error: 2 }, 3, "commit", r#"{"ok":false,"number":3,"code":"commit","commit_error":2}"#),
        (ReceiptOutcome::ValueCap { cap: 5, control: u128::from(u64::MAX) + 1 }, 10, "value_cap", r#"{"ok":false,"number":10,"code":"value_cap","cap":"5","control":"18446744073709551616"}"#),
        (ReceiptOutcome::ExitOnly, 11, "exit_only", r#"{"ok":false,"number":11,"code":"exit_only"}"#),
        (ReceiptOutcome::StorageLimit, 12, "storage_limit", r#"{"ok":false,"number":12,"code":"storage_limit"}"#),
        (ReceiptOutcome::StateFull, 13, "state_full", r#"{"ok":false,"number":13,"code":"state_full"}"#),
        (ReceiptOutcome::InvalidCode { rule: float.clone(), func: Some(0), offset: Some(48) }, 14, "invalid_code", r#"{"ok":false,"number":14,"code":"invalid_code","rule":{"number":4,"name":"float"},"func":0,"offset":48}"#),
        (ReceiptOutcome::InvalidCode { rule: float.clone(), func: None, offset: None }, 14, "invalid_code", r#"{"ok":false,"number":14,"code":"invalid_code","rule":{"number":4,"name":"float"}}"#),
        (ReceiptOutcome::TranslationRefused, 15, "translation_refused", r#"{"ok":false,"number":15,"code":"translation_refused"}"#),
        (ReceiptOutcome::InterfaceMismatch, 16, "interface_mismatch", r#"{"ok":false,"number":16,"code":"interface_mismatch"}"#),
        (ReceiptOutcome::Native, 20, "native", r#"{"ok":false,"number":20,"code":"native"}"#),
    ];
    for (outcome, number, code, json) in cases {
        assert_eq!((outcome.number(), outcome.code()), (number, code));
        assert_eq!(serde_json::to_string(&outcome).unwrap(), json);
        assert_eq!(serde_json::from_str::<ReceiptOutcome>(json).unwrap(), outcome, "{json}");
    }
    for bad in [
        r#"{"ok":false,"number":2,"code":"trapped","program":"boo1p","trap":{"number":1,"name":"out_of_fuel"}}"#,
        r#"{"ok":false,"number":1,"code":"trapped","program":"boo1p"}"#,
        r#"{"ok":false,"number":10,"code":"value_cap","cap":"5","control":"x"}"#,
        r#"{"ok":false,"number":9,"code":"oops"}"#,
        r#"{"ok":false}"#,
    ] {
        assert!(serde_json::from_str::<ReceiptOutcome>(bad).is_err(), "{bad}");
    }
    // Spot checks of W1's numbers and names (`every_trap_kind_and_validation_rule_keeps_its_number_and_name_in_receipts` has them all).
    let named = |n: Numbered| (n.number, n.name);
    assert_eq!(named(Numbered::of(TrapKind::VersionMismatch.number(), TrapKind::VersionMismatch)), (18, "version_mismatch".into()));
    assert_eq!(named(Numbered::of(TrapKind::EventBytes.number(), TrapKind::EventBytes)), (30, "event_bytes".into()));
    assert_eq!(named(Numbered::of(Rule::MemoryGrow.number(), Rule::MemoryGrow)), (7, "memory_grow".into()));
    assert_eq!(named(Numbered::of(Rule::CustomSection.number(), Rule::CustomSection)), (14, "custom_section".into()));
    // A failure's outcome: the VM's numbers, the node's, and never a validator's message.
    let refused = Failure::InvalidCode(ValidateError { rule: Rule::Float, func: Some(3), offset: Some(48), message: "a float in function 3".into() });
    let outcome = ReceiptOutcome::failed(&refused, "boo");
    assert_eq!(outcome, ReceiptOutcome::InvalidCode { rule: float, func: Some(3), offset: Some(48) });
    assert!(!serde_json::to_string(&outcome).unwrap().contains("a float"));
    let p = [5u8; 32];
    let trapped = Failure::Vm(FailCode::Trapped { program: p, kind: TrapKind::OutOfFuel });
    assert_eq!(ReceiptOutcome::failed(&trapped, "boo"), ReceiptOutcome::Trapped { program: crypto::render_address("boo", &p), trap });
    assert_eq!(trapped.reason("boo"), format!("program {} trapped: out of fuel (trap 1)", short(&crypto::render_address("boo", &p))));
    assert_eq!(ReceiptOutcome::failed(&Failure::Native("insufficient balance".into()), "boo"), ReceiptOutcome::Native);
    assert_eq!(ReceiptOutcome::failed(&Failure::StorageLimit { deposits: 9, limit: 1 }, "boo").number(), 12);
    assert_eq!(ReceiptOutcome::failed(&Failure::Vm(FailCode::Commit(CommitError::Debited)), "boo"), ReceiptOutcome::Commit { commit_error: 3 });
}

/// Every trap kind and every validation rule as a receipt prints it, `{number, name}` (§8.1): the
/// tables below name each variant with the number and the name its receipt JSON carries. W1 pins
/// the numbers (`exec::tests`, `validate::tests::rules_keep_their_numbers`) but not the names,
/// which a receipt takes from the variants' `Debug` text in snake case, so renaming a variant in
/// node/vm would change the receipt of every block already recorded (P2's replay gate would see
/// that only as a differing block record): here it fails to compile, or fails on the line naming
/// the variant. The tables also have to list every variant of W1's `ALL`, in number order.
#[test]
fn every_trap_kind_and_validation_rule_keeps_its_number_and_name_in_receipts() {
    let traps: [(TrapKind, u8, &str); 30] = [
        (TrapKind::OutOfFuel, 1, "out_of_fuel"),
        (TrapKind::Unreachable, 2, "unreachable"),
        (TrapKind::MemoryOutOfBounds, 3, "memory_out_of_bounds"),
        (TrapKind::TableOutOfBounds, 4, "table_out_of_bounds"),
        (TrapKind::IndirectCallToNull, 5, "indirect_call_to_null"),
        (TrapKind::DivisionByZero, 6, "division_by_zero"),
        (TrapKind::IntegerOverflow, 7, "integer_overflow"),
        (TrapKind::StackOverflow, 8, "stack_overflow"),
        (TrapKind::BadSignature, 9, "bad_signature"),
        (TrapKind::MemoryAccess, 10, "memory_access"),
        (TrapKind::Length, 11, "length"),
        (TrapKind::Amount, 12, "amount"),
        (TrapKind::Flags, 13, "flags"),
        (TrapKind::Name, 14, "name"),
        (TrapKind::UnboundProgram, 15, "unbound_program"),
        (TrapKind::Reentrancy, 16, "reentrancy"),
        (TrapKind::CallDepth, 17, "call_depth"),
        (TrapKind::VersionMismatch, 18, "version_mismatch"),
        (TrapKind::NotAProgram, 19, "not_a_program"),
        (TrapKind::NoMethod, 20, "no_method"),
        (TrapKind::NotCallable, 21, "not_callable"),
        (TrapKind::NotPayable, 22, "not_payable"),
        (TrapKind::ReadOnly, 23, "read_only"),
        (TrapKind::Insufficient, 24, "insufficient"),
        (TrapKind::StorageLimit, 25, "storage_limit"),
        (TrapKind::StateFull, 26, "state_full"),
        (TrapKind::KeysWritten, 27, "keys_written"),
        (TrapKind::WrittenBytes, 28, "written_bytes"),
        (TrapKind::Events, 29, "events"),
        (TrapKind::EventBytes, 30, "event_bytes"),
    ];
    let rules: [(Rule, u8, &str); 14] = [
        (Rule::Size, 1, "size"),
        (Rule::Wasm, 2, "wasm"),
        (Rule::Feature, 3, "feature"),
        (Rule::Float, 4, "float"),
        (Rule::Import, 5, "import"),
        (Rule::Memory, 6, "memory"),
        (Rule::MemoryGrow, 7, "memory_grow"),
        (Rule::Table, 8, "table"),
        (Rule::Limit, 9, "limit"),
        (Rule::Segment, 10, "segment"),
        (Rule::BodySize, 11, "body_size"),
        (Rule::Export, 12, "export"),
        (Rule::Start, 13, "start"),
        (Rule::CustomSection, 14, "custom_section"),
    ];
    assert_eq!(traps.iter().map(|t| t.0).collect::<Vec<_>>(), TrapKind::ALL, "a trap kind without a row, or out of order");
    assert_eq!(rules.iter().map(|r| r.0).collect::<Vec<_>>(), Rule::ALL, "a rule without a row, or out of order");
    let p = [5u8; 32];
    let program = crypto::render_address("boo", &p);
    for (kind, number, name) in traps {
        assert_eq!((kind.number(), TrapKind::from_number(number)), (number, Some(kind)), "{kind:?}");
        let failure = Failure::Vm(FailCode::Trapped { program: p, kind });
        let outcome = ReceiptOutcome::failed(&failure, "boo");
        let json = serde_json::to_string(&outcome).unwrap();
        assert_eq!(json, format!(r#"{{"ok":false,"number":1,"code":"trapped","program":"{program}","trap":{{"number":{number},"name":"{name}"}}}}"#), "{kind:?}");
        assert_eq!(serde_json::from_str::<ReceiptOutcome>(&json).unwrap(), outcome, "{kind:?}");
        assert!(failure.reason("boo").ends_with(&format!("trapped: {} (trap {number})", name.replace('_', " "))), "{kind:?}: {}", failure.reason("boo"));
    }
    for (rule, number, name) in rules {
        assert_eq!((rule.number(), Rule::from_number(number)), (number, Some(rule)), "{rule:?}");
        let failure = Failure::InvalidCode(ValidateError { rule, func: None, offset: None, message: "never in a receipt".into() });
        let outcome = ReceiptOutcome::failed(&failure, "boo");
        let json = serde_json::to_string(&outcome).unwrap();
        assert_eq!(json, format!(r#"{{"ok":false,"number":14,"code":"invalid_code","rule":{{"number":{number},"name":"{name}"}}}}"#), "{rule:?}");
        assert_eq!(serde_json::from_str::<ReceiptOutcome>(&json).unwrap(), outcome, "{rule:?}");
    }
}

/// §8.1's budget: in block order, receipts record their claims up to 256 KiB in all (each event
/// counted as its data, its name and 32 bytes, each return as its length); one that doesn't fit
/// what's left records none of them and says so, spending nothing, so a later, smaller one may
/// still fit. Facts are kept either way.
#[test]
fn the_claims_budget_records_claims_up_to_256_kib_then_omits_them() {
    const RET: usize = 4 << 10;
    // 64 receipts of a 4 KiB return each fill the budget exactly; the 65th doesn't fit.
    let mut receipts: BTreeMap<usize, ProgramReceipt> = (0..65).map(|i| (i * 2, claiming(&[RET], &[]))).collect();
    // A receipt with no claims always fits; one event of one byte (1 + 1 + 32) no longer does.
    receipts.insert(200, ProgramReceipt { balance_changes: vec![BalanceChange { account: "boo1a".into(), change: -5 }], ..bare() });
    receipts.insert(201, ProgramReceipt { fee: FeeParts { bytes: 1, fuel: 2, tip: 3 }, ..claiming(&[], &[1]) });
    apply_claims_budget(&mut receipts);
    let recorded: u64 = receipts.values().map(ProgramReceipt::claim_bytes).sum();
    assert_eq!(recorded, MAX_BLOCK_CLAIM_BYTES);
    for i in 0..64 {
        assert_eq!((receipts[&(i * 2)].omitted.as_ref(), receipts[&(i * 2)].calls[0].return_data.0.len()), (None, RET), "receipt {i}");
    }
    let cut = &receipts[&128];
    assert_eq!(cut.omitted, Some(Omitted { events: 0, event_bytes: 0, return_bytes: RET as u64 }));
    assert!(cut.calls[0].return_data.0.is_empty() && cut.calls[0].method == "ret", "the call stays, its claim goes");
    assert_eq!((receipts[&200].omitted.as_ref(), receipts[&200].balance_changes.len()), (None, 1));
    let event = &receipts[&201];
    assert_eq!((event.omitted.clone(), event.events.len(), event.fee.tip), (Some(Omitted { events: 1, event_bytes: 2, return_bytes: 0 }), 0, 3));

    // What doesn't fit is skipped, not the end: a smaller receipt after it still fits.
    let mut receipts: BTreeMap<usize, ProgramReceipt> = BTreeMap::new();
    receipts.insert(0, claiming(&[RET; 8], &[16 << 10]));
    for i in 1..5 {
        receipts.insert(i, claiming(&[RET; 8], &[8 << 10]));
    }
    receipts.insert(5, claiming(&[RET; 8], &[16 << 10]));
    receipts.insert(6, claiming(&[RET; 8], &[]));
    apply_claims_budget(&mut receipts);
    let omitted: Vec<usize> = receipts.iter().filter(|(_, r)| r.omitted.is_some()).map(|(i, _)| *i).collect();
    // 0: 32 KiB + 16 KiB + 33 = 49,185; 1–4: 40,993 each, 213,157 in all, leaving 48,987: 5
    // (49,185) doesn't fit it, 6 (32 KiB) does.
    assert_eq!(omitted, vec![5]);
    assert_eq!(receipts[&5].omitted, Some(Omitted { events: 1, event_bytes: (16 << 10) + 1, return_bytes: 8 * RET as u64 }));
    assert_eq!(receipts.values().map(ProgramReceipt::claim_bytes).sum::<u64>(), 213_157 + 8 * RET as u64);
}

/// The lines of a recorded v1 block log (`node/testdata/replay/<name>`).
fn recorded_lines(name: &str) -> Vec<Vec<u8>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/replay").join(name).join("blocks-000000000001.jsonl");
    std::fs::read(&path).unwrap().split(|b| *b == b'\n').filter(|l| !l.is_empty()).map(<[u8]>::to_vec).collect()
}

/// A v1 block's record is what v0.5.2 wrote, byte for byte (§8.1, §10.1): every line of both
/// recorded v1 block logs reads into a `Block` with no receipts and writes back as the very same
/// bytes, with no `programs` key; so does a block of a chain born at v2 with programs dormant, and
/// one without program transactions on a chain that runs programs. A block with receipts writes
/// them under `programs`, after `base_fee`, and reads them back.
#[test]
fn a_v1_block_is_byte_identical_and_has_no_programs_key() {
    let mut lines = 0;
    for corpus in ["v1-open", "v1-season0"] {
        for line in recorded_lines(corpus) {
            let block: Block = serde_json::from_slice(&line).unwrap();
            assert!(block.programs.is_empty());
            assert_eq!(serde_json::to_vec(&block).unwrap(), line, "block {} of {corpus}", block.height);
            lines += 1;
        }
    }
    assert!(lines >= 2_199, "{lines} recorded blocks");
    assert!(!recorded_lines("v1-open").iter().any(|l| l.windows(10).any(|w| w == b"\"programs\"")));

    // v2 born dormant: its blocks carry no receipts and no key.
    let home = Dev::fresh_home("dormant");
    let mut o = crate::chain::SummonOpts::new("programs-dormant-1", 1);
    o.address_prefix = crypto::ADDRESS_HRP.into();
    Chain::summon(&home, &o).unwrap();
    let path = home.join("genesis.json");
    let mut g: crate::chain::Genesis = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    g.upgrades = vec![crate::upgrade::ScheduledUpgrade { version: crate::upgrade::PROGRAMS_VERSION, activation_height: 1 }];
    std::fs::write(&path, serde_json::to_string_pretty(&g).unwrap()).unwrap();
    let mut c = Chain::open(&home).unwrap();
    c.produce_block_with(vec![]).unwrap();
    let b = c.last_block().unwrap();
    assert_eq!((b.version, super::super::status(&c.state, 2)), (crate::upgrade::PROGRAMS_VERSION, super::super::Status::Dormant));
    assert!(!String::from_utf8(serde_json::to_vec(b).unwrap()).unwrap().contains("\"programs\""));
    drop(c);
    std::fs::remove_dir_all(home).ok();

    // Programs on: a block of a transfer has none either; one of an upload has its receipt.
    let mut d = Dev::new("v1-bytes");
    let (alice, bob) = (keys(ALICE), keys(BOB));
    let transfer = d.block(vec![d.tx(&alice, Action::Transfer { to: d.address(BOB), amount: FANG })]);
    let json = String::from_utf8(serde_json::to_vec(&transfer).unwrap()).unwrap();
    assert!(transfer.programs.is_empty() && !json.contains("\"programs\""), "{json}");
    let uploaded = d.block(vec![d.tx(&bob, upload(PROBE_WASM, &probe_interface()))]);
    let json = String::from_utf8(serde_json::to_vec(&uploaded).unwrap()).unwrap();
    assert!(json.contains(r#""base_fee":{"#) && json.find("\"base_fee\"") < json.find("\"programs\":{\"0\":{\"version\":1,\"kind\":\"upload\""), "{json}");
    let back: Block = serde_json::from_str(&json).unwrap();
    assert_eq!(back.programs, uploaded.programs);
    assert_eq!(serde_json::to_string(&back).unwrap(), json);
}

/// What a block's receipts say for uploads, creates and calls that apply (§8.1): the kind; the
/// fuel (an upload's limit and use its `upload_fuel`, a create without `init` none, a call its
/// limit, load and use); the fee in parts adding up to what was charged; the blob hashes and
/// deposits; the program created; each call's return data and the events (claims); the storage
/// moved, which is META's; and none for a transfer in the same block.
#[test]
fn receipts_say_what_uploads_creates_and_calls_did() {
    let mut d = Dev::new("receipts");
    let (alice, bob) = (keys(ALICE), keys(BOB));
    let iface = probe_interface();
    let schedule = blockula_vm::meter::schedule(crate::upgrade::PROGRAMS_VERSION).unwrap();

    // The upload.
    let (before, meta) = (d.balance(ALICE), d.c.state.programs.meta.clone().unwrap());
    let rate = fuel::rate(&d.c.state);
    let b = d.block(vec![d.tx(&alice, upload(PROBE_WASM, &iface))]);
    let r = &b.programs[&0];
    let after = d.c.state.programs.meta.clone().unwrap();
    let upload_fuel = schedule.upload_fuel(PROBE_WASM.len(), iface.len());
    assert_eq!((r.version, r.kind, &r.outcome), (1, ReceiptKind::Upload, &ReceiptOutcome::Ok));
    assert_eq!(r.fuel, FuelUse { limit: upload_fuel, load: 0, used: upload_fuel, reserved: upload_fuel, rate });
    let deposit = after.escrow - meta.escrow;
    assert_eq!(r.upload, Some(UploadResult { wasm_hash: hex::encode(digest::wasm_hash(PROBE_WASM)), interface_hash: hex::encode(digest::interface_hash(&iface)), new_code: true, new_interface: true, deposit }));
    assert_eq!(r.storage, StorageSummary { bytes_delta: (PROBE_WASM.len() + iface.len()) as i64, bonded: deposit, refunded: 0 });
    assert_eq!(r.outside_vm, OutsideVm { upload_deposit: deposit, ..OutsideVm::default() }, "the deposit is debited outside the VM");
    assert_eq!(r.fee.bytes + r.fee.fuel + r.fee.tip, before - d.balance(ALICE) - deposit, "the fee parts are what was charged");
    assert!(r.fee.fuel > 0 && r.calls.is_empty() && r.events.is_empty() && r.balance_changes.is_empty() && r.created.is_none());

    // Two creates and a transfer: the creates have receipts, the transfer none.
    let creates = d.txs(&alice, vec![create_probe(), create_probe()]);
    let made = |t: &Tx| digest::program_address(&d.prefix(), &crypto::address_bytes(&t.from).unwrap(), t.nonce);
    let (p, q) = (made(&creates[0]), made(&creates[1]));
    let pay = d.tx(&bob, Action::Transfer { to: d.address(CAROL), amount: FANG });
    let meta = d.c.state.programs.meta.clone().unwrap();
    let b = d.block(vec![creates[0].clone(), pay, creates[1].clone()]);
    assert_eq!(b.programs.keys().copied().collect::<Vec<_>>(), vec![0, 2]);
    let after = d.c.state.programs.meta.clone().unwrap();
    for (i, program) in [(0, &p), (2, &q)] {
        let r = &b.programs[&i];
        assert_eq!((r.kind, &r.outcome, r.created.as_ref()), (ReceiptKind::Create, &ReceiptOutcome::Ok, Some(program)));
        assert_eq!((r.fuel.limit, r.fuel.load, r.fuel.used, r.fuel.reserved, r.fee.fuel), (0, 0, 0, 0, 0), "a create without init runs nothing");
        assert_eq!((r.storage.bytes_delta, r.storage.refunded), (super::super::RECORD_BYTES as i64, 0));
        let creation_fee = d.c.state.chain_params.account_creation_fee;
        assert!(creation_fee > 0);
        assert_eq!(r.outside_vm, OutsideVm { record_deposit: r.storage.bonded, upload_deposit: 0, account_creation_fee: creation_fee }, "the record's deposit and the new account's creation fee");
    }
    assert_eq!(b.programs[&0].storage.bonded + b.programs[&2].storage.bonded, after.escrow - meta.escrow);

    // Calls: return data and an event (claims), on Bob's call.
    let meta = d.c.state.programs.meta.clone().unwrap();
    let call = d.tx(&bob, calls(vec![d.call(&p, "ret", vec![], 0), d.call(&p, "emit", vec![], 0)], vec![]));
    let charged = crate::econ::check_fee(&d.c.state, &call, d.next()).unwrap();
    let b = d.block(vec![call]);
    let r = &b.programs[&0];
    assert_eq!((r.kind, &r.outcome, r.fee.bytes + r.fee.fuel + r.fee.tip), (ReceiptKind::Call, &ReceiptOutcome::Ok, charged));
    let load = schedule.load_fuel(PROBE_WASM.len());
    assert_eq!((r.fuel.limit, r.fuel.load, r.fuel.reserved), (FUEL, load, FUEL + load));
    assert!(r.fuel.used > 0 && r.fuel.used < FUEL, "{:?}", r.fuel);
    assert_eq!(r.calls, vec![CallResult { program: p.clone(), method: "ret".into(), value: 0, return_data: HexBytes(probe_bytes()) }, CallResult { program: p.clone(), method: "emit".into(), value: 0, return_data: HexBytes::default() }]);
    assert_eq!(r.events, vec![ReceiptEvent { program: p.clone(), name: "e".into(), data: HexBytes(probe_bytes()) }]);
    assert_eq!((r.claim_bytes(), r.omitted.as_ref()), (4096 + 4096 + 1 + 32, None));
    assert_eq!((r.storage.clone(), r.balance_changes.clone(), r.outside_vm.clone()), (StorageSummary::default(), vec![], OutsideVm::default()));
    assert_eq!(d.c.state.programs.meta.clone().unwrap().escrow, meta.escrow);
    // The receipt is the block's: none is left in the state for the next block.
    assert!(d.c.state.programs.block.receipts.is_empty());
}

/// A program transaction that fails is charged and its receipt says why, with the fuel it burned
/// and nothing else: an abort (its code and its own message), a nested out-of-fuel (`used ==
/// limit`, although only the callee's 100,000-fuel share ran out, and the trap where it began), an
/// upload validation refuses (its rule, `upload_fuel` used), a create whose `init` aborts (no
/// program, no deposit: the create's record and its bond were noted before `init` ran, and rolled
/// back with it). The block's `failed` reason is the receipt's outcome in words.
#[test]
fn a_failed_program_tx_keeps_its_fee_and_fuel_and_nothing_else() {
    let mut d = Dev::new("failed");
    let (alice, bob, carol) = (keys(ALICE), keys(BOB), keys(CAROL));
    let programs = d.deploy(2);
    let (p, q) = (&programs[0], &programs[1]);
    let iface = probe_interface();
    d.block(vec![d.tx(&alice, upload(TOKEN.0, TOKEN.1))]);
    // The token's `init` takes 40 bytes (an owner and a supply): with 3 it aborts (code 1).
    let mut aborts = create(TOKEN.0, TOKEN.1);
    if let Action::CreateProgram { init, fuel_limit, .. } = &mut aborts {
        (*init, *fuel_limit) = (Some(InitCall { method: "init".into(), input: HexBytes(vec![1, 2, 3]), value: 0 }), FUEL);
    }
    let mut txs = vec![
        d.tx(&bob, calls(vec![d.call(p, "ret", vec![], 0), d.call(p, "no", vec![], 0)], vec![])),
        d.tx(&carol, calls(vec![d.call(p, "go", crypto::address_bytes(q).unwrap().to_vec(), 0)], vec![d.bind(q)])),
    ];
    txs.extend(d.txs(&alice, vec![upload(FLOAT_WASM, &iface), aborts]));
    let token = digest::program_address(&d.prefix(), &crypto::address_bytes(&txs[3].from).unwrap(), txs[3].nonce);
    let charged: Vec<u64> = txs.iter().map(|t| crate::econ::check_fee(&d.c.state, t, d.next()).unwrap()).collect();
    let (bob_before, carol_before, meta) = (d.balance(BOB), d.balance(CAROL), d.c.state.programs.meta.clone().unwrap());
    let b = d.block(txs);
    assert_eq!(b.failed.keys().copied().collect::<Vec<_>>(), vec![0, 1, 2, 3], "{:?}", b.failed);
    assert_eq!((d.balance(BOB), d.balance(CAROL)), (bob_before - charged[0], carol_before - charged[1]), "charged, and nothing else moved");

    let aborted = &b.programs[&0];
    assert_eq!(aborted.outcome, ReceiptOutcome::Aborted { program: p.clone(), abort_code: 7, untrusted_message: "e".into() });
    assert_eq!(b.failed[&0], format!("program {} aborted with code 7", short(p)));
    assert!(aborted.fuel.used > 0 && aborted.fuel.used < FUEL);
    assert_eq!(aborted.calls.iter().map(|c| (c.method.as_str(), c.return_data.0.len())).collect::<Vec<_>>(), vec![("ret", 0), ("no", 0)], "no return data from a failed tx");
    assert!(aborted.events.is_empty() && aborted.balance_changes.is_empty() && aborted.storage == StorageSummary::default());
    assert_eq!(aborted.fee.bytes + aborted.fee.fuel + aborted.fee.tip, charged[0]);

    let nested = &b.programs[&1];
    assert_eq!(nested.outcome, ReceiptOutcome::Trapped { program: q.clone(), trap: Numbered { number: 1, name: "out_of_fuel".into() } });
    assert_eq!((nested.fuel.used, nested.fuel.limit), (FUEL, FUEL), "a nested out-of-fuel uses the whole limit");
    const { assert!(SHARE < FUEL) };
    assert_eq!(b.failed[&1], format!("program {} trapped: out of fuel (trap 1)", short(q)));

    let refused = &b.programs[&2];
    let ReceiptOutcome::InvalidCode { rule, .. } = &refused.outcome else { panic!("{:?}", refused.outcome) };
    assert_eq!((rule.number, rule.name.as_str()), (Rule::Float.number(), "float"));
    assert!(b.failed[&2].starts_with("invalid code: upload validation rule 4 "), "{}", b.failed[&2]);
    assert_eq!((refused.fuel.used, refused.fuel.limit), (refused.fuel.reserved, refused.fuel.reserved));
    assert!(refused.upload.is_none() && refused.storage == StorageSummary::default() && refused.outside_vm == OutsideVm::default(), "nothing stored, nothing debited outside the VM");

    let init = &b.programs[&3];
    assert!(matches!(&init.outcome, ReceiptOutcome::Aborted { program, abort_code: 1, .. } if *program == token), "{:?}", init.outcome);
    assert_eq!(b.failed[&3], format!("program {} aborted with code 1", short(&token)));
    assert_eq!((init.kind, init.fuel.limit, init.calls.len(), init.calls[0].method.as_str()), (ReceiptKind::Create, FUEL, 1, "init"));
    assert!(init.fuel.used > 0 && init.calls[0].return_data.0.is_empty());
    assert_eq!((init.created.as_ref(), &init.storage, init.balance_changes.len(), &init.outside_vm), (None, &StorageSummary::default(), 0, &OutsideVm::default()), "no program, no deposit, no creation fee");
    let after = d.c.state.programs.meta.clone().unwrap();
    assert_eq!((after.programs, after.escrow, after.state_bytes), (meta.programs, meta.escrow, meta.state_bytes));
}

/// What receipts say calls stored and moved (§8.1's facts, which the state holds too): the
/// counter's first `increment` bonds a new entry, paid by its caller, and its second overwrites
/// the entry in place, keeping its bond (nothing bonded or refunded, no balance moved); each
/// receipt's storage is META's change, and each call's return data the count. The probe's `fund`
/// takes 3 FANG from Bob: the balance changes are the program diff's, sorted by address bytes.
#[test]
fn receipts_say_what_calls_stored_and_moved() {
    let mut d = Dev::new("stored");
    let (alice, bob) = (keys(ALICE), keys(BOB));
    let p = d.deploy(1).remove(0);
    d.block(vec![d.tx(&alice, upload(COUNTER.0, COUNTER.1))]);
    let create = d.tx(&alice, create(COUNTER.0, COUNTER.1));
    let counter = digest::program_address(&d.prefix(), &crypto::address_bytes(&create.from).unwrap(), create.nonce);
    d.block(vec![create]);
    let meta = |d: &Dev| d.c.state.programs.meta.clone().unwrap();
    for n in 1..=2u64 {
        let before = meta(&d);
        let b = d.block(vec![d.tx(&bob, calls(vec![d.call(&counter, "increment", vec![], 0)], vec![]))]);
        let (r, after) = (&b.programs[&0], meta(&d));
        assert_eq!((&r.outcome, r.calls[0].return_data.0.as_slice()), (&ReceiptOutcome::Ok, n.to_le_bytes().as_slice()));
        assert_eq!(r.storage.bytes_delta, after.state_bytes as i64 - before.state_bytes as i64, "increment {n}");
        assert_eq!(i128::from(r.storage.bonded) - i128::from(r.storage.refunded), i128::from(after.escrow) - i128::from(before.escrow), "increment {n}");
        if n == 1 {
            assert!(r.storage.bytes_delta > 0 && r.storage.bonded > 0 && r.storage.refunded == 0, "{:?}", r.storage);
            assert_eq!(r.balance_changes, vec![BalanceChange { account: d.address(BOB), change: -i128::from(r.storage.bonded) }], "Bob paid the bond");
        } else {
            assert_eq!((r.storage.clone(), r.balance_changes.clone()), (StorageSummary::default(), vec![]), "the bond is kept");
        }
    }

    let held = meta(&d).value_held;
    let b = d.block(vec![d.tx(&bob, calls(vec![d.call(&p, "fund", vec![], 3 * FANG)], vec![]))]);
    let mut moved = vec![(d.address(BOB), -i128::from(3 * FANG)), (p.clone(), i128::from(3 * FANG))];
    moved.sort_by_key(|(a, _)| crypto::address_bytes(a).unwrap());
    let moved: Vec<BalanceChange> = moved.into_iter().map(|(account, change)| BalanceChange { account, change }).collect();
    assert_eq!((&b.programs[&0].balance_changes, meta(&d).value_held - held), (&moved, 3 * FANG));
    let json = serde_json::to_string(&b.programs[&0].balance_changes).unwrap();
    assert_eq!(json, format!(r#"[{{"account":"{}","change":"{}"}},{{"account":"{}","change":"{}"}}]"#, moved[0].account, moved[0].change, moved[1].account, moved[1].change), "drops as strings");
}

/// A program can run, and its transaction still fail on a native check after it (§6.1 step 6's
/// `check_locks`): Dave sends a program 50 FANG of his 100 locked ones, which the VM lets through
/// (it checks balances) and the lock check refuses. Charged, and its receipt says `native`, with
/// nothing of the call's effects.
#[test]
fn a_check_after_the_program_ran_fails_its_receipt_as_native() {
    let mut d = Dev::new("native");
    let (alice, dave) = (keys(ALICE), keys(DAVE));
    let p = d.deploy(1).remove(0);
    d.block(vec![d.tx(&alice, Action::Transfer { to: d.address(DAVE), amount: 10 * FANG })]);
    let held = d.c.state.programs.meta.clone().unwrap().value_held;
    let call = d.tx(&dave, calls(vec![d.call(&p, "fund", vec![], 50 * FANG)], vec![]));
    let charged = crate::econ::check_fee(&d.c.state, &call, d.next()).unwrap();
    let before = d.balance(DAVE);
    let b = d.block(vec![call]);
    assert!(b.failed.contains_key(&0), "{:?}", b.failed);
    let r = &b.programs[&0];
    assert_eq!(r.outcome, ReceiptOutcome::Native);
    assert_eq!((r.calls[0].method.as_str(), r.calls[0].value), ("fund", 50 * FANG));
    assert!(r.fuel.used > 0, "the program ran");
    assert!(r.balance_changes.is_empty() && r.events.is_empty() && r.storage == StorageSummary::default() && r.outside_vm == OutsideVm::default());
    assert_eq!((d.balance(DAVE), d.c.state.programs.meta.clone().unwrap().value_held), (before - charged, held));
}

/// `outside_vm`'s JSON: the three parts and their total, as strings; what it writes reads back, and a
/// total that isn't the parts' sum, or a part that isn't a number, doesn't.
#[test]
fn outside_vm_reads_back_as_written_and_refuses_a_wrong_total() {
    let o = OutsideVm { record_deposit: 25_600_000, upload_deposit: 7, account_creation_fee: 100_000_000 };
    let json = r#"{"record_deposit":"25600000","upload_deposit":"7","account_creation_fee":"100000000","total":"125600007"}"#;
    assert_eq!((o.total(), serde_json::to_string(&o).unwrap()), (125_600_007, json.to_string()));
    assert_eq!(serde_json::from_str::<OutsideVm>(json).unwrap(), o);
    assert_eq!(serde_json::to_string(&OutsideVm::default()).unwrap(), r#"{"record_deposit":"0","upload_deposit":"0","account_creation_fee":"0","total":"0"}"#);
    for bad in [
        r#"{"record_deposit":"25600000","upload_deposit":"7","account_creation_fee":"100000000","total":"125600008"}"#,
        r#"{"record_deposit":"1","upload_deposit":"0","account_creation_fee":"0"}"#,
        r#"{"record_deposit":"x","upload_deposit":"0","account_creation_fee":"0","total":"0"}"#,
    ] {
        assert!(serde_json::from_str::<OutsideVm>(bad).is_err(), "{bad}");
    }
    let max = OutsideVm { record_deposit: u64::MAX, upload_deposit: 1, account_creation_fee: 1 };
    assert_eq!(max.total(), u64::MAX, "saturating");
}

/// What a create debits outside the VM (§8.1's `outside_vm`) is what the chain takes from its creator:
/// the record deposit, and the creation fee of the program's account only when the create opened that
/// account (an address FANG was sent to ahead is adopted, and pays none); a create whose creator can't
/// pay the creation fee fails as `native` after the program's deposit was noted, and its receipt
/// reports nothing outside the VM, as nothing was taken.
#[test]
fn a_creates_debits_outside_the_vm_are_what_the_chain_takes() {
    let mut d = Dev::new("outside");
    let (alice, e) = (keys(ALICE), keys(55));
    let iface = probe_interface();
    d.block(vec![d.tx(&alice, upload(PROBE_WASM, &iface))]);
    let creation_fee = d.c.state.chain_params.account_creation_fee;
    let charged = |r: &ProgramReceipt| r.fee.bytes + r.fee.fuel + r.fee.tip;

    // An address nobody holds: the create opens it, and pays for that.
    let before = d.balance(ALICE);
    let b = d.block(vec![d.tx(&alice, create_probe())]);
    let opened = &b.programs[&0];
    let record_deposit = opened.storage.bonded;
    assert_eq!(opened.outside_vm, OutsideVm { record_deposit, upload_deposit: 0, account_creation_fee: creation_fee });
    assert_eq!(before - d.balance(ALICE), charged(opened) + opened.outside_vm.total(), "the creator paid the fee and what was debited outside the VM, nothing else");

    // An address FANG was sent to ahead of the create: adopted, no creation fee.
    let nonce = d.c.state.account(&d.address(ALICE)).nonce;
    let adopted = digest::program_address(&d.prefix(), &crypto::address_bytes(&d.address(ALICE)).unwrap(), nonce + 1);
    let pair = d.txs_at(&alice, Some(nonce), 0, vec![Action::Transfer { to: adopted.clone(), amount: FANG }, create_probe()]);
    d.block(vec![pair[0].clone()]);
    let before = d.balance(ALICE);
    let b = d.block(vec![pair[1].clone()]);
    let r = &b.programs[&0];
    assert_eq!(r.created.as_deref(), Some(adopted.as_str()));
    assert_eq!(r.outside_vm, OutsideVm { record_deposit, upload_deposit: 0, account_creation_fee: 0 }, "an adopted account has no creation fee to burn");
    assert_eq!(before - d.balance(ALICE), charged(r) + r.outside_vm.total());

    // A creator with the fee and the record deposit but not the creation fee: the create fails.
    let probe = d.tx(&e, create_probe());
    let funds = crate::econ::check_fee(&d.c.state, &probe, d.next()).unwrap() + record_deposit + creation_fee - 1;
    d.block(vec![d.tx(&alice, Action::Transfer { to: d.address(55), amount: funds })]);
    assert_eq!(d.balance(55), funds);
    let b = d.block(vec![probe]);
    assert_eq!(b.failed.keys().copied().collect::<Vec<_>>(), vec![0], "{:?}", b.failed);
    let r = &b.programs[&0];
    assert_eq!((&r.outcome, r.created.as_ref(), &r.storage, &r.outside_vm), (&ReceiptOutcome::Native, None, &StorageSummary::default(), &OutsideVm::default()));
    assert_eq!(d.balance(55), funds - charged(r), "only the fee was charged");
}

/// The claims budget on a real block, the same on every node (§8.1): ten calls of eight 4 KiB
/// returns each and one of an event fill 256 KiB with the first eight; the rest record their facts
/// and say what they left out, and a call with no claims after them is whole. A second node of the
/// chain executing the same blocks writes byte for byte the same block records, and the first,
/// its database deleted, rebuilds its state from its block log to the same root and keeps the
/// logged receipts (`Chain::replay`), having checked those it computed again against them: none
/// differs (`Chain::receipt_mismatches`).
#[test]
fn the_claims_budget_cuts_the_same_on_every_node_and_receipts_survive_a_rebuild() {
    let mut a = Dev::new("claims-a");
    let mut b = a.node("claims-b");
    let (alice, bob, carol) = (keys(ALICE), keys(BOB), keys(CAROL));
    let mut blocks = vec![vec![a.tx(&alice, upload(PROBE_WASM, &probe_interface()))]];
    let create = a.txs_at(&alice, Some(1), 0, vec![create_probe()]);
    let p = digest::program_address(&a.prefix(), &crypto::address_bytes(&create[0].from).unwrap(), 1);
    blocks.push(create);
    for txs in &blocks {
        a.block(txs.clone());
        b.block(txs.clone());
    }
    let ret = |d: &Dev| calls(vec![d.call(&p, "ret", vec![], 0); 8], vec![]);
    let mut txs = a.txs(&bob, (0..10).map(|_| ret(&a)).collect());
    txs.extend(a.txs(&carol, vec![calls(vec![a.call(&p, "emit", vec![], 0)], vec![]), calls(vec![a.call(&p, "fund", vec![], 0)], vec![])]));
    let block = a.block(txs.clone());
    let other = b.block(txs);
    assert_eq!(serde_json::to_vec(&block).unwrap(), serde_json::to_vec(&other).unwrap(), "both nodes write the same record");
    assert_eq!(block.programs.len(), 12);
    assert!(block.failed.is_empty(), "{:?}", block.failed);
    let mut left = MAX_BLOCK_CLAIM_BYTES;
    for (i, r) in &block.programs {
        match *i {
            0..=7 => {
                assert_eq!((r.omitted.as_ref(), r.claim_bytes()), (None, 8 * 4096), "receipt {i}");
                left -= r.claim_bytes();
            }
            8 | 9 => assert_eq!(r.omitted, Some(Omitted { events: 0, event_bytes: 0, return_bytes: 8 * 4096 }), "receipt {i}"),
            10 => assert_eq!(r.omitted, Some(Omitted { events: 1, event_bytes: 4096 + 1, return_bytes: 0 })),
            _ => assert_eq!((r.omitted.as_ref(), r.claim_bytes()), (None, 0)),
        }
        assert_eq!((&r.outcome, r.calls.len()), (&ReceiptOutcome::Ok, if *i < 10 { 8 } else { 1 }), "facts are kept");
    }
    assert_eq!(left, 0);

    // The first node rebuilds its database from its block log: same root, same records, and the
    // receipts its replay computed again taken out of the state (`Chain::replay`'s `replayed`).
    let (height, root) = (a.c.height(), a.c.state.root());
    let records: Vec<Vec<u8>> = (1..=height).map(|h| serde_json::to_vec(&a.c.block(h).unwrap()).unwrap()).collect();
    drop(a.take());
    std::fs::remove_file(a.home.join(store::DB_FILE)).unwrap();
    a.reopen().unwrap();
    assert_eq!((a.c.height(), a.c.state.root(), a.c.replayed), (height, root, height));
    assert_eq!(b.c.state.root(), a.c.state.root());
    for h in 1..=height {
        assert_eq!(serde_json::to_vec(&a.c.block(h).unwrap()).unwrap(), records[h as usize - 1], "block {h}");
    }
    assert!(a.c.state.programs.block.receipts.is_empty(), "none stay behind in the state");
    // The receipts the real replay computed again are the logged ones, on every block (a mismatch
    // is logged at error level and counted, and the logged ones are kept all the same).
    assert_eq!(a.c.receipt_mismatches, 0, "the startup replay computed receipts that differ from the logged ones");

    // What that replay checks, on a third node: the logged block's transactions applied again
    // compute the logged receipts, and a logged receipt that differs is found out.
    let mut third = a.node("claims-c");
    for txs in blocks {
        third.block(txs);
    }
    let (logged, chain_id) = (a.c.block(height).unwrap(), a.c.genesis.chain_id.clone());
    let st = &mut third.c.state;
    for tx in &logged.txs {
        st.apply(tx, &chain_id, &logged.proposer, logged.height).unwrap();
    }
    let mut again = st.clone();
    assert!(replayed(st, &logged));
    assert!(st.programs.block.receipts.is_empty());
    let mut changed = logged.clone();
    changed.programs.get_mut(&11).unwrap().fuel.used += 1;
    assert!(!replayed(&mut again, &changed));
}

/// What a receipt's facts add up to (§8.1): Σ `balance_changes` + (`storage.bonded` −
/// `outside_vm.record_deposit` − `outside_vm.upload_deposit` − `storage.refunded`) + `burned_by_programs`.
/// Zero for every receipt the chain writes.
fn conservation_gap(r: &ProgramReceipt) -> i128 {
    let moved: i128 = r.balance_changes.iter().map(|c| c.change).sum();
    moved + i128::from(r.storage.bonded) - i128::from(r.outside_vm.record_deposit) - i128::from(r.outside_vm.upload_deposit) - i128::from(r.storage.refunded) + i128::from(r.burned_by_programs)
}

/// What the programs' own sends burn is a fact of the receipt (§8.1): a deal's payout to an account
/// that doesn't exist opens it, and the creation fee comes out of the payment, inside the VM's diff
/// (the program's balance pays it, never the signer, whom `econ::charge_new_accounts` doesn't bill
/// for an account a program's commit opened). `burned_by_programs` states it, the chain's own burn
/// counter agrees with it to the drop, `/api/tx` serves it, and every receipt of the run conserves
/// FANG with it: upload, create, a payable call, the payout.
#[test]
fn a_payout_that_opens_an_account_states_its_burn_and_every_receipt_conserves() {
    let mut d = Dev::new("burned");
    let (alice, bob) = (keys(ALICE), keys(BOB));
    let seller = 77;
    let le = |n: u64| n.to_le_bytes().to_vec();
    let creation_fee = d.c.state.chain_params.account_creation_fee;
    assert!(creation_fee > 0 && !d.c.state.accounts.contains_key(d.address(seller).as_str()), "nobody holds the seller's account");

    let mut receipts = vec![];
    receipts.push(d.block(vec![d.tx(&alice, upload(ESCROW.0, ESCROW.1))]).programs[&0].clone());
    let made = d.tx(&alice, create(ESCROW.0, ESCROW.1));
    let escrow = digest::program_address(&d.prefix(), &crypto::address_bytes(&made.from).unwrap(), made.nonce);
    receipts.push(d.block(vec![made]).programs[&0].clone());
    let open = d.tx(&bob, calls(vec![d.call(&escrow, "open", [le(1), crypto::address_bytes(&d.address(seller)).unwrap().to_vec()].concat(), 5 * FANG)], vec![]));
    let opened = d.block(vec![open]).programs[&0].clone();
    assert_eq!((&opened.outcome, opened.burned_by_programs), (&ReceiptOutcome::Ok, 0), "taking a deposit burns nothing");
    receipts.push(opened);

    let burned_before = d.c.state.burned;
    let release = d.tx(&bob, calls(vec![d.call(&escrow, "release", le(1), 0)], vec![]));
    let b = d.block(vec![release]);
    let r = &b.programs[&0];
    assert_eq!(r.outcome, ReceiptOutcome::Ok, "{:?}", b.failed);
    assert_eq!(r.burned_by_programs, creation_fee, "the creation fee of the account the payout opened");
    assert_eq!(d.balance(seller), 5 * FANG - creation_fee, "the seller got the payment less the fee, out of the amount");
    assert_eq!(d.c.state.burned - burned_before, r.fee.bytes + r.fee.fuel + r.burned_by_programs, "the chain's burn counter: the fee's base parts (no tip) and what the programs burned, to the drop");
    assert_eq!(r.outside_vm, OutsideVm::default(), "the signer is billed no creation fee for an account a program opened");
    receipts.push(r.clone());
    for (i, r) in receipts.iter().enumerate() {
        assert_eq!(conservation_gap(r), 0, "receipt {i}: {r:?}");
    }
    assert_eq!(receipts.iter().map(|r| r.burned_by_programs).sum::<u64>(), creation_fee, "only the payout burned");

    // The API's section serves it as a fact, as drops in a string.
    assert_eq!(crate::programs::tx_json::section(Some(r))["facts"]["burned_by_programs"], serde_json::json!(creation_fee.to_string()));

    // Releasing again fails (the deal is gone): a failed receipt keeps its fee and fuel and states no burn.
    let again = d.tx(&bob, calls(vec![d.call(&escrow, "release", le(1), 0)], vec![]));
    let b = d.block(vec![again]);
    let r = &b.programs[&0];
    assert_ne!(r.outcome, ReceiptOutcome::Ok);
    assert_eq!((r.burned_by_programs, conservation_gap(r)), (0, 0));
}

/// A failed receipt states no burn (`into_failed` clears it with the rest of the action's facts), and a
/// receipt logged before the field existed reads back with 0: it's additive, and display only.
#[test]
fn burned_by_programs_is_cleared_by_a_failure_and_reads_as_zero_when_absent() {
    let r = ProgramReceipt { burned_by_programs: 7, ..bare() };
    assert_eq!(r.clone().into_failed(ReceiptOutcome::Native).burned_by_programs, 0);
    let mut json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["burned_by_programs"], serde_json::json!("7"), "drops as a string, like every amount");
    json.as_object_mut().unwrap().remove("burned_by_programs");
    let old: ProgramReceipt = serde_json::from_value(json).unwrap();
    assert_eq!(old, ProgramReceipt { burned_by_programs: 0, ..r });
}
