//! The engine-neutral boundary (docs/PROGRAMS.md 4.1, 4.3). Everything above this module (the
//! host functions, the overlay, the executor) sees only these types; a backend maps them onto one
//! engine's API, so a later protocol version can change the engine by changing a backend and the
//! pin, never the ABI, fees or state. The engine is soroban-wasmi 29.0.0 (`soroban.rs`, D1);
//! wasmi 0.38.0 (`wasmi038.rs`, `--features ref038`) is a test-only differential oracle.
//!
//! # The shape
//!
//! * **An engine per block** ([`BlockEngine`]): created, loaded with every declared code before
//!   anything runs, dropped after. Loading translates; nothing translates while a call runs
//!   (soroban-wasmi's engine holds a spin lock for the whole of a call: see `soroban.rs`).
//! * **A store per frame** ([`BlockEngine::invoke`]): every call, top-level or nested, gets a
//!   fresh store with one instance, its fuel and its memory, dropped when the frame returns. So at
//!   most `MAX_CALL_DEPTH` instances are ever live (4.3), and a store's arenas (which never shrink)
//!   can't grow with the number of calls a transaction makes.
//! * **The host is a trait object** ([`Host`]): the engine calls `Host::call` for every import,
//!   with the instance's memory and fuel behind [`Instance`]. A nested `call_v1` re-enters
//!   [`BlockEngine::invoke`] from inside `Host::call`, so native recursion is bounded by the call
//!   depth, as 4.3 requires.
//! * **One meter, many stores.** The transaction's remaining fuel lives in the running frame's
//!   store. A nested call takes its share out of the caller's store, runs it in the callee's, and
//!   puts back what's left: see `exec.rs`.

use crate::meter::Schedule;
use std::fmt;

pub mod soroban;
#[cfg(feature = "ref038")]
pub mod wasmi038;

/// A code's hash (BLAKE3 of its bytes).
pub type CodeHash = [u8; 32];
/// An account's address (a program's included).
pub type Address = [u8; 32];

/// The engines this crate builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EngineKind {
    /// soroban-wasmi 29.0.0: the engine (D1).
    Soroban,
    /// wasmi 0.38.0, the last audited wasmi release: a differential oracle for tests, never a
    /// node's engine.
    #[cfg(feature = "ref038")]
    Wasmi038,
}

impl EngineKind {
    /// Every engine built into this crate, the node's first.
    pub const ALL: &[EngineKind] = &[
        EngineKind::Soroban,
        #[cfg(feature = "ref038")]
        EngineKind::Wasmi038,
    ];

    pub fn name(self) -> &'static str {
        match self {
            EngineKind::Soroban => "soroban-wasmi 29.0.0",
            #[cfg(feature = "ref038")]
            EngineKind::Wasmi038 => "wasmi 0.38.0",
        }
    }
}

/// What instantiating a code costs to know (4.5): its memory and its data segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeInfo {
    /// Pages of its one memory (initial = maximum, so it never grows).
    pub pages: u32,
    /// Bytes of its data segments, copied into memory at instantiation.
    pub data_bytes: usize,
    /// Bytes of code.
    pub size: usize,
}

impl CodeInfo {
    /// Reads `wasm`'s memory and data sections. A structural pass only: upload validation
    /// (`validate.rs`) has already refused anything else wrong with stored code.
    pub fn read(wasm: &[u8]) -> Result<CodeInfo, LoadError> {
        use wasmparser::{Parser, Payload};
        let bad = |e: wasmparser::BinaryReaderError| LoadError::Parse(e.to_string());
        let mut info = CodeInfo { pages: 0, data_bytes: 0, size: wasm.len() };
        for payload in Parser::new(0).parse_all(wasm) {
            match payload.map_err(bad)? {
                Payload::MemorySection(memories) => {
                    for memory in memories {
                        let initial = memory.map_err(bad)?.initial;
                        info.pages = u32::try_from(initial).map_err(|_| LoadError::Parse("memory too large".into()))?;
                    }
                }
                Payload::DataSection(segments) => {
                    for segment in segments {
                        info.data_bytes = info.data_bytes.saturating_add(segment.map_err(bad)?.data.len());
                    }
                }
                _ => {}
            }
        }
        Ok(info)
    }
}

