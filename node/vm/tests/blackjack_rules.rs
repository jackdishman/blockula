//! The blackjack guest's rules (`guests/bj-core`, docs/BLACKJACK.md), built for the host:
//!
//! * **BLAKE3.** The crate's own one-chunk BLAKE3 is the `blake3` crate's for every length it
//!   takes, so a card on the chain is the card anyone recomputes with a stock BLAKE3.
//! * **The rules, against an independent reference.** 10,000 random hands played twice: once
//!   through `bj_core::Hand` as the guest drives it (links checked against the chain, cards fed
//!   one by one), once through [`reference`], a second implementation written from the rules'
//!   wording with nothing shared but the `blake3` crate. Every card, total, outcome and payout
//!   must agree.
//! * **The bounds.** No hand holds more cards than `MAX_PLAYER_CARDS` / `MAX_DEALER_CARDS`, and
//!   hands that long exist, by search over every sequence of ranks.
//! * **The vectors.** `services/blackjack/core/test/vectors.json` holds hands this file
//!   generated; the TypeScript rules (`@blockula/blackjack`) are held to them. After changing the
//!   rules, regenerate with `cargo test --release --test blackjack_rules write -- --ignored`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use bj_core::{CHAIN_LEN, Hand, MAX_DEALER_CARDS, MAX_PLAYER_CARDS, Outcome, Phase, Step, card, exposure, payout, prev_link, seed_hash, total};

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

    /// True `percent` times in a hundred.
    fn chance(&mut self, percent: u64) -> bool {
        self.u64() % 100 < percent
    }
}

// ---------------------------------------------------------------------------------- BLAKE3

