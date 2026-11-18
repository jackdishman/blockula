//! # blackjack2: the `blackjack` program under rules v2 (a dealer that stops pays for nothing it didn't deal)
//!
//! A separate program from `../blackjack`, which is unchanged: a table is one or the other for
//! life, and its `version` view (and its stored `b"V"` record) says which. Everything about the
//! cards, the money and the turns is v1's (docs/BLACKJACK.md) except what this page lists; the
//! reasons are in docs/proposals/blackjack-dealer-outage.md. The rules are `bj2-core`'s, which
//! runs v1's own card code. It imports only protocol v2's host functions.
//!
//! **What changed from v1.**
//!
//! 1. **The dealer opens a hand before it can know a card.** `bet` still carries only the hash of
//!    the player's seed. The dealer then has `open_blocks` to reveal the chain's first three links
//!    with `reveal` (phase 6, *open*), and only after that does the player owe the seed. A card is
//!    `BLAKE3(link ‖ client_seed ‖ game ‖ j) mod 52`: the dealer opens without the seed, so it
//!    opens (or doesn't) without knowing a card. A link that arrives past that deadline is
//!    refused (abort 73), as a late seed is: once it is public, nobody may choose between letting
//!    it in and calling `timeout`.
//! 2. **A hand that is never opened is refunded.** Past `open_blocks` anyone calls `timeout` and
//!    the stake goes back (outcome 9, `refund`). Nothing else is paid: a bet placed while the
//!    dealer is down wins nothing. Under v1 it won 3:2.
//! 3. **The seed deals the hand.** `reveal_seed` checks the seed against the bet's hash, computes
//!    the first three cards from the three links already on the chain and puts them on the table
//!    in the same transaction (events `seed`, `cards`, and `settled` for a natural the upcard
//!    can't match). There is no state in which the dealer knows the deal and still owes it. The
//!    player has seen those three cards before revealing; the seed was fixed by its hash at the
//!    bet, and a seed that never comes still forfeits the stake (outcome 8), so nothing is gained.
//! 4. **A later stall pays what the cards on the table still allow.** From the seed on the dealer
//!    knows every card, so a missed reveal pays the player the most the hand could still have
//!    paid, as in v1, but computed from the cards (`bj2_core::Hand::forfeit`): 3:2 only for a
//!    natural in the player's hand, or while a relayed hand's first card is unseen or is an ace
//!    or a ten-value card; otherwise 1:1.
//! 5. **A circuit breaker.** A hand that ends by the dealer's missed deadline at the house's cost
//!    (outcomes 6 and 7) sets the pause to 2, *tripped* (event `tripped`): no new bet is taken
//!    until the owner calls `pause(false)`. Open hands play on and are timed out as before, and
//!    withdrawals work. So one outage costs the house at most the hands that were open when it
//!    began. A refund doesn't trip it: it cost nothing.
//!
//! **A relayed bet is dealt as in v1.** Its seed is derived from the post and public at the bet,
//! so the dealer can't open it blind: `relay_bet` starts in phase 1 and the dealer reveals the
//! three cards. A stall there still pays the player (a dealer that could decline a relayed hand
//! it had already read would be a dealer that keeps only the hands it wins), by the tighter rule
//! of point 4, and the breaker bounds what an outage costs through relayers.
//!
//! **Who is who, fairness, the money, paying out:** as `../blackjack`. The invariant is the same:
//!
//! ```text
//! balance = bankroll + reserved + stakes + table
//! ```
//!
//! A hand still reserves 1.5 × its stake at the bet; a refund pays the stake and returns the
//! whole reserve to the bankroll.
//!
//! **Deadlines.** The dealer has `open_blocks` from an account's bet to open the hand. The player
//! then has `seed_blocks` from the opening to reveal the seed. Whenever the dealer owes a card it
//! has `reveal_blocks`, counted from the seed's reveal (a natural waiting on the hole card), a
//! relayed bet, the hit or the stand. A player who doesn't move for `idle_blocks` (counted from
//! the cards that gave them the turn) stands by `timeout`.
//!
//! **Storage** (all integers little-endian): v1's records, with these differences.
//! - `b"V"` → `[2]`: the rules' version. A v1 table has no such record.
//! - `b"H"` → the house (114 bytes), as v1; `paused` is 0 (bets are taken), 1 (the owner paused)
//!   or 2 (tripped by a forfeit).
//! - `b"C"` → the limits (112 bytes): fourteen `u64`s, v1's thirteen and `open_blocks`.
//! - `b"g" ‖ id (8)` → a hand (157 bytes), v1's layout. Phase 6: the dealer owes the opening
//!   links. While the phase is 6 or 0 the seed field holds the seed's hash, `cards dealt` counts
//!   the opening links revealed (0 to 3), `newest link` is the newest of them (the commitment
//!   before any) and there are no cards.
//! - `b"p" ‖ account (32)` → a player (195 bytes), v1's layout. For outcomes 8 and 9 the last
//!   hand's seed field is the hash that was never opened, `cards dealt` the opening links that
//!   were revealed, and the link the newest of them.
//!
//! **Methods.** v1's, with the same inputs, except:
//!
//! | Method | Input | Who | Does | Event |
//! |---|---|---|---|---|
//! | `version` (view) | — | | `[2]` | |
//! | `set_limits` | 14 × 8 | the owner | v1's thirteen, then `open_blocks` | `limits`: the 112 bytes |
//! | `limits` (view) | — | | the 112 bytes | |
//! | `pause` | on 1 | the owner | stops (1) or allows (0) new bets; allowing them clears a trip | `paused`: on 1 |
//! | `bet` (payable) | seed hash 32 | a player | as v1; the hand starts in phase 6 and its deadline is the dealer's, `open_blocks` away | `bet` |
//! | `reveal` | game 8 ‖ n 4 ‖ n × 32 | anyone who knows the links | on a hand in phase 6: takes the next opening links (at most the three), deals no card, and once all three are out the player owes the seed; refused past the deadline (73). Otherwise as v1 | `opened`, or v1's `cards` and `settled` |
//! | `reveal_seed` | game 8 ‖ client seed 32 | the hand's player, once it is opened, within `seed_blocks` | opens the commitment and deals the first three cards; returns phase 1 ‖ outcome 1 | `seed`, `cards`, and `settled` when they settle it |
//! | `timeout` | game 8 | anyone, past the deadline | refunds a hand the dealer never opened (9), ends a hand whose seed never came (8), stands an idle player, or pays the player when the dealer missed a reveal (6, 7: and trips the breaker) | `settled` (and `tripped`) or `stand` |
//!
//! **Limits the owner can't set.** v1's, and `open_blocks` is from 1 to 86,400 like
//! `reveal_blocks`: a stake never waits for the dealer without end.
//!
//! **Events.** v1's, and:
//! - `opened`: game 8 ‖ player 32 ‖ first link's index 1 ‖ n 1 ‖ n × link 32 ‖ phase 1 (6 while
//!   more are owed, 0 once the player owes the seed) ‖ deadline 8.
//! - `tripped`: game 8 ‖ outcome 1 (9).
//! - `seed` is v1's; its deadline is the next one (the player's move, or the dealer's hole card).
//! - `settled`'s outcome may be 9; then, as for 8, its seed field is the seed's hash.
//!
//! **Aborts.** v1's, and 73: past the deadline to open the hand. 70 (the hand isn't waiting for a
//! seed) now also answers a seed sent before the dealer opened the hand.

