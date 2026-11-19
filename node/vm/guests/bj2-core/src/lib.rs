//! # bj2-core: blackjack rules v2
//!
//! Rules v1 (`../bj-core`) with one thing changed: **what happens when the dealer stops**
//! (docs/proposals/blackjack-dealer-outage.md). The cards are v1's: the same hash chain, the same
//! card formula, the same totals, the same dealer policy and the same showdown, by calling v1's
//! code ([`Hand::deal`] runs `bj_core::Hand::deal`). A hand that is played to its end ends as it
//! would under v1, and pays the same.
//!
//! **What v1 does.** Under v1 a dealer that misses a reveal pays the player the most a hand can
//! ever pay: 3:2 until the deal is done, 1:1 after. That is right against a dealer who has seen
//! the cards and holds one back, and wrong for a dealer that is simply down: every bet placed
//! against a commitment it left on the chain is then paid 3:2, with no card dealt, until the
//! owner pauses the table.
//!
//! **What v2 does.** It separates the two cases by what the dealer could know.
//!
//! 1. **The dealer opens a hand before it can know a card.** A bet from an account carries only
//!    the hash of the player's seed, as in v1. Under v2 the dealer then reveals the chain's first
//!    three links ([`OPENING_LINKS`]) *before* the seed is revealed ([`Phase::Open`]). A card is
//!    `BLAKE3(link ‖ seed ‖ game ‖ j)`: without the seed the links say nothing about it, so a
//!    dealer that opens, or doesn't, isn't choosing by the cards. If it misses that deadline the
//!    stake goes back ([`Outcome::Refund`]) and nobody is paid anything: a bet against a dealer
//!    that is down wins nothing.
//! 2. **The player's seed deals the hand.** Once the three links are out the player reveals the
//!    seed ([`Phase::Seed`]) and the first three cards exist at once ([`Hand::seeded`]): there is
//!    no state in which the dealer knows the deal and still owes it. The player has seen the
//!    three cards before revealing, and gains nothing by it: the seed was fixed by its hash at
//!    the bet, and a seed that never comes forfeits the stake, as in v1.
//! 3. **After that, a stall pays exactly what the cards on the table still allow.** From the seed
//!    on the dealer knows every card, so a missed reveal still pays the player the most the hand
//!    could have paid ([`Hand::forfeit`]): otherwise a dealer would hold back the cards it
//!    dislikes. But the most is now computed from the cards: 3:2 only for a natural in the
//!    player's hand, or (a relayed hand, dealt as in v1) while the player's first card is unseen
//!    or is an ace or a ten-value card. v1 paid 3:2 in 88 states where no natural was possible.
//!
//! A relayed bet's seed is known at the bet (it is derived from the post), so the dealer can't
//! open it blind: a relayed hand is dealt as in v1 ([`Hand::new`], [`Phase::Deal`]) and only the
//! tighter forfeit applies to it. The program adds a circuit breaker on top: the first forfeit
//! stops new bets until the owner looks (`../blackjack2`).
//!
//! A card is a number from 0 to 51, as in v1.

#![no_std]

pub use bj_core::{CARD_PREIMAGE, CHAIN_LEN, DEALER_STANDS_ON, DECK, MAX_DEALER_CARDS, MAX_HASH_INPUT, MAX_PLAYER_CARDS, SEED_DOMAIN, SEED_PREIMAGE, blake3, card, exposure, may_be_natural, natural, points, prev_link, rank, seed_hash, showdown, total};

/// The rules' version: what the program's `version` view answers.
pub const RULES: u8 = 2;

/// Links the dealer reveals to open a hand, before the player's seed: the chain's first three,
/// which deal the player's two cards and the dealer's upcard once the seed is known.
pub const OPENING_LINKS: u8 = 3;

