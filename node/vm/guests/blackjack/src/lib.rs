//! # blackjack: one hand against a house bankroll, dealt from the dealer's committed hash chains
//!
//! A game played with FANG (docs/BLACKJACK.md). The rules are `bj-core`'s: one hand, an infinite
//! deck, the dealer stands on every 17, a natural pays 3:2, no split, double or insurance. This
//! crate is the money and the turns around them. It imports only protocol v2's host functions:
//! BLAKE3 is compiled in (`bj_core::blake3`), so it uploads on today's chains.
//!
//! **Who is who.** The *owner* is the account that created the program (set by `init`): it sets
//! the limits, pauses new bets, names the dealer and the relayers, and takes free bankroll out.
//! The *dealer* is the account that commits hash chains ahead of the bets (`commit_batch`). A
//! *player* signs `bet`, `hit` and `stand` with their own account. A *relayer* is an account the
//! owner approved and a player then granted (`set_relayer`): it may play that player's **table
//! balance** for them within the grant's limits, and can never move it anywhere but back to
//! the player.
//!
//! **Fairness.** For each future game `k` the dealer draws a secret, hashes it down
//! [`CHAIN_LEN`] times and commits the last hash, `c_k` (`commit_batch`). A bet takes the next
//! unused commitment in order; the game's id is `k`. Card `j` is dealt by revealing the chain's
//! link `s_j` (`reveal`), which must hash to the link before it, and is
//! `BLAKE3(s_j ‖ client_seed ‖ k ‖ j) mod 52` (`bj_core::card`). The dealer's hole card is the
//! link after the player's last card: nobody can read it before the player stands. Anyone may
//! submit a link (knowing it is the authorization); only the dealer knows them.
//!
//! **A bet is two transactions.** `bet` carries the stake and `seed_hash =
//! BLAKE3("blockula/blackjack/seed" ‖ player ‖ client_seed)` (`bj_core::seed_hash`), never the
//! seed. It takes the commitment, reserves the exposure and counts against every limit. Once it
//! is final the player sends `reveal_seed` with the seed, the program checks it against the hash,
//! and only then does the dealer owe the deal. So when the seed becomes public the hand already
//! holds its commitment: nobody who sees a seed (in the mempool, or after) can put another bet in
//! front of it, drop the commitment it would have met, or pick a different one. The hash binds
//! the player's account, so a hash copied into someone else's bet can't be opened by them. A
//! player who never reveals has `seed_blocks`; past that anyone calls `timeout` and the stake is
//! the house's (outcome 8). That is the right end: no link of the chain is public before the
//! seed is, so the player knows no card and gains nothing by withholding, and a hand that could
//! be abandoned for free would let anyone burn the dealer's commitments and hold seats at no
//! cost.
//!
//! A relayed bet stays one transaction. Its seed isn't revealed by anyone: it is
//! `BLAKE3("blockula/blackjack/relay" ‖ player ‖ post id)`. The relayer chooses when to send it
//! and which post id to give, so relayed play trusts the relayer (docs/BLACKJACK.md, section 2).
//!
//! **Deadlines.** A player has `seed_blocks` from the bet to reveal the seed; a seed that arrives
//! later is refused. Whenever the dealer owes a card it has `reveal_blocks` to reveal it, counted
//! from the seed's reveal (a relayed bet: the bet), the hit or the stand (a partial reveal doesn't
//! extend it). Past that anyone calls `timeout` and **the player wins**: 3:2 if a natural was
//! still possible or held, else 1:1, which is the most the hand could still have paid, so the
//! dealer never gains by holding back a card it has seen. A player who doesn't move for
//! `idle_blocks` stands by `timeout`, and the dealer finishes the hand.
//!
//! **The money.** Every drop the program holds is in exactly one of four counters:
//!
//! ```text
//! balance = bankroll + reserved + stakes + table
//! ```
//!
//! `bankroll` is the house's free FANG; `reserved` is the open hands' exposure (1.5 × each
//! stake, moved out of `bankroll` at the bet); `stakes` is the open hands' stakes; `table` is the
//! players' table balances. A bet is refused unless its exposure fits the free bankroll, so every
//! hand is paid from FANG set aside for it. The program is created without `accepts_transfers`,
//! so nothing arrives outside a method and the equation is exact (`house` returns all five
//! numbers; the tests check it after every step).
//!
//! **Storage** (all integers little-endian):
//! - `b"H"` → the house (114 bytes): owner 32 ‖ dealer 32 ‖ bankroll 8 ‖ reserved 8 ‖ stakes 8 ‖
//!   table 8 ‖ next commitment 8 ‖ next game 8 ‖ paused 1 ‖ open hands 1.
//! - `b"C"` → the limits (104 bytes): thirteen `u64`s, in `set_limits`' order.
//! - `b"O"` → the open hands' ids (256 bytes): 32 slots of 8, the first `open hands` in use.
//! - `b"c" ‖ k (8)` → commitment `c_k` (32 bytes), removed when a bet takes it.
//! - `b"g" ‖ id (8)` → a hand (157 bytes): player 32 ‖ stake 8 ‖ client seed 32 (the seed's hash
//!   while the phase is 0) ‖ newest link 32 ‖ deadline 8 ‖ post id 8 ‖ phase 1 (0 the player owes
//!   the seed, 1 the deal, 2 the player's move, 3 a hit's card, 4 the dealer's hand) ‖ flags 1
//!   (bit 0: played from the table balance) ‖ cards dealt 1 ‖ player's cards 1 ‖ dealer's cards 1
//!   ‖ the player's cards 20 ‖ the dealer's 12. Removed when the hand settles, so its deposit goes
//!   back to whoever placed the bet.
//! - `b"p" ‖ account (32)` → a player (195 bytes): table balance 8 ‖ day 8 ‖ wagered that day 8 ‖
//!   open hand's id + 1 (0: none) 8 ‖ relayer 32 (zeros: none) ‖ grant: most per bet 8 ‖ most per
//!   day 8 ‖ expires at 8 ‖ day 8 ‖ wagered that day 8 ‖ the last settled hand: id 8 ‖ outcome 1
//!   ‖ cards dealt 1 ‖ player's cards 1 ‖ stake 8 ‖ payout 8 ‖ client seed 32 ‖ newest link 32.
//!   The last hand is enough to recompute every one of its cards (each earlier link is the hash
//!   of the next). For outcome 8 the seed is the hash that was never opened, and the link the
//!   commitment.
//! - `b"r" ‖ account (32)` → `[1]` for a relayer the owner approved.
//!
//! **Methods.** Inputs are Borsh for the interface's types (`tests/programs/blackjack.iface.json`):
//! a `u64` or `fang` is 8 bytes, an `address` or `hash32` 32, a `bool` 1, a `vec` a `u32` count
//! and its items. Views return `bytes` (a `u32` length and the layout below).
//!
//! | Method | Input | Who | Does | Event |
//! |---|---|---|---|---|
//! | `init` (init) | dealer 32 | the creator, once | sets the owner, the dealer and the default limits | none |
//! | `fund` (payable) | — | anyone | adds the value to the bankroll | `fund`: from 32 ‖ amount 8 ‖ bankroll 8 |
//! | `defund` | amount 8 | the owner | pays free bankroll to the owner | `defund`: amount 8 ‖ bankroll 8 |
//! | `set_limits` | 13 × 8 | the owner | min bet, max bet, daily cap, day blocks, max open hands, bankroll divisor, max exposure, reveal blocks, idle blocks, deposit cap, relayed max bet, grant max blocks, seed blocks | `limits`: the 104 bytes |
//! | `pause` | on 1 | the owner | stops (or allows) new bets; nothing else | `paused`: on 1 |
//! | `set_dealer` | dealer 32 | the owner | names the account that commits | `dealer`: 32 |
//! | `approve_relayer` | relayer 32 ‖ on 1 | the owner | lets players grant this relayer, or ends every grant to it | `relayer`: 32 ‖ on 1 |
//! | `commit_batch` | first 8 ‖ n 4 ‖ n × 32 | the dealer | stores commitments `first..first + n` (`first` must be the next one: a repeated batch is refused) | `commit`: first 8 ‖ n × 32 |
//! | `skip_commits` | count 4 | the owner | drops up to `count` unused commitments (a dealer that lost its secrets) | `skipped`: first 8 ‖ count 4 |
//! | `deposit` (payable) | — | a player | adds the value to their table balance | `deposit`: player 32 ‖ amount 8 ‖ balance 8 |
//! | `withdraw` | amount 8 | a player | pays their table balance back to them; never refused for a pause | `withdraw`: player 32 ‖ amount 8 ‖ balance 8 |
//! | `set_relayer` | relayer 32 ‖ max bet 8 ‖ daily cap 8 ‖ expires at 8 | a player | grants an approved relayer their table balance, within limits, until a block | `grant`: player 32 ‖ relayer 32 ‖ max bet 8 ‖ daily cap 8 ‖ expires at 8 |
//! | `revoke` | — | a player | ends their grant | `revoke`: player 32 ‖ relayer 32 |
//! | `bet` (payable) | seed hash 32 | a player | stakes the value on a new hand and commits to a seed; returns its id | `bet` (below) |
//! | `reveal_seed` | game 8 ‖ client seed 32 | the hand's player, within `seed_blocks` of the bet | opens the commitment: the dealer now owes the deal | `seed`: game 8 ‖ player 32 ‖ client seed 32 ‖ deadline 8 |
//! | `relay_bet` | player 32 ‖ amount 8 ‖ post id 8 | the player's relayer | a bet from the table balance, its seed derived (no reveal); returns the id | `bet`, `relayed` |
//! | `hit`, `stand` | game 8 | the hand's player | moves | `hit` / `stand`: game 8 ‖ player 32 ‖ deadline 8 (‖ how 1 for `stand`: 0 the player, 1 idle) |
//! | `relay_hit`, `relay_stand` | game 8 ‖ post id 8 | the player's relayer, on a table hand | the same | the move's, `relayed` |
//! | `relay_withdraw` | player 32 ‖ post id 8 | the player's relayer | pays the player's whole table balance to the player | `withdraw`, `relayed` |
//! | `reveal` | game 8 ‖ n 4 ‖ n × 32 | anyone who knows the links | deals the next cards: the links in order, passing over any the hand already holds (a call someone got ahead of still deals the rest; one with nothing new changes nothing); returns phase 1 ‖ outcome 1 | `cards` (its `n` counts the links this call revealed), `settled` |
//! | `timeout` | game 8 | anyone, past the deadline | stands an idle player, pays the player when the dealer missed a reveal, or ends a hand whose seed never came (the stake is the house's); returns phase 1 ‖ outcome 1 | `stand` or `settled` |
//! | `game` (view) | game 8 | | the hand's record ‖ player's total 1 ‖ dealer's total 1 | |
//! | `player` (view) | account 32 | | the player's record (zeros if none) ‖ the height 8 | |
//! | `house` (view) | — | | the house's record ‖ the program's balance 8 ‖ the height 8 | |
//! | `limits` (view) | — | | the 104 bytes | |
//! | `open_games` (view) | — | | a `vec<u64>` of the open hands' ids | |
//! | `commitment` (view) | k 8 | | commitment `c_k`, while no bet has taken it | |
//!
//! **Limits the owner can't set.** Besides amounts within the supply and block counts from 1 to
//! 2^40: `idle_blocks` and `seed_blocks` are at least 20 and `reveal_blocks` at most 86,400. The
//! owner sets the limits while hands are open and the dealer knows their cards, so a player is
//! never given too little time to move or to reveal a seed, and the dealer never so much that a
//! stall has no end.
//!
//! **Events of a hand.**
//! - `bet`: game 8 ‖ player 32 ‖ stake 8 ‖ seed hash 32 (a relayed bet: its client seed) ‖
//!   commitment 32 ‖ deadline 8 ‖ flags 1 ‖ post id 8 (129).
//! - `seed`: game 8 ‖ player 32 ‖ client seed 32 ‖ deadline 8 (80).
//! - `cards`: game 8 ‖ player 32 ‖ first card's index 1 ‖ n 1 ‖ n × (link 32 ‖ card 1) ‖ phase 1 ‖
//!   player's total 1 ‖ dealer's total 1 ‖ deadline 8.
//! - `settled`: game 8 ‖ player 32 ‖ outcome 1 ‖ stake 8 ‖ payout 8 ‖ paid to 1 (0 the account,
//!   1 the table balance) ‖ cards dealt 1 ‖ player's cards 1 ‖ client seed 32 ‖ newest link 32 ‖
//!   player's total 1 ‖ dealer's total 1 (126).
//! - `relayed`: player 32 ‖ relayer 32 ‖ post id 8 ‖ action 1 (1 bet, 2 hit, 3 stand, 4 withdraw)
//!   ‖ game 8 (81; the game is 0 for a withdrawal).
//!
//! **Aborts.** 1 `BAD_INPUT` (a wrong length, a count out of range), 2 `NOT_PAYABLE`, and:
//! 40 already initialized, 41 not initialized, 42 not the owner, 43 not the dealer, 44 no value,
//! 45 bad limits, 46 paused, 47 bet too small, 48 bet too large (the max bet, or the bankroll's
//! share), 49 the bankroll can't cover it, 50 past the daily cap, 51 the table is full, 52 a hand
//! is already open, 53 no commitment ready, 54 no such hand, 55 not the player, 56 not the
//! player's turn, 57 the dealer owes no card, 58 the link isn't the chain's next, 59 more links
//! than the hand takes, 60 not past the deadline, 61 bad batch, 62 past the deposit cap,
//! 63 balance short, 64 not the relayer, 65 the grant expired, 66 past the grant's limits,
//! 67 bad grant, 68 not a table hand, 69 past the max exposure, 70 the hand isn't waiting for a
//! seed, 71 not the seed the bet committed to, 72 past the seed's deadline.
//!
//! **Paying out.** Effects come before transfers. A hand bet from an account pays that account;
//! if that transfer fails (an account that can't receive), the payout is credited to the
//! player's table balance instead, so a hand always settles and `withdraw` is the way out. A
//! table hand pays the table balance.

