//! The native-credit guard and the value cap (docs/PROGRAMS-V0.md §7.1, §10.1's training wheels).
//! First on their own (WBS S5's modules): an ordinary recipient is never refused, whatever programs'
//! status; a program takes in a plain transfer only while programs are on and it accepts transfers,
//! within the cap on the FANG programs control (`value_held + sponsored`); every other credit to a
//! program is refused; a change that doesn't raise the control is never refused by the cap. Each
//! credit runs as the `Transfer` arm does: `j_credit`, then the guard in the same journal, rolled
//! back on `Err`, called by the paths the sites name (`crate::programs::{native_credit,
//! check_credit, Credit}`). Then at the four sites through their hooks (WBS S5's), end to end: a
//! `Transfer` and an `AwardHunt` through `State::apply`, a `TreasurySpend` when it's proposed and
//! when it runs, and an `Unshield` through a dev chain's mempool and blocks; at each a program is
//! refused and an ordinary account is paid.

use super::{check_cap, control};
use crate::chain::{Account, Journal, State, ValidatorRecord, ValidatorStatus};
use crate::crypto::render_address;
use crate::programs::state::{blob_key, storage_key, BlobRef, CodeRecord, Entry, InterfaceRecord, Pause, ProgramRecord, Versioned, BLOB_CODE, BLOB_INTERFACE};
use crate::programs::{check_credit, check_invariants, native_credit, on_activate, status, Credit, Status, RECORD_BYTES};
use crate::types::{fmt_fang, DROPS_PER_FANG};
use crate::upgrade::{Feature, ScheduledUpgrade, PROGRAMS_FEATURE, PROGRAMS_VERSION};
use anyhow::Result;
use blockula_vm::storage::capacity;

const FANG: u64 = DROPS_PER_FANG;
/// One of W1's guests, which passes validation: what a stored code record holds.
const WASM: &[u8] = include_bytes!("../../../vm/tests/programs/counter.wasm");
/// The program's 32 address bytes, and the key of the entry it sponsors.
const PROGRAM: [u8; 32] = [7; 32];
const KEY: &[u8] = b"sp";
const H: u64 = 10;

fn program(st: &State) -> String {
    render_address(st.params.prefix(), &PROGRAM)
}

fn alice(st: &State) -> String {
    render_address(st.params.prefix(), &[1; 32])
}

/// A state at v2 with programs on and one program, holding `balance` in its account and `sponsored`
/// in the deposit of an entry it paid itself (`SPONSOR`), with the value cap at `cap`: every total
/// consistent (programs' invariants hold). Alice has 1,000 FANG to send.
fn with_program(accepts_transfers: bool, balance: u64, sponsored: u64, cap: u64) -> State {
    let mut st = State::default();
    st.upgrade.schedule = vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 1 }];
    st.upgrade.features.insert(PROGRAMS_FEATURE.into(), Feature { activated_at: 1 });
    on_activate(&mut st, None);
    let (wasm_hash, interface_hash) = ([3; 32], [4; 32]);
    let interface = b"\x01an interface's bytes".to_vec();
    let exports = blockula_vm::validate::validate(WASM).unwrap().exports;
    let entry = Entry { value: vec![0xab; 10], payer: PROGRAM, deposit: sponsored, capacity: capacity(KEY.len(), 10) };
    let entry_bytes = u64::from(entry.capacity);
    let (program, alice) = (program(&st), alice(&st));
    let p = &mut st.programs;
    p.code.insert(wasm_hash, Versioned(CodeRecord { wasm: WASM.to_vec(), exports }));
    p.interfaces.insert(interface_hash, Versioned(InterfaceRecord::new(interface.clone())));
    let blob = |size: usize| Versioned(BlobRef { uploader: alice.clone(), deposit: 0, size: size as u32, refs: 1, uploaded_at: 1, last_used_at: 1 });
    p.blob_refs.insert(blob_key(BLOB_CODE, &wasm_hash), blob(WASM.len()));
    p.blob_refs.insert(blob_key(BLOB_INTERFACE, &interface_hash), blob(interface.len()));
    p.storage.insert(storage_key(&PROGRAM, KEY), Versioned(entry));
    let record = ProgramRecord {
        creator: alice.clone(),
        created_at: 2,
        wasm_hash,
        interface_hash,
        version: 1,
        authority: None,
        upgrade_delay_blocks: 0,
        policy: 0,
        accepts_transfers,
        items: 1,
        bytes: entry_bytes,
        deposit: sponsored,
        sponsored,
        record_deposit: 0,
        record_payer: alice.clone(),
    };
    st.programs.records.insert(program.clone(), Versioned(record));
    st.accounts.insert(program, Account { balance, ..Default::default() });
    st.accounts.insert(alice, Account { balance: 1_000 * FANG, ..Default::default() });
    let m = st.programs.meta.as_mut().unwrap();
    m.programs = 1;
    m.state_bytes = (WASM.len() + interface.len()) as u64 + entry_bytes + RECORD_BYTES;
    m.value_held = balance;
    m.sponsored = sponsored;
    m.escrow = sponsored;
    m.params.insert("program_value_cap".into(), cap);
    check_invariants(&st).unwrap();
    assert_eq!(status(&st, H), Status::On);
    st
}

