//! The node-local safeguards end to end (docs/PROGRAMS-V0.md §6.2, §6.3, §7.1; WBS S4c's
//! acceptance), on a dev chain with programs on, through `App::propose` and `App::verify` and
//! `Chain::execute`: faults are injected through `ProgramView`'s test hook (a panic inside the VM,
//! or in `upload::check`) or a refused spawn, for one transaction by its id.
//!
//! Every test that makes a path count a fault holds `COUNTING` (S4b's lock, `fault/tests.rs`) while
//! it does, so the exact tallies those tests read stay theirs.

use super::super::probe::*;
use super::super::view::inject::{self, Injected};
use super::marker::{records_in, MARKER_FILE};
use super::totals::{self, drift};
use super::tests::{halt_of, moved, tally, Armed, COUNTING, PANIC};
use super::*;
use crate::consensus::finalized_from;
use crate::crypto::Keys;
use crate::types::Action;
use crate::chain::State;
use commonware_cryptography::Digestible as _;
use crate::upgrade::{EXIT_PROGRAM_FAULT, EXIT_PROGRAM_RESOURCES};

/// A transfer of 1 FANG from `k` to Bob.
fn pay(d: &Dev, k: &Keys) -> Tx {
    d.tx(k, Action::Transfer { to: d.address(BOB), amount: FANG })
}

/// A call of `ret` on `p` from `k`.
fn call_ret(d: &Dev, k: &Keys, p: &str) -> Tx {
    d.tx(k, calls(vec![d.call(p, "ret", vec![], 0)], vec![]))
}

/// `Some(why)` if the chain's mempool door refuses `tx`, by HTTP's path (`submit`) and by gossip's
/// (`track_pending`) alike; `None` if both take it. (A tx both take is queued.)
fn refused(c: &mut Chain, tx: &Tx) -> Option<String> {
    let (http, gossip) = (c.submit(tx.clone()).err().map(|e| format!("{e:#}")), c.track_pending(tx.clone()).err().map(|e| format!("{e:#}")));
    match (http, gossip) {
        (Some(a), Some(b)) => {
            assert_eq!(a, b, "the door says the same on both paths");
            Some(a)
        }
        (None, None) => None,
        (a, b) => panic!("the paths differ: HTTP {a:?}, gossip {b:?}"),
    }
}

const QUARANTINED: &str = "quarantined on this node after a program execution fault, until block";

/// What the door says to a sender held after two uncertified proposals, until block `until`
/// (S4C-R3): not a word about a fault.
fn struck_text(until: u64) -> String {
    format!("held on this node until block {until}: two of this node's proposals carrying this sender's program transactions weren't certified; other nodes accept them")
}

/// Empty blocks until `d`'s chain has executed `height`: for the tests that need a chain past a
/// height at which a hold counted from block 1, or an epoch counted from view 0, would differ.
fn run_to(d: &mut Dev, height: u64) {
    while d.c.height() < height {
        d.run(vec![]).unwrap();
    }
}

/// What the door is asked about once a proposal has quarantined something: each built on the
/// chain's own state before it went to the proposal (the [`Round2`] takes the chain over).
struct Asked {
    /// Alice's other call, at the nonce the faulting one held.
    other_call: Tx,
    /// Carol's create of the stored probe code.
    create: Tx,
    /// Bob's upload of the faulting upload's bytes.
    same_bytes: Tx,
    /// Bob's upload of other bytes (the probe, stored already: the door doesn't care).
    other_bytes: Tx,
    /// Alice's and Carol's transfers, at the nonces their dropped transactions held, and Bob's
    /// call to the program the faulting call declared.
    alice_pays: Tx,
    carol_pays: Tx,
    bob_calls: Tx,
}

/// A proposal in which Alice's call and Carol's upload of new code both fault in their trials
/// (while Bob's payment is packed), on a node that deployed one probe program.
struct Faulted {
    d: Dev,
    r: Round2,
    call: Tx,
    upload: Tx,
    asked: Asked,
}

fn faulted_proposal(tag: &str) -> Faulted {
    let mut d = Dev::new(tag);
    let (alice, bob, carol) = (keys(ALICE), keys(BOB), keys(CAROL));
    let p = d.deploy(1).remove(0);
    let call = call_ret(&d, &alice, &p);
    let upload = d.tx(&carol, upload(FLOAT_WASM, &probe_interface()));
    let pay = pay(&d, &bob);
    let nonce = |d: &Dev, k: &Keys| d.c.state.account(&k.address_with(&d.prefix())).nonce;
    let asked = Asked {
        other_call: d.tx(&alice, calls(vec![d.call(&p, "no", vec![], 0)], vec![])),
        create: d.tx(&carol, create_probe()),
        same_bytes: d.txs_at(&bob, Some(nonce(&d, &bob) + 1), 0, vec![super::super::probe::upload(FLOAT_WASM, &probe_interface())]).remove(0),
        other_bytes: d.txs_at(&bob, Some(nonce(&d, &bob) + 2), 0, vec![super::super::probe::upload(PROBE_WASM, &probe_interface())]).remove(0),
        alice_pays: pay_to(&d, &alice, BOB),
        carol_pays: pay_to(&d, &carol, BOB),
        bob_calls: d.txs_at(&bob, Some(nonce(&d, &bob) + 1), 0, vec![calls(vec![d.call(&p, "ret", vec![], 0)], vec![])]).remove(0),
    };
    for t in [&call, &upload, &pay] {
        d.c.submit(t.clone()).unwrap();
    }
    let r = Round2::new(&mut d, &[]);
    let _counting = COUNTING.lock();
    let counted = tally();
    let (_a, _b) = (Armed::new(&call, Injected::Panic), Armed::new(&upload, Injected::Panic));
    let block = r.propose().expect("a proposal");
    assert_eq!(block.txs, vec![pay.to_bytes()], "both faulting transactions are left out, the payment packed");
    assert_eq!(moved(&counted), [(Path::Propose, FaultClass::Tx, 2)]);
    Faulted { d, r, call, upload, asked }
}