#![no_std]

use core::mem::MaybeUninit;

use abi::{Address, OK, abort, addr_at, block_height, caller, code, emit, get, input, input_into, nonpayable, pay, put, remove, ret, self_balance, transfer, u64_at, value};
use bj_core::{CHAIN_LEN, Hand, MAX_DEALER_CARDS, MAX_PLAYER_CARDS, Outcome, Phase, Step, card, exposure, payout, prev_link, seed_hash, total};

// Abort codes.
const ALREADY_INITIALIZED: u32 = 40;
const NOT_INITIALIZED: u32 = 41;
const NOT_OWNER: u32 = 42;
const NOT_DEALER: u32 = 43;
const NO_VALUE: u32 = 44;
const BAD_LIMITS: u32 = 45;
const PAUSED: u32 = 46;
const BET_TOO_SMALL: u32 = 47;
const BET_TOO_LARGE: u32 = 48;
const BANKROLL_SHORT: u32 = 49;
const DAILY_CAP: u32 = 50;
const TABLE_FULL: u32 = 51;
const HAND_OPEN: u32 = 52;
const NO_COMMITMENT: u32 = 53;
const NO_GAME: u32 = 54;
const NOT_PLAYER: u32 = 55;
const NOT_YOUR_TURN: u32 = 56;
const NOT_DEALERS_TURN: u32 = 57;
const BAD_LINK: u32 = 58;
const TOO_MANY_LINKS: u32 = 59;
const NOT_YET: u32 = 60;
const BAD_BATCH: u32 = 61;
const DEPOSIT_CAP: u32 = 62;
const BALANCE_SHORT: u32 = 63;
const NOT_RELAYER: u32 = 64;
const GRANT_EXPIRED: u32 = 65;
const GRANT_LIMIT: u32 = 66;
const BAD_GRANT: u32 = 67;
const NOT_TABLE_GAME: u32 = 68;
const MAX_EXPOSURE: u32 = 69;
const NO_SEED_OWED: u32 = 70;
const BAD_SEED: u32 = 71;
const SEED_LATE: u32 = 72;