/// `with_program` with transfers accepted, 50 FANG held, nothing sponsored, the default cap.
fn on() -> State {
    with_program(true, 50 * FANG, 0, 10_000_000 * FANG)
}

/// The same program while a pause runs over `H`.
fn paused() -> State {
    let mut st = on();
    // A pause names its signers, validators (programs' invariants check it).
    let v = render_address(st.params.prefix(), &[9; 32]);
    let record = ValidatorRecord { address: v.clone(), owner: v.clone(), consensus_key: String::new(), p2p_addr: String::new(), http_addr: String::new(), status: ValidatorStatus::Active, registered_at: 0, approved_at: None, approved_by: None, active_from: Some(0), removed_at: None, removed_by: None, active_until: None, commission_bps: 0 };
    st.validators.insert(v.clone(), record);
    st.programs.meta.as_mut().unwrap().pause = Some(Pause { from: H, until: H + 5, started: H, signers: vec![v] });
    assert_eq!(status(&st, H), Status::Paused { until: H + 5 });
    st
}

/// The same program once programs are switched off (META kept).
fn exit_only() -> State {
    let mut st = on();
    st.upgrade.features.remove(PROGRAMS_FEATURE);
    assert_eq!(status(&st, H), Status::ExitOnly);
    st
}

/// A native credit as the `Transfer` arm runs it: Alice pays `to` `amount`, then the guard, in the
/// action's journal, which an `Err` rolls back. Programs' invariants hold either way.
fn credit(st: &mut State, to: &str, amount: u64, kind: Credit) -> Result<()> {
    let read_only = check_credit(st, to, amount, kind, H).map_err(|e| e.to_string());
    let mut j = Journal::default();
    let alice = alice(st);
    let r = st.j_debit(&mut j, &alice, amount).and_then(|()| {
        st.j_credit(&mut j, to, amount);
        native_credit(st, &mut j, to, amount, kind, H)
    });
    assert_eq!(r.as_ref().map(|_| ()).map_err(|e| e.to_string()), read_only, "check_credit is the same rule");
    if r.is_err() {
        st.rollback(j);
    }
    if st.programs.meta.is_some() && status(st, H) != Status::Dormant {
        check_invariants(st).expect("programs' invariants hold after a credit");
    }
    r
}

fn value_held(st: &State) -> u64 {
    st.programs.meta.as_ref().unwrap().value_held
}

/// For a recipient that isn't a program, the guard does nothing and passes, of either kind and
/// whatever programs' status: below v2, at v2 before programs exist, on, paused and exit-only.
#[test]
fn an_ordinary_recipient_is_never_refused() {
    let mut v1 = State::default();
    v1.accounts.insert(alice(&v1), Account { balance: 1_000 * FANG, ..Default::default() });
    let mut dormant = v1.clone();
    dormant.upgrade.schedule = vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 1 }];
    for (label, st) in [("v1", v1), ("v2, before programs", dormant), ("on", on()), ("paused", paused()), ("exit-only", exit_only())] {
        let bob = render_address(st.params.prefix(), &[2; 32]);
        for kind in [Credit::Transfer, Credit::Other] {
            let mut st = st.clone();
            let meta = st.programs.meta.clone();
            credit(&mut st, &bob, 5 * FANG, kind).unwrap_or_else(|e| panic!("{label}, {kind:?}: {e}"));
            assert_eq!(st.programs.meta, meta, "{label}, {kind:?}: programs' state untouched");
            assert_eq!(st.account(&bob).balance, 5 * FANG, "{label}, {kind:?}");
        }
    }
}

