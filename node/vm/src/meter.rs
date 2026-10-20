//! The fuel schedule (docs/PROGRAMS.md 4.5), engine-neutral: `engine/*.rs` turns it into the
//! engine's own cost model. A code constant: a protocol version changes it, never a parameter
//! (14.2), and an old schedule stays for replay ([`schedule`]).
//!
//! # Schedule v0
//!
//! [`SCHEDULE_V0`] is the first schedule a chain runs programs under. It's deliberately
//! overpriced: phase 0's calibration (CALIBRATION.md and its per-architecture records, measured
//! under the prototype's starting values, "v1" there) found those unsafe on the floor runners, and
//! v0 prices every item but one 1.5–3× above the worst floor figure on both architectures (x86-64
//! and aarch64), at the 2.5 ns-per-fuel budget (4.5).
//!
//! **The exception is `UploadCode`**, the thinnest margin in the schedule: its 71 per byte is the
//! most that keeps a maximum upload within [`MAX_TX_FUEL`] (const-asserted below), only ~1.22×
//! the worst floor figure (58 fuel per byte). Upload work (validation, the test translation,
//! hashing) burns no engine fuel, so its ns per fuel is wall time over `upload_fuel`; the
//! calibration bench reports it as a go/no-go line of its own. If a floor run puts it over 2.5,
//! the price can't rise within these bounds: that takes a smaller `MAX_CODE_BYTES` /
//! `MAX_INTERFACE_BYTES` or a new schedule, either a protocol version (until then only a lower
//! block fuel budget, the node's parameter, keeps a block of uploads within its time).
//!
//! The row marked † isn't in phase 0's records: a local run (an Apple M-series laptop, where a
//! function instantiates in 5.3 ns against the x86 floor's ~12) measured 0.76 ns per element, so
//! ~1.7 ns on the floor, and the next calibration run measures it there. A table's own entries
//! cost ~0.1 ns each to allocate (at most 4,096: within the base). docs/programs/phase0.md records
//! the whole derivation.
//!
//! | Item | v0 | Basis |
//! |---|---:|---|
//! | Wasm operator, `base` class | 3 | Worst x86 compute: 4.85 ns per fuel at 1 |
//! | `entity` (globals, `memory.size`, tables) | 4 | Global copies: 3.65 ns per fuel at 1 |
//! | `load` | 20 | Cache-missing loads: 28.5 ns each on aarch64 (needs ≥ 11.4) |
//! | `store` | 4 | Random stores: 4.06 ns per fuel at 1 |
//! | `call` | 30 | Keeps a call ≥ 3× `base`; locals stay at the engine's fixed ratio, measured under budget |
//! | Load, per distinct declared code | 10,000 + 40/B | Warm minimal load 1.1k; worst translation 22.8 fuel/B |
//! | `UploadCode` (code and interface bytes) | 50,000 + 71/B | Worst upload (validate, translate, hash) 58 fuel/B; 71 is the most that keeps a maximum upload within [`MAX_TX_FUEL`] |
//! | Instantiate | 2,000 | 544 measured for an empty module |
//! | + per imported function | 300 | ~222 ns (x86), ~271 ns (aarch64) |
//! | + per export | 500 | ~451 ns; at most 64 methods and `memory` (`config::MAX_METHODS`) |
//! | + per defined function | 15 | ~12 ns |
//! | + per global | 20 | ~16.5 ns |
//! | + per element segment | 50 | ~41 ns |
//! | + per table element a segment initializes † | 2 | ~1.7 ns projected (above); at most one per code byte |
//! | + per data segment | 20 | ~17 ns |
//! | + per page | 2,000 | ~760 ns, touching every page included |
//! | + per data-segment byte | 1 | 0.04–0.05 ns per fuel |
//! | Host functions | as 4.5's table | All measured ≤ 1.2 ns per fuel, but `abort_v1` (2.5–2.7: +1,000 here); `debug_log_v1` is priced like `emit_v1` (10.2) |
//!
//! Storage costs cover the overlay and the host, not the node's state tree and database at
//! commit: that's the node's to measure (PROGRAMS-PLAN.md C6).
//!
//! # Reservation bounds
//!
//! A transaction reserves `exec + load` (5.1): its `fuel_limit` (at most [`MAX_TX_FUEL`]) plus,
//! for each distinct declared code, `load_base + load_per_byte × size`. At most
//! `config::MAX_CALLS + config::MAX_BINDINGS` = 24 programs are declared, with at most
//! `config::MAX_TX_CODE_BYTES` of distinct code between them, so under v0
//!
//! ```text
//! load ≤ 24 × 10,000 + 40 × 524,288 = 21,211,520
//! exec + load ≤ 20,000,000 + 21,211,520 = 41,211,520 ≤ MAX_RESERVATION = 2^26 = 67,108,864
//! ```
//!
//! PROGRAMS.md rev 3 had 1 MiB of declared code and `MAX_RESERVATION` = 2^25 under a load price
//! of 10 per byte. At 40 per byte, 1 MiB alone is 41.9M of load, past 2^25 and, with a full
//! `fuel_limit`, past the 60M block. So v0 halves `MAX_TX_CODE_BYTES` to 512 KiB (two maximum-size
//! programs, or one and many small ones) and doubles `MAX_RESERVATION` to 2^26, which still leaves
//! the node's fuel rate clamp (`u64::MAX / MAX_RESERVATION`) far above any rate a chain reaches.
//! Every bound is a `const` assertion below, so a schedule that breaks one doesn't compile.