/// How a hand ended. The numbers are the program's event and view encoding; 1 to 8 are v1's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Outcome {
    /// The player's natural against a dealer without one: pays 3:2.
    Blackjack = 1,
    /// The player's total beats the dealer's, or the dealer went over 21: pays 1:1.
    Win = 2,
    /// Equal totals, or two naturals: the stake goes back.
    Push = 3,
    /// The dealer's total beats the player's, or the dealer's natural beats a 21 in three cards
    /// or more.
    Lose = 4,
    /// The player went over 21.
    Bust = 5,
    /// The dealer missed a reveal on a hand that can no longer be a natural: the player is paid
    /// as a win, 1:1, the most that hand could still have paid.
    Forfeit = 6,
    /// The dealer missed a reveal on a hand that holds a natural, or (a relayed hand's deal)
    /// could still become one: the player is paid 3:2.
    ForfeitDeal = 7,
    /// The player never revealed the seed their bet committed to: the stake is the house's.
    NoSeed = 8,
    /// The dealer never opened the hand: the stake goes back, and nothing else is paid. The
    /// dealer had seen no card (it had only the seed's hash), so it gained nothing by not opening.
    Refund = 9,
}

impl Outcome {
    pub const fn from_u8(n: u8) -> Option<Outcome> {
        Some(match n {
            1 => Outcome::Blackjack,
            2 => Outcome::Win,
            3 => Outcome::Push,
            4 => Outcome::Lose,
            5 => Outcome::Bust,
            6 => Outcome::Forfeit,
            7 => Outcome::ForfeitDeal,
            8 => Outcome::NoSeed,
            9 => Outcome::Refund,
            _ => return None,
        })
    }

    const fn from_v1(o: bj_core::Outcome) -> Outcome {
        match o {
            bj_core::Outcome::Blackjack => Outcome::Blackjack,
            bj_core::Outcome::Win => Outcome::Win,
            bj_core::Outcome::Push => Outcome::Push,
            bj_core::Outcome::Lose => Outcome::Lose,
            bj_core::Outcome::Bust => Outcome::Bust,
            bj_core::Outcome::Forfeit => Outcome::Forfeit,
            bj_core::Outcome::ForfeitDeal => Outcome::ForfeitDeal,
            bj_core::Outcome::NoSeed => Outcome::NoSeed,
        }
    }

    /// Whether the dealer's missed deadline cost the house: the program's circuit breaker trips
    /// on these and on nothing else (a refund costs the house nothing).
    pub const fn dealers_fault(self) -> bool {
        matches!(self, Outcome::Forfeit | Outcome::ForfeitDeal)
    }
}

/// What the player gets back from a hand of `stake` drops that ended in `outcome`, the stake
/// included: v1's payouts, and the stake itself for a refund. Never more than
/// `stake + exposure(stake)`.
pub const fn payout(stake: u64, outcome: Outcome) -> u64 {
    match outcome {
        Outcome::Blackjack | Outcome::ForfeitDeal => stake + exposure(stake),
        Outcome::Win | Outcome::Forfeit => stake + stake,
        Outcome::Push | Outcome::Refund => stake,
        Outcome::Lose | Outcome::Bust | Outcome::NoSeed => 0,
    }
}

/// Where a hand is. The numbers are the program's stored state; 0 to 5 are v1's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Phase {
    /// The dealer opened the hand (its first three links are out); the player owes the seed.
    Seed = 0,
    /// A relayed hand, whose seed is known from the bet: the dealer owes the first three cards.
    Deal = 1,
    /// The player's turn: hit or stand.
    Player = 2,
    /// The player hit; the dealer owes one card.
    Hit = 3,
    /// The player stands (or holds 21); the dealer owes its hole card and whatever it draws.
    Dealer = 4,
    /// Settled.
    Done = 5,
    /// The bet is placed with the hash of the player's seed; the dealer owes the opening links.
    /// It knows no card: it has never seen the seed.
    Open = 6,
}

impl Phase {
    pub const fn from_u8(n: u8) -> Option<Phase> {
        Some(match n {
            0 => Phase::Seed,
            1 => Phase::Deal,
            2 => Phase::Player,
            3 => Phase::Hit,
            4 => Phase::Dealer,
            5 => Phase::Done,
            6 => Phase::Open,
            _ => return None,
        })
    }

