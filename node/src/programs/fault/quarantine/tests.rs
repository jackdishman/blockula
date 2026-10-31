//! The quarantine, strikes and the verified-clean set on their own (docs/PROGRAMS-V0.md §6.2);
//! what feeds them, and what they do to a chain, is `fault/safeguards.rs`.

use crate::programs::probe::{bare_call, bare_tx, bare_upload};
use super::*;
use crate::types::Action;

const DAY: u64 = 86_400;

fn record(id: &str, sender: &str, valid_until: u64, wasm_hash: Option<[u8; 32]>) -> Record {
    Record { id: id.repeat(64), sender: sender.into(), kind: "call_program".into(), valid_until, wasm_hash }
}

/// `Held` keeps at most its cap, oldest out first, and a key held again keeps the later end and
/// counts as the newest.
#[test]
fn held_keys_are_bounded_oldest_out_and_keep_their_later_end() {
    let mut h = Held::new(3);
    for (k, until) in [("a", 10), ("b", 10), ("c", 10)] {
        h.hold(k, until);
    }
    h.hold("a", 5);
    assert_eq!(h.until("a", 0), Some(10), "the later end stays");
    h.hold("a", 20);
    assert_eq!(h.until("a", 0), Some(20), "and a later one replaces it");
    h.hold("d", 10);
    assert_eq!((h.until("b", 0), h.until("c", 0), h.until("a", 0), h.until("d", 0)), (None, Some(10), Some(20), Some(10)), "b was the oldest (a was held again, so it counts as new)");
    assert_eq!((h.until("c", 9), h.until("c", 10)), (Some(10), None), "held while next < until");
    h.expire(10);
    assert_eq!(h.entries.iter().map(|(k, _)| *k).collect::<Vec<_>>(), ["a"]);
}

/// A `Tx` fault holds what its author controls and nothing else, each until its own end: the id
/// until the height after its `valid_until`, the sender (for program transactions only) for
/// `QUARANTINE_HOLD_BLOCKS`, an upload's code for a day. `refuses` is the mempool's door.
#[test]
fn a_fault_holds_the_id_the_sender_and_an_uploads_code_each_until_its_own_end() {
    let mut q = Quarantine::default();
    assert!(q.is_empty());
    let faulting = bare_tx("boo1alice", 7, 1_050, bare_upload(b"held bytes"));
    let code = digest::wasm_hash(b"held bytes");
    q.hold(&Record::of(&faulting), 1_000, DAY);
    assert_eq!(q.lens(), [1, 1, 1]);
    let (id, other) = (faulting.id(), bare_tx("boo1alice", 7, 1_050, bare_call()));
    let refused = |q: &Quarantine, tx: &Tx, next: u64| q.refuses(tx, &tx.id(), next).err().map(|e| e.to_string());
    // The id: until the block after its last valid one.
    assert!(refused(&q, &faulting, 1_050).is_some_and(|m| m.contains("until block 1051")), "{:?}", refused(&q, &faulting, 1_050));
    assert_eq!(q.holds_id(|i| i == id, 1_050), Some(&id));
    assert_eq!(q.holds_id(|i| i == id, 1_051), None, "no block can carry it any more");
    assert!(refused(&q, &faulting, 1_051).is_some(), "its sender and its code are still held");
    // The sender's program transactions, until 1,000 + QUARANTINE_HOLD_BLOCKS; its transfer never.
    let transfer = bare_tx("boo1alice", 8, 1_050, Action::Transfer { to: "boo1bob".into(), amount: 1 });
    assert_eq!(QUARANTINE_HOLD_BLOCKS, 600);
    assert!(refused(&q, &other, 1_599).is_some_and(|m| m.contains("until block 1600")));
    assert_eq!(refused(&q, &other, 1_600), None, "the sender is free after the hold");
    assert_eq!(refused(&q, &transfer, 1_001), None, "a transfer is never held");
    // Other senders' calls, and their uploads of other bytes.
    assert_eq!(refused(&q, &bare_tx("boo1bob", 7, 1_050, bare_call()), 1_001), None);
    assert_eq!(refused(&q, &bare_tx("boo1bob", 0, 1_050, bare_upload(b"other bytes")), 1_001), None);
    // The code, from anyone, for a day.
    let same_bytes = bare_tx("boo1bob", 0, 1_050, bare_upload(b"held bytes"));
    assert!(refused(&q, &same_bytes, 1_000 + DAY - 1).is_some());
    assert_eq!(refused(&q, &same_bytes, 1_000 + DAY), None);
    assert_eq!(q.codes.until(&code, 1_001), Some(1_000 + DAY));
}

