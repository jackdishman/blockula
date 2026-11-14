//! # bj-core: the rules of the `blackjack` guest
//!
//! Everything about a hand that isn't storage or FANG: how a card comes out of the dealer's hash
//! chain, what a hand totals, when the dealer draws, who wins and what a stake pays. The guest
//! (`../blackjack`) runs this code on the chain; `node/vm/tests/blackjack_rules.rs` runs the same
//! code on the host against an independent reference, and writes the vectors the TypeScript side
//! checks (`sdk-ts/test/blackjack-vectors.json`).
//!
//! **Rules v1.** One hand against the dealer. An infinite deck: every card is drawn from all 52
//! with equal chance, whatever came before. The dealer stands on every 17, soft ones included. A
//! natural (an ace and a ten-value card as the first two cards) pays 3:2. No split, no double, no
//! insurance, no surrender.
//!
//! **The hash chain.** For game `k` the dealer draws a secret `s_N` (`N` = [`CHAIN_LEN`]) and
//! hashes down: `s_i = BLAKE3(s_{i+1})`. It commits `c = s_0` before any bet. Card `j` (from 1) is
//! dealt by revealing `s_j`, which must hash to `s_{j-1}` ([`prev_link`]), so the dealer can only
//! ever reveal the one chain it committed to. The card is [`card`]:
//!
//! ```text
//! card_j = u64_le(BLAKE3(s_j ‖ client_seed ‖ game_id (8, LE) ‖ j (1))[..8]) mod 52
//! ```
//!
//! The player picks `client_seed` after the commitment is fixed and before any link is revealed,
//! so neither side chooses a card. A bet from an account fixes the seed in two steps: the bet
//! carries only [`seed_hash`], which takes the commitment, and the seed itself follows once the
//! bet is final, so nobody who sees the seed can still change which commitment it meets.
//! **The bias.** 2^64 isn't a multiple of 52: the first
//! 2^64 mod 52 = 16 card numbers are each more likely than the others by one part in 2^64 / 52,
//! about 3 × 10^-18. No rejection loop is worth that.
//!
//! **Order.** Card 1 is the player's, card 2 the dealer's upcard, card 3 the player's. Then every
//! hit takes the next link. The dealer's hole card is the first link after the player's last
//! card, revealed only once the player stands: nobody, the chain included, can read it before.
//!
//! A card is a number from 0 to 51: `card % 13` is its rank (0 ace, 1–9 the cards 2–10, 10 jack,
//! 11 queen, 12 king) and `card / 13` its suit (0 spades, 1 hearts, 2 diamonds, 3 clubs).
//!
//! **BLAKE3 here.** Protocol v2 has no hash host function, so [`blake3`] is BLAKE3 for inputs of
//! one chunk (at most 1,024 bytes), written out from the specification: no dependency, no
//! allocation, no SIMD. The tests hold it to the `blake3` crate for every length it takes.

#![no_std]

/// Links a chain holds after its commitment: more than any hand can use
/// ([`MAX_PLAYER_CARDS`] + [`MAX_DEALER_CARDS`]).
pub const CHAIN_LEN: usize = 32;
/// The most cards a player can hold: ten aces (soft 20), a two (hard 12), eight aces (hard 20)
/// and one more card. A hand that totals 21 stands by itself, so nothing longer exists.
pub const MAX_PLAYER_CARDS: usize = 20;
/// The most cards the dealer can hold: six aces (soft 16), a six (hard 12), four aces (hard 16)
/// and one more card.
pub const MAX_DEALER_CARDS: usize = 12;
/// Cards in the deck every draw is made from.
pub const DECK: u64 = 52;
/// The dealer draws below this total and stands from it on, soft or hard.
pub const DEALER_STANDS_ON: u8 = 17;

// ---------------------------------------------------------------------------------- BLAKE3

const IV: [u32; 8] = [0x6A09_E667, 0xBB67_AE85, 0x3C6E_F372, 0xA54F_F53A, 0x510E_527F, 0x9B05_688C, 0x1F83_D9AB, 0x5BE0_CD19];
const PERMUTATION: [u8; 16] = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];
const CHUNK_START: u32 = 1;
const CHUNK_END: u32 = 2;
const ROOT: u32 = 8;
/// The longest input [`blake3`] takes: one chunk.
pub const MAX_HASH_INPUT: usize = 1024;