#[test]
fn the_crates_blake3_is_blake3_for_every_one_chunk_input() {
    let data: Vec<u8> = (0..1024u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    for len in 0..=1024 {
        assert_eq!(bj_core::blake3(&data[..len]), *blake3::hash(&data[..len]).as_bytes(), "{len} bytes");
    }
    // The specification's own test vector input: bytes 0, 1, …, 250, 0, 1, …
    let spec: Vec<u8> = (0..1024usize).map(|i| (i % 251) as u8).collect();
    for len in [0, 1, 2, 3, 4, 5, 6, 7, 8, 63, 64, 65, 127, 128, 129, 1023, 1024] {
        assert_eq!(bj_core::blake3(&spec[..len]), *blake3::hash(&spec[..len]).as_bytes(), "{len} bytes");
    }
    assert_eq!(hex(&bj_core::blake3(b"")), "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262");
}

#[test]
#[should_panic]
fn more_than_one_chunk_is_refused() {
    bj_core::blake3(&[0u8; 1025]);
}

// ---------------------------------------------------------------------------------- the chain

/// A chain from its top link `s_N`: `links[i] = s_i`, so `links[0]` is the commitment.
fn chain(top: [u8; 32]) -> Vec<[u8; 32]> {
    let mut links = vec![top];
    for _ in 0..CHAIN_LEN {
        links.push(*blake3::hash(links.last().unwrap()).as_bytes());
    }
    links.reverse();
    links
}

#[test]
fn a_chain_hashes_down_to_its_commitment() {
    let links = chain([7; 32]);
    assert_eq!(links.len(), CHAIN_LEN + 1);
    for j in 1..=CHAIN_LEN {
        assert_eq!(prev_link(&links[j]), links[j - 1], "link {j}");
    }
    // A link from another chain, or out of order, doesn't.
    assert_ne!(prev_link(&links[2]), links[0]);
    assert_ne!(prev_link(&chain([8; 32])[1]), links[0]);
}

// ---------------------------------------------------------------------------------- hands

/// A hand played to its end: what both implementations must agree on.
#[derive(Debug, PartialEq, Eq, Clone)]
struct Played {
    /// Every card in chain order.
    cards: Vec<u8>,
    player: Vec<u8>,
    dealer: Vec<u8>,
    player_total: u8,
    dealer_total: u8,
    outcome: u8,
    /// What a 1 FANG stake pays back.
    payout: u64,
    /// The moves the player made: `H` or `S`. A hand that reaches 21 or goes over makes none.
    moves: String,
}

/// The guest's way: every link checked against the one before, every card through `Hand::deal`.
/// `wants` is asked for a move on the player's turn (`true` to hit).
fn play_core(links: &[[u8; 32]], seed: &[u8; 32], id: u64, mut wants: impl FnMut(&[u8], u8) -> bool) -> Played {
    let mut hand = Hand::new();
    let mut last = links[0];
    let mut cards = vec![];
    let mut moves = String::new();
    let outcome = loop {
        if hand.phase == Phase::Player {
            if wants(hand.player_cards(), hand.dealer[0]) {
                assert!(hand.hit());
                moves.push('H');
            } else {
                assert!(hand.stand());
                moves.push('S');
            }
            assert!(!hand.hit() && !hand.stand(), "one move a turn");
        }
        assert!(hand.phase.dealers());
        let j = hand.dealt + 1;
        let link = links[j as usize];
        assert_eq!(prev_link(&link), last);
        last = link;
        let c = card(&link, seed, id, j);
        cards.push(c);
        match hand.deal(c).expect("the dealer owed a card") {
            Step::More => assert!(hand.phase.dealers()),
            Step::Player => assert_eq!(hand.phase, Phase::Player),
            Step::Settled(o) => break o,
        }
    };
    assert_eq!(hand.phase, Phase::Done);
    assert_eq!(hand.deal(0), None, "a settled hand takes no card");
    assert_eq!(hand.dealt as usize, cards.len());
    let (player, dealer) = (hand.player_cards().to_vec(), hand.dealer_cards().to_vec());
    Played { cards, player_total: total(&player).0, dealer_total: total(&dealer).0, player, dealer, outcome: outcome as u8, payout: payout(FANG, outcome), moves }
}

/// A second implementation, from the rules as a player would read them, sharing nothing with
/// `bj-core` but the `blake3` crate.
mod reference {
    use super::Played;

    pub fn card(link: &[u8; 32], seed: &[u8; 32], id: u64, j: usize) -> u8 {
        let mut h = blake3::Hasher::new();
        h.update(link).update(seed).update(&id.to_le_bytes()).update(&[j as u8]);
        let first8: [u8; 8] = h.finalize().as_bytes()[..8].try_into().unwrap();
        (u64::from_le_bytes(first8) % 52) as u8
    }

    /// The best total: every ace as 1 or 11, the highest that isn't over 21, else the lowest.
    pub fn best(cards: &[u8]) -> u32 {
        let mut totals = vec![0u32];
        for c in cards {
            let rank = c % 13 + 1; // 1 ace … 13 king
            totals = match rank {
                1 => totals.iter().flat_map(|t| [t + 1, t + 11]).collect(),
                11..=13 => totals.iter().map(|t| t + 10).collect(),
                n => totals.iter().map(|t| t + u32::from(n)).collect(),
            };
        }
        totals.iter().copied().filter(|t| *t <= 21).max().unwrap_or_else(|| *totals.iter().min().unwrap())
    }

    const BLACKJACK: u8 = 1;
    const WIN: u8 = 2;
    const PUSH: u8 = 3;
    const LOSE: u8 = 4;
    const BUST: u8 = 5;

    pub fn play(links: &[[u8; 32]], seed: &[u8; 32], id: u64, mut wants: impl FnMut(&[u8], u8) -> bool) -> Played {
        let mut next = 1;
        let mut cards = vec![];
        let mut draw = |cards: &mut Vec<u8>| {
            let c = card(&links[next], seed, id, next);
            next += 1;
            cards.push(c);
            c
        };
        let mut player = vec![draw(&mut cards)];
        let mut dealer = vec![draw(&mut cards)];
        player.push(draw(&mut cards));
        let mut moves = String::new();
        let ten_or_ace = |c: u8| matches!(c % 13, 0 | 9..=12);

        let outcome = if best(&player) == 21 {
            // A natural. The dealer looks at its hole card only if its upcard could make one too.
            if ten_or_ace(dealer[0]) {
                dealer.push(draw(&mut cards));
                if best(&dealer) == 21 { PUSH } else { BLACKJACK }
            } else {
                BLACKJACK
            }
        } else {
            while best(&player) < 21 {
                if !wants(&player, dealer[0]) {
                    moves.push('S');
                    break;
                }
                moves.push('H');
                player.push(draw(&mut cards));
            }
            if best(&player) > 21 {
                BUST
            } else {
                dealer.push(draw(&mut cards));
                while best(&dealer) < 17 {
                    dealer.push(draw(&mut cards));
                }
                let (p, d) = (best(&player), best(&dealer));
                let dealer_natural = dealer.len() == 2 && d == 21;
                if dealer_natural {
                    LOSE
                } else if d > 21 || p > d {
                    WIN
                } else if p == d {
                    PUSH
                } else {
                    LOSE
                }
            }
        };
        let payout = match outcome {
            BLACKJACK => 2_500_000_000,
            WIN => 2_000_000_000,
            PUSH => 1_000_000_000,
            _ => 0,
        };
        Played { player_total: best(&player) as u8, dealer_total: best(&dealer) as u8, cards, player, dealer, outcome, payout, moves }
    }
}

/// A player who hits more often on low totals, and now and then on anything: long hands and
/// busts both happen.
fn policy(rng: &mut Rng) -> impl FnMut(&[u8], u8) -> bool + '_ {
    move |player, _up| {
        let t = reference::best(player);
        rng.chance(if t < 12 { 95 } else if t < 17 { 60 } else { 15 })
    }
}

#[test]
fn ten_thousand_random_hands_match_the_reference() {
    let mut rng = Rng::new("blackjack/property");
    let mut seen: BTreeMap<u8, u32> = BTreeMap::new();
    let (mut longest_player, mut longest_dealer) = (0, 0);
    for n in 0..10_000u64 {
        let links = chain(rng.bytes32());
        let seed = rng.bytes32();
        let id = if n % 3 == 0 { rng.u64() } else { n };
        // Both get the same decisions: a fork of the stream per hand.
        let mut a = Rng::new(&format!("moves/{n}"));
        let mut b = Rng::new(&format!("moves/{n}"));
        let core = play_core(&links, &seed, id, policy(&mut a));
        let reference = reference::play(&links, &seed, id, policy(&mut b));
        assert_eq!(core, reference, "hand {n}");
        assert!(core.cards.iter().all(|c| *c < 52));
        *seen.entry(core.outcome).or_default() += 1;
        longest_player = longest_player.max(core.player.len());
        longest_dealer = longest_dealer.max(core.dealer.len());
    }
    for outcome in 1..=5 {
        assert!(seen.get(&outcome).copied().unwrap_or(0) > 50, "outcome {outcome} barely happened: {seen:?}");
    }
    assert!(longest_player >= 6 && longest_dealer >= 5, "longest hands {longest_player} and {longest_dealer}");
    println!("outcomes {seen:?}; longest player hand {longest_player}, dealer {longest_dealer}");
}

