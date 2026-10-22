# Programs phase 0: the go/no-go memo (S5)

Status: **D1 decided, 2026-10-20: soroban-wasmi 29.0.0.** wasmi 2.0 is dropped; wasmi 0.38.0 stays
as a test-only differential oracle (`--features ref038`). Phase 0 started 2026-10-19. It built the
engine prototype (S1), the hand-written guests (S2), development measurements on the maintainer's
Mac and a first floor run on x86 and aarch64 Linux (S3, section 4.3), and fuzz targets building and
running clean (S4 started). The prototype is now the production crate on main (`node/vm`, section
7), with fuel schedule v0 built from these measurements. Still owed: a floor run under schedule v0,
a dedicated x86 run and the two-week AddressSanitizer run (section 5). Every number in sections 2–6
is a development measurement of the prototype, under its starting schedule (v1 in its records);
even the floor run is `--quick` on a shared runner. Design: [PROGRAMS.md](../PROGRAMS.md). Plan:
[PROGRAMS-PLAN.md](../PROGRAMS-PLAN.md) section 5.

## 1. The question

D1: which engine to pin for programs. The candidates:
- `soroban-wasmi` 29.x, the default since D3. With no external review of Blockula's programs, the
  default is the engine a production chain runs, from the 0.31 line SRLabs audited;
- wasmi 2.0.x, the alternative;
- wasmi 0.38.0, the last audited release, as a reference only.

Phase 0 was to pick wasmi 2.0 only if its measured advantages were large; they weren't, and it
picked soroban-wasmi (section 6). The engine sits behind `node/vm/src/engine/`.
Swapping engines changes that directory, the pin and the calibrated schedule (the `meter.rs`
values and `LOAD_PER_BYTE`: the same program uses different fuel on each engine, section 3.3). It
doesn't change the ABI, the fee mechanism, the state or the tools.

## 2. What phase 0 built (the prototype)

`node/vm`, the `blockula-vm` crate in its own workspace, as the prototype stood when these
measurements were taken (section 7 says what main's crate changed since):
- **The engine-neutral boundary and three backends:** soroban-wasmi 29.0.0, wasmi 2.0.0 and, behind
  `ref038`, wasmi 0.38.0. There's one engine per block and one store per frame, dropped when the
  frame returns. Main keeps soroban-wasmi and the `ref038` oracle.
- **Host ABI v1:** a subset of 10.2, plus two functions 10.2 lacks (section 3.4). It covers input
  and output, the program's own storage with deposits, context reads, transfers with the opening
  flags, `call`, `try_call`, `last_return`, `last_return_len` and `last_abort_code`, and events.
- **The executor:** a read-only view plus a layered overlay, one layer per frame. A successful
  transaction becomes a sorted diff that's checked for conservation and for who was debited.
  Execution shares one fuel meter, honours the declared program set, refuses reentrancy, limits
  depth to 8 and runs under a `catch_unwind` backstop. Running out of fuel reports
  `fuel_used = fuel_limit`.
- **Upload validation** (`validate.rs`): 4.2 enforced the same way whichever engine runs. It's
  stricter than either engine, on wasmparser 0.228.0.
- **The guests:** a counter, a token, an escrow and a router (which calls the token through `call`
  and `try_call`), written by hand in `no_std` Rust for `wasm32v1-none` on Rust 1.98.1. They're
  565 B, 2,467 B, 3,389 B and 1,484 B. `build.sh --check` rebuilds them byte for byte on the
  maintainer's Mac, and in `rust:1.98.1-bookworm` on x86-64 and aarch64 Linux (CI's Node job runs
  it on every pull request).

Every scenario runs on every engine, cold on a fresh engine and then warm. Within an engine the
receipts are identical, fuel included. Across engines everything matches except fuel. The one
exception was `memory.grow` past the fuel left, which validation refuses (section 3.3).

## 3. Findings

### 3.1 Engine maturity and robustness

- **wasmi 2.0.0** is dropped (D1), so no build of the node contains it. This record keeps only its
  measurements (sections 3.2–4).