/// Why a code didn't load. Stored code passed validation at upload, so this is a node fault (an
/// engine bump that no longer accepts it, section 7.6), never a transaction's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    Parse(String),
    /// The engine refused it: its message, which differs by engine.
    Translate(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Parse(m) => write!(f, "malformed code: {m}"),
            LoadError::Translate(m) => write!(f, "the engine refused the code: {m}"),
        }
    }
}

/// Why a frame stopped. Engine-neutral, so two engines' receipts compare equal: every engine's
/// trap codes map onto [`WasmTrap`], and host functions trap with these directly (the backends
/// carry them through the engine as its host-error type and recover them on the way out).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trap {
    /// The transaction's fuel, or the share a caller gave this frame, ran out.
    OutOfFuel,
    /// The program called `abort_v1`. The message is sanitized and at most `MAX_ABORT_MESSAGE`.
    Abort { code: i32, message: String },
    /// The engine trapped while running Wasm.
    Wasm(WasmTrap),
    /// The program misused a host function (10.1): it traps, with what was wrong.
    Misuse(Misuse),
    /// A call reached a program outside the transaction's declared set (8.1).
    UnboundProgram,
    /// A call reached a program already on the stack (8.2: A → B → A).
    Reentrancy,
    /// A call would put more than `MAX_CALL_DEPTH` programs on the stack.
    CallDepth,
    /// A call's `expect` digest isn't the callee's version in force.
    VersionMismatch,
    /// The callee is in the declared set but isn't a program.
    NotAProgram,
    /// The program has no export `bk_<method>` of type `[] → []`.
    NoMethod,
    /// A payer is short: a call's value, or a storage deposit.
    Insufficient,
    /// The signer's deposits would pass the transaction's `storage_limit` (6.3).
    StorageLimit,
    /// A per-transaction bound (4.4).
    Limit(Limit),
    /// Not the program's doing: code not loaded, instantiation refused, an engine error. A node
    /// fault; the text is the engine's and differs by engine.
    Fault(String),
}

impl Trap {
    /// Whether this is the node's trouble rather than the program's: never absorbed by
    /// `try_call`, and a sign the block's engine should be rebuilt.
    pub fn is_node_fault(&self) -> bool {
        matches!(self, Trap::Fault(_))
    }
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// The engine's own traps, by cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmTrap {
    Unreachable,
    MemoryOutOfBounds,
    TableOutOfBounds,
    IndirectCallToNull,
    DivisionByZero,
    IntegerOverflow,
    BadConversionToInteger,
    /// The value stack or recursion depth (per frame: 4.3).
    StackOverflow,
    BadSignature,
    Other,
}

/// Host-function misuse (10.1), each a trap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Misuse {
    /// A pointer and length outside the instance's memory.
    MemoryAccess,
    /// A length past its cap (key, value, return data, input, method, event name).
    Length,
    /// An amount above `MAX_SUPPLY` (a negative `i64` included).
    Amount,
    /// Unknown or conflicting flags.
    Flags,
    /// A method or event name that isn't an identifier.
    Name,
}

/// The per-transaction bounds of 4.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    KeysWritten,
    WrittenBytes,
    Events,
    EventBytes,
}

/// What a host function sees of the instance that called it: its memory and the transaction's
/// meter, as that instance's store holds it.
pub trait Instance {
    /// The whole linear memory (fixed size: it never grows).
    fn memory(&mut self) -> &mut [u8];
    /// Fuel left.
    fn fuel(&self) -> u64;
    /// Sets the fuel left: the executor's way to give a callee its share and take back the rest.
    fn set_fuel(&mut self, fuel: u64);
}

