//! Protocol v3's host functions (`blake3_v1`, `epoch_v1`, `beacon_v1`: docs/RANDOMNESS.md,
//! docs/PROGRAMS.md 10.3) and the `dice` guest that settles on them through `abi::random`, on
//! `MemHost`, on every engine (`EngineKind::ALL`, wasmi 0.38 too with `--features ref038`):
//!
//! * **The gate.** Code importing one of the three uploads only from protocol v3; everything
//!   else uploads the same under v2 and v3.
//! * **The functions.** `beacon_v1` is −1 before a value is derived, −1 again once it's pruned,
//!   and the value with its reveal count in between; `blake3_v1` is BLAKE3; `epoch_v1` is the
//!   height's epoch; each charges its schedule price, and every answer is the same on every
//!   engine and every run.
//! * **The pattern.** `dice`: a roll can't settle before its epoch's beacon, settles on
//!   BLAKE3(domain ‖ program ‖ request id ‖ beacon value) for as long as the chain keeps that
//!   value, pays whoever settles it a bounty, is refunded when the beacon had too few reveals,
//!   and is forfeit (never refunded) once the value is gone.

use std::path::Path;

use blockula_vm::abi::{self, FIRST_VERSION, RANDOMNESS_VERSION};
use blockula_vm::engine::{EngineKind, TrapKind};
use blockula_vm::exec::{Call, Event, FailCode, Receipt, TxLimits, execute};
use blockula_vm::host::{Beacon, MethodKind, View};
use blockula_vm::meter::SCHEDULE_V0;
use blockula_vm::testing::{MemHost, addr, strip_custom_sections};
use blockula_vm::upload::Refusal;
use blockula_vm::validate::{Rule, validate, validate_at};

const FANG: u64 = 1_000_000_000;
const LIMITS: TxLimits = TxLimits { fuel_limit: 10_000_000, storage_limit: FANG };

fn guest(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/programs/{name}.wasm"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (run guests/build.sh)", path.display()))
}

/// A program that calls each new host function with the arguments its input gives:
/// - `hash`: the input's BLAKE3 (32 bytes);
/// - `hash_raw`: input `ptr 4 ‖ len 4 ‖ out 4`, passed to `blake3_v1` as they are;
/// - `epoch`: `epoch_v1()`, 8 bytes LE;
/// - `beacon`: input `epoch 8 ‖ out 4`; returns `beacon_v1`'s result (4 bytes LE) and then the
///   32 bytes at address 64, which start as `0xAA`s (so a call that writes nothing shows).
fn probe() -> Vec<u8> {
    let wasm = wat::parse_str(
        r#"(module
  (import "blockula" "input_len_v1" (func $input_len (result i32)))
  (import "blockula" "input_read_v1" (func $input_read (param i32)))
  (import "blockula" "return_data_v1" (func $return_data (param i32 i32)))
  (import "blockula" "blake3_v1" (func $blake3 (param i32 i32 i32)))
  (import "blockula" "epoch_v1" (func $epoch (result i64)))
  (import "blockula" "beacon_v1" (func $beacon (param i64 i32) (result i32)))
  (memory (export "memory") 1 1)
  (func (export "bk_hash")
    (call $input_read (i32.const 1024))
    (call $blake3 (i32.const 1024) (call $input_len) (i32.const 0))
    (call $return_data (i32.const 0) (i32.const 32)))
  (func (export "bk_hash_raw")
    (call $input_read (i32.const 0))
    (call $blake3 (i32.load (i32.const 0)) (i32.load (i32.const 4)) (i32.load (i32.const 8))))
  (func (export "bk_epoch")
    (i64.store (i32.const 0) (call $epoch))
    (call $return_data (i32.const 0) (i32.const 8)))
  (func (export "bk_beacon")
    (call $input_read (i32.const 0))
    (i32.store (i32.const 60) (call $beacon (i64.load (i32.const 0)) (i32.load (i32.const 8))))
    (call $return_data (i32.const 60) (i32.const 36)))
  (data (i32.const 64) "\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa\aa"))"#,
    )
    .unwrap();
    strip_custom_sections(&wasm)
}

fn on_program_thread<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    std::thread::scope(|s| std::thread::Builder::new().stack_size(64 << 20).spawn_scoped(s, f).unwrap().join().unwrap())
}

