//! # Host ABI v1 for the test guests, by hand
//!
//! What the guests link against in place of `blockula-program` (docs/PROGRAMS.md 12.1), which
//! doesn't exist yet: the `blockula` imports in [`sys`], named and typed exactly as
//! `blockula_vm::abi::HOST_FNS` lists them (`node/vm/tests/guests.rs` checks every built module
//! against that table), thin wrappers over them, and a panic handler.
//!
//! No allocator: every buffer is a fixed-size array on the 64 KiB stack, so nothing ever asks for
//! more memory (it couldn't: memory is 32/32 pages and never grows).
//!
//! **Encoding.** Inputs, stored values, return data and event data are fixed little-endian
//! layouts, each documented by the program that uses it: `u64` as 8 bytes LE, an address as its
//! 32 bytes. A method aborts with [`code::BAD_INPUT`] unless its input is exactly its layout's
//! length, so a wrong-sized input never reads as a short or padded one.

#![no_std]

use core::mem::MaybeUninit;

/// An account or program address, as the host reads and writes them (`self_address`, `caller`,
/// `transfer`'s recipient).
pub type Address = [u8; 32];

/// The raw imports. On wasm32 a pointer or `usize` is an `i32`, so these lower to exactly the
/// signatures in `HOST_FNS`. Only what a program calls is imported: the linker drops the rest.
pub mod sys {
    #[link(wasm_import_module = "blockula")]
    unsafe extern "C" {
        pub safe fn input_len_v1() -> i32;
        pub fn input_read_v1(out: *mut u8);
        pub fn return_data_v1(ptr: *const u8, len: usize);
        pub fn abort_v1(code: i32, msg: *const u8, msg_len: usize);
        pub fn debug_log_v1(ptr: *const u8, len: usize);

        pub fn storage_read_v1(key: *const u8, key_len: usize, out: *mut u8, cap: usize) -> i32;
        pub fn storage_write_v1(key: *const u8, key_len: usize, val: *const u8, val_len: usize, flags: i32);
        pub fn storage_remove_v1(key: *const u8, key_len: usize) -> i32;

        pub fn self_address_v1(out: *mut u8);
        pub fn caller_v1(out: *mut u8);
        pub safe fn value_v1() -> i64;
        pub safe fn block_height_v1() -> i64;

        pub safe fn self_balance_v1() -> i64;
        pub fn transfer_v1(to: *const u8, amount: i64, flags: i32) -> i32;

        #[allow(clippy::too_many_arguments)]
        pub fn call_v1(program: *const u8, expect: *const u8, method: *const u8, method_len: usize, input: *const u8, input_len: usize, value: i64, fuel: i64) -> i32;
        #[allow(clippy::too_many_arguments)]
        pub fn try_call_v1(program: *const u8, expect: *const u8, method: *const u8, method_len: usize, input: *const u8, input_len: usize, value: i64, fuel: i64) -> i32;
        pub fn last_return_v1(out: *mut u8);
        pub safe fn last_return_len_v1() -> i32;
        pub safe fn last_abort_code_v1() -> i32;

        pub fn emit_v1(name: *const u8, name_len: usize, data: *const u8, data_len: usize);
    }
}

/// Abort codes the wrappers use. Programs number their own from 10, so a receipt's `abort_code`
/// says whether the program's logic refused or its plumbing did.
pub mod code {
    /// The input isn't exactly the method's layout.
    pub const BAD_INPUT: u32 = 1;
    /// FANG was sent to a method that takes none (the interface's `payable` check, 8.1, done by
    /// hand: the guests have no interfaces).
    pub const NOT_PAYABLE: u32 = 2;
    /// A stored value isn't the length this program writes there: never, unless the host is wrong.
    pub const BAD_STORED: u32 = 3;
    /// `transfer` returned a status other than `OK`.
    pub const TRANSFER: u32 = 4;
    /// A panic: overflow, a bounds check, an `unwrap`. Never formatted: formatting `PanicInfo`
    /// would add about 3 KiB, a function table and source paths to every module.
    pub const PANIC: u32 = u32::MAX;
}

