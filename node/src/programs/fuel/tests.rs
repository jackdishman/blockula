//! Programs' fees and the fuel rate (WBS S3's acceptance): `fuel_fee` for each tag, the tip cap on
//! bytes and fuel together, v1's charges unchanged, the stateless lower bound, the rate's floor,
//! its end-of-block step, and proposal order's share; and WBS T's property tests (docs/PROGRAMS-V0.md
//! §10.1): `next_rate` is `econ::next_slot`'s formula over its whole domain, 10,000 consecutive full
//! blocks overflow neither the rate nor a charge, `suggested_rate` covers any 17 blocks, and
//! `lowest_fuel_fee` is never above what a state charges, over the parameters' bounds.

use super::*;
use crate::chain::{Outcome, TipRate};
use crate::econ::{self, FeeSlot};
use crate::programs::digest;
use crate::programs::fixtures::*;
use crate::programs::params::{MAX_FUEL_FLOOR_RATE, MIN_BLOCK_FUEL};
use crate::programs::state::{BlockRun, Pause};
use crate::types::tests::{ghost_vector_actions, program_vector_actions, small_set_vector_actions, v1_vector_actions, vector_actions, vector_tx};
use crate::types::{Binding, HexBytes, InitCall};
use crate::upgrade::{ScheduledUpgrade, PROGRAMS_VERSION};
use proptest::prelude::*;
use std::cmp::Ordering;

/// Every vector tx that isn't a program's: one per tag 0–61 (some twice), Ghost Mode's included.
fn non_program_txs() -> Vec<Tx> {
    vector_actions().into_iter().chain(v1_vector_actions()).chain(ghost_vector_actions()).chain(small_set_vector_actions()).map(vector_tx).collect()
}

/// A state with programs on, at a fuel rate of `rate`, holding the counter and the token (created
/// at block 2) and a third program whose code is `big` bytes: what the fee tests price against.
struct Priced {
    st: State,
    counter: String,
    token: String,
    big: String,
    /// The code sizes: counter's, token's, the big program's.
    sizes: [usize; 3],
}

fn priced(rate: u64) -> Priced {
    let mut st = state_on();
    st.programs.meta.as_mut().unwrap().fuel_rate = rate;
    let (counter, token, big) = (program_of(ALICE, 0), program_of(ALICE, 1), program_of(ALICE, 2));
    let blobs = store(&mut st, COUNTER.0, COUNTER.1, 1);
    install(&mut st, &counter, blobs, 2);
    let blobs = store(&mut st, TOKEN.0, TOKEN.1, 1);
    install(&mut st, &token, blobs, 2);
    let large = largest_wasm();
    let blobs = store(&mut st, &large, COUNTER.1, 1);
    install(&mut st, &big, blobs, 2);
    Priced { st, counter, token, big, sizes: [COUNTER.0.len(), TOKEN.0.len(), large.len()] }
}

fn load_of(sizes: &[usize]) -> u64 {
    sizes.iter().map(|s| SCHEDULE_V0.load_fuel(*s)).sum()
}

/// `fuel_fee` per tag (§5.1): an upload pays for its validation by its code's and interface's size,
/// with no load; a create without `init` reserves nothing (it runs no code); a create with `init`
/// pays its `fuel_limit` and the load of its own code and its bindings'; a call pays its
/// `fuel_limit` and the load of each distinct program it declares (one program called twice and
/// bound once is loaded once). All at `rate × reserved / 1000`, floored. The pause and every v1
/// tag reserve nothing.
#[test]
fn the_fuel_fee_of_each_tag() {
    let p = priced(777);
    let h = 3;
    let fee = |tx: &Tx| fuel_fee(&p.st, tx, h);
    let at = |reserved: u64| 777 * reserved / 1_000;
    let [counter_size, token_size, big_size] = p.sizes;
    // An upload: `exec` by its sizes, no load.
    let up = tx(&p.st, ALICE, upload(ESCROW.0, ESCROW.1, FANG), h);
    let exec_up = SCHEDULE_V0.upload_fuel(ESCROW.0.len(), ESCROW.1.len());
    assert_eq!((exec(&up, &SCHEDULE_V0), reserved(&p.st, &up, h), fee(&up)), (exec_up, exec_up, at(exec_up)));
    // A create without `init`: nothing runs, nothing reserved.
    let bare = tx(&p.st, ALICE, create(&digest::wasm_hash(COUNTER.0), &digest::interface_hash(COUNTER.1)), h);
    assert_eq!((reserved(&p.st, &bare, h), fee(&bare)), (0, 0));
    // A create with `init`: its fuel limit, its own code and its bindings' (the counter's, at any version).
    let any = hex::encode(blockula_vm::exec::ANY_VERSION);
    let init = Action::CreateProgram {
        wasm_hash: hex::encode(digest::wasm_hash(TOKEN.0)),
        interface_hash: hex::encode(digest::interface_hash(TOKEN.1)),
        authority: None,
        upgrade_delay_blocks: 0,
        policy: crate::types::ProgramPolicy::Open,
        accepts_transfers: false,
        init: Some(InitCall { method: "init".into(), input: HexBytes(vec![]), value: 0 }),
        fuel_limit: 300_000,
        storage_limit: FANG,
        bindings: vec![Binding { program: p.counter.clone(), version_digest: any.clone() }],
    };
    let init = tx(&p.st, ALICE, init, h);
    let reserved_init = 300_000 + load_of(&[token_size, counter_size]);
    assert_eq!((reserved(&p.st, &init, h), fee(&init)), (reserved_init, at(reserved_init)));
    // A call: the counter twice and the token once, the big program bound, and the counter bound
    // too (each distinct code loaded once).
    let call_digest = "d2".repeat(32);
    let batch = calls(vec![call(&p.counter, &call_digest, "increment", 0), call(&p.token, &call_digest, "transfer", 0), call(&p.counter, &call_digest, "increment", 0)], 400_000, vec![Binding { program: p.big.clone(), version_digest: any }]);
    let batch = tx(&p.st, ALICE, batch, h);
    let reserved_call = 400_000 + load_of(&[counter_size, token_size, big_size]);
    assert_eq!(declared_code(&p.st, &batch).len(), 3);
    assert_eq!((reserved(&p.st, &batch, h), fee(&batch)), (reserved_call, at(reserved_call)));
    // A program it can't find adds no load (admission refuses the tx for it).
    let missing = tx(&p.st, ALICE, calls(vec![call(&program_of(BOB, 9), &call_digest, "increment", 0)], 50_000, vec![]), h);
    assert_eq!(reserved(&p.st, &missing, h), 50_000);
    // The pause, and every tx that isn't a program's, reserve nothing.
    for t in [vector_tx(Action::ProgramsPause {})].into_iter().chain(non_program_txs()) {
        assert_eq!((exec(&t, &SCHEDULE_V0), reserved(&p.st, &t, h), fee(&t)), (0, 0, 0), "{}", t.action.kind());
    }
    // While programs are dormant nothing reserves fuel, so every fee is v1's: below v2, and at v2
    // before programs first switch on (where the rate would be the floor's default).
    let mut v1 = p.st.clone();
    v1.upgrade.schedule = vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 100 }];
    let dormant = genesis(&[]).initial_state();
    assert_eq!(rate(&dormant), 250);
    for st in [&v1, &dormant] {
        for t in [&up, &init, &batch] {
            assert_eq!((reserved(st, t, h), fuel_fee(st, t, h)), (0, 0), "{}", t.action.kind());
        }
    }
    // In exit-only and while paused they still reserve (admission refuses what it refuses).
    let mut off = p.st.clone();
    off.upgrade.features.remove(crate::upgrade::PROGRAMS_FEATURE);
    assert_eq!(fuel_fee(&off, &batch, h), at(reserved_call));
}

