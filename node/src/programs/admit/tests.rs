//! Programs' admission (WBS S3's acceptance and T's), on `admit` directly and through
//! `State::apply_j`: docs/PROGRAMS-V0.md §10.1's matrix, every v0 tag against every kind of signer
//! and every `status` (S3's `Dormant` columns in detail, then T's whole table), a create's and a
//! call's checks one by one on a state with programs on, and the block's fuel budget in `Block`
//! and `Mempool` mode.

use super::*;
use crate::chain::{Journal, KeyRecord, Outcome};
use crate::programs::fixtures::*;
use crate::programs::state::{BlobRef, Pause, Versioned};
use crate::types::tests::program_vector_actions;
use crate::types::{key_id, HexBytes, KeyRole, ProgramCall, ProgramPolicy};
use crate::upgrade::{Feature, ScheduledUpgrade, PROGRAMS_FEATURE, PROGRAMS_VERSION};
use std::collections::BTreeMap;

/// §10.1's `Dormant` columns: below v2 (testnet-2 forever, and a chain before its v2 upgrade) and
/// at v2 before programs first switch on (blockula-1 at launch, and a state holding the feature
/// flag alone), every program tag is refused, whoever signs it, in either mode: by programs' own
/// rule in its words (`admit`), and through `apply_j` (`Err`, the state untouched: nobody is
/// charged).
#[test]
fn every_program_tag_is_refused_while_dormant_whoever_signs() {
    let v1 = || {
        let mut st = genesis(&[]).initial_state();
        st.upgrade.schedule = vec![];
        st
    };
    let upgraded_at_100 = || {
        let mut st = genesis(&[]).initial_state();
        st.upgrade.schedule = vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 100 }];
        st
    };
    let flag_alone = || {
        let mut st = genesis(&[]).initial_state();
        st.upgrade.features.insert(PROGRAMS_FEATURE.into(), Feature { activated_at: 1 });
        st
    };
    let cases: [(&str, State, &[u64]); 4] = [("a v1 chain", v1(), &[1, 2, 1_000_000]), ("upgraded to v2 at 100", upgraded_at_100(), &[99, 100, 101]), ("born at v2", genesis(&[]).initial_state(), &[1, 2, 1_000_000]), ("v2 with the flag alone", flag_alone(), &[1, 2])];
    for (label, st, heights) in cases {
        for &h in heights {
            assert_eq!(status(&st, h), Status::Dormant, "{label}, height {h}");
            for action in program_vector_actions() {
                let mut st = st.clone();
                for (signer, t) in signers(&mut st, &action, h) {
                    let what = format!("{label}, height {h}, {} by {signer:?}", action.kind());
                    for mode in [ApplyMode::Block, ApplyMode::Mempool] {
                        let e = admit(&st, &t, h, mode).unwrap_err().to_string();
                        assert_eq!(e, format!("programs are off: a {} transaction is refused until protocol v2 is in force and governance switches the `programs` feature on", action.kind()), "{what}");
                        let mut after = st.clone();
                        let mut j = Journal::default();
                        assert!(after.apply_j(&mut j, &t, CHAIN, &addr(VALIDATOR), h, mode).is_err(), "{what}");
                        assert_eq!(after.full_root(), st.full_root(), "{what}: nothing charged");
                    }
                }
            }
        }
    }
}

