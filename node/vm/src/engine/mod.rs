//! The engine-neutral boundary (docs/PROGRAMS.md 4.1, 4.3). Everything above this module (the
//! host functions, the overlay, the executor) sees only these types; a backend maps them onto one
//! engine's API, so a later protocol version can change the engine by changing a backend and the
//! pin, never the ABI, fees or state. The engine is soroban-wasmi 29.0.0 (`soroban.rs`, D1);
//! wasmi 0.38.0 (`wasmi038.rs`, `--features ref038`) is a test-only differential oracle.
//!
//! # The shape
//!
//! * **An engine per transaction** ([`BlockEngine`]): created, loaded with every code the
//!   transaction declares before anything runs, used by its `exec::execute`, and dropped after,
//!   on the transaction's own thread (a `Box<dyn BlockEngine>` isn't `Send`). Loading translates;
//!   nothing translates while a call runs (soroban-wasmi's engine holds a spin lock for the whole
//!   of a call: see `soroban.rs`). PROGRAMS.md 4.3 shares one engine across a block; the node this
//!   release ships doesn't, so nothing an engine holds can carry from one transaction to another,
//!   and each pays its own load. The API serves either; the names are 4.3's.
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

/// A code's hash: how an engine keys the code it has loaded, and nothing else (it never hashes
/// code itself). The node's is its `wasm_hash` (PROGRAMS.md 7.1: BLAKE3's `derive_key` under
/// "blockula program code v1"); `testing::MemHost`'s is plain BLAKE3.
pub type CodeHash = [u8; 32];
/// An account's address (a program's included).
pub type Address = [u8; 32];
/// A program's version digest (7.3, 11.2): what a call binds, so a call previewed against one
/// version never runs another. The node computes it; execution only compares it.
pub type Digest = [u8; 32];

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

/// What a code holds, as far as pricing it needs (4.5): its size for load, and the entities
/// instantiation creates or copies (`meter::Schedule::instantiate_fuel`). Counts are what the
/// sections declare; upload validation has checked each against the entries it holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodeInfo {
    /// Bytes of code.
    pub size: usize,
    /// Pages of its one memory (initial = maximum, so it never grows).
    pub pages: u32,
    /// Bytes of its data segments, copied into memory at instantiation.
    pub data_bytes: usize,
    /// Imported (host) functions: each is linked at instantiation.
    pub imports: u32,
    /// Exports, `memory` included.
    pub exports: u32,
    /// Functions it defines.
    pub functions: u32,
    pub globals: u32,
    pub element_segments: u32,
    /// Table elements its element segments initialize, over all of them.
    pub elements: u32,
    pub data_segments: u32,
}