/// 1 FANG from `k` to the account of `key_seed`.
fn pay_to(d: &Dev, k: &Keys, key_seed: u8) -> Tx {
    d.tx(k, Action::Transfer { to: d.address(key_seed), amount: FANG })
}

/// A `Tx` fault in a proposer's trial quarantines the transaction's id, holds its sender's program
/// transactions and, for an upload, its code (§6.2); the mempool's door refuses exactly those, on
/// HTTP's path and gossip's, and nothing else: not the sender's transfer, not another account's
/// call to the same program (a call's declared code is never held), not an upload of other bytes.
#[test]
fn a_tx_fault_quarantines_the_id_the_sender_and_an_uploads_bytes_and_the_door_refuses_exactly_those() {
    let Faulted { mut d, r, call, upload: up, asked, .. } = faulted_proposal("door");
    let mut c = r.chain.lock();
    assert_eq!(c.quarantine.lens(), [2, 2, 1], "two ids, two senders, one upload's code");
    // Each held until its own end, from the height the proposal was for: the id until the block
    // after its last valid one, the sender 600 blocks on, the code a day (86,400 blocks of 1 s).
    let at = r.e + 2;
    assert_eq!(c.quarantine.held_until(&Record::of(&call), 1), [Some(call.valid_until + 1), Some(at + 600), None]);
    assert_eq!(c.quarantine.held_until(&Record::of(&up), 1), [Some(up.valid_until + 1), Some(at + 600), Some(at + 86_400)]);
    for (t, what) in [(&call, PANIC), (&up, "a panic on the program thread")] {
        assert!(!c.in_mempool(&t.id()));
        assert_eq!(c.failure(&t.id()).unwrap().0, format!("not proposed: program execution faulted on this node ({what})"));
    }
    // The ids; the senders' other program transactions (Alice's call at the nonce the dropped one
    // held, Carol's create); the upload's bytes, from anyone.
    for t in [&call, &up, &asked.other_call, &asked.create, &asked.same_bytes] {
        assert!(refused(&mut c, t).is_some_and(|m| m.contains(QUARANTINED)), "{}: {:?}", t.action.kind(), refused(&mut c, t));
    }
    // What isn't held: the senders' transfers (so each can replace the nonce its dropped
    // transaction held), another account's call to the program the faulting call declared, and an
    // upload of other bytes.
    for (what, ok) in [("alice's transfer", &asked.alice_pays), ("carol's transfer", &asked.carol_pays), ("bob's call", &asked.bob_calls), ("other bytes", &asked.other_bytes)] {
        assert_eq!(refused(&mut c, ok), None, "{what}");
    }
    drop(c);
    r.give_back(&mut d);
}

/// Verification abstains on a block carrying a quarantined transaction id (§6.2), before replaying
/// anything, whoever else the block carries, and judges a block carrying only the held sender's
/// transfer as usual. The same block gets its vote before the fault.
#[test]
fn verify_abstains_on_a_quarantined_id_and_verifies_the_held_senders_transfer() {
    let mut d = Dev::new("verify-quarantine");
    let (alice, bob) = (keys(ALICE), keys(BOB));
    let p = d.deploy(1).remove(0);
    // Alice's call and her transfer, at the same nonce: the transfer is what replaces the call.
    let (call, transfer, bobs) = (call_ret(&d, &alice, &p), pay(&d, &alice), call_ret(&d, &bob, &p));
    d.c.submit(call.clone()).unwrap();
    let r = Round2::new(&mut d, &[]);
    assert_eq!(r.verify(r.block(std::slice::from_ref(&call))), Some(true), "before the fault: a vote");
    {
        let _counting = COUNTING.lock();
        let _armed = Armed::new(&call, Injected::Panic);
        assert!(r.propose().expect("a proposal").txs.is_empty(), "the call faults in its trial");
    }
    assert_eq!(r.chain.lock().quarantine.lens(), [1, 1, 0]);
    assert_eq!(r.verify(r.block(std::slice::from_ref(&call))), None, "no vote on a block carrying the quarantined id");
    assert_eq!(r.verify(r.block(&[bobs.clone(), call])), None, "whoever else it carries");
    // Alice is held for program transactions, and her transfer is judged as usual: a vote. So is a
    // block with only another account's call.
    assert_eq!(r.verify(r.block(&[transfer])), Some(true));
    assert_eq!(r.verify(r.block(&[bobs])), Some(true));
    r.give_back(&mut d);
}

/// A `Setup` fault quarantines nothing (§6.2): it's this node's trouble, not the transaction's.
#[test]
fn a_setup_fault_quarantines_nothing() {
    let mut d = Dev::new("setup-quarantine");
    let p = d.deploy(1).remove(0);
    let call = call_ret(&d, &keys(ALICE), &p);
    d.c.submit(call.clone()).unwrap();
    let r = Round2::new(&mut d, &[]);
    {
        let _counting = COUNTING.lock();
        let counted = tally();
        let _armed = Armed::new(&call, Injected::RefuseSpawns(3));
        assert!(r.propose().expect("a proposal").txs.is_empty());
        assert_eq!(moved(&counted), [(Path::Propose, FaultClass::Setup, 1)]);
    }
    let c = r.chain.lock();
    assert!(c.quarantine.is_empty() && c.in_mempool(&call.id()));
    drop(c);
    r.give_back(&mut d);
}