/// The fee check (`econ::check_fee`, admission's) prices a program tx at its bytes plus its fuel,
/// and caps its tip at `tip_cap_multiple` times that whole base fee (§5.1): a tip past the byte
/// fee's cap but within the whole fee's is admitted, one past the whole fee's cap is refused, and
/// the charge (`econ::charge`, the API's) is the same sum.
#[test]
fn the_tip_cap_covers_bytes_and_fuel() {
    let p = priced(1_000_000);
    let h = 3;
    let mut t = tx(&p.st, ALICE, calls(vec![call(&p.counter, &"d2".repeat(32), "increment", 0)], 10_000_000, vec![]), h);
    let bytes = crate::econ::base_fee(&p.st, h, t.size());
    let fuel = fuel_fee(&p.st, &t, h);
    assert!(fuel > bytes, "this call's fuel costs more than its bytes ({fuel} against {bytes})");
    let cap = crate::econ::max_tip(&p.st, bytes + fuel);
    t.fee = u64::MAX;
    t.tip = crate::econ::max_tip(&p.st, bytes) + 1;
    assert_eq!(crate::econ::check_fee(&p.st, &t, h).unwrap(), bytes + fuel + t.tip, "past the byte fee's cap, within the whole fee's");
    t.tip = cap;
    assert_eq!(crate::econ::check_fee(&p.st, &t, h).unwrap(), bytes + fuel + cap);
    assert_eq!(crate::econ::charge(&p.st, &t, h), Some(bytes + fuel + cap));
    t.tip = cap + 1;
    assert!(crate::econ::check_fee(&p.st, &t, h).unwrap_err().to_string().starts_with("tip too high"));
    // A fee that covers the bytes but not the fuel is too low.
    t.tip = 0;
    t.fee = bytes + fuel - 1;
    assert!(crate::econ::check_fee(&p.st, &t, h).unwrap_err().to_string().starts_with("fee too low"));
    t.fee = bytes + fuel;
    assert_eq!(crate::econ::check_fee(&p.st, &t, h).unwrap(), bytes + fuel);
}

/// For every v1 vector tx (and Ghost Mode's), on every kind of state (v1, v2 with programs
/// dormant, v2 with programs on at a high fuel rate), the fee check and the charge are exactly
/// what they were before programs: the byte base fee plus the tip, the same tip cap, the same
/// refusals; and the stateless lower bound adds nothing.
#[test]
fn v1_charges_are_unchanged() {
    let mut v1 = genesis(&[]).initial_state();
    v1.upgrade.schedule = vec![];
    let dormant = genesis(&[]).initial_state();
    let on = priced(MAX_FUEL_RATE).st;
    for (label, st) in [("v1", &v1), ("dormant", &dormant), ("on", &on)] {
        for h in [1, 2, 18, 1_000] {
            for t in non_program_txs() {
                let base = crate::econ::base_fee(st, h, t.size());
                // The charge before programs, and the check's answer.
                let before = base.checked_add(t.tip).filter(|c| *c <= t.fee && t.tip <= crate::econ::max_tip(st, base));
                assert_eq!(crate::econ::check_fee(st, &t, h).ok(), before, "{label}, height {h}, {}", t.action.kind());
                assert_eq!(crate::econ::charge(st, &t, h), base.checked_add(t.tip), "{label}, height {h}, {}", t.action.kind());
                // With a fee that covers it, the same charge.
                let paid = Tx { fee: base + t.tip, ..t.clone() };
                assert_eq!(crate::econ::check_fee(st, &paid, h).unwrap(), base + t.tip, "{label}, height {h}, {}", t.action.kind());
                assert_eq!((fuel_fee(st, &t, h), lowest_fuel_fee(&t)), (0, 0), "{}", t.action.kind());
            }
        }
    }
}

/// The stateless lower bound (§5.3): 0 for every tag but 62–64; for those, what their own bytes
/// declare at the floor parameter's lower bound (1 milli-drop per fuel), never more than any state
/// charges (here: at the default floor, and with no load), and what `chain::check_fee` adds.
#[test]
fn the_lowest_fuel_fee() {
    for t in non_program_txs().into_iter().chain([vector_tx(Action::ProgramsPause {})]) {
        assert_eq!(lowest_fuel_fee(&t), 0, "{}", t.action.kind());
    }
    let p = priced(250);
    for a in program_vector_actions().into_iter().filter(|a| !matches!(a, Action::ProgramsPause {})) {
        let t = vector_tx(a);
        assert_eq!(lowest_fuel_fee(&t), exec(&t, &SCHEDULE_V0) / 1_000, "{}", t.action.kind());
        assert!(lowest_fuel_fee(&t) <= fuel_fee(&p.st, &t, 3), "{}", t.action.kind());
        // `chain::check_fee` refuses a fee one drop short of the bytes' and the fuel's lower bounds.
        let least = crate::econ::lowest_base_fee(t.size()) + lowest_fuel_fee(&t);
        let short = Tx { fee: least + t.tip - 1, ..t.clone() };
        assert!(crate::chain::check_fee(&short).is_err(), "{}", t.action.kind());
        crate::chain::check_fee(&Tx { fee: least + t.tip, ..t.clone() }).unwrap();
    }
}