impl CodeInfo {
    /// Reads `wasm`'s sections: a structural pass, no function body is parsed. Upload validation
    /// (`validate.rs`) has already refused anything else wrong with stored code, and returns the
    /// same reading in its `ModuleInfo`.
    pub fn read(wasm: &[u8]) -> Result<CodeInfo, LoadError> {
        use wasmparser::{ElementItems, Parser, Payload};
        let bad = |e: wasmparser::BinaryReaderError| LoadError::Parse(e.to_string());
        let mut info = CodeInfo { size: wasm.len(), ..CodeInfo::default() };
        for payload in Parser::new(0).parse_all(wasm) {
            match payload.map_err(bad)? {
                Payload::ImportSection(s) => info.imports = s.count(),
                Payload::FunctionSection(s) => info.functions = s.count(),
                Payload::GlobalSection(s) => info.globals = s.count(),
                Payload::ExportSection(s) => info.exports = s.count(),
                Payload::MemorySection(memories) => {
                    for memory in memories {
                        let initial = memory.map_err(bad)?.initial;
                        info.pages = u32::try_from(initial).map_err(|_| LoadError::Parse("memory too large".into()))?;
                    }
                }
                Payload::ElementSection(segments) => {
                    info.element_segments = segments.count();
                    for segment in segments {
                        let count = match segment.map_err(bad)?.items {
                            ElementItems::Functions(items) => items.count(),
                            ElementItems::Expressions(_, items) => items.count(),
                        };
                        info.elements = info.elements.saturating_add(count);
                    }
                }
                Payload::DataSection(segments) => {
                    info.data_segments = segments.count();
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

/// Why a code didn't load. Stored code passed validation and a test translation at upload, so
/// on a node this is a node fault (an engine bump that no longer accepts it, section 7.6), never a
/// transaction's. The text is diagnostic, for logs: it never reaches a receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    Parse(String),
    /// The engine refused it, in the engine's words.
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

/// Why a frame stopped, as receipts record it (docs/PROGRAMS.md 6.5): a number, never the
/// engine's or the host's text, so no release that rewords an error, and no engine bump that
/// renames a trap, can change a receipt. Every cause a program (or its caller) can bring about has
/// its own kind; what no valid program can cause is a [`NodeFault`] instead.
///
/// **The numbers are consensus** once receipts are committed (6.5): a kind keeps its number
/// forever, a new kind takes the next unused number with a protocol version, and none is reused.
/// `exec::tests` pins the table.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TrapKind {
    /// The transaction's fuel, or the share a caller gave this frame, ran out.
    OutOfFuel = 1,
    // The engine's traps: Wasm's own semantics.
    /// `unreachable` executed.
    Unreachable = 2,
    /// A load or store outside the instance's memory.
    MemoryOutOfBounds = 3,
    /// `call_indirect` past the table's end.
    TableOutOfBounds = 4,
    /// `call_indirect` of an empty table entry.
    IndirectCallToNull = 5,
    /// Integer division or remainder by zero.
    DivisionByZero = 6,
    /// Signed division overflow (`INT_MIN / -1`).
    IntegerOverflow = 7,
    /// The value stack or the recursion depth (per frame: 4.3).
    StackOverflow = 8,
    /// `call_indirect` of a function of another type.
    BadSignature = 9,
    // Host-function misuse (10.1).
    /// A pointer and length outside the instance's memory.
    MemoryAccess = 10,
    /// A length past its cap (key, value, return data, input, method, event name).
    Length = 11,
    /// An amount or fuel argument above `MAX_SUPPLY` (a negative `i64` included).
    Amount = 12,
    /// Unknown or conflicting flags.
    Flags = 13,
    /// A method or event name that isn't an identifier.
    Name = 14,
    // Calls (8.1, 8.2).
    /// A call reached a program outside the transaction's declared set.
    UnboundProgram = 15,
    /// A call reached a program already on the stack (A → B → A).
    Reentrancy = 16,
    /// A call would put more than `MAX_CALL_DEPTH` programs on the stack.
    CallDepth = 17,
    /// A version digest (the call's, a binding's, or `call`'s `expect`) isn't the callee's version
    /// in force.
    VersionMismatch = 18,
    /// The callee isn't a program.
    NotAProgram = 19,
    /// The program has no method of that name.
    NoMethod = 20,
    /// The method can't be called this way: an `Init` method from anything but its program's
    /// creation, or a top-level call to a method of another kind than the action runs.
    NotCallable = 21,
    /// Value sent to a method that isn't payable.
    NotPayable = 22,
    /// A `View` frame tried to write, remove, transfer, emit, send value or call a non-`View`
    /// method.
    ReadOnly = 23,
    // FANG and storage (6.3).
    /// A payer is short: a call's value, or a storage deposit.
    Insufficient = 24,
    /// The signer's deposits would pass the transaction's `storage_limit`.
    StorageLimit = 25,
    /// Program state would pass its ceiling (`View::storage_price` has no price for it).
    StateFull = 26,
    // The per-transaction bounds (4.4).
    /// More than `MAX_KEYS_WRITTEN` distinct keys written.
    KeysWritten = 27,
    /// More than `MAX_WRITTEN_BYTES` of value bytes written.
    WrittenBytes = 28,
    /// More than `MAX_EVENTS` events.
    Events = 29,
    /// More than `MAX_EVENT_BYTES` of event names and data.
    EventBytes = 30,
}

impl TrapKind {
    /// Every kind, in number order.
    pub const ALL: &[TrapKind] = &[
        TrapKind::OutOfFuel,
        TrapKind::Unreachable,
        TrapKind::MemoryOutOfBounds,
        TrapKind::TableOutOfBounds,
        TrapKind::IndirectCallToNull,
        TrapKind::DivisionByZero,
        TrapKind::IntegerOverflow,
        TrapKind::StackOverflow,
        TrapKind::BadSignature,
        TrapKind::MemoryAccess,
        TrapKind::Length,
        TrapKind::Amount,
        TrapKind::Flags,
        TrapKind::Name,
        TrapKind::UnboundProgram,
        TrapKind::Reentrancy,
        TrapKind::CallDepth,
        TrapKind::VersionMismatch,
        TrapKind::NotAProgram,
        TrapKind::NoMethod,
        TrapKind::NotCallable,
        TrapKind::NotPayable,
        TrapKind::ReadOnly,
        TrapKind::Insufficient,
        TrapKind::StorageLimit,
        TrapKind::StateFull,
        TrapKind::KeysWritten,
        TrapKind::WrittenBytes,
        TrapKind::Events,
        TrapKind::EventBytes,
    ];

    /// The kind's number in receipts.
    pub fn number(self) -> u8 {
        self as u8
    }

    /// The kind numbered `n`, if there is one.
    pub fn from_number(n: u8) -> Option<TrapKind> {
        TrapKind::ALL.iter().copied().find(|k| k.number() == n)
    }
}

/// Why `exec::execute` returned no receipt: something no valid transaction can cause went wrong
/// in the node, so the block can't be executed here. **Never a transaction failure:** committing
/// one as `Failed` would turn this node's trouble into a deterministic-looking outcome that another
/// node, without the trouble, wouldn't reach. The node must not commit the block (it halts), and
/// must drop the engine. `upload::check` returns it too. The text is for logs only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeFault {
    /// A panic under `execute` or `upload::check`, caught by its backstop.
    Panic,
    /// A reachable program's code isn't loaded in the engine: the node didn't load every declared
    /// code before executing.
    NotLoaded,
    /// Stored code no longer loads (for the node's own use when it loads a transaction's code).
    Load(LoadError),
    /// Instantiating loaded code failed (linking, a missing `memory`, a start function, the host
    /// out of memory), in the engine's words.
    Instantiate(String),
    /// The engine stopped with an error no validated program can cause, in its words.
    Engine(String),
    /// The engine's fuel counter refused an operation (metering is always on).
    Fuel,
    /// The host broke one of its own invariants.
    Host(&'static str),
}

impl fmt::Display for NodeFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeFault::Panic => f.write_str("a panic in program execution"),
            NodeFault::NotLoaded => f.write_str("a reachable program's code isn't loaded in the engine"),
            NodeFault::Load(e) => write!(f, "stored code doesn't load: {e}"),
            NodeFault::Instantiate(m) => write!(f, "instantiation failed: {m}"),
            NodeFault::Engine(m) => write!(f, "engine error: {m}"),
            NodeFault::Fuel => f.write_str("the engine's fuel counter failed"),
            NodeFault::Host(m) => write!(f, "host invariant broken: {m}"),
        }
    }
}

impl std::error::Error for NodeFault {}

/// Why a frame stopped, as host functions and backends report it (the backends carry it through
/// the engine as its host-error type and recover it on the way out).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trap {
    /// The program's doing, or its caller's: the transaction fails with this kind.
    Kind(TrapKind),
    /// The program called `abort_v1`: its code, and its message sanitized to at most
    /// `MAX_ABORT_MESSAGE` bytes.
    Abort { code: u32, message: String },
    /// Not the transaction's doing: `execute` returns it instead of a receipt.
    Fault(NodeFault),
}