use crate::config::{MAX_BINDINGS, MAX_CALLS, MAX_CODE_BYTES, MAX_INTERFACE_BYTES, MAX_TX_CODE_BYTES};
use crate::engine::CodeInfo;

/// What host functions cost, in fuel, charged before they do any work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostCosts {
    /// Every host call.
    pub base: u64,
    /// Per byte copied across the boundary (input, return data, abort messages).
    pub per_byte: u64,
    pub storage_read: u64,
    pub storage_read_per_byte: u64,
    pub storage_write: u64,
    pub storage_write_per_byte: u64,
    pub storage_remove: u64,
    /// `emit_v1`, and `debug_log_v1` (priced alike, 10.2).
    pub emit: u64,
    pub emit_per_byte: u64,
    pub transfer: u64,
    /// `abort_v1`, on top of `base` and `per_byte`: unwinding the frame and its layer.
    pub abort: u64,
    /// A nested `call`/`try_call`, plus the callee's instantiation.
    pub call: u64,
    pub call_per_input_byte: u64,
    /// Context reads (caller, value, height, balance…).
    pub context: u64,
}

/// What the engine charges per executed Wasm operator, by class: soroban-wasmi's five
/// (`engine/soroban.rs` maps them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperatorCosts {
    /// Arithmetic, comparison, conversion, constants, locals, `select`, branches.
    pub base: u8,
    /// Globals, `memory.size`, tables (upload validation refuses `memory.grow`).
    pub entity: u8,
    pub load: u8,
    pub store: u8,
    pub call: u8,
}

/// Load, instantiation and upload, priced outside the engine: load at admission per distinct
/// declared code (4.5), instantiation per frame before its store exists, upload by size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadCosts {
    pub load_base: u64,
    pub load_per_byte: u64,
    pub instantiate_base: u64,
    pub instantiate_per_import: u64,
    pub instantiate_per_export: u64,
    pub instantiate_per_function: u64,
    pub instantiate_per_global: u64,
    pub instantiate_per_element_segment: u64,
    pub instantiate_per_element: u64,
    pub instantiate_per_data_segment: u64,
    pub instantiate_per_page: u64,
    pub instantiate_per_data_byte: u64,
    /// `UploadCode`: validation, a test translation and hashing, fixed by the size of the code and
    /// the interface.
    pub upload_base: u64,
    pub upload_per_byte: u64,
}

/// A fuel schedule: what everything a program does costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schedule {
    /// The schedule's number: 0 for [`SCHEDULE_V0`]. A later one takes the next.
    pub version: u32,
    pub ops: OperatorCosts,
    pub host: HostCosts,
    pub load: LoadCosts,
}