/// The rate is `max(fuel_rate, floor)` (§5.6): after the council raises the floor above the
/// controller's rate, every program tx pays the floor at once, and the next block's end brings the
/// stored rate up to it; after it lowers the floor, the stored rate stands and falls with empty
/// blocks toward the new floor.
#[test]
fn the_rate_follows_a_floor_change() {
    let p = priced(250);
    let mut st = p.st.clone();
    let t = tx(&st, ALICE, upload(ESCROW.0, ESCROW.1, FANG), 3);
    let reserved = reserved(&st, &t, 3);
    let floor = |st: &mut State, v: u64| {
        st.programs.meta.as_mut().unwrap().params.insert(Param::FuelFloorRate.key().into(), v);
    };
    floor(&mut st, 1_000);
    assert_eq!((rate(&st), fuel_fee(&st, &t, 3)), (1_000, reserved));
    let empty = |st: &mut State, h: u64| end_block(st, &BlockInfo { chain_id: CHAIN, height: h, version: 2, hash: "h", proposer: "p", view: h, parent_view: h - 1, skipped: &[], tx_bytes: 0 });
    // An empty block lowers 1,000 by 12.5%, below the floor, which holds it.
    empty(&mut st, 3);
    assert_eq!(st.programs.meta.as_ref().unwrap().fuel_rate, 1_000);
    floor(&mut st, 100);
    assert_eq!(rate(&st), 1_000, "a lower floor doesn't lower the rate");
    empty(&mut st, 4);
    assert_eq!(st.programs.meta.as_ref().unwrap().fuel_rate, 875);
    // Each step is `next_slot`'s: down 12.5% for an empty block, to the floor at last.
    for h in 5..60 {
        empty(&mut st, h);
    }
    assert_eq!(st.programs.meta.as_ref().unwrap().fuel_rate, 100);
}

/// `end_block` does nothing while programs are dormant: on a v1 state and on a v2 state before
/// programs first switch on (no META), the state is exactly as before, its root included.
#[test]
fn the_rate_step_does_nothing_without_programs() {
    let mut v1 = genesis(&[]).initial_state();
    v1.upgrade.schedule = vec![];
    let dormant = genesis(&[]).initial_state();
    for (label, mut st) in [("v1", v1), ("dormant", dormant)] {
        st.commit_block().unwrap();
        let root = st.full_root();
        for h in [1, 2, 3] {
            let version = crate::upgrade::version_at(&st, h);
            end_block(&mut st, &BlockInfo { chain_id: CHAIN, height: h, version, hash: "h", proposer: "p", view: h, parent_view: h - 1, skipped: &[], tx_bytes: 1_000_000 });
        }
        assert!(st.programs.meta.is_none(), "{label}");
        assert_eq!(st.programs.block, BlockRun::default(), "{label}");
        assert_eq!(st.full_root(), root, "{label}");
    }
}

/// One full block (its counted fuel at `program_max_block_fuel`) raises the rate by `next_slot`'s
/// step (12.5%, at least a milli-drop); a block at the target (half full) leaves it; an empty one
/// lowers it 12.5%. The counted fuel is the block's own (a run left at another height counts as
/// empty). And `next_rate` is `next_slot`'s formula on a rate both clamps leave alone.
#[test]
fn a_full_block_raises_the_rate_by_next_slots_step() {
    let mut st = priced(250).st;
    let limit = param(&st, Param::MaxBlockFuel);
    let step = st.chain_params.base_fee_max_change_bps;
    let run = |st: &mut State, h: u64, fuel: u64| {
        st.programs.block = BlockRun { height: h, fuel, ..BlockRun::default() };
        end_block(st, &BlockInfo { chain_id: CHAIN, height: h, version: 2, hash: "h", proposer: "p", view: h, parent_view: h - 1, skipped: &[], tx_bytes: 0 });
        st.programs.meta.as_ref().unwrap().fuel_rate
    };
    assert_eq!(run(&mut st, 3, limit), 250 + 250 * step / 10_000);
    assert_eq!(run(&mut st, 4, 0), 250, "an empty block at the floor stays there");
    st.programs.meta.as_mut().unwrap().fuel_rate = 1_000;
    assert_eq!(run(&mut st, 5, limit), 1_125);
    assert_eq!(run(&mut st, 6, limit / 2), 1_125, "at the target, unchanged");
    assert_eq!(run(&mut st, 7, 0), 1_125 - 1_125 * step / 10_000);
    // A run left by block 8 counts nothing at block 9.
    st.programs.meta.as_mut().unwrap().fuel_rate = 1_000;
    st.programs.block = BlockRun { height: 8, fuel: limit, ..BlockRun::default() };
    end_block(&mut st, &BlockInfo { chain_id: CHAIN, height: 9, version: 2, hash: "h", proposer: "p", view: 9, parent_view: 8, skipped: &[], tx_bytes: 0 });
    assert_eq!(st.programs.meta.as_ref().unwrap().fuel_rate, 875);
    // The same formula as the byte rate's, above both floors.
    let cp = st.chain_params.clone();
    for rate in [20_000u64, 123_456, 1_000_000, 1 << 30] {
        for used in [0, 1, limit / 4, limit / 2, limit / 2 + 1, 3 * limit / 4, limit, 2 * limit] {
            let slot = crate::econ::FeeSlot { rate, ..crate::econ::FeeSlot::floor(&cp) };
            assert_eq!(next_rate(rate, used, limit, &cp), crate::econ::next_slot(&slot, used, limit, &cp).rate, "rate {rate}, used {used}");
        }
    }
    // A full block from the ceiling stays there.
    st.programs.meta.as_mut().unwrap().fuel_rate = MAX_FUEL_RATE;
    assert_eq!(run(&mut st, 10, limit), MAX_FUEL_RATE);
}

/// `suggested_rate` is the rate raised by a full block's step for each of the next `FEE_LAG`
/// blocks: what 17 full blocks in a row would charge at most.
#[test]
fn the_suggested_rate_covers_seventeen_full_blocks() {
    let mut st = priced(250).st;
    let limit = param(&st, Param::MaxBlockFuel);
    let suggested = suggested_rate(&st);
    for h in 3..3 + FEE_LAG {
        st.programs.block = BlockRun { height: h, fuel: limit, ..BlockRun::default() };
        end_block(&mut st, &BlockInfo { chain_id: CHAIN, height: h, version: 2, hash: "h", proposer: "p", view: h, parent_view: h - 1, skipped: &[], tx_bytes: 0 });
    }
    assert_eq!(st.programs.meta.as_ref().unwrap().fuel_rate, suggested);
    assert_eq!(suggested, 1_833, "docs/PROGRAMS-V0.md §5.5's example, from the default floor");
}