impl Trap {
    /// Whether this is the node's trouble rather than the program's: never absorbed by
    /// `try_call`, never a receipt.
    pub fn is_node_fault(&self) -> bool {
        matches!(self, Trap::Fault(_))
    }
}

impl From<TrapKind> for Trap {
    fn from(kind: TrapKind) -> Trap {
        Trap::Kind(kind)
    }
}

impl From<NodeFault> for Trap {
    fn from(fault: NodeFault) -> Trap {
        Trap::Fault(fault)
    }
}

/// Only for the engine's host-error bound (`Display`); a receipt never holds this text.
impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// What a host function sees of the instance that called it: its memory and the transaction's
/// meter, as that instance's store holds it.
pub trait Instance {
    /// The whole linear memory (fixed size: it never grows).
    fn memory(&mut self) -> &mut [u8];
    /// Fuel left.
    fn fuel(&self) -> Result<u64, NodeFault>;
    /// Sets the fuel left: the executor's way to give a callee its share and take back the rest.
    fn set_fuel(&mut self, fuel: u64) -> Result<(), NodeFault>;
}

impl dyn Instance + '_ {
    /// Charges `cost` before the work it pays for, or traps `OutOfFuel` leaving the fuel as it
    /// was, exactly as the engine does for Wasm blocks.
    pub fn charge(&mut self, cost: u64) -> Result<(), Trap> {
        let left = self.fuel()?.checked_sub(cost).ok_or(TrapKind::OutOfFuel)?;
        self.set_fuel(left)?;
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
    /// Fuel left in the frame's store. After `OutOfFuel` the engine leaves what was there before
    /// the charge that failed, so the executor counts the frame's whole share as used instead.
    pub fuel_left: u64,
}

