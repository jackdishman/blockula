//! # blockula-vm: the programs engine
//!
//! docs/PROGRAMS.md sections 4 (the VM), 5 (metering), 6 (storage deposits), 8 (calls) and 10 (the
//! host API): what runs a program, behind one engine-neutral boundary. The engine is
//! soroban-wasmi 29.0.0 (D1); wasmi 0.38.0 (`--features ref038`) is a test-only differential
//! oracle. Programs, never contracts.
//!
//! * [`abi`]: host ABI v1, engine-neutral.
//! * [`config`], [`meter`]: limits and the fuel schedule.
//! * [`validate`]: upload validation (4.2), before any engine sees the code.
//! * [`engine`]: the engine-neutral boundary and its backends (an engine per block, 4.3).
//! * [`host`], [`overlay`], [`exec`]: execution against a read-only view and a sorted diff (4.4),
//!   frames, the declared program set, no reentrancy, depth 8 (8.1, 8.2).
//! * [`testing`] (feature `testing`): `MemHost`, an in-memory chain to run programs on.
//!
//! See README.md for how the node drives it.

pub mod abi;
pub mod config;
pub mod meter;
pub mod engine;
pub mod exec;
pub mod host;
pub mod overlay;
#[cfg(feature = "testing")]
pub mod testing;
pub mod validate;
