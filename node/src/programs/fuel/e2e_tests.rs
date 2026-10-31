//! The fuel rate and its charge end to end (docs/PROGRAMS-V0.md §5.1, §5.6; WBS T's acceptance), on
//! S4b's probe chain: a dev chain born at v2 with programs on, one validator, a probe program to
//! call. The charge and the rate are written out here apart from the node's `fuel_fee`, `reserved`
//! and `next_rate` (§5.1's formulas and §5.3's constants), so the tests hold the node to the spec
//! and not to itself.

use super::super::probe::*;
use super::*;
use crate::consensus::finalized_from;
use crate::crypto::{self, Keys};
use crate::econ;
use crate::programs::receipt::FeeParts;

/// What a call of the probe with a `fuel_limit` of `limit` reserves: the limit, and the load of the
/// probe's code, once (§5.3's table: 10,000 and 40 fuel a byte).
fn reserved_by(limit: u64) -> u64 {
    limit + 10_000 + 40 * PROBE_WASM.len() as u64
}

/// §5.1's fuel fee: `rate × reserved / 1000`, floored.
fn fuel_at(rate: u64, reserved: u64) -> u64 {
    u64::try_from(u128::from(rate) * u128::from(reserved) / 1_000).unwrap()
}

/// §5.6's step at the default knobs (a target of half the block, a step of 12.5%) on a block that
/// counted `used` of `limit`, held at `floor`.
fn model_rate(rate: u64, used: u64, limit: u64, floor: u64) -> u64 {
    let (r, used, target, max) = (u128::from(rate), u128::from(used.min(limit)), u128::from(limit / 2), u128::from(limit));
    let next = if used > target {
        r + (r * 1_250 * (used - target) / ((max - target) * 10_000)).max(1)
    } else if used < target {
        r - r * 1_250 * (target - used) / (target * 10_000)
    } else {
        r
    };
    u64::try_from(next).unwrap().max(floor)
}

/// The stored fuel rate: what the next block's parent state holds (the rate it charges is this,
/// or the floor if that's higher).
fn stored(d: &Dev) -> u64 {
    d.c.state.programs.meta.as_ref().unwrap().fuel_rate
}

/// A call of the probe `p`'s `method` with a `fuel_limit` of `limit`.
fn call_of(d: &Dev, p: &str, method: &str, limit: u64) -> Action {
    Action::CallProgram { calls: vec![d.call(p, method, vec![], 0)], fuel_limit: limit, storage_limit: FANG, bindings: vec![] }
}

/// `action` from `k`'s account at `nonce` paying exactly `fee` (of which `tip` is the tip), signed.
fn tx_paying(d: &Dev, k: &Keys, nonce: u64, fee: u64, tip: u64, action: Action) -> Tx {
    let from = k.address_with(&d.prefix());
    Tx { chain_id: d.c.genesis.chain_id.clone(), genesis_hash: d.c.genesis_hash.clone(), nonce, from, fee, tip, valid_until: d.c.height() + 100, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) }.sign(k)
}

/// What a wallet signs for a call of `limit` fuel (§5.5): the highest base fee already fixed for its
/// bytes, and the suggested fuel rate on what it reserves, which covers any of the next 17 blocks.
fn wallet_tx(d: &Dev, k: &Keys, nonce: u64, tip: u64, p: &str, method: &str, limit: u64) -> Tx {
    let action = call_of(d, p, method, limit);
    let size = tx_paying(d, k, nonce, 0, tip, action.clone()).size();
    let fee = econ::max_fee(&d.c.state, size) + fuel_at(suggested_rate(&d.c.state), reserved_by(limit)) + tip;
    tx_paying(d, k, nonce, fee, tip, action)
}

/// Four calls from `k` counting 39.6M of the default block's 40M (99%).
fn full_block(d: &Dev, k: &Keys, p: &str) -> Vec<Tx> {
    let nonce = d.c.state.account(&k.address_with(&d.prefix())).nonce;
    (0..4).map(|i| wallet_tx(d, k, nonce + i, 0, p, "ret", 9_900_000)).collect()
}