/// One call from `signer`, on every engine, twice on each: every receipt must be the same, fuel
/// too between a cold and a warm run (the engines' own fuel differs). A successful one is applied.
fn tx(host: &mut MemHost, signer: &str, program: &str, method: &str, input: Vec<u8>, value: u64, kind: MethodKind) -> Receipt {
    let call = host.bind(Call { program: addr(program), version_digest: [0; 32], method: method.into(), input, value, kind });
    let h = &*host;
    let receipts: Vec<Receipt> = EngineKind::ALL
        .iter()
        .map(|&kind| {
            on_program_thread(|| {
                let (engine, _) = h.engine_for(kind, &SCHEDULE_V0, &[call.program]).unwrap();
                let cold = execute(engine.as_ref(), h, addr(signer), std::slice::from_ref(&call), &[], LIMITS, &SCHEDULE_V0).expect("no node fault");
                let warm = execute(engine.as_ref(), h, addr(signer), std::slice::from_ref(&call), &[], LIMITS, &SCHEDULE_V0).expect("no node fault");
                assert_eq!(cold, warm, "{method}: a second run differs on {}", kind.name());
                cold
            })
        })
        .collect();
    for r in &receipts[1..] {
        assert_eq!(Receipt { fuel_used: receipts[0].fuel_used, ..r.clone() }, receipts[0], "{method}: the engines disagree");
    }
    let r = receipts[0].clone();
    if r.ok() {
        host.apply(&r.diff);
    }
    r
}

fn call(host: &mut MemHost, method: &str, input: Vec<u8>) -> Receipt {
    tx(host, "alice", "probe", method, input, 0, MethodKind::Call)
}

fn returned(r: &Receipt) -> &[u8] {
    assert!(r.ok(), "{:?}", r.fail);
    &r.returns[0]
}

fn trap(r: &Receipt) -> Option<TrapKind> {
    match &r.fail {
        Some(FailCode::Trapped { kind, .. }) => Some(*kind),
        _ => None,
    }
}

fn abort_code(r: &Receipt) -> Option<u32> {
    match &r.fail {
        Some(FailCode::Aborted { code, .. }) => Some(*code),
        _ => None,
    }
}

fn le(n: u64) -> Vec<u8> {
    n.to_le_bytes().to_vec()
}

/// A `MemHost` whose uploads run under protocol `version`.
fn host_at(version: u32) -> MemHost {
    let mut host = MemHost::new();
    host.protocol_version = version;
    host
}

fn probe_host() -> MemHost {
    let mut host = MemHost::new();
    host.fund(addr("alice"), 100 * FANG);
    host.deploy(addr("probe"), &probe());
    host
}

/// `beacon_v1(epoch, 64)` through the probe: its result and the 32 bytes at the out pointer.
fn beacon(host: &mut MemHost, epoch: i64) -> (i32, [u8; 32]) {
    let r = call(host, "beacon", [&epoch.to_le_bytes()[..], &64u32.to_le_bytes()].concat());
    let out = returned(&r);
    (i32::from_le_bytes(out[..4].try_into().unwrap()), out[4..].try_into().unwrap())
}

const UNTOUCHED: [u8; 32] = [0xAA; 32];

// --- the gate ---