/// Proposal order (§5.6): every v1 vector tx orders against every other, at every tip, exactly as
/// tip per byte did (the comparison before programs, cross-multiplied), so every proposal and
/// every eviction keeps v1's order. A program tx's share is the larger of its bytes' and its fuel's:
/// a heavy call that tips the same per byte as a light tx ranks below it, and tipping the same per
/// share ranks equal.
#[test]
fn proposal_order_is_tip_per_share() {
    let before = |a: &Tx, b: &Tx| (u128::from(a.tip) * u128::from(b.size().max(1))).cmp(&(u128::from(b.tip) * u128::from(a.size().max(1))));
    let txs: Vec<Tx> = non_program_txs().into_iter().enumerate().flat_map(|(i, t)| [0, 1, 7 + i as u64, 1_000 * i as u64, u64::MAX].map(|tip| Tx { tip, ..t.clone() })).collect();
    for a in &txs {
        assert_eq!(share(a), a.size() * CALIBRATED_BLOCK_FUEL);
        for b in &txs {
            assert_eq!(TipRate::of(a).cmp(&TipRate::of(b)), before(a, b), "{} tipping {} against {} tipping {}", a.action.kind(), a.tip, b.action.kind(), b.tip);
        }
    }
    // A light transfer, and a heavy call (10M fuel declared) tipping more per byte: the call's
    // share is its fuel's, so it ranks below, where tip per byte put it above.
    let light = Tx { tip: 1_000, ..vector_tx(Action::Transfer { to: "x.boo".into(), amount: 5 }) };
    let heavy = vector_tx(calls(vec![call(&program_of(BOB, 0), &"d2".repeat(32), "increment", 0)], 10_000_000, vec![]));
    let heavy = Tx { tip: 1_000 * heavy.size() / light.size() + 1, ..heavy };
    assert_eq!(before(&heavy, &light), Ordering::Greater, "it tips more per byte");
    assert!(share(&heavy) == 10_000_000 * crate::consensus::MAX_BLOCK_BYTES as u64 && share(&heavy) > heavy.size() * CALIBRATED_BLOCK_FUEL);
    assert_eq!(TipRate::of(&heavy).cmp(&TipRate::of(&light)), Ordering::Less);
    // Tipping the same per share ranks equal (cross-multiplied exactly): tips in the shares' ratio.
    let gcd = |mut a: u64, mut b: u64| {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    };
    let (light_share, heavy_share) = (share(&light), share(&heavy));
    let g = gcd(heavy_share, light_share);
    let (light, heavy) = (Tx { tip: light_share / g, ..light }, Tx { tip: heavy_share / g, ..heavy });
    assert_eq!(TipRate::of(&heavy).cmp(&TipRate::of(&light)), Ordering::Equal);
    assert_eq!(TipRate::of(&Tx { tip: heavy.tip + 1, ..heavy.clone() }).cmp(&TipRate::of(&light)), Ordering::Greater);
    // An upload's share is the larger of its two too: here its fuel's (71 fuel a byte of code
    // against 60M fuel to 4 MiB a block, about 14.3).
    let up = vector_tx(upload(ESCROW.0, ESCROW.1, 0));
    let (bytes, fuel) = (up.size() * CALIBRATED_BLOCK_FUEL, SCHEDULE_V0.upload_fuel(ESCROW.0.len(), ESCROW.1.len()) * crate::consensus::MAX_BLOCK_BYTES as u64);
    assert!(fuel > bytes);
    assert_eq!(share(&up), fuel);
}

/// The block counter through `apply_j` (§5.4): a program tx's reservation is counted in the fee
/// phase in `Block` mode, never in `Mempool` mode; a tx that reserves nothing writes nothing (the
/// run keeps the height it had); a rolled-back trial un-counts. (The budget's refusals are
/// admission's: `admit::tests`.)
#[test]
fn the_block_counts_fuel_in_block_mode_only() {
    let mut st = priced(250).st;
    let h = 3;
    st.programs.block = BlockRun { height: 2, fuel: 5, ..BlockRun::default() };
    let upload_tx = |st: &State| tx(st, ALICE, upload(ESCROW.0, ESCROW.1, 0), h);
    let reserved_up = SCHEDULE_V0.upload_fuel(ESCROW.0.len(), ESCROW.1.len());
    // A transfer writes nothing.
    let transfer = tx(&st, ALICE, Action::Transfer { to: addr(BOB), amount: FANG }, h);
    assert_eq!(st.apply(&transfer, CHAIN, &addr(VALIDATOR), h).unwrap(), Outcome::Applied);
    assert_eq!(st.programs.block, BlockRun { height: 2, fuel: 5, ..BlockRun::default() });
    // Mempool mode: admitted, nothing counted.
    let mut j = Journal::default();
    assert!(matches!(st.apply_j(&mut j, &upload_tx(&st), CHAIN, &addr(VALIDATOR), h, ApplyMode::Mempool), Ok(Outcome::Applied)));
    st.rollback(j);
    assert_eq!(st.programs.block.fuel_at(h), 0);
    // Block mode: counted (the action itself fails, charged: a `storage_limit` of 0 can't bond the
    // blobs' deposits).
    let mut j = Journal::default();
    let r = st.apply_j(&mut j, &upload_tx(&st), CHAIN, &addr(VALIDATOR), h, ApplyMode::Block);
    assert!(matches!(&r, Ok(Outcome::Failed(why)) if why.starts_with("deposits of ") && why.ends_with(" drops pass the transaction's storage limit of 0")), "{r:?}");
    // (Its receipt, failed, is in the run too: receipts outlive an action's rollback, §8.1.)
    assert_eq!((st.programs.block.height, st.programs.block.fuel, st.programs.block.receipts.len()), (h, reserved_up, 1));
    // Rolled back: the run as it was, the older height's.
    st.rollback(j);
    assert_eq!(st.programs.block, BlockRun { height: 2, fuel: 5, ..BlockRun::default() });
    assert_eq!(st.programs.block.fuel_at(h), 0);
    // Two in a row add up.
    for _ in 0..2 {
        assert!(st.apply(&upload_tx(&st), CHAIN, &addr(VALIDATOR), h).is_ok());
    }
    assert_eq!(st.programs.block.fuel_at(h), 2 * reserved_up);
}

// ---- WBS T: the rate and the fees over their parameters' bounds ----

/// A fuel rate anywhere the controller reaches: around the default floor, the middle, and up to
/// the ceiling.
fn rates() -> impl Strategy<Value = u64> {
    prop_oneof![1u64..1_000, 1_000u64..1_000_000, 1_000_000u64..=MAX_FUEL_RATE, Just(1), Just(MAX_FUEL_RATE)]
}

/// A floor the parameter allows (its bounds are 1 and `MAX_FUEL_FLOOR_RATE`), the bounds themselves
/// and the low end, where the stateless lower bound is exact, included.
fn floors() -> impl Strategy<Value = u64> {
    prop_oneof![Just(1), 1u64..1_000, 1u64..=MAX_FUEL_FLOOR_RATE, Just(MAX_FUEL_FLOOR_RATE)]
}

/// A value of a parameter bounded by `lo..=hi`: each bound a quarter of the draws, anywhere between
/// the rest. (proptest's integer ranges are uniform: a bound would come up once in `hi - lo + 1`
/// draws, and a rule that lives at a bound, a zero step above all, would go untested.)
fn bounded(lo: u64, hi: u64) -> impl Strategy<Value = u64> {
    prop_oneof![1 => Just(lo), 1 => Just(hi), 2 => lo..=hi]
}