/// Schedule v0: see the module documentation for every number's basis.
pub const SCHEDULE_V0: Schedule = Schedule {
    version: 0,
    ops: OperatorCosts { base: 3, entity: 4, load: 20, store: 4, call: 30 },
    host: HostCosts {
        base: 100,
        per_byte: 1,
        storage_read: 1_000,
        storage_read_per_byte: 1,
        storage_write: 5_000,
        storage_write_per_byte: 10,
        storage_remove: 3_000,
        emit: 2_000,
        emit_per_byte: 8,
        transfer: 10_000,
        abort: 1_000,
        call: 20_000,
        call_per_input_byte: 1,
        context: 200,
    },
    load: LoadCosts {
        load_base: 10_000,
        load_per_byte: 40,
        instantiate_base: 2_000,
        instantiate_per_import: 300,
        instantiate_per_export: 500,
        instantiate_per_function: 15,
        instantiate_per_global: 20,
        instantiate_per_element_segment: 50,
        instantiate_per_element: 2,
        instantiate_per_data_segment: 20,
        instantiate_per_page: 2_000,
        instantiate_per_data_byte: 1,
        upload_base: 50_000,
        upload_per_byte: 71,
    },
};

/// The schedule blocks at `protocol_version` run programs under: none before v2 (there are no
/// programs), [`SCHEDULE_V0`] from v2 on. A protocol version that reprices adds an arm from the
/// version it takes effect at, and the old arms stay, so a node replays history under the schedule
/// each block ran under.
pub fn schedule(protocol_version: u32) -> Option<&'static Schedule> {
    match protocol_version {
        0 | 1 => None,
        _ => Some(&SCHEDULE_V0),
    }
}

/// Most fuel one transaction may reserve for execution: the upper bound of the
/// `program_max_tx_fuel` parameter, a third of [`CALIBRATED_BLOCK_FUEL`] (14.2).
pub const MAX_TX_FUEL: u64 = CALIBRATED_BLOCK_FUEL / 3;
/// Most fuel a block may count (the upper bound of `program_max_block_fuel`): the worst case
/// fits 150 ms at 2.5 ns per fuel on the floor machine (4.5).
pub const CALIBRATED_BLOCK_FUEL: u64 = 60_000_000;
/// Most fuel any transaction can reserve (`exec + load`, 5.1): the node's fuel rate is clamped to
/// `u64::MAX / MAX_RESERVATION` so `rate × reserved` never overflows (5.2).
pub const MAX_RESERVATION: u64 = 1 << 26;

impl Schedule {
    /// Load fuel for one distinct code of `size` bytes (charged at admission, 4.5).
    pub const fn load_fuel(&self, size: usize) -> u64 {
        self.load.load_base.saturating_add(self.load.load_per_byte.saturating_mul(size as u64))
    }

    /// The most load fuel one transaction can be charged: every declared program with its own
    /// code, `MAX_TX_CODE_BYTES` of it in all.
    pub const fn max_load_fuel(&self) -> u64 {
        let codes = (MAX_CALLS + MAX_BINDINGS) as u64;
        self.load.load_base.saturating_mul(codes).saturating_add(self.load.load_per_byte.saturating_mul(MAX_TX_CODE_BYTES as u64))
    }

    /// `UploadCode`'s fuel (its whole `exec`, 5.1) for `code` bytes of Wasm and `interface` bytes
    /// of interface.
    pub const fn upload_fuel(&self, code: usize, interface: usize) -> u64 {
        let bytes = (code as u64).saturating_add(interface as u64);
        self.load.upload_base.saturating_add(self.load.upload_per_byte.saturating_mul(bytes))
    }

    /// Instantiation fuel for one frame of `code` (4.5): what the engine does before the
    /// method's first instruction, by what the module holds.
    pub fn instantiate_fuel(&self, code: &CodeInfo) -> u64 {
        let l = &self.load;
        let per = |cost: u64, count: u64| cost.saturating_mul(count);
        [
            per(l.instantiate_per_import, code.imports.into()),
            per(l.instantiate_per_export, code.exports.into()),
            per(l.instantiate_per_function, code.functions.into()),
            per(l.instantiate_per_global, code.globals.into()),
            per(l.instantiate_per_element_segment, code.element_segments.into()),
            per(l.instantiate_per_element, code.elements.into()),
            per(l.instantiate_per_data_segment, code.data_segments.into()),
            per(l.instantiate_per_page, code.pages.into()),
            per(l.instantiate_per_data_byte, code.data_bytes as u64),
        ]
        .into_iter()
        .fold(l.instantiate_base, u64::saturating_add)
    }
}