/// Entries expire as the chain moves (`apply_block`): an id is held until the block after its
/// last valid one has gone by. (The sender's 600 blocks and an upload's day are
/// `fault/quarantine/tests.rs`'s.)
#[test]
fn executing_blocks_expires_the_quarantine() {
    let mut d = Dev::new("expiry");
    let at = d.next();
    let doomed = Record { id: "ab".repeat(32), sender: d.address(ALICE), kind: "call_program".into(), valid_until: at + 1, wasm_hash: None };
    d.c.quarantine.hold(&doomed, at, 1_000_000);
    assert_eq!(d.c.quarantine.lens(), [1, 1, 0]);
    d.run(vec![]).unwrap();
    assert_eq!(d.c.quarantine.lens(), [1, 1, 0], "block {at}: the id can still be carried at {}", at + 1);
    d.run(vec![]).unwrap();
    assert_eq!(d.c.quarantine.lens(), [0, 1, 0], "block {}: no block can carry it now; the sender is held for 600", at + 1);
}

/// The trial marker names the program transaction under trial while it runs, in a proposal and in
/// a verification, and is empty when each ends (§6.2). (A native transaction isn't named.)
#[test]
fn the_marker_names_the_tx_under_trial_while_it_runs_and_is_empty_after() {
    let mut d = Dev::new("marker-flow");
    let p = d.deploy(1).remove(0);
    let (call, pay) = (call_ret(&d, &keys(ALICE), &p), pay(&d, &keys(CAROL)));
    for t in [&call, &pay] {
        d.c.submit(t.clone()).unwrap();
    }
    let path = d.home.join(MARKER_FILE);
    let r = Round2::new(&mut d, &[]);
    let empty = |path: &std::path::Path| records_in(&std::fs::read(path).unwrap());
    assert!(!path.exists(), "nothing written before the first program transaction is tried");
    inject::look(&call.id(), path.clone());
    let block = r.propose().expect("a proposal");
    assert_eq!(block.txs.len(), 2);
    let seen = inject::looked(&call.id()).expect("the call's view was made");
    assert_eq!(records_in(&seen), vec![Record::of(&call)], "the file named the call, and only it, while its trial ran");
    assert_eq!(empty(&path), vec![], "and nothing once packing ended");
    inject::look(&call.id(), path.clone());
    assert_eq!(r.verify(block), Some(true));
    assert_eq!(records_in(&inject::looked(&call.id()).expect("the call's view was made")), vec![Record::of(&call)], "verification names it too");
    assert_eq!(empty(&path), vec![]);
    r.give_back(&mut d);
}

/// Two program transactions in one block: each trial's marker, in the proposal and in the
/// verification, names its own transaction, never the one before it (the proposer and the verifier
/// hash each id once and hand it down with its transaction; stage N's review, N-R1).
#[test]
fn two_program_txs_in_one_block_are_each_named_by_their_own_id() {
    let mut d = Dev::new("two-program-txs");
    let p = d.deploy(1).remove(0);
    let (call1, call2) = (call_ret(&d, &keys(ALICE), &p), call_ret(&d, &keys(BOB), &p));
    for t in [&call1, &call2] {
        d.c.submit(t.clone()).unwrap();
    }
    let path = d.home.join(MARKER_FILE);
    let r = Round2::new(&mut d, &[]);
    inject::look(&call1.id(), path.clone());
    inject::look(&call2.id(), path.clone());
    let block = r.propose().expect("a proposal");
    assert_eq!(block.txs.len(), 2);
    assert_eq!(records_in(&inject::looked(&call1.id()).expect("view 1")), vec![Record::of(&call1)], "first trial");
    assert_eq!(records_in(&inject::looked(&call2.id()).expect("view 2")), vec![Record::of(&call2)], "second trial");
    inject::look(&call1.id(), path.clone());
    inject::look(&call2.id(), path.clone());
    assert_eq!(r.verify(block), Some(true));
    assert_eq!(records_in(&inject::looked(&call1.id()).expect("view 1")), vec![Record::of(&call1)], "first verification");
    assert_eq!(records_in(&inject::looked(&call2.id()).expect("view 2")), vec![Record::of(&call2)], "second verification");
    r.give_back(&mut d);
}

/// A marker a process that died left behind fills the quarantine at startup under the rule a fault
/// gets (§6.2), once: the file is removed, and the next restart quarantines nothing. The chain is
/// past block 600, so holds counted from the wrong height (block 1, say) differ from the right
/// ones: the id is held until the block after its `valid_until`, the sender 600 blocks and an
/// upload's code a day from the block after the executed tip.
#[test]
fn a_marker_left_by_a_dead_process_fills_the_quarantine_at_startup() {
    let mut d = Dev::new("marker-startup");
    run_to(&mut d, 650);
    let (alice, bob, carol) = (keys(ALICE), keys(BOB), keys(CAROL));
    let p = d.deploy(1).remove(0);
    let (call, up) = (call_ret(&d, &bob, &p), d.tx(&carol, upload(FLOAT_WASM, &probe_interface())));
    // The process was in the middle of two trials when it died.
    let marker = d.c.trial.clone();
    let _died: Vec<_> = [&call, &up]
        .into_iter()
        .map(|t| {
            let mut trial = marker.begin();
            trial.mark(t);
            std::mem::ManuallyDrop::new(trial)
        })
        .collect();
    assert_eq!(records_in(&std::fs::read(d.home.join(MARKER_FILE)).unwrap()).len(), 2);
    d.reopen().unwrap();
    assert!(!d.home.join(MARKER_FILE).exists(), "read once");
    assert_eq!(d.c.quarantine.lens(), [2, 2, 1]);
    let at = d.next();
    assert!(at > 650, "the tip is {}", d.c.height());
    assert_eq!(d.c.quarantine.held_until(&Record::of(&call), at), [Some(call.valid_until + 1), Some(at + 600), None]);
    assert_eq!(d.c.quarantine.held_until(&Record::of(&up), at), [Some(up.valid_until + 1), Some(at + 600), Some(at + 86_400)]);
    for t in [&call, &up] {
        assert!(refused(&mut d.c, t).is_some_and(|m| m.contains(QUARANTINED)));
    }
    // Bob's other program transaction is held (his call was named), Alice's transfer is not.
    let bobs_other = d.tx(&bob, calls(vec![d.call(&p, "no", vec![], 0)], vec![]));
    assert!(refused(&mut d.c, &bobs_other).is_some_and(|m| m.contains(&format!("{QUARANTINED} {}:", at + 600))), "{:?}", refused(&mut d.c, &bobs_other));
    let alice_pays = pay(&d, &alice);
    assert_eq!(refused(&mut d.c, &alice_pays), None, "a sender the marker didn't name is free");
    d.reopen().unwrap();
    assert!(d.c.quarantine.is_empty(), "a restart empties the quarantine, and the marker is gone");
}