/// The block fuel limit `next_rate` is given: the parameter's bounds and anywhere between, and the
/// edges `next_slot` also takes (0, which it reads as 1, and tiny limits).
fn limits() -> impl Strategy<Value = u64> {
    prop_oneof![1 => Just(0), 1 => Just(1), 1 => Just(2), 3 => bounded(MIN_BLOCK_FUEL, CALIBRATED_BLOCK_FUEL), 2 => 1u64..=CALIBRATED_BLOCK_FUEL]
}

/// How much of its limit a block counted: nothing, around the target (the formula's three
/// branches meet there), the limit, past it (`next_slot` is given bytes past the limit too), or
/// anything in between.
fn counted(limit: u64, target_bps: u64, pick: u8, any: u64) -> u64 {
    let target = (u128::from(limit.max(1)) * u128::from(target_bps) / 10_000).max(1) as u64;
    match pick % 7 {
        0 => 0,
        1 => target - 1,
        2 => target,
        3 => target + 1,
        4 => limit,
        5 => limit + limit / 2,
        _ => any % (limit + limit / 2 + 1),
    }
}

/// `ChainParams` with the fee controller's two knobs (and the byte floor, which `next_slot` clamps
/// to) set, at any value their bounds allow.
fn controller(target_bps: u64, step_bps: u64, per_byte: u64) -> ChainParams {
    ChainParams { target_fullness_bps: target_bps, base_fee_max_change_bps: step_bps, base_fee_floor_per_byte: per_byte, ..ChainParams::default() }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 4096, failure_persistence: None, ..ProptestConfig::default() })]

    /// `next_rate` is `econ::next_slot`'s formula on a rate (§5.6): for every rate the fuel controller
    /// can hold, every limit a block can have (0, which `next_slot` reads as 1, included) and every
    /// count of fuel (past the limit too), under every target and step the parameters allow (their
    /// bounds drawn a quarter of the time each: `bounded`), `next_slot(..).rate == next_rate(..)
    /// .clamp(floor_rate, econ::MAX_RATE)`: both answer the same wherever `next_slot`'s own clamp (the
    /// byte rate's floor and ceiling, which a fuel rate's `end_block` clamps replace) doesn't act.
    #[test]
    fn next_rate_is_next_slots_formula(rate in rates(), limit in limits(), target in bounded(100, 10_000), step in bounded(0, 10_000), per_byte in bounded(1, 1_000_000), pick in any::<u8>(), any in any::<u64>()) {
        let cp = controller(target, step, per_byte);
        let used = counted(limit, target, pick, any);
        let ours = next_rate(rate, used, limit, &cp);
        let slot = econ::next_slot(&FeeSlot { rate, ..FeeSlot::floor(&cp) }, used, limit, &cp).rate;
        let floor = (per_byte * 1_000).min(econ::MAX_RATE);
        prop_assert_eq!(slot, ours.clamp(floor, econ::MAX_RATE), "rate {} used {} of {}: next_slot's answer is ours, clamped", rate, used, limit);
        if (floor..=econ::MAX_RATE).contains(&ours) {
            prop_assert_eq!(slot, ours, "the same answer where the clamp doesn't act");
        }
        // The controller's shape: a block above the target raises the rate (at least a milli-drop,
        // if the step is not 0), one below it lowers it, one at it leaves it.
        let target_used = (u128::from(limit.max(1)) * u128::from(target) / 10_000).max(1) as u64;
        let max = limit.max(1);
        if step == 0 {
            prop_assert_eq!(ours, rate, "a zero step never moves the rate");
        } else if used.min(max) > target_used && max > target_used {
            prop_assert!(ours > rate);
        } else if used.min(max) < target_used {
            prop_assert!(ours <= rate);
        } else {
            prop_assert_eq!(ours, rate);
        }
    }

    /// Fuller blocks never lower the rate more: `next_rate` grows with the fuel counted.
    #[test]
    fn next_rate_grows_with_the_fuel_counted(rate in rates(), limit in limits().prop_filter("a limit", |l| *l > 0), target in bounded(100, 10_000), step in bounded(0, 10_000), a in any::<u64>(), b in any::<u64>()) {
        let cp = controller(target, step, 10);
        let (a, b) = (a % (limit + 1), b % (limit + 1));
        let (less, more) = (a.min(b), a.max(b));
        prop_assert!(next_rate(rate, less, limit, &cp) <= next_rate(rate, more, limit, &cp));
    }
}

/// `next_rate` against `next_slot` on one input: `(next_rate, next_slot's rate, next_slot's byte floor)`.
fn both(rate: u64, used: u64, limit: u64, cp: &ChainParams) -> (u64, u64, u64) {
    let slot = econ::next_slot(&FeeSlot { rate, ..FeeSlot::floor(cp) }, used, limit, cp).rate;
    (next_rate(rate, used, limit, cp), slot, (cp.base_fee_floor_per_byte * 1_000).min(econ::MAX_RATE))
}

/// A step of 0 never moves the rate, whatever the block counted: the one-milli-drop climb above the
/// target belongs to a non-zero step (§5.6), as `next_slot`'s does. The property above draws step 0
/// a quarter of the time; this is the deterministic case beside it.
#[test]
fn a_zero_step_never_moves_the_rate() {
    let cp = controller(5_000, 0, 1);
    for rate in [1u64, 1_500, 1_000_000, MAX_FUEL_RATE] {
        for used in [0u64, 19_999_999, 20_000_000, 20_000_001, 30_000_000, 40_000_000, 60_000_000] {
            let (ours, slot, floor) = both(rate, used, 40_000_000, &cp);
            assert_eq!(ours, rate, "rate {rate}, used {used}");
            assert_eq!(slot, rate.clamp(floor, econ::MAX_RATE), "rate {rate}, used {used}: next_slot's");
        }
    }
}