/// The status gate's other cells, a few of them (`the_admission_matrix` checks every one): with
/// programs on every tag gets past it; exit-only refuses uploads, creates and a call carrying FANG
/// and lets a value-free call by; a running pause refuses all three; and the pause itself, signed
/// by a validator of the set, is admitted in each (its own checks: `pause::tests`).
#[test]
fn the_status_gate() {
    let mut st = state_on();
    let blobs = store(&mut st, ESCROW.0, ESCROW.1, 1);
    let escrow = program_of(BOB, 0);
    let digest = install(&mut st, &escrow, blobs, 2);
    let h = 5;
    let txs = [
        tx(&st, ALICE, upload(COUNTER.0, COUNTER.1, FANG), h),
        tx(&st, ALICE, create(&blobs.0, &blobs.1), h),
        tx(&st, ALICE, calls(vec![call(&escrow, &digest, "refund", 0)], 100_000, vec![]), h),
        tx(&st, ALICE, calls(vec![call(&escrow, &digest, "open", FANG)], 100_000, vec![]), h),
    ];
    let pause = tx(&st, VALIDATOR, Action::ProgramsPause {}, h);
    for t in &txs {
        admit(&st, t, h, ApplyMode::Block).unwrap_or_else(|e| panic!("on, {}: {e}", t.action.kind()));
    }
    admit(&st, &pause, h, ApplyMode::Block).unwrap();
    // Exit-only.
    let mut off = st.clone();
    off.upgrade.features.remove(PROGRAMS_FEATURE);
    assert_eq!(status(&off, h), Status::ExitOnly);
    for t in &txs[..2] {
        assert_eq!(admit(&off, t, h, ApplyMode::Mempool).unwrap_err().to_string(), format!("programs are exit-only (governance switched them off): a {} transaction is refused; only calls that send programs no FANG run", t.action.kind()));
    }
    admit(&off, &txs[2], h, ApplyMode::Block).unwrap();
    assert_eq!(admit(&off, &txs[3], h, ApplyMode::Block).unwrap_err().to_string(), "programs are exit-only (governance switched them off): a call can't send a program FANG");
    admit(&off, &pause, h, ApplyMode::Block).unwrap();
    // Paused (on or off).
    for mut paused in [st.clone(), off] {
        paused.programs.meta.as_mut().unwrap().pause = Some(crate::programs::state::Pause { from: h, until: h + 10, started: h, signers: vec![addr(VALIDATOR)] });
        for t in &txs {
            assert_eq!(admit(&paused, t, h, ApplyMode::Block).unwrap_err().to_string(), format!("programs are paused until block {}: a {} transaction is refused until then", h + 10, t.action.kind()));
        }
        admit(&paused, &pause, h, ApplyMode::Block).unwrap();
        admit(&paused, &txs[2], h + 10, ApplyMode::Block).unwrap();
    }
}

/// §10.1's admission matrix, in full: every v0 tag (62, 63, 64 as a value-free call, a call with FANG
/// and a batch in which only a later call carries FANG, and 69) against {an owner key, a session key,
/// a consensus key acting for its owner, the consensus key on the validator's own account (§2.5's
/// other form), a multisig's owner key, a program as `from`} against `status` {dormant below v2,
/// dormant at v2, on, exit-only, paused}: 180 cells, each checked three ways, with the verdict
/// written from the spec (`fixtures::spec_refusal` and the signer rules of §2.1) and its reason
/// pinned:
/// - `programs::admit` alone, in `Block` and in `Mempool` mode (the mode changes only the fuel
///   budget);
/// - the whole of `State::admit`, as `apply_j` runs it in `Mempool` mode (which runs no VM, so an
///   admitted cell is `Ok` whatever the program would do), where a refusal is `Err` and leaves the
///   state untouched, so nobody is charged.
///
/// The columns are real states (`fixtures::column_state`): below v2 is a chain with programs on
/// whose v2 is scheduled above the height, so only the version keeps it dormant; exit-only is the
/// feature removed. The paused column is a pause written into META by hand, so tag 69 in it is
/// the status gate's verdict only: its own limits under a pause a validator signed are
/// `tag_69_while_paused_follows_the_window`. S3's
/// `every_program_tag_is_refused_while_dormant_whoever_signs` goes deeper into the dormant columns
/// (an upgrade height, the feature flag alone, every vector action).
#[test]
fn the_admission_matrix() {
    let w = world();
    let actions = matrix_actions(&w);
    let mut cells = 0;
    for column in COLUMNS {
        let base = column_state(&w, column);
        if column == Column::DormantBelowV2 {
            // Only the version keeps it dormant: META and the feature are there.
            assert!(base.programs.meta.is_some() && base.upgrade.features.contains_key(PROGRAMS_FEATURE));
            assert_eq!(crate::upgrade::version_at(&base, H), 1);
        }
        for action in &actions {
            let mut st = base.clone();
            for (signer, t) in signers(&mut st, action, H) {
                let what = format!("{column:?}, a {} by {signer:?}", describe(action));
                for mode in [ApplyMode::Block, ApplyMode::Mempool] {
                    expect_verdict(&format!("{what}, programs::admit in {mode:?} mode"), admit(&st, &t, H, mode).map_err(|e| e.to_string()), programs_refusal(signer, action, column));
                }
                let (mut after, mut j) = (st.clone(), Journal::default());
                let applied = after.apply_j(&mut j, &t, CHAIN, &addr(VALIDATOR), H, ApplyMode::Mempool);
                let refusal = state_refusal(signer, action, column);
                expect_verdict(&format!("{what}, State::admit"), applied.as_ref().map(|_| ()).map_err(|e| e.to_string()), refusal);
                if refusal.is_some() {
                    assert_eq!(after.full_root(), st.full_root(), "{what}: a refusal charges nobody");
                }
                cells += 1;
            }
        }
    }
    assert_eq!(cells, COLUMNS.len() * actions.len() * 6);
    assert_eq!(cells, 5 * 6 * 6);
}