    /// Whether the next move is the dealer's: a link to reveal.
    pub const fn dealers(self) -> bool {
        matches!(self, Phase::Open | Phase::Deal | Phase::Hit | Phase::Dealer)
    }

    /// Whether the dealer can know the hand's cards: the player's seed is public. Before that
    /// ([`Phase::Open`], [`Phase::Seed`]) it has only the seed's hash.
    pub const fn seeded(self) -> bool {
        !matches!(self, Phase::Open | Phase::Seed)
    }

    const fn v1(self) -> Option<bj_core::Phase> {
        Some(match self {
            Phase::Seed => bj_core::Phase::Seed,
            Phase::Deal => bj_core::Phase::Deal,
            Phase::Player => bj_core::Phase::Player,
            Phase::Hit => bj_core::Phase::Hit,
            Phase::Dealer => bj_core::Phase::Dealer,
            Phase::Done => bj_core::Phase::Done,
            Phase::Open => return None,
        })
    }

    const fn from_v1(p: bj_core::Phase) -> Phase {
        match p {
            bj_core::Phase::Seed => Phase::Seed,
            bj_core::Phase::Deal => Phase::Deal,
            bj_core::Phase::Player => Phase::Player,
            bj_core::Phase::Hit => Phase::Hit,
            bj_core::Phase::Dealer => Phase::Dealer,
            bj_core::Phase::Done => Phase::Done,
        }
    }
}

/// What a dealt card did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// The dealer owes another card.
    More,
    /// The player's turn.
    Player,
    /// The hand is over.
    Settled(Outcome),
}

impl Step {
    const fn from_v1(s: bj_core::Step) -> Step {
        match s {
            bj_core::Step::More => Step::More,
            bj_core::Step::Player => Step::Player,
            bj_core::Step::Settled(o) => Step::Settled(Outcome::from_v1(o)),
        }
    }
}

/// One hand: the cards on the table and whose move it is. The record is v1's, field for field;
/// while the hand is being opened ([`Phase::Open`], [`Phase::Seed`]) `dealt` counts the links the
/// dealer has revealed and there are no cards yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hand {
    pub phase: Phase,
    /// Links revealed so far: the chain index of the newest. From the seed on, each is a card.
    pub dealt: u8,
    pub n_player: u8,
    pub n_dealer: u8,
    pub player: [u8; MAX_PLAYER_CARDS],
    pub dealer: [u8; MAX_DEALER_CARDS],
}

impl Default for Hand {
    fn default() -> Self {
        Hand::new()
    }
}

impl Hand {
    const fn at(phase: Phase) -> Hand {
        Hand { phase, dealt: 0, n_player: 0, n_dealer: 0, player: [0; MAX_PLAYER_CARDS], dealer: [0; MAX_DEALER_CARDS] }
    }

    /// A relayed hand: its bet is placed and its seed is known, so it is dealt as in v1.
    pub const fn new() -> Hand {
        Hand::at(Phase::Deal)
    }

    /// A hand whose bet is placed with the hash of a seed: the dealer owes the opening links.
    pub const fn committed() -> Hand {
        Hand::at(Phase::Open)
    }

    pub fn player_cards(&self) -> &[u8] {
        &self.player[..self.n_player as usize]
    }

    pub fn dealer_cards(&self) -> &[u8] {
        &self.dealer[..self.n_dealer as usize]
    }

    /// The dealer revealed the next opening link. `Some(true)` once all [`OPENING_LINKS`] are out
    /// (the player now owes the seed), `Some(false)` while more are owed, `None` (and nothing
    /// changes) unless the hand is being opened.
    pub fn open_link(&mut self) -> Option<bool> {
        if self.phase != Phase::Open {
            return None;
        }
        self.dealt += 1;
        if self.dealt < OPENING_LINKS {
            return Some(false);
        }
        self.phase = Phase::Seed;
        Some(true)
    }