/// Statuses and flags, the same numbers as `blockula_vm::abi::{status, flags}`.
pub const OK: i32 = 0;
/// `try_call`: the callee aborted ([`last_abort_code`], and its message through [`last_return_into`]).
pub const ERR_ABORT: i32 = 2;
/// `try_call`: any other failure of the callee.
pub const ERR_FAULT: i32 = 3;
/// `transfer` to an account that doesn't exist: the creation fee comes out of the amount.
pub const OPEN_DEDUCT: i32 = 1;
/// `transfer` to an account that doesn't exist: the program burns the creation fee on top.
pub const OPEN_PAY: i32 = 2;
/// `storage_write`: the program pays the deposit from its own balance.
pub const SPONSOR: i32 = 1;

/// Stops the call with a program error: the host traps, drops this frame's writes, events and
/// transfers, and reports `code` and `msg` (at most 256 bytes) in the receipt.
#[inline(never)]
pub fn abort(code: u32, msg: &[u8]) -> ! {
    unsafe { sys::abort_v1(code as i32, msg.as_ptr(), msg.len()) };
    // abort_v1 always traps; if a host ever returned, trap here rather than run on.
    core::arch::wasm32::unreachable()
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    abort(code::PANIC, b"panic")
}

/// The call's input, which must be exactly `N` bytes.
pub fn input<const N: usize>() -> [u8; N] {
    if sys::input_len_v1() as usize != N {
        abort(code::BAD_INPUT, b"input length");
    }
    // Not zeroed first: the host writes all N bytes, and zeroing links in compiler-builtins'
    // memset (428 bytes of code, without bulk memory) for nothing.
    let mut buf = core::mem::MaybeUninit::<[u8; N]>::uninit();
    if N > 0 {
        unsafe { sys::input_read_v1(buf.as_mut_ptr().cast()) };
    }
    unsafe { buf.assume_init() }
}

/// Sets this call's return data (the last call wins).
pub fn ret(data: &[u8]) {
    unsafe { sys::return_data_v1(data.as_ptr(), data.len()) }
}

/// A no-op on consensus nodes; printed by dev nodes' simulation.
pub fn debug_log(msg: &[u8]) {
    unsafe { sys::debug_log_v1(msg.as_ptr(), msg.len()) }
}

/// The call's input, read into `buf`: aborts with [`code::BAD_INPUT`] if it's longer. For inputs
/// whose length varies; `buf` needn't be initialized, so no zeroing is linked in.
pub fn input_into(buf: &mut [MaybeUninit<u8>]) -> &[u8] {
    let n = sys::input_len_v1() as usize;
    if n > buf.len() {
        abort(code::BAD_INPUT, b"input length");
    }
    if n > 0 {
        unsafe { sys::input_read_v1(buf.as_mut_ptr().cast()) };
    }
    // SAFETY: the host wrote all n bytes.
    unsafe { core::slice::from_raw_parts(buf.as_ptr().cast(), n) }
}

/// The stored value under `key`, which must be exactly `N` bytes, or `None` if absent. (The host
/// copies at most `N` bytes and returns the full length, so a longer value aborts here.)
pub fn get<const N: usize>(key: &[u8]) -> Option<[u8; N]> {
    // Not zeroed first, as in `input`: it's used only once the host wrote all N bytes.
    let mut out = core::mem::MaybeUninit::<[u8; N]>::uninit();
    let n = unsafe { sys::storage_read_v1(key.as_ptr(), key.len(), out.as_mut_ptr().cast(), N) };
    match n {
        -1 => None,
        n if n as usize == N => Some(unsafe { out.assume_init() }),
        _ => abort(code::BAD_STORED, b"stored length"),
    }
}

/// A stored `u64`, 0 if absent.
pub fn get_u64(key: &[u8]) -> u64 {
    get::<8>(key).map_or(0, u64::from_le_bytes)
}

/// Writes `val` under `key`; the signer pays any deposit (no `SPONSOR` flag).
pub fn put(key: &[u8], val: &[u8]) {
    unsafe { sys::storage_write_v1(key.as_ptr(), key.len(), val.as_ptr(), val.len(), 0) }
}

/// Removes `key`, refunding its deposit to whoever paid it; whether it was there.
pub fn remove(key: &[u8]) -> bool {
    unsafe { sys::storage_remove_v1(key.as_ptr(), key.len()) == 1 }
}

