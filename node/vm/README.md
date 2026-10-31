# blockula-vm: the programs engine

The crate that runs programs (protocol v2): third-party WebAssembly programs, never "contracts".
Design: [docs/PROGRAMS.md](../../docs/PROGRAMS.md) sections 4 (the VM), 5 (fees and metering),
6 (state and deposits), 8 (calls) and 10 (the host API); phase 0's record and D1 in
[docs/programs/phase0.md](../../docs/programs/phase0.md). It has its own workspace, like
`node/ghost`, so its pins and profiles never touch the node's (`node/Cargo.toml` excludes it).

The engine is **soroban-wasmi 29.0.0** (D1), pinned exactly; any bump is a consensus change made
with a protocol version. **wasmi 0.38.0** (`--features ref038`) is a differential oracle for
tests only: every test that runs on `EngineKind::ALL` runs on it too, and must agree with
soroban-wasmi on everything but fuel. No node ever runs it.

The crate never touches the node's state: it reads a `View` and returns a sorted `Diff`. Wiring it
into the node (the records, admission, fees, commit, receipts root) is the node's work.

## Layout

| File | What |
|---|---|
| `src/abi.rs` | Host ABI v1: the import table, status codes, flags, `MAX_SUPPLY`, `is_ident` |
| `src/config.rs` | Code-constant limits (pages, functions, methods, locals, depth, keys, values, events, batch, declared code…) |
| `src/meter.rs` | Fuel schedule v0 (`SCHEDULE_V0`), the schedule per protocol version, the reservation bounds |
| `src/validate.rs` | Upload validation (4.2), engine-independent, on `wasmparser =0.228.0`; each `Rule` numbered for receipts |
| `src/upload.rs` | `upload::check`: `UploadCode`'s checks on new code (validation, then a test translation), refusals apart from node faults |
| `src/interface.rs` | Interface format 1 (docs/PROGRAMS-V0.md §3): `decode` (every rule), `encode`, method selectors, `check_exports` against the code's `bk_` exports, `Interface::method` |
| `src/engine/mod.rs` | The engine-neutral boundary: `BlockEngine` (an engine per transaction, or per block pass in the node), `Host`, `Instance`, `Trap`, `TrapKind`, `NodeFault`, `CodeInfo` |
| `src/engine/soroban.rs` | The soroban-wasmi backend and its configuration (4.2 mapped onto its `Config`) |
| `src/engine/wasmi038.rs` | The wasmi 0.38 oracle backend (`ref038` only) |
| `src/host.rs` | The `View` trait (what execution reads), method kinds, the host functions |
| `src/overlay.rs` | The layered overlay, deposits, the sorted `Diff` and its commit checks |
| `src/exec.rs` | `execute`: frames, one meter, the declared set, versions, method kinds, receipts |
| `src/storage.rs` | Storage economics: an entry's capacity and the price curve (6.2, 6.3) |
| `src/testing.rs` | `MemHost`, an in-memory chain (feature `testing`: tests, benches, fuzzing; never a node build) |
| `guests/` | Hand-written test programs for `wasm32v1-none` (`counter`, `token`, `escrow`, `router`), their own workspace |
| `tests/programs/*.wasm` | The built guests, committed so tests are hermetic (`guests/build.sh --check` rebuilds them byte for byte; CI does on Linux) |
| `tests/programs/*.iface.json`, `*.iface` | The guests' interfaces, as a deployer writes them (JSON, §3.4) and as `UploadCode` carries them (format 1); `tests/interfaces.rs` writes and checks both |
| `tests/` | `exec.rs` (the executor, WAT programs), `e2e.rs` (the guests end to end), `guests.rs` (the guests against 4.2 and ABI v1), `engines.rs` (engine facts), `interfaces.rs` (the guests' interfaces) |
| `fuzz/` | cargo-fuzz targets `program_validate` (with a wasm-smith generator for v0's subset), `program_exec` and `interface_codec`, their own workspace |
| `benches/calibrate.rs` | The calibration bench (`cargo bench --bench calibrate`; `.github/workflows/calibrate-vm.yml` runs it on the floor machines) |
| `CALIBRATION*.md` | Phase 0's measurements, the basis of schedule v0 |

## How the node drives it

**An engine per transaction outside a block pass, one per pass in the node.** PROGRAMS.md 4.3
shares one engine across a block, and so does the node's block pass (`node/src/programs/pass.rs`,
docs/PROGRAMS-V0.md A3, with the argument in `CALIBRATION-node.md`, "A3: one thread and one engine
per block pass"): one thread with a 64 MiB stack and one engine for all the program
transactions of a pass, where each transaction still loads every code it declares, code the engine
already holds is a no-op, and the engine is dropped after any fault and when it has taken in too
much code. Outside a pass (`/view`, a lone transaction, or a pass the OS gave no thread) each program
transaction gets a fresh engine, loaded with only the code it declares and dropped when it's done,
on a thread made for it. Either way nothing a transaction does depends on what else its engine
holds: it reaches only its declared set, and no fuel is read off the engine. The API serves
either; `BlockEngine` and `new_block_engine` keep 4.3's names. For each program transaction, on a
64 MiB stack (host calls nest native frames up to `MAX_CALL_DEPTH` deep):
1. `meter::schedule(version_at(h))` is the schedule (`None` before v2: no program can run).
2. `engine::new_block_engine(EngineKind::Soroban, schedule)` makes the engine (an `Err` is a node
   fault). A `Box<dyn BlockEngine>` isn't `Send`: make it on the thread that uses it, and let it
   go with that thread.
3. `engine.load(hash, wasm)` loads every distinct code the transaction declares (the code in force
   of each call's program and each binding), **before anything runs**: soroban-wasmi deadlocks if
   code is translated on an engine while a call runs on it, so `load` takes `&mut self` and
   execution only `&dyn BlockEngine`. The engine keys code by the hash it's given, which it never
   computes (`CodeHash`): the node passes its `wasm_hash`. Stored code passed `upload::check`, so
   a `LoadError` here is stored code that no longer loads: a node fault (`NodeFault::Load`, 7.6).
   Load fuel is admission's, from the code's size: `schedule.load_fuel(size)` per distinct code,
   in each transaction's reservation.
4. `exec::execute(&*engine, view, signer, calls, bindings, limits, schedule)`:
   - `Ok(receipt)`: the transaction's outcome either way. If `receipt.ok()`, apply
     `receipt.diff` (below) then the node's own checks (the value cap); if not, it's `Failed`
     with `receipt.fail`, its fee charged, nothing else applied.
   - `Err(NodeFault)`: the node can't execute it. Commit nothing of the block and halt; never
     record it as a transaction's failure.
5. The engine is dropped with the thread, after the receipt and after any node fault alike, or
   (on a pass's engine) kept for the pass's next transaction after a receipt and dropped after any
   node fault.

**Applying a diff** (4.4), in this order: `storage` (each `StorageChange` sets or removes
`program ‖ key`); `programs` (each program's `items`, `bytes` and `deposit` change, for its
record) and `state_bytes` (META's total); then `balances` (net per account, deposits folded in:
debits then credits); then `opened` (accounts the transaction opened; `burned` is their creation
fees, already debited from the programs that opened them); `escrow` is the change in deposits
held. `execute` has already checked conservation and who was debited (`Diff::check`).

**Interfaces** (docs/PROGRAMS-V0.md §3): `interface::decode(bytes)` checks an interface statelessly
(format 1, every rule of §3.2, one encoding per interface), so the node runs it in `check_format`
on every `UploadCode`. At upload, and at create when the code and the interface came from
different uploads, `interface::check_exports(&interface, &exports)` matches its methods one to
one with the code's `bk_` exports (`ModuleInfo::exports`, or the stored code's). The node answers
`View::method` from the stored interface with `Interface::method(name)`.

**Upload** (`UploadCode`, 7.1): for code the chain doesn't hold yet, `upload::check(wasm,
schedule)`, on the same kind of thread, runs upload validation (`validate::validate`), then a test
translation on a fresh soroban-wasmi engine, dropped after. Its result is shaped like
`execute`'s:
- `Ok(Ok(ModuleInfo))`: the code is accepted. `ModuleInfo` has the method names to match one to
  one with the interface's, the imports, and the `CodeInfo`.
- `Ok(Err(Refusal))`: the upload fails, the same on every node: `Refusal::Invalid(e)` (its
  receipt records `e.rule.number()`, below) or `Refusal::Translate(_)` (its receipt records only
  that the translation was refused; the engine's words are for logs).
- `Err(NodeFault)`: a panic in validation or in the test translation (caught:
  `NodeFault::Panic`), or an engine that can't be made. **Never a refused upload:** commit
  nothing of the block and halt, as for `execute`.

`testing::MemHost::upload` runs `upload::check` and stores the code only if it's accepted. An
upload's fuel is `schedule.upload_fuel(wasm.len(), interface.len())`, fixed by size; its deposits
are the node's, priced as entries are (Storage, below).

## The `View` (what execution reads)

Every answer is the state at the executing block's height, before the transaction:

| Method | Answers |
|---|---|
| `height()` | the executing block's height |
| `balance(account)` | its balance in drops, `None` if it doesn't exist |
| `code_of(program)` | the code hash in force (7.3), `None` if it isn't an active program |
| `version_digest(program)` | the version digest in force (11.2), `None` if it isn't an active program (a program with code but no digest is a `NodeFault`) |
| `method(program, name)` | the interface's `MethodInfo { kind: Call \| View \| Init, payable }`, `None` if it has no such method |
| `storage(program, key)` | the stored `Entry { value, payer, deposit, capacity }` |
| `state_bytes()` | program state's accounted bytes (6.2) |
| `storage_price(used)` | drops per accounted byte when state would hold `used` bytes; `None` past the ceiling. The node answers `storage::price(program_storage_price, program_state_soft_bytes, used)` |
| `account_creation_fee()` | what opening an account burns |

## What `execute` checks

- **The declared set** (8.1): a call reaches its own program and the bindings, nothing else
  (`UnboundProgram`); no program twice on the stack (`Reentrancy`); at most 8 deep (`CallDepth`).
- **Versions:** a `Call` runs only if its `version_digest` is the program's in force (no
  wildcard); a `Binding`'s digest, and `call_v1`'s `expect`, are checked each time a frame of
  that program opens, zeros (`ANY_VERSION`) accepting any (`VersionMismatch`).
- **Method kinds** (11.1): a top-level call runs only a method of its `Call::kind` (`Call` for
  `CallProgram`, `Init` for `CreateProgram`'s initializer, `View` for a read-only call); `call_v1`
  never reaches an `Init` method (`NotCallable`); a `View` frame, and everything it calls, may not
  write, remove, transfer, emit, move value or call a non-`View` method (`ReadOnly`); value goes
  only to payable methods (`NotPayable`).
- **Each call's own bounds:** an identifier method, input ≤ 32 KiB, value ≤ `MAX_SUPPLY` (a
  failed receipt). The batch bounds (1–8 calls, ≤ 16 bindings, Σ inputs, `MAX_TX_CODE_BYTES`) are
  the tx codec's and admission's; `execute` re-checks the counts, and a batch without calls, with
  more than `MAX_CALLS` or with more than `MAX_BINDINGS` bindings is a `NodeFault::Host` (no valid
  transaction has one), never a receipt.

## Receipts and node faults

A `Receipt` holds `fail: Option<FailCode>`, `fuel_used`, `returns` (each call's return data),
`events` and the `diff`; a failed one has nothing but the code and the fuel. A transaction that
fails out of fuel reports `fuel_used == fuel_limit`, wherever it ran out: a callee that exhausts a
smaller share its caller gave it through `call_v1` bills the whole `fuel_limit` too, once its
`OutOfFuel` fails the transaction. (A `try_call_v1` on the way up absorbs it: its caller runs on,
and only the failed frame's share counts as used.) Fees don't depend on it (the node charges what
a transaction reserves), but receipts show it. Receipts hold numbers and the program's own bytes only, never an engine's or
the host's text, so no release that rewords an error can change one:
- `FailCode::Trapped { program, kind }` (1): `TrapKind`'s numbers 1–30 (`engine/mod.rs`);
- `FailCode::Aborted { program, code: u32, message }` (2): the program's `abort_v1`, its message
  sanitized and at most 256 bytes;
- `FailCode::Commit(CommitError)` (3): the diff failed the commit checks, a host bug every node
  reaches alike.

An upload refused by validation records its `validate::Rule`'s number, 1–14 (`Rule::number`,
`Rule::from_number`, `Rule::ALL`; `validate.rs`'s tests pin the table).

**The numbers are consensus** once receipts are committed (6.5): a kind or a rule keeps its number
forever, and `tests/exec.rs` and `validate.rs` pin the tables. What no valid transaction can cause
(an engine error, code that isn't loaded, a broken host invariant, a caught panic, the fuel
counter failing, a batch no codec would decode) is a `NodeFault`, returned instead of a receipt;
`try_call` never absorbs one.

## Fuel schedule v0

`meter::SCHEDULE_V0` (`meter.rs` gives every number's basis; docs/programs/phase0.md the
measurements): operators by class base 3, entity 4, load 20, store 4, call 30; load 10,000 + 40
per byte; upload 50,000 + 71 per byte of code and interface; instantiation 2,000 + 300 per import,
500 per export, 15 per function, 20 per global, 50 per element segment, 2 per table element, 20
per data segment, 2,000 per page and 1 per data byte (`CodeInfo` counts them); host functions as
PROGRAMS.md 4.5's table, `abort_v1` +1,000, `debug_log_v1` priced like `emit_v1`. Every item but
upload is 1.5–3× the worst floor figure phase 0 measured. Upload's 71 per byte is the most that
keeps a maximum upload within `MAX_TX_FUEL`, about 1.22× the worst floor figure (58 fuel per
byte): the thinnest margin in the schedule. The calibration bench reports upload's ns per fuel as
its own go/no-go line; if a floor run puts it over 2.5, the price can rise only with smaller code
limits or a new schedule (a protocol version either way), and until then only a lower block fuel
budget keeps a block of uploads within its time.

**Bounds** (const-asserted in `meter.rs`): a transaction reserves at most `MAX_TX_FUEL` (20M) of
execution plus `max_load_fuel()` (21.2M: 24 declared codes, `MAX_TX_CODE_BYTES` = 512 KiB of
them), 41.2M in all, within `MAX_RESERVATION` = 2^26; a maximum upload (256 KiB of code, 16 KiB of
interface) is 19.8M, within `MAX_TX_FUEL`.

## Storage

An entry accounts for `storage::capacity(key, value)` bytes: key + value + 96, rounded up to a
multiple of 64. A write that fits the entry's capacity bonds nothing; otherwise the old payer is
refunded and the payer (the signer, or the program with `SPONSOR`) bonds the new capacity at
`View::storage_price(used)`, fixed then, never re-priced. Past the ceiling there's no price
(`StateFull`). The signer's bonds count against `storage_limit`.

**The convention: a deposit is priced at the bytes once the write lands.** `used` is what
`View::state_bytes()` would be with the write in: the committed bytes, plus the net bytes this
transaction's live layers have added so far in execution order (removals and failed frames taken
back), plus this write's growth (its capacity less the capacity it replaces). Never the bytes
before it. The node bonds what it stores outside execution by the same rule: each upload blob
(code, then interface) and a create's program record at `storage_price(used)`, `used` counting
that blob or record.

## Opening accounts

A program's `transfer_v1` to an account that doesn't exist is a failure status (`ERR_TRANSFER`,
nothing moves) unless it passes a flag, and unless the account opens with at least
`config::MIN_NEW_ACCOUNT` (0.01 FANG, the node's `types::MIN_NEW_ACCOUNT`) in it
(docs/PROGRAMS-V0.md A30), as for the node's native transfers:
- `OPEN_DEDUCT`: the creation fee comes out of the amount, so the amount must be at least the fee
  plus `MIN_NEW_ACCOUNT`;
- `OPEN_PAY`: the program burns the fee on top of the amount, which must be at least
  `MIN_NEW_ACCOUNT`.

Accounts a transaction opened are `Diff::opened` (their fees are `Diff::burned`), so the node
never bills the signer for them again. A keyless system account is an existing account to the VM:
the node's `View::balance` reads it as `Some(0)`, so no program pays to "open" one.

## Rules the crate keeps

- **Upload validation first.** Code reaches an engine only after `validate::validate()` passed
  it, then a test translation on a throwaway engine (`upload::check`): no engine's translator is
  trusted with code validation refused. A panic in either is a node fault, never a refusal.
- **An engine for a transaction, or a pass**, made on its thread, loaded before anything runs, and
  dropped after any node fault (above). No code cache outlives a pass, and nothing a transaction
  reaches comes from what an earlier one loaded.
- **Memory never grows: validation refuses `memory.grow`.** Exactly one memory, defined, exported
  as `memory`, initial = maximum ≤ `config::MAX_PAGES`; `memory.grow` is refused anywhere in a
  body, reachable or not (`Rule::MemoryGrow`): with a fixed memory it can do nothing a program
  needs, and refusing it keeps its cost the validator's, not the engine's. `memory.size` stays
  allowed.
- **At most 64 methods** (`bk_` exports) per code: the interface's limit.
- **One meter per transaction**, shared by every frame. Host functions charge before doing work.
- **Execution never mutates the state.** Host functions read a `View` and write a layered
  overlay; on success it becomes a sorted diff, checked for conservation and for who was debited.
- **Determinism.** No `HashMap`/`HashSet` (Clippy refuses them, `clippy.toml`), no floats, no
  clock, no RNG, no fuel that depends on a cache.

## Overflow checks

Arithmetic overflow must panic (and so become a node fault) rather than wrap, in this crate and in
the engine's crates. This workspace's `release` and `bench` profiles turn `overflow-checks` on for
its own tests, benches and fuzzing. Once the node depends on this crate, only `node/Cargo.toml`'s
profiles apply, so it must list `[profile.release.package.*]` entries with `overflow-checks = true`
for `blockula-vm`, `soroban-wasmi`, `wasmi_core`, `wasmi_arena`, `wasmparser-nostd` and
`wasmparser`, with a test (like `ghost::tests::release_profile_covers_plonky3`) that fails if one
is missing. `panic` stays `"unwind"`: `execute`'s `catch_unwind` relies on it.

## Testing

```
cargo test --release --locked                      # the suite (MemHost comes in through the dev-dependency on itself)
cargo test --release --locked --features ref038    # and the wasmi 0.38 oracle alongside
cargo test --locked --manifest-path fuzz/Cargo.toml # the fuzz harnesses' seed checks, on stable
guests/build.sh --check                            # the committed guests rebuild byte for byte
cargo bench --locked --bench calibrate -- --quick  # the calibration bench (ns per fuel, this machine)
```

CI runs the first four on every pull request, and `scripts/clippy.sh` lints the crate. Long-running
experiments are `#[ignore]`d; `cargo test -- --ignored` runs them.