#[test]
fn totals_count_one_ace_high_when_it_fits() {
    // (cards by rank index, total, soft)
    let cases: &[(&[u8], u8, bool)] = &[
        (&[0, 12], 21, true),
        (&[0, 0], 12, true),
        (&[0, 5, 9], 17, false),
        (&[0, 5], 17, true),
        (&[9, 10, 11], 30, false),
        (&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 21, true),
        (&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], 12, false),
        (&[4, 5], 11, false),
        (&[], 0, false),
    ];
    for (cards, t, soft) in cases {
        assert_eq!(total(cards), (*t, *soft), "{cards:?}");
        assert_eq!(u32::from(*t), reference::best(cards), "{cards:?}");
    }
    // Every suit of a rank counts the same.
    for c in 0..52u8 {
        assert_eq!(bj_core::points(c), bj_core::points(c % 13));
        assert_eq!(total(&[c]).0 as u32, reference::best(&[c]));
    }
}

#[test]
fn payouts_and_exposure() {
    for stake in [0, 1, 2, 3, FANG, 50 * FANG, 50 * FANG + 1, u64::MAX / 4] {
        assert_eq!(exposure(stake), stake + stake / 2);
        assert_eq!(payout(stake, Outcome::Blackjack), stake + stake + stake / 2);
        assert_eq!(payout(stake, Outcome::ForfeitDeal), payout(stake, Outcome::Blackjack));
        assert_eq!(payout(stake, Outcome::Win), 2 * stake);
        assert_eq!(payout(stake, Outcome::Forfeit), 2 * stake);
        assert_eq!(payout(stake, Outcome::Push), stake);
        assert_eq!(payout(stake, Outcome::Lose), 0);
        assert_eq!(payout(stake, Outcome::Bust), 0);
        assert_eq!(payout(stake, Outcome::NoSeed), 0);
        for o in 1..=8 {
            let o = Outcome::from_u8(o).unwrap();
            assert!(payout(stake, o) <= stake + exposure(stake), "{o:?} pays within the reserve");
        }
    }
    assert_eq!(Outcome::from_u8(0), None);
    assert_eq!(Outcome::from_u8(9), None);
    assert_eq!(Phase::from_u8(6), None);
    for p in 0..=5 {
        assert_eq!(Phase::from_u8(p).unwrap() as u8, p);
    }
}

#[test]
fn payouts_round_down_never_up_for_any_stake() {
    // The largest stake whose payout fits a `u64`: far above the supply (10^18 drops).
    const TOP: u64 = u64::MAX / 5 * 2;
    let mut rng = Rng::new("blackjack/payouts");
    let mut stakes: Vec<u64> = vec![0, 1, 2, 3, 4, 5, FANG - 1, FANG, FANG + 1, 1_000_000_000 * FANG, TOP - 1, TOP];
    for _ in 0..20_000 {
        // Every size of stake, from a few drops to the top.
        stakes.push(rng.u64() % TOP >> (rng.u64() % 62));
    }
    for &stake in &stakes {
        let s = u128::from(stake);
        // Exactly 1.5 × and 2.5 ×, the half drop dropped: never a drop more than the odds.
        assert_eq!(u128::from(exposure(stake)), s * 3 / 2, "{stake}");
        assert_eq!(u128::from(payout(stake, Outcome::Blackjack)), s * 5 / 2, "{stake}");
        assert_eq!(u128::from(payout(stake, Outcome::Win)), s * 2, "{stake}");
        // The house keeps the half drop of an odd stake's natural, and only that.
        assert_eq!(s * 5 - 2 * u128::from(payout(stake, Outcome::Blackjack)), s % 2, "{stake}");
        // What a hand sets aside (its stake and its exposure) covers every way it can end.
        let aside = s + u128::from(exposure(stake));
        let mut most = 0;
        for o in (1..=8).map(|o| Outcome::from_u8(o).unwrap()) {
            let pays = u128::from(payout(stake, o));
            assert!(pays <= aside, "{o:?} of {stake} pays {pays}, more than the {aside} set aside");
            most = most.max(pays);
        }
        assert_eq!(most, aside, "{stake}: the reserve is exactly the dearest ending, not a drop more");
        // A forfeit pays what the hand it stands for would have: no more, no less.
        assert_eq!(payout(stake, Outcome::ForfeitDeal), payout(stake, Outcome::Blackjack));
        assert_eq!(payout(stake, Outcome::Forfeit), payout(stake, Outcome::Win));
    }
    // A bigger stake never pays less, and a stake cut in pieces never pays more than it whole:
    // nobody gains a drop from the rounding by betting many small odd stakes.
    for pair in stakes.chunks(2) {
        let (a, b) = (pair[0] / 2, pair[1] / 2);
        for o in (1..=8).map(|o| Outcome::from_u8(o).unwrap()) {
            assert!(payout(a + b, o) >= payout(a, o) + payout(b, o), "{o:?}: {a} + {b} pays more apart");
            assert!(payout(a + b, o) - (payout(a, o) + payout(b, o)) <= 1, "{o:?}: {a} + {b} differ by more than the half drop");
            assert!(payout(a.max(b), o) >= payout(a.min(b), o), "{o:?} isn't monotone");
            assert!(exposure(a + b) >= exposure(a) + exposure(b));
        }
    }
}

