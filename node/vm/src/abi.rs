//! Host ABI v1 (docs/PROGRAMS.md 10): the functions a program may import, from module
//! `blockula`, each named with its ABI version. Engine-neutral: no engine type appears here, so
//! the engine behind `engine.rs` can change without changing what programs see.
//!
//! This is a subset of 10.2: input and output, storage, context, transfers, cross-program calls
//! and events, plus `last_return_len_v1` and `last_abort_code_v1`, which 10.2 lacks. Functions
//! are only ever added, and only with a protocol version (10.1), so what this table holds when
//! programs activate is frozen; the rest of 10.2 (`storage_next`, the beacon, names, crypto,
//! `origin_session`, `caller_auth`, `epoch`, `chain_instance`, `tx_hash`) comes with a later one.

/// The import module every host function lives in.
pub const MODULE: &str = "blockula";

/// A Wasm value type at the host boundary: pointers and lengths are `i32`, amounts and heights
/// `i64` (MVP has only signed integers; amounts are `u64` bit patterns at most [`MAX_SUPPLY`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    I32,
    I64,
}

/// One host function: its import name and exact signature.
#[derive(Clone, Copy, Debug)]
pub struct HostFn {
    pub name: &'static str,
    pub params: &'static [Ty],
    pub results: &'static [Ty],
}

use Ty::{I32, I64};

const fn f(name: &'static str, params: &'static [Ty], results: &'static [Ty]) -> HostFn {
    HostFn { name, params, results }
}

/// Host ABI v1. An import not listed here, or listed with another signature, fails upload
/// validation (`validate.rs`).
///
/// # Reading into a buffer
///
/// `storage_read_v1(key_ptr, key_len, out_ptr, cap)` returns −1 if the key is absent. Otherwise
/// it copies `min(len, cap)` bytes of the value to `out_ptr` and returns the value's full `len`,
/// so a buffer too small gets the value's first `cap` bytes and the length to retry with. Only the
/// bytes it copies must lie in memory (`cap` is never a length to check on its own); the fuel is
/// for the whole value, which the host read either way.
///
/// # After a call
///
/// Before a frame's first call there's nothing to read (length 0, code 0), and each `call_v1`/
/// `try_call_v1` first clears what the last one left. Then:
///
/// | The callee | `call_v1` | `try_call_v1` | `last_return_v1` copies | `last_abort_code_v1` |
/// |---|---|---|---|---|
/// | returned | its return length | `OK` | its return data (at most `MAX_RETURN`) | 0 |
/// | aborted | traps the caller | `ERR_ABORT` | its abort message, sanitized as receipts show it (at most `MAX_ABORT_MESSAGE` bytes) | its code |
/// | failed otherwise | traps the caller | `ERR_FAULT` | nothing (length 0) | 0 |
///
/// `last_return_v1(out_ptr)` takes no capacity: it copies `last_return_len_v1()` bytes, so a
/// caller reads that first and brings a buffer big enough (it never needs more than `MAX_RETURN`).
/// The abort code has its own function so a caller can branch on it (a token's "balance short",
/// say) without parsing text; the message is for people.
pub const HOST_FNS: &[HostFn] = &[
    // Input and output.
    f("input_len_v1", &[], &[I32]),
    f("input_read_v1", &[I32], &[]),                    // (out_ptr): copies the whole input
    f("return_data_v1", &[I32, I32], &[]),              // (ptr, len): at most MAX_RETURN bytes
    f("abort_v1", &[I32, I32, I32], &[]),               // (code, msg_ptr, msg_len): always traps
    f("debug_log_v1", &[I32, I32], &[]),                // (ptr, len): a no-op on consensus nodes
    // Storage (the program's own).
    // (key_ptr, key_len, out_ptr, cap) -> len | -1: see "Reading into a buffer" above.
    f("storage_read_v1", &[I32, I32, I32, I32], &[I32]),
    f("storage_write_v1", &[I32, I32, I32, I32, I32], &[]), // (key_ptr, key_len, val_ptr, val_len, flags)
    f("storage_remove_v1", &[I32, I32], &[I32]),        // (key_ptr, key_len) -> 1 removed | 0 absent
    // Context.
    f("self_address_v1", &[I32], &[]),                  // (out32)
    f("caller_v1", &[I32], &[]),                        // (out32)
    f("value_v1", &[], &[I64]),
    f("block_height_v1", &[], &[I64]),
    // FANG.
    f("self_balance_v1", &[], &[I64]),
    f("transfer_v1", &[I32, I64, I32], &[I32]),         // (to32, amount, flags) -> status
    // Calls.
    f("call_v1", &[I32, I32, I32, I32, I32, I32, I64, I64], &[I32]), // (program32, expect32, method_ptr, method_len, input_ptr, input_len, value, fuel) -> return len; traps if the callee fails
    f("try_call_v1", &[I32, I32, I32, I32, I32, I32, I64, I64], &[I32]), // same arguments -> status
    // (out_ptr): copies all of the last callee's return data (after `ERR_ABORT`, its abort
    // message); `last_return_len_v1` says how much that is.
    f("last_return_v1", &[I32], &[]),
    f("last_return_len_v1", &[], &[I32]),               // the length `last_return_v1` would copy
    f("last_abort_code_v1", &[], &[I32]),               // the code after `try_call` returned ERR_ABORT, else 0
    // Events.
    f("emit_v1", &[I32, I32, I32, I32], &[]),           // (name_ptr, name_len, data_ptr, data_len)
];