// The reservation bounds (module documentation): a schedule that breaks one doesn't compile.
const _: () = assert!(MAX_TX_FUEL.saturating_add(SCHEDULE_V0.max_load_fuel()) <= MAX_RESERVATION);
const _: () = assert!(SCHEDULE_V0.upload_fuel(MAX_CODE_BYTES, MAX_INTERFACE_BYTES) <= MAX_TX_FUEL);
const _: () = assert!(MAX_TX_FUEL <= CALIBRATED_BLOCK_FUEL);
// soroban-wasmi sums a basic block's operator costs in a `u32` (`engine/soroban.rs`): a class cost
// of at most 255 (a `u8`) over a function's operators stays far below it.
const _: () = assert!(SCHEDULE_V0.ops.base > 0 && SCHEDULE_V0.ops.call >= 3 * SCHEDULE_V0.ops.base);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_by_protocol_version() {
        assert_eq!(schedule(0), None);
        assert_eq!(schedule(1), None, "v1 has no programs");
        assert_eq!(schedule(2), Some(&SCHEDULE_V0));
        assert_eq!(SCHEDULE_V0.version, 0);
    }

    /// The bounds the module documentation derives, as numbers.
    #[test]
    fn reservation_bounds() {
        assert_eq!(SCHEDULE_V0.max_load_fuel(), 21_211_520);
        assert_eq!(MAX_TX_FUEL + SCHEDULE_V0.max_load_fuel(), 41_211_520);
        assert_eq!(MAX_RESERVATION, 67_108_864);
        assert_eq!(SCHEDULE_V0.upload_fuel(MAX_CODE_BYTES, MAX_INTERFACE_BYTES), 19_825_488);
        // 72 per byte would pass the transaction's budget.
        let dearer = Schedule { load: LoadCosts { upload_per_byte: 72, ..SCHEDULE_V0.load }, ..SCHEDULE_V0 };
        assert!(dearer.upload_fuel(MAX_CODE_BYTES, MAX_INTERFACE_BYTES) > MAX_TX_FUEL);
        assert_eq!(SCHEDULE_V0.load_fuel(0), 10_000);
        assert_eq!(SCHEDULE_V0.load_fuel(65_536), 10_000 + 40 * 65_536);
    }

    /// Every entity is priced, and only once.
    #[test]
    fn instantiation_prices_every_entity() {
        let empty = CodeInfo::default();
        assert_eq!(SCHEDULE_V0.instantiate_fuel(&empty), 2_000);
        let one = |f: fn(&mut CodeInfo)| {
            let mut code = CodeInfo::default();
            f(&mut code);
            SCHEDULE_V0.instantiate_fuel(&code) - 2_000
        };
        assert_eq!(one(|c| c.imports = 1), 300);
        assert_eq!(one(|c| c.exports = 1), 500);
        assert_eq!(one(|c| c.functions = 1), 15);
        assert_eq!(one(|c| c.globals = 1), 20);
        assert_eq!(one(|c| c.element_segments = 1), 50);
        assert_eq!(one(|c| c.elements = 1), 2);
        assert_eq!(one(|c| c.data_segments = 1), 20);
        assert_eq!(one(|c| c.pages = 1), 2_000);
        assert_eq!(one(|c| c.data_bytes = 1), 1);
        assert_eq!(one(|c| c.size = 1_000), 0, "the code's size is load's, not instantiation's");
        let all = CodeInfo {
            size: 9,
            pages: 32,
            data_bytes: 100,
            imports: 20,
            exports: 65,
            functions: 10_000,
            globals: 256,
            element_segments: 1_000,
            elements: 4_096,
            data_segments: 1_000,
        };
        let expect = 2_000 + 300 * 20 + 500 * 65 + 15 * 10_000 + 20 * 256 + 50 * 1_000 + 2 * 4_096 + 20 * 1_000 + 2_000 * 32 + 100;
        assert_eq!(SCHEDULE_V0.instantiate_fuel(&all), expect);
        let max = CodeInfo { imports: u32::MAX, data_bytes: usize::MAX, ..all };
        assert_eq!(SCHEDULE_V0.instantiate_fuel(&max), u64::MAX, "saturates, never wraps");
    }
}