pub fn self_address() -> Address {
    let mut a = [0u8; 32];
    unsafe { sys::self_address_v1(a.as_mut_ptr()) };
    a
}

/// Who called: the transaction's account in the top frame, the calling program in a nested one.
pub fn caller() -> Address {
    let mut a = [0u8; 32];
    unsafe { sys::caller_v1(a.as_mut_ptr()) };
    a
}

/// FANG (drops) sent with this call, already in this program's balance when it runs.
pub fn value() -> u64 {
    sys::value_v1() as u64
}

/// Aborts with [`code::NOT_PAYABLE`] if the call carried FANG.
pub fn nonpayable() {
    if sys::value_v1() != 0 {
        abort(code::NOT_PAYABLE, b"not payable");
    }
}

pub fn block_height() -> u64 {
    sys::block_height_v1() as u64
}

pub fn self_balance() -> u64 {
    sys::self_balance_v1() as u64
}

/// Sends `amount` drops from this program's balance; the host's status (`OK`, or
/// `ERR_TRANSFER`: short balance, a missing account without an opening flag, a closed program).
pub fn transfer(to: &Address, amount: u64, flags: i32) -> i32 {
    unsafe { sys::transfer_v1(to.as_ptr(), amount as i64, flags) }
}

/// [`transfer`], aborting with [`code::TRANSFER`] unless it went through.
pub fn pay(to: &Address, amount: u64, flags: i32) {
    if transfer(to, amount, flags) != OK {
        abort(code::TRANSFER, b"transfer failed");
    }
}

/// Calls `method` on `program` (pinned to version digest `expect`, or any version if zeros) with
/// `value` drops and at most `fuel` (0: all that's left); traps this frame if the callee fails.
/// The callee's return data length; read it with [`last_return_into`].
pub fn call(program: &Address, expect: &[u8; 32], method: &[u8], input: &[u8], value: u64, fuel: u64) -> usize {
    let n = unsafe {
        sys::call_v1(program.as_ptr(), expect.as_ptr(), method.as_ptr(), method.len(), input.as_ptr(), input.len(), value as i64, fuel as i64)
    };
    n as usize
}

/// [`call`] that returns a status (`OK`, `ERR_ABORT`, `ERR_FAULT`) instead of trapping.
pub fn try_call(program: &Address, expect: &[u8; 32], method: &[u8], input: &[u8], value: u64, fuel: u64) -> i32 {
    unsafe {
        sys::try_call_v1(program.as_ptr(), expect.as_ptr(), method.as_ptr(), method.len(), input.as_ptr(), input.len(), value as i64, fuel as i64)
    }
}

/// The length of the last callee's return data (its abort message after [`try_call`] returned
/// `ERR_ABORT`; 0 after `ERR_FAULT` or before any call).
pub fn last_return_len() -> usize {
    sys::last_return_len_v1() as usize
}

/// The last callee's abort code after [`try_call`] returned `ERR_ABORT`, else 0.
pub fn last_abort_code() -> i32 {
    sys::last_abort_code_v1()
}

/// The last callee's return data (or abort message), read into `buf`; `None` if it's longer than
/// `buf`. `last_return_v1` copies all of it, so the length is checked first.
pub fn last_return_into(buf: &mut [MaybeUninit<u8>]) -> Option<&[u8]> {
    let n = last_return_len();
    if n > buf.len() {
        return None;
    }
    unsafe { sys::last_return_v1(buf.as_mut_ptr().cast()) };
    // SAFETY: the host wrote all n bytes.
    Some(unsafe { core::slice::from_raw_parts(buf.as_ptr().cast(), n) })
}

/// Emits event `name` (an identifier) with `data`; dropped if this frame rolls back.
pub fn emit(name: &[u8], data: &[u8]) {
    unsafe { sys::emit_v1(name.as_ptr(), name.len(), data.as_ptr(), data.len()) }
}

/// The `u64` at `b[at..at + 8]`, little-endian.
pub fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut x = [0u8; 8];
    x.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(x)
}

/// The address at `b[at..at + 32]`.
pub fn addr_at(b: &[u8], at: usize) -> Address {
    let mut x = [0u8; 32];
    x.copy_from_slice(&b[at..at + 32]);
    x
}