    /// The player revealed their seed: `cards` are the three the opening links deal under it, in
    /// chain order (the player's, the dealer's upcard, the player's), and they are on the table
    /// at once. What they did: the player's turn, the dealer's hole card against a natural, or
    /// the hand settled (a natural against an upcard that can't match it). `None` (and nothing
    /// changes) unless the hand was opened and waited for its seed.
    pub fn seeded(&mut self, cards: [u8; OPENING_LINKS as usize]) -> Option<Step> {
        if self.phase != Phase::Seed || self.dealt != OPENING_LINKS {
            return None;
        }
        // The deal itself is v1's: the same three calls a v1 dealer's reveal makes.
        let mut v1 = bj_core::Hand::new();
        let mut step = bj_core::Step::More;
        for c in cards {
            step = v1.deal(c)?;
        }
        self.take(&v1);
        Some(Step::from_v1(step))
    }

    /// The player hits: the dealer owes a card. `false` (and nothing changes) unless it's the
    /// player's turn.
    pub fn hit(&mut self) -> bool {
        if self.phase != Phase::Player {
            return false;
        }
        self.phase = Phase::Hit;
        true
    }

    /// The player stands: the dealer plays. `false` (and nothing changes) unless it's the
    /// player's turn.
    pub fn stand(&mut self) -> bool {
        if self.phase != Phase::Player {
            return false;
        }
        self.phase = Phase::Dealer;
        true
    }

    /// The next card off the chain, to whoever it belongs by the hand's phase: v1's rules, run
    /// by v1's code. `None` (and nothing changes) when the dealer owes no card, and while the
    /// hand is being opened (an opening link deals no card until the seed is known:
    /// [`Hand::open_link`]).
    pub fn deal(&mut self, card: u8) -> Option<Step> {
        let mut v1 = self.v1()?;
        let step = v1.deal(card)?;
        self.take(&v1);
        Some(Step::from_v1(step))
    }

    /// How the hand ends if the dealer misses its deadline now. `None` when the dealer owes
    /// nothing (the player's seed or move is awaited, or the hand is over).
    ///
    /// * Being opened: [`Outcome::Refund`]. The dealer has seen no card.
    /// * Otherwise the most the player could still have been paid, from the cards on the table:
    ///   [`Outcome::ForfeitDeal`] (3:2) for a natural in the player's hand, and during a relayed
    ///   hand's deal while the player's first card is unseen or is an ace or a ten-value card;
    ///   else [`Outcome::Forfeit`] (1:1). Never less than the cards could still pay, so a dealer
    ///   never gains by holding back a card it has seen; never more, so a stall is never worth
    ///   more to a player than the hand was.
    pub fn forfeit(&self) -> Option<Outcome> {
        Some(match self.phase {
            Phase::Open => Outcome::Refund,
            // A natural needs an ace and a ten-value card: the first card must be one of them.
            Phase::Deal if self.n_player == 0 || may_be_natural(self.player[0]) => Outcome::ForfeitDeal,
            Phase::Deal => Outcome::Forfeit,
            Phase::Hit | Phase::Dealer if natural(self.player_cards()) => Outcome::ForfeitDeal,
            Phase::Hit | Phase::Dealer => Outcome::Forfeit,
            Phase::Seed | Phase::Player | Phase::Done => return None,
        })
    }

    fn v1(&self) -> Option<bj_core::Hand> {
        Some(bj_core::Hand { phase: self.phase.v1()?, dealt: self.dealt, n_player: self.n_player, n_dealer: self.n_dealer, player: self.player, dealer: self.dealer })
    }

    fn take(&mut self, v1: &bj_core::Hand) {
        self.phase = Phase::from_v1(v1.phase);
        self.dealt = v1.dealt;
        self.n_player = v1.n_player;
        self.n_dealer = v1.n_dealer;
        self.player = v1.player;
        self.dealer = v1.dealer;
    }
}