/// The host function named `name`, if ABI v1 has it.
pub fn host_fn(name: &str) -> Option<&'static HostFn> {
    HOST_FNS.iter().find(|h| h.name == name)
}

/// Status codes host functions return.
pub mod status {
    pub const OK: i32 = 0;
    /// A transfer the program can't make: its balance is short (with `OPEN_PAY`, short of the
    /// amount and the creation fee), or the recipient doesn't exist and either no opening flag was
    /// passed or it would open with less than `config::MIN_NEW_ACCOUNT` (with `OPEN_DEDUCT`, the
    /// amount less the creation fee; with `OPEN_PAY`, the amount).
    pub const ERR_TRANSFER: i32 = 1;
    /// `try_call`: the callee aborted. Its code is `last_abort_code_v1()`, its message
    /// `last_return_v1` (see [`HOST_FNS`](super::HOST_FNS)).
    pub const ERR_ABORT: i32 = 2;
    /// `try_call`: any host-level failure of the callee (out of fuel, a trap, the storage limit,
    /// an unbound program): one code, so a caller can't branch on the signer's limits.
    pub const ERR_FAULT: i32 = 3;
}

/// Flags of `storage_write` and `transfer`.
pub mod flags {
    /// `storage_write`: the program pays the deposit from its own balance (6.3).
    pub const SPONSOR: i32 = 1;
    /// `transfer` to an account that doesn't exist: the creation fee comes out of the amount, and
    /// what's left, at least `config::MIN_NEW_ACCOUNT`, opens it.
    pub const OPEN_DEDUCT: i32 = 1;
    /// `transfer` to an account that doesn't exist: the program burns the creation fee on top of
    /// the amount, at least `config::MIN_NEW_ACCOUNT`, which opens it.
    pub const OPEN_PAY: i32 = 2;
}

/// Most FANG there can ever be, in drops (10⁹ FANG). Amounts cross the host boundary as `u64`
/// bit patterns at most this, so a negative `i64` is never an amount.
pub const MAX_SUPPLY: u64 = 1_000_000_000 * 1_000_000_000;

/// Exports a program may have besides `memory`: functions named `bk_<method>`, of type `[] -> []`.
pub const EXPORT_PREFIX: &str = "bk_";

/// Whether `method` is a valid method identifier: `[a-z][a-z0-9_]{0,31}`.
pub fn is_ident(method: &str) -> bool {
    let b = method.as_bytes();
    !b.is_empty() && b.len() <= 32 && b[0].is_ascii_lowercase() && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}
