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
//! * [`upload`]: `UploadCode`'s checks on new code: validation, then a test translation (7.1).
//! * [`interface`]: interface format 1, the methods a program declares, and their match with its
//!   code's exports (docs/PROGRAMS-V0.md §3).
//! * [`engine`]: the engine-neutral boundary and its backends (an engine per transaction outside a
//!   block pass, one per pass in the node: README.md, "How the node drives it", and
//!   CALIBRATION-node.md, "A3: one thread and one engine per block pass").
//! * [`host`], [`overlay`], [`exec`]: execution against a read-only view and a sorted diff (4.4),
//!   frames, method kinds, the declared program set, no reentrancy, depth 8 (8.1, 8.2).
//! * [`storage`]: an entry's accounted capacity and the price curve (6.2, 6.3).
//! * [`testing`] (feature `testing`): `MemHost`, an in-memory chain to run programs on.
//!
//! See README.md for how the node drives it.

pub mod abi;
pub mod config;
pub mod meter;
pub mod engine;
pub mod exec;
pub mod host;
pub mod interface;
pub mod overlay;
pub mod storage;
#[cfg(feature = "testing")]
pub mod testing;
pub mod upload;
pub mod validate;
