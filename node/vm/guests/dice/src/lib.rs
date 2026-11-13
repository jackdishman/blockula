//! # dice: a six-sided die settled on the randomness beacon
//!
//! The test program for `abi::random` (docs/RANDOMNESS.md): a player stakes FANG on a face, and
//! two epochs later anyone settles the roll on the beacon. A hit pays six times the stake from
//! the bank; a miss leaves the stake in it; whoever settles earns a small bounty. It imports `blake3_v1`, `epoch_v1` and `beacon_v1`,
//! so it uploads only under protocol v3.
//!
//! **The pattern.** `roll` fixes the player, the stake and the pick and makes the request, all
//! before any validator has revealed for the request's epoch. `settle` only reads the outcome and
//! applies it to what `roll` stored. Anyone may settle, and every rule below is fixed here, in the
//! code, before any roll:
//!
//! - **A settled roll pays its settler a bounty**: a hundredth of the stake (rounded down), to
//!   whoever sends the `settle`. On a miss it comes out of the stake and the rest stays in the
//!   bank; on a hit the player gets six stakes and the bounty comes from the bank. Without it
//!   nobody would have a reason to settle a miss (the bank has no owner), and a losing player
//!   would just leave the roll open.
//! - **A roll nobody settled is forfeit, never refunded.** A roll settles for as long as the
//!   chain keeps its epoch's beacon value: 64 epochs, the target's own and the 63 after it
//!   (`abi::random`). Once the value is gone nobody can tell what the roll was, and the stake
//!   stays in the bank, less the same bounty to whoever closes the roll. A refund there would be
//!   a free option: hits paid six times, misses handed back to whoever waited.
//! - **Too few reveals refund the whole stake.** When the beacon value had fewer reveals than
//!   the program asks for, it isn't randomness, so there is no result to hide from: the stake
//!   goes back to the player, and no bounty is paid (the player is the one with a reason to
//!   send that `settle`). Left past the 64 epochs, such a roll is forfeit like any other.
//!
//! **Storage.**
//! - `b"m"` → the fewest reveals a roll's beacon must hold, `u32` LE; set once by `init`.
//! - `b"o"` → what the open rolls could pay out together (six stakes and a bounty each), `u64` LE.
//! - `b"r" ‖ id (8, LE)` → player 32 ‖ stake 8 ‖ pick 1 ‖ request 28 (69 bytes). Removed when
//!   the roll settles, so its deposit goes back.
//! - `abi::random::NEXT_ID_KEY` → the next request id (the module's).
//!
//! | Method | Input (LE) | Who | Returns | Event | Aborts |
//! |---|---|---|---|---|---|
//! | `init` (init) | min reveals 4 | the creator, once | nothing | none | 30 already initialized |
//! | `fund` (payable) | empty | anyone: adds to the bank | nothing | `fund`: from 32 ‖ amount 8 (40) | 31 no value |
//! | `roll` (payable) | pick 1 (1–6) | anyone: the player | id 8 | `roll`: id 8 ‖ player 32 ‖ stake 8 ‖ pick 1 ‖ target epoch 8 ‖ deadline epoch 8 (65) | 31 no value, 32 bad pick, 33 bank short, 36 not initialized |
//! | `settle` | id 8 | anyone: the settler | the face rolled 1 (0 for a refund or a forfeit) | `settled`: id 8 ‖ player 32 ‖ face 1 ‖ payout 8 ‖ settler 32 ‖ bounty 8 (89); or `refunded`: id 8 ‖ player 32 ‖ stake 8 (48; too few reveals); or `forfeited`: id 8 ‖ player 32 ‖ stake 8 ‖ settler 32 ‖ bounty 8 (88; the value is gone) | 34 no roll, 35 not yet, 4 transfer |
//!
//! Every method also aborts 1 `BAD_INPUT` on another input length; `init` and `settle` abort
//! 2 `NOT_PAYABLE` on value.
//!
//! **The bank.** A roll is taken only if the program's balance covers every open roll's payout
//! and bounty, this one's included (`b"o"`), so a hit and its settler are always paid. Stakes of
//! missed and forfeited rolls stay in the bank; this test program has no way to take them out.
//! The odds are even (one face in six pays six stakes), so the bounty on a hit is the bank's
//! loss: a hundredth of a stake per roll on average. A real game would price that in. A stake
//! under 100 drops has no bounty at all, so nobody but its player is paid to settle it.

#![no_std]

use abi::random::{self, Outcome, Request};
use abi::{abort, addr_at, caller, emit, get, get_u64, input, nonpayable, pay, put, remove, ret, self_balance, u64_at, value};

/// Abort codes.
const ALREADY_INITIALIZED: u32 = 30;
const NO_VALUE: u32 = 31;
const BAD_PICK: u32 = 32;
const BANK_SHORT: u32 = 33;
const NO_ROLL: u32 = 34;
const NOT_YET: u32 = 35;
const NOT_INITIALIZED: u32 = 36;

const MIN_REVEALS: &[u8] = b"m";
const OWED: &[u8] = b"o";
const ROLL: usize = 41 + Request::BYTES;
/// What the value is for (`random::settle`'s domain).
const DOMAIN: &[u8] = b"dice/roll";
const FACES: u64 = 6;

/// The settler's bounty is the stake divided by this, rounded down.
const BOUNTY_DIVISOR: u64 = 100;