/// A proposal tries at most [`MAX_FAULTED_TRIALS`] faulting program transactions, then stops trying
/// program transactions (S4B-R4): a faulted trial is rolled back, so the block's fuel budget doesn't
/// count it, and the packing holds the chain lock. Nine funded senders each have a call that
/// faults; the first proposal tries and quarantines eight (the highest tips), packs Carol's payment
/// all the same, and leaves the ninth queued, untried and not quarantined; the next one tries it.
#[test]
fn a_proposal_tries_at_most_eight_faulting_program_txs_and_then_stops_trying_program_txs() {
    assert_eq!(MAX_FAULTED_TRIALS, 8);
    let mut d = Dev::new("cap");
    let (alice, carol) = (keys(ALICE), keys(CAROL));
    let p = d.deploy(1).remove(0);
    let senders: Vec<Keys> = (90..99).map(keys).collect();
    let funding = d.txs(&alice, senders.iter().map(|k| Action::Transfer { to: k.address_with(&d.prefix()), amount: 20 * FANG }).collect());
    d.block(funding);
    // Each sender's call tips a little more than the one before it, so they're tried in the
    // order of `senders` reversed: the first sender's is the ninth.
    let size = call_ret(&d, &senders[0], &p).size();
    let base = crate::econ::base_fee(&d.c.state, d.next() + 1, size);
    let faulting: Vec<Tx> = senders.iter().enumerate().map(|(i, k)| d.txs_at(k, None, base * (i as u64 + 1) / 2, vec![calls(vec![d.call(&p, "ret", vec![], 0)], vec![])]).remove(0)).collect();
    let pays = pay(&d, &carol);
    for t in faulting.iter().chain([&pays]) {
        d.c.submit(t.clone()).unwrap();
    }
    let r = Round2::new(&mut d, &[]);
    let _counting = COUNTING.lock();
    let counted = tally();
    let _armed: Vec<Armed> = faulting.iter().map(|t| Armed::new(t, Injected::Panic)).collect();
    let block = r.propose().expect("a proposal");
    assert_eq!(block.txs, vec![pays.to_bytes()], "the payment is packed; no program transaction is, after eight faults");
    assert_eq!(moved(&counted), [(Path::Propose, FaultClass::Tx, 8)], "eight trials, not nine");
    let ninth = &faulting[0];
    {
        let c = r.chain.lock();
        assert!(c.in_mempool(&ninth.id()) && c.failure(&ninth.id()).is_none(), "the ninth waits, untouched");
        assert_eq!(c.quarantine.lens(), [8, 8, 0]);
        assert!(faulting[1..].iter().all(|t| !c.in_mempool(&t.id())), "the eight went");
    }
    // The next proposal tries the ninth.
    let block = r.propose().expect("a proposal");
    assert_eq!(block.txs, vec![pays.to_bytes()]);
    assert_eq!(moved(&counted), [(Path::Propose, FaultClass::Tx, 9)]);
    assert!(!r.chain.lock().in_mempool(&ninth.id()));
    r.give_back(&mut d);
}

/// Serializes the tests that read the strikes metric (process-wide, and `Strikes::strike` bumps it
/// in `quarantine/tests.rs` too).
use super::quarantine::STRIKE_METRIC;

/// The chain executes `r`'s parent and then an empty block in `view`, whose parent was proposed in
/// the parent's view: every view between was nullified.
fn skip_to(c: &mut Chain, r: &Round2, view: u64) {
    c.execute(finalized_from(&r.parent, None)).unwrap();
    let mut f = c.test_block(vec![]);
    (f.view, f.parent_view) = (view, r.parent.view);
    c.execute(f).unwrap();
}

