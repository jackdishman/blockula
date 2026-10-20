//! The host functions of ABI v1 (docs/PROGRAMS.md 10), over a read-only [`View`] of the chain
//! and the transaction's overlay. Engine-neutral: the engines call [`Host::call`] with the
//! import's index in `abi::HOST_FNS` and the calling instance behind [`Instance`].
//!
//! Every function follows 10.1's calling convention: a read-only frame's writes trap first
//! (`ReadOnly`), then scalar arguments are checked (a length past its cap, an amount above
//! `MAX_SUPPLY` or unknown flags trap as misuse), then the fuel is charged before any work, then
//! pointers are bounds-checked against the instance's memory before any copy. Costs come from
//! `meter::HostCosts`; every call pays `base` on top.

use crate::abi::{self, MAX_SUPPLY, flags, status};
use crate::config::{MAX_ABORT_MESSAGE, MAX_EVENT_BYTES, MAX_INPUT, MAX_KEY_BYTES, MAX_RETURN, MAX_VALUE_BYTES};
use crate::engine::{Address, CodeHash, Digest, Host, Instance, NodeFault, Trap, TrapKind};
use crate::exec::{Opener, Run};
use crate::overlay::{Entry, Event};
use std::ops::Range;

/// The chain as execution reads it: never written (4.4). Every answer must be a function of the
/// state at the executing block's height (before the transaction), so every node gets the same
/// ones. The node's is its state at that height; tests use `testing::MemHost`.
pub trait View {
    /// The executing block's height.
    fn height(&self) -> u64;
    /// An account's balance in drops, or `None` if it doesn't exist (programs are accounts).
    fn balance(&self, account: &Address) -> Option<u64>;
    /// A program's code in force at this height (7.3), or `None` if `account` isn't an active
    /// program.
    fn code_of(&self, program: &Address) -> Option<CodeHash>;
    /// A program's version digest in force at this height (7.3, 11.2), or `None` if `account`
    /// isn't an active program. What a call's `version_digest`, a binding's and `call_v1`'s
    /// `expect` are compared with.
    fn version_digest(&self, program: &Address) -> Option<Digest>;
    /// A method of a program's interface in force at this height: its kind and whether it takes
    /// value (11.1), or `None` if the interface has no method `name`. The interface matches the
    /// code's `bk_` exports one to one (7.1).
    fn method(&self, program: &Address, name: &str) -> Option<MethodInfo>;
    /// One of a program's storage entries.
    fn storage(&self, program: &Address, key: &[u8]) -> Option<Entry>;
    /// Bytes program state holds (6.2's accounted bytes: code, interfaces, entry capacities,
    /// records), before this transaction.
    fn state_bytes(&self) -> u64;
    /// Drops per accounted byte a write bonds when it brings program state to `used` bytes, or
    /// `None` if `used` passes the ceiling (6.3: `storage::price` over the chain's parameters).
    fn storage_price(&self, used: u64) -> Option<u64>;
    /// What opening an account burns.
    fn account_creation_fee(&self) -> u64;
}

/// A method's kind (11.1): what may call it and what it may do (8.1, 8.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodKind {
    /// Called by `CallProgram` and by other programs.
    Call,
    /// Read-only: its frame (and every frame it opens) may read, but not write, remove, transfer,
    /// emit, receive value or call a non-`View` method. Other programs may call it; a top-level
    /// call runs it only for a read-only call (8.5).
    View,
    /// Runs once, from `CreateProgram`: never callable by `CallProgram` or another program.
    Init,
}

/// What execution enforces from a method's interface entry (11.1): its kind and whether it takes
/// value. Nothing else in the interface reaches execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodInfo {
    pub kind: MethodKind,
    /// Whether a call may send it value (`value > 0`).
    pub payable: bool,
}

const fn eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// `name`'s index in `abi::HOST_FNS`, at compile time.
const fn index(name: &str) -> usize {
    let mut i = 0;
    while i < abi::HOST_FNS.len() {
        if eq(abi::HOST_FNS[i].name, name) {
            return i;
        }
        i += 1;
    }
    panic!("not in ABI v1")
}

const INPUT_LEN: usize = index("input_len_v1");
const INPUT_READ: usize = index("input_read_v1");
const RETURN_DATA: usize = index("return_data_v1");
const ABORT: usize = index("abort_v1");
const DEBUG_LOG: usize = index("debug_log_v1");
const STORAGE_READ: usize = index("storage_read_v1");
const STORAGE_WRITE: usize = index("storage_write_v1");
const STORAGE_REMOVE: usize = index("storage_remove_v1");
const SELF_ADDRESS: usize = index("self_address_v1");
const CALLER: usize = index("caller_v1");
const VALUE: usize = index("value_v1");
const BLOCK_HEIGHT: usize = index("block_height_v1");
const SELF_BALANCE: usize = index("self_balance_v1");
const TRANSFER: usize = index("transfer_v1");
const CALL: usize = index("call_v1");
const TRY_CALL: usize = index("try_call_v1");
const LAST_RETURN: usize = index("last_return_v1");
const LAST_RETURN_LEN: usize = index("last_return_len_v1");
const LAST_ABORT_CODE: usize = index("last_abort_code_v1");
const EMIT: usize = index("emit_v1");