#[test]
fn a_payout_that_doesnt_fit_stops_the_program_instead_of_wrapping() {
    // Past the top the sums don't fit a `u64`. The guests are built with overflow checks (as this
    // test is), so such a sum is a panic, which the program's panic handler turns into an abort:
    // the call fails and nothing is paid. A wrapped sum would pay a small number for a huge stake.
    let top = u64::MAX / 5 * 2;
    assert_eq!(u128::from(payout(top, Outcome::Blackjack)), u128::from(top) * 5 / 2);
    for (stake, outcome) in [(u64::MAX, Outcome::Blackjack), (u64::MAX, Outcome::Win), (u64::MAX / 2 + 1, Outcome::Win), (u64::MAX / 5 * 2 + 2, Outcome::ForfeitDeal), (u64::MAX, Outcome::Forfeit)] {
        let wrapped = std::panic::catch_unwind(|| payout(std::hint::black_box(stake), outcome));
        assert!(wrapped.is_err(), "{outcome:?} of {stake} wrapped to {wrapped:?}");
    }
    assert!(std::panic::catch_unwind(|| exposure(std::hint::black_box(u64::MAX))).is_err());
    // Nothing that can't pay is affected.
    assert_eq!(payout(u64::MAX, Outcome::Push), u64::MAX);
    assert_eq!(payout(u64::MAX, Outcome::Lose), 0);
    // The profile the guests are built with says so, for every guest.
    let manifest = include_str!("../guests/Cargo.toml");
    let release = manifest.split("[profile.release]").nth(1).expect("the guests' release profile");
    let release = release.split("\n[").next().unwrap();
    assert!(release.lines().any(|l| l.trim() == "overflow-checks = true"), "the guests must be built with overflow checks");
    assert!(release.lines().any(|l| l.trim() == "panic = \"abort\""));
}

/// The rules as a player would read them, on final hands: an oracle that shares nothing with
/// `bj_core::showdown` (aces start at 11 and come down, where the crate's start at 1 and go up).
fn oracle(player: &[u8], dealer: &[u8]) -> Outcome {
    fn value(cards: &[u8]) -> u32 {
        let mut aces = 0;
        let mut sum = 0;
        for c in cards {
            match c % 13 {
                0 => {
                    aces += 1;
                    sum += 11;
                }
                r @ 1..=8 => sum += u32::from(r) + 1,
                _ => sum += 10,
            }
        }
        while sum > 21 && aces > 0 {
            sum -= 10;
            aces -= 1;
        }
        sum
    }
    let (p, d) = (value(player), value(dealer));
    let (p_natural, d_natural) = (player.len() == 2 && p == 21, dealer.len() == 2 && d == 21);
    match () {
        _ if p > 21 => Outcome::Bust,
        _ if p_natural && d_natural => Outcome::Push,
        _ if p_natural => Outcome::Blackjack,
        _ if d_natural => Outcome::Lose,
        _ if d > 21 => Outcome::Win,
        _ if p > d => Outcome::Win,
        _ if p == d => Outcome::Push,
        _ => Outcome::Lose,
    }
}

#[test]
fn every_showdown_of_up_to_three_cards_a_side_matches_the_oracle() {
    // Every hand of one to three ranks (suits never matter: `totals_count…` checks that).
    let mut hands: Vec<Vec<u8>> = vec![];
    for a in 0..13u8 {
        hands.push(vec![a]);
        for b in 0..13u8 {
            hands.push(vec![a, b]);
            for c in 0..13u8 {
                hands.push(vec![a, b, c]);
            }
        }
    }
    let mut seen: BTreeMap<u8, u64> = BTreeMap::new();
    for player in hands.iter().filter(|h| h.len() >= 2) {
        for dealer in &hands {
            let got = bj_core::showdown(player, dealer);
            assert_eq!(got, oracle(player, dealer), "{player:?} against {dealer:?}");
            *seen.entry(got as u8).or_default() += 1;
        }
    }
    assert_eq!(seen.keys().copied().collect::<Vec<_>>(), [1, 2, 3, 4, 5], "every outcome of a showdown: {seen:?}");
    // Suits change nothing.
    for (player, dealer) in [(vec![0u8, 12], vec![9u8, 0]), (vec![5, 6, 7], vec![9, 8]), (vec![9, 9, 9], vec![3])] {
        for suit in 0..4u8 {
            let lift = |h: &[u8]| h.iter().map(|c| c + 13 * suit).collect::<Vec<_>>();
            assert_eq!(bj_core::showdown(&lift(&player), &dealer), bj_core::showdown(&player, &dealer));
            assert_eq!(bj_core::showdown(&player, &lift(&dealer)), bj_core::showdown(&player, &dealer));
        }
    }
}

/// What a walk over every hand found.
#[derive(Default)]
struct Walk {
    /// The most a state can still pay (of a stake of 2), by the state's key.
    memo: BTreeMap<(u8, u8, bool, u8, u8, bool, u8), u64>,
    ends: BTreeMap<u8, u64>,
    longest: (u8, u8, u8),
    /// States where a forfeit pays more than any ending the cards could still reach.
    generous: u64,
}

