//! Blackjack rules v2 (`guests/bj2-core`, docs/proposals/blackjack-dealer-outage.md), built for
//! the host:
//!
//! * **The cards are v1's.** 10,000 random hands played three ways: through `bj_core::Hand`,
//!   through a v2 relayed hand (dealt as v1) and through a v2 hand the dealer opens before the
//!   seed. Every card, total, outcome and payout must agree: v2 changes what a stall pays and
//!   nothing about a hand that is played out.
//! * **An exhaustive walk** of every reachable v2 state, with every card and both player moves:
//!   the record's bounds hold, a hand being opened has no cards, and wherever the dealer owes a
//!   card it could know, the forfeit pays **exactly** the most the hand could still have paid
//!   (an oracle that searches every continuation). Never less: a dealer gains nothing by holding
//!   back a card. Never more: v1 paid 3:2 in states where no natural was possible, and the walk
//!   counts them.
//! * **A hand that was never opened is refunded**, whatever was revealed of the opening.
//! * **Payouts**: conservation and solvency for 20,000 stakes against `u128` arithmetic, a stake
//!   cut in pieces never pays more, the reserve is exactly the dearest ending, and sums that
//!   don't fit a `u64` panic instead of wrapping.
//! * **The vectors.** `services/blackjack/core/test/vectors2.json` holds what this file
//!   generated; the TypeScript rules are held to them. After changing the rules, regenerate with
//!   `cargo test --release --test blackjack2_rules write -- --ignored`.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use bj2_core::{CHAIN_LEN, Hand, MAX_DEALER_CARDS, MAX_PLAYER_CARDS, OPENING_LINKS, Outcome, Phase, RULES, Step, card, exposure, natural, payout, points, prev_link, total};

const FANG: u64 = 1_000_000_000;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A deterministic stream of bytes: BLAKE3 in counter mode over a label.
struct Rng {
    label: Vec<u8>,
    n: u64,
}

impl Rng {
    fn new(label: &str) -> Rng {
        Rng { label: label.as_bytes().to_vec(), n: 0 }
    }

    fn bytes32(&mut self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(&self.label).update(&self.n.to_le_bytes());
        self.n += 1;
        *h.finalize().as_bytes()
    }

    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.bytes32()[..8].try_into().unwrap())
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.u64() % 100 < percent
    }
}

/// A chain from its top link `s_N`: `links[i] = s_i`, so `links[0]` is the commitment.
fn chain(top: [u8; 32]) -> Vec<[u8; 32]> {
    let mut links = vec![top];
    for _ in 0..CHAIN_LEN {
        links.push(*blake3::hash(links.last().unwrap()).as_bytes());
    }
    links.reverse();
    links
}

// ---------------------------------------------------------------------------------- numbers

#[test]
fn the_numbers_are_v1s_and_two_more() {
    assert_eq!(RULES, 2);
    assert_eq!(OPENING_LINKS, 3);
    for n in 1..=8u8 {
        assert_eq!(Outcome::from_u8(n).unwrap() as u8, bj_core::Outcome::from_u8(n).unwrap() as u8);
        assert_eq!(format!("{:?}", Outcome::from_u8(n).unwrap()), format!("{:?}", bj_core::Outcome::from_u8(n).unwrap()));
    }
    assert_eq!(Outcome::from_u8(9), Some(Outcome::Refund));
    assert_eq!(Outcome::from_u8(0), None);
    assert_eq!(Outcome::from_u8(10), None);
    for n in 0..=5u8 {
        assert_eq!(Phase::from_u8(n).unwrap() as u8, bj_core::Phase::from_u8(n).unwrap() as u8);
        assert_eq!(format!("{:?}", Phase::from_u8(n).unwrap()), format!("{:?}", bj_core::Phase::from_u8(n).unwrap()));
        // The dealer owes a link in the same phases; a hand waiting for its seed isn't seeded.
        assert_eq!(Phase::from_u8(n).unwrap().dealers(), bj_core::Phase::from_u8(n).unwrap().dealers());
        assert_eq!(Phase::from_u8(n).unwrap().seeded(), n != 0);
    }
    assert_eq!(Phase::from_u8(6), Some(Phase::Open));
    assert!(Phase::Open.dealers() && !Phase::Open.seeded());
    assert_eq!(Phase::from_u8(7), None);
    // Only a forfeit the house paid for trips the breaker.
    for n in 1..=9u8 {
        assert_eq!(Outcome::from_u8(n).unwrap().dealers_fault(), n == 6 || n == 7, "outcome {n}");
    }
}