/// While programs are on, a program that accepts transfers takes in a plain transfer: its balance
/// and `value_held` grow by the amount (journaled with the credit), so `value_held` stays Σ program
/// balances. A zero transfer moves nothing.
#[test]
fn a_program_takes_in_a_transfer_while_programs_are_on() {
    let mut st = on();
    let p = program(&st);
    credit(&mut st, &p, 7 * FANG, Credit::Transfer).unwrap();
    assert_eq!((st.account(&p).balance, value_held(&st)), (57 * FANG, 57 * FANG));
    credit(&mut st, &p, 0, Credit::Transfer).unwrap();
    assert_eq!(value_held(&st), 57 * FANG);
    // Rolled back with its transaction, the guard's write goes too.
    let mut j = Journal::default();
    st.j_credit(&mut j, &p, FANG);
    native_credit(&mut st, &mut j, &p, FANG, Credit::Transfer, H).unwrap();
    assert_eq!(value_held(&st), 58 * FANG);
    st.rollback(j);
    assert_eq!((st.account(&p).balance, value_held(&st)), (57 * FANG, 57 * FANG));
}

/// No FANG goes into a program while a pause runs (stricter than docs/PROGRAMS.md, A35), nor in
/// exit-only, nor into a program that doesn't accept plain transfers: the transfer fails, and
/// nothing moves.
#[test]
fn a_program_takes_in_no_transfer_paused_exit_only_or_closed_to_transfers() {
    let p = program(&on());
    let short = crate::chain::short(&p);
    let cases = [
        (paused(), format!("programs are paused until block {}: no FANG goes into a program while a pause runs", H + 5)),
        (exit_only(), format!("programs are exit-only: FANG may leave {short}, a program, but no transfer pays one")),
        (with_program(false, 50 * FANG, 0, 10_000_000 * FANG), format!("{short} is a program that doesn't accept plain transfers")),
    ];
    for (mut st, refusal) in cases {
        let before = (st.account(&p).balance, st.account(&alice(&st)).balance, st.programs.meta.clone());
        for amount in [0, 5 * FANG] {
            assert_eq!(credit(&mut st, &p, amount, Credit::Transfer).unwrap_err().to_string(), refusal);
            assert_eq!((st.account(&p).balance, st.account(&alice(&st)).balance, st.programs.meta.clone()), before, "{refusal}");
        }
    }
}

/// Every other native credit that pays a named recipient (a hunt award, a treasury spend, an
/// unshield) is refused for a program, whatever programs' status but dormant.
#[test]
fn other_credits_never_pay_a_program() {
    for (label, mut st) in [("on", on()), ("paused", paused()), ("exit-only", exit_only())] {
        let p = program(&st);
        let before = st.programs.meta.clone();
        let e = credit(&mut st, &p, 5 * FANG, Credit::Other).unwrap_err().to_string();
        assert_eq!(e, format!("{} is a program: programs take in native FANG only from a plain transfer, never from a hunt award, a treasury spend or an unshield", crate::chain::short(&p)), "{label}");
        assert_eq!((st.account(&p).balance, st.programs.meta.clone()), (50 * FANG, before), "{label}");
    }
}

/// The cap bounds the FANG programs control, `value_held + sponsored` (A40): a transfer up to the
/// cap is taken in, one past it fails, and a program that bonded its balance into an entry it
/// sponsors can't take in more for it. A transfer that doesn't raise the control (zero) passes at
/// the cap, and above a cap lowered under the control.
#[test]
fn the_cap_bounds_the_control_not_value_held_alone() {
    let cap = 100 * FANG;
    let mut st = with_program(true, 60 * FANG, 30 * FANG, cap);
    let p = program(&st);
    assert_eq!(control(st.programs.meta.as_ref().unwrap()).unwrap(), 90 * FANG);
    credit(&mut st, &p, 10 * FANG, Credit::Transfer).unwrap();
    assert_eq!(value_held(&st), 70 * FANG, "exactly at the cap");
    let past = format!("programs may control at most {} (program_value_cap), and this would take them to {}", fmt_fang(cap), fmt_fang(cap + 1));
    assert_eq!(credit(&mut st, &p, 1, Credit::Transfer).unwrap_err().to_string(), past);
    credit(&mut st, &p, 0, Credit::Transfer).unwrap();

    // The program's 100 FANG are nearly all in its own deposit: `value_held` is far below the cap,
    // the control is at it.
    let mut parked = with_program(true, 5 * FANG, 95 * FANG, cap);
    assert_eq!(credit(&mut parked, &p, 1, Credit::Transfer).unwrap_err().to_string(), past);
    assert_eq!(value_held(&parked), 5 * FANG);

    // The council lowered the cap under the control: nothing more comes in, a zero transfer passes.
    let mut lowered = with_program(true, 60 * FANG, 30 * FANG, 50 * FANG);
    credit(&mut lowered, &p, 0, Credit::Transfer).unwrap();
    assert!(credit(&mut lowered, &p, FANG, Credit::Transfer).unwrap_err().to_string().starts_with(&format!("programs may control at most {}", fmt_fang(50 * FANG))));
}

