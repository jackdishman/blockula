//! Fuzzing harness for the node's network-facing decoders (SEC-3).
//!
//! The node is a binary crate, so instead of depending on it this library compiles its modules
//! straight from `node/src` (the `#[path]` module below, re-exported at the root so the node's
//! `crate::…` paths resolve). `check-sync.sh` fails when the module list or the dependencies
//! drift from `node/src/main.rs` and `node/Cargo.toml`.
//!
//! [`harness`] has one entry point per fuzz target (`fuzz_targets/`); `tests/seeds.rs` runs them
//! over the committed seeds on stable Rust.
#![allow(dead_code, unused_imports, unused_macros, unused_variables, clippy::all)]
// As in node/src/main.rs: `/api/status` and friends are large `json!` literals.
#![recursion_limit = "256"]

#[path = "../../src"]
mod node_src {
    pub mod api;
    pub mod beacon;
    pub mod chain;
    pub mod checkpoints;
    pub mod cli_net;
    pub mod consensus;
    pub mod contrib_cli;
    pub mod crypto;
    pub mod econ;
    pub mod epochs;
    pub mod genesis_builder;
    pub mod ghost;
    pub mod github;
    pub mod gov_cli;
    pub mod governance;
    pub mod node;
    pub mod params;
    pub mod poc;
    pub mod pq;
    pub mod protection;
    pub mod release_cli;
    pub mod snapshot;
    pub mod staking;
    pub mod state;
    pub mod status_cli;
    pub mod store;
    pub mod types;
    pub mod upgrade;
    pub mod validator_cli;
    pub mod view;
}
pub use node_src::*;

pub mod harness;