/// What an open roll can cost the bank: six stakes to the player and the bounty to the settler.
/// `None` if that overflows.
fn owed_for(stake: u64) -> Option<u64> {
    stake.checked_mul(FACES)?.checked_add(stake / BOUNTY_DIVISOR)
}

fn roll_key(id: &[u8; 8]) -> [u8; 9] {
    let mut k = [b'r'; 9];
    k[1..].copy_from_slice(id);
    k
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_init() {
    nonpayable();
    let min_reveals = input::<4>();
    if get::<4>(MIN_REVEALS).is_some() {
        abort(ALREADY_INITIALIZED, b"already initialized");
    }
    put(MIN_REVEALS, &min_reveals);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_fund() {
    input::<0>();
    let amount = value();
    if amount == 0 {
        abort(NO_VALUE, b"no value");
    }
    let mut ev = [0u8; 40];
    ev[..32].copy_from_slice(&caller());
    ev[32..].copy_from_slice(&amount.to_le_bytes());
    emit(b"fund", &ev);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_roll() {
    let [pick] = input::<1>();
    let stake = value();
    if stake == 0 {
        abort(NO_VALUE, b"no value");
    }
    if !(1..=FACES as u8).contains(&pick) {
        abort(BAD_PICK, b"pick a face from 1 to 6");
    }
    let Some(min_reveals) = get::<4>(MIN_REVEALS) else {
        abort(NOT_INITIALIZED, b"not initialized");
    };
    // The stake is in the balance already. Every open roll's payout must be covered.
    let owed = owed_for(stake).and_then(|roll| get_u64(OWED).checked_add(roll));
    let Some(owed) = owed.filter(|owed| *owed <= self_balance()) else {
        abort(BANK_SHORT, b"the bank can't cover this roll");
    };
    put(OWED, &owed.to_le_bytes());

    // Everything the value will decide is fixed here, before the request's epoch has a reveal.
    let player = caller();
    let request = random::request(u32::from_le_bytes(min_reveals));
    let id = request.id.to_le_bytes();
    let mut roll = [0u8; ROLL];
    roll[..32].copy_from_slice(&player);
    roll[32..40].copy_from_slice(&stake.to_le_bytes());
    roll[40] = pick;
    roll[41..].copy_from_slice(&request.to_bytes());
    put(&roll_key(&id), &roll);

    let mut ev = [0u8; 65];
    ev[..8].copy_from_slice(&id);
    ev[8..49].copy_from_slice(&roll[..41]);
    ev[49..57].copy_from_slice(&request.target_epoch.to_le_bytes());
    ev[57..].copy_from_slice(&request.deadline_epoch.to_le_bytes());
    emit(b"roll", &ev);
    ret(&id);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_settle() {
    nonpayable();
    let id = input::<8>();
    let key = roll_key(&id);
    let Some(roll) = get::<ROLL>(&key) else {
        abort(NO_ROLL, b"no roll");
    };
    let (player, stake, pick) = (addr_at(&roll, 0), u64_at(&roll, 32), roll[40]);
    let request = Request::from_bytes(roll[41..].try_into().unwrap());

    let (settler, bounty) = (caller(), stake / BOUNTY_DIVISOR);

    // (what the player gets, what the settler gets, the face or 0, the event)
    let (amount, bounty, face, name): (u64, u64, u8, &[u8]) = match random::settle(&request, DOMAIN) {
        Outcome::Pending => abort(NOT_YET, b"not yet"),
        Outcome::Settled(value) => {
            let face = 1 + random::below(&value, FACES) as u8;
            // A hit's bounty comes from the bank, a miss's out of the stake that stays there.
            (if face == pick { stake * FACES } else { 0 }, bounty, face, b"settled")
        }
        // Not randomness, so no result: the whole stake goes back, and nobody is paid for it.
        Outcome::TooFewReveals => (stake, 0, 0, b"refunded"),
        // Nobody settled while the value was kept. Forfeit, never a refund: see the top.
        Outcome::Expired => (0, bounty, 0, b"forfeited"),
    };
    // Effects before the transfers; a failed transfer aborts and rolls them back. `roll` checked
    // that this roll's share of `OWED` doesn't overflow.
    remove(&key);
    put(OWED, &(get_u64(OWED) - stake * FACES - stake / BOUNTY_DIVISOR).to_le_bytes());
    if amount > 0 {
        pay(&player, amount, 0);
    }
    if bounty > 0 {
        pay(&settler, bounty, 0);
    }

    // id 8 ‖ player 32 ‖ (face 1 ‖ payout 8 | stake 8) ‖ settler 32 ‖ bounty 8, the last two
    // left off a refund.
    let mut ev = [0u8; 89];
    ev[..8].copy_from_slice(&id);
    ev[8..40].copy_from_slice(&player);
    let mut n = 40;
    if face == 0 {
        ev[n..n + 8].copy_from_slice(&stake.to_le_bytes());
        n += 8;
    } else {
        ev[n] = face;
        ev[n + 1..n + 9].copy_from_slice(&amount.to_le_bytes());
        n += 9;
    }
    if name != b"refunded" {
        ev[n..n + 32].copy_from_slice(&settler);
        ev[n + 32..n + 40].copy_from_slice(&bounty.to_le_bytes());
        n += 40;
    }
    emit(name, &ev[..n]);
    ret(&[face]);
}