/// Which message word each round's sixteen inputs are: row `r` is the permutation applied `r`
/// times. A table, so a round reads its words in place instead of shuffling the block.
const SCHEDULE: [[u8; 16]; 7] = {
    let mut rows = [[0u8; 16]; 7];
    let mut i = 0;
    while i < 16 {
        rows[0][i] = i as u8;
        i += 1;
    }
    let mut r = 1;
    while r < 7 {
        let mut j = 0;
        while j < 16 {
            rows[r][j] = rows[r - 1][PERMUTATION[j] as usize];
            j += 1;
        }
        r += 1;
    }
    rows
};

/// The quarter-round, on four of the state's words by name: the sixteen words are locals, never
/// an array in memory (a memory load costs the program several times a local's read).
macro_rules! g {
    ($a:ident, $b:ident, $c:ident, $d:ident, $x:expr, $y:expr) => {
        $a = $a.wrapping_add($b).wrapping_add($x);
        $d = ($d ^ $a).rotate_right(16);
        $c = $c.wrapping_add($d);
        $b = ($b ^ $c).rotate_right(12);
        $a = $a.wrapping_add($b).wrapping_add($y);
        $d = ($d ^ $a).rotate_right(8);
        $c = $c.wrapping_add($d);
        $b = ($b ^ $c).rotate_right(7);
    };
}

/// BLAKE3's compression function for a block of the one chunk this module hashes (its counter is
/// 0): the new chaining value.
fn compress(cv: &[u32; 8], block: &[u8; 64], len: u32, flags: u32) -> [u32; 8] {
    let mut m = [0u32; 16];
    let mut i = 0;
    while i < 16 {
        m[i] = u32::from_le_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        i += 1;
    }
    let [mut v0, mut v1, mut v2, mut v3, mut v4, mut v5, mut v6, mut v7] = *cv;
    let [mut v8, mut v9, mut v10, mut v11] = [IV[0], IV[1], IV[2], IV[3]];
    let [mut v12, mut v13, mut v14, mut v15] = [0u32, 0, len, flags];
    let mut round = 0;
    while round < 7 {
        let s = &SCHEDULE[round];
        // `& 15`: every entry is below 16, and the mask lets the compiler see it.
        let w = |i: usize| m[(s[i] & 15) as usize];
        g!(v0, v4, v8, v12, w(0), w(1));
        g!(v1, v5, v9, v13, w(2), w(3));
        g!(v2, v6, v10, v14, w(4), w(5));
        g!(v3, v7, v11, v15, w(6), w(7));
        g!(v0, v5, v10, v15, w(8), w(9));
        g!(v1, v6, v11, v12, w(10), w(11));
        g!(v2, v7, v8, v13, w(12), w(13));
        g!(v3, v4, v9, v14, w(14), w(15));
        round += 1;
    }
    [v0 ^ v8, v1 ^ v9, v2 ^ v10, v3 ^ v11, v4 ^ v12, v5 ^ v13, v6 ^ v14, v7 ^ v15]
}