/// While a pause runs in exit-only, `Paused` wins (§10.1): a call that carries no FANG, which exit-only
/// lets through, is refused for the pause (and an upload for it too, not for exit-only).
#[test]
fn a_pause_running_in_exit_only_refuses_a_value_free_call() {
    let w = world();
    let mut st = column_state(&w, Column::ExitOnly);
    let free = tx(&st, ALICE, calls(vec![call(&w.escrow.0, &w.escrow.1, "refund", 0)], 100_000, vec![]), H);
    admit(&st, &free, H, ApplyMode::Block).unwrap();
    st.programs.meta.as_mut().unwrap().pause = Some(Pause { from: H, until: H + 10, started: H, signers: vec![addr(VALIDATOR)] });
    assert_eq!(status(&st, H), Status::Paused { until: H + 10 });
    for t in [free, tx(&st, ALICE, upload(COUNTER.0, COUNTER.1, FANG), H)] {
        for mode in [ApplyMode::Block, ApplyMode::Mempool] {
            let e = admit(&st, &t, H, mode).unwrap_err().to_string();
            assert!(e.contains(&format!("programs are paused until block {}", H + 10)), "{}: {e}", t.action.kind());
        }
    }
}

/// Tag 69 while `Paused` depends on more than the status (§7.2, §10.1): after a pause the sole validator
/// signed through `pause::sign` (so `pause_signed` holds its signature, as a real pause leaves it),
/// its next signature, by its owner's key or its consensus key from its own account, is refused by
/// the window, in `programs::admit` and `State::admit` alike and charged to nobody, unless a fix is
/// pending (an upgrade scheduled above the height), when it's admitted (it renews the pause). The
/// matrix's Paused column is the status gate's verdict only.
#[test]
fn tag_69_while_paused_follows_the_window() {
    let w = world();
    let mut st = w.st.clone();
    let pause = Action::ProgramsPause {};
    // Keys, the validator's owner (Alice), the multisig: `signers` sets them up on `st`.
    signers(&mut st, &pause, H);
    // The sole validator signs at H - 1 with its consensus key from its own account: f + 1 = 1, so a
    // run starts at H.
    pause::sign(&mut st, &mut Journal::default(), &addr(VALIDATOR), &key_id(&keys(VALIDATOR).public_key()), H - 1).unwrap();
    let m = st.programs.meta.as_ref().unwrap();
    assert_eq!(status(&st, H), Status::Paused { until: m.pause.as_ref().unwrap().until });
    assert_eq!(m.pause_signed.get(&addr(VALIDATOR)), Some(&(H - 1)), "a real pause leaves its signer's height");
    let window = format!("started or renewed a pause at block {}: it may sign again from block {}", H - 1, H - 1 + pause::Spans::of(&st.params).sign_window);
    let by_validator = |st: &mut State| signers(st, &pause, H).into_iter().filter(|(s, _)| matches!(s, Signer::Owner | Signer::ConsensusOnOwnAccount)).collect::<Vec<_>>();
    for (signer, t) in by_validator(&mut st) {
        for mode in [ApplyMode::Block, ApplyMode::Mempool] {
            let e = admit(&st, &t, H, mode).unwrap_err().to_string();
            assert!(e.contains(&window), "{signer:?}, {mode:?}: {e}");
        }
        let (mut after, mut j) = (st.clone(), Journal::default());
        let e = after.apply_j(&mut j, &t, CHAIN, &addr(VALIDATOR), H, ApplyMode::Mempool).unwrap_err().to_string();
        assert!(e.contains(&window), "{signer:?}, State::admit: {e}");
        assert_eq!(after.full_root(), st.full_root(), "{signer:?}: refused uncharged");
    }
    // A pending fix waives the window.
    assert!(!pause::fix_pending(&st, H));
    st.upgrade.schedule.push(ScheduledUpgrade { version: PROGRAMS_VERSION + 1, activation_height: H + 1_000 });
    assert!(pause::fix_pending(&st, H));
    for (signer, t) in by_validator(&mut st) {
        for mode in [ApplyMode::Block, ApplyMode::Mempool] {
            admit(&st, &t, H, mode).unwrap_or_else(|e| panic!("{signer:?}, {mode:?}: {e}"));
        }
        let (mut after, mut j) = (st.clone(), Journal::default());
        assert_eq!(after.apply_j(&mut j, &t, CHAIN, &addr(VALIDATOR), H, ApplyMode::Mempool).unwrap(), Outcome::Applied, "{signer:?}");
    }
}