- **soroban-wasmi** came through phase 0's tests and its local fuzzing runs (S4) clean.
- **soroban-wasmi deadlocks if a module is translated on an engine from inside a host call on that
  engine.** Its engine holds a spin read-lock for the whole call, and translating needs the write
  lock. (Another thread would wait for the call to end, not deadlock.)
  - Inside a host call it's safe to create a store, instantiate an already-translated module and
    call into it. Translating or wrapping a host function on that engine is not.
  - PROGRAMS.md doesn't state the rule yet. Load is priced at admission (4.5), but 4.3's per-block
    cache would also allow translating on first use, which deadlocks when the first use is a
    nested call. 4.3 needs the rule (section 3.4).
  - The crate enforces it by type: `BlockEngine::load` takes `&mut self` and `invoke` takes
    `&self`, and `exec::execute` (with the host it builds) holds only a shared `&dyn BlockEngine`.
    The node must keep that shape.

### 3.2 What each engine can express

| PROGRAMS.md 4.2 / 4.5 | soroban-wasmi 29.0.0 | wasmi 2.0.0 |
|---|---|---|
| Fuel on Wasm operators | 5 classes (base, entity, load, store, call), tied to its IR. Charged per metered block up front, each block's charge starting at `base` | One `u8` cost per Wasm operator (207). Charged per metered block up front, plus a fixed 1 per metered block (function entry, loop, `if`/`else` arm) |
| Per-local cost on calls | 1 per 8 locals, fixed | None: locals are zeroed for free |
| `br_table` by targets | None | None |
| Translation fuel | Not metered (load is priced at admission) | Not metered under `Eager` |
| `floats(false)` | Leaves float→int conversions and reinterprets in unreachable code | Same: wasmparser 0.228 doesn't refuse them |
| Start function, custom sections | No `Config` switch; `validate.rs` refuses them | `allow_start_fn(false)`; custom sections ignorable, but `validate.rs` refuses them for both |
| Translation limits | No `EnforcedLimits`: only declared-count checks and its parser's loose caps (1M functions, 50,000 locals, 131,072 `br_table` targets) | `EnforcedLimits::strict()`, fixed values; also a 65,535-slot frame limit per function |
| Value stack and recursion limit | Per `Func::call`, so per frame, in 8-byte cells | Per `Stack`, so per frame, in bytes |

`validate.rs` closes the validation gaps in the table, and refuses more than either engine:
- float operators anywhere;
- start sections and custom sections;
- `memory.grow` (section 3.3);
- an operand stack deeper than 4,096;
- wasmi 2.0's `EnforcedLimits::strict()` values and 4.2's tighter caps, whichever engine runs;
- segments that don't fit their memory or table;
- anything but Wasm 1.0's single-constant expressions;
- a data count section, passive data or bulk-memory encodings;
- duplicate or re-exported imports.

The fuel gaps (per-local cost, `br_table` targets) aren't validation's to close; section 3.4 says
how they're priced.

### 3.3 Where the engines disagree

- **`memory.grow`.** soroban-wasmi charges 1,024 fuel per requested page before refusing past the
  maximum. wasmi 2.0 and 0.38 refuse first at their static cost. With little fuel left, the same
  program runs out of fuel on one engine and gets −1 on the others. Recommendation: `validate.rs`
  refuses `memory.grow`, since memory never grows anyway.
  - **Done.** `validate.rs` refuses `memory.grow` anywhere in a function body,
    reachable or not and by 0 pages too (`Rule::MemoryGrow`, naming the function and offset).
    `memory.size` stays allowed, and no committed guest uses `memory.grow`.
- **Fuel.** The engines charge different fuel for the same program. That's expected: each maps the
  engine-neutral schedule onto its own cost model (five IR classes against one cost per Wasm
  operator), and wasmi 0.38 fuels its own IR. A chain pins one engine and calibrates the schedule
  for it.
  - The gaps are largest for functions with many locals, which soroban-wasmi charges for and
    wasmi 2.0 doesn't: 52,002 fuel against 26,001 for 1,000 calls with 200 locals, and 12 times as
    much with 1,024. On `tests/exec.rs`'s loops, wasmi 0.38's IR-based fuel is a third to
    a half of the others'; per operator in CALIBRATION.md it ranges from about a quarter
    (3-address arithmetic) to nine tenths (unary chains, loads, globals).
  - Section 4 turns them into nanoseconds per fuel.

### 3.4 Design corrections for PROGRAMS.md

- **4.2's profile isn't Soroban's.** Soroban's host also allows sign extension and bulk memory, and
  Blockula's is stricter.
- **4.2's code block is wasmi 2.0's API.** On soroban-wasmi the same settings go through its own
  `Config`; `node/vm/src/engine/soroban.rs` maps each one and names the gaps `validate.rs` closes.