const FANG: u64 = 1_000_000_000;
/// The chain's supply cap: no amount is larger, so sums of a few never overflow.
const MAX_SUPPLY: u64 = 1_000_000_000 * FANG;
/// The most blocks any limit counts: heights plus it stay far inside a `u64`.
const MAX_BLOCKS: u64 = 1 << 40;
/// The fewest blocks a player may be given to reveal a seed or to move. The owner sets the
/// limits and the dealer knows every card, so without a floor an owner could give a player one
/// block to move (and stand them by `timeout` before a hit it didn't like) or one block to
/// reveal a seed (and keep the stake).
const MIN_PLAYER_BLOCKS: u64 = 20;
/// The most blocks the dealer may be given to reveal a card. A hand waits that long before its
/// player can claim it, so without a ceiling an owner could turn a stall into a hand that never
/// ends.
const MAX_DEALER_BLOCKS: u64 = 86_400;

// The house record.
const HOUSE_KEY: &[u8] = b"H";
const HOUSE: usize = 114;
const H_OWNER: usize = 0;
const H_DEALER: usize = 32;
const H_BANKROLL: usize = 64;
const H_RESERVED: usize = 72;
const H_STAKES: usize = 80;
const H_TABLE: usize = 88;
const H_NEXT_COMMIT: usize = 96;
const H_NEXT_GAME: usize = 104;
const H_PAUSED: usize = 112;
const H_OPEN: usize = 113;

// The limits, each a u64.
const LIMITS_KEY: &[u8] = b"C";
const LIMITS: usize = 104;
const L_MIN_BET: usize = 0;
const L_MAX_BET: usize = 8;
const L_DAILY_CAP: usize = 16;
const L_DAY_BLOCKS: usize = 24;
const L_MAX_OPEN: usize = 32;
const L_DIVISOR: usize = 40;
const L_MAX_EXPOSURE: usize = 48;
const L_REVEAL_BLOCKS: usize = 56;
const L_IDLE_BLOCKS: usize = 64;
const L_DEPOSIT_CAP: usize = 72;
const L_RELAY_MAX_BET: usize = 80;
const L_GRANT_BLOCKS: usize = 88;
const L_SEED_BLOCKS: usize = 96;

/// The defaults `init` writes: 1 to 50 FANG a bet, 500 FANG a day (43,200 blocks: a day of
/// 2-second blocks), 10 open hands, a bet at most a twentieth of the free bankroll, 750 FANG of
/// exposure in all, 150 blocks to reveal, 1,800 to move, 200 FANG of table balance, 25 FANG a
/// relayed bet, grants of at most 43,200 blocks, 300 blocks to reveal a bet's seed.
const DEFAULT_LIMITS: [u64; 13] = [FANG, 50 * FANG, 500 * FANG, 43_200, 10, 20, 750 * FANG, 150, 1_800, 200 * FANG, 25 * FANG, 43_200, 300];

// The open hands' ids.
const OPEN_KEY: &[u8] = b"O";
const MAX_OPEN: usize = 32;
const OPEN: usize = 8 * MAX_OPEN;

// A hand.
const GAME: usize = 125 + MAX_PLAYER_CARDS + MAX_DEALER_CARDS;
const G_PLAYER: usize = 0;
const G_STAKE: usize = 32;
const G_SEED: usize = 40;
const G_LINK: usize = 72;
const G_DEADLINE: usize = 104;
const G_POST: usize = 112;
const G_PHASE: usize = 120;
const G_FLAGS: usize = 121;
const G_DEALT: usize = 122;
const G_N_PLAYER: usize = 123;
const G_N_DEALER: usize = 124;
const G_CARDS: usize = 125;
/// `flags` bit 0: the stake came from the player's table balance, and the payout goes back there.
const FLAG_TABLE: u8 = 1;

// A player.
const PLAYER: usize = 195;
const P_BALANCE: usize = 0;
const P_DAY: usize = 8;
const P_WAGERED: usize = 16;
const P_OPEN: usize = 24;
const P_RELAYER: usize = 32;
const P_GRANT_MAX_BET: usize = 64;
const P_GRANT_DAILY: usize = 72;
const P_GRANT_EXPIRES: usize = 80;
const P_GRANT_DAY: usize = 88;
const P_GRANT_WAGERED: usize = 96;
const P_LAST_GAME: usize = 104;
const P_LAST_OUTCOME: usize = 112;
const P_LAST_DEALT: usize = 113;
const P_LAST_N_PLAYER: usize = 114;
const P_LAST_STAKE: usize = 115;
const P_LAST_PAYOUT: usize = 123;
const P_LAST_SEED: usize = 131;
const P_LAST_LINK: usize = 163;

/// The most links one `reveal` carries: a hit to 21 and the dealer's whole hand after it.
const MAX_REVEAL: usize = 12;
/// The most commitments one `commit_batch` carries.
const MAX_BATCH: usize = 64;
/// What a relayed bet's client seed is hashed from, before the player and the post id.
const RELAY_DOMAIN: &[u8; 24] = b"blockula/blackjack/relay";

// `relayed`'s action.
const ACT_BET: u8 = 1;
const ACT_HIT: u8 = 2;
const ACT_STAND: u8 = 3;
const ACT_WITHDRAW: u8 = 4;

// ---------------------------------------------------------------------------------- records

fn set(b: &mut [u8], at: usize, n: u64) {
    b[at..at + 8].copy_from_slice(&n.to_le_bytes());
}

fn add(b: &mut [u8], at: usize, n: u64) {
    let sum = u64_at(b, at) + n;
    set(b, at, sum);
}

fn sub(b: &mut [u8], at: usize, n: u64) {
    let rest = u64_at(b, at) - n;
    set(b, at, rest);
}

fn key8(prefix: u8, id: u64) -> [u8; 9] {
    let mut k = [prefix; 9];
    k[1..].copy_from_slice(&id.to_le_bytes());
    k
}