fn binding(program: &(String, String)) -> Binding {
    Binding { program: program.0.clone(), version_digest: program.1.clone() }
}

/// The token's create, initialized (its `init` isn't payable), binding the counter.
fn token_create(w: &World) -> Action {
    let (code, interface) = w.blobs[1];
    Action::CreateProgram {
        wasm_hash: hex::encode(code),
        interface_hash: hex::encode(interface),
        authority: None,
        upgrade_delay_blocks: 0,
        policy: ProgramPolicy::Open,
        accepts_transfers: false,
        init: Some(InitCall { method: "init".into(), input: HexBytes(vec![]), value: 0 }),
        fuel_limit: 1_000_000,
        storage_limit: FANG,
        bindings: vec![binding(&w.counter)],
    }
}

/// `action` with its `CreateProgram` fields changed by `f`.
fn changed(action: &Action, f: impl FnOnce(&mut Action)) -> Action {
    let mut a = action.clone();
    f(&mut a);
    a
}

fn refused(st: &State, t: &Tx, mode: ApplyMode) -> String {
    admit(st, t, H, mode).map(|()| panic!("admitted: {:?}", t.action)).unwrap_err().to_string()
}

/// A create's checks against the state (§2.3), one by one, each with its reason: both blobs stored
/// and usable (code and an interface uploaded in this block are usable from the next), matching;
/// `init` exactly when the interface declares an initializer, naming it, with value only if it's
/// payable; an authority's minimum notice; bindings to usable programs at their version in force
/// (or any version); `fuel_limit` within `program_max_tx_fuel`; the code it loads within
/// `MAX_TX_CODE_BYTES`; a reservation no larger than a block; and an address that's free or
/// adoptable.
#[test]
fn a_create_is_checked_one_by_one() {
    let w = world();
    let mk = |st: &State, a: Action| tx(st, ALICE, a, H);
    let base = token_create(&w);
    admit(&w.st, &mk(&w.st, base.clone()), H, ApplyMode::Block).unwrap();
    let set = |f: fn(&mut Action)| changed(&base, f);
    let (token_code, token_interface) = (short_hash(&w.blobs[1].0), short_hash(&w.blobs[1].1));
    // Blobs: stored, and usable from the block after their upload.
    let unknown = changed(&base, |a| if let Action::CreateProgram { wasm_hash, .. } = a { *wasm_hash = "ab".repeat(32) });
    assert_eq!(refused(&w.st, &mk(&w.st, unknown), ApplyMode::Block), format!("code {} isn't stored: upload it first", short_hash(&[0xab; 32])));
    let unknown = changed(&base, |a| if let Action::CreateProgram { interface_hash, .. } = a { *interface_hash = "ab".repeat(32) });
    assert_eq!(refused(&w.st, &mk(&w.st, unknown), ApplyMode::Block), format!("interface {} isn't stored: upload it first", short_hash(&[0xab; 32])));
    for (kind, what, hash) in [(BLOB_CODE, "code", &token_code), (BLOB_INTERFACE, "interface", &token_interface)] {
        let mut st = w.st.clone();
        let key = blob_key(kind, if kind == BLOB_CODE { &w.blobs[1].0 } else { &w.blobs[1].1 });
        let fresh: Versioned<BlobRef> = Versioned(BlobRef { uploaded_at: H, ..st.programs.blob_refs.get(&key).unwrap().0.clone() });
        st.programs.blob_refs.insert(key, fresh);
        for mode in [ApplyMode::Block, ApplyMode::Mempool] {
            assert_eq!(refused(&st, &mk(&st, base.clone()), mode), format!("{what} {hash} was uploaded at block {H}: it's usable from block {}", H + 1));
        }
        admit(&st, &mk(&st, base.clone()), H + 1, ApplyMode::Block).unwrap();
    }
    // The pair matches: the token's code with the counter's interface doesn't.
    let mismatched = changed(&base, |a| if let Action::CreateProgram { interface_hash, init, fuel_limit, bindings, .. } = a { *interface_hash = hex::encode(digest::interface_hash(COUNTER.1)); (*init, *fuel_limit, *bindings) = (None, 0, vec![]) });
    let e = refused(&w.st, &mk(&w.st, mismatched), ApplyMode::Block);
    assert!(e.starts_with(&format!("interface {} doesn't match code {token_code}: ", short_hash(&digest::interface_hash(COUNTER.1)))), "{e}");
    // `init` exactly when the interface declares an initializer.
    let without = set(|a| if let Action::CreateProgram { init, fuel_limit, bindings, .. } = a { (*init, *fuel_limit, *bindings) = (None, 0, vec![]) });
    assert_eq!(refused(&w.st, &mk(&w.st, without), ApplyMode::Block), "the interface declares an initializer, so the create must run it (`init`)");
    let (counter_code, counter_interface) = w.blobs[0];
    let counter_init = changed(&base, |a| if let Action::CreateProgram { wasm_hash, interface_hash, .. } = a { (*wasm_hash, *interface_hash) = (hex::encode(counter_code), hex::encode(counter_interface)) });
    assert_eq!(refused(&w.st, &mk(&w.st, counter_init), ApplyMode::Block), "the interface declares no initializer, so the create can't carry `init`");
    let counter_bare = mk(&w.st, create(&counter_code, &counter_interface));
    admit(&w.st, &counter_bare, H, ApplyMode::Block).unwrap();
    let wrong = set(|a| if let Action::CreateProgram { init: Some(i), .. } = a { i.method = "setup".into() });
    assert_eq!(refused(&w.st, &mk(&w.st, wrong), ApplyMode::Block), "`init.method` isn't the interface's initializer");
    let paying = set(|a| if let Action::CreateProgram { init: Some(i), .. } = a { i.value = 1 });
    assert_eq!(refused(&w.st, &mk(&w.st, paying), ApplyMode::Block), "the interface's initializer isn't payable, so `init.value` must be 0");
    // An authority gives at least the minimum notice (2 days at 1 s blocks).
    let least = param(&w.st, Param::MinUpgradeDelayBlocks);
    assert_eq!(least, 172_800);
    let upgradable = |delay: u64| changed(&base, |a| if let Action::CreateProgram { authority, upgrade_delay_blocks, .. } = a { (*authority, *upgrade_delay_blocks) = (Some(addr(ALICE)), delay) });
    assert_eq!(refused(&w.st, &mk(&w.st, upgradable(least - 1)), ApplyMode::Block), format!("an upgradable program gives at least {least} blocks' notice of an upgrade (program_min_upgrade_delay_blocks), not {}", least - 1));
    admit(&w.st, &mk(&w.st, upgradable(least)), H, ApplyMode::Block).unwrap();
    // Bindings: to a program, usable, at its version in force or any version.
    let bound = |b: Binding| changed(&base, |a| if let Action::CreateProgram { bindings, .. } = a { *bindings = vec![b] });
    let nobody = program_of(BOB, 99);
    assert_eq!(refused(&w.st, &mk(&w.st, bound(Binding { program: nobody.clone(), version_digest: hex::encode(ANY_VERSION) })), ApplyMode::Block), format!("{} isn't a program", short(&nobody)));
    let digest_c = w.counter.1.clone();
    assert_eq!(refused(&w.st, &mk(&w.st, bound(Binding { program: w.counter.0.clone(), version_digest: w.token.1.clone() })), ApplyMode::Block), format!("a binding names version {} of program {}, whose version in force is {}", short_hex(&w.token.1), short(&w.counter.0), short_hex(&digest_c)));
    admit(&w.st, &mk(&w.st, bound(Binding { program: w.counter.0.clone(), version_digest: hex::encode(ANY_VERSION) })), H, ApplyMode::Block).unwrap();
    let mut young = w.st.clone();
    young.programs.records.get_mut(&w.counter.0).unwrap().created_at = H;
    assert_eq!(refused(&young, &mk(&young, base.clone()), ApplyMode::Mempool), format!("program {} was created at block {H}: it's usable from block {}", short(&w.counter.0), H + 1));
    // Fuel: the transaction's limit, the code it loads, the block's budget.
    let max_tx = param(&w.st, Param::MaxTxFuel);
    let heavy = changed(&base, |a| if let Action::CreateProgram { fuel_limit, .. } = a { *fuel_limit = max_tx + 1 });
    assert_eq!(refused(&w.st, &mk(&w.st, heavy), ApplyMode::Mempool), format!("`fuel_limit` {} is above program_max_tx_fuel ({max_tx})", max_tx + 1));
    let loads = changed(&base, |a| if let Action::CreateProgram { bindings, .. } = a { *bindings = w.big.iter().map(binding).collect() });
    let code = 2 * blockula_vm::config::MAX_CODE_BYTES + TOKEN.0.len();
    assert_eq!(refused(&w.st, &mk(&w.st, loads), ApplyMode::Mempool), format!("the code this transaction declares is {code} bytes, past the {MAX_TX_CODE_BYTES} one transaction may load"));
    let mut small = w.st.clone();
    small.programs.meta.as_mut().unwrap().params.insert(Param::MaxBlockFuel.key().into(), 20_000_000);
    let past = changed(&base, |a| if let Action::CreateProgram { bindings, fuel_limit, .. } = a { (*bindings, *fuel_limit) = (vec![binding(&w.big[0])], max_tx) });
    let past = mk(&small, past);
    let reserved = fuel::reserved(&small, &past, H);
    assert!(reserved > 20_000_000 && reserved < MAX_RESERVATION_FOR_TESTS);
    for mode in [ApplyMode::Block, ApplyMode::Mempool] {
        assert_eq!(refused(&small, &past, mode), format!("larger than a block: this transaction reserves {reserved} fuel, and a block counts at most 20000000 (program_max_block_fuel)"));
    }
    // The address it creates: free, or adoptable (a balance sent ahead), never a program.
    let t = mk(&w.st, base.clone());
    let address = program_of(ALICE, t.nonce);
    let mut sent_ahead = w.st.clone();
    sent_ahead.accounts.insert(address.clone(), crate::chain::Account { balance: 5 * FANG, ..Default::default() });
    admit(&sent_ahead, &t, H, ApplyMode::Block).unwrap();
    for (label, account) in [("a nonce", crate::chain::Account { nonce: 1, ..Default::default() }), ("keys", crate::chain::Account { keys: BTreeMap::from([("ab".repeat(32), KeyRecord { label: "k".into(), role: KeyRole::Owner, added_at: 0, spent: 0, window_start: 0 })]), ..Default::default() })] {
        let mut st = w.st.clone();
        st.accounts.insert(address.clone(), account);
        assert_eq!(refused(&st, &t, ApplyMode::Block), format!("the address this create makes, {}, holds an account a program can't adopt (it has keys, locks or a nonce)", short(&address)), "{label}");
    }
    let mut taken = w.st.clone();
    install(&mut taken, &address, w.blobs[0], 2);
    assert_eq!(refused(&taken, &t, ApplyMode::Block), format!("the address this create makes, {}, is a program already", short(&address)));
}

