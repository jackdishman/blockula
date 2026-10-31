# Fuzzing the programs engine

Three cargo-fuzz targets over `blockula-vm` (docs/PROGRAMS-PLAN.md C1, C8;
docs/PROGRAMS-V0.md §10.4):

- **`program_validate`**: `validate()` never panics, and everything it accepts
  uploads: `MemHost::upload` (validation, then a test translation on
  soroban-wasmi under `catch_unwind`) neither faults nor refuses it. Any
  disagreement between validation and the engine is a finding. An input that
  starts with the Wasm magic (`\0asm`) is the module, as it is; any other input
  drives `wasm-smith`, restricted to the subset validation accepts and shaped
  for it (each `memory.grow` replaced, a fixed memory, the exports `memory` and
  `bk_*`), since mutated bytes rarely get past validation. No imports on that
  path: wasm-smith takes an import list only through its `wasmparser` feature,
  which would turn on wasmparser's `simd` in this build, unlike the node's. The
  seeds are the guests (raw) and `smith-*` (generated modules that validate).
- **`program_exec`**: the executor, host and overlay never panic, corrupt
  memory or report a node fault (`MemHost` is a sound view, so a `NodeFault`
  is the executor's bug), however a batch of calls to the four guests is
  shaped: callees, methods, inputs, values, the method kinds and payability
  their interfaces declare, the kind each call runs as and stale version
  digests are all fuzzer-chosen. Only validated code runs, as on a node.
- **`interface_codec`**: `interface::decode` never panics; what it accepts is
  the one encoding of an interface that keeps every rule of
  docs/PROGRAMS-V0.md §3.2 (it encodes back to the same bytes, its methods
  ascend with distinct selectors, …); and an interface built field by field
  from the same bytes, rules broken as often as kept, decodes back to itself
  whenever `encode` accepts it. It runs on every gossiped `UploadCode` even
  while programs are off. Seeds: the guests' interfaces and §3.4's example.

## Run it

Needs a nightly toolchain and cargo-fuzz (`cargo install cargo-fuzz --locked
--version 0.13.2`). From `node/vm`:

```
cargo +nightly fuzz build
cargo +nightly fuzz run program_validate fuzz/corpus/program_validate fuzz/seeds/program_validate -- -max_total_time=300 -timeout=10 -rss_limit_mb=2048
cargo +nightly fuzz run program_exec     fuzz/corpus/program_exec     fuzz/seeds/program_exec     -- -max_total_time=300 -timeout=10 -rss_limit_mb=2048
cargo +nightly fuzz run interface_codec  fuzz/corpus/interface_codec  fuzz/seeds/interface_codec  -- -max_total_time=300 -timeout=10 -rss_limit_mb=2048
```

The flags are the campaign's (docs/PROGRAMS-V0.md §10.4): an input that runs
past 10 seconds is a hang, and a hang is a finding, as is going past 2 GiB.

The harness logic is in `src/lib.rs`; `cargo test --locked --manifest-path
fuzz/Cargo.toml` runs the seed checks on the pinned stable toolchain (no fuzzer
needed).

CI runs each target nightly for five minutes (`.github/workflows/fuzz.yml`'s `fuzz-vm` job, on the
same schedule and with the same pinned nightly as `node/fuzz`'s), with the campaign's flags, its own
corpus cache (`fuzz/corpus/<target>`, kept between runs, saved only after a run without a crash),
output to a file rather than the public log, and a crash sealed to the maintainers' age keys
(`node/fuzz/README.md`, "Findings"). The seeds are checked on every PR by the `Fuzz harness` job.

The sustained run is two weeks under AddressSanitizer on the floor machines
(plan S4). `ref038` is left off: the targets fuzz the engine a node runs.

## A crash is a vulnerability

Same rule as `node/fuzz` and SECURITY.md: a crash the fuzzer finds is an
undisclosed vulnerability. Keep its input private, never commit it, never put
it in a public issue or PR, and handle it as a private security advisory
(`node/fuzz/README.md`, "Findings"). A crash lands under the git-ignored
`fuzz/artifacts/<target>/`; an open finding's input is kept in the git-ignored
`fuzz/private/<target>/` (or wherever the advisory's private fork keeps it),
and becomes a regression seed only once the fix is public. `private/` and
`coverage/` (a report names the code a crash reaches) are ignored here as in
`node/fuzz`, and `scripts/check-fuzz-workflow.sh` (CI) fails if either stops
being ignored or if a `fuzz/regressions/` directory appears.