/// Fuel is charged per §5.1 at the rate of the block that includes the transaction (§5.6): over a
/// run of blocks that raise the rate, lower it, leave it, and raise it again, each included call is
/// debited exactly its bytes' base fee, `rate × reserved / 1000` of fuel at the rate its block's
/// parent state held (the stored rate, which the floor holds up) and its tip, whatever the wallet's
/// fee allowed (it signed for the suggested rate, which is more once the rate is rising); the fee
/// is burned (the burn and the supply move by exactly bytes and fuel), the receipt's parts say the
/// same, and nothing is refunded: a call that runs out of fuel and one that aborts pay the same as
/// one that returns. The stored rate after each block is the spec's step on its counted fuel.
#[test]
fn fuel_is_charged_at_the_including_blocks_rate() {
    let mut d = Dev::new("fuel-charge");
    let p = d.deploy(1).remove(0);
    let (alice, bob) = (keys(ALICE), keys(BOB));
    let (limit, floor) = (param(&d.c.state, Param::MaxBlockFuel), param(&d.c.state, Param::FuelFloorRate));
    assert_eq!((limit, floor), (40_000_000, 250));
    // The two blocks of `deploy` counted an upload's fuel and nothing more: the floor holds.
    assert_eq!(stored(&d), floor);
    let mut model = floor;
    // Two calls that reserve exactly half a block, the controller's target.
    let half = 10_000_000 - 10_000 - 40 * PROBE_WASM.len() as u64;
    // Each block's calls, as (method, fuel limit); the senders alternate. Full blocks raise the
    // rate (by 12.4%); a quarter-full one lowers it; an empty one lowers it by 12.5%; one at the
    // target leaves it.
    let blocks: Vec<Vec<(&str, u64)>> = vec![
        vec![("ret", 9_900_000); 4],
        vec![("ret", 9_900_000); 4],
        vec![("ret", 9_900_000); 4],
        vec![("spin", 5_000_000), ("no", 3_000_000), ("ret", 2_000_000)],
        vec![],
        vec![("ret", half); 2],
        vec![("ret", 9_900_000); 4],
        vec![("ret", 9_900_000); 3],
        vec![("ret", 9_900_000); 4],
    ];
    let mut rates = vec![];
    for (n, spec) in blocks.iter().enumerate() {
        let height = d.next();
        let charged_rate = model;
        rates.push(charged_rate);
        let mut nonces = [&alice, &bob].map(|k| d.c.state.account(&k.address_with(&d.prefix())).nonce);
        let txs: Vec<Tx> = spec
            .iter()
            .enumerate()
            .map(|(i, (method, fuel))| {
                let who = i % 2;
                let nonce = nonces[who];
                nonces[who] += 1;
                // The first call of every other block tips.
                let tip = if i == 0 && n % 2 == 1 { 7_000 } else { 0 };
                wallet_tx(&d, if who == 0 { &alice } else { &bob }, nonce, tip, &p, method, *fuel)
            })
            .collect();
        let used: u64 = spec.iter().map(|(_, fuel)| reserved_by(*fuel)).sum();
        assert!(used <= limit);
        // What each is charged, by the spec: bytes at this block's slot, fuel at the stored rate.
        let parts: Vec<FeeParts> = txs.iter().map(|t| FeeParts { bytes: econ::base_fee(&d.c.state, height, t.size()), fuel: fuel_at(charged_rate, reserved_by(match &t.action { Action::CallProgram { fuel_limit, .. } => *fuel_limit, _ => unreachable!() })), tip: t.tip }).collect();
        let (balances, burned, supply) = ([ALICE, BOB].map(|s| d.balance(s)), d.c.state.burned, d.c.state.supply);
        let b = d.block(txs.clone());
        let owed = |who: usize| -> u64 { parts.iter().enumerate().filter(|(i, _)| i % 2 == who).map(|(_, p)| p.bytes + p.fuel + p.tip).sum() };
        assert_eq!((balances[0] - d.balance(ALICE), balances[1] - d.balance(BOB)), (owed(0), owed(1)), "block {n}: each sender is debited exactly its charges");
        let base: u64 = parts.iter().map(|p| p.bytes + p.fuel).sum();
        assert_eq!((d.c.state.burned - burned, supply - d.c.state.supply), (base, base), "block {n}: bytes and fuel are burned");
        assert_eq!(b.txs.len(), spec.len(), "block {n}: every call is included");
        for (i, (method, _)) in spec.iter().enumerate() {
            assert_eq!(b.programs[&i].fee, parts[i], "block {n}, call {i}: the receipt's parts");
            // Nothing refunds: out of fuel and an abort pay what a return does.
            assert_eq!(b.failed.contains_key(&i), matches!(*method, "spin" | "no"), "block {n}, call {i} ({method})");
        }
        // The wallet signed for the suggested rate: its fee covers the charge with room to spare,
        // and only the including block's charge is taken.
        for (i, (t, p)) in txs.iter().zip(&parts).enumerate() {
            assert!(t.fee > p.bytes + p.fuel + p.tip, "block {n}, call {i}: the margin isn't charged");
        }
        model = model_rate(model, used, limit, floor);
        assert_eq!(stored(&d), model, "block {n}: the rate after counting {used} of {limit}");
    }
    // The run did what it was built to: up, down, held and up again.
    assert_eq!(rates[0], floor);
    assert!(rates[1] > rates[0] && rates[3] > rates[2] && rates[4] < rates[3] && rates[5] < rates[4] && rates[6] == rates[5] && rates[7] > rates[6], "{rates:?}");
}