/// `MAX_RESERVATION`, as the reservations admission lets through stay under it.
const MAX_RESERVATION_FOR_TESTS: u64 = blockula_vm::meter::MAX_RESERVATION;

/// A call's checks against the state (§2.4), one by one: each call's program exists and is usable,
/// at its version in force; its method is in the interface, of kind `Call` (never a view or the
/// initializer), payable if the call sends FANG; the bindings as a create's; `fuel_limit`; the code
/// it loads; and a reservation no larger than a block.
#[test]
fn a_call_is_checked_one_by_one() {
    let w = world();
    let mk = |st: &State, calls_: Vec<ProgramCall>, fuel: u64, bindings: Vec<Binding>| tx(st, ALICE, calls(calls_, fuel, bindings), H);
    let (counter, dc) = (&w.counter.0, &w.counter.1);
    let ok = mk(&w.st, vec![call(counter, dc, "increment", 0)], 100_000, vec![binding(&w.token)]);
    admit(&w.st, &ok, H, ApplyMode::Block).unwrap();
    let one = |c: ProgramCall| mk(&w.st, vec![call(counter, dc, "increment", 0), c], 100_000, vec![]);
    // The program: one, usable, at the version in force.
    let nobody = program_of(BOB, 99);
    assert_eq!(refused(&w.st, &one(call(&nobody, dc, "increment", 0)), ApplyMode::Block), format!("{} isn't a program", short(&nobody)));
    assert_eq!(refused(&w.st, &one(call(counter, &w.token.1, "increment", 0)), ApplyMode::Block), format!("call 2 names version {} of program {}, whose version in force is {}", short_hex(&w.token.1), short(counter), short_hex(dc)));
    let mut young = w.st.clone();
    young.programs.records.get_mut(counter).unwrap().created_at = H;
    for mode in [ApplyMode::Block, ApplyMode::Mempool] {
        assert_eq!(refused(&young, &ok, mode), format!("program {} was created at block {H}: it's usable from block {}", short(counter), H + 1));
    }
    admit(&young, &ok, H + 1, ApplyMode::Block).unwrap();
    // The method: in the interface, of kind `Call`, payable if it's sent FANG.
    assert_eq!(refused(&w.st, &one(call(counter, dc, "decrement", 0)), ApplyMode::Block), format!("call 2: program {}'s interface has no method by that name", short(counter)));
    assert_eq!(refused(&w.st, &one(call(counter, dc, "get", 0)), ApplyMode::Block), format!("call 2: that method of program {} is a view, which no transaction calls", short(counter)));
    assert_eq!(refused(&w.st, &one(call(&w.token.0, &w.token.1, "init", 0)), ApplyMode::Block), format!("call 2: that method of program {} is its initializer, which runs only when the program is created", short(&w.token.0)));
    assert_eq!(refused(&w.st, &one(call(counter, dc, "increment", 1)), ApplyMode::Block), format!("call 2: that method of program {} isn't payable, so the call can't send it FANG", short(counter)));
    admit(&w.st, &one(call(&w.escrow.0, &w.escrow.1, "open", 5 * FANG)), H, ApplyMode::Block).unwrap();
    // Bindings, as a create's.
    let bad = mk(&w.st, vec![call(counter, dc, "increment", 0)], 100_000, vec![Binding { program: w.token.0.clone(), version_digest: w.escrow.1.clone() }]);
    assert_eq!(refused(&w.st, &bad, ApplyMode::Block), format!("a binding names version {} of program {}, whose version in force is {}", short_hex(&w.escrow.1), short(&w.token.0), short_hex(&w.token.1)));
    // Fuel.
    let max_tx = param(&w.st, Param::MaxTxFuel);
    assert_eq!(refused(&w.st, &mk(&w.st, vec![call(counter, dc, "increment", 0)], max_tx + 1, vec![]), ApplyMode::Block), format!("`fuel_limit` {} is above program_max_tx_fuel ({max_tx})", max_tx + 1));
    admit(&w.st, &mk(&w.st, vec![call(counter, dc, "increment", 0)], max_tx, vec![]), H, ApplyMode::Block).unwrap();
    let loads = mk(&w.st, vec![call(counter, dc, "increment", 0)], 100_000, w.big.iter().map(binding).collect());
    let code = 2 * blockula_vm::config::MAX_CODE_BYTES + COUNTER.0.len();
    assert_eq!(refused(&w.st, &loads, ApplyMode::Mempool), format!("the code this transaction declares is {code} bytes, past the {MAX_TX_CODE_BYTES} one transaction may load"));
    let mut small = w.st.clone();
    small.programs.meta.as_mut().unwrap().params.insert(Param::MaxBlockFuel.key().into(), 20_000_000);
    let past = mk(&small, vec![call(&w.big[0].0, &w.big[0].1, "increment", 0)], max_tx, vec![]);
    let reserved = fuel::reserved(&small, &past, H);
    assert_eq!(reserved, max_tx + blockula_vm::meter::SCHEDULE_V0.load_fuel(blockula_vm::config::MAX_CODE_BYTES));
    for mode in [ApplyMode::Block, ApplyMode::Mempool] {
        assert_eq!(refused(&small, &past, mode), format!("larger than a block: this transaction reserves {reserved} fuel, and a block counts at most 20000000 (program_max_block_fuel)"));
    }
    admit(&w.st, &past, H, ApplyMode::Block).unwrap();
}