/// PRG 10.1's admission of imports: the three functions are in the table from protocol v3, and
/// an upload under v2 refuses code that imports one as it refuses any import the ABI lacks.
#[test]
fn the_new_functions_upload_only_from_protocol_v3() {
    for name in ["blake3_v1", "epoch_v1", "beacon_v1"] {
        let f = abi::host_fn(name).unwrap();
        assert_eq!(f.since, RANDOMNESS_VERSION, "{name}");
        assert!(abi::host_fn_at(name, FIRST_VERSION).is_none() && abi::host_fn_at(name, RANDOMNESS_VERSION).is_some(), "{name}");
    }
    assert_eq!((FIRST_VERSION, RANDOMNESS_VERSION, abi::LATEST_VERSION), (2, 3, 3));
    // Everything else is as it was: in force from v2, and at the same index.
    let v2: Vec<&str> = abi::HOST_FNS.iter().filter(|f| f.since == FIRST_VERSION).map(|f| f.name).collect();
    assert_eq!(v2.len(), 20);
    assert_eq!(abi::HOST_FNS[..20].iter().map(|f| f.name).collect::<Vec<_>>(), v2, "new functions go after the first table");

    for (name, code) in [("probe", probe()), ("dice", guest("dice"))] {
        for v in [FIRST_VERSION, 1, 0] {
            assert_eq!(validate_at(&code, v).unwrap_err().rule, Rule::Import, "{name} under v{v}");
            let mut host = host_at(v);
            match host.upload(&code) {
                Ok(Err(Refusal::Invalid(e))) => assert_eq!(e.rule, Rule::Import, "{name} under v{v}: {e}"),
                other => panic!("{name} under v{v}: expected a refusal, got {other:?}"),
            }
            assert_eq!(host, host_at(v), "a refused upload stores nothing");
        }
        for v in [RANDOMNESS_VERSION, RANDOMNESS_VERSION + 1] {
            let info = validate_at(&code, v).unwrap_or_else(|e| panic!("{name} under v{v}: {e}"));
            assert_eq!(info, validate(&code).unwrap());
            let mut host = host_at(v);
            host.upload(&code).unwrap().unwrap_or_else(|e| panic!("{name} under v{v}: {e}"));
        }
    }
    // The guests written before v3 import none of them: the same answer under both.
    for name in ["counter", "token", "escrow", "router"] {
        let code = guest(name);
        assert_eq!(validate_at(&code, FIRST_VERSION), validate_at(&code, RANDOMNESS_VERSION), "{name}");
        assert!(validate_at(&code, FIRST_VERSION).is_ok(), "{name}");
    }
}

// --- beacon_v1 ---

/// −1 until the epoch's value is derived, the value and its reveal count while the state keeps
/// it, and −1 again, for good, once it's pruned. A call that returns −1 writes nothing.
#[test]
fn beacon_is_minus_one_before_derivation_and_after_pruning() {
    let mut host = probe_host();
    let value = |e: u64| *blake3::hash(&e.to_le_bytes()).as_bytes();
    // Epochs 0..=5 derived; nothing yet for 6 (the next epoch's reveals are still landing).
    for e in 0..=5u64 {
        host.beacons.insert(e, Beacon { value: value(e), reveals: e as u32 });
    }
    host.height = 5 * host.epoch_blocks + 3;
    assert_eq!(beacon(&mut host, 6), (-1, UNTOUCHED), "not derived yet");
    assert_eq!(beacon(&mut host, 7), (-1, UNTOUCHED));
    assert_eq!(beacon(&mut host, i64::MAX), (-1, UNTOUCHED));
    assert_eq!(beacon(&mut host, -1), (-1, UNTOUCHED), "a negative epoch is no epoch");
    assert_eq!(beacon(&mut host, i64::MIN), (-1, UNTOUCHED));
    // The reveal counts: 0 is the hash-chain fallback, and a program can tell.
    for e in 0..=5u64 {
        assert_eq!(beacon(&mut host, e as i64), (e as i32, value(e)), "epoch {e}");
    }
    // The end of epoch 5 files epoch 6's value: readable from the next block on.
    host.beacons.insert(6, Beacon { value: value(6), reveals: 4 });
    host.height += host.epoch_blocks;
    assert_eq!(beacon(&mut host, 6), (4, value(6)));
    // Pruned: gone, and nothing brings it back.
    host.beacons.remove(&0);
    host.beacons.remove(&1);
    assert_eq!(beacon(&mut host, 0), (-1, UNTOUCHED), "pruned");
    assert_eq!(beacon(&mut host, 1), (-1, UNTOUCHED));
    assert_eq!(beacon(&mut host, 2), (2, value(2)), "the oldest one kept");
    for later in 1..=3 {
        host.height += later * host.epoch_blocks;
        assert_eq!(beacon(&mut host, 0), (-1, UNTOUCHED), "still pruned {later} epochs on");
    }
}