/// `check_cap` (the rule a program's commit and an adoption share with the transfer guard) refuses
/// only a change that raises the control and leaves it past the cap: payouts, and a sponsored
/// deposit or its refund (which leave the control as it was), pass at the cap and above it.
#[test]
fn the_cap_refuses_only_a_rise_past_it() {
    let st = with_program(true, 0, 0, 100);
    for (before, after, ok) in [(50, 100, true), (50, 101, false), (100, 100, true), (150, 150, true), (150, 120, true), (150, 151, false), (101, 0, true), (0, 0, true)] {
        assert_eq!(check_cap(&st, before, after).is_ok(), ok, "{before} → {after}");
    }
    let closed = with_program(true, 0, 0, 0);
    assert!(check_cap(&closed, 0, 0).is_ok() && check_cap(&closed, 0, 1).is_err(), "a zero cap: nothing more comes in");
}

/// The cap is written twice, once here for a transfer (`check_cap`) and once in S4a's
/// `commit::check_control` for a call's commit and an adoption, which reads the control from META
/// after the change. While programs are on they decide alike on the same change, whichever way the
/// control after it splits between `value_held` and `sponsored`, for caps up to the largest the
/// parameter takes and totals well past it. (In exit-only they differ on purpose: the guard refuses
/// every transfer to a program, the commit only a rise; §7.1, §7.3.)
#[test]
fn the_transfer_cap_and_the_commit_cap_agree() {
    let (top, big) = (crate::params::MAX_SUPPLY, u64::MAX / 2);
    let changes = [(0, 0), (0, 1), (50, 100), (50, 101), (100, 100), (150, 150), (150, 120), (150, 151), (101, 0), (99, 100), (100, 101), (top - 1, top), (top, top + 1), (big, big), (big - 1, big), (big, big + 1)];
    for cap in [0, 1, 100, top] {
        for (before, after) in changes {
            for split in [0, after / 2, after] {
                let mut st = with_program(true, 0, 0, cap);
                let m = st.programs.meta.as_mut().unwrap();
                (m.value_held, m.sponsored) = (after - split, split);
                let commit = super::super::commit::check_control(&st, u128::from(before), H);
                assert_eq!(check_cap(&st, before, after).is_ok(), commit.is_ok(), "cap {cap}: {before} → {after} ({split} sponsored)");
            }
        }
    }
}

/// Like every hook of §1.2 the guard is gated on `status`, not on "no program exists": below v2 it
/// reads no record, so even a state holding one (which `check_invariants` refuses) wakes nothing.
#[test]
fn the_guard_is_gated_on_status() {
    let mut st = on();
    st.upgrade.schedule = vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 100 }];
    assert_eq!(status(&st, H), Status::Dormant);
    let p = program(&st);
    let meta = st.programs.meta.clone();
    for kind in [Credit::Transfer, Credit::Other] {
        let mut st = st.clone();
        credit(&mut st, &p, 5 * FANG, kind).unwrap();
        assert_eq!(st.programs.meta, meta, "{kind:?}");
    }
}