/// Everything the rules can do from `hand`: every card the dealer could turn (all thirteen
/// ranks) and both of the player's moves, each ending checked against the oracle and the bounds.
/// Returns the most the hand can still pay for a stake of 2. States are remembered by what the
/// rules look at (the phase, each side's hard total, whether it holds an ace, and whether it
/// holds two cards or more), so the walk is over every reachable state, each with every move.
fn walk(hand: Hand, w: &mut Walk) -> u64 {
    let side = |cards: &[u8]| {
        let hard: u8 = cards.iter().map(|c| bj_core::points(*c)).sum();
        (hard, cards.iter().any(|c| c % 13 == 0), (cards.len() as u8).min(3))
    };
    let (p, d) = (side(hand.player_cards()), side(hand.dealer_cards()));
    let key = (hand.phase as u8, p.0, p.1, p.2, d.0, d.1, d.2);
    if let Some(most) = w.memo.get(&key) {
        return *most;
    }
    // The bounds the program's records are sized by.
    assert!(hand.n_player as usize <= MAX_PLAYER_CARDS && hand.n_dealer as usize <= MAX_DEALER_CARDS, "{hand:?}");
    assert_eq!(hand.dealt, hand.n_player + hand.n_dealer, "{hand:?}");
    assert!((hand.dealt as usize) < CHAIN_LEN, "an open hand always has a link left: {hand:?}");
    w.longest = (w.longest.0.max(hand.n_player), w.longest.1.max(hand.n_dealer), w.longest.2.max(hand.dealt));
    let most = match hand.phase {
        Phase::Player => {
            // The player's turn comes only below 21, and never with a card owed.
            assert!(total(hand.player_cards()).0 < 21 && hand.n_player >= 2 && hand.n_dealer == 1, "{hand:?}");
            assert_eq!({ hand }.deal(0), None, "no card on the player's turn");
            let (mut hit, mut stand) = (hand, hand);
            assert!(hit.hit() && stand.stand());
            assert_eq!((hit.phase, stand.phase), (Phase::Hit, Phase::Dealer));
            // A move changes whose turn it is and nothing else.
            assert_eq!(Hand { phase: Phase::Player, ..hit }, hand);
            assert_eq!(Hand { phase: Phase::Player, ..stand }, hand);
            walk(hit, w).max(walk(stand, w))
        }
        Phase::Deal | Phase::Hit | Phase::Dealer => {
            assert!(!{ hand }.hit() && !{ hand }.stand() && !{ hand }.seeded(), "no move while a card is owed");
            let mut most = 0;
            for rank in 0..13u8 {
                let mut next = hand;
                let step = next.deal(rank).expect("the dealer owed a card");
                // One card, to one side, and never one taken back.
                assert_eq!(next.dealt, hand.dealt + 1);
                assert!(next.player_cards().starts_with(hand.player_cards()) && next.dealer_cards().starts_with(hand.dealer_cards()));
                most = most.max(match step {
                    Step::More => {
                        assert!(next.phase.dealers(), "{next:?}");
                        walk(next, w)
                    }
                    Step::Player => {
                        assert_eq!(next.phase, Phase::Player);
                        walk(next, w)
                    }
                    Step::Settled(outcome) => {
                        assert_eq!(next.phase, Phase::Done);
                        let (player, dealer) = (next.player_cards(), next.dealer_cards());
                        assert_eq!(outcome, oracle(player, dealer), "{player:?} against {dealer:?}");
                        // The dealer played by the book: nothing after a bust; only the hole
                        // card against a natural, and only when the upcard could match it;
                        // otherwise up to 17 and not a card further.
                        let natural = player.len() == 2 && total(player).0 == 21;
                        if outcome == Outcome::Bust {
                            assert_eq!(dealer.len(), 1);
                        } else if natural {
                            assert_eq!(dealer.len(), if matches!(dealer[0] % 13, 0 | 9..=12) { 2 } else { 1 });
                        } else {
                            assert!(dealer.len() >= 2 && total(dealer).0 >= 17 && total(&dealer[..dealer.len() - 1]).0 < 17, "{dealer:?}");
                        }
                        assert_eq!({ next }.deal(0), None, "a settled hand takes no card");
                        assert!(!{ next }.hit() && !{ next }.stand() && !{ next }.seeded());
                        *w.ends.entry(outcome as u8).or_default() += 1;
                        payout(2, outcome)
                    }
                });
            }
            // The dealer never gains by holding a card back: a missed deadline pays at least
            // the most the cards could still have paid, and exactly that once the deal is done.
            let forfeit = payout(2, hand.forfeit());
            assert!(forfeit >= most, "{hand:?}: a forfeit pays {forfeit}, the cards could still pay {most}");
            if hand.phase != Phase::Deal {
                assert_eq!(forfeit, most, "{hand:?}");
            } else if forfeit > most {
                w.generous += 1;
            }
            most
        }
        Phase::Seed | Phase::Done => unreachable!("{hand:?}"),
    };
    w.memo.insert(key, most);
    most
}

