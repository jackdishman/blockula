//! # router: moves its own token balance through cross-program calls
//!
//! The guests' caller (docs/PROGRAMS.md 8.2), so the guests exercise `call_v1` (with value 0),
//! `try_call_v1`, `last_return_len_v1`, `last_return_v1` and `last_abort_code_v1`, the declared
//! program set (a transaction must bind the token, and every router it passes through) and the
//! call depth (routers forward to routers). The token sees the router as its caller, so what moves
//! is the router's own token balance. No storage and no events of its own: the token's
//! `transfer` event is the record. Every method refuses FANG ([`abi::code::NOT_PAYABLE`]).
//!
//! | Method | Input (LE) | Does | Returns | Aborts |
//! |---|---|---|---|---|
//! | `send` | hops (0 to 8 router addresses, 32 each) ‖ token 32 ‖ to 32 ‖ amount 8 | No hops: `call` token.`transfer`(to ‖ amount), then `call` token.`balance_of`(self). Else `call` hops[0].`send` with the rest of the input | the last router's token balance after the transfer, `u64` LE | 30 a callee returned other than 8 bytes; a failing callee traps this frame too |
//! | `try_send` | token 32 ‖ to 32 ‖ amount 8 (72) | `try_call` token.`transfer`(to ‖ amount) | status i32 ‖ abort code i32 ‖ return length u32 (12) ‖ the callee's return data, which after `ERR_ABORT` is its abort message | 30 a return longer than 256 bytes |
//!
//! Every method also aborts 1 `BAD_INPUT` on another input length and 2 `NOT_PAYABLE` on value.
//! Calls pin no version (`expect` is zeros) and give the callee all the fuel left.

#![no_std]

use abi::{Address, abort, call, code, input_into, last_abort_code, last_return_into, nonpayable, ret, self_address, try_call};
use core::mem::MaybeUninit;

/// token 32 ‖ to 32 ‖ amount 8: the tail of every input.
const HEAD: usize = 72;
const MAX_HOPS: usize = 8;
/// The longest return `try_send` passes on: an abort message is at most 256 bytes.
const MAX_PASSED: usize = 256;
/// Any version of the callee.
const ANY: [u8; 32] = [0; 32];

/// Abort codes.
const BAD_RETURN: u32 = 30;

const fn uninit<const N: usize>() -> [MaybeUninit<u8>; N] {
    [const { MaybeUninit::uninit() }; N]
}

fn address(b: &[u8]) -> Address {
    let mut a = [0u8; 32];
    a.copy_from_slice(b);
    a
}

/// The 8-byte return of the call that just returned `n` bytes, or an abort.
fn balance_returned(n: usize) -> [u8; 8] {
    let mut buf = uninit::<8>();
    match last_return_into(&mut buf) {
        Some(b) if n == 8 && b.len() == 8 => b.try_into().unwrap(),
        _ => abort(BAD_RETURN, b"unexpected return"),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_send() {
    nonpayable();
    let mut buf = uninit::<{ HEAD + 32 * MAX_HOPS }>();
    let inp = input_into(&mut buf);
    if inp.len() < HEAD || !(inp.len() - HEAD).is_multiple_of(32) {
        abort(code::BAD_INPUT, b"input length");
    }
    let balance = if inp.len() > HEAD {
        // Forward to the next router: it sees this one as its caller, one frame deeper.
        let n = call(&address(&inp[..32]), &ANY, b"send", &inp[32..], 0, 0);
        balance_returned(n)
    } else {
        let token = address(&inp[..32]);
        call(&token, &ANY, b"transfer", &inp[32..], 0, 0);
        let n = call(&token, &ANY, b"balance_of", &self_address(), 0, 0);
        balance_returned(n)
    };
    ret(&balance);
}

#[unsafe(no_mangle)]
pub extern "C" fn bk_try_send() {
    nonpayable();
    let inp = abi::input::<HEAD>();
    let status = try_call(&address(&inp[..32]), &ANY, b"transfer", &inp[32..], 0, 0);
    // Status, code and length first; the callee's bytes are read straight in after them.
    let mut out = uninit::<{ 12 + MAX_PASSED }>();
    let Some(passed) = last_return_into(&mut out[12..]) else {
        abort(BAD_RETURN, b"return too long");
    };
    let n = passed.len();
    let head = [status.to_le_bytes(), last_abort_code().to_le_bytes(), (n as u32).to_le_bytes()];
    for (slot, byte) in out.iter_mut().zip(head.iter().flatten()) {
        slot.write(*byte);
    }
    // SAFETY: the 12 bytes above and the n the host wrote after them.
    ret(unsafe { core::slice::from_raw_parts(out.as_ptr().cast(), 12 + n) });
}