/// The guard takes the recipient's address on this chain, never what a transaction may name it by
/// (S5m-R2): records are keyed by that address, so a `.boo` name, its uppercase alias or the same
/// bytes under another network's prefix would miss the program's record and pass as "not a
/// program", past the cap and the pause. Every credit site resolves its recipient first (admission
/// holds the addresses an action names to this chain's prefix); a debug build asserts it at every
/// call, dormant or not. Release builds compile the assertion out, and this test with it.
#[cfg(debug_assertions)]
#[test]
fn the_guard_takes_an_address_never_a_name() {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let on = on();
    let mut dormant = on.clone();
    dormant.upgrade.schedule = vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 100 }];
    let p = program(&on);
    let other_network = render_address(crate::crypto::TESTNET_HRP, &PROGRAM);
    assert_ne!(on.params.prefix(), crate::crypto::TESTNET_HRP);
    for (label, st) in [("on", on), ("dormant", dormant)] {
        for to in ["prog.boo".to_string(), "prog".to_string(), p.to_uppercase(), other_network.clone()] {
            for kind in [Credit::Transfer, Credit::Other] {
                let expected = format!("the native-credit guard takes the recipient's address on this chain, not {to:?}: resolve it first");
                let message = |e: Box<dyn std::any::Any + Send>| e.downcast::<String>().map(|s| *s).unwrap_or_default();
                let read = catch_unwind(AssertUnwindSafe(|| check_credit(&st, &to, 5 * FANG, kind, H).is_ok()));
                assert_eq!(read.map_err(message), Err(expected.clone()), "check_credit, {label}, {to}, {kind:?}");
                let mut written = st.clone();
                let write = catch_unwind(AssertUnwindSafe(|| native_credit(&mut written, &mut Journal::default(), &to, 5 * FANG, kind, H).is_ok()));
                assert_eq!(write.map_err(message), Err(expected), "native_credit, {label}, {to}, {kind:?}");
            }
        }
    }
}

// ---- at the four sites, through their hooks (WBS S5) ----

mod sites {
    use crate::chain::{now_ms, short, Chain, Genesis, HuntStatus, Outcome, State, SummonOpts};
    use crate::crypto::{self, Keys};
    use crate::governance::{digest_of, ProposalStatus};
    use crate::programs::fixtures as fx;
    use crate::programs::state::Pause;
    use crate::programs::{digest, is_program};
    use crate::types::{fmt_fang, Action, ProposalKind, DROPS_PER_FANG, MIN_HUNT};
    use crate::upgrade::{ScheduledUpgrade, PROGRAMS_FEATURE, PROGRAMS_VERSION};
    use std::path::PathBuf;

    const FANG: u64 = DROPS_PER_FANG;
    /// The height the transactions below run at.
    const H: u64 = 10;

    /// What the guard says when a credit other than a plain transfer would pay program `p`.
    fn never(p: &str) -> String {
        format!("{} is a program: programs take in native FANG only from a plain transfer, never from a hunt award, a treasury spend or an unshield", short(p))
    }

    /// Fixtures' chain with programs on (Alice and Bob funded, one validator), proposals bonding 10
    /// FANG and the treasury's spend gate open; the counter's code stored at block 1 and two programs
    /// of it created at block 2 (by Bob's account): `open`, which accepts plain transfers, and
    /// `closed`, which doesn't.
    fn sites() -> (State, String, String) {
        let mut g = fx::genesis(&[PROGRAMS_FEATURE]);
        g.param_overrides.extend([("proposal_deposit".to_string(), 10 * FANG), ("governance_spend_height".to_string(), 0)]);
        g.check().unwrap();
        let mut st = g.initial_state();
        let blobs = fx::store(&mut st, fx::COUNTER.0, fx::COUNTER.1, 1);
        let (open, closed) = (fx::program_of(fx::BOB, 0), fx::program_of(fx::BOB, 1));
        fx::install(&mut st, &open, blobs, 2);
        fx::install(&mut st, &closed, blobs, 2);
        st.programs.records.get_mut(&open).unwrap().accepts_transfers = true;
        st.check_invariants().unwrap();
        (st, open, closed)
    }

    /// `action` from `key_seed`'s account through `State::apply` at block `H` (admission, the fee phase,
    /// the action): its outcome. A failure took the sender's fee and nothing else (programs' state
    /// included). The state's invariants hold after.
    fn run(st: &mut State, key_seed: u8, action: Action) -> Outcome {
        let t = fx::tx(st, key_seed, action, H);
        let charge = crate::econ::check_fee(st, &t, H).unwrap();
        let (paid, meta) = (st.account(&t.from).balance, st.programs.meta.clone());
        let out = st.apply(&t, fx::CHAIN, &fx::addr(fx::VALIDATOR), H).unwrap_or_else(|e| panic!("admitted: {e}"));
        if let Outcome::Failed(_) = out {
            assert_eq!((st.account(&t.from).balance, &st.programs.meta), (paid - charge, &meta), "a failure moves nothing but its fee");
        }
        st.check_invariants().unwrap();
        out
    }

