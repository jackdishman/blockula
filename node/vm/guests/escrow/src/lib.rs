//! # escrow: FANG held for a seller until the buyer releases it
//!
//! The M2 demo's program (docs/PROGRAMS-PLAN.md) and the example in docs/PROGRAMS.md 12.1, by
//! hand: a buyer opens a deal with attached FANG, then either the buyer releases it to the seller
//! or the seller refunds it to the buyer. Effects come before the transfer, and a failed transfer
//! aborts, which rolls the whole frame back, the removed deal included.
//!
//! **Storage.** `b"d" ‖ id (8, LE)` → buyer 32 ‖ seller 32 ‖ amount 8 (72 bytes). Removed when
//! the deal settles, so its deposit goes back.
//!
//! | Method | Input (LE) | Who | Pays | Event | Aborts |
//! |---|---|---|---|---|---|
//! | `open` (payable) | id 8 ‖ seller 32 (40) | anyone: the buyer | holds the call's value | `open`: id 8 ‖ buyer 32 ‖ seller 32 ‖ amount 8 (80) | 20 no value, 21 id taken |
//! | `release` | id 8 | the buyer | the seller, `transfer` + `OPEN_DEDUCT` | `release`: id 8 ‖ seller 32 ‖ amount 8 (48) | 22 no deal, 23 not the buyer, 4 transfer |
//! | `refund` | id 8 | the seller | the buyer, `transfer` without flags | `refund`: id 8 ‖ buyer 32 ‖ amount 8 (48) | 22 no deal, 24 not the seller, 4 transfer |
//!
//! Every method also aborts 1 `BAD_INPUT` on another input length; `release` and `refund` abort
//! 2 `NOT_PAYABLE` on value. Nothing is returned.
//!
//! **Why the flags differ.** A seller may be a fresh account, so `release` opens it and the
//! creation fee comes out of the amount (`OPEN_DEDUCT`; a deal smaller than the fee can't be
//! released to a new account, only refunded). The buyer called `open`, so it exists: `refund`
//! needs no flag. Ids are the buyer's choice, as in 12.1; one taken can't be reopened until it
//! settles.

#![no_std]

use abi::{Address, OPEN_DEDUCT, abort, addr_at, caller, emit, get, input, nonpayable, pay, put, remove, u64_at, value};

/// Abort codes.
const NO_VALUE: u32 = 20;
const ID_TAKEN: u32 = 21;
const NO_DEAL: u32 = 22;
const NOT_BUYER: u32 = 23;
const NOT_SELLER: u32 = 24;

const DEAL: usize = 72;

fn deal_key(id: &[u8; 8]) -> [u8; 9] {
    let mut k = [b'd'; 9];
    k[1..].copy_from_slice(id);
    k
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_open() {
    let inp = input::<40>();
    let amount = value();
    if amount == 0 {
        abort(NO_VALUE, b"no value");
    }
    let key = deal_key(&inp[..8].try_into().unwrap());
    if get::<DEAL>(&key).is_some() {
        abort(ID_TAKEN, b"id taken");
    }
    let buyer = caller();
    let mut deal = [0u8; DEAL];
    deal[..32].copy_from_slice(&buyer);
    deal[32..64].copy_from_slice(&inp[8..40]);
    deal[64..].copy_from_slice(&amount.to_le_bytes());
    put(&key, &deal);

    let mut ev = [0u8; 80];
    ev[..8].copy_from_slice(&inp[..8]);
    ev[8..].copy_from_slice(&deal);
    emit(b"open", &ev);
}

/// Removes deal `id` if the caller is its buyer (`by_buyer`) or seller, and pays its amount to
/// the other party.
fn settle(by_buyer: bool) {
    nonpayable();
    let id = input::<8>();
    let key = deal_key(&id);
    let Some(deal) = get::<DEAL>(&key) else {
        abort(NO_DEAL, b"no deal");
    };
    let (buyer, seller, amount) = (addr_at(&deal, 0), addr_at(&deal, 32), u64_at(&deal, 64));
    let who = caller();
    let (to, flags, name): (Address, i32, &[u8]) = if by_buyer {
        if who != buyer {
            abort(NOT_BUYER, b"not the buyer");
        }
        (seller, OPEN_DEDUCT, b"release")
    } else {
        if who != seller {
            abort(NOT_SELLER, b"not the seller");
        }
        (buyer, 0, b"refund")
    };
    remove(&key);
    pay(&to, amount, flags);

    let mut ev = [0u8; 48];
    ev[..8].copy_from_slice(&id);
    ev[8..40].copy_from_slice(&to);
    ev[40..].copy_from_slice(&amount.to_le_bytes());
    emit(name, &ev);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_release() {
    settle(true)
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_refund() {
    settle(false)
}