// ---------------------------------------------------------------------------------- the cards are v1's

/// A hand played to its end.
#[derive(Debug, PartialEq, Eq, Clone)]
struct Played {
    cards: Vec<u8>,
    player: Vec<u8>,
    dealer: Vec<u8>,
    outcome: u8,
    payout: u64,
    moves: String,
}

fn play_v1(links: &[[u8; 32]], seed: &[u8; 32], id: u64, mut wants: impl FnMut(&[u8], u8) -> bool) -> Played {
    use bj_core::{Hand, Phase, Step};
    let mut hand = Hand::new();
    let (mut cards, mut moves) = (vec![], String::new());
    let outcome = loop {
        if hand.phase == Phase::Player {
            if wants(hand.player_cards(), hand.dealer[0]) {
                hand.hit();
                moves.push('H');
            } else {
                hand.stand();
                moves.push('S');
            }
        }
        let j = hand.dealt + 1;
        let c = bj_core::card(&links[j as usize], seed, id, j);
        cards.push(c);
        if let Some(Step::Settled(o)) = hand.deal(c) {
            break o;
        }
    };
    Played { cards, player: hand.player_cards().to_vec(), dealer: hand.dealer_cards().to_vec(), outcome: outcome as u8, payout: bj_core::payout(FANG, outcome), moves }
}

/// What a missed deadline would end the hand as at each point the dealer owed a link: entry `k`
/// is the outcome had it stopped with `k` links revealed.
type Stalls = Vec<u8>;

/// A v2 hand: `opened` is a bet from an account (the dealer reveals three links blind, the seed
/// deals them), otherwise a relayed hand (dealt as v1). Every link is checked against the one
/// before it, as the guest does.
fn play_v2(links: &[[u8; 32]], seed: &[u8; 32], id: u64, opened: bool, mut wants: impl FnMut(&[u8], u8) -> bool) -> (Played, Stalls) {
    let mut hand = if opened { Hand::committed() } else { Hand::new() };
    let (mut cards, mut moves, mut stalls) = (vec![], String::new(), vec![]);
    let mut last = links[0];
    let reveal = |hand: &Hand, last: &mut [u8; 32]| {
        let j = hand.dealt + 1;
        assert_eq!(prev_link(&links[j as usize]), *last, "link {j}");
        *last = links[j as usize];
        j
    };
    let mut settled = None;
    if opened {
        assert!(!hand.hit() && !hand.stand(), "no move before the cards");
        assert_eq!(hand.deal(0), None, "an opening link deals no card");
        assert_eq!(hand.seeded([0, 1, 2]), None, "no seed before the hand is opened");
        for n in 1..=OPENING_LINKS {
            stalls.push(hand.forfeit().expect("the dealer owes the opening") as u8);
            reveal(&hand, &mut last);
            assert_eq!(hand.open_link(), Some(n == OPENING_LINKS));
            assert_eq!((hand.n_player, hand.n_dealer), (0, 0), "a hand being opened has no cards");
        }
        assert_eq!(hand.phase, Phase::Seed);
        assert_eq!(hand.forfeit(), None, "the player owes the seed, not the dealer a card");
        assert_eq!(hand.open_link(), None);
        assert_eq!(hand.deal(0), None);
        let first: [u8; 3] = core::array::from_fn(|i| card(&links[i + 1], seed, id, i as u8 + 1));
        cards.extend(first);
        if let Step::Settled(o) = hand.seeded(first).expect("the hand waited for its seed") {
            settled = Some(o);
        }
        assert_eq!(hand.seeded(first), None, "once");
    }
    let outcome = loop {
        if let Some(o) = settled {
            break o;
        }
        if hand.phase == Phase::Player {
            assert_eq!(hand.forfeit(), None, "the player's turn");
            if wants(hand.player_cards(), hand.dealer[0]) {
                assert!(hand.hit());
                moves.push('H');
            } else {
                assert!(hand.stand());
                moves.push('S');
            }
        }
        assert!(hand.phase.dealers() && hand.phase.seeded());
        stalls.push(hand.forfeit().expect("the dealer owes a card") as u8);
        let j = reveal(&hand, &mut last);
        let c = card(&links[j as usize], seed, id, j);
        cards.push(c);
        if let Step::Settled(o) = hand.deal(c).expect("the dealer owed a card") {
            settled = Some(o);
        }
    };
    assert_eq!(hand.phase, Phase::Done);
    assert_eq!((hand.forfeit(), hand.deal(0), hand.open_link()), (None, None, None), "a settled hand takes nothing");
    assert_eq!(hand.dealt as usize, cards.len());
    (Played { cards, player: hand.player_cards().to_vec(), dealer: hand.dealer_cards().to_vec(), outcome: outcome as u8, payout: payout(FANG, outcome), moves }, stalls)
}

