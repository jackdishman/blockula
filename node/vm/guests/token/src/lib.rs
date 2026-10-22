//! # token: a minimal fungible token
//!
//! Balances in the program's own storage, moved by their holder. No allowances, decimals or
//! metadata: BTS-1 (docs/PROGRAMS.md 11.4) is the real convention; this is what S2 needs to
//! measure sizes, storage and events. Every method refuses FANG ([`abi::code::NOT_PAYABLE`]).
//!
//! **Storage.**
//! - `b"s"` → the total supply, `u64` LE; present once initialized.
//! - `b"b" ‖ account (32)` → that account's balance, `u64` LE. A balance that reaches 0 is
//!   removed, so its deposit goes back; absent reads as 0.
//!
//! | Method | Input (LE) | Returns | Event | Aborts |
//! |---|---|---|---|---|
//! | `init` | owner 32 ‖ supply 8 (40) | nothing | none | 10 already initialized |
//! | `transfer` | to 32 ‖ amount 8 (40), from the caller | nothing | `transfer`: from 32 ‖ to 32 ‖ amount 8 (72) | 11 balance short |
//! | `balance_of` | account 32 | its balance, `u64` LE | none | |
//! | `total_supply` | empty | the supply, `u64` LE (0 before `init`) | none | |
//!
//! Every method also aborts 1 `BAD_INPUT` on another input length and 2 `NOT_PAYABLE` on value.
//! A transfer to oneself, or of 0, succeeds and emits its event. `init` is first come, first
//! served: a stand-in for an `Init` method run at create (7.2).

#![no_std]

use abi::{Address, abort, addr_at, caller, emit, get, get_u64, input, nonpayable, put, remove, ret, u64_at};

const SUPPLY: &[u8] = b"s";

/// Abort codes.
const ALREADY_INITIALIZED: u32 = 10;
const BALANCE_SHORT: u32 = 11;

fn balance_key(account: &Address) -> [u8; 33] {
    let mut k = [b'b'; 33];
    k[1..].copy_from_slice(account);
    k
}

fn balance(account: &Address) -> u64 {
    get_u64(&balance_key(account))
}

fn set_balance(account: &Address, amount: u64) {
    let key = balance_key(account);
    if amount == 0 {
        remove(&key);
    } else {
        put(&key, &amount.to_le_bytes());
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_init() {
    nonpayable();
    let inp = input::<40>();
    if get::<8>(SUPPLY).is_some() {
        abort(ALREADY_INITIALIZED, b"already initialized");
    }
    let (owner, supply) = (addr_at(&inp, 0), u64_at(&inp, 32));
    put(SUPPLY, &supply.to_le_bytes());
    set_balance(&owner, supply);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_transfer() {
    nonpayable();
    let inp = input::<40>();
    let (to, amount) = (addr_at(&inp, 0), u64_at(&inp, 32));
    let from = caller();
    let Some(left) = balance(&from).checked_sub(amount) else {
        abort(BALANCE_SHORT, b"balance short");
    };
    set_balance(&from, left);
    // Read after the debit, so a transfer to oneself nets to nothing. Can't overflow: balances
    // sum to the supply, a u64.
    set_balance(&to, balance(&to) + amount);

    let mut ev = [0u8; 72];
    ev[..32].copy_from_slice(&from);
    ev[32..].copy_from_slice(&inp);
    emit(b"transfer", &ev);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_balance_of() {
    nonpayable();
    let account = input::<32>();
    ret(&balance(&account).to_le_bytes());
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_total_supply() {
    nonpayable();
    input::<0>();
    ret(&get_u64(SUPPLY).to_le_bytes());
}