#[test]
fn every_hand_there_is_ends_as_the_rules_say_and_a_stall_never_pays_the_dealer() {
    let mut w = Walk::default();
    assert_eq!(walk(Hand::new(), &mut w), payout(2, Outcome::Blackjack), "a new hand can still be a natural");
    // Every ending the cards can give was reached, many times over.
    assert_eq!(w.ends.keys().copied().collect::<Vec<_>>(), [1, 2, 3, 4, 5], "{:?}", w.ends);
    assert!(w.ends.values().all(|n| *n > 50), "{:?}", w.ends);
    // The longest hands are exactly the bounds, and together they fit the chain.
    assert_eq!((w.longest.0 as usize, w.longest.1 as usize), (MAX_PLAYER_CARDS, MAX_DEALER_CARDS - 1), "the longest open hands");
    assert!((w.longest.2 as usize) < CHAIN_LEN);
    // A dealer that stalls during the deal is charged 3:2 even where the cards already out rule
    // a natural out: more than the hand could have paid, never less.
    assert!(w.generous > 0);
    println!("{} states, endings {:?}, longest open hand {:?}, {} deal states where a forfeit pays above the cards' best", w.memo.len(), w.ends, w.longest, w.generous);
}

#[test]
fn cards_come_out_evenly_whatever_the_seed_the_game_or_the_index() {
    // 52,000 cards from the formula: each of the 52 about 1,000 times. The bound is nine
    // standard deviations (σ ≈ 31): a formula that favoured a card would be far outside it.
    let mut rng = Rng::new("blackjack/cards");
    let mut counts = [0u32; 52];
    let mut by_rank_first_two = [0u32; 13];
    for n in 0..52_000u32 {
        let (link, seed) = (rng.bytes32(), rng.bytes32());
        let c = card(&link, &seed, rng.u64(), 1 + (n % CHAIN_LEN as u32) as u8);
        assert!(c < 52);
        counts[c as usize] += 1;
        by_rank_first_two[(c % 13) as usize] += 1;
    }
    for (c, n) in counts.iter().enumerate() {
        assert!((720..=1280).contains(n), "card {c} came out {n} times in 52,000");
    }
    for (r, n) in by_rank_first_two.iter().enumerate() {
        assert!((3_500..=4_500).contains(n), "rank {r} came out {n} times in 52,000");
    }
    // Each input decides the card: change any one and the 32 cards of a hand change.
    let (link, seed) = (rng.bytes32(), rng.bytes32());
    let hand = |link: &[u8; 32], seed: &[u8; 32], id: u64| (1..=CHAIN_LEN as u8).map(|j| card(link, seed, id, j)).collect::<Vec<_>>();
    let base = hand(&link, &seed, 7);
    let mut other = seed;
    other[31] ^= 1;
    assert_ne!(hand(&link, &other, 7), base, "the player's seed");
    let mut other = link;
    other[0] ^= 0x80;
    assert_ne!(hand(&other, &seed, 7), base, "the dealer's link");
    assert_ne!(hand(&link, &seed, 8), base, "the game");
    assert_ne!(hand(&link, &seed, 7 + (1 << 63)), base, "the game's high bits");
    // The same link at another index is another card: a card is its place in the chain too.
    assert!((1..CHAIN_LEN as u8).any(|j| card(&link, &seed, 7, j) != card(&link, &seed, 7, j + 1)));
}

/// The longest hand from a total, by search over every rank: `limit` is the total a hand stops
/// drawing at (21 for a player who always hits, 17 for the dealer).
fn longest(hard: u8, ace: bool, limit: u8, memo: &mut BTreeMap<(u8, bool), usize>) -> usize {
    let best = if ace && hard + 10 <= 21 { hard + 10 } else { hard };
    if best >= limit {
        return 0;
    }
    if let Some(n) = memo.get(&(hard, ace)) {
        return *n;
    }
    let n = 1 + (1..=10u8).map(|points| longest(hard + points, ace || points == 1, limit, memo)).max().unwrap();
    memo.insert((hard, ace), n);
    n
}

#[test]
fn no_hand_is_longer_than_the_bounds_and_hands_that_long_exist() {
    assert_eq!(longest(0, false, 21, &mut BTreeMap::new()), MAX_PLAYER_CARDS);
    assert_eq!(longest(0, false, 17, &mut BTreeMap::new()), MAX_DEALER_CARDS);
    const { assert!(MAX_PLAYER_CARDS + MAX_DEALER_CARDS <= CHAIN_LEN) };
    // The longest player hand, through the state machine: ten aces, a two, eight aces, an ace.
    let mut hand = Hand::new();
    let ranks: Vec<u8> = [vec![0u8; 10], vec![1], vec![0; 9]].concat();
    assert_eq!(hand.deal(ranks[0]), Some(Step::More));
    assert_eq!(hand.deal(12), Some(Step::More));
    assert_eq!(hand.deal(ranks[1]), Some(Step::Player));
    for (i, r) in ranks[2..].iter().enumerate() {
        assert!(hand.hit());
        let step = hand.deal(*r).unwrap();
        if i + 3 < MAX_PLAYER_CARDS {
            assert_eq!(step, Step::Player, "card {}", i + 3);
        } else {
            assert_eq!(step, Step::More, "21 in twenty cards stands by itself");
        }
    }
    assert_eq!((hand.n_player as usize, hand.phase), (MAX_PLAYER_CARDS, Phase::Dealer));
    assert_eq!(total(hand.player_cards()), (21, false));
    // The longest dealer hand: an ace up, five more aces, a six, four aces, a ten.
    let mut hand = Hand::new();
    for c in [9, 0, 8] {
        hand.deal(c);
    }
    assert!(hand.stand());
    let draws: Vec<u8> = [vec![0u8; 5], vec![5], vec![0; 4], vec![9]].concat();
    for (i, c) in draws.iter().enumerate() {
        let step = hand.deal(*c).unwrap();
        assert_eq!(step == Step::More, i + 1 < draws.len(), "draw {i}");
    }
    assert_eq!(hand.n_dealer as usize, MAX_DEALER_CARDS);
    assert_eq!(hand.phase, Phase::Done);
}

