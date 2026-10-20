//! The fuel schedule (docs/PROGRAMS.md 4.5), engine-neutral: `engine/*.rs` turns it into each
//! engine's own cost model. Versioned by protocol version; every number here is a starting value
//! that phase 0's calibration replaces (S3), never a measurement.

/// What host functions cost, in fuel, charged before they do any work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostCosts {
    /// Every host call.
    pub base: u64,
    /// Per byte copied across the boundary (input, return data, debug logs).
    pub per_byte: u64,
    pub storage_read: u64,
    pub storage_read_per_byte: u64,
    pub storage_write: u64,
    pub storage_write_per_byte: u64,
    pub storage_remove: u64,
    pub emit: u64,
    pub emit_per_byte: u64,
    pub transfer: u64,
    /// A nested `call`/`try_call`, plus the callee's instantiation.
    pub call: u64,
    pub call_per_input_byte: u64,
    /// Context reads (caller, value, height, balance…).
    pub context: u64,
}

/// What the engine charges per executed Wasm operator, by class. Engines that can't price every
/// class separately (soroban-wasmi: five classes, wasmi 2.0: one cost per operator) map what
/// they can; `engine/*.rs` documents the mapping, and S3 measures the gap.
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

/// Load and instantiation, priced outside the engine: load at admission per distinct declared
/// code (4.5), instantiation per program per transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadCosts {
    pub load_base: u64,
    pub load_per_byte: u64,
    pub instantiate_base: u64,
    pub instantiate_per_page: u64,
    pub instantiate_per_data_byte: u64,
    /// `UploadCode`: validation, a test translation and hashing, fixed by size.
    pub upload_base: u64,
    pub upload_per_byte: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schedule {
    pub version: u32,
    pub ops: OperatorCosts,
    pub host: HostCosts,
    pub load: LoadCosts,
}

/// Schedule v1's starting values (4.5's table).
pub const V1: Schedule = Schedule {
    version: 1,
    ops: OperatorCosts { base: 1, entity: 1, load: 1, store: 1, call: 10 },
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
        call: 20_000,
        call_per_input_byte: 1,
        context: 200,
    },
    load: LoadCosts {
        load_base: 2_000,
        load_per_byte: 10,
        instantiate_base: 1_000,
        instantiate_per_page: 2_000,
        instantiate_per_data_byte: 1,
        upload_base: 50_000,
        upload_per_byte: 20,
    },
};

impl Schedule {
    /// Load fuel for one distinct code of `size` bytes (charged at admission).
    pub fn load_fuel(&self, size: usize) -> u64 {
        self.load.load_base.saturating_add(self.load.load_per_byte.saturating_mul(size as u64))
    }

    /// Instantiation fuel for a module with `pages` of memory and `data_bytes` of data segments.
    pub fn instantiate_fuel(&self, pages: u32, data_bytes: usize) -> u64 {
        self.load
            .instantiate_base
            .saturating_add(self.load.instantiate_per_page.saturating_mul(pages as u64))
            .saturating_add(self.load.instantiate_per_data_byte.saturating_mul(data_bytes as u64))
    }
}