/// Strikes (§6.2) through the proposer and `Chain::apply_block`. A proposal's program transactions
/// are remembered by its view; a finalized block whose `parent_view < v < view` for that view
/// strikes each of them (a native transaction isn't one); one strike changes nothing, two drop the
/// transaction from the mempool and hold its sender's program transactions, and the metric counts
/// each. A proposal whose block was finalized takes no strike. The chain is past block 600 (and in
/// its second epoch; the views are above its heights), so a hold counted from the wrong height, or
/// a record kept under the wrong epoch, shows: the sender's hold ends 600 blocks after the block that struck, and it is refused
/// in its own words (S4C-R3), not as a fault's.
#[test]
fn a_nullified_view_strikes_its_proposals_program_txs_two_drop_one_and_hold_its_sender() {
    let mut d = Dev::new("strikes");
    run_to(&mut d, 650);
    let (alice, bob, carol) = (keys(ALICE), keys(BOB), keys(CAROL));
    let p = d.deploy(1).remove(0);
    let certified = call_ret(&d, &bob, &p);
    d.c.submit(certified.clone()).unwrap();
    let _metric = STRIKE_METRIC.lock();
    let before = quarantine::UNCERTIFIED.load(std::sync::atomic::Ordering::Relaxed);
    let uncertified = || quarantine::UNCERTIFIED.load(std::sync::atomic::Ordering::Relaxed) - before;

    // A proposal whose block is finalized is not a nullified view: no strike, and it's settled.
    let r = Round2::new(&mut d, &[]);
    let block = r.propose_in(1_050).expect("a proposal");
    assert_eq!(block.txs, vec![certified.to_bytes()]);
    assert_eq!(r.chain.lock().strikes.pending_views(), 1);
    {
        let mut c = r.chain.lock();
        c.execute(finalized_from(&r.parent, None)).unwrap();
        c.execute(finalized_from(&block, None)).unwrap();
        assert_eq!((c.strikes.pending_views(), c.strikes.strikes_of(&certified.id())), (0, 0));
    }
    r.give_back(&mut d);

    // A proposal in a view the chain skipped, whose transaction the finalized block after the gap
    // includes (another leader's block carried it): it isn't waiting any more, so there's nothing
    // to give up on, and nothing is struck.
    let included = call_ret(&d, &bob, &p);
    d.c.submit(included.clone()).unwrap();
    let r = Round2::new(&mut d, &[]);
    assert_eq!(r.propose_in(1_080).expect("a proposal").txs, vec![included.to_bytes()]);
    {
        let mut c = r.chain.lock();
        c.execute(finalized_from(&r.parent, None)).unwrap();
        let mut f = c.test_block(vec![included.clone()]);
        (f.view, f.parent_view) = (1_090, r.parent.view);
        c.execute(f).unwrap();
        assert_eq!((c.strikes.pending_views(), c.strikes.strikes_of(&included.id())), (0, 0), "included by the block after the gap: no strike");
        assert!(c.quarantine.is_empty());
    }
    r.give_back(&mut d);
    assert_eq!(uncertified(), 0);

    // Alice's call, and Carol's payment (never struck: it isn't a program transaction).
    let call = call_ret(&d, &alice, &p);
    let payment = pay(&d, &carol);
    for t in [&call, &payment] {
        d.c.submit(t.clone()).unwrap();
    }
    // First proposal, in view 1,100: the chain goes on to a block of view 1,110 without it.
    let r = Round2::new(&mut d, &[]);
    assert_eq!(r.propose_in(1_100).expect("a proposal").txs.len(), 2);
    {
        let mut c = r.chain.lock();
        assert_eq!(c.strikes.pending_views(), 1, "the program tx is remembered, the payment isn't");
        skip_to(&mut c, &r, 1_110);
        assert_eq!((c.strikes.strikes_of(&call.id()), c.strikes.strikes_of(&payment.id())), (1, 0));
        assert!(c.in_mempool(&call.id()) && c.in_mempool(&payment.id()), "one strike drops nothing");
        assert!(c.quarantine.is_empty(), "and holds no one");
        assert_eq!(uncertified(), 1);
    }
    r.give_back(&mut d);
    // Second proposal, in view 1,200, skipped too: two strikes.
    let r = Round2::new(&mut d, &[]);
    assert_eq!(r.propose_in(1_200).expect("a proposal").txs.len(), 2);
    {
        let mut c = r.chain.lock();
        skip_to(&mut c, &r, 1_210);
        assert_eq!(uncertified(), 2);
        assert!(!c.in_mempool(&call.id()), "dropped at two strikes");
        assert_eq!(c.failure(&call.id()).unwrap().0, "not certified in two proposals: other validators may be faulting on it");
        assert!(c.in_mempool(&payment.id()), "the payment isn't");
        assert_eq!(c.quarantine.lens(), [0, 1, 0], "its sender's program transactions are held, nothing else");
        // Held for 600 blocks from the block after the one that struck, which is past block 600.
        let next = c.height() + 1;
        assert!(next > 650);
        assert_eq!(c.quarantine.held_until(&Record::of(&call), next), [None, Some(next + 600), None]);
    }
    // The sender's program transactions are refused at the door, in the strikes' own words (no
    // fault happened, and other nodes accept them); her transfer isn't.
    r.give_back(&mut d);
    let (other_call, alice_pays) = (call_ret(&d, &alice, &p), pay(&d, &alice));
    let until = d.next() + 600;
    assert_eq!(refused(&mut d.c, &other_call), Some(struck_text(until)));
    assert!(!struck_text(until).contains("fault") && !refused(&mut d.c, &other_call).unwrap().contains(QUARANTINED));
    assert_eq!(refused(&mut d.c, &alice_pays), None);
}