/// The BLAKE3 hash of `data`, which must be at most [`MAX_HASH_INPUT`] bytes (one chunk). Panics
/// on anything longer: every input this crate hashes is 32, 73 or 87 bytes.
pub fn blake3(data: &[u8]) -> [u8; 32] {
    assert!(data.len() <= MAX_HASH_INPUT);
    let mut cv = IV;
    let mut at = 0;
    let mut flags = CHUNK_START;
    loop {
        let rest = data.len() - at;
        let take = if rest > 64 { 64 } else { rest };
        let mut block = [0u8; 64];
        block[..take].copy_from_slice(&data[at..at + take]);
        at += take;
        if at == data.len() {
            cv = compress(&cv, &block, take as u32, flags | CHUNK_END | ROOT);
            break;
        }
        cv = compress(&cv, &block, 64, flags);
        flags = 0;
    }
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 8 {
        out[4 * i..4 * i + 4].copy_from_slice(&cv[i].to_le_bytes());
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------------- the chain

/// The link before `link` in a chain: `s_{j-1} = BLAKE3(s_j)`. A revealed link is the chain's
/// next one exactly when this is the last link revealed (the commitment, for the first).
pub fn prev_link(link: &[u8; 32]) -> [u8; 32] {
    blake3(link)
}

/// Bytes [`card`] hashes.
pub const CARD_PREIMAGE: usize = 73;

/// Card `j` (from 1) of game `game_id`, from the chain's link `s_j` and the player's seed: see
/// the module documentation for the formula and its bias.
pub fn card(link: &[u8; 32], client_seed: &[u8; 32], game_id: u64, j: u8) -> u8 {
    let mut pre = [0u8; CARD_PREIMAGE];
    pre[..32].copy_from_slice(link);
    pre[32..64].copy_from_slice(client_seed);
    pre[64..72].copy_from_slice(&game_id.to_le_bytes());
    pre[72] = j;
    let h = blake3(&pre);
    let n = u64::from_le_bytes([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]]);
    (n % DECK) as u8
}

/// What a seed's hash starts with, before the player and the seed: no other hash this crate
/// makes is of 87 bytes, or starts with these.
pub const SEED_DOMAIN: &[u8; 23] = b"blockula/blackjack/seed";

/// Bytes [`seed_hash`] hashes.
pub const SEED_PREIMAGE: usize = 87;

/// What a bet commits to: `BLAKE3("blockula/blackjack/seed" ‖ player (32) ‖ client_seed (32))`.
/// The player's account is in it, so a hash copied from someone else's bet can never be opened by
/// the copier: the seed that opens it under their account would be a different one.
pub fn seed_hash(player: &[u8; 32], client_seed: &[u8; 32]) -> [u8; 32] {
    let mut pre = [0u8; SEED_PREIMAGE];
    pre[..23].copy_from_slice(SEED_DOMAIN);
    pre[23..55].copy_from_slice(player);
    pre[55..].copy_from_slice(client_seed);
    blake3(&pre)
}

// ---------------------------------------------------------------------------------- hands

/// A card's rank: 0 ace, 1–9 the cards 2–10, 10 jack, 11 queen, 12 king.
pub const fn rank(card: u8) -> u8 {
    card % 13
}

/// What a card counts for: an ace 1 (a hand may count one ace as 11, [`total`]), a face card 10.
pub const fn points(card: u8) -> u8 {
    match rank(card) {
        r @ 0..=8 => r + 1,
        _ => 10,
    }
}

/// A hand's best total and whether it's soft (an ace counted as 11): the highest total that
/// isn't over 21, or the hard total when every one is.
pub fn total(cards: &[u8]) -> (u8, bool) {
    let mut hard = 0u8;
    let mut ace = false;
    for &c in cards {
        hard += points(c);
        ace |= rank(c) == 0;
    }
    if ace && hard + 10 <= 21 { (hard + 10, true) } else { (hard, false) }
}

/// A natural: 21 in the first two cards.
pub fn natural(cards: &[u8]) -> bool {
    cards.len() == 2 && total(cards).0 == 21
}

/// Whether a dealer showing `upcard` could still hold a natural: an ace or a ten-value card.
pub const fn may_be_natural(upcard: u8) -> bool {
    matches!(points(upcard), 1 | 10)
}

/// How a hand ended. The numbers are the program's event and view encoding.
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
    /// The dealer missed a reveal once the player's cards ruled a natural out: the player is paid
    /// as a win, 1:1, the most that hand could still have paid.
    Forfeit = 6,
    /// The dealer missed a reveal while a natural was still possible (the deal) or held (its hole
    /// card against one): the player is paid 3:2. Either way a forfeit pays the most the hand
    /// could have, so the dealer never gains by holding back a card it has seen.
    ForfeitDeal = 7,
    /// The player never revealed the seed their bet committed to: the stake is the house's. The
    /// hand had no cards, and the player could know none of them, so withholding a seed is never
    /// a way out of a hand that would have lost.
    NoSeed = 8,
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
            _ => return None,
        })
    }
}

/// The house's exposure to a stake, reserved while its hand is open: the most it can lose, a
/// natural's 3:2 (half a drop rounds down, as the payout does).
pub const fn exposure(stake: u64) -> u64 {
    stake + stake / 2
}

/// What the player gets back from a hand of `stake` drops that ended in `outcome`, the stake
/// included: 2.5 × for a natural (half a drop rounds down), 2 × for a win, the stake for a push,
/// nothing for a loss. Never more than `stake + exposure(stake)`.
pub const fn payout(stake: u64, outcome: Outcome) -> u64 {
    match outcome {
        Outcome::Blackjack | Outcome::ForfeitDeal => stake + exposure(stake),
        Outcome::Win | Outcome::Forfeit => stake + stake,
        Outcome::Push => stake,
        Outcome::Lose | Outcome::Bust | Outcome::NoSeed => 0,
    }
}

/// Final hands compared. The dealer's hand is as far as it was played: one card when the player
/// went over or held a natural the upcard couldn't match, otherwise two or more.
pub fn showdown(player: &[u8], dealer: &[u8]) -> Outcome {
    let (p, _) = total(player);
    let (d, _) = total(dealer);
    if p > 21 {
        Outcome::Bust
    } else if natural(player) {
        if natural(dealer) { Outcome::Push } else { Outcome::Blackjack }
    } else if natural(dealer) {
        Outcome::Lose
    } else if d > 21 || p > d {
        Outcome::Win
    } else if p == d {
        Outcome::Push
    } else {
        Outcome::Lose
    }
}