fn policy(rng: &mut Rng) -> impl FnMut(&[u8], u8) -> bool + '_ {
    move |player, _up| {
        let t = total(player).0;
        rng.chance(if t < 12 { 95 } else if t < 17 { 60 } else { 15 })
    }
}

#[test]
fn ten_thousand_hands_play_out_as_under_v1_whichever_way_they_start() {
    let mut rng = Rng::new("blackjack2/property");
    let mut seen: BTreeMap<u8, u32> = BTreeMap::new();
    let mut settled_by_the_seed = 0;
    for n in 0..10_000u64 {
        let links = chain(rng.bytes32());
        let seed = rng.bytes32();
        let id = if n % 3 == 0 { rng.u64() } else { n };
        let label = format!("moves/{n}");
        let v1 = play_v1(&links, &seed, id, policy(&mut Rng::new(&label)));
        let (relayed, relayed_stalls) = play_v2(&links, &seed, id, false, policy(&mut Rng::new(&label)));
        let (opened, opened_stalls) = play_v2(&links, &seed, id, true, policy(&mut Rng::new(&label)));
        assert_eq!(relayed, v1, "hand {n}, dealt as v1");
        assert_eq!(opened, v1, "hand {n}, opened before its seed");
        assert!((1..=5).contains(&v1.outcome));
        *seen.entry(v1.outcome).or_default() += 1;
        settled_by_the_seed += u32::from(v1.cards.len() == 3);

        // One stall per link either way; an unopened hand is only ever refunded, and from the
        // seed on both kinds of hand forfeit alike.
        assert_eq!(relayed_stalls.len(), v1.cards.len());
        assert_eq!(opened_stalls.len(), v1.cards.len());
        assert_eq!(opened_stalls[..3], [Outcome::Refund as u8; 3]);
        assert_eq!(opened_stalls[3..], relayed_stalls[3..], "hand {n}");
        // A stall never pays more than v1's did, nor less than the hand ended up paying a player
        // who was owed a card at that point.
        for (k, o) in relayed_stalls.iter().enumerate() {
            let o = Outcome::from_u8(*o).unwrap();
            assert!(matches!(o, Outcome::Forfeit | Outcome::ForfeitDeal));
            assert!(payout(FANG, o) >= v1.payout, "hand {n}: a stall after {k} links pays less than the hand did");
        }
    }
    for outcome in 1..=5 {
        assert!(seen.get(&outcome).copied().unwrap_or(0) > 50, "outcome {outcome} barely happened: {seen:?}");
    }
    assert!(settled_by_the_seed > 100, "naturals against a small upcard settle in the seed's own transaction: {settled_by_the_seed}");
    println!("outcomes {seen:?}; {settled_by_the_seed} hands settled by the seed's reveal");
}

// ---------------------------------------------------------------------------------- the walk

/// What a hand's future depends on. Two hands with the same key play out alike, so the walk keeps
/// one hand per key and still meets every state.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Key {
    phase: u8,
    /// Opening links revealed, while there are no cards.
    opening: u8,
    n_player: u8,
    player_hard: u8,
    player_ace: bool,
    n_dealer: u8,
    dealer_hard: u8,
    dealer_ace: bool,
}

fn hard(cards: &[u8]) -> (u8, bool) {
    (cards.iter().map(|c| points(*c)).sum(), cards.iter().any(|c| c % 13 == 0))
}

fn key(h: &Hand) -> Key {
    let (player_hard, player_ace) = hard(h.player_cards());
    let (dealer_hard, dealer_ace) = hard(h.dealer_cards());
    let opening = if h.phase.seeded() { 0 } else { h.dealt };
    Key { phase: h.phase as u8, opening, n_player: h.n_player, player_hard, player_ace, n_dealer: h.n_dealer, dealer_hard, dealer_ace }
}