impl dyn Instance + '_ {
    /// Charges `cost` before the work it pays for, or traps `OutOfFuel` leaving the fuel as it
    /// was, exactly as both engines do for Wasm blocks.
    pub fn charge(&mut self, cost: u64) -> Result<(), Trap> {
        let left = self.fuel().checked_sub(cost).ok_or(Trap::OutOfFuel)?;
        self.set_fuel(left);
        Ok(())
    }
}

/// The executor's side of the boundary: what a running program's imports reach.
pub trait Host {
    /// Runs `abi::HOST_FNS[index]` for `instance`. `args` are the Wasm arguments in order (`i32`s
    /// sign-extended); the result is ignored for functions without one and truncated to `i32` for
    /// those returning `i32`.
    fn call(&mut self, index: usize, args: &[i64], instance: &mut dyn Instance) -> Result<i64, Trap>;
}

/// How an invocation ended.
#[derive(Debug)]
pub struct Invoked {
    pub result: Result<(), Trap>,
    /// Fuel left in the frame's store. After `OutOfFuel` both engines leave what was there before
    /// the charge that failed, so the executor counts the frame's whole share as used instead.
    pub fuel_left: u64,
}

/// One per block (4.3): created, loaded with every declared code before anything runs, dropped
/// after. Never shared across blocks, so neither its memory nor its code map depends on a node's
/// history; rebuilt after any caught panic.
pub trait BlockEngine {
    fn kind(&self) -> EngineKind;
    /// Translates `wasm` (once per hash: loading a hash again is a no-op). Never called while a
    /// call is running on this engine.
    fn load(&mut self, hash: CodeHash, wasm: &[u8]) -> Result<(), LoadError>;
    /// What instantiating a loaded code costs to know; `None` if it isn't loaded.
    fn info(&self, hash: &CodeHash) -> Option<CodeInfo>;
    /// Instantiates `hash` in a fresh store holding `fuel`, runs its export `export` (`[] → []`)
    /// with `host` behind every import, and drops the store. May be re-entered from inside
    /// `host.call` (a nested `call_v1`).
    fn invoke(&self, hash: &CodeHash, export: &str, fuel: u64, host: &mut dyn Host) -> Invoked;
}

/// A fresh engine of `kind`, configured from `schedule` and the 4.2 settings.
pub fn new_block_engine(kind: EngineKind, schedule: &Schedule) -> Box<dyn BlockEngine> {
    match kind {
        EngineKind::Soroban => Box::new(soroban::SorobanEngine::new(schedule)),
        #[cfg(feature = "ref038")]
        EngineKind::Wasmi038 => Box::new(wasmi038::Wasmi038Engine::new(schedule)),
    }
}

/// The host, as a store's data holds it while one `invoke` runs.
///
/// Engines want their store data `'static` (the linker, built once per block, names its type),
/// but the host borrows the transaction's view of the chain. So the store holds a raw pointer
/// with the borrow's lifetime erased. Sound because each backend creates the store inside
/// `invoke`, from the `&mut dyn Host` it was given, and drops it before `invoke` returns: the
/// pointer never outlives the borrow, and it's only dereferenced by host functions running inside
/// that `invoke`, while the borrow it came from is otherwise unused. A nested `invoke` gets a
/// pointer reborrowed from the outer one, used only while the outer host call waits for it.
#[derive(Clone, Copy)]
pub(crate) struct HostPtr(*mut (dyn Host + 'static));

impl HostPtr {
    pub(crate) fn new(host: &mut dyn Host) -> Self {
        let ptr: *mut (dyn Host + '_) = host;
        // SAFETY: only the trait object's lifetime changes; see the type's comment.
        HostPtr(unsafe { std::mem::transmute::<*mut (dyn Host + '_), *mut (dyn Host + 'static)>(ptr) })
    }

    /// SAFETY: call only from a host function running inside the `invoke` that made this.
    pub(crate) unsafe fn get<'a>(self) -> &'a mut dyn Host {
        unsafe { &mut *self.0 }
    }
}