const FAULT: &str = "quarantined on this node after a program execution fault, until block";
const STRUCK: &str = "held on this node until block";

/// What the door says to `tx` (a program transaction of Alice's, or a transfer) at block `next`.
fn said(q: &Quarantine, tx: &Tx, next: u64) -> Option<String> {
    q.refuses(tx, &tx.id(), next).err().map(|e| e.to_string())
}

/// A sender held after two uncertified proposals (S4C-R3) is refused in its own words, not the
/// fault quarantine's: nothing faulted, and other nodes accept the transaction. A fault's hold
/// keeps its text; a sender held for both is told about the one that lasts longer, the fault's if
/// they end together; a transfer is never refused.
#[test]
fn a_sender_held_after_strikes_is_refused_in_its_own_words() {
    let call = bare_tx("boo1alice", 7, 1_050, bare_call());
    let transfer = bare_tx("boo1alice", 8, 1_050, Action::Transfer { to: "boo1bob".into(), amount: 1 });
    let mut q = Quarantine::default();
    q.hold_sender("boo1alice", 1_001);
    assert_eq!(q.lens(), [0, 1, 0]);
    assert_eq!(said(&q, &call, 1_002).as_deref(), Some("held on this node until block 1601: two of this node's proposals carrying this sender's program transactions weren't certified; other nodes accept them"));
    assert_eq!(q.held_until(&Record::of(&call), 1_002), [None, Some(1_601), None], "600 blocks from the height given");
    assert_eq!(said(&q, &call, 1_601), None, "free after the hold");
    assert_eq!(said(&q, &transfer, 1_002), None, "a transfer is never held");
    assert_eq!(said(&q, &bare_tx("boo1bob", 7, 1_050, bare_call()), 1_002), None, "nor is another sender's call");

    // A fault in the same sender's transaction, which holds it 100 blocks longer: its own text, at
    // the later end. The strike hold's end is behind it. A second entry is kept for each reason.
    let faulting = bare_tx("boo1alice", 9, 1_050, bare_call());
    q.hold(&Record::of(&faulting), 1_101, DAY);
    assert_eq!(q.lens(), [1, 2, 0]);
    assert_eq!(said(&q, &call, 1_002).map(|m| m.contains(FAULT) && m.ends_with("until block 1701: see its log")), Some(true), "{:?}", said(&q, &call, 1_002));
    assert_eq!(said(&q, &faulting, 1_002).map(|m| m.starts_with(FAULT)), Some(true), "the id is the fault's too");
    assert_eq!(q.held_until(&Record::of(&call), 1_002)[1], Some(1_701));
    // Once the fault's end is the earlier one, the strikes' text is what the sender reads.
    let mut q = Quarantine::default();
    q.hold(&Record::of(&faulting), 1_001, DAY);
    q.hold_sender("boo1alice", 1_101);
    assert!(said(&q, &call, 1_002).is_some_and(|m| m.starts_with(STRUCK) && m.contains("until block 1701:")));
    assert!(said(&q, &call, 1_601).is_some_and(|m| m.starts_with(STRUCK)), "the fault's 600 blocks have gone, the strikes' haven't");
    // Ending together: the fault's text.
    let mut q = Quarantine::default();
    q.hold(&Record::of(&faulting), 1_001, DAY);
    q.hold_sender("boo1alice", 1_001);
    assert!(said(&q, &call, 1_002).is_some_and(|m| m.starts_with(FAULT)));
    // Expiry lets go of each on its own.
    q.expire(1_601);
    assert!(q.is_empty(), "{:?}", q.lens());
}