/// A proposal recorded in an earlier epoch whose view never settled is no strike when a finalized
/// block of a later epoch has `parent_view < v < view` by the numbers (S4C-R8): views restart at
/// each epoch, so the record is keyed by both. Epochs are six blocks here (block 6 is the first of
/// the second); Alice's call is proposed in view 50 of the first, and the block at height 6 comes
/// in view 60 on a parent of view 5.
#[test]
fn a_proposal_of_an_earlier_epoch_is_not_struck_by_a_block_of_a_later_one() {
    let mut d = Dev::with("strikes-epoch", true, Some(6));
    let p = d.deploy(1).remove(0);
    let call = call_ret(&d, &keys(ALICE), &p);
    d.c.submit(call.clone()).unwrap();
    let _metric = STRIKE_METRIC.lock();
    let before = quarantine::UNCERTIFIED.load(std::sync::atomic::Ordering::Relaxed);
    let r = Round2::new(&mut d, &[]);
    assert_eq!(r.e + 2, 4, "the proposal is for block 4, in the first epoch");
    assert_eq!(r.propose_in(50).expect("a proposal").txs, vec![call.to_bytes()]);
    {
        let mut c = r.chain.lock();
        assert_eq!(c.state.params.epoch_of(4), 0);
        assert_eq!(c.strikes.pending_views(), 1);
        c.execute(finalized_from(&r.parent, None)).unwrap();
        // Blocks 4 and 5 finish the first epoch, in views of their own height: the record of view
        // 50 is neither settled nor struck.
        for _ in 0..2 {
            let f = c.test_block(vec![]);
            c.execute(f).unwrap();
        }
        assert_eq!((c.height(), c.strikes.pending_views()), (5, 1));
        // Block 6, the first of the second epoch, in view 60 on a parent of view 5.
        let mut f = c.test_block(vec![]);
        assert_eq!(c.state.params.epoch_of(f.height), 1);
        (f.view, f.parent_view) = (60, 5);
        c.execute(f).unwrap();
        assert_eq!(c.strikes.strikes_of(&call.id()), 0, "view 50 of the first epoch is no skipped view of the second");
        assert_eq!(c.strikes.pending_views(), 0, "but it is settled: its epoch is over");
        assert!(c.in_mempool(&call.id()) && c.quarantine.is_empty());
    }
    assert_eq!(quarantine::UNCERTIFIED.load(std::sync::atomic::Ordering::Relaxed) - before, 0);
    r.give_back(&mut d);
}

/// A proposal that is never made strikes nothing (S4C-R4): at a block that must certify a checkpoint
/// (block 104 certifies 100) a proposer that hasn't executed the checkpoint waits for it and gives
/// up after three block times, and returns no block. Its program transactions were carried by a
/// proposal that doesn't exist, so a skipped view can't strike them: nothing is remembered. A
/// proposal at a height that needn't certify one is made and remembered, on the same unexecuted
/// ancestors. (The chain has executed block 99; the parent is block 103 or 102, over empty blocks.)
#[test]
fn a_proposal_the_checkpoint_wait_gives_up_is_never_remembered_for_strikes() {
    assert_eq!((crate::snapshot::certifies(104), crate::snapshot::certifies(103)), (Some(100), None));
    let mut d = Dev::new("checkpoint-wait");
    let p = d.deploy(1).remove(0);
    run_to(&mut d, 99);
    let call = call_ret(&d, &keys(BOB), &p);
    d.c.submit(call.clone()).unwrap();
    // Blocks 100 to 103 and 100 to 102 (newest first), over the executed tip, before the round
    // takes the chain over.
    let (to_104, to_103) = (unexecuted_chain(&d, &[vec![], vec![], vec![], vec![]], 0), unexecuted_chain(&d, &[vec![], vec![], vec![]], 0));
    let r = Round2::new(&mut d, &[]);
    let _metric = STRIKE_METRIC.lock();
    assert_eq!(r.propose_on(to_104, 200), None, "block 104 needs the state root of block 100, which this node hasn't executed");
    assert_eq!(r.chain.lock().strikes.pending_views(), 0, "no proposal, nothing to strike");
    assert!(r.chain.lock().trial.in_flight().is_empty(), "and nothing in flight in the marker");
    let block = r.propose_on(to_103, 300).expect("block 103 certifies no checkpoint: a proposal");
    assert_eq!(block.txs, vec![call.to_bytes()]);
    assert_eq!(r.chain.lock().strikes.pending_views(), 1, "a proposal made is remembered");
    r.give_back(&mut d);
}

/// The marker is empty while a verification that abstained waits (S4C-R4), not only once it is
/// dropped: simplex keeps a pending verification until its view times out, and a crash in that
/// window must not quarantine the last program transaction it tried. Both ways a verification can
/// abstain after its loop has started trying them: a fault met by the last transaction (the loop
/// ended), and a later transaction that can't be admitted after an earlier one's fault (the loop
/// stopped at it). The file names the call while its trial runs.
#[test]
fn a_verification_that_abstains_leaves_the_marker_empty_while_it_waits() {
    let mut d = Dev::new("verify-parked");
    let (alice, bob) = (keys(ALICE), keys(BOB));
    let p = d.deploy(1).remove(0);
    let call = call_ret(&d, &bob, &p);
    // A transfer of Alice's at a nonce she hasn't reached: unadmittable on any state.
    let too_far = d.txs_at(&alice, Some(d.c.state.account(&d.address(ALICE)).nonce + 5), 0, vec![Action::Transfer { to: d.address(CAROL), amount: FANG }]).remove(0);
    let r = Round2::new(&mut d, &[]);
    let path = d.home.join(MARKER_FILE);
    let parked = || (records_in(&std::fs::read(&path).unwrap_or_default()), r.chain.lock().trial.in_flight());
    let _counting = COUNTING.lock();
    let _armed = Armed::new(&call, Injected::Panic);
    for (what, txs) in [("the loop ended", vec![call.clone()]), ("the loop stopped at an unadmittable transaction", vec![call.clone(), too_far])] {
        inject::look(&call.id(), path.clone());
        let (verdict, waiting) = r.verify_parked(r.block(&txs), parked);
        assert_eq!(verdict, None, "{what}: the verifier abstained");
        assert_eq!(records_in(&inject::looked(&call.id()).expect("the call's view was made")), vec![Record::of(&call)], "{what}: the file named the call while its trial ran");
        assert_eq!(waiting, Some((vec![], vec![])), "{what}: nothing is named while the verification waits");
    }
    r.give_back(&mut d);
}