/// Every hand one step on from `h`: a link, a seed (every three cards), a card, or either move.
/// A step that settles the hand gives its outcome instead.
fn steps(h: &Hand) -> Vec<Result<Hand, Outcome>> {
    let after = |n: Hand, step: Step| match step {
        Step::Settled(o) => {
            assert_eq!(n.phase, Phase::Done);
            Err(o)
        }
        Step::More => {
            assert!(n.phase.dealers());
            Ok(n)
        }
        Step::Player => {
            assert_eq!(n.phase, Phase::Player);
            Ok(n)
        }
    };
    match h.phase {
        Phase::Open => {
            let mut n = *h;
            n.open_link().unwrap();
            vec![Ok(n)]
        }
        Phase::Seed => {
            let mut out = vec![];
            for a in 0..13 {
                for b in 0..13 {
                    for c in 0..13 {
                        let mut n = *h;
                        let step = n.seeded([a, b, c]).unwrap();
                        out.push(after(n, step));
                    }
                }
            }
            out
        }
        Phase::Player => {
            let (mut hit, mut stand) = (*h, *h);
            assert!(hit.hit() && stand.stand());
            vec![Ok(hit), Ok(stand)]
        }
        Phase::Deal | Phase::Hit | Phase::Dealer => (0..13)
            .map(|c| {
                let mut n = *h;
                let step = n.deal(c).unwrap();
                after(n, step)
            })
            .collect(),
        Phase::Done => unreachable!("a settled hand is never walked"),
    }
}

/// Every reachable state, one hand per key, from both starts.
fn walk() -> BTreeMap<Key, Hand> {
    let mut seen = BTreeMap::new();
    let mut queue = VecDeque::from([Hand::committed(), Hand::new()]);
    while let Some(h) = queue.pop_front() {
        if seen.contains_key(&key(&h)) {
            continue;
        }
        seen.insert(key(&h), h);
        for next in steps(&h).into_iter().flatten() {
            queue.push_back(next);
        }
    }
    seen
}

/// The most a hand could still pay a stake of 2 drops (so 3:2 is 5, a win 4, a push 2), whatever
/// cards come and however the player moves: a search of every continuation, sharing nothing with
/// `Hand::forfeit`.
fn most(h: &Hand, memo: &mut BTreeMap<Key, u64>) -> u64 {
    if let Some(m) = memo.get(&key(h)) {
        return *m;
    }
    let m = steps(h)
        .into_iter()
        .map(|next| match next {
            Ok(n) => most(&n, memo),
            Err(o) => payout(2, o),
        })
        .max()
        .unwrap();
    memo.insert(key(h), m);
    m
}

#[test]
fn every_reachable_state_forfeits_exactly_what_the_cards_still_allow() {
    let states = walk();
    let mut memo = BTreeMap::new();
    let mut per_phase: BTreeMap<u8, usize> = BTreeMap::new();
    let (mut dealer_owes, mut v1_paid_more, mut naturals) = (0, 0, 0);
    for (k, h) in &states {
        *per_phase.entry(k.phase).or_default() += 1;
        // The record's bounds.
        assert!(h.n_player as usize <= MAX_PLAYER_CARDS && h.n_dealer as usize <= MAX_DEALER_CARDS && h.dealt as usize <= CHAIN_LEN, "{k:?}");
        if h.phase.seeded() {
            assert_eq!(h.dealt, h.n_player + h.n_dealer, "every link is a card: {k:?}");
        } else {
            assert_eq!((h.n_player, h.n_dealer), (0, 0), "no card before the seed: {k:?}");
            assert!(h.dealt <= OPENING_LINKS);
            assert_eq!(h.phase == Phase::Seed, h.dealt == OPENING_LINKS);
        }
        let most = most(h, &mut memo);
        match h.phase {
            // The dealer has seen no card: the stake goes back, though the hand could still have
            // been a natural. That is the point of v2: nothing is paid for a hand never dealt.
            Phase::Open => {
                assert_eq!(h.forfeit(), Some(Outcome::Refund));
                assert_eq!((payout(2, Outcome::Refund), most), (2, 5));
            }
            Phase::Seed | Phase::Player => assert_eq!(h.forfeit(), None, "{k:?}"),
            Phase::Deal | Phase::Hit | Phase::Dealer => {
                dealer_owes += 1;
                let forfeit = h.forfeit().unwrap();
                assert!(forfeit.dealers_fault());
                // Exactly: not a drop less than the cards could still pay (the dealer never
                // gains by a stall), not a drop more (the player never does).
                assert_eq!(payout(2, forfeit), most, "{k:?} forfeits {forfeit:?}");
                let is_natural = natural(h.player_cards());
                naturals += usize::from(is_natural);
                if forfeit == Outcome::ForfeitDeal {
                    assert!(is_natural || h.phase == Phase::Deal, "3:2 only for a natural, or while one can still be dealt: {k:?}");
                }
                // The same state under v1: never less than v2, and more where no natural was
                // possible (the audit's I-1).
                let v1 = bj_core::Hand { phase: bj_core::Phase::from_u8(h.phase as u8).unwrap(), dealt: h.dealt, n_player: h.n_player, n_dealer: h.n_dealer, player: h.player, dealer: h.dealer };
                let v1_pays = bj_core::payout(2, v1.forfeit());
                assert!(v1_pays >= most, "{k:?}");
                v1_paid_more += usize::from(v1_pays > most);
            }
            Phase::Done => unreachable!(),
        }
    }
    assert_eq!(per_phase.get(&(Phase::Open as u8)), Some(&3), "0, 1 and 2 opening links");
    assert_eq!(per_phase.get(&(Phase::Seed as u8)), Some(&1));
    assert!(v1_paid_more > 0, "v1 overpaid somewhere, or this walk found nothing");
    assert!(naturals > 0);
    println!("{} states {per_phase:?}; the dealer owes a card in {dealer_owes}; v1 paid more than the cards allowed in {v1_paid_more}", states.len());
}