    /// Alice pays `to` `amount`: the outcome; applied, `to` has it.
    fn pay(st: &mut State, to: &str, amount: u64) -> Outcome {
        let before = st.account(to).balance;
        let out = run(st, fx::ALICE, Action::Transfer { to: to.into(), amount });
        let expected = if out == Outcome::Applied { before + amount } else { before };
        assert_eq!(st.account(to).balance, expected);
        out
    }

    fn value_held(st: &State) -> u64 {
        st.programs.meta.as_ref().unwrap().value_held
    }

    /// The `Transfer` site (`State::apply_action`'s `Transfer` arm): a program that accepts plain
    /// transfers takes one in (its balance and `value_held` grow), up to the value cap on the control;
    /// past the cap, while a pause runs, in exit-only, and into a program closed to transfers, the
    /// transfer fails, charged its fee, and nothing else moves. Bob, an ordinary account, is paid in
    /// every case.
    #[test]
    fn the_transfer_site() {
        let (base, open, closed) = sites();
        let bob = fx::addr(fx::BOB);
        let mut st = base.clone();
        assert_eq!(pay(&mut st, &open, 5 * FANG), Outcome::Applied);
        assert_eq!(value_held(&st), 5 * FANG);
        assert_eq!(pay(&mut st, &bob, 5 * FANG), Outcome::Applied);
        assert_eq!(pay(&mut st, &closed, FANG), Outcome::Failed(format!("{} is a program that doesn't accept plain transfers", short(&closed))));
        // The cap on the control: up to it, then past it by a drop.
        st.programs.meta.as_mut().unwrap().params.insert("program_value_cap".into(), 7 * FANG);
        assert_eq!(pay(&mut st, &open, 2 * FANG), Outcome::Applied);
        assert_eq!(value_held(&st), 7 * FANG, "at the cap");
        assert_eq!(pay(&mut st, &open, 1), Outcome::Failed(format!("programs may control at most {} (program_value_cap), and this would take them to {}", fmt_fang(7 * FANG), fmt_fang(7 * FANG + 1))));
        assert_eq!(pay(&mut st, &bob, FANG), Outcome::Applied);

        let mut paused = base.clone();
        paused.programs.meta.as_mut().unwrap().pause = Some(Pause { from: H, until: H + 5, started: H, signers: vec![fx::addr(fx::VALIDATOR)] });
        assert_eq!(pay(&mut paused, &open, FANG), Outcome::Failed(format!("programs are paused until block {}: no FANG goes into a program while a pause runs", H + 5)));
        assert_eq!(pay(&mut paused, &bob, FANG), Outcome::Applied);

        let mut exit_only = base.clone();
        exit_only.upgrade.features.remove(PROGRAMS_FEATURE);
        assert_eq!(pay(&mut exit_only, &open, FANG), Outcome::Failed(format!("programs are exit-only: FANG may leave {}, a program, but no transfer pays one", short(&open))));
        assert_eq!(pay(&mut exit_only, &bob, FANG), Outcome::Applied);
    }

    /// The `AwardHunt` site: a hunt's creator can't award it to a program (the award fails, charged
    /// its fee; the hunt stays open with its escrow), and awards it to Bob.
    #[test]
    fn the_award_hunt_site() {
        let (mut st, open, _) = sites();
        let hunt = Action::CreateHunt { repo: "blockula/blockula".into(), issue: 1, amount: MIN_HUNT, expires_at: H + 1_000, title: "a fix".into() };
        assert_eq!(run(&mut st, fx::ALICE, hunt), Outcome::Applied);
        let id = st.next_hunt_id - 1;
        let award = |to: &str| Action::AwardHunt { hunt_id: id, to: to.into(), pr: 7, familiar_key: None };
        let open_before = st.account(&open).balance;
        assert_eq!(run(&mut st, fx::ALICE, award(&open)), Outcome::Failed(never(&open)));
        assert_eq!((st.account(&open).balance, st.hunts.get(&id).unwrap().status), (open_before, HuntStatus::Open));
        let bob = fx::addr(fx::BOB);
        let bob_before = st.account(&bob).balance;
        assert_eq!(run(&mut st, fx::ALICE, award(&bob)), Outcome::Applied);
        assert_eq!((st.account(&bob).balance, st.hunts.get(&id).unwrap().status), (bob_before + MIN_HUNT, HuntStatus::Awarded));
    }