#![no_std]

use core::mem::MaybeUninit;

use abi::{Address, OK, abort, addr_at, block_height, caller, code, emit, get, input, input_into, nonpayable, pay, put, remove, ret, self_balance, transfer, u64_at, value};
use bj2_core::{CHAIN_LEN, Hand, MAX_DEALER_CARDS, MAX_PLAYER_CARDS, OPENING_LINKS, Outcome, Phase, RULES, Step, card, exposure, payout, prev_link, seed_hash, total};

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
const OPEN_LATE: u32 = 73;

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
/// `paused`: the owner stopped new bets.
const PAUSED_BY_OWNER: u8 = 1;
/// `paused`: a hand ended by the dealer's missed deadline at the house's cost. New bets stop
/// until the owner allows them again (`pause(false)`).
const TRIPPED: u8 = 2;

/// The rules' version, stored so that a client reading storage can tell the tables apart.
const VERSION_KEY: &[u8] = b"V";

// The limits, each a u64.
const LIMITS_KEY: &[u8] = b"C";
const LIMITS: usize = 112;
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
const L_OPEN_BLOCKS: usize = 104;

/// The defaults `init` writes: 1 to 50 FANG a bet, 500 FANG a day (43,200 blocks: a day of
/// 2-second blocks), 10 open hands, a bet at most a twentieth of the free bankroll, 750 FANG of
/// exposure in all, 150 blocks to reveal, 1,800 to move, 200 FANG of table balance, 25 FANG a
/// relayed bet, grants of at most 43,200 blocks, 300 blocks to reveal a bet's seed, 150 blocks
/// for the dealer to open a hand (what it has for any card it owes).
const DEFAULT_LIMITS: [u64; 14] = [FANG, 50 * FANG, 500 * FANG, 43_200, 10, 20, 750 * FANG, 150, 1_800, 200 * FANG, 25 * FANG, 43_200, 300, 150];

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
    put(VERSION_KEY, &[RULES]);
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
    let blocks_ok = [L_DAY_BLOCKS, L_REVEAL_BLOCKS, L_IDLE_BLOCKS, L_GRANT_BLOCKS, L_SEED_BLOCKS, L_OPEN_BLOCKS].iter().all(|i| (1..=MAX_BLOCKS).contains(&at(*i)));
    // What a player is given is never under the floor, what the dealer is given never over the
    // ceiling: the owner can't use the limits to take a move or a seed's reveal from a player,
    // or to leave a hand (or a stake that was never opened) waiting for the dealer for good.
    let turns_ok = at(L_IDLE_BLOCKS) >= MIN_PLAYER_BLOCKS && at(L_SEED_BLOCKS) >= MIN_PLAYER_BLOCKS && at(L_REVEAL_BLOCKS) <= MAX_DEALER_BLOCKS && at(L_OPEN_BLOCKS) <= MAX_DEALER_BLOCKS;
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
    // Allowing bets again also clears a trip: the owner has looked.
    h[H_PAUSED] = if on == 1 { PAUSED_BY_OWNER } else { 0 };
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
/// [`Phase::Open`] (a bet from an account: the dealer owes the opening links, and has
/// `open_blocks` for them). Returns the hand's id.
#[allow(clippy::too_many_arguments)]
fn open_hand(h: &mut [u8; HOUSE], l: &[u8; LIMITS], player: &Address, p: &mut [u8; PLAYER], stake: u64, seed: &[u8; 32], start: Phase, flags: u8, post: u64) -> u64 {
    // By the owner, or by a forfeit (the circuit breaker): either way until the owner allows
    // bets again.
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

    let (hand, blocks) = if start == Phase::Open { (Hand::committed(), L_OPEN_BLOCKS) } else { (Hand::new(), L_REVEAL_BLOCKS) };
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
    // The hash of the player's seed (`bj2_core::seed_hash`), never the seed: it stays private
    // until this hand holds its commitment and the dealer has opened it.
    let hash = input::<32>();
    let stake = value();
    if stake == 0 {
        abort(NO_VALUE, b"no value");
    }
    let mut h = house();
    let l = limits();
    let player = caller();
    let mut p = player_record(&player);
    let id = open_hand(&mut h, &l, &player, &mut p, stake, &hash, Phase::Open, 0, 0);
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
    // Only once the dealer has opened the hand: before that a seed would let it see the cards
    // and then choose whether to open.
    if hand.phase != Phase::Seed {
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
    // The three opening links are on the chain: the record keeps the newest, and each earlier
    // one is its hash. With the seed they are the first three cards, dealt here and now.
    let mut links = [[0u8; 32]; OPENING_LINKS as usize];
    links[2] = addr_at(&g, G_LINK);
    links[1] = prev_link(&links[2]);
    links[0] = prev_link(&links[1]);
    let cards = [card(&links[0], &seed, id, 1), card(&links[1], &seed, id, 2), card(&links[2], &seed, id, 3)];
    let step = match hand.seeded(cards) {
        Some(step) => step,
        None => abort(code::BAD_STORED, b"an opened hand"),
    };
    // The player's turn starts its own deadline; a natural waiting on the hole card starts the
    // dealer's.
    let deadline = now + u64_at(&l, if hand.phase == Phase::Player { L_IDLE_BLOCKS } else { L_REVEAL_BLOCKS });
    g[G_SEED..G_SEED + 32].copy_from_slice(&seed);
    store_hand(&mut g, &hand);
    set(&mut g, G_DEADLINE, deadline);

    let mut ev = [0u8; 80];
    set(&mut ev, 0, id);
    ev[8..40].copy_from_slice(&player);
    ev[40..72].copy_from_slice(&seed);
    set(&mut ev, 72, deadline);
    emit(b"seed", &ev);

    // v1's `cards` event, as a v1 dealer's first reveal emits it.
    let mut ev = [0u8; 42 + 33 * OPENING_LINKS as usize + 11];
    set(&mut ev, 0, id);
    ev[8..40].copy_from_slice(&player);
    ev[40] = 1;
    ev[41] = OPENING_LINKS;
    for i in 0..OPENING_LINKS as usize {
        ev[42 + 33 * i..74 + 33 * i].copy_from_slice(&links[i]);
        ev[74 + 33 * i] = cards[i];
    }
    let tail = 42 + 33 * OPENING_LINKS as usize;
    ev[tail] = hand.phase as u8;
    ev[tail + 1] = total(hand.player_cards()).0;
    ev[tail + 2] = total(hand.dealer_cards()).0;
    set(&mut ev, tail + 3, deadline);
    emit(b"cards", &ev);

    match step {
        Step::Settled(outcome) => {
            settle(id, &g, &hand, outcome);
            ret(&[Phase::Done as u8, outcome as u8]);
        }
        Step::More | Step::Player => {
            put(&key8(b'g', id), &g);
            ret(&[hand.phase as u8, 0]);
        }
    }
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
    let seed = bj2_core::blake3(&pre);
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
    // The circuit breaker: the dealer missed a reveal and the house paid for it. No new bet is
    // taken until the owner allows bets again, so one outage costs at most the hands that were
    // open when it began.
    let tripped = outcome.dealers_fault();
    if tripped {
        h[H_PAUSED] = TRIPPED;
    }

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
    if tripped {
        let mut ev = [0u8; 9];
        set(&mut ev, 0, id);
        ev[8] = outcome as u8;
        emit(b"tripped", &ev);
    }
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
    // A hand being opened takes links and deals no card: the seed isn't known. A call is all
    // opening links or all cards, since the opening ends with the player owing the seed.
    let opening = hand.phase == Phase::Open;
    let now = block_height();
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
        if opening {
            // Past the deadline the stake is the player's to take back, whoever gets there
            // first: a late link is public in the mempool, and a player who read the cards it
            // deals could otherwise choose between letting it in and calling `timeout`.
            if now > u64_at(&g, G_DEADLINE) {
                abort(OPEN_LATE, b"past the deadline to open the hand");
            }
            g[G_LINK..G_LINK + 32].copy_from_slice(&link);
            // The `opened` event's links are packed: no card follows each.
            ev[42 + 32 * i..74 + 32 * i].copy_from_slice(&link);
            hand.open_link();
            continue;
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
    if opening {
        // All three out: the player owes the seed, and has `seed_blocks` from here. Until then
        // the dealer's own deadline stands, whatever it revealed so far.
        if hand.phase == Phase::Seed {
            set(&mut g, G_DEADLINE, now + u64_at(&l, L_SEED_BLOCKS));
        }
        let tail = 42 + 32 * n;
        ev[tail] = hand.phase as u8;
        ev[tail + 1..tail + 9].copy_from_slice(&g[G_DEADLINE..G_DEADLINE + 8]);
        emit(b"opened", &ev[..tail + 9]);
        put(&key8(b'g', id), &g);
        ret(&[hand.phase as u8, 0]);
        return;
    }
    // The dealer's deadline runs from the bet, the hit or the stand, whatever it revealed since;
    // the player's turn starts its own.
    if hand.phase == Phase::Player {
        set(&mut g, G_DEADLINE, now + u64_at(&l, L_IDLE_BLOCKS));
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
        // The dealer missed its deadline. A hand it never opened is refunded: it had seen no
        // card. One it did open pays the player the most the cards on the table still allowed,
        // and trips the breaker (`settle`).
        let outcome = match hand.forfeit() {
            Some(outcome) => outcome,
            None => abort(code::BAD_STORED, b"an open hand nobody owes a move on"),
        };
        settle(id, &g, &hand, outcome);
        ret(&[Phase::Done as u8, outcome as u8]);
    }
}

// ---------------------------------------------------------------------------------- views

#[unsafe(no_mangle)]
pub extern "C" fn bk_version() {
    input::<0>();
    ret(&[RULES]);
}

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