/// The out pointer is checked when there's a value to write, the fuel is charged either way, and
/// a view may read the beacon.
#[test]
fn beacon_charges_before_it_looks_and_checks_the_pointer_it_writes() {
    let mut host = probe_host();
    host.beacons.insert(3, Beacon { value: [7; 32], reveals: 2 });
    let at = |epoch: u64, out: u32| [&le(epoch)[..], &out.to_le_bytes()].concat();
    let kept = call(&mut host, "beacon", at(3, 64));
    let absent = call(&mut host, "beacon", at(4, 64));
    assert!(kept.ok() && absent.ok());
    assert_eq!(kept.fuel_used, absent.fuel_used, "the price doesn't depend on the answer");
    // The last 32 bytes of memory are fine; one further is out of bounds.
    assert!(call(&mut host, "beacon", at(3, 65_536 - 32)).ok());
    assert_eq!(trap(&call(&mut host, "beacon", at(3, 65_536 - 31))), Some(TrapKind::MemoryAccess));
    assert_eq!(trap(&call(&mut host, "beacon", at(3, u32::MAX))), Some(TrapKind::MemoryAccess));
    // Nothing is written for −1, so nothing is checked.
    assert!(call(&mut host, "beacon", at(4, u32::MAX)).ok());
    // Read-only frames read it too.
    host.set_method(addr("probe"), "beacon", MethodKind::View, false);
    let r = tx(&mut host, "alice", "probe", "beacon", at(3, 64), 0, MethodKind::View);
    assert_eq!(returned(&r)[..4], 2i32.to_le_bytes());
    assert_eq!((SCHEDULE_V0.host.beacon, SCHEDULE_V0.host.blake3, SCHEDULE_V0.host.blake3_per_byte), (1_000, 200, 2));
}

// --- epoch_v1 and blake3_v1 ---

#[test]
fn epoch_is_the_heights_epoch() {
    let mut host = probe_host();
    host.epoch_blocks = 60;
    for (height, epoch) in [(1, 0), (59, 0), (60, 1), (61, 1), (119, 1), (120, 2), (6_000_000, 100_000)] {
        host.height = height;
        assert_eq!(View::epoch(&host), epoch);
        assert_eq!(returned(&call(&mut host, "epoch", vec![])), le(epoch), "height {height}");
    }
}

/// BLAKE3 of exactly the bytes named, at 2 fuel a byte; a length past a program's memory or a
/// range outside it traps.
#[test]
fn blake3_hashes_what_it_is_given_and_charges_per_byte() {
    let mut host = probe_host();
    let mut fuel = Vec::new();
    for len in [0usize, 1, 31, 32, 63, 64, 65, 1023, 1024, 1025, 4096, 30_000] {
        let input: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
        let r = call(&mut host, "hash", input.clone());
        assert_eq!(returned(&r), blake3::hash(&input).as_bytes(), "{len} bytes");
        fuel.push((len as u64, r.fuel_used));
    }
    // Reading the input costs 1 a byte (`per_byte`), hashing it `blake3_per_byte`: nothing else
    // in the method depends on the length.
    let (_, empty) = fuel[0];
    let per_byte = SCHEDULE_V0.host.per_byte + SCHEDULE_V0.host.blake3_per_byte;
    for (len, used) in &fuel {
        assert_eq!(used - empty, per_byte * len, "{len} bytes");
    }
    let raw = |ptr: u32, len: u32, out: u32| [ptr.to_le_bytes(), len.to_le_bytes(), out.to_le_bytes()].concat();
    // All 64 KiB of this program's memory hashes; the digest may land inside what was hashed.
    assert!(call(&mut host, "hash_raw", raw(0, 65_536, 100)).ok());
    assert!(call(&mut host, "hash_raw", raw(65_536, 0, 65_536 - 32)).ok(), "an empty range at the end");
    assert_eq!(trap(&call(&mut host, "hash_raw", raw(1, 65_536, 100))), Some(TrapKind::MemoryAccess));
    assert_eq!(trap(&call(&mut host, "hash_raw", raw(0, 64, 65_536 - 31))), Some(TrapKind::MemoryAccess));
    assert_eq!(trap(&call(&mut host, "hash_raw", raw(u32::MAX, 2, 0))), Some(TrapKind::MemoryAccess));
    // Longer than any program's memory: misuse, refused before any fuel is charged for it.
    let too_long = abi::MAX_HASH_INPUT as u32 + 1;
    assert_eq!(trap(&call(&mut host, "hash_raw", raw(0, too_long, 0))), Some(TrapKind::Length));
    assert_eq!(trap(&call(&mut host, "hash_raw", raw(0, u32::MAX, 0))), Some(TrapKind::Length));
    assert_eq!(abi::MAX_HASH_INPUT, 2 << 20);
    // Within the cap but past this memory, on fuel that covers it: out of bounds.
    assert_eq!(trap(&call(&mut host, "hash_raw", raw(0, 65_537, 0))), Some(TrapKind::MemoryAccess));
}