#[test]
fn naturals_and_the_dealers_hole_card() {
    let deal = |cards: &[u8]| {
        let mut hand = Hand::new();
        let mut last = None;
        for c in cards {
            last = hand.deal(*c);
        }
        (hand, last.unwrap())
    };
    // A natural against a 7 up: paid at once, the dealer draws nothing.
    let (hand, step) = deal(&[0, 6, 12]);
    assert_eq!((step, hand.n_dealer), (Step::Settled(Outcome::Blackjack), 1));
    // Against an ace or a ten-value card up, the hole card decides, and only the hole card.
    for up in [0, 9, 10, 11, 12] {
        let (hand, step) = deal(&[0, up, 12]);
        assert_eq!((step, hand.phase), (Step::More, Phase::Dealer), "upcard {up}");
        let hole_for_natural = if up == 0 { 10 } else { 0 };
        let mut pushed = hand;
        assert_eq!(pushed.deal(hole_for_natural), Some(Step::Settled(Outcome::Push)));
        let mut paid = hand;
        assert_eq!(paid.deal(3), Some(Step::Settled(Outcome::Blackjack)), "the dealer's 14 or 15 draws no further");
        assert_eq!(paid.n_dealer, 2);
    }
    // 21 in three cards isn't a natural: it loses to the dealer's, and pushes a dealer's 21.
    let (mut hand, _) = deal(&[4, 9, 5]);
    assert!(hand.hit());
    assert_eq!(hand.deal(9), Some(Step::More), "21 stands by itself");
    let mut lost = hand;
    assert_eq!(lost.deal(0), Some(Step::Settled(Outcome::Lose)));
    assert_eq!(hand.deal(3), Some(Step::More));
    assert_eq!(hand.deal(6), Some(Step::Settled(Outcome::Push)), "10 + 4 + 7 = 21 against 21");
    // The dealer stands on a soft 17.
    let (mut hand, _) = deal(&[9, 0, 8]);
    assert!(hand.stand());
    assert_eq!(hand.deal(5), Some(Step::Settled(Outcome::Win)), "19 against a soft 17");
    // A missed deadline pays the most the hand could still have paid: 3:2 before the deal is
    // done or against a natural, 1:1 after.
    assert_eq!(Hand::new().forfeit(), Outcome::ForfeitDeal);
    let (hand, _) = deal(&[9, 0]);
    assert_eq!(hand.forfeit(), Outcome::ForfeitDeal);
    let (hand, _) = deal(&[0, 9, 12]);
    assert_eq!(hand.forfeit(), Outcome::ForfeitDeal, "a natural waiting on the hole card");
    let (mut hand, _) = deal(&[4, 9, 5]);
    assert_eq!(hand.forfeit(), Outcome::Forfeit);
    assert!(hand.hit());
    assert_eq!(hand.forfeit(), Outcome::Forfeit);
    // Nothing moves out of turn.
    let (mut hand, _) = deal(&[9, 0]);
    assert!(!hand.hit() && !hand.stand());
    assert_eq!(hand.phase, Phase::Deal);
}

#[test]
fn a_committed_hand_waits_for_its_seed_and_nothing_else() {
    let mut hand = Hand::committed();
    assert_eq!(hand.phase, Phase::Seed);
    assert!(!hand.phase.dealers(), "the dealer owes nothing before the seed");
    assert!(!hand.hit() && !hand.stand());
    assert_eq!(hand.deal(0), None, "no card is dealt before the seed");
    assert_eq!(hand, Hand::committed());
    assert!(hand.seeded());
    assert_eq!(hand, Hand::new(), "the seed's reveal leaves the hand as a one-transaction bet did");
    assert!(!hand.seeded(), "once");
    for c in [4, 9, 5] {
        hand.deal(c);
    }
    assert_eq!(hand.phase, Phase::Player);
    assert!(!hand.seeded());
    assert_eq!(hand.phase, Phase::Player);
}

#[test]
fn a_seeds_hash_is_blake3_of_the_domain_the_player_and_the_seed() {
    let mut rng = Rng::new("blackjack/seed-hash");
    for _ in 0..200 {
        let (player, seed) = (rng.bytes32(), rng.bytes32());
        let mut h = blake3::Hasher::new();
        h.update(b"blockula/blackjack/seed").update(&player).update(&seed);
        assert_eq!(seed_hash(&player, &seed), *h.finalize().as_bytes());
        // The player's account is part of it, and it is neither side's other hashes.
        assert_ne!(seed_hash(&player, &seed), seed_hash(&seed, &player));
        assert_ne!(seed_hash(&player, &seed), bj_core::blake3(&seed));
    }
    assert_eq!(bj_core::SEED_PREIMAGE, 23 + 32 + 32);
    assert_eq!(bj_core::SEED_DOMAIN.len(), 23);
}

// ---------------------------------------------------------------------------------- vectors