/// An engine: one per program transaction, made on the thread that runs the transaction (it isn't
/// `Send`), loaded with every code the transaction declares before anything that may reach it
/// runs, and dropped with that thread, so neither its memory nor its code map depends on a node's
/// history. Dropped after any node fault. (The name is PROGRAMS.md 4.3's engine per block, which
/// this API also serves.)
pub trait BlockEngine {
    fn kind(&self) -> EngineKind;
    /// Translates `wasm` (once per hash: loading a hash again is a no-op). Never called while a
    /// call is running on this engine.
    fn load(&mut self, hash: CodeHash, wasm: &[u8]) -> Result<(), LoadError>;
    /// What instantiating a loaded code costs to know; `None` if it isn't loaded.
    fn info(&self, hash: &CodeHash) -> Option<CodeInfo>;
    /// Instantiates `hash` in a fresh store holding `fuel`, runs its export `export` (`[] → []`)
    /// with `host` behind every import, and drops the store. May be re-entered from inside
    /// `host.call` (a nested `call_v1`). Code that isn't loaded is `NodeFault::NotLoaded`; an
    /// export that doesn't exist is `TrapKind::NoMethod`.
    fn invoke(&self, hash: &CodeHash, export: &str, fuel: u64, host: &mut dyn Host) -> Invoked;
}

/// A fresh engine of `kind`, configured from `schedule` and the 4.2 settings, with nothing loaded.
/// Fails only if the engine refuses its own configuration or the host table: a node fault, never
/// a program's.
pub fn new_block_engine(kind: EngineKind, schedule: &Schedule) -> Result<Box<dyn BlockEngine>, NodeFault> {
    Ok(match kind {
        EngineKind::Soroban => Box::new(soroban::SorobanEngine::new(schedule)?),
        #[cfg(feature = "ref038")]
        EngineKind::Wasmi038 => Box::new(wasmi038::Wasmi038Engine::new(schedule)?),
    })
}

/// The host, as a store's data holds it while one `invoke` runs.
///
/// Engines want their store data `'static` (the linker, built once per engine, names its type),
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