// --- dice ---

const BANK: u64 = 100 * FANG;
const STAKE: u64 = FANG;
/// The dice's reveal floor in these tests.
const MIN_REVEALS: u32 = 2;
/// What the dice pays whoever settles a roll of `STAKE`: a hundredth of it.
const BOUNTY: u64 = STAKE / 100;
/// More than the storage deposit a closed roll hands back to the player who bonded it (about
/// 19,000 drops), and far less than a bounty or a stake.
const DEPOSIT: u64 = 100_000;

/// A chain of 10-block epochs with the dice created (`init` run as its initializer), its bank
/// funded, and alice and bob holding 50 FANG each.
fn dice_host() -> MemHost {
    let mut host = MemHost::new();
    host.fund(addr("alice"), 50 * FANG).fund(addr("bob"), 50 * FANG).fund(addr("carol"), 50 * FANG).fund(addr("house"), 200 * FANG);
    host.deploy(addr("dice"), &guest("dice"));
    host.set_method(addr("dice"), "init", MethodKind::Init, false);
    host.set_method(addr("dice"), "settle", MethodKind::Call, false);
    assert!(tx(&mut host, "house", "dice", "init", MIN_REVEALS.to_le_bytes().to_vec(), 0, MethodKind::Init).ok());
    assert!(tx(&mut host, "house", "dice", "fund", vec![], BANK, MethodKind::Call).ok());
    host
}

fn at_epoch(host: &mut MemHost, epoch: u64) {
    host.height = epoch * host.epoch_blocks + 1;
}

fn roll(host: &mut MemHost, who: &str, pick: u8, stake: u64) -> Receipt {
    tx(host, who, "dice", "roll", vec![pick], stake, MethodKind::Call)
}

fn settle(host: &mut MemHost, who: &str, id: u64) -> Receipt {
    tx(host, who, "dice", "settle", le(id), 0, MethodKind::Call)
}

/// The face request `id` settles on under `beacon`, computed here from the documented formula:
/// BLAKE3("dice/roll" ‖ the program's address ‖ id ‖ beacon), its first 8 bytes modulo 6, plus 1.
fn face(id: u64, beacon: &[u8; 32]) -> u8 {
    let preimage = [&b"dice/roll"[..], &addr("dice"), &le(id), beacon].concat();
    let value = blake3::hash(&preimage);
    1 + (u64::from_le_bytes(value.as_bytes()[..8].try_into().unwrap()) % 6) as u8
}

fn balance(host: &MemHost, who: &str) -> u64 {
    host.balance(&addr(who)).unwrap()
}

fn event(r: &Receipt) -> (&str, &[u8]) {
    let [Event { name, data, .. }] = &r.events[..] else { panic!("expected one event, got {:?}", r.events) };
    (name, data)
}