- **`memory.grow` is refused at upload.** 4.2 has it return −1; validation refuses it instead
  (section 3.3), so no stored program has one.
- **Custom sections are refused, not ignored.** soroban-wasmi keeps them in memory, and two
  uploads that differ only in a custom section would behave the same but hash differently.
- **Every declared code is translated before anything in the block runs (4.3).** Translating on
  first use deadlocks soroban-wasmi inside a nested call (section 3.1).
- **4.5's "10 + 2 per local" and "`br_table` 1 + 1 per 8 targets" can't be expressed on either
  engine.**
  - Locals have to be bounded by the 1,024-local cap and priced through `call` and
    `call_indirect` at their measured worst case. wasmi 2.0 zeroes them for free on every call,
    Wasm-internal ones included, so the host's per-frame instantiation charge can't bound calls
    made in a loop.
  - `br_table` measures within budget at one static cost on the Mac (4.1), and its targets are
    paid for at load.
- **Overflow checks.** soroban-wasmi's crate list also needs `wasmi_arena`.
- **Node faults are never a transaction's outcome.** Engine errors, out-of-memory reported as an
  error, a caught panic and broken host invariants must stop the node, never become a
  transaction's `Failed`. The prototype reported them as `Trapped`; main's `exec::execute` returns
  them as a `NodeFault` instead of a receipt (section 7).
- **Guest builds (12.1).** `wasm-opt -Oz` adds sign extension unless it's told the feature set.
  - The default `wasm32v1-none` link gives 17 pages with no maximum, so the link arguments must set
    initial = maximum (at most 32 pages; the guests use 32) and a stack that fits (the guests use
    64 KiB, against rustc's 1 MiB).
  - Without bulk memory, compiler-builtins links a byte-loop `memset` and `memcpy`. They were most
    of the token's code before the guests stopped zero-filling buffers the host overwrites.
- **ABI v1 gaps (8.2, 10.2).**
  - After `try_call` a caller can't learn the return length.
  - 8.2 says an aborted callee's code is readable, but no function returns it.
  - `storage_read` with a value longer than `cap` is unspecified.
  - The crate adds `last_return_len_v1` and `last_abort_code_v1`, puts the sanitized abort message
    in `last_return` after `ERR_ABORT`, and clears both before each call. Its `storage_read` copies
    `min(len, cap)` bytes and returns the full length.
  - Still open: `last_return_v1` takes no capacity, and `storage_read` bounds-checks only the bytes
    it copies.

## 4. Measurements

4.1 and 4.2 are the development run on the maintainer's Apple M4 Max; 4.3 is the first floor run on
x86 and aarch64 Linux. The full tables are in
[`node/vm/CALIBRATION.md`](../../node/vm/CALIBRATION.md) (Mac) and
[`CALIBRATION-x86_64.md`](../../node/vm/CALIBRATION-x86_64.md) /
[`CALIBRATION-aarch64.md`](../../node/vm/CALIBRATION-aarch64.md) (floor), with how to rerun them.
"Schedule v1" in them and below is the prototype's starting schedule (PROGRAMS.md 4.5's table);
main prices with schedule v0 (section 7).
- **Setup (4.1, 4.2):** one Apple M4 Max, rustc 1.98.1, release with `overflow-checks`, `--quick`
  (the median of 3). Calls go through the real executor on its 64 MiB thread, and translation and
  raw instantiation call the block engine directly. The native-stack scenarios go through the
  executor too, in child processes on threads whose stack is bisected.
- **These aren't prices.** The Mac is faster than both floor machines, so every ratio against the
  2.5 ns-per-fuel rule in 4.1–4.2 is optimistic; 4.3 shows by how much.
- **`--quick` medians move between runs.** soroban-wasmi's pointer chase across 2 MiB, the
  candidates' noisiest case, read 5.9 ns/fuel in an earlier run and 7.1 in this one. Section 2
  (translation) comes from a second run of that section alone.
- **Storage costs here exclude the node's.** `MemHost` is a `BTreeMap`, so the node's real costs
  for storage (redb rows, state-tree hashing) aren't measured. C6 adds them.

### 4.1 Against the 2.5 ns-per-fuel rule (schedule v1)