    /// The `TreasurySpend` site when it's proposed (`governance::prepare_kind`): a spend to a program
    /// is refused (the `Propose` fails, charged its fee: no proposal, no deposit bonded); one to Bob
    /// is proposed.
    #[test]
    fn a_treasury_spend_to_a_program_is_refused_when_proposed() {
        let (mut st, open, _) = sites();
        let deposit = st.chain_params.proposal_deposit;
        let propose = |to: &str| Action::Propose { kind: ProposalKind::TreasurySpend { to: to.into(), amount: FANG }, deposit, title: "pay".into(), url: String::new() };
        let next = st.governance.next_proposal_id;
        assert_eq!(run(&mut st, fx::ALICE, propose(&open)), Outcome::Failed(never(&open)));
        assert_eq!(st.governance.next_proposal_id, next, "no proposal");
        assert_eq!(run(&mut st, fx::ALICE, propose(&fx::addr(fx::BOB))), Outcome::Applied);
        assert!(matches!(st.governance.proposals.get(&next).map(|p| (&p.kind, p.status)), Some((ProposalKind::TreasurySpend { .. }, ProposalStatus::Voting))));
    }

    /// The `TreasurySpend` site when it runs (`governance::run_kind`), on a dev chain whose council
    /// (its one validator) passes two spends: one to the address the validator's next create makes,
    /// a program by the time the spend runs, fails when it runs (the treasury keeps its FANG, and the
    /// program gets none); the other, to the validator's own account, pays.
    #[test]
    fn a_treasury_spend_to_what_became_a_program_fails_when_it_runs() {
        let params = [("proposal_deposit", 10 * FANG), ("voting_blocks", 10), ("timelock_blocks", 3), ("lock_period_blocks", 5), ("quorum_floor", 0), ("governance_spend_height", 0)];
        let (home, mut c) = fx::dev_chain("treasury-site", &params);
        let me = c.keys.as_ref().unwrap().clone_keys();
        let addr = c.me().unwrap();
        let n = c.next_nonce(&addr);
        // The validator's create, at nonce n + 3, makes this program.
        let program = digest::program_address(c.state.params.prefix(), &crypto::address_bytes(&addr).unwrap(), n + 3);
        let spend = |to: &str| Action::Propose { kind: ProposalKind::TreasurySpend { to: to.into(), amount: 3 * FANG }, deposit: 10 * FANG, title: "pay".into(), url: String::new() };
        let id = c.state.governance.next_proposal_id;
        let first = vec![fx::signed(&c, &me, n, 10 * FANG, spend(&program)), fx::signed(&c, &me, n + 1, 10 * FANG, spend(&addr)), fx::signed(&c, &me, n + 2, 10 * FANG, fx::upload(fx::COUNTER.0, fx::COUNTER.1, 10 * FANG))];
        c.produce_block_with(first).unwrap();
        assert!(c.last_block().unwrap().failed.is_empty() && c.last_block().unwrap().txs.len() == 3);
        let vote = |c: &Chain, p: u64| Action::Vote { proposal_id: p, aye: true, amount: FANG, conviction: 0, proposal_digest: digest_of(&c.state.governance.proposals[&p]) };
        let (code, iface) = (digest::wasm_hash(fx::COUNTER.0), digest::interface_hash(fx::COUNTER.1));
        let second = vec![fx::signed(&c, &me, n + 3, 10 * FANG, fx::create(&code, &iface)), fx::signed(&c, &me, n + 4, 10 * FANG, vote(&c, id)), fx::signed(&c, &me, n + 5, 10 * FANG, vote(&c, id + 1))];
        c.produce_block_with(second).unwrap();
        assert!(c.last_block().unwrap().failed.is_empty() && c.last_block().unwrap().txs.len() == 3);
        assert!(is_program(&c.state, &program));
        let treasury = c.state.treasury();
        let treasury_before = c.state.account(&treasury).balance;
        let closed = |c: &Chain, p: u64| !matches!(c.state.governance.proposals[&p].status, ProposalStatus::Voting | ProposalStatus::Queued);
        while !(closed(&c, id) && closed(&c, id + 1)) {
            assert!(c.height() < 60, "the proposals never ran");
            c.produce_block().unwrap();
        }
        let (failed, paid) = (&c.state.governance.proposals[&id], &c.state.governance.proposals[&(id + 1)]);
        assert_eq!((failed.status, failed.outcome.clone()), (ProposalStatus::Failed, format!("failed when it ran: {}", never(&program))));
        assert_eq!((paid.status, paid.outcome.clone()), (ProposalStatus::Executed, format!("paid {} to {}", fmt_fang(3 * FANG), short(&addr))));
        assert_eq!(c.state.account(&program).balance, 0, "the program got nothing");
        assert_eq!(c.state.account(&treasury).balance, treasury_before - 3 * FANG, "the treasury paid one spend");
        drop(c);
        std::fs::remove_dir_all(home).ok();
    }