/// A pointer argument and a length as a range of memory, or a trap.
fn range(memory: &[u8], ptr: i64, len: usize) -> Result<Range<usize>, Trap> {
    let start = ptr as i32 as u32 as usize;
    let end = start.checked_add(len).filter(|end| *end <= memory.len()).ok_or(TrapKind::MemoryAccess)?;
    Ok(start..end)
}

fn read(instance: &mut dyn Instance, ptr: i64, len: usize) -> Result<Vec<u8>, Trap> {
    let memory = instance.memory();
    Ok(memory[range(memory, ptr, len)?].to_vec())
}

fn read32(instance: &mut dyn Instance, ptr: i64) -> Result<[u8; 32], Trap> {
    let memory = instance.memory();
    let bytes = &memory[range(memory, ptr, 32)?];
    Ok(<[u8; 32]>::try_from(bytes).map_err(|_| NodeFault::Host("a 32-byte range of another length"))?)
}

fn write(instance: &mut dyn Instance, ptr: i64, data: &[u8]) -> Result<(), Trap> {
    let memory = instance.memory();
    let r = range(memory, ptr, data.len())?;
    memory[r].copy_from_slice(data);
    Ok(())
}

/// A length argument, at most `cap`.
fn length(arg: i64, cap: usize) -> Result<usize, Trap> {
    let n = arg as i32 as u32 as usize;
    if n > cap { Err(TrapKind::Length.into()) } else { Ok(n) }
}

/// An amount (or fuel) argument: a `u64` bit pattern at most `MAX_SUPPLY`, so never negative.
fn amount(arg: i64) -> Result<u64, Trap> {
    let a = arg as u64;
    if a > MAX_SUPPLY { Err(TrapKind::Amount.into()) } else { Ok(a) }
}

fn per_byte(cost: u64, bytes: usize) -> u64 {
    cost.saturating_mul(bytes as u64)
}