/// The safe pattern end to end: a roll names the beacon two epochs ahead, can't settle before it,
/// and then settles on a value nobody knew at the roll: a hit pays six stakes, a miss none, and
/// whoever settles either gets the bounty.
#[test]
fn a_roll_settles_two_epochs_later_on_the_beacon() {
    let mut host = dice_host();
    let value = [0x5e; 32];
    at_epoch(&mut host, 7);
    // Alice picks what request 0 will miss, bob what request 1 will hit: the test knows the
    // beacon it's about to file; nobody on a chain does at this height.
    let (miss, hit) = (face(0, &value) % 6 + 1, face(1, &value));
    let r = roll(&mut host, "alice", miss, STAKE);
    assert_eq!(returned(&r), le(0));
    let (name, data) = event(&r);
    assert_eq!(name, "roll");
    assert_eq!(data, [&le(0)[..], &addr("alice"), &le(STAKE), &[miss], &le(9), &le(9 + 32)].concat(), "target = epoch + 2, deadline = target + 32");
    assert_eq!(returned(&roll(&mut host, "bob", hit, STAKE)), le(1));
    let stored = host.storage(&addr("dice"), &[&b"r"[..], &le(1)].concat()).unwrap().value;
    assert_eq!(stored, [&addr("bob")[..], &le(STAKE), &[hit], &le(1), &le(9), &le(41), &MIN_REVEALS.to_le_bytes()].concat());
    assert_eq!(balance(&host, "dice"), BANK + 2 * STAKE);

    // Not in the roll's epoch, nor the next: the value isn't derived (and with the current
    // epoch's value on file, it still isn't the one the request names).
    host.beacons.insert(7, Beacon { value: [1; 32], reveals: 4 });
    for epoch in [7, 8] {
        at_epoch(&mut host, epoch);
        host.beacons.insert(epoch, Beacon { value: [epoch as u8; 32], reveals: 4 });
        for id in [0, 1] {
            assert_eq!(abort_code(&settle(&mut host, "alice", id)), Some(35), "not yet, in epoch {epoch}");
        }
    }
    // Epoch 9's value, with every reveal.
    host.beacons.insert(9, Beacon { value, reveals: 4 });
    at_epoch(&mut host, 9);
    let before = (balance(&host, "alice"), balance(&host, "bob"));
    // Anyone settles: here each settles the other's.
    let r = settle(&mut host, "bob", 0);
    assert_eq!(returned(&r), [face(0, &value)]);
    assert_eq!(event(&r), ("settled", &[&le(0)[..], &addr("alice"), &[face(0, &value)], &le(0), &addr("bob"), &le(BOUNTY)].concat()[..]));
    let r = settle(&mut host, "alice", 1);
    assert_eq!(returned(&r), [hit]);
    assert_eq!(event(&r), ("settled", &[&le(1)[..], &addr("bob"), &[hit], &le(6 * STAKE), &addr("alice"), &le(BOUNTY)].concat()[..]));
    // Each settler got the bounty, the miss's from the stake, the hit's from the bank (and each
    // player the deposit of their removed roll).
    assert_eq!(balance(&host, "dice"), BANK + 2 * STAKE - 6 * STAKE - 2 * BOUNTY);
    assert!(balance(&host, "bob") >= before.1 + 6 * STAKE + BOUNTY && balance(&host, "alice") >= before.0 + BOUNTY);
    assert!(balance(&host, "alice") < before.0 + STAKE, "a miss pays its player nothing");
    // Settled once: the rolls are gone, and what the bank owes is back to nothing.
    assert_eq!(abort_code(&settle(&mut host, "alice", 0)), Some(34));
    assert_eq!(abort_code(&settle(&mut host, "bob", 1)), Some(34));
    assert_eq!(host.storage(&addr("dice"), b"o").unwrap().value, le(0));
    assert_eq!(host.storage(&addr("dice"), b"\0random/next").unwrap().value, le(2));
}

/// The outcome is the request's own: two rolls on one epoch's beacon get unrelated values, and
/// the same roll reads the same value whoever settles it and whenever: before its deadline
/// (epoch 41) and after it, through the last epoch the chain keeps the value in (9 + 63).
#[test]
fn each_request_has_its_own_value_and_it_never_changes() {
    let value = [0x77; 32];
    let faces: Vec<u8> = (0..12).map(|id| face(id, &value)).collect();
    assert!(faces.windows(2).any(|w| w[0] != w[1]), "{faces:?}");
    for settle_at in [9, 10, 25, 41, 42, 72] {
        let mut host = dice_host();
        at_epoch(&mut host, 7);
        for id in 0..12u64 {
            assert_eq!(returned(&roll(&mut host, "alice", 1, STAKE / 10)), le(id));
        }
        host.beacons.insert(9, Beacon { value, reveals: MIN_REVEALS });
        at_epoch(&mut host, settle_at);
        for id in 0..12u64 {
            assert_eq!(returned(&settle(&mut host, "bob", id)), [faces[id as usize]], "request {id} settled in epoch {settle_at}");
        }
    }
}

