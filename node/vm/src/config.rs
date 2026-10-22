//! Limits the engine configuration and upload validation enforce (docs/PROGRAMS.md 4.2, 4.4, 6.2,
//! 8.2). Code constants: a protocol version changes them, never a parameter.

/// Linear memory: exactly one, defined and exported as `memory`, with initial = maximum ≤ this
/// many 64 KiB pages (2 MiB). It never grows.
pub const MAX_PAGES: u32 = 32;
/// At most one table, of at most this many entries.
pub const MAX_TABLE_ENTRIES: u32 = 4_096;
/// Functions defined in a module.
pub const MAX_FUNCS: u32 = 10_000;
/// Locals (params included) in any one function.
pub const MAX_LOCALS: u32 = 1_024;
/// Globals defined in a module.
pub const MAX_GLOBALS: u32 = 256;
/// Smallest average function body, in bytes, once the code section passes
/// [`MIN_AVG_BODY_FROM`] bytes: wasmi's guard against translation bombs (many tiny functions),
/// enforced here so it holds whichever engine runs.
pub const MIN_AVG_BODY_BYTES: u32 = 40;
pub const MIN_AVG_BODY_FROM: u32 = 1_000;
/// Code, as uploaded.
pub const MAX_CODE_BYTES: usize = 256 * 1024;
/// Methods (`bk_` exports) of one code: the interface's method limit (11.1), which matches them
/// one to one.
pub const MAX_METHODS: usize = 64;
/// An interface, as uploaded (6.2). The interface codec enforces it; it's here because upload's
/// fuel is priced over the code and the interface together (`meter::Schedule::upload_fuel`).
pub const MAX_INTERFACE_BYTES: usize = 16 * 1024;

/// Programs on the call stack at once (nested `call`s, the top-level call included).
pub const MAX_CALL_DEPTH: usize = 8;
/// Wasm recursion depth and value stack, per call (each nested `call` gets its own).
pub const MAX_RECURSION_DEPTH: u32 = 1_024;
pub const MIN_STACK_BYTES: usize = 64 << 10;
pub const MAX_STACK_BYTES: usize = 1 << 20;

/// Storage keys and values (6.2).
pub const MAX_KEY_BYTES: usize = 128;
pub const MAX_VALUE_BYTES: usize = 16 * 1024;
/// Per transaction (4.4): distinct storage keys written, written value bytes, events, event bytes
/// and return data.
pub const MAX_KEYS_WRITTEN: usize = 256;
pub const MAX_WRITTEN_BYTES: usize = 1 << 20;
pub const MAX_EVENTS: usize = 64;
pub const MAX_EVENT_BYTES: usize = 16 * 1024;
pub const MAX_RETURN: usize = 4 * 1024;
/// Input to one call (Σ over a batch is bounded by the tx codec).
pub const MAX_INPUT: usize = 32 * 1024;
/// Calls in one transaction's batch, and bindings beside them (8.1). The tx codec enforces both;
/// together they bound the declared program set, and so the distinct codes a transaction loads.
pub const MAX_CALLS: usize = 8;
pub const MAX_BINDINGS: usize = 16;
/// Distinct declared code per transaction, in bytes (4.5): what its load fuel is charged on.
/// 512 KiB, half of PROGRAMS.md rev 3's 1 MiB, so a transaction's reservation stays within
/// `meter::MAX_RESERVATION` at schedule v0's load price (`meter.rs`, "Reservation bounds").
pub const MAX_TX_CODE_BYTES: usize = 512 * 1024;
/// An abort message, after the host replaces control, format, bidi and private-use characters.
pub const MAX_ABORT_MESSAGE: usize = 256;