/// An abort message as receipts may show it: control, format (bidi included) and private-use
/// characters replaced, at most `MAX_ABORT_MESSAGE` bytes.
pub fn sanitize(bytes: &[u8]) -> String {
    let unsafe_char = |c: char| {
        c.is_control()
            || matches!(c as u32,
                0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x890..=0x891 | 0x8E2 | 0x180E
                | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF
                | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3
                | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F
                | 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
    };
    let mut out = String::new();
    for c in String::from_utf8_lossy(bytes).chars() {
        let c = if unsafe_char(c) { char::REPLACEMENT_CHARACTER } else { c };
        if out.len() + c.len_utf8() > MAX_ABORT_MESSAGE {
            break;
        }
        out.push(c);
    }
    out
}

impl Host for Run<'_> {
    fn call(&mut self, index: usize, args: &[i64], instance: &mut dyn Instance) -> Result<i64, Trap> {
        // The engine linked each import with the ABI's exact signature, so this always holds; it's
        // checked once here so every `args[i]` below is in bounds.
        let f = abi::HOST_FNS.get(index).ok_or(NodeFault::Host("an import outside ABI v1"))?;
        if args.len() != f.params.len() {
            return Err(NodeFault::Host("arguments unlike the ABI's").into());
        }
        let h = self.schedule.host;
        let context = h.base.saturating_add(h.context);
        match index {
            INPUT_LEN => {
                instance.charge(h.base)?;
                Ok(self.frame()?.input.len() as i64)
            }
            INPUT_READ => {
                instance.charge(h.base.saturating_add(per_byte(h.per_byte, self.frame()?.input.len())))?;
                write(instance, args[0], &self.frame()?.input)?;
                Ok(0)
            }
            RETURN_DATA => {
                let len = length(args[1], MAX_RETURN)?;
                instance.charge(h.base.saturating_add(per_byte(h.per_byte, len)))?;
                self.frame_mut()?.ret = read(instance, args[0], len)?;
                Ok(0)
            }
            ABORT => {
                let len = (args[2] as i32 as u32 as usize).min(MAX_ABORT_MESSAGE);
                instance.charge(h.base.saturating_add(h.abort).saturating_add(per_byte(h.per_byte, len)))?;
                let message = sanitize(&read(instance, args[1], len)?);
                // The code is the `i32` argument's bit pattern, read as the `u32` receipts hold.
                Err(Trap::Abort { code: args[0] as i32 as u32, message })
            }
            DEBUG_LOG => {
                // A no-op on consensus nodes: priced like `emit` (10.2) and bounds-checked, never
                // copied.
                let len = args[1] as i32 as u32 as usize;
                instance.charge(h.base.saturating_add(h.emit).saturating_add(per_byte(h.emit_per_byte, len)))?;
                range(instance.memory(), args[0], len)?;
                Ok(0)
            }
            STORAGE_READ => {
                let key_len = length(args[1], MAX_KEY_BYTES)?;
                instance.charge(h.base.saturating_add(h.storage_read).saturating_add(per_byte(h.storage_read_per_byte, key_len)))?;
                let key = read(instance, args[0], key_len)?;
                let Some(entry) = self.overlay.read(self.view, &self.frame()?.program, &key) else { return Ok(-1) };
                instance.charge(per_byte(h.storage_read_per_byte, entry.value.len()))?;
                // `abi::HOST_FNS`, "Reading into a buffer": the first `cap` bytes at most, and
                // the full length (to retry with, if the buffer was too small).
                let cap = args[3] as i32 as u32 as usize;
                write(instance, args[2], &entry.value[..entry.value.len().min(cap)])?;
                Ok(entry.value.len() as i64)
            }
            STORAGE_WRITE => {
                self.writable()?;
                let key_len = length(args[1], MAX_KEY_BYTES)?;
                let value_len = length(args[3], MAX_VALUE_BYTES)?;
                let f = args[4] as i32;
                if f & !flags::SPONSOR != 0 {
                    return Err(TrapKind::Flags.into());
                }
                let cost = h.base.saturating_add(h.storage_write).saturating_add(per_byte(h.storage_write_per_byte, key_len + value_len));
                instance.charge(cost)?;
                let key = read(instance, args[0], key_len)?;
                let value = read(instance, args[2], value_len)?;
                let program = self.frame()?.program;
                let payer = if f & flags::SPONSOR != 0 { program } else { self.signer };
                self.overlay.write(self.view, program, &key, value, payer, self.storage_limit)?;
                Ok(0)
            }
            STORAGE_REMOVE => {
                self.writable()?;
                let key_len = length(args[1], MAX_KEY_BYTES)?;
                instance.charge(h.base.saturating_add(h.storage_remove))?;
                let key = read(instance, args[0], key_len)?;
                let program = self.frame()?.program;
                Ok(i64::from(self.overlay.remove(self.view, program, &key)?))
            }
            SELF_ADDRESS => {
                instance.charge(context)?;
                write(instance, args[0], &self.frame()?.program)?;
                Ok(0)
            }
            CALLER => {
                instance.charge(context)?;
                write(instance, args[0], &self.frame()?.caller)?;
                Ok(0)
            }
            VALUE => {
                instance.charge(context)?;
                Ok(self.frame()?.value as i64)
            }
            BLOCK_HEIGHT => {
                instance.charge(context)?;
                Ok(self.view.height() as i64)
            }
            SELF_BALANCE => {
                instance.charge(context)?;
                let balance = self.overlay.balance(self.view, &self.frame()?.program);
                Ok(i64::try_from(balance).map_err(|_| NodeFault::Host("a balance outside i64"))?)
            }
            TRANSFER => {
                self.writable()?;
                let value = amount(args[1])?;
                let f = args[2] as i32;
                if f & !(flags::OPEN_DEDUCT | flags::OPEN_PAY) != 0 || f == flags::OPEN_DEDUCT | flags::OPEN_PAY {
                    return Err(TrapKind::Flags.into());
                }
                instance.charge(h.base.saturating_add(h.transfer))?;
                let to = read32(instance, args[0])?;
                let from = self.frame()?.program;
                let fee = self.view.account_creation_fee();
                Ok(i64::from(self.overlay.transfer(self.view, from, to, value, f, fee)))
            }
            CALL => self.call_program(args, instance, false),
            TRY_CALL => self.call_program(args, instance, true),
            LAST_RETURN => {
                instance.charge(h.base.saturating_add(per_byte(h.per_byte, self.frame()?.last_return.len())))?;
                write(instance, args[0], &self.frame()?.last_return)?;
                Ok(0)
            }
            LAST_RETURN_LEN => {
                instance.charge(h.base)?;
                Ok(self.frame()?.last_return.len() as i64)
            }
            LAST_ABORT_CODE => {
                instance.charge(h.base)?;
                // The `u32` code's bit pattern, as the `i32` the import returns.
                Ok(i64::from(self.frame()?.last_abort_code as i32))
            }
            EMIT => {
                self.writable()?;
                let name_len = length(args[1], 32)?;
                let data_len = length(args[3], MAX_EVENT_BYTES)?;
                instance.charge(h.base.saturating_add(h.emit).saturating_add(per_byte(h.emit_per_byte, name_len + data_len)))?;
                let name = read(instance, args[0], name_len)?;
                let name = String::from_utf8(name).ok().filter(|n| abi::is_ident(n)).ok_or(TrapKind::Name)?;
                let data = read(instance, args[2], data_len)?;
                let program = self.frame()?.program;
                self.overlay.emit(Event { program, name, data })?;
                Ok(0)
            }
            _ => Err(NodeFault::Host("an import outside ABI v1").into()),
        }
    }
}