/// A chain without programs touches none of the safeguards (§6.5): no marker file is written, the
/// quarantine and the strikes stay empty, no digest is recorded and the replay cache is never
/// used, through a proposal, a vote and the execution of the block.
#[test]
fn a_chain_without_programs_touches_none_of_the_safeguards() {
    let mut d = Dev::dormant("inert");
    let pay_tx = pay(&d, &keys(ALICE));
    d.c.submit(pay_tx.clone()).unwrap();
    let home = d.home.clone();
    let r = Round2::new(&mut d, &[]);
    let block = r.propose().expect("a proposal");
    assert_eq!(block.txs, vec![pay_tx.to_bytes()]);
    assert_eq!(r.verify(block.clone()), Some(true));
    {
        let mut c = r.chain.lock();
        c.execute(finalized_from(&r.parent, None)).unwrap();
        c.execute(finalized_from(&block, None)).unwrap();
        assert!(c.quarantine.is_empty() && c.verified_clean.is_empty());
        assert_eq!(c.strikes.pending_views(), 0);
    }
    assert!(!home.join(MARKER_FILE).exists(), "the marker is never written");
    assert!(r.cache.heights().is_empty(), "and the replay cache never used");
    r.give_back(&mut d);
}

/// A tx fault at execution only, on a block this node's verification passed, halts with exit 14
/// ("this node verified the block without a fault, then faulted executing it": a restart most
/// likely executes it); after the restart the set is gone and the same fault exits 13 (§6.2).
#[test]
fn a_tx_fault_at_execution_on_a_block_verify_passed_halts_14_and_after_a_restart_13() {
    let mut d = Dev::new("verified-clean");
    let p = d.deploy(1).remove(0);
    let call = call_ret(&d, &keys(BOB), &p);
    let r = Round2::new(&mut d, &[]);
    let block = r.block(std::slice::from_ref(&call));
    assert_eq!(r.verify(block.clone()), Some(true), "no fault while verifying");
    assert!(r.chain.lock().verified_clean.contains(&hex::encode(block.digest().0)));
    let _counting = COUNTING.lock();
    let counted = tally();
    {
        let mut c = r.chain.lock();
        c.execute(finalized_from(&r.parent, None)).unwrap();
        let _armed = Armed::new(&call, Injected::Panic);
        let halt = halt_of(&c.execute(finalized_from(&block, None)).unwrap_err());
        assert!(matches!(&halt, Halt::ProgramResources { height, fault } if *height == block.height && fault.contains(&format!("this node verified block {} without a fault", block.height)) && fault.contains("restarting once")), "{halt:?}");
        assert_eq!(halt.exit_code(), EXIT_PROGRAM_RESOURCES);
        assert_eq!(c.halt.signal.get(), Some(halt), "what node::watch_halts exits with");
    }
    assert_eq!(moved(&counted), [(Path::Execute, FaultClass::Tx, 1)], "still a Tx fault, counted as one");
    r.give_back(&mut d);
    // The restart: nothing is remembered, and the same fault exits 13.
    d.reopen().unwrap();
    assert!(d.c.verified_clean.is_empty());
    let _armed = Armed::new(&call, Injected::Panic);
    let halt = halt_of(&d.c.execute(finalized_from(&block, None)).unwrap_err());
    assert_eq!(halt, Halt::ProgramFault { height: block.height, tx: call.id(), fault: PANIC.into() });
    assert_eq!(halt.exit_code(), EXIT_PROGRAM_FAULT);
    assert_eq!(moved(&counted), [(Path::Execute, FaultClass::Tx, 2)]);
}

/// `verified` records a digest only for a block that carries a program transaction and only while
/// META exists in the state it was verified on; and a chain without programs records none
/// (§6.2: v1 nodes run today's code).
#[test]
fn a_digest_is_recorded_only_while_programs_exist_and_for_a_block_with_a_program_tx() {
    let mut d = Dev::new("verified-record");
    let p = d.deploy(1).remove(0);
    let (call, payment) = (call_ret(&d, &keys(BOB), &p), pay(&d, &keys(CAROL)));
    let r = Round2::new(&mut d, &[]);
    let state = r.chain.lock().state.clone();
    let mut without_meta = state.clone();
    without_meta.programs.meta = None;
    let digest = |n: u8| hex::encode([n; 32]);
    verified(&r.chain, &without_meta, std::slice::from_ref(&call), &digest(1));
    verified(&r.chain, &state, std::slice::from_ref(&payment), &digest(2));
    assert!(r.chain.lock().verified_clean.is_empty(), "no META, or no program transaction: nothing recorded");
    verified(&r.chain, &state, &[payment, call], &digest(3));
    let c = r.chain.lock();
    assert!(c.verified_clean.contains(&digest(3)) && c.verified_clean.len() == 1);
    drop(c);
    r.give_back(&mut d);
    // A chain without programs, end to end: the block verifies and records nothing.
    let mut v1 = Dev::dormant("verified-v1");
    let pay_tx = pay(&v1, &keys(ALICE));
    let r = Round2::new(&mut v1, &[]);
    assert_eq!(r.verify(r.block(&[pay_tx])), Some(true));
    assert!(r.chain.lock().verified_clean.is_empty(), "no digest on a chain without META");
    r.give_back(&mut v1);
}