/// Where a hand is. The numbers are the program's stored state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Phase {
    /// The bet is placed with the hash of the player's seed; the player owes the seed.
    Seed = 0,
    /// The seed is known; the dealer owes the first three cards.
    Deal = 1,
    /// The player's turn: hit or stand.
    Player = 2,
    /// The player hit; the dealer owes one card.
    Hit = 3,
    /// The player stands (or holds 21); the dealer owes its hole card and whatever it draws.
    Dealer = 4,
    /// Settled.
    Done = 5,
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
            _ => return None,
        })
    }

    /// Whether the next move is the dealer's: a link to reveal.
    pub const fn dealers(self) -> bool {
        matches!(self, Phase::Deal | Phase::Hit | Phase::Dealer)
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

/// One hand: the cards on the table and whose move it is. Cards go in through [`Hand::deal`] in
/// chain order; the player's moves through [`Hand::hit`] and [`Hand::stand`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hand {
    pub phase: Phase,
    /// Cards dealt so far, the dealer's and the player's together: the chain index of the last.
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
    /// A hand whose bet is placed, whose seed is known and whose cards aren't dealt.
    pub const fn new() -> Hand {
        Hand { phase: Phase::Deal, dealt: 0, n_player: 0, n_dealer: 0, player: [0; MAX_PLAYER_CARDS], dealer: [0; MAX_DEALER_CARDS] }
    }

    /// A hand whose bet is placed with the hash of a seed the player hasn't revealed yet.
    pub const fn committed() -> Hand {
        Hand { phase: Phase::Seed, dealt: 0, n_player: 0, n_dealer: 0, player: [0; MAX_PLAYER_CARDS], dealer: [0; MAX_DEALER_CARDS] }
    }

    /// The player revealed their seed: the dealer owes the deal. `false` (and nothing changes)
    /// unless the hand was waiting for it.
    pub fn seeded(&mut self) -> bool {
        if self.phase != Phase::Seed {
            return false;
        }
        self.phase = Phase::Deal;
        true
    }

    pub fn player_cards(&self) -> &[u8] {
        &self.player[..self.n_player as usize]
    }

    pub fn dealer_cards(&self) -> &[u8] {
        &self.dealer[..self.n_dealer as usize]
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

    /// The next card off the chain, to whoever it belongs by the hand's phase. `None` (and
    /// nothing changes) when the dealer owes no card: it's the player's turn, or the hand is over.
    pub fn deal(&mut self, card: u8) -> Option<Step> {
        let step = match self.phase {
            Phase::Deal => {
                // Card 2 is the dealer's upcard; cards 1 and 3 are the player's.
                if self.dealt == 1 {
                    self.push_dealer(card);
                } else {
                    self.push_player(card);
                }
                if self.dealt < 3 {
                    Step::More
                } else if !natural(self.player_cards()) {
                    self.phase = Phase::Player;
                    Step::Player
                } else if may_be_natural(self.dealer[0]) {
                    // The dealer's hole card decides between a push and 3:2.
                    self.phase = Phase::Dealer;
                    Step::More
                } else {
                    self.settle()
                }
            }
            Phase::Hit => {
                self.push_player(card);
                let (t, _) = total(self.player_cards());
                if t > 21 {
                    self.settle()
                } else if t == 21 {
                    self.phase = Phase::Dealer;
                    Step::More
                } else {
                    self.phase = Phase::Player;
                    Step::Player
                }
            }
            Phase::Dealer => {
                self.push_dealer(card);
                // Against a natural the dealer turns its hole card and draws no more.
                if natural(self.player_cards()) || total(self.dealer_cards()).0 >= DEALER_STANDS_ON { self.settle() } else { Step::More }
            }
            Phase::Seed | Phase::Player | Phase::Done => return None,
        };
        Some(step)
    }

    /// How the hand ends if the dealer misses its deadline now ([`Outcome::Forfeit`],
    /// [`Outcome::ForfeitDeal`]): the most the player could still have been paid.
    pub fn forfeit(&self) -> Outcome {
        if self.phase == Phase::Deal || natural(self.player_cards()) { Outcome::ForfeitDeal } else { Outcome::Forfeit }
    }

    fn push_player(&mut self, card: u8) {
        self.player[self.n_player as usize] = card;
        self.n_player += 1;
        self.dealt += 1;
    }

    fn push_dealer(&mut self, card: u8) {
        self.dealer[self.n_dealer as usize] = card;
        self.n_dealer += 1;
        self.dealt += 1;
    }

    fn settle(&mut self) -> Step {
        self.phase = Phase::Done;
        Step::Settled(showdown(self.player_cards(), self.dealer_cards()))
    }
}