/// With fewer reveals than the program asks for the beacon isn't used: the whole stake goes
/// back, whether the value came from one reveal or from none (the hash-chain fallback, which
/// anyone could have computed ahead), before the deadline or after it, and no bounty is paid.
#[test]
fn too_few_reveals_refund_the_stake() {
    for (reveals, settle_at) in [(0, 5), (1, 5), (1, 5 + 33), (0, 5 + 63)] {
        let mut host = dice_host();
        at_epoch(&mut host, 3);
        let value = [9; 32];
        // The pick that would have won on this value: it's refunded all the same.
        let pick = face(0, &value);
        assert!(roll(&mut host, "alice", pick, STAKE).ok());
        let after_roll = (balance(&host, "alice"), balance(&host, "bob"));
        host.beacons.insert(5, Beacon { value, reveals });
        at_epoch(&mut host, settle_at);
        let r = settle(&mut host, "bob", 0);
        assert_eq!(returned(&r), [0], "{reveals} reveals");
        assert_eq!(event(&r), ("refunded", &[&le(0)[..], &addr("alice"), &le(STAKE)].concat()[..]));
        let back = balance(&host, "alice") - after_roll.0;
        assert!((STAKE..STAKE + DEPOSIT).contains(&back), "the whole stake (and the roll's deposit), in epoch {settle_at}: {back}");
        assert_eq!(balance(&host, "bob"), after_roll.1, "no bounty on a refund");
        assert_eq!(balance(&host, "dice"), BANK);
        assert_eq!(host.storage(&addr("dice"), b"o").unwrap().value, le(0));
        assert_eq!(abort_code(&settle(&mut host, "bob", 0)), Some(34), "refunded once");
    }
    // Exactly the floor is enough.
    let mut host = dice_host();
    at_epoch(&mut host, 3);
    assert!(roll(&mut host, "alice", 1, STAKE).ok());
    host.beacons.insert(5, Beacon { value: [9; 32], reveals: MIN_REVEALS });
    at_epoch(&mut host, 5);
    assert_eq!(event(&settle(&mut host, "bob", 0)).0, "settled");
}

/// The free option is closed: a player whose roll missed gains nothing by waiting. Past the
/// deadline (target + 32) the roll still settles as a miss for as long as the chain keeps the
/// value (through target + 63), whoever sends the settle; once the value is gone the stake is
/// forfeit to the bank. The player never gets it back, and a hit left that long is lost too.
#[test]
fn a_loser_who_waits_past_the_deadline_gets_no_refund() {
    let value = [3; 32];
    let hit = face(0, &value);
    let miss = hit % 6 + 1;
    // (the epoch the settle lands in, whether the chain still keeps epoch 5's value)
    for (epoch, kept) in [(5 + 32, true), (5 + 33, true), (5 + 63, true), (5 + 64, false), (1_000, false)] {
        for (pick, settler) in [(miss, "alice"), (miss, "carol"), (hit, "alice"), (hit, "carol")] {
            let mut host = dice_host();
            at_epoch(&mut host, 3);
            assert!(roll(&mut host, "alice", pick, STAKE).ok());
            let after_roll = balance(&host, "alice");
            // The chain keeps 64 epochs of values: epoch 5's is there until epoch 5 + 63 ends.
            if kept {
                host.beacons.insert(5, Beacon { value, reveals: 4 });
            }
            at_epoch(&mut host, epoch);
            let r = settle(&mut host, settler, 0);
            let what = format!("epoch {epoch}, pick {pick}, settled by {settler}");
            let tail = [&addr(settler)[..], &le(BOUNTY)].concat();
            // All alice can get back: her roll's deposit, and the bounty if she settles it herself.
            let own = DEPOSIT + if settler == "alice" { BOUNTY } else { 0 };
            if kept && pick == hit {
                assert_eq!(returned(&r), [hit], "{what}");
                assert_eq!(event(&r), ("settled", &[&le(0)[..], &addr("alice"), &[hit], &le(6 * STAKE), &tail].concat()[..]), "{what}");
                assert_eq!(balance(&host, "dice"), BANK + STAKE - 6 * STAKE - BOUNTY, "{what}");
                continue;
            }
            // Every other case leaves the stake in the bank, less the settler's bounty.
            if kept {
                assert_eq!(returned(&r), [hit], "{what}");
                assert_eq!(event(&r), ("settled", &[&le(0)[..], &addr("alice"), &[hit], &le(0), &tail].concat()[..]), "{what}");
            } else {
                assert_eq!(returned(&r), [0], "{what}");
                assert_eq!(event(&r), ("forfeited", &[&le(0)[..], &addr("alice"), &le(STAKE), &tail].concat()[..]), "{what}");
            }
            assert_eq!(balance(&host, "dice"), BANK + STAKE - BOUNTY, "{what}");
            assert!(balance(&host, "alice") <= after_roll + own, "{what}: no refund, {} > {}", balance(&host, "alice"), after_roll + own);
            assert!(own < STAKE);
            assert_eq!(host.storage(&addr("dice"), b"o").unwrap().value, le(0), "{what}");
            assert_eq!(abort_code(&settle(&mut host, "alice", 0)), Some(34), "{what}: closed once");
        }
    }
}

