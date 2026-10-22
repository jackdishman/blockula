# Fuzzing the programs engine

Two cargo-fuzz targets over `blockula-vm` (docs/PROGRAMS-PLAN.md C1, C8):

- **`program_validate`**: `validate()` never panics, and everything it accepts
  uploads: `MemHost::upload` (validation, then a test translation on
  soroban-wasmi under `catch_unwind`) neither faults nor refuses it. Any
  disagreement between validation and the engine is a finding.
- **`program_exec`**: the executor, host and overlay never panic, corrupt
  memory or report a node fault (`MemHost` is a sound view, so a `NodeFault`
  is the executor's bug), however a batch of calls to the four guests is
  shaped: callees, methods, inputs, values, the method kinds and payability
  their interfaces declare, the kind each call runs as and stale version
  digests are all fuzzer-chosen. Only validated code runs, as on a node.

## Run it

Needs a nightly toolchain and cargo-fuzz (`cargo install cargo-fuzz --locked
--version 0.13.2`). From `node/vm`:

```
cargo +nightly fuzz build
cargo +nightly fuzz run program_validate fuzz/corpus/program_validate fuzz/seeds/program_validate -- -max_total_time=300 -timeout=20 -rss_limit_mb=4096
cargo +nightly fuzz run program_exec     fuzz/corpus/program_exec     fuzz/seeds/program_exec     -- -max_total_time=300 -timeout=20 -rss_limit_mb=4096
```

The harness logic is in `src/lib.rs`; `cargo test --locked --manifest-path
fuzz/Cargo.toml` runs the seed checks on the pinned stable toolchain (no fuzzer
needed).

The sustained run is two weeks under AddressSanitizer on the floor machines
(plan S4). `ref038` is left off: the targets fuzz the engine a node runs.

## A crash is a vulnerability

Same rule as `node/fuzz` and SECURITY.md: a crash the fuzzer finds is an
undisclosed vulnerability. Keep its input private (it stays under the
git-ignored `artifacts/`), never commit it, never put it in a public issue or
PR, and handle it as a private security advisory.