/// `holds_id` asks the block about each quarantined id (oldest first) and answers with the first
/// the block carries; an id whose end has gone, or that the block doesn't carry, is no reason.
#[test]
fn holds_id_names_the_first_quarantined_id_the_block_carries() {
    let mut q = Quarantine::default();
    assert_eq!(q.holds_id(|_| true, 1), None, "nothing held");
    for (id, valid_until) in [("a", 100), ("b", 200), ("c", 300)] {
        q.hold(&record(id, "boo1alice", valid_until, None), 10, DAY);
    }
    let id = |c: &str| c.repeat(64);
    let carries = |ids: &'static [&'static str]| move |i: &str| ids.iter().any(|c| i == id(c));
    assert_eq!(q.holds_id(carries(&["c", "b"]), 50), Some(&id("b")), "the older one, whichever order the block has them in");
    assert_eq!(q.holds_id(carries(&["c"]), 50), Some(&id("c")));
    assert_eq!(q.holds_id(carries(&["d", "e"]), 50), None, "a block carrying only other ids");
    assert_eq!(q.holds_id(carries(&["a", "b"]), 101), Some(&id("b")), "a's valid_until has passed: no block can carry it any more");
    assert_eq!(q.holds_id(carries(&["a"]), 101), None);
    assert_eq!(q.holds_id(|_| true, 301), None, "all expired");
}

/// A call's declared code is never quarantined: a `Record` of a call holds no code, and the code
/// hash set stays empty.
#[test]
fn a_call_holds_no_code() {
    let mut q = Quarantine::default();
    q.hold(&Record::of(&bare_tx("boo1alice", 0, 100, bare_call())), 10, DAY);
    assert_eq!(q.lens(), [1, 1, 0]);
}

/// Entries go once they no longer hold, and the quarantine is empty again.
#[test]
fn entries_expire() {
    let mut q = Quarantine::default();
    q.hold(&record("a", "boo1alice", 100, Some([1; 32])), 10, DAY);
    q.expire(101);
    assert_eq!(q.lens(), [0, 1, 1], "the id is past its valid_until; the sender's 600 blocks and the code's day aren't");
    q.expire(10 + 600);
    assert_eq!(q.lens(), [0, 0, 1]);
    q.expire(10 + DAY);
    assert!(q.is_empty());
}

/// The holds are bounded: past 1,024 ids, 1,024 senders and 256 code hashes the oldest go.
#[test]
fn the_quarantine_is_bounded() {
    let mut q = Quarantine::default();
    for i in 0..(MAX_QUARANTINED_IDS as u64 + 10) {
        q.hold(&record("a", &format!("boo1sender{i}"), 100, Some(blake3::hash(&i.to_le_bytes()).into())), 10, DAY);
    }
    // The same id every time: one id; the senders and codes are what grow.
    assert_eq!(q.lens(), [1, MAX_QUARANTINED_SENDERS, MAX_QUARANTINED_CODES]);
    let mut q = Quarantine::default();
    for i in 0..(MAX_QUARANTINED_IDS as u64 + 10) {
        q.hold(&record(&format!("{:x}", i % 16), "boo1alice", i, None), 10, DAY);
    }
    assert_eq!(q.lens()[0], 16, "sixteen distinct ids");
}

/// `Strikes`: a proposal's program transactions are remembered by view; a finalized block settles
/// every view up to its own, and strikes what was carried strictly between its parent's view and
/// its own.
#[test]
fn a_nullified_view_strikes_what_its_proposal_carried() {
    let carried = |id: &str| Carried { id: id.repeat(64), sender: "boo1alice".into(), valid_until: 100 };
    let mut s = Strikes::default();
    s.record(0, 7, vec![carried("a")]);
    s.record(0, 9, vec![carried("b"), carried("c")]);
    s.record(0, 12, vec![carried("d")]);
    s.record(0, 15, vec![]);
    assert_eq!(s.pending_views(), 3, "a proposal with no program transaction records nothing");
    // Parent proposed in 6, this block in 11: 7 and 9 were skipped; 12 is still to come.
    assert_eq!(s.nullified(0, 6, 11).iter().map(|c| &c.id[..1]).collect::<Vec<_>>(), ["a", "b", "c"]);
    assert_eq!(s.pending_views(), 1);
    // The block at view 12 is a proposal of this node's own, certified: no strike.
    assert!(s.nullified(0, 11, 12).is_empty());
    assert_eq!(s.pending_views(), 0);
    // A view at or below the parent's is settled without a strike (its block was an earlier one's).
    s.record(0, 20, vec![carried("e")]);
    assert!(s.nullified(0, 20, 25).is_empty() && s.pending_views() == 0);
    s.record(0, 20, vec![carried("e")]);
    assert!(s.nullified(0, 21, 25).is_empty() && s.pending_views() == 0);
}