#[test]
fn a_relayed_deal_pays_three_to_two_only_while_a_natural_can_still_be_dealt() {
    let dealt = |cards: &[u8]| {
        let mut h = Hand::new();
        for c in cards {
            h.deal(*c);
        }
        h
    };
    assert_eq!(Hand::new().forfeit(), Some(Outcome::ForfeitDeal), "no card is out");
    for first in 0..52u8 {
        let can = matches!(first % 13, 0 | 9..=12);
        let want = Some(if can { Outcome::ForfeitDeal } else { Outcome::Forfeit });
        assert_eq!(dealt(&[first]).forfeit(), want, "first card {first}");
        for up in 0..52u8 {
            // The upcard changes nothing: 3:2 or a push is the hole card's to decide.
            assert_eq!(dealt(&[first, up]).forfeit(), want, "{first} against {up}");
        }
    }
    // Once the deal is done, 3:2 is a natural's and nobody else's.
    let h = dealt(&[0, 9, 12]);
    assert_eq!((h.phase, h.forfeit()), (Phase::Dealer, Some(Outcome::ForfeitDeal)), "a natural waiting on the hole card");
    let mut h = dealt(&[4, 9, 5]);
    assert_eq!((h.phase, h.forfeit()), (Phase::Player, None));
    assert!(h.hit());
    assert_eq!(h.forfeit(), Some(Outcome::Forfeit));
    assert_eq!(h.deal(9), Some(Step::More), "21 in three cards stands by itself");
    assert_eq!(h.forfeit(), Some(Outcome::Forfeit), "and isn't a natural");
}

#[test]
fn an_opened_hand_is_the_hand_v1_deals() {
    for a in 0..52u8 {
        for b in (0..52u8).step_by(3) {
            for c in (0..52u8).step_by(5) {
                let mut v1 = bj_core::Hand::new();
                let mut last = bj_core::Step::More;
                for card in [a, b, c] {
                    last = v1.deal(card).unwrap();
                }
                let mut h = Hand::committed();
                for _ in 0..3 {
                    h.open_link();
                }
                let step = h.seeded([a, b, c]).unwrap();
                assert_eq!(format!("{step:?}"), format!("{last:?}"));
                assert_eq!((h.phase as u8, h.dealt, h.n_player, h.n_dealer, h.player, h.dealer), (v1.phase as u8, v1.dealt, v1.n_player, v1.n_dealer, v1.player, v1.dealer));
            }
        }
    }
}

// ---------------------------------------------------------------------------------- payouts

fn outcomes() -> impl Iterator<Item = Outcome> {
    (1..=9).map(|n| Outcome::from_u8(n).unwrap())
}

/// What an outcome pays, in halves of a stake, from the rules' wording.
fn halves(o: Outcome) -> u128 {
    match o {
        Outcome::Blackjack | Outcome::ForfeitDeal => 5,
        Outcome::Win | Outcome::Forfeit => 4,
        Outcome::Push | Outcome::Refund => 2,
        Outcome::Lose | Outcome::Bust | Outcome::NoSeed => 0,
    }
}