/// The block's fuel budget (§5.4), on one state at one height, through `apply_j`: two of the
/// largest uploads fit a default block (40M); the third is `Err` in `Block` mode ("the block's
/// program fuel is spent") and admitted in `Mempool` mode (a queue isn't a block); a rolled-back
/// trial un-counts, so the next one fits again; at the next height the count starts over.
#[test]
fn the_block_budget() {
    let mut st = state_on();
    let h = 3;
    let largest = || upload(&largest_wasm(), COUNTER.1, 0);
    let exec = blockula_vm::meter::SCHEDULE_V0.upload_fuel(blockula_vm::config::MAX_CODE_BYTES, COUNTER.1.len());
    let max_block = param(&st, Param::MaxBlockFuel);
    assert!(2 * exec <= max_block && 3 * exec > max_block, "{exec} fuel an upload, {max_block} a block");
    let apply = |st: &mut State, mode: ApplyMode| {
        let t = tx(st, ALICE, largest(), h);
        let mut j = Journal::default();
        let r = st.apply_j(&mut j, &t, CHAIN, &addr(VALIDATOR), h, mode);
        (r, j)
    };
    // Two fit (each is charged and counted; the action itself fails, as validation refuses the
    // code: the Wasm header, then zeros).
    for n in 1..=2 {
        let (r, _) = apply(&mut st, ApplyMode::Block);
        assert!(matches!(&r, Ok(Outcome::Failed(why)) if why.starts_with("invalid code: upload validation rule ")), "{r:?}");
        assert_eq!(st.programs.block.fuel_at(h), n * exec);
    }
    // The third: spent in a block, admitted in the mempool, which doesn't count.
    let third = tx(&st, ALICE, largest(), h);
    let spent = format!("the block's program fuel is spent: this transaction reserves {exec} fuel, and block {h} has counted {} of its {max_block}", 2 * exec);
    assert_eq!(admit(&st, &third, h, ApplyMode::Block).unwrap_err().to_string(), spent);
    let (r, _) = apply(&mut st, ApplyMode::Block);
    assert_eq!(r.unwrap_err().to_string(), spent);
    let (r, j) = apply(&mut st, ApplyMode::Mempool);
    assert_eq!(r.unwrap(), Outcome::Applied);
    st.rollback(j);
    assert_eq!(st.programs.block.fuel_at(h), 2 * exec);
    // A rolled-back trial un-counts: roll the second back and a third fits in its place.
    let mut st2 = state_on();
    let (r, _) = apply(&mut st2, ApplyMode::Block);
    assert!(r.is_ok());
    let (r, trial) = apply(&mut st2, ApplyMode::Block);
    assert!(r.is_ok());
    st2.rollback(trial);
    assert_eq!(st2.programs.block.fuel_at(h), exec);
    let (r, _) = apply(&mut st2, ApplyMode::Block);
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(st2.programs.block.fuel_at(h), 2 * exec);
    // At the next height the count starts over.
    let next = tx(&st, ALICE, largest(), h + 1);
    admit(&st, &next, h + 1, ApplyMode::Block).unwrap();
    let mut j = Journal::default();
    assert!(st.apply_j(&mut j, &next, CHAIN, &addr(VALIDATOR), h + 1, ApplyMode::Block).is_ok());
    assert_eq!((st.programs.block.height, st.programs.block.fuel), (h + 1, exec));
}