/// `check_totals` flags a corrupted `value_held`, `sponsored` and `escrow`, whichever side of the
/// comparison is wrong (META's total, or what it's the sum of), and nothing in a sound state or
/// one without programs; and it sets (and clears) `blockula_program_accounting_drift`.
#[test]
fn check_totals_flags_a_corrupted_value_held_sponsored_and_escrow() {
    let mut d = Dev::new("totals");
    let programs = d.deploy(2);
    // Some FANG in a program, so `value_held` isn't zero: the probe's payable `fund`.
    let fund = d.tx(&keys(ALICE), calls(vec![d.call(&programs[0], "fund", vec![], 5 * FANG)], vec![]));
    let b = d.block(vec![fund]);
    assert!(b.failed.is_empty(), "{:?}", b.failed);
    let sound = d.c.state.clone();
    let meta = |s: &State| s.programs.meta.clone().unwrap();
    assert_eq!((meta(&sound).value_held, meta(&sound).sponsored > 0), (5 * FANG, false));
    assert!(meta(&sound).escrow > 0, "the creates' record deposits and the upload's blobs");
    assert_eq!(drift(&sound), vec![]);

    let program = &programs[0];
    type Corrupt = Box<dyn Fn(&mut State)>;
    let cases: Vec<(&str, Corrupt, &str)> = vec![
        ("META's value_held", Box::new(|s| s.programs.meta.as_mut().unwrap().value_held += 1), "value_held"),
        ("a program account's balance", Box::new({
            let program = program.clone();
            move |s| s.accounts.get_mut(&program).unwrap().balance += 1
        }), "value_held"),
        ("META's sponsored", Box::new(|s| s.programs.meta.as_mut().unwrap().sponsored += 1), "sponsored"),
        ("a record's sponsored", Box::new({
            let program = program.clone();
            move |s| s.programs.records.get_mut(&program).unwrap().sponsored += 1
        }), "sponsored"),
        ("META's escrow", Box::new(|s| s.programs.meta.as_mut().unwrap().escrow += 1), "escrow"),
        ("a record's deposit", Box::new({
            let program = program.clone();
            move |s| s.programs.records.get_mut(&program).unwrap().deposit += 1
        }), "escrow"),
        ("a record's record_deposit", Box::new({
            let program = program.clone();
            move |s| s.programs.records.get_mut(&program).unwrap().record_deposit += 1
        }), "escrow"),
        ("a blob's deposit", Box::new(|s| {
            let key = s.programs.blob_refs.keys().next().unwrap().clone();
            s.programs.blob_refs.get_mut(&key).unwrap().deposit += 1
        }), "escrow"),
    ];
    for (what, corrupt, total) in &cases {
        let mut s = sound.clone();
        corrupt(&mut s);
        let found = drift(&s);
        assert_eq!(found.iter().map(|d| d.total).collect::<Vec<_>>(), [*total], "{what}: {found:?}");
        assert_ne!(found[0].recorded, found[0].found);
    }
    // All three at once.
    let mut s = sound.clone();
    for (_, corrupt, _) in cases.iter().filter(|(w, ..)| matches!(*w, "META's value_held" | "META's sponsored" | "META's escrow")) {
        corrupt(&mut s);
    }
    assert_eq!(drift(&s).iter().map(|d| d.total).collect::<Vec<_>>(), ["value_held", "sponsored", "escrow"]);

    // The alarm: set by a drift, cleared by a sound check, untouched by a state without programs.
    let _gauge = DRIFT_LOCK.lock();
    assert!(check_totals(&sound) && !accounting_drift());
    cases[0].1(&mut s);
    assert!(!check_totals(&s) && accounting_drift());
    assert!(metrics().contains("blockula_program_accounting_drift 1\n"));
    assert!(check_totals(&State::default()) && accounting_drift(), "no programs: true, and the alarm isn't touched");
    assert!(check_totals(&sound) && !accounting_drift());
    assert!(metrics().contains("blockula_program_accounting_drift 0\n"));
}

/// Serializes the tests that set or read the accounting alarm (process-wide).
static DRIFT_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// `Chain::apply_block` runs the check at each epoch boundary and only there (§7.1). Epochs are
/// four blocks here, so blocks 3 and 7 end one; the alarm is set by hand (a corrupted state would
/// trip `debug_check` in a test build), and a sound check at the boundary clears it.
#[test]
fn the_totals_are_checked_at_each_epoch_boundary_and_only_there() {
    let mut d = Dev::with("totals-boundary", true, Some(4));
    d.deploy(1);
    assert_eq!(d.c.height(), 2);
    let _gauge = DRIFT_LOCK.lock();
    totals::DRIFT.store(true, std::sync::atomic::Ordering::Relaxed);
    d.run(vec![]).unwrap();
    assert!(!accounting_drift(), "block 3 ends the first epoch: checked, sound, cleared");
    totals::DRIFT.store(true, std::sync::atomic::Ordering::Relaxed);
    for height in 4..7 {
        d.run(vec![]).unwrap();
        assert!(accounting_drift(), "block {height} is no boundary: unchecked");
    }
    d.run(vec![]).unwrap();
    assert!(!accounting_drift(), "block 7 ends the second");
}

/// `/metrics` carries the safeguards' series once programs exist (`programs::api::metrics` prints
/// `fault::metrics`).
#[test]
fn the_safeguards_have_their_series_in_metrics() {
    let text = metrics();
    for series in ["blockula_program_quarantined{kind=\"tx\"} ", "blockula_program_quarantined{kind=\"sender\"} ", "blockula_program_quarantined{kind=\"code\"} ", "blockula_program_uncertified_proposals_total ", "blockula_program_accounting_drift ", "blockula_replay_cache_hits_total "] {
        assert!(text.contains(series), "{series}: {text}");
    }
    let d = Dev::new("series");
    let served = super::super::api::metrics(&d.c.view());
    assert!(served.contains("blockula_program_quarantined{kind=\"tx\"}") && served.contains("blockula_replay_cache_hits_total"));
}