#[test]
fn payouts_conserve_every_drop_and_fit_the_reserve() {
    let mut rng = Rng::new("blackjack2/payouts");
    let mut stakes: Vec<u64> = vec![0, 1, 2, 3, 4, 5, FANG - 1, FANG, FANG + 1, 50 * FANG, 50 * FANG + 1, 1_000_000_000 * FANG / 3];
    while stakes.len() < 20_000 {
        let n = rng.u64();
        // Every size up to the supply.
        stakes.push(n % (1 + (1u64 << (n % 60))).min(1_000_000_000 * FANG / 3));
    }
    for stake in stakes {
        let reserve = exposure(stake);
        assert_eq!(u128::from(reserve), u128::from(stake) * 3 / 2);
        let mut dearest = 0;
        for o in outcomes() {
            let pays = payout(stake, o);
            // Against exact arithmetic: half a drop of a 3:2 rounds down, to the house.
            assert_eq!(u128::from(pays), u128::from(stake) * halves(o) / 2, "{o:?} of {stake}");
            // Solvency: what the hand holds (its stake and its reserve) covers every ending, and
            // what isn't paid goes back to the bankroll: nothing is made or lost.
            let held = stake + reserve;
            assert!(pays <= held, "{o:?} of {stake} pays within the stake and its reserve");
            let to_bankroll = held - pays;
            assert_eq!(u128::from(pays) + u128::from(to_bankroll), u128::from(stake) + u128::from(reserve));
            // v1's outcomes pay what v1 pays.
            if let Some(v1) = bj_core::Outcome::from_u8(o as u8) {
                assert_eq!(pays, bj_core::payout(stake, v1));
            }
            dearest = dearest.max(pays);
            // A stake cut in two never pays more than the whole.
            let (a, b) = (stake / 3, stake - stake / 3);
            assert!(payout(a, o) + payout(b, o) <= pays, "{o:?}: {a} + {b}");
        }
        assert_eq!(dearest, stake + reserve, "the reserve is exactly the dearest ending");
        // A refund is the stake, to the drop: the house neither pays nor keeps anything.
        assert_eq!(payout(stake, Outcome::Refund), stake);
        assert_eq!(stake + reserve - payout(stake, Outcome::Refund), reserve, "the whole reserve returns");
    }
}

#[test]
#[should_panic(expected = "overflow")]
fn a_payout_that_does_not_fit_panics_instead_of_wrapping() {
    payout(std::hint::black_box(u64::MAX / 2), Outcome::ForfeitDeal);
}

#[test]
#[should_panic(expected = "overflow")]
fn a_win_that_does_not_fit_panics_instead_of_wrapping() {
    payout(std::hint::black_box(u64::MAX / 2 + 1), Outcome::Forfeit);
}

#[test]
fn a_refund_of_any_stake_fits() {
    assert_eq!(payout(u64::MAX, Outcome::Refund), u64::MAX);
    assert_eq!(payout(u64::MAX, Outcome::NoSeed), 0);
}

/// The guests are built with overflow checks, so the same sums trap on the chain.
#[test]
fn the_guests_keep_overflow_checks() {
    let manifest = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("guests/Cargo.toml")).unwrap();
    assert!(manifest.contains("overflow-checks = true"));
    assert!(manifest.contains("\"bj2-core\"") && manifest.contains("\"blackjack2\""));
}

// ---------------------------------------------------------------------------------- vectors

fn vectors_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../services/blackjack/core/test/vectors2.json")
}

fn list(cards: &[u8]) -> String {
    format!("[{}]", cards.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", "))
}