fn key32(prefix: u8, account: &Address) -> [u8; 33] {
    let mut k = [prefix; 33];
    k[1..].copy_from_slice(account);
    k
}

fn house() -> [u8; HOUSE] {
    match get::<HOUSE>(HOUSE_KEY) {
        Some(h) => h,
        None => abort(NOT_INITIALIZED, b"not initialized"),
    }
}

fn limits() -> [u8; LIMITS] {
    match get::<LIMITS>(LIMITS_KEY) {
        Some(l) => l,
        None => abort(NOT_INITIALIZED, b"not initialized"),
    }
}

/// The house, for a method only the owner calls.
fn owners_house() -> [u8; HOUSE] {
    let h = house();
    if caller() != addr_at(&h, H_OWNER) {
        abort(NOT_OWNER, b"not the owner");
    }
    h
}

fn player_record(account: &Address) -> [u8; PLAYER] {
    get::<PLAYER>(&key32(b'p', account)).unwrap_or([0; PLAYER])
}

fn put_player(account: &Address, p: &[u8; PLAYER]) {
    put(&key32(b'p', account), p);
}

fn game_record(id: u64) -> [u8; GAME] {
    match get::<GAME>(&key8(b'g', id)) {
        Some(g) => g,
        None => abort(NO_GAME, b"no such hand"),
    }
}

fn hand_of(g: &[u8; GAME]) -> Hand {
    let mut hand = Hand::new();
    hand.phase = match Phase::from_u8(g[G_PHASE]) {
        Some(p) => p,
        None => abort(code::BAD_STORED, b"stored phase"),
    };
    hand.dealt = g[G_DEALT];
    hand.n_player = g[G_N_PLAYER];
    hand.n_dealer = g[G_N_DEALER];
    hand.player.copy_from_slice(&g[G_CARDS..G_CARDS + MAX_PLAYER_CARDS]);
    hand.dealer.copy_from_slice(&g[G_CARDS + MAX_PLAYER_CARDS..]);
    hand
}

fn store_hand(g: &mut [u8; GAME], hand: &Hand) {
    g[G_PHASE] = hand.phase as u8;
    g[G_DEALT] = hand.dealt;
    g[G_N_PLAYER] = hand.n_player;
    g[G_N_DEALER] = hand.n_dealer;
    g[G_CARDS..G_CARDS + MAX_PLAYER_CARDS].copy_from_slice(&hand.player);
    g[G_CARDS + MAX_PLAYER_CARDS..].copy_from_slice(&hand.dealer);
}

/// Returns `data` as Borsh `bytes`: a `u32` length, then the bytes.
fn ret_bytes<const N: usize, const M: usize>(data: &[u8; N]) {
    let mut out = [0u8; M];
    out[..4].copy_from_slice(&(N as u32).to_le_bytes());
    out[4..].copy_from_slice(data);
    ret(&out);
}

/// A variable input `head ‖ n (u32) ‖ n × 32`: the head and the items, with `1 ≤ n ≤ max`.
fn counted<const HEAD: usize>(buf: &mut [MaybeUninit<u8>], max: usize) -> (&[u8], &[u8]) {
    let inp = input_into(buf);
    if inp.len() < HEAD + 4 {
        abort(code::BAD_INPUT, b"input length");
    }
    let n = u32::from_le_bytes([inp[HEAD], inp[HEAD + 1], inp[HEAD + 2], inp[HEAD + 3]]) as usize;
    if n == 0 || n > max || inp.len() != HEAD + 4 + 32 * n {
        abort(code::BAD_INPUT, b"input length");
    }
    (&inp[..HEAD], &inp[HEAD + 4..])
}

// ---------------------------------------------------------------------------------- the owner