/// Views restart at each epoch, so a record is keyed by its epoch too (S4C-R8): a record of an
/// earlier epoch is settled without a strike by a block of a later one, whatever its view; a record
/// of a later epoch than the block waits; and the bound on remembered proposals drops the earliest
/// epoch's first, whatever its views.
#[test]
fn strike_records_are_keyed_by_epoch_and_view() {
    let carried = |id: &str| Carried { id: id.repeat(64), sender: "boo1alice".into(), valid_until: 100 };
    let ids = |v: Vec<Carried>| v.iter().map(|c| c.id[..1].to_string()).collect::<Vec<_>>();
    // A proposal of epoch 0 in view 9 never settled (the epoch ended first). The first block of
    // epoch 1 (parent view 0) is at view 12: epoch 1's views 1 to 11 were skipped, and view 9 of
    // epoch 0 is not one of them.
    let mut s = Strikes::default();
    s.record(0, 9, vec![carried("a")]);
    s.record(1, 5, vec![carried("b")]);
    s.record(2, 3, vec![carried("z")]);
    assert_eq!(ids(s.nullified(1, 0, 12)), ["b"], "the earlier epoch's record is no strike");
    assert_eq!(s.pending_views(), 1, "and it is settled; the later epoch's waits");
    assert_eq!(ids(s.nullified(2, 0, 4)), ["z"]);
    // The same view in another epoch is another record: neither replaces the other.
    s.record(3, 7, vec![carried("c")]);
    s.record(4, 7, vec![carried("d")]);
    assert_eq!(s.pending_views(), 2);
    assert_eq!(ids(s.nullified(4, 0, 8)), ["d"]);
    assert_eq!(s.pending_views(), 0, "epoch 3's view 7 is of an epoch that is over");
    // The bound drops the earliest epoch's records first, not the lowest views.
    let mut s = Strikes::default();
    s.record(0, 90, vec![carried("a")]);
    for view in 1..=MAX_STRIKE_VIEWS as u64 {
        s.record(1, view, vec![carried("b")]);
    }
    assert_eq!(s.pending_views(), MAX_STRIKE_VIEWS);
    assert_eq!(s.proposed.keys().next(), Some(&(1, 1)), "epoch 0's view 90 went, not epoch 1's view 1");
    assert!(!s.proposed.contains_key(&(0, 90)));
}

/// Two strikes end a transaction's turn; one doesn't. The count stays until the transaction can no
/// longer be carried, and is bounded.
#[test]
fn strikes_count_per_transaction_and_expire_with_its_validity() {
    let _metric = STRIKE_METRIC.lock();
    let tx = Carried { id: "a".repeat(64), sender: "boo1alice".into(), valid_until: 100 };
    let mut s = Strikes::default();
    assert_eq!(s.strike(&tx), 1);
    assert!(s.strike(&tx) >= STRIKES_TO_DROP);
    assert_eq!(s.strikes_of(&tx.id), 2);
    s.expire(100);
    assert_eq!(s.strikes_of(&tx.id), 2, "it can still be carried at block 100");
    s.expire(101);
    assert_eq!(s.strikes_of(&tx.id), 0);
    for i in 0..(MAX_STRUCK_IDS + 5) {
        s.strike(&Carried { id: format!("{i:064}"), sender: "boo1alice".into(), valid_until: 100 });
    }
    assert_eq!(s.struck.len(), MAX_STRUCK_IDS);
    // Remembered proposals are bounded too.
    for view in 0..(MAX_STRIKE_VIEWS as u64 + 5) {
        s.record(0, view, vec![tx.clone()]);
    }
    assert_eq!(s.pending_views(), MAX_STRIKE_VIEWS);
    assert_eq!(s.proposed.keys().next(), Some(&(0, 5)), "the oldest views went");
}

/// The last 64 digests, each once.
#[test]
fn verified_clean_remembers_the_last_64_digests() {
    let mut v = VerifiedClean::default();
    assert!(v.is_empty());
    for i in 0..70 {
        v.record(&format!("{i:064x}"));
    }
    v.record(&format!("{:064x}", 69));
    assert_eq!(v.len(), MAX_VERIFIED_CLEAN);
    assert!(!v.contains(&format!("{:064x}", 5)) && v.contains(&format!("{:064x}", 6)) && v.contains(&format!("{:064x}", 69)));
}