/// The vector file: what each outcome pays, what a stall ends every kind of state as, and hands
/// with the outcome a missed deadline would have had at every link, opened or relayed.
fn vectors() -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"about\": \"Blackjack rules v2 vectors, written by node/vm/tests/blackjack2_rules.rs from guests/bj2-core. Do not edit: cargo test --release --test blackjack2_rules write -- --ignored\",\n");
    writeln!(out, "  \"rules\": {RULES},").unwrap();
    writeln!(out, "  \"opening_links\": {OPENING_LINKS},").unwrap();

    out.push_str("  \"payouts\": [\n");
    let mut rows = vec![];
    for stake in [1u64, 3, FANG, 25 * FANG + 1] {
        for o in outcomes() {
            rows.push(format!("    {{ \"stake\": \"{stake}\", \"outcome\": {}, \"payout\": \"{}\" }}", o as u8, payout(stake, o)));
        }
    }
    out.push_str(&rows.join(",\n"));
    out.push_str("\n  ],\n");

    // States: every one before the player's first move, and a spread of the rest.
    out.push_str("  \"forfeits\": [\n");
    let mut rows = vec![];
    for (i, (k, h)) in walk().iter().enumerate() {
        let early = !h.phase.seeded() || h.phase == Phase::Deal || (h.n_player == 2 && h.n_dealer == 1 && natural(h.player_cards()));
        if !early && i % 101 != 0 {
            continue;
        }
        let forfeit = h.forfeit().map_or("null".to_string(), |o| (o as u8).to_string());
        rows.push(format!("    {{ \"phase\": {}, \"dealt\": {}, \"player\": {}, \"dealer\": {}, \"forfeit\": {forfeit} }}", k.phase, h.dealt, list(h.player_cards()), list(h.dealer_cards())));
    }
    out.push_str(&rows.join(",\n"));
    out.push_str("\n  ],\n");

    out.push_str("  \"hands\": [\n");
    let mut rng = Rng::new("blackjack2/vectors");
    let mut per_outcome: BTreeMap<u8, u32> = BTreeMap::new();
    let mut hands = vec![];
    let mut n = 0u64;
    while hands.len() < 30 || (1..=5).any(|o| per_outcome.get(&o).copied().unwrap_or(0) < 4) {
        let top = rng.bytes32();
        let links = chain(top);
        let seed = rng.bytes32();
        let id = match n % 3 {
            0 => n,
            1 => rng.u64(),
            _ => (1 << 53) + n,
        };
        let label = format!("vector-moves/{n}");
        n += 1;
        let (played, opened_stalls) = play_v2(&links, &seed, id, true, policy(&mut Rng::new(&label)));
        let (_, relayed_stalls) = play_v2(&links, &seed, id, false, policy(&mut Rng::new(&label)));
        let count = per_outcome.entry(played.outcome).or_default();
        if *count >= 8 {
            continue;
        }
        *count += 1;
        hands.push((top, seed, id, links, played, opened_stalls, relayed_stalls));
    }
    for (i, (top, seed, id, links, p, opened_stalls, relayed_stalls)) in hands.iter().enumerate() {
        let revealed: Vec<String> = links[1..=p.cards.len()].iter().map(|l| format!("\"{}\"", hex(l))).collect();
        let stake = 7 * FANG + 1 + i as u64;
        out.push_str("    {\n");
        writeln!(out, "      \"top\": \"{}\",", hex(top)).unwrap();
        writeln!(out, "      \"client_seed\": \"{}\",", hex(seed)).unwrap();
        writeln!(out, "      \"game_id\": \"{id}\",").unwrap();
        writeln!(out, "      \"moves\": \"{}\",", p.moves).unwrap();
        writeln!(out, "      \"commit\": \"{}\",", hex(&links[0])).unwrap();
        writeln!(out, "      \"links\": [{}],", revealed.join(", ")).unwrap();
        writeln!(out, "      \"cards\": {},", list(&p.cards)).unwrap();
        writeln!(out, "      \"player\": {},", list(&p.player)).unwrap();
        writeln!(out, "      \"dealer\": {},", list(&p.dealer)).unwrap();
        writeln!(out, "      \"outcome\": {},", p.outcome).unwrap();
        writeln!(out, "      \"stake\": \"{stake}\",").unwrap();
        writeln!(out, "      \"payout\": \"{}\",", payout(stake, Outcome::from_u8(p.outcome).unwrap())).unwrap();
        writeln!(out, "      \"opened_stalls\": {},", list(opened_stalls)).unwrap();
        writeln!(out, "      \"relayed_stalls\": {}", list(relayed_stalls)).unwrap();
        out.push_str(if i + 1 < hands.len() { "    },\n" } else { "    }\n" });
    }
    out.push_str("  ]\n}\n");
    out
}

#[test]
fn the_committed_vectors_are_current() {
    let path = vectors_path();
    let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(committed == vectors(), "{} is stale: cargo test --release --test blackjack2_rules write -- --ignored", path.display());
}

#[test]
#[ignore = "writes services/blackjack/core/test/vectors2.json"]
fn write() {
    let path = vectors_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, vectors()).unwrap();
}