/// Every path reads the fuel rate of the state the transaction runs on (§5.6), to the drop. A parent
/// block that fills fuel (not yet executed) raises the rate its child is charged at above the
/// executed head's: a call at the head's rate is admitted by the mempool, yet a block at the child's
/// height holds it to the child's. Calls priced exactly at the child's rate, and one drop under it,
/// are judged alike by:
/// - admission at the head (the mempool: a call a drop under the head's rate is refused, and one at
///   it admitted), and again after the parent executes (the mempool's re-check, at the new state);
/// - the proposer's trial (`App::propose`, on the parent's replayed state);
/// - verification (`App::verify`, on the same);
/// - execution (`Chain::execute`: a call under the rate is skipped and never charged, one at it
///   charged exactly).
#[test]
fn every_path_reads_the_fuel_rate_of_the_state_the_tx_runs_on() {
    let mut d = Dev::new("fuel-paths");
    let p = d.deploy(1).remove(0);
    let (alice, bob, carol) = (keys(ALICE), keys(BOB), keys(CAROL));
    // The validator's own account is a fourth sender: its key is the one in its home.
    let key_seed: [u8; 32] = hex::decode(std::fs::read_to_string(d.home.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
    let validator = Keys::from_seed(key_seed);
    assert_eq!(Some(validator.address_with(&d.prefix())), d.c.me());
    let (limit, floor) = (param(&d.c.state, Param::MaxBlockFuel), param(&d.c.state, Param::FuelFloorRate));
    // Eight full blocks of Carol's lift the rate off the floor.
    let full = 4 * reserved_by(9_900_000);
    let mut model = floor;
    for _ in 0..8 {
        let txs = full_block(&d, &carol, &p);
        d.block(txs);
        model = model_rate(model, full, limit, floor);
    }
    let head_rate = stored(&d);
    assert_eq!(head_rate, model);
    assert!(head_rate > 2 * floor, "{head_rate}");
    // The parent block, one more of Carol's (counting 99% of fuel), is ordered but not executed.
    let parent_txs = full_block(&d, &carol, &p);
    let child_rate = model_rate(head_rate, full, limit, floor);
    assert!(fuel_at(child_rate, reserved_by(5_000_000)) > fuel_at(head_rate, reserved_by(5_000_000)) + 1, "the child's rate is well above the head's");
    // The calls under test, 5M fuel each, priced by the spec.
    let (head, child) = (d.next(), d.next() + 1);
    let [a_nonce, b_nonce, v_nonce] = [&alice, &bob, &validator].map(|k| d.c.state.account(&k.address_with(&d.prefix())).nonce);
    let action = call_of(&d, &p, "ret", 5_000_000);
    let size = tx_paying(&d, &alice, a_nonce, 0, 0, action.clone()).size();
    let reserved = reserved_by(5_000_000);
    let (bytes_at_head, bytes_at_child) = (econ::base_fee(&d.c.state, head, size), econ::base_fee(&d.c.state, child, size));
    assert_eq!(bytes_at_head, bytes_at_child, "the byte fee sits at its floor: only the fuel rate differs");
    let at_head = bytes_at_head + fuel_at(head_rate, reserved);
    let at_child = bytes_at_child + fuel_at(child_rate, reserved);
    // Alice pays exactly the child's rate, Bob a drop under it, the validator's account exactly
    // the head's.
    let exact = tx_paying(&d, &alice, a_nonce, at_child, 0, action.clone());
    let short = tx_paying(&d, &bob, b_nonce, at_child - 1, 0, action.clone());
    let stale = tx_paying(&d, &validator, v_nonce, at_head, 0, action.clone());

    // Admission at the head: a drop under its rate is refused, at it admitted; the child's price
    // is more than enough.
    let under = tx_paying(&d, &validator, v_nonce, at_head - 1, 0, action.clone());
    let e = d.c.submit(under).unwrap_err().to_string();
    assert!(e.contains(&format!("fee too low: this tx costs {at_head} drops at block {head} (base fee {at_head} + tip 0)")), "{e}");
    for t in [&stale, &exact, &short] {
        d.c.submit(t.clone()).unwrap();
    }

    let r = Round2::new(&mut d, &parent_txs);
    // The proposer's trial runs on the parent's replayed state, at the child's rate: Alice's call
    // goes in; Bob's, a drop short, and the validator's, priced for the head, are left out.
    let proposed = r.propose().expect("a proposal");
    assert_eq!(proposed.txs, vec![exact.to_bytes()]);
    // Verification, on the same state: the same three verdicts, one by one.
    assert_eq!(r.verify(proposed.clone()), Some(true));
    assert_eq!(r.verify(r.block(std::slice::from_ref(&exact))), Some(true));
    for t in [&short, &stale] {
        assert_eq!(r.verify(r.block(std::slice::from_ref(t))), Some(false), "{}", t.id());
    }
    // Execution. The parent first: the mempool's re-check, at the state it leaves, keeps Alice's
    // call and drops the other two, saying what the call costs there.
    let child_block = r.block(&[exact.clone(), short.clone()]);
    let mut c = r.chain.lock();
    c.execute(finalized_from(&r.parent, None)).unwrap();
    assert_eq!((stored_of(&c), rate(&c.state)), (child_rate, child_rate), "the parent counted {full} fuel");
    assert!(c.in_mempool(&exact.id()));
    for t in [&short, &stale] {
        assert!(!c.in_mempool(&t.id()), "{}", t.id());
        let why = &c.failure(&t.id()).expect("it says why").0;
        assert!(why.starts_with(&format!("dropped from the mempool: fee too low: this tx costs {at_child} drops at block {child}")), "{why}");
    }
    // Then the child, carrying Alice's call and Bob's: Alice is charged exactly the child's rate,
    // Bob's call is skipped, and Bob charged nothing.
    let (alice_before, bob_before, bob_nonce_before) = (c.state.account(&exact.from).balance, c.state.account(&short.from).balance, c.state.account(&short.from).nonce);
    let (burned, proposer) = (c.state.burned, c.state.account(&c.me().unwrap()).balance);
    c.execute(finalized_from(&child_block, None)).unwrap();
    let b = c.last_block().unwrap().clone();
    assert_eq!(b.txs.iter().map(Tx::id).collect::<Vec<_>>(), vec![exact.id()], "Bob's call isn't in the block");
    assert!(b.failed.is_empty());
    assert_eq!(alice_before - c.state.account(&exact.from).balance, at_child, "charged exactly the child's rate");
    assert_eq!(b.programs[&0].fee, FeeParts { bytes: bytes_at_child, fuel: fuel_at(child_rate, reserved), tip: 0 });
    assert_eq!((c.state.account(&short.from).balance, c.state.account(&short.from).nonce), (bob_before, bob_nonce_before), "a call a drop short is never charged");
    // What the skipped call does cost: the proposer pays its bytes' base fee as junk (never fuel: it
    // never ran), burned beside Alice's charge.
    assert_eq!(proposer - c.state.account(&c.me().unwrap()).balance, bytes_at_child, "the proposer's junk charge");
    assert_eq!(c.state.burned - burned, at_child + bytes_at_child, "Alice's charge and the junk fee are burned");
    drop(c);
    r.give_back(&mut d);
}

/// The stored rate of a chain (the fuel rate in META).
fn stored_of(c: &crate::chain::Chain) -> u64 {
    c.state.programs.meta.as_ref().unwrap().fuel_rate
}