/// Why anyone settles a stranger's miss: the settler is paid a hundredth of the stake, out of
/// the stake on a miss and a forfeit, out of the bank on a hit. A stake under 100 drops has no
/// bounty. What the bank owes counts each open roll's bounty, so a hit's settler is always paid.
#[test]
fn a_third_party_settling_earns_the_bounty() {
    let value = [0x42; 32];
    // (stake, the bounty it carries)
    for (stake, bounty) in [(STAKE, BOUNTY), (5 * STAKE, 5 * BOUNTY), (12_345, 123), (199, 1), (99, 0)] {
        for hit in [false, true] {
            let mut host = dice_host();
            at_epoch(&mut host, 3);
            let pick = if hit { face(0, &value) } else { face(0, &value) % 6 + 1 };
            assert!(roll(&mut host, "alice", pick, stake).ok());
            assert_eq!(host.storage(&addr("dice"), b"o").unwrap().value, le(6 * stake + bounty), "six stakes and the bounty");
            host.beacons.insert(5, Beacon { value, reveals: MIN_REVEALS });
            at_epoch(&mut host, 5);
            let (alice, carol) = (balance(&host, "alice"), balance(&host, "carol"));
            let r = settle(&mut host, "carol", 0);
            let payout = if hit { 6 * stake } else { 0 };
            assert_eq!(event(&r), ("settled", &[&le(0)[..], &addr("alice"), &[face(0, &value)], &le(payout), &addr("carol"), &le(bounty)].concat()[..]));
            assert!((alice + payout..alice + payout + DEPOSIT).contains(&balance(&host, "alice")), "stake {stake}, hit {hit}");
            assert_eq!(balance(&host, "carol"), carol + bounty, "stake {stake}, hit {hit}: the bounty and nothing else");
            assert_eq!(balance(&host, "dice"), BANK + stake - payout - bounty, "stake {stake}, hit {hit}");
            assert_eq!(host.storage(&addr("dice"), b"o").unwrap().value, le(0));
        }
    }
}

/// The dice's own rules: a face from 1 to 6, a stake, a bank that covers every open roll, and no
/// roll before `init`.
#[test]
fn a_roll_needs_a_pick_a_stake_and_a_bank_that_covers_it() {
    let mut host = dice_host();
    at_epoch(&mut host, 2);
    assert_eq!(abort_code(&roll(&mut host, "alice", 0, STAKE)), Some(32));
    assert_eq!(abort_code(&roll(&mut host, "alice", 7, STAKE)), Some(32));
    assert_eq!(abort_code(&roll(&mut host, "alice", 1, 0)), Some(31));
    assert_eq!(abort_code(&tx(&mut host, "alice", "dice", "roll", vec![1, 2], STAKE, MethodKind::Call)), Some(1));
    assert_eq!(trap(&tx(&mut host, "alice", "dice", "settle", le(0), 1, MethodKind::Call)), Some(TrapKind::NotPayable));
    assert_eq!(abort_code(&tx(&mut host, "alice", "dice", "init", 0u32.to_le_bytes().to_vec(), 0, MethodKind::Init)), Some(30));
    // 100 FANG of bank: a 20 FANG stake could pay 120 and a bounty of 0.2, and the bank and the
    // stake hold 120. A 19 FANG stake is covered: 114.19 of 119.
    assert_eq!(abort_code(&roll(&mut host, "alice", 1, 20 * FANG)), Some(33), "the hit's bounty isn't covered");
    assert!(roll(&mut host, "alice", 1, 19 * FANG).ok());
    assert_eq!(abort_code(&roll(&mut host, "bob", 1, FANG)), Some(33), "114.19 + 6.01 is more than the 120 there would be");
    assert_eq!(host.storage(&addr("dice"), b"o").unwrap().value, le(114 * FANG + 19 * FANG / 100));
    // Before `init` there's no reveal floor to roll under.
    let mut fresh = MemHost::new();
    fresh.fund(addr("alice"), 50 * FANG);
    fresh.deploy(addr("dice"), &guest("dice"));
    assert!(tx(&mut fresh, "alice", "dice", "fund", vec![], 10 * FANG, MethodKind::Call).ok());
    assert_eq!(abort_code(&roll(&mut fresh, "alice", 1, FANG)), Some(36));
}