#[unsafe(no_mangle)]
pub extern "C" fn bk_init() {
    nonpayable();
    let dealer = input::<32>();
    if get::<HOUSE>(HOUSE_KEY).is_some() {
        abort(ALREADY_INITIALIZED, b"already initialized");
    }
    let mut h = [0u8; HOUSE];
    h[H_OWNER..H_OWNER + 32].copy_from_slice(&caller());
    h[H_DEALER..H_DEALER + 32].copy_from_slice(&dealer);
    put(HOUSE_KEY, &h);
    let mut l = [0u8; LIMITS];
    for (i, n) in DEFAULT_LIMITS.iter().enumerate() {
        set(&mut l, 8 * i, *n);
    }
    put(LIMITS_KEY, &l);
    // At its full size from the start, so no later call bonds for it.
    put(OPEN_KEY, &[0u8; OPEN]);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_fund() {
    input::<0>();
    let amount = value();
    if amount == 0 {
        abort(NO_VALUE, b"no value");
    }
    let mut h = house();
    let bankroll = u64_at(&h, H_BANKROLL) + amount;
    set(&mut h, H_BANKROLL, bankroll);
    put(HOUSE_KEY, &h);

    let mut ev = [0u8; 48];
    ev[..32].copy_from_slice(&caller());
    set(&mut ev, 32, amount);
    set(&mut ev, 40, bankroll);
    emit(b"fund", &ev);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_defund() {
    nonpayable();
    let amount = u64::from_le_bytes(input::<8>());
    let mut h = owners_house();
    let bankroll = u64_at(&h, H_BANKROLL);
    if amount == 0 || amount > bankroll {
        abort(BALANCE_SHORT, b"more than the free bankroll");
    }
    set(&mut h, H_BANKROLL, bankroll - amount);
    put(HOUSE_KEY, &h);
    pay(&addr_at(&h, H_OWNER), amount, 0);

    let mut ev = [0u8; 16];
    set(&mut ev, 0, amount);
    set(&mut ev, 8, bankroll - amount);
    emit(b"defund", &ev);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_set_limits() {
    nonpayable();
    let l = input::<LIMITS>();
    owners_house();
    let at = |i: usize| u64_at(&l, i);
    let amounts_ok = [L_MIN_BET, L_MAX_BET, L_DAILY_CAP, L_MAX_EXPOSURE, L_DEPOSIT_CAP, L_RELAY_MAX_BET].iter().all(|i| at(*i) <= MAX_SUPPLY);
    let blocks_ok = [L_DAY_BLOCKS, L_REVEAL_BLOCKS, L_IDLE_BLOCKS, L_GRANT_BLOCKS, L_SEED_BLOCKS].iter().all(|i| (1..=MAX_BLOCKS).contains(&at(*i)));
    // What a player is given is never under the floor, what the dealer is given never over the
    // ceiling: the owner can't use the limits to take a move or a seed's reveal from a player,
    // or to leave a hand waiting for the dealer for good.
    let turns_ok = at(L_IDLE_BLOCKS) >= MIN_PLAYER_BLOCKS && at(L_SEED_BLOCKS) >= MIN_PLAYER_BLOCKS && at(L_REVEAL_BLOCKS) <= MAX_DEALER_BLOCKS;
    let ok = amounts_ok && blocks_ok && turns_ok && at(L_MIN_BET) >= 1 && at(L_MIN_BET) <= at(L_MAX_BET) && (1..=MAX_OPEN as u64).contains(&at(L_MAX_OPEN)) && (1..=MAX_SUPPLY).contains(&at(L_DIVISOR));
    if !ok {
        abort(BAD_LIMITS, b"bad limits");
    }
    put(LIMITS_KEY, &l);
    emit(b"limits", &l);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_pause() {
    nonpayable();
    let [on] = input::<1>();
    if on > 1 {
        abort(code::BAD_INPUT, b"a bool is 0 or 1");
    }
    let mut h = owners_house();
    h[H_PAUSED] = on;
    put(HOUSE_KEY, &h);
    emit(b"paused", &[on]);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_set_dealer() {
    nonpayable();
    let dealer = input::<32>();
    let mut h = owners_house();
    h[H_DEALER..H_DEALER + 32].copy_from_slice(&dealer);
    put(HOUSE_KEY, &h);
    emit(b"dealer", &dealer);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_approve_relayer() {
    nonpayable();
    let inp = input::<33>();
    if inp[32] > 1 {
        abort(code::BAD_INPUT, b"a bool is 0 or 1");
    }
    owners_house();
    let key = key32(b'r', &addr_at(&inp, 0));
    if inp[32] == 1 {
        put(&key, &[1]);
    } else {
        remove(&key);
    }
    emit(b"relayer", &inp);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_skip_commits() {
    nonpayable();
    let count = u32::from_le_bytes(input::<4>());
    if count == 0 || count as usize > MAX_BATCH {
        abort(BAD_BATCH, b"skip 1 to 64");
    }
    let mut h = owners_house();
    let first = u64_at(&h, H_NEXT_GAME);
    let mut next = first;
    while next < first + count as u64 && next < u64_at(&h, H_NEXT_COMMIT) {
        remove(&key8(b'c', next));
        next += 1;
    }
    set(&mut h, H_NEXT_GAME, next);
    put(HOUSE_KEY, &h);

    let mut ev = [0u8; 12];
    set(&mut ev, 0, first);
    ev[8..].copy_from_slice(&((next - first) as u32).to_le_bytes());
    emit(b"skipped", &ev);
}

// ---------------------------------------------------------------------------------- the dealer

#[unsafe(no_mangle)]
pub extern "C" fn bk_commit_batch() {
    nonpayable();
    let mut buf = [MaybeUninit::<u8>::uninit(); 12 + 32 * MAX_BATCH];
    let (head, commits) = counted::<8>(&mut buf, MAX_BATCH);
    let first = u64_at(head, 0);
    let mut h = house();
    if caller() != addr_at(&h, H_DEALER) {
        abort(NOT_DEALER, b"not the dealer");
    }
    // The batch names where it starts: one sent twice (a retry after a restart) is refused
    // instead of committing the same chains to two runs of games.
    if first != u64_at(&h, H_NEXT_COMMIT) {
        abort(BAD_BATCH, b"not the next commitment");
    }
    let n = commits.len() / 32;
    for i in 0..n {
        put(&key8(b'c', first + i as u64), &commits[32 * i..32 * i + 32]);
    }
    set(&mut h, H_NEXT_COMMIT, first + n as u64);
    put(HOUSE_KEY, &h);

    let mut ev = [0u8; 8 + 32 * MAX_BATCH];
    set(&mut ev, 0, first);
    ev[8..8 + commits.len()].copy_from_slice(commits);
    emit(b"commit", &ev[..8 + commits.len()]);
}

// ---------------------------------------------------------------------------------- table balances

#[unsafe(no_mangle)]
pub extern "C" fn bk_deposit() {
    input::<0>();
    let amount = value();
    if amount == 0 {
        abort(NO_VALUE, b"no value");
    }
    let mut h = house();
    let l = limits();
    let who = caller();
    let mut p = player_record(&who);
    let balance = u64_at(&p, P_BALANCE) + amount;
    if balance > u64_at(&l, L_DEPOSIT_CAP) {
        abort(DEPOSIT_CAP, b"past the deposit cap");
    }
    set(&mut p, P_BALANCE, balance);
    put_player(&who, &p);
    let table = u64_at(&h, H_TABLE) + amount;
    set(&mut h, H_TABLE, table);
    put(HOUSE_KEY, &h);
    emit_balance(b"deposit", &who, amount, balance);
}

fn emit_balance(name: &[u8], who: &Address, amount: u64, balance: u64) {
    let mut ev = [0u8; 48];
    ev[..32].copy_from_slice(who);
    set(&mut ev, 32, amount);
    set(&mut ev, 40, balance);
    emit(name, &ev);
}

/// Pays `amount` of `who`'s table balance to `who`: the only way a table balance leaves.
fn withdraw_to(who: &Address, p: &mut [u8; PLAYER], amount: u64) {
    let mut h = house();
    let balance = u64_at(p, P_BALANCE);
    if amount == 0 || amount > balance {
        abort(BALANCE_SHORT, b"more than the table balance");
    }
    set(p, P_BALANCE, balance - amount);
    put_player(who, p);
    let table = u64_at(&h, H_TABLE) - amount;
    set(&mut h, H_TABLE, table);
    put(HOUSE_KEY, &h);
    pay(who, amount, 0);
    emit_balance(b"withdraw", who, amount, balance - amount);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_withdraw() {
    nonpayable();
    let amount = u64::from_le_bytes(input::<8>());
    let who = caller();
    let mut p = player_record(&who);
    withdraw_to(&who, &mut p, amount);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_set_relayer() {
    nonpayable();
    let inp = input::<56>();
    let relayer = addr_at(&inp, 0);
    let (max_bet, daily_cap, expires_at) = (u64_at(&inp, 32), u64_at(&inp, 40), u64_at(&inp, 48));
    let l = limits();
    if get::<1>(&key32(b'r', &relayer)).is_none() {
        abort(NOT_RELAYER, b"not an approved relayer");
    }
    let now = block_height();
    let ok = max_bet >= 1 && max_bet <= u64_at(&l, L_RELAY_MAX_BET) && daily_cap >= max_bet && daily_cap <= MAX_SUPPLY && expires_at > now && expires_at - now <= u64_at(&l, L_GRANT_BLOCKS);
    if !ok {
        abort(BAD_GRANT, b"bad grant");
    }
    let who = caller();
    let mut p = player_record(&who);
    p[P_RELAYER..P_RELAYER + 32].copy_from_slice(&relayer);
    set(&mut p, P_GRANT_MAX_BET, max_bet);
    set(&mut p, P_GRANT_DAILY, daily_cap);
    set(&mut p, P_GRANT_EXPIRES, expires_at);
    set(&mut p, P_GRANT_DAY, 0);
    set(&mut p, P_GRANT_WAGERED, 0);
    put_player(&who, &p);

    let mut ev = [0u8; 88];
    ev[..32].copy_from_slice(&who);
    ev[32..].copy_from_slice(&inp);
    emit(b"grant", &ev);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_revoke() {
    nonpayable();
    input::<0>();
    let who = caller();
    let mut p = player_record(&who);
    let mut ev = [0u8; 64];
    ev[..32].copy_from_slice(&who);
    ev[32..].copy_from_slice(&p[P_RELAYER..P_RELAYER + 32]);
    p[P_RELAYER..P_LAST_GAME].fill(0);
    put_player(&who, &p);
    emit(b"revoke", &ev);
}

/// Aborts unless the caller is `p`'s relayer, still approved by the owner, with a grant that
/// hasn't expired.
fn live_grant(p: &[u8; PLAYER]) -> Address {
    let who = caller();
    if who == [0; 32] || who != addr_at(p, P_RELAYER) || get::<1>(&key32(b'r', &who)).is_none() {
        abort(NOT_RELAYER, b"not the player's relayer");
    }
    if block_height() > u64_at(p, P_GRANT_EXPIRES) {
        abort(GRANT_EXPIRED, b"the grant expired");
    }
    who
}

fn emit_relayed(player: &Address, relayer: &Address, post: u64, action: u8, game: u64) {
    let mut ev = [0u8; 81];
    ev[..32].copy_from_slice(player);
    ev[32..64].copy_from_slice(relayer);
    set(&mut ev, 64, post);
    ev[72] = action;
    set(&mut ev, 73, game);
    emit(b"relayed", &ev);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_relay_withdraw() {
    nonpayable();
    let inp = input::<40>();
    let player = addr_at(&inp, 0);
    let mut p = player_record(&player);
    let relayer = live_grant(&p);
    let balance = u64_at(&p, P_BALANCE);
    withdraw_to(&player, &mut p, balance);
    emit_relayed(&player, &relayer, u64_at(&inp, 32), ACT_WITHDRAW, 0);
}

// ---------------------------------------------------------------------------------- a hand

/// Opens a hand of `stake` drops for `player`, whose record is `p`: every limit, the next
/// commitment, the reserve. The stake is already counted in the balance (a bet's value) or was
/// just taken from the table balance (`flags`). `seed` is the client seed when `start` is
/// [`Phase::Deal`] (a relayed bet: the dealer owes the deal), and the seed's hash when it is
/// [`Phase::Seed`] (a bet from an account: the player owes the seed). Returns the hand's id.
#[allow(clippy::too_many_arguments)]
fn open_hand(h: &mut [u8; HOUSE], l: &[u8; LIMITS], player: &Address, p: &mut [u8; PLAYER], stake: u64, seed: &[u8; 32], start: Phase, flags: u8, post: u64) -> u64 {
    if h[H_PAUSED] != 0 {
        abort(PAUSED, b"new bets are paused");
    }
    if stake < u64_at(l, L_MIN_BET) {
        abort(BET_TOO_SMALL, b"below the minimum bet");
    }
    let bankroll = u64_at(h, H_BANKROLL);
    if stake > u64_at(l, L_MAX_BET) || stake > bankroll / u64_at(l, L_DIVISOR) {
        abort(BET_TOO_LARGE, b"above the maximum bet");
    }
    let reserve = exposure(stake);
    if reserve > bankroll {
        abort(BANKROLL_SHORT, b"the bankroll can't cover this bet");
    }
    let reserved = u64_at(h, H_RESERVED) + reserve;
    if reserved > u64_at(l, L_MAX_EXPOSURE) {
        abort(MAX_EXPOSURE, b"past the house's exposure limit");
    }
    let open = h[H_OPEN] as usize;
    if open as u64 >= u64_at(l, L_MAX_OPEN) {
        abort(TABLE_FULL, b"the table is full");
    }
    if u64_at(p, P_OPEN) != 0 {
        abort(HAND_OPEN, b"finish the open hand first");
    }
    let now = block_height();
    let day = now / u64_at(l, L_DAY_BLOCKS);
    let wagered = if u64_at(p, P_DAY) == day { u64_at(p, P_WAGERED) } else { 0 } + stake;
    if wagered > u64_at(l, L_DAILY_CAP) {
        abort(DAILY_CAP, b"past the daily cap");
    }
    // The next unused commitment, in order: the game's id is its index.
    let id = u64_at(h, H_NEXT_GAME);
    let commit_key = key8(b'c', id);
    let commit = match get::<32>(&commit_key) {
        Some(c) if id < u64_at(h, H_NEXT_COMMIT) => c,
        _ => abort(NO_COMMITMENT, b"the dealer has no commitment ready"),
    };
    remove(&commit_key);

    set(h, H_BANKROLL, bankroll - reserve);
    set(h, H_RESERVED, reserved);
    add(h, H_STAKES, stake);
    set(h, H_NEXT_GAME, id + 1);
    h[H_OPEN] = open as u8 + 1;
    put(HOUSE_KEY, h);
    let mut ids = get::<OPEN>(OPEN_KEY).unwrap_or([0; OPEN]);
    set(&mut ids, 8 * open, id);
    put(OPEN_KEY, &ids);

    set(p, P_DAY, day);
    set(p, P_WAGERED, wagered);
    set(p, P_OPEN, id + 1);
    put_player(player, p);

    let (hand, blocks) = if start == Phase::Seed { (Hand::committed(), L_SEED_BLOCKS) } else { (Hand::new(), L_REVEAL_BLOCKS) };
    let deadline = now + u64_at(l, blocks);
    let mut g = [0u8; GAME];
    g[G_PLAYER..G_PLAYER + 32].copy_from_slice(player);
    set(&mut g, G_STAKE, stake);
    g[G_SEED..G_SEED + 32].copy_from_slice(seed);
    g[G_LINK..G_LINK + 32].copy_from_slice(&commit);
    set(&mut g, G_DEADLINE, deadline);
    set(&mut g, G_POST, post);
    g[G_FLAGS] = flags;
    store_hand(&mut g, &hand);
    put(&key8(b'g', id), &g);

    let mut ev = [0u8; 129];
    set(&mut ev, 0, id);
    ev[8..40].copy_from_slice(player);
    set(&mut ev, 40, stake);
    ev[48..80].copy_from_slice(seed);
    ev[80..112].copy_from_slice(&commit);
    set(&mut ev, 112, deadline);
    ev[120] = flags;
    set(&mut ev, 121, post);
    emit(b"bet", &ev);
    id
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_bet() {
    // The hash of the player's seed (`bj_core::seed_hash`), never the seed: it stays private
    // until this hand holds its commitment.
    let hash = input::<32>();
    let stake = value();
    if stake == 0 {
        abort(NO_VALUE, b"no value");
    }
    let mut h = house();
    let l = limits();
    let player = caller();
    let mut p = player_record(&player);
    let id = open_hand(&mut h, &l, &player, &mut p, stake, &hash, Phase::Seed, 0, 0);
    ret(&id.to_le_bytes());
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_reveal_seed() {
    nonpayable();
    let inp = input::<40>();
    let id = u64_at(&inp, 0);
    let seed: [u8; 32] = addr_at(&inp, 8);
    let l = limits();
    let mut g = game_record(id);
    let player = addr_at(&g, G_PLAYER);
    if caller() != player {
        abort(NOT_PLAYER, b"not this hand's player");
    }
    let mut hand = hand_of(&g);
    if !hand.seeded() {
        abort(NO_SEED_OWED, b"the hand isn't waiting for a seed");
    }
    // Past the deadline the hand is the house's, whoever gets there first: a late seed is public
    // in the mempool, and whoever saw it there could otherwise choose between letting it in and
    // calling `timeout`.
    let now = block_height();
    if now > u64_at(&g, G_DEADLINE) {
        abort(SEED_LATE, b"past the seed's deadline");
    }
    if seed_hash(&player, &seed) != g[G_SEED..G_SEED + 32] {
        abort(BAD_SEED, b"not the seed the bet committed to");
    }
    let deadline = now + u64_at(&l, L_REVEAL_BLOCKS);
    g[G_SEED..G_SEED + 32].copy_from_slice(&seed);
    store_hand(&mut g, &hand);
    set(&mut g, G_DEADLINE, deadline);
    put(&key8(b'g', id), &g);

    let mut ev = [0u8; 80];
    set(&mut ev, 0, id);
    ev[8..40].copy_from_slice(&player);
    ev[40..72].copy_from_slice(&seed);
    set(&mut ev, 72, deadline);
    emit(b"seed", &ev);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_relay_bet() {
    nonpayable();
    let inp = input::<48>();
    let player = addr_at(&inp, 0);
    let (stake, post) = (u64_at(&inp, 32), u64_at(&inp, 40));
    let mut h = house();
    let l = limits();
    let mut p = player_record(&player);
    let relayer = live_grant(&p);
    if stake == 0 {
        abort(NO_VALUE, b"no value");
    }
    // The grant's own limits, on top of the table's.
    let day = block_height() / u64_at(&l, L_DAY_BLOCKS);
    let granted = if u64_at(&p, P_GRANT_DAY) == day { u64_at(&p, P_GRANT_WAGERED) } else { 0 } + stake;
    if stake > u64_at(&p, P_GRANT_MAX_BET) || stake > u64_at(&l, L_RELAY_MAX_BET) || granted > u64_at(&p, P_GRANT_DAILY) {
        abort(GRANT_LIMIT, b"past the grant's limits");
    }
    let balance = u64_at(&p, P_BALANCE);
    if stake > balance {
        abort(BALANCE_SHORT, b"more than the table balance");
    }
    set(&mut p, P_GRANT_DAY, day);
    set(&mut p, P_GRANT_WAGERED, granted);
    // The stake moves from the table balance to the hand.
    set(&mut p, P_BALANCE, balance - stake);
    sub(&mut h, H_TABLE, stake);
    // The seed is the post's, not the relayer's choice.
    let mut pre = [0u8; 64];
    pre[..24].copy_from_slice(RELAY_DOMAIN);
    pre[24..56].copy_from_slice(&player);
    set(&mut pre, 56, post);
    let seed = bj_core::blake3(&pre);
    let id = open_hand(&mut h, &l, &player, &mut p, stake, &seed, Phase::Deal, FLAG_TABLE, post);
    emit_relayed(&player, &relayer, post, ACT_BET, id);
    ret(&id.to_le_bytes());
}

/// The player's move on hand `id`: `hit`, or stand. `relayed` carries the post id of a relayer's.
fn player_move(id: u64, hit: bool, relayed: Option<u64>) {
    let l = limits();
    let key = key8(b'g', id);
    let mut g = game_record(id);
    let player = addr_at(&g, G_PLAYER);
    let relayer = match relayed {
        None => {
            if caller() != player {
                abort(NOT_PLAYER, b"not this hand's player");
            }
            None
        }
        Some(_) => {
            if g[G_FLAGS] & FLAG_TABLE == 0 {
                abort(NOT_TABLE_GAME, b"not a table hand");
            }
            Some(live_grant(&player_record(&player)))
        }
    };
    let mut hand = hand_of(&g);
    if !(if hit { hand.hit() } else { hand.stand() }) {
        abort(NOT_YOUR_TURN, b"not the player's turn");
    }
    let deadline = block_height() + u64_at(&l, L_REVEAL_BLOCKS);
    store_hand(&mut g, &hand);
    set(&mut g, G_DEADLINE, deadline);
    put(&key, &g);
    emit_move(id, &player, deadline, if hit { None } else { Some(0) });
    if let (Some(relayer), Some(post)) = (relayer, relayed) {
        emit_relayed(&player, &relayer, post, if hit { ACT_HIT } else { ACT_STAND }, id);
    }
}

/// `hit` (no `how`) or `stand` (`how`: 0 the player, 1 idle).
fn emit_move(id: u64, player: &Address, deadline: u64, how: Option<u8>) {
    let mut ev = [0u8; 49];
    set(&mut ev, 0, id);
    ev[8..40].copy_from_slice(player);
    set(&mut ev, 40, deadline);
    match how {
        None => emit(b"hit", &ev[..48]),
        Some(how) => {
            ev[48] = how;
            emit(b"stand", &ev);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_hit() {
    nonpayable();
    player_move(u64::from_le_bytes(input::<8>()), true, None);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_stand() {
    nonpayable();
    player_move(u64::from_le_bytes(input::<8>()), false, None);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_relay_hit() {
    nonpayable();
    let inp = input::<16>();
    player_move(u64_at(&inp, 0), true, Some(u64_at(&inp, 8)));
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_relay_stand() {
    nonpayable();
    let inp = input::<16>();
    player_move(u64_at(&inp, 0), false, Some(u64_at(&inp, 8)));
}

/// Ends hand `id` (record `g`, cards `hand`) with `outcome`: frees its reserve, removes it, keeps
/// it as the player's last hand and pays what it pays.
fn settle(id: u64, g: &[u8; GAME], hand: &Hand, outcome: Outcome) {
    let mut h = house();
    let player = addr_at(g, G_PLAYER);
    let stake = u64_at(g, G_STAKE);
    let reserve = exposure(stake);
    let pays = payout(stake, outcome);
    // The stake and the reserve leave the hand: what isn't paid out is the bankroll's.
    sub(&mut h, H_RESERVED, reserve);
    sub(&mut h, H_STAKES, stake);
    add(&mut h, H_BANKROLL, stake + reserve - pays);
    // Off the open list: the last id takes its slot.
    let open = h[H_OPEN] as usize;
    let mut ids = get::<OPEN>(OPEN_KEY).unwrap_or([0; OPEN]);
    let mut slot = 0;
    while slot < open && u64_at(&ids, 8 * slot) != id {
        slot += 1;
    }
    if slot == open {
        abort(code::BAD_STORED, b"an open hand off the list");
    }
    let last = u64_at(&ids, 8 * (open - 1));
    set(&mut ids, 8 * slot, last);
    set(&mut ids, 8 * (open - 1), 0);
    put(OPEN_KEY, &ids);
    h[H_OPEN] = open as u8 - 1;
    remove(&key8(b'g', id));

    let mut p = player_record(&player);
    set(&mut p, P_OPEN, 0);
    set(&mut p, P_LAST_GAME, id);
    p[P_LAST_OUTCOME] = outcome as u8;
    p[P_LAST_DEALT] = hand.dealt;
    p[P_LAST_N_PLAYER] = hand.n_player;
    set(&mut p, P_LAST_STAKE, stake);
    set(&mut p, P_LAST_PAYOUT, pays);
    p[P_LAST_SEED..P_LAST_SEED + 32].copy_from_slice(&g[G_SEED..G_SEED + 32]);
    p[P_LAST_LINK..P_LAST_LINK + 32].copy_from_slice(&g[G_LINK..G_LINK + 32]);

    let mut to_table = g[G_FLAGS] & FLAG_TABLE != 0 && pays > 0;
    let credit = |h: &mut [u8; HOUSE], p: &mut [u8; PLAYER]| {
        add(p, P_BALANCE, pays);
        add(h, H_TABLE, pays);
    };
    if to_table {
        credit(&mut h, &mut p);
    }
    put(HOUSE_KEY, &h);
    put_player(&player, &p);
    // Effects first. An account that can't be paid gets its payout as table balance instead, so
    // a hand always settles.
    if pays > 0 && !to_table && transfer(&player, pays, 0) != OK {
        to_table = true;
        credit(&mut h, &mut p);
        put(HOUSE_KEY, &h);
        put_player(&player, &p);
    }

    let mut ev = [0u8; 126];
    set(&mut ev, 0, id);
    ev[8..40].copy_from_slice(&player);
    ev[40] = outcome as u8;
    set(&mut ev, 41, stake);
    set(&mut ev, 49, pays);
    ev[57] = to_table as u8;
    ev[58] = hand.dealt;
    ev[59] = hand.n_player;
    ev[60..92].copy_from_slice(&g[G_SEED..G_SEED + 32]);
    ev[92..124].copy_from_slice(&g[G_LINK..G_LINK + 32]);
    ev[124] = total(hand.player_cards()).0;
    ev[125] = total(hand.dealer_cards()).0;
    emit(b"settled", &ev);
}

/// Whether `link` is one the hand already revealed: the last one, or one before it (never the
/// commitment). The record keeps only the last link, and each earlier one is its hash.
fn revealed(link: &[u8; 32], g: &[u8], dealt: u8) -> bool {
    let mut at: [u8; 32] = addr_at(g, G_LINK);
    for back in 0..dealt {
        if at == *link {
            return true;
        }
        if back + 1 < dealt {
            at = prev_link(&at);
        }
    }
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_reveal() {
    nonpayable();
    let mut buf = [MaybeUninit::<u8>::uninit(); 12 + 32 * MAX_REVEAL];
    let (head, links) = counted::<8>(&mut buf, MAX_REVEAL);
    let id = u64_at(head, 0);
    let l = limits();
    let mut g = game_record(id);
    let mut hand = hand_of(&g);
    let seed: [u8; 32] = addr_at(&g, G_SEED);
    let mut ev = [0u8; 42 + 33 * MAX_REVEAL + 11];
    set(&mut ev, 0, id);
    ev[8..40].copy_from_slice(&g[G_PLAYER..G_PLAYER + 32]);
    ev[40] = hand.dealt + 1;
    let mut settled = None;
    // Links this call revealed. One the hand already holds is passed over, so a call that
    // someone got ahead of (with its first link, or all of them) still deals the rest.
    let mut n = 0;
    for at in 0..links.len() / 32 {
        let link: [u8; 32] = addr_at(links, 32 * at);
        let next = prev_link(&link) == g[G_LINK..G_LINK + 32];
        if !next && revealed(&link, &g, hand.dealt) {
            continue;
        }
        if !hand.phase.dealers() {
            if n == 0 {
                abort(NOT_DEALERS_TURN, b"the dealer owes no card");
            }
            // A link past the hand's last card: the hand is the player's to move, or over.
            abort(TOO_MANY_LINKS, b"more links than the hand takes");
        }
        if !next {
            abort(BAD_LINK, b"not the chain's next link");
        }
        let i = n;
        n += 1;
        let j = hand.dealt + 1;
        if j as usize > CHAIN_LEN {
            abort(code::BAD_STORED, b"past the chain");
        }
        let c = card(&link, &seed, id, j);
        g[G_LINK..G_LINK + 32].copy_from_slice(&link);
        ev[42 + 33 * i..74 + 33 * i].copy_from_slice(&link);
        ev[74 + 33 * i] = c;
        if let Some(Step::Settled(outcome)) = hand.deal(c) {
            settled = Some(outcome);
        }
    }
    // Every link was one the hand already held: nothing changes, and no deadline moves.
    if n == 0 {
        ret(&[hand.phase as u8, 0]);
        return;
    }
    ev[41] = n as u8;
    store_hand(&mut g, &hand);
    // The dealer's deadline runs from the bet, the hit or the stand, whatever it revealed since;
    // the player's turn starts its own.
    if hand.phase == Phase::Player {
        set(&mut g, G_DEADLINE, block_height() + u64_at(&l, L_IDLE_BLOCKS));
    }
    let tail = 42 + 33 * n;
    ev[tail] = hand.phase as u8;
    ev[tail + 1] = total(hand.player_cards()).0;
    ev[tail + 2] = total(hand.dealer_cards()).0;
    ev[tail + 3..tail + 11].copy_from_slice(&g[G_DEADLINE..G_DEADLINE + 8]);
    emit(b"cards", &ev[..tail + 11]);
    match settled {
        Some(outcome) => {
            settle(id, &g, &hand, outcome);
            ret(&[Phase::Done as u8, outcome as u8]);
        }
        None => {
            put(&key8(b'g', id), &g);
            ret(&[hand.phase as u8, 0]);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_timeout() {
    nonpayable();
    let id = u64::from_le_bytes(input::<8>());
    let l = limits();
    let mut g = game_record(id);
    let mut hand = hand_of(&g);
    let now = block_height();
    if now <= u64_at(&g, G_DEADLINE) {
        abort(NOT_YET, b"not past the deadline");
    }
    if hand.phase == Phase::Seed {
        // The seed never came: the stake is the house's, the reserve is free again.
        settle(id, &g, &hand, Outcome::NoSeed);
        ret(&[Phase::Done as u8, Outcome::NoSeed as u8]);
    } else if hand.stand() {
        // An idle player stands; the dealer finishes the hand.
        let deadline = now + u64_at(&l, L_REVEAL_BLOCKS);
        store_hand(&mut g, &hand);
        set(&mut g, G_DEADLINE, deadline);
        put(&key8(b'g', id), &g);
        emit_move(id, &addr_at(&g, G_PLAYER), deadline, Some(1));
        ret(&[hand.phase as u8, 0]);
    } else {
        // The dealer missed a reveal: the player wins.
        let outcome = hand.forfeit();
        settle(id, &g, &hand, outcome);
        ret(&[Phase::Done as u8, outcome as u8]);
    }
}

// ---------------------------------------------------------------------------------- views

#[unsafe(no_mangle)]
pub extern "C" fn bk_game() {
    let g = game_record(u64::from_le_bytes(input::<8>()));
    let hand = hand_of(&g);
    let mut out = [0u8; GAME + 2];
    out[..GAME].copy_from_slice(&g);
    out[GAME] = total(hand.player_cards()).0;
    out[GAME + 1] = total(hand.dealer_cards()).0;
    ret_bytes::<{ GAME + 2 }, { GAME + 6 }>(&out);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_player() {
    let p = player_record(&input::<32>());
    let mut out = [0u8; PLAYER + 8];
    out[..PLAYER].copy_from_slice(&p);
    set(&mut out, PLAYER, block_height());
    ret_bytes::<{ PLAYER + 8 }, { PLAYER + 12 }>(&out);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_house() {
    input::<0>();
    let h = house();
    let mut out = [0u8; HOUSE + 16];
    out[..HOUSE].copy_from_slice(&h);
    set(&mut out, HOUSE, self_balance());
    set(&mut out, HOUSE + 8, block_height());
    ret_bytes::<{ HOUSE + 16 }, { HOUSE + 20 }>(&out);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_limits() {
    input::<0>();
    ret_bytes::<LIMITS, { LIMITS + 4 }>(&limits());
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_open_games() {
    input::<0>();
    let h = house();
    let ids = get::<OPEN>(OPEN_KEY).unwrap_or([0; OPEN]);
    let open = h[H_OPEN] as usize;
    let mut out = [0u8; 4 + OPEN];
    out[..4].copy_from_slice(&(open as u32).to_le_bytes());
    out[4..].copy_from_slice(&ids);
    ret(&out[..4 + 8 * open]);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_commitment() {
    let k = u64::from_le_bytes(input::<8>());
    match get::<32>(&key8(b'c', k)) {
        Some(c) => ret(&c),
        None => abort(NO_COMMITMENT, b"no such commitment"),
    }
}