| What | soroban-wasmi | wasmi 2.0 | Verdict |
|---|---:|---:|---|
| Arithmetic, compare, locals, globals, `select`, stores | 0.6–1.6 ns/fuel | 0.04–1.1 | Priced with room |
| Control flow | 0.6–1.6 | 0.3–1.2 | Priced |
| Loads in cache, and independent random loads | 0.8–1.6 | 0.3–1.0 | Priced |
| `br_table` | 1.1–2.4 | 0.2–0.7 | Priced, but soroban-wasmi's 8-target case is at 96% of the budget |
| Calls with up to 64 locals | 0.4–0.7 | 0.3–0.9 | Priced |
| Calls with 256 locals | 0.5 | 1.7 | Priced; wasmi 2.0 crosses 2.5 at about 400 locals |
| **Calls with 1,024 locals** (also `call_indirect` and recursion) | 0.5–0.7 | **5.7–6.2** | **Underpriced on wasmi 2.0, 2.3–2.5×**: it zeroes locals without charging fuel |
| **Pointer-chasing loads across 2 MiB** (cache misses) | **7.1** | **5.2** | **Underpriced on both: 2.9× on soroban-wasmi, 2.1× on wasmi 2.0** |
| Every host function in ABI v1 (all 20) | ≤ 1.2 | ≤ 1.7 | Priced. The maximum is `abort_v1`, once a transaction; repeatable calls are ≤ 0.35, most ≤ 0.3 |

### 4.2 Load, instantiation and the block budget