/// Interfaces are held decoded in their records (§4.2), so admission never parses one: a record
/// made from an interface's bytes holds it decoded, keeps exactly the bytes' Borsh form (a u32
/// length and the bytes), and decodes again when it's loaded; bytes that don't decode (which no
/// upload stores) hold nothing, and anything naming them is refused.
#[test]
fn interfaces_are_held_decoded() {
    use crate::programs::state::InterfaceRecord;
    use crate::state::codec::{decode, encode};
    let record = InterfaceRecord::new(TOKEN.1.to_vec());
    assert_eq!(record.interface(), Some(&blockula_vm::interface::decode(TOKEN.1).unwrap()));
    let bytes = encode(&Versioned(&record));
    let mut expected = vec![crate::programs::state::FORMAT];
    expected.extend_from_slice(&(TOKEN.1.len() as u32).to_le_bytes());
    expected.extend_from_slice(TOKEN.1);
    assert_eq!(bytes, expected, "the format byte, then the bytes alone");
    let loaded = decode::<Versioned<InterfaceRecord>>(&bytes).unwrap();
    assert_eq!(loaded.0, record);
    assert!(loaded.interface().is_some());
    // Bytes that don't decode.
    let broken = InterfaceRecord::new(vec![0xff; 4]);
    assert!(broken.interface().is_none());
    assert_eq!(decode::<Versioned<InterfaceRecord>>(&encode(&Versioned(&broken))).unwrap().0, broken);
    let mut w = world();
    w.st.programs.interfaces.insert(w.blobs[2].1, Versioned(broken));
    let t = tx(&w.st, ALICE, calls(vec![call(&w.escrow.0, &w.escrow.1, "open", 0)], 100_000, vec![]), H);
    assert_eq!(refused(&w.st, &t, ApplyMode::Block), format!("stored interface {} doesn't decode", short_hash(&w.blobs[2].1)));
}
