# blockula-vm: the programs engine

The crate that runs programs (protocol v2): third-party WebAssembly programs, never "contracts".
Design: [docs/PROGRAMS.md](../../docs/PROGRAMS.md) sections 4 (the VM), 5 (fees and metering),
6 (state and deposits), 8 (calls) and 10 (the host API). It has its own workspace, like
`node/ghost`, so its pins and profiles never touch the node's (`node/Cargo.toml` excludes it).

The engine is **soroban-wasmi 29.0.0** (D1), pinned exactly; any bump is a consensus change made
with a protocol version. **wasmi 0.38.0** (`--features ref038`) is a differential oracle for
tests only: every test that runs on `EngineKind::ALL` runs on it too, and must agree with
soroban-wasmi on everything but fuel. No node ever runs it.

## Layout

| File | What |
|---|---|
| `src/abi.rs` | Host ABI v1: the import table, status codes, flags, `MAX_SUPPLY`, `is_ident` |
| `src/config.rs` | Code-constant limits (pages, functions, locals, depth, keys, values, events…) |
| `src/meter.rs` | The fuel schedule, engine-neutral |
| `src/validate.rs` | Upload validation (4.2), engine-independent, on `wasmparser =0.228.0` |
| `src/engine/mod.rs` | The engine-neutral boundary: `BlockEngine`, `Host`, `Instance`, `Trap`, `CodeInfo` |
| `src/engine/soroban.rs` | The soroban-wasmi backend and its configuration (4.2 mapped onto its `Config`) |
| `src/engine/wasmi038.rs` | The wasmi 0.38 oracle backend (`ref038` only) |
| `src/host.rs`, `src/overlay.rs`, `src/exec.rs` | Host functions over a read-only `View`, the layered overlay and its sorted diff, frames and the executor |
| `src/testing.rs` | `MemHost`, an in-memory chain (feature `testing`: tests, benches, fuzzing) |
| `guests/` | Hand-written test programs for `wasm32v1-none` (`counter`, `token`, `escrow`, `router`), their own workspace |
| `tests/programs/*.wasm` | The built guests, committed so tests are hermetic (`guests/build.sh --check` rebuilds them byte for byte) |
| `tests/` | `exec.rs` (the executor, WAT programs), `e2e.rs` (the guests end to end), `guests.rs` (the guests against 4.2 and ABI v1), `engines.rs` (engine facts) |
| `fuzz/` | cargo-fuzz targets `program_validate` and `program_exec`, their own workspace |
| `benches/calibrate.rs` | The calibration bench (`cargo bench --bench calibrate`) |
| `CALIBRATION*.md` | Phase 0's measurements, the basis of the fuel schedule |

## Rules the crate keeps

- **Upload validation first.** Code reaches an engine only after `validate::validate()` passed
  it, then a test translation on a throwaway engine: no engine's translator is trusted with code
  validation refused.
- **An engine per block.** One `BlockEngine` per block, loaded with every declared program's code
  before any call that might reach it runs, and dropped after. soroban-wasmi deadlocks (a spin
  `RwLock`) if a module is translated on an engine while a call runs on it, so nothing may be
  translated during execution: `BlockEngine::load` takes `&mut self` and execution holds only
  `&dyn BlockEngine`.
- **Memory never grows: validation refuses `memory.grow`.** Exactly one memory, defined, exported
  as `memory`, initial = maximum ≤ `config::MAX_PAGES`; `memory.grow` is refused anywhere in a
  body, reachable or not (`Rule::MemoryGrow`), and `memory.size` stays allowed.
- **One meter per transaction**, shared by every frame. Host functions charge before doing work.
  An out-of-fuel transaction reports `fuel_used = fuel_limit`.
- **Calls.** `call` traps the caller if the callee fails; `try_call` returns a status. No
  reentrancy, depth ≤ `config::MAX_CALL_DEPTH`, only the declared program set is reachable.
- **Execution never mutates the base state.** Host functions read a `View` and write a layered
  overlay; on success it becomes a sorted diff, checked for conservation and for who was debited.
- **Determinism.** No `HashMap`/`HashSet` (Clippy refuses them, `clippy.toml`), no floats, no
  clock, no RNG.

## Testing

```
cargo test --release --locked                      # the suite (MemHost comes in through the dev-dependency on itself)
cargo test --release --locked --features ref038    # and the wasmi 0.38 oracle alongside
cargo test --locked --manifest-path fuzz/Cargo.toml # the fuzz harnesses' seed checks, on stable
guests/build.sh --check                            # the committed guests rebuild byte for byte
```

Long-running experiments are `#[ignore]`d; `cargo test -- --ignored` runs them.