- **Translation and validation.**
  - Each engine's worst shape implies a `LOAD_PER_BYTE` of **10.8 on soroban-wasmi** (14,980
    distinct function types, 223 KB) and **20.9 on wasmi 2.0** (6,094 functions with 1,024 locals
    each, 262 KB; the first run's load row implies about 23), against the schedule's 10. Other
    shapes are over 10 too: 87,364 identical function types imply 10.3 on soroban-wasmi and 16.1 on
    wasmi 2.0.
  - Upload's worst case, 87,364 identical function types, needs **24.4 fuel/B on soroban-wasmi and
    30.2 on wasmi 2.0**, against its 20. Validation alone is 14.2 of that.
  - The fixed part of load measured about 460–700 fuel warm and batched, against the schedule's
    2,000. A single cold load of the counter guest in `tests/e2e.rs` takes about 5–8 µs more
    (about 2,000–3,200 fuel), so the floor machines must measure cold loads before that price
    comes down.
  - soroban-wasmi translates faster on 22 of the 24 modules measured: 2.1–6.9× on 11 of them,
    1.05–1.9× on the rest. It's 2.5–3.2× slower on two: many `br_table` targets to one label, and
    many distinct i64 constants.
  - The guests validate in 6–21 µs and load in 7–23 µs on soroban-wasmi and 11–55 µs on wasmi 2.0
    (warm and batched; a single cold load in `tests/e2e.rs` takes 1.1–2.5 times that).
- **Instantiation counts only pages and data, and that's both too much and too little.**
  - **Too much:** zeroing a fixed 32-page memory costs 0.11–0.24 ns/fuel (the high end with every
    4 KiB touched). A page past the first takes 250–450 ns against the 5,000 ns its 2,000 fuel
    buys, so the per-page price is 11–20 times what it costs here. With 32 pages it's 65k fuel per
    frame: 98% of a `counter.get`'s fuel and three quarters of a token transfer's. Guest memory
    sizing and the per-page price both need a decision; a 2-page guest would instantiate for about
    5k fuel.
  - **Too little:** nothing is charged for exports, imports, functions, tables or segments.
    - Measured per entity: about 200 ns an export (80 fuel's worth), 85–115 ns an import, 5–8 ns
      a function, 18–42 ns an element segment and 8.3–8.7 ns a data segment.
    - Against `instantiate_fuel` alone, a module with 22,104 exports costs about 1,500 ns/fuel,
      one with 5,040 functions 9–14, and a full table with 1,000 element segments 6–15.
    - Through `call_v1`, whose 20,100 fuel covers the last two (0.8–1.9 ns/fuel), the exports
      case is still **about 190 ns/fuel (76×)**.
    - 1,000 one-byte data segments sit at the line: 2.4–2.5 ns/fuel here, 2.6 in an earlier run.
  - Validation should cap exports and functions far below what the size limit allows, or
    instantiation needs a charge for each.
- **The adversarial 60M-fuel block.** It's a projection; each engine's worst compute block was also
  run end to end.
  - The worst compute pattern projects to **2.5–2.9 times the 150 ms budget** (369–428 ms):
    pointer-chasing loads on soroban-wasmi, many-local calls on wasmi 2.0. End to end it measured
    **2.3–2.4 times** (349–366 ms). wasmi 2.0's projection is within 1% of its measured block;
    soroban-wasmi's overstates it by 23%: its pointer chase is the candidates' noisiest case.
  - Blocks of distinct-code loads are over too: 165 ms on soroban-wasmi (1.1×) and 344 ms on
    wasmi 2.0 (2.3×) in the first run; section 2's rerun implies about 163 ms and 314 ms.
  - The many-exports instantiation pattern projects to **76× over** (not run end to end).
  - So schedule v1's numbers can't stand. Phase 0's schedule has to:
    - raise `load`, `LOAD_PER_BYTE` and upload's per-byte price;
    - price locals on wasmi 2.0;
    - price instantiation per export (and per import, function and segment), or cap them at
      validation.
  - Lowering `CALIBRATED_BLOCK_FUEL` to about 21–26M could stand in for the compute and load
    items, but not for exports: that would take under 1M, below the 10M floor.
- **Cold and warm.** Fuel is identical, asserted over 10 repetitions and every cold sample.
  - Time differs by 0–7% on the guest transactions and `call_v1`. The 1,000-iteration loops swing
    further, but `--quick` times them only 1–2 to a sample and an earlier 9-repetition run flipped
    their signs, so that's noise.
  - A block engine costs microseconds to create.
  - Dropping a loaded one isn't charged. It can take up to about 260 µs for one module with many
    exports or types, and 0.4–0.7 ms for an engine holding 1,000 token codes.
- **Native stack.** The deepest call chain the design allows runs on a **20 KiB** request on every
  engine (release builds): eight programs host → engine → host, each first recursing 1,000 Wasm
  frames (the cap is 1,024; wasmi keeps them off the native stack). macOS rounds that up to a 16 KiB
  page, so the chain needs more than 16 KiB and ran in 32 KiB. The 64 MiB executor thread has at
  least 2,000 times that.
- **What a typical call costs** (guests on a warm engine; the median over 15 runs of
  `tests/e2e.rs`, each the median of 101 calls; indicative only):

  | Call | soroban-wasmi | wasmi 2.0 |
  |---|---:|---:|
  | `counter.get` | 10.0 µs | 9.2 µs |
  | `token.transfer` | 13.9 µs | 11.8 µs |
  | A router's 3-frame call | 36.2 µs | 32.4 µs |

  Instantiation is most of it: in the same test an empty 32-page program takes about 8–9 µs,
  against about 1.0–1.2 µs at one page.

### 4.3 On the floor machines (x86 and aarch64)

S3's first floor run, through `.github/workflows/calibrate-vm.yml`: all three engines, `--quick`,
on GitHub Actions. Full tables in
[`CALIBRATION-x86_64.md`](../../node/vm/CALIBRATION-x86_64.md) and
[`CALIBRATION-aarch64.md`](../../node/vm/CALIBRATION-aarch64.md). x86 here is GitHub's shared
`ubuntu-24.04` runner, **not** the plan's dedicated Fly `performance-4x`, so it's a noisier first
pass; aarch64 is the free `ubuntu-24.04-arm` runner. They confirm the Mac was optimistic: every
overage is larger on the floor, by about 2× on x86 and 3× on aarch64.

| ns/fuel (soroban-wasmi / wasmi 2.0) | Mac | x86_64 | aarch64 |
|---|---:|---:|---:|
| **Pointer-chasing loads, 2 MiB** | 7.1 / 5.2 | 13.7 / 13.4 | 24.4 / 22.2 |
| &nbsp;&nbsp;— multiple of 2.5 ns | 2.9× / 2.1× | 5.5× / 5.4× | **9.8× / 8.9×** |
| In-cache `i32.load`, one address | 1.4 / 0.8 | 4.4 / 1.7 | 3.9 / 1.6 |
| `br_table`, 8 targets | 2.4 / 0.7 | 3.7 / 2.3 | 3.2 / 2.0 |
| `call`, 1,024 locals | 0.5 / 5.7 | 0.9 / 11.3 | 0.4 / 5.1 |

- **Cache-missing loads dominate, on both engines.** A pointer-chase block runs ~5.4–5.5× over the
  150 ms budget on x86 and **~9–10× on aarch64** (1,460 ms soroban-wasmi, 1,335 ms wasmi 2.0). This
  is the single biggest exposure, it's shared, so it bears on the block budget and `load`'s price,
  not on D1.
- **soroban-wasmi loses its plain-compute margin on the floor.** On x86 its in-cache load (4.4),
  8-target `br_table` (3.7) and a local-operand arithmetic chain (3.0) all cross 2.5 ns/fuel, where
  on the Mac only cache-missing loads did. wasmi 2.0 keeps headroom there (in-cache load 1.7,
  arithmetic ≤ 0.6). Its own weak case, 1,024-local calls, is worst on x86 (4.5×).
- **LOAD_PER_BYTE roughly doubles:** worst implied value soroban-wasmi 10.8 → 23, wasmi 2.0 21 →
  39–40, against the schedule's 10. Upload's worst, where validation (engine-independent) rises
  from 14 fuel/B on the Mac to 33–35 on the floor, is 55–58 fuel/B (soroban-wasmi) and 72–75
  (wasmi 2.0), against the schedule's 20.
- **Instantiation by export is 114× (aarch64) to 175× (x86)** over budget, up from 76×. Capping
  exports, or pricing per entity, is unavoidable.
- **The block budget can't be rescued by lowering `CALIBRATED_BLOCK_FUEL` alone.** To fit a
  pointer-chase block at 60M × ns/fuel, block fuel would have to fall to about 11M (x86) or **6M
  (aarch64)** — below `program_max_block_fuel`'s 10M floor on aarch64. So `load` has to be priced
  for cache misses, not the budget lowered.

These are `--quick` on shared/free runners, so treat the exact ns/fuel as indicative; the
direction (every Mac overage is 2–3× larger on the floor, and the dominant cost is cache-missing
loads shared by both engines) is unambiguous.

## 5. Still to do

- **S3 under schedule v0:** re-run the bench on both floors (`.github/workflows/calibrate-vm.yml`,
  dispatch only) and confirm every path stays under 2.5 ns per fuel; then a dedicated x86 run
  (the plan's Fly `performance-4x`, not GitHub's shared runner) and full rather than `--quick`
  runs for the noisy cases.
  - S3 also asks for what the crate can't measure yet: ML-DSA verification and the host functions
    beyond ABI v1's current subset (`storage_next`, `resolve_name`, the beacon, crypto), the native
    stack on P3's executor thread, single cold loads (4.2) and the node's own storage costs (C6).
- **S4 (started):** cargo-fuzz targets for `validate()` and `execute()` (`program_validate` and
  `program_exec`, C1 and C8) are in `node/vm/fuzz`, building and running clean under
  AddressSanitizer locally, their seeds checked on every pull request. Still owed: the two-week
  sustained run on the floor machines, the engine's own suites and the Wasm spec suite under
  `overflow-checks`, and a wasm-smith `program_exec` over arbitrary validated modules.
- **Done since the prototype** (section 7): node faults kept apart from receipts; exports capped
  and instantiation priced by entity; schedule v0; tests for an upload refused by validation; the
  guests' reproducibility on Linux.
- **PROGRAMS.md:** the corrections in 3.4, with 8.2 and 10.2 for the ABI and 4.3 for the load rule.
- **Still to decide:** guest memory sizing (a 32-page frame is most of a small call's fuel) and
  `last_return`'s missing capacity.

Phase 0's exit also needs P2–P6 merged, and P0–P7 running on testnet-2 (PROGRAMS-PLAN.md 5.2's exit;
the P items are in 5.1).

## 6. The decision (D1): soroban-wasmi 29.0.0

The default held through the floor run. Why:
- **Robustness and review.** soroban-wasmi came through phase 0's tests and fuzzing clean (3.1),
  and its line is the one SRLabs audited. wasmi 2.0 has never been audited, so the plan would
  take it only for large measured advantages (PROGRAMS-PLAN.md section 3), and the measurements
  below don't show them. This is the strongest reason and the floor run
  doesn't touch it.
- **Cost model.** The dominant cost on the floor is cache-missing loads, and it's **shared**:
  soroban-wasmi 9.8× and wasmi 2.0 8.9× on aarch64 (4.3). So compute doesn't separate the engines
  the way the Mac suggested.
  - A higher `load` cost fixes the top exposure on both; neither engine is spared.
  - Where they differ: soroban-wasmi charges for locals; wasmi 2.0 doesn't, and its per-operator
    table can't express a per-local cost (3.2), so on the floor its 1,024-local calls are 4.5× on
    x86. Against that, soroban-wasmi **loses its plain-compute margin on x86**: in-cache loads,
    `br_table` and local-operand arithmetic all cross 2.5 there (4.3), where wasmi 2.0 keeps
    headroom. Schedule v0 prices soroban-wasmi's classes for that.
- **Load.** soroban-wasmi keeps a real, floor-confirmed edge. It translates faster on 22 of 24
  modules, and its worst implied `LOAD_PER_BYTE` stays about half of wasmi 2.0's on every machine
  (floor: 23 against 39–40; 4.3). Upload's worst is 55–58 fuel/B against 72–75, where the shared
  validation term is 33–35.
  - Load dominates only a block whose calls declare different code (PROGRAMS.md 5.3). A block of
    calls to one program is bound by execution fuel, mostly instantiation at 32 pages.
- **What wasmi 2.0 would have offered:** more plain-compute headroom on x86. It only pays once
  loads, locals and instantiation are priced, it doesn't help the shared top cost, and it came
  with roughly double the load cost and an unaudited code base, against soroban-wasmi's audited
  lineage.
- **Production.** soroban-wasmi's fork line runs in production on Stellar; whether 29.0.0 itself
  is live there isn't established.
  - Its README says other projects shouldn't use it, that it may be abandoned, and that Stellar's
    bug bounty covers only stellar-core's configuration, which Blockula's isn't.
  - So the pin (`=29.0.0`) is watched against Stellar's releases, and Blockula is ready to carry
    fixes itself. A bump is a consensus change, made with a protocol version (PROGRAMS.md 7.6).

## 7. What main's crate has (`node/vm`)

The prototype moved onto main as the production crate, soroban-wasmi only, with these changes
(`node/vm/README.md` documents the crate):
- **Fuel schedule v0** (`meter::SCHEDULE_V0`, versioned so later protocol versions add their
  own): operator classes base 3, entity 4, load 20, store 4, call 30; load 10,000 + 40 per byte;
  upload 50,000 + 71 per byte of code and interface (the most that keeps a maximum upload within
  a transaction's 20M); instantiation 2,000 + 300 per import, 500 per export, 15 per function, 20
  per global, 50 per element segment, 2 per table element, 20 per data segment, 2,000 per page
  and 1 per data byte; `abort_v1` +1,000; `debug_log_v1` priced as `emit_v1`. Every item but
  upload is 1.5–3× the worst floor figure in 4.3 (`meter.rs` gives every basis); upload's 71 per
  byte is ~1.22× its worst (58), the schedule's thinnest margin, so the calibration bench reports
  upload as a go/no-go line of its own. A local `--quick` run
  under v0 on the maintainer's Mac puts every path at or below 0.87 ns per fuel (8-target
  `br_table`), and the floor runs remain to be done (section 5).
- **Reservation bounds.** `MAX_TX_CODE_BYTES` is 512 KiB and `MAX_RESERVATION` 2^26: at v0's load
  price a transaction can reserve at most 20M + 21.2M = 41.2M fuel (`meter.rs` derives it and
  checks it at compile time).
- **At most 64 methods** (`bk_` exports, the interface's limit) per code, and `CodeInfo` counts
  every entity instantiation prices.
- **Node faults apart from receipts:** `exec::execute` returns `Result<Receipt, NodeFault>`;
  receipts carry numbered `FailCode`s and `TrapKind`s only, never an engine's or the host's text,
  and abort codes are `u32`. Uploads likewise: `upload::check` (validation, then the test
  translation) returns a refusal (a numbered validation `Rule`, or a refused translation) apart
  from a node fault, and a panic in either step is a node fault, never a refused upload.
- **An engine per transaction:** the node this release ships makes, loads and drops a fresh
  engine for each program transaction, on that transaction's own thread, rather than one per
  block (PROGRAMS.md 4.3); the crate's API serves either.
- **Method kinds** from the host's view of the interface: `View` frames are read-only, `Init`
  runs only as creation's call, value goes only to payable methods, and every version digest a
  call binds comes from the host.
- **Storage economics:** an entry's capacity is key + value + 96 rounded up to 64; each deposit is
  priced on 6.3's curve at the bytes state holds once the write lands, in execution order, with a
  ceiling; diffs carry each program's storage totals.