fn vectors_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../services/blackjack/core/test/vectors.json")
}

fn list(cards: &[u8]) -> String {
    format!("[{}]", cards.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", "))
}

/// The vector file: hands of every outcome (the first of each from a fixed stream, then more),
/// with everything a client derives: the commitment, the links revealed, each card, both hands,
/// the totals, the outcome and what stakes pay.
fn vectors() -> String {
    let mut rng = Rng::new("blackjack/vectors");
    let mut hands = vec![];
    let mut per_outcome: BTreeMap<u8, u32> = BTreeMap::new();
    let mut n = 0u64;
    // Until there are 60 hands and at least 8 of every outcome (a natural is one hand in 21).
    while hands.len() < 60 || (1..=5).any(|o| per_outcome.get(&o).copied().unwrap_or(0) < 8) {
        let top = rng.bytes32();
        let links = chain(top);
        let seed = rng.bytes32();
        // Ids past 2^53 too: a client that reads them as floats gets other cards.
        let id = match n % 4 {
            0 => n,
            1 => rng.u64(),
            2 => u64::MAX - n,
            _ => (1 << 53) + n,
        };
        let mut moves = Rng::new(&format!("vector-moves/{n}"));
        let played = play_core(&links, &seed, id, policy(&mut moves));
        n += 1;
        let count = per_outcome.entry(played.outcome).or_default();
        // At most 14 of any outcome, and long hands always.
        if *count >= 14 && played.cards.len() < 9 {
            continue;
        }
        *count += 1;
        hands.push((top, seed, id, links, played));
    }

    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"about\": \"Blackjack rules vectors, written by node/vm/tests/blackjack_rules.rs from guests/bj-core. Do not edit: cargo test --release --test blackjack_rules write -- --ignored\",\n");
    writeln!(out, "  \"chain_len\": {CHAIN_LEN},").unwrap();
    writeln!(out, "  \"max_player_cards\": {MAX_PLAYER_CARDS},").unwrap();
    writeln!(out, "  \"max_dealer_cards\": {MAX_DEALER_CARDS},").unwrap();
    out.push_str("  \"hands\": [\n");
    for (i, (top, seed, id, links, p)) in hands.iter().enumerate() {
        let revealed: Vec<String> = links[1..=p.cards.len()].iter().map(|l| format!("\"{}\"", hex(l))).collect();
        let outcome = Outcome::from_u8(p.outcome).unwrap();
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
        writeln!(out, "      \"player_total\": {},", p.player_total).unwrap();
        writeln!(out, "      \"dealer_total\": {},", p.dealer_total).unwrap();
        writeln!(out, "      \"outcome\": {},", p.outcome).unwrap();
        writeln!(out, "      \"stake\": \"{stake}\",").unwrap();
        writeln!(out, "      \"exposure\": \"{}\",", exposure(stake)).unwrap();
        writeln!(out, "      \"payout\": \"{}\"", payout(stake, outcome)).unwrap();
        out.push_str(if i + 1 < hands.len() { "    },\n" } else { "    }\n" });
    }
    out.push_str("  ],\n");
    // Totals on their own: hands a game rarely reaches.
    out.push_str("  \"totals\": [\n");
    let mut t = Rng::new("blackjack/totals");
    for i in 0..40 {
        let len = 1 + (t.u64() % 8) as usize;
        let cards: Vec<u8> = (0..len).map(|_| if t.chance(35) { 13 * (t.u64() % 4) as u8 } else { (t.u64() % 52) as u8 }).collect();
        let (total, soft) = total(&cards);
        writeln!(out, "    {{ \"cards\": {}, \"total\": {total}, \"soft\": {soft} }}{}", list(&cards), if i < 39 { "," } else { "" }).unwrap();
    }
    out.push_str("  ],\n");
    // What a forfeit pays, which no played hand above shows.
    out.push_str("  \"payouts\": [\n");
    let stakes = [1u64, 3, FANG, 25 * FANG + 1];
    let mut rows = vec![];
    for stake in stakes {
        for o in 1..=8u8 {
            rows.push(format!("    {{ \"stake\": \"{stake}\", \"outcome\": {o}, \"payout\": \"{}\" }}", payout(stake, Outcome::from_u8(o).unwrap())));
        }
    }
    out.push_str(&rows.join(",\n"));
    out.push_str("\n  ],\n");
    // What a bet commits to: the hash of the player's account and seed.
    out.push_str("  \"seed_hashes\": [\n");
    let mut s = Rng::new("blackjack/vectors/seed-hashes");
    let rows: Vec<String> = (0..8)
        .map(|_| {
            let (player, seed) = (s.bytes32(), s.bytes32());
            format!("    {{ \"player\": \"{}\", \"client_seed\": \"{}\", \"seed_hash\": \"{}\" }}", hex(&player), hex(&seed), hex(&seed_hash(&player, &seed)))
        })
        .collect();
    out.push_str(&rows.join(",\n"));
    out.push_str("\n  ]\n}\n");
    out
}

#[test]
fn the_committed_vectors_are_current() {
    let path = vectors_path();
    let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(committed == vectors(), "{} is stale: cargo test --release --test blackjack_rules write -- --ignored", path.display());
}

#[test]
#[ignore = "writes services/blackjack/core/test/vectors.json"]
fn write() {
    let path = vectors_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, vectors()).unwrap();
}