    /// A dev chain (one validator, `boo` addresses) born at v2 with Ghost Mode and programs on at
    /// genesis and a 1,000 FANG shielded pool cap, opened as its validator.
    fn ghost_dev_chain(tag: &str) -> (PathBuf, Chain) {
        let home = std::env::temp_dir().join(format!("blockula-programs-s5-{tag}-{}-{}", now_ms(), std::process::id()));
        let mut o = SummonOpts::new("programs-ghost-1", 1);
        o.address_prefix = crypto::ADDRESS_HRP.into();
        o.params.insert("ghost_pool_cap".into(), 1_000 * FANG);
        Chain::summon(&home, &o).unwrap();
        let path = home.join("genesis.json");
        let mut g: Genesis = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        g.upgrades = vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 1 }];
        g.features = vec![crate::ghost::FEATURE.into(), PROGRAMS_FEATURE.into()];
        g.check().unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&g).unwrap()).unwrap();
        let c = Chain::open(&home).unwrap();
        (home, c)
    }

    /// The `Unshield` site (`ghost::apply_unsigned`, once the recipient is resolved): an unshield to a
    /// program is unadmittable, before any effect (refused at the mempool's door and left out of a
    /// block: the program and the pool are as they were), and the same note unshielded to an ordinary
    /// account pays it.
    #[test]
    fn an_unshield_to_a_program_is_unadmittable() {
        let (home, mut c) = ghost_dev_chain("unshield-site");
        let me = c.keys.as_ref().unwrap().clone_keys();
        let addr = c.me().unwrap();
        let mut w = crate::ghost::tests::Wallet::new("unshield-site");
        let key = blockula_ghost::SpendingKey::from_seed([5; 32]);
        let (shield, note) = w.shield(&c, &me, 20 * FANG, &key);
        let upload = fx::signed(&c, &me, c.next_nonce(&addr) + 1, 10 * FANG, fx::upload(fx::COUNTER.0, fx::COUNTER.1, 10 * FANG));
        c.produce_block_with(vec![shield, upload]).unwrap();
        let nonce = c.next_nonce(&addr);
        let program = digest::program_address(c.state.params.prefix(), &crypto::address_bytes(&addr).unwrap(), nonce);
        let create = fx::signed(&c, &me, nonce, 10 * FANG, fx::create(&digest::wasm_hash(fx::COUNTER.0), &digest::interface_hash(fx::COUNTER.1)));
        c.produce_block_with(vec![create]).unwrap();
        assert!(is_program(&c.state, &program) && c.last_block().unwrap().failed.is_empty());

        let to_program = w.unshield(&c, &key, note, &program, 5 * FANG);
        let e = c.submit(to_program.clone()).unwrap_err().to_string();
        assert!(e.contains(&never(&program)), "{e}");
        let pool = c.state.ghost.meta.pool;
        c.produce_block_with(vec![to_program.clone()]).unwrap();
        assert!(!c.last_block().unwrap().txs.iter().any(|t| t.id() == to_program.id()), "left out of the block");
        assert_eq!((c.state.account(&program).balance, c.state.ghost.meta.pool), (0, pool));

        let carol = Keys::from_seed([73; 32]).address();
        let to_carol = w.unshield(&c, &key, note, &carol, 5 * FANG);
        c.submit(to_carol.clone()).unwrap();
        c.produce_block().unwrap();
        assert!(c.last_block().unwrap().txs.iter().any(|t| t.id() == to_carol.id()), "included");
        assert_eq!(c.state.account(&carol).balance, 5 * FANG);
        drop(c);
        std::fs::remove_dir_all(home).ok();
    }
}