/// The controller at its parameters' bounds, deterministically (the property draws them too):
/// every target (100, the default 5,000, 10,000 bps), step (0, the default 1,250, 10,000) and byte
/// floor bound, from the lowest rate to the ceiling, at a block that counted nothing, one under the
/// target, the target, one over, the limit and past it. `next_slot`'s rate is `next_rate`'s within its
/// clamp; a step of 0 never moves the rate; a step of 10,000 doubles it at a full block and takes it
/// to 0 at an empty one.
#[test]
fn next_rate_at_the_controllers_bounds() {
    for limit in [MIN_BLOCK_FUEL, 40_000_000, CALIBRATED_BLOCK_FUEL] {
        for target in [100u64, 5_000, 10_000] {
            for step in [0u64, 1_250, 10_000] {
                for per_byte in [1u64, 1_000_000] {
                    let cp = controller(target, step, per_byte);
                    let at = limit * target / 10_000;
                    for rate in [1u64, 1_000, 1_500, 123_456_789, MAX_FUEL_RATE] {
                        for used in [0, at - 1, at, at + 1, limit, 2 * limit] {
                            let (ours, slot, floor) = both(rate, used, limit, &cp);
                            let what = format!("limit {limit}, target {target}, step {step}, per byte {per_byte}, rate {rate}, used {used}");
                            assert_eq!(slot, ours.clamp(floor, econ::MAX_RATE), "{what}");
                            if step == 0 {
                                assert_eq!(ours, rate, "{what}");
                            }
                            // At the largest step a block above the target doubles the rate (the full
                            // block's step is the whole rate), and an empty one takes it to 0, where
                            // `end_block`'s floor holds it.
                            if step == 10_000 && used >= limit && target < 10_000 {
                                assert_eq!(ours, 2 * rate, "{what}");
                            }
                            if step == 10_000 && used == 0 {
                                assert_eq!(ours, 0, "{what}");
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The state the controller's tests run on: programs on, the floor, the stored rate, the block limit
/// and the controller's knobs as given.
fn controlled(floor: u64, rate: u64, limit: u64, cp: &ChainParams) -> State {
    let mut st = state_on();
    let m = st.programs.meta.as_mut().unwrap();
    m.fuel_rate = rate;
    m.params.insert(Param::FuelFloorRate.key().into(), floor);
    m.params.insert(Param::MaxBlockFuel.key().into(), limit);
    st.chain_params = cp.clone();
    st
}

/// The block at `height` counted `fuel`, and ended.
fn end_with(st: &mut State, height: u64, fuel: u64) {
    st.programs.block = BlockRun { height, fuel, ..BlockRun::default() };
    end_block(st, &BlockInfo { chain_id: CHAIN, height, version: 2, hash: "h", proposer: "p", view: height, parent_view: height - 1, skipped: &[], tx_bytes: 0 });
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, failure_persistence: None, ..ProptestConfig::default() })]

    /// `suggested_rate` covers any 17 blocks (§5.5): whatever the stored rate, floor, limit and
    /// controller, and however full the next `FEE_LAG` blocks are, none of them is charged more
    /// than it (the rate a block is charged is the one the block before it left, `rate`), and
    /// 17 full blocks reach it exactly unless the ceiling stops them first.
    #[test]
    fn the_suggested_rate_covers_any_seventeen_blocks(floor in floors(), stored in rates(), limit in bounded(MIN_BLOCK_FUEL, CALIBRATED_BLOCK_FUEL), target in bounded(100, 10_000), step in bounded(0, 10_000), fullness in proptest::collection::vec(bounded(0, 10_000), FEE_LAG as usize)) {
        let mut st = controlled(floor, stored, limit, &controller(target, step, 10));
        let suggested = suggested_rate(&st);
        prop_assert!(suggested <= MAX_FUEL_RATE.max(floor));
        let mut full = st.clone();
        for (k, bps) in fullness.iter().enumerate() {
            prop_assert!(rate(&st) <= suggested, "block {} would be charged {} > {}", k, rate(&st), suggested);
            end_with(&mut st, 3 + k as u64, (u128::from(limit) * u128::from(*bps) / 10_000) as u64);
            end_with(&mut full, 3 + k as u64, limit);
        }
        prop_assert!(rate(&st) <= suggested);
        prop_assert_eq!(rate(&full), suggested, "17 full blocks reach it");
    }

    /// `lowest_fuel_fee` is never above what a state charges (§5.3), over the parameters' bounds: for
    /// any state in which programs aren't dormant (a dormant one refuses every program tx), with
    /// any floor the fuel parameter allows, any stored rate, any byte floor and a fee slot above it,
    /// any tip cap, any of the three tags (uploads of any size, creates with `init` and calls with
    /// any limit, declaring any code the state holds) and a fee that is exactly the state's charge
    /// (so the state admits it), the stateless check (`chain::check_fee`: gossip, HTTP, block
    /// verification) admits it too, and the bytes' lower bound is below the state's byte fee.
    #[test]
    fn the_stateless_bound_never_refuses_what_a_state_accepts(
        action in program_action(), status in 0u8..3, floor in floors(), stored in rates(),
        flat in bounded(0, FANG), per_byte in bounded(1, 1_000_000), slot_flat in bounded(0, FANG), slot_rate in bounded(0, 1_000_000_000), multiple in bounded(1, 1_000), tip_percent in bounded(0, 100),
    ) {
        let mut st = WORLD.with(|p| p.st.clone());
        st.chain_params.base_fee_floor_flat = flat;
        st.chain_params.base_fee_floor_per_byte = per_byte;
        st.chain_params.tip_cap_multiple = multiple;
        econ::init_genesis(&mut st);
        let h = 3;
        // The fee slot of block 3 (the ring starts at height 0): above the floor by any amount.
        let slot = st.econ.fees[h as usize];
        st.econ.fees[h as usize] = FeeSlot { flat: slot.flat.saturating_add(slot_flat), rate: slot.rate.saturating_add(slot_rate), ..slot };
        let m = st.programs.meta.as_mut().unwrap();
        m.fuel_rate = stored;
        m.params.insert(Param::FuelFloorRate.key().into(), floor);
        match status {
            0 => {}
            1 => {
                st.upgrade.features.remove(crate::upgrade::PROGRAMS_FEATURE);
            }
            _ => st.programs.meta.as_mut().unwrap().pause = Some(Pause { from: h, until: h + 10, started: h, signers: vec![] }),
        }
        prop_assert_ne!(crate::programs::status(&st, h), Status::Dormant);
        let t = tx(&st, ALICE, action, h);
        prop_assert!(is_program_tx(&t));
        let bytes = econ::base_fee(&st, h, t.size());
        let fuel = fuel_fee(&st, &t, h);
        prop_assert!(lowest_fuel_fee(&t) <= fuel, "the stateless fuel fee {} is above the state's {}", lowest_fuel_fee(&t), fuel);
        prop_assert!(econ::lowest_base_fee(t.size()) <= bytes);
        let base = bytes.saturating_add(fuel);
        let tip = u64::try_from(u128::from(econ::max_tip(&st, base)) * u128::from(tip_percent) / 100).unwrap();
        // A fee the base fee and the tip overflow is a fee no one can sign.
        prop_assume!(base.checked_add(tip).is_some());
        let t = Tx { fee: base + tip, tip, ..t };
        prop_assert_eq!(econ::check_fee(&st, &t, h).unwrap(), base + tip, "the state admits it");
        prop_assert!(crate::chain::check_fee(&t).is_ok(), "so does the stateless check");
    }
}

thread_local! {
    /// The state the fee property runs on, built once per test thread: the counter, the token and a
    /// program with the largest code the codec takes installed.
    static WORLD: Priced = priced(250);
}

/// A program action of any of the three tags, with any of the sizes and limits the codec takes: an
/// upload (its code and interface of any length, `exec` by their sum), a create with `init`
/// (a `fuel_limit`, declaring its own code and its bindings'), and a call (a `fuel_limit`, up to 8
/// calls and 16 bindings among the three programs the state holds).
fn program_action() -> impl Strategy<Value = Action> {
    let max_code = blockula_vm::config::MAX_CODE_BYTES;
    let max_interface = blockula_vm::config::MAX_INTERFACE_BYTES;
    let upload_ = (8usize..=max_code, 1usize..=max_interface).prop_map(|(w, i)| upload(&vec![0; w], &vec![0; i], FANG));
    let bindings = |picks: Vec<usize>| -> Vec<Binding> {
        WORLD.with(|p| {
            let programs = [&p.counter, &p.token, &p.big];
            let mut seen = vec![];
            picks.into_iter().filter(|i| !seen.contains(i) && { seen.push(*i); true }).map(|i| Binding { program: programs[i % 3].clone(), version_digest: hex::encode(blockula_vm::exec::ANY_VERSION) }).collect()
        })
    };
    let create_ = (1u64..=MAX_TX_FUEL, proptest::collection::vec(0usize..3, 0..4)).prop_map(move |(fuel_limit, picks)| Action::CreateProgram {
        wasm_hash: hex::encode(digest::wasm_hash(TOKEN.0)),
        interface_hash: hex::encode(digest::interface_hash(TOKEN.1)),
        authority: None,
        upgrade_delay_blocks: 0,
        policy: crate::types::ProgramPolicy::Open,
        accepts_transfers: false,
        init: Some(InitCall { method: "init".into(), input: HexBytes(vec![]), value: 0 }),
        fuel_limit,
        storage_limit: FANG,
        bindings: bindings(picks),
    });
    let call_ = (proptest::collection::vec(0usize..3, 1..=8), proptest::collection::vec(0usize..3, 0..4), 1u64..=MAX_TX_FUEL).prop_map(move |(calls_, picks, fuel_limit)| {
        let called = WORLD.with(|p| calls_.iter().map(|i| call([&p.counter, &p.token, &p.big][*i % 3], &"d2".repeat(32), "increment", 0)).collect());
        calls(called, fuel_limit, bindings(picks))
    });
    prop_oneof![upload_, create_, call_]
}

/// The stateless bound is exact where the parameters are at their lower bounds (§5.3): a byte floor
/// of 0 flat and 1 per byte, a fuel floor of 1 milli-drop per fuel, an upload (it loads nothing):
/// the state charges exactly `lowest_base_fee + lowest_fuel_fee`, a fee of that is admitted by both
/// checks, and one drop less is refused by both. So the bound is no looser than the parameters
/// allow, and a mutant that raises it (or lowers it past a state's charge) is caught by this and
/// the property above.
#[test]
fn the_stateless_bound_is_exact_at_the_parameters_lower_bounds() {
    let mut st = WORLD.with(|p| p.st.clone());
    st.chain_params.base_fee_floor_flat = 0;
    st.chain_params.base_fee_floor_per_byte = 1;
    econ::init_genesis(&mut st);
    let m = st.programs.meta.as_mut().unwrap();
    m.fuel_rate = 1;
    m.params.insert(Param::FuelFloorRate.key().into(), 1);
    let h = 3;
    for (wasm, interface) in [(8usize, 1usize), (1_000, 100), (blockula_vm::config::MAX_CODE_BYTES, blockula_vm::config::MAX_INTERFACE_BYTES)] {
        let t = tx(&st, ALICE, upload(&vec![0; wasm], &vec![0; interface], FANG), h);
        let (bytes, fuel) = (econ::base_fee(&st, h, t.size()), fuel_fee(&st, &t, h));
        assert_eq!((bytes, fuel), (econ::lowest_base_fee(t.size()), lowest_fuel_fee(&t)), "{wasm} bytes of code");
        assert_eq!(fuel, SCHEDULE_V0.upload_fuel(wasm, interface) / 1_000);
        let paid = Tx { fee: bytes + fuel, ..t.clone() };
        assert_eq!(econ::check_fee(&st, &paid, h).unwrap(), bytes + fuel);
        crate::chain::check_fee(&paid).unwrap();
        let short = Tx { fee: bytes + fuel - 1, ..t };
        assert!(econ::check_fee(&st, &short, h).is_err() && crate::chain::check_fee(&short).is_err());
    }
}

/// 10,000 consecutive full blocks overflow neither the rate nor a charge (§5.6, PRG 5.2's test), at
/// the default step: the block counts exactly `program_max_block_fuel` each time, from the default
/// floor. See [`full_blocks`]. The ceiling is reached after about 177 blocks (§5.3).
#[test]
fn ten_thousand_full_blocks_overflow_neither_the_rate_nor_a_charge() {
    full_blocks(1_250, 170..=185);
}

/// The same run at `base_fee_max_change_bps`' bound, 10,000: the rate doubles a block, so the
/// ceiling is 31 blocks away, and the controller's u128 product is at its largest.
#[test]
fn ten_thousand_full_blocks_at_the_largest_step_overflow_neither_the_rate_nor_a_charge() {
    full_blocks(10_000, 25..=35);
}

/// 10,000 consecutive full blocks at a step of `step` bps, from the default floor, ending each with
/// `programs::end_block`: the rate follows an oracle written out here (`rate + max(1, rate × step /
/// 10,000)`, held at [`MAX_FUEL_RATE`]) and `next_slot`'s own step wherever its byte floor doesn't act;
/// it first reaches the ceiling after a number of blocks in `reaches`, holds there, never exceeds it
/// and never falls. At every block a large admissible reservation (a call of 10M `fuel_limit` with
/// two programs of the largest code: 40,991,520 fuel) costs exactly `rate × reserved / 1000` in a u64,
/// with no wrap and no saturation, and `econ::check_fee` and `econ::charge` add it to the byte fee and
/// a tip at the tip cap's two extremes, the default 20× and the bound 1,000×, without overflowing
/// (a call stays below 2^64 even at both ceilings, near 70% of it). Then, at both ceilings and the
/// 1,000× cap, a large upload does pass it: its tip saturates `max_tip`, `base + tip` overflows,
/// `check_fee` refuses it and `econ::charge` is `None`, never a wrapped sum; and the sum that exactly
/// reaches `u64::MAX` is the largest charge admitted.
fn full_blocks(step: u64, reaches: std::ops::RangeInclusive<u64>) {
    let mut p = priced(250);
    // The second program of the largest code: two bound programs make a call declare 512 KiB.
    let mut other = largest_wasm();
    *other.last_mut().unwrap() = 1;
    let other_program = program_of(ALICE, 3);
    let blobs = store(&mut p.st, &other, COUNTER.1, 1);
    install(&mut p.st, &other_program, blobs, 2);
    let mut st = p.st;
    st.chain_params.base_fee_max_change_bps = step;
    let any = hex::encode(blockula_vm::exec::ANY_VERSION);
    let heavy = calls(vec![call(&p.big, &"d2".repeat(32), "increment", 0)], MAX_TX_FUEL, vec![Binding { program: other_program, version_digest: any }]);
    let h = 3;
    let t = tx(&st, ALICE, heavy, h);
    let large = MAX_TX_FUEL + 2 * SCHEDULE_V0.load_fuel(blockula_vm::config::MAX_CODE_BYTES);
    assert_eq!((reserved(&st, &t, h), large), (40_991_520, 40_991_520));
    assert!(large <= MAX_RESERVATION);
    assert!(u128::from(MAX_FUEL_RATE) * u128::from(MAX_RESERVATION) / 1_000 < u128::from(u64::MAX), "even a reservation at the cap");
    let (limit, floor) = (param(&st, Param::MaxBlockFuel), param(&st, Param::FuelFloorRate));
    assert_eq!((limit, floor), (40_000_000, 250));
    st.programs.meta.as_mut().unwrap().fuel_rate = floor;
    // The oracle: a full block's step is the whole of `rate × step / 10,000` (floored, at least one).
    let mut model = floor;
    let mut reached = None;
    let mut cp = st.chain_params.clone();
    cp.base_fee_floor_per_byte = 1;
    const _: () = assert!(econ::MAX_RATE > 2 * MAX_FUEL_RATE, "the byte ceiling is above the fuel one, doubled");
    for k in 0..10_000u64 {
        let height = 3 + k;
        let before = st.programs.meta.as_ref().unwrap().fuel_rate;
        end_with(&mut st, height, limit);
        model = (model + (model * step / 10_000).max(1)).min(MAX_FUEL_RATE);
        let stored = st.programs.meta.as_ref().unwrap().fuel_rate;
        assert_eq!(stored, model, "block {k}");
        assert!(stored >= before, "block {k}: full blocks never lower the rate");
        // Where `next_slot`'s byte floor doesn't act (1,000 milli-drops at its lowest bound), the
        // step is its own; the fuel ceiling is below the byte one, so only this one cuts it.
        if before >= 1_000 {
            let slot = econ::next_slot(&FeeSlot { rate: before, ..FeeSlot::floor(&cp) }, limit, limit, &cp);
            assert_eq!(stored, slot.rate.min(MAX_FUEL_RATE), "block {k}: `next_slot`'s step on the same rate");
        }
        assert!(stored <= MAX_FUEL_RATE);
        if stored == MAX_FUEL_RATE && reached.is_none() {
            reached = Some(k + 1);
        }
        // The charge of the large reservation, at the rate the next block charges.
        assert_eq!(rate(&st), stored);
        let exact = u128::from(stored) * u128::from(large) / 1_000;
        assert!(exact < u128::from(u64::MAX));
        let (bytes, fuel) = (econ::base_fee(&st, height + 1, t.size()), fuel_fee(&st, &t, height + 1));
        assert_eq!(u128::from(fuel), exact, "block {k}: the fuel fee neither wraps nor saturates");
        for multiple in [20, 1_000] {
            st.chain_params.tip_cap_multiple = multiple;
            let base = bytes + fuel;
            let tip = econ::max_tip(&st, base);
            assert_eq!(u128::from(tip), (u128::from(base) * u128::from(multiple)).min(u128::from(u64::MAX)));
            let at_cap = Tx { fee: u64::MAX, tip, ..t.clone() };
            let charge = u64::try_from(u128::from(base) + u128::from(tip)).expect("a call stays below 2^64 at both ceilings and the largest cap");
            assert_eq!(econ::check_fee(&st, &at_cap, height + 1).unwrap(), charge, "block {k}, cap {multiple}");
            assert_eq!(econ::charge(&st, &at_cap, height + 1), Some(charge));
        }
        st.chain_params.tip_cap_multiple = 20;
    }
    let reached = reached.expect("the ceiling is reached");
    assert!(reaches.contains(&reached), "{reached} blocks");
    assert_eq!(st.programs.meta.as_ref().unwrap().fuel_rate, MAX_FUEL_RATE, "held there");
    assert_eq!(suggested_rate(&st), MAX_FUEL_RATE);

    // Both ceilings now (the byte rate extrapolates to its own): a large upload's charge passes a u64.
    let next = 3 + 10_000;
    st.chain_params.tip_cap_multiple = 1_000;
    let up = tx(&st, ALICE, upload(&largest_wasm(), &vec![0; blockula_vm::config::MAX_INTERFACE_BYTES], FANG), next);
    let (bytes, fuel) = (econ::base_fee(&st, next, up.size()), fuel_fee(&st, &up, next));
    assert_eq!(bytes, FeeSlot::pricing(st.chain_params.base_fee_floor_flat, econ::MAX_RATE).fee(up.size()), "the byte rate is at its ceiling too");
    assert_eq!(u128::from(fuel), u128::from(MAX_FUEL_RATE) * u128::from(SCHEDULE_V0.upload_fuel(blockula_vm::config::MAX_CODE_BYTES, blockula_vm::config::MAX_INTERFACE_BYTES)) / 1_000);
    let base = bytes + fuel;
    let tip = econ::max_tip(&st, base);
    assert!(u128::from(base) * 1_000 > u128::from(u64::MAX) && tip == u64::MAX, "the cap saturates");
    assert!(base.checked_add(tip).is_none(), "and the sum passes a u64");
    let over = Tx { fee: u64::MAX, tip, ..up.clone() };
    let e = econ::check_fee(&st, &over, next).unwrap_err().to_string();
    assert!(e.starts_with("fee too low: this tx costs"), "{e}");
    assert_eq!(econ::charge(&st, &over, next), None, "never a wrapped sum");
    // The sum that reaches u64::MAX exactly is the most a fee can ask; one drop more is refused.
    let exactly = Tx { fee: u64::MAX, tip: u64::MAX - base, ..up.clone() };
    assert_eq!(econ::check_fee(&st, &exactly, next).unwrap(), u64::MAX);
    assert_eq!(econ::charge(&st, &exactly, next), Some(u64::MAX));
    let one_more = Tx { tip: u64::MAX - base + 1, ..exactly };
    assert!(econ::check_fee(&st, &one_more, next).is_err());
    assert_eq!(econ::charge(&st, &one_more, next), None);
}