impl Run<'_> {
    /// Traps `ReadOnly` in a frame running a `View` method (4.4): what writes, removes,
    /// transfers or emits calls this first.
    fn writable(&self) -> Result<(), Trap> {
        if self.frame()?.read_only { Err(TrapKind::ReadOnly.into()) } else { Ok(()) }
    }

    /// `call_v1` and `try_call_v1` (8.2): `(program32, expect32, method_ptr, method_len,
    /// input_ptr, input_len, value, fuel)`. `expect` is a version digest, zeros for any. `fuel` is
    /// the most the callee may use; 0 means all the caller has left. The caller pays the call and
    /// the callee's instantiation first.
    ///
    /// `call` returns the callee's return length (read it with `last_return`) and traps if the
    /// callee fails, with the callee's trap. `try_call` returns a status: `OK`, `ERR_ABORT` (the
    /// code is then `last_abort_code`, the sanitized message `last_return`) or `ERR_FAULT` for
    /// every other failure (nothing to read), the callee's layer dropped and the caller running
    /// on: `abi::HOST_FNS`, "After a call". The callee's checks ([`Run::admit`]: the declared
    /// set, reentrancy, depth, version, method kind, payability, read-only) are its failures too,
    /// so `try_call` absorbs them; the caller's own misuse (lengths, amounts, pointers, a method
    /// name that isn't an identifier) and its fuel running out trap the caller either way. A node
    /// fault (`Trap::Fault`) is never absorbed: it stops the transaction whatever the caller asked
    /// and `execute` returns it, so no node turns its own trouble into a deterministic-looking
    /// outcome.
    fn call_program(&mut self, args: &[i64], instance: &mut dyn Instance, try_call: bool) -> Result<i64, Trap> {
        let h = self.schedule.host;
        let method_len = length(args[3], 32)?;
        let input_len = length(args[5], MAX_INPUT)?;
        let value = amount(args[6])?;
        let limit = amount(args[7])?;
        instance.charge(h.base.saturating_add(h.call).saturating_add(per_byte(h.call_per_input_byte, input_len)))?;
        let program = read32(instance, args[0])?;
        let expect = read32(instance, args[1])?;
        let method = read(instance, args[2], method_len)?;
        let method = String::from_utf8(method).ok().filter(|m| abi::is_ident(m)).ok_or(TrapKind::Name)?;
        let input = read(instance, args[4], input_len)?;
        let caller = self.frame()?.program;
        let frame = self.frame_mut()?;
        frame.last_return.clear();
        frame.last_abort_code = 0;
        let frame = match self.admit(Opener::Nested { expect }, &program, &method, value) {
            Ok(frame) => frame,
            Err(trap) if try_call && !trap.is_node_fault() => return Ok(i64::from(status::ERR_FAULT)),
            Err(trap) => return Err(trap),
        };
        instance.charge(self.schedule.instantiate_fuel(&frame.info))?;
        let have = instance.fuel()?;
        let share = if limit == 0 { have } else { have.min(limit) };
        let (result, used) = self.enter(program, frame.code, caller, value, &method, input, share, frame.read_only)?;
        instance.set_fuel(have.checked_sub(used).ok_or(NodeFault::Host("a callee used more fuel than its share"))?)?;
        match result {
            Ok(ret) => {
                let len = ret.len() as i64;
                self.frame_mut()?.last_return = ret;
                Ok(if try_call { i64::from(status::OK) } else { len })
            }
            Err(Trap::Abort { code, message }) if try_call => {
                self.origin = None;
                let frame = self.frame_mut()?;
                frame.last_return = message.into_bytes();
                frame.last_abort_code = code;
                Ok(i64::from(status::ERR_ABORT))
            }
            Err(Trap::Kind(_)) if try_call => {
                self.origin = None;
                Ok(i64::from(status::ERR_FAULT))
            }
            Err(trap) => Err(trap),
        }
    }
}
