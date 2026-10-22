//! # counter: the smallest stateful program
//!
//! One `u64` in storage, the M1 demo's program (docs/PROGRAMS-PLAN.md) and the cheapest real call
//! for calibration: one read, one write, one return.
//!
//! **Storage.** `b"n"` → the count, `u64` LE (absent reads as 0).
//!
//! | Method | Input | Returns | Aborts |
//! |---|---|---|---|
//! | `increment` | empty | the new count, `u64` LE | 1 `BAD_INPUT` |
//! | `get` | empty | the count, `u64` LE | 1 `BAD_INPUT` |
//!
//! It doesn't check the call's value: refusing FANG sent to a non-payable method is the host's
//! job (8.1), and this program stays the minimal one. The token and the escrow check by hand.

#![no_std]

use abi::{get_u64, input, put, ret};

const COUNT: &[u8] = b"n";

#[unsafe(no_mangle)]
pub extern "C" fn bk_increment() {
    input::<0>();
    let n = get_u64(COUNT) + 1;
    put(COUNT, &n.to_le_bytes());
    ret(&n.to_le_bytes());
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_get() {
    input::<0>();
    ret(&get_u64(COUNT).to_le_bytes());
}
