# node/vm calibration (S3)

> **A record of phase 0.** These measurements (here and in `CALIBRATION-x86_64.md`,
> `CALIBRATION-aarch64.md`) were taken on the phase 0 prototype, with the starting schedule v1 and
> three engines side by side. They decided D1 (soroban-wasmi 29.0.0; wasmi 2.0 was dropped) and
> are the basis of fuel schedule v0 (`src/meter.rs`, README.md "Fuel schedule v0"). The bench in
> this directory now measures soroban-wasmi (and wasmi 0.38 with `--features ref038`) under
> schedule v0; re-run it on the floor machines before changing a price.

Phase 0's local measurements for the programs engine (docs/PROGRAMS-PLAN.md S3; docs/PROGRAMS.md
4.5 and 12.2). They size the fuel schedule against the design's **2.5 ns-per-fuel** rule: on the
floor machine no adversarial path may run slower than 2.5 ns per fuel, translation included, and
`program_max_block_fuel` (60M) is set so the worst block fits in 150 ms (60M × 2.5 ns).

**These are development numbers on one Apple M4 Max, never prices.** The schedule is set on the
plan's floor machines, which this Mac is not: an ephemeral Fly `performance-4x` (x86, 4 dedicated
cores, 16 GB) and GitHub's `ubuntu-24.04-arm` runners (aarch64). An M4 Max is faster than both, so
every ns/fuel here is **optimistic**: the floor will be slower, the gaps against 2.5 ns wider.
Re-run this on the floor machines before any number reaches `meter.rs`. Nothing here was run on a
cloud machine, billed, or pushed.

**What produced this file.** One `--quick` run of the bench in this commit, on the spike's code
with all 20 ABI v1 host functions; section 2 was then rerun on its own, after its summary was fixed
to report upload's own worst case and the router guest joined its corpus. The tables under "Run
environment and measurements" are the bench's output; the prose above them is written by hand from
those tables. `--quick` medians move between runs: soroban-wasmi's pointer chase across 2 MiB, the
candidates' noisiest case, read 5.9 ns/fuel in an earlier run and 7.1 in this one.

## Running it

```
cargo bench --locked --bench calibrate                              # all sections, soroban-wasmi
cargo bench --locked --features ref038 --bench calibrate            # with wasmi 0.38 alongside, for comparison
cargo bench --locked --bench calibrate -- --quick                   # 3 reps (what produced this file)
cargo bench --locked --bench calibrate -- --only ops,host --out /tmp/run.md
```

`--only <sections>` runs a subset (`ops`, `translate`, `instantiate`, `host`, `block`, `warm`,
`stack`); `--out <file>` writes the bench's Markdown (the tables below, without this prose);
`--quick` trades repetitions for speed. The source is `benches/calibrate.rs` (plain
`std::time::Instant`, no new dependency: `harness = false`).

## Method

- Sections 1–6 run on a thread with the executor's **64 MiB stack** (12.2), through the real
  `exec::execute` and the real host over `testing::MemHost` (sections 1 and 3–6), or through
  `BlockEngine::load` / `invoke` directly (sections 2 and 3, section 5's load rows and section 6's
  engine cost). Section 7 runs each scenario through `exec::execute` in a child process, on a
  thread whose stack it bisects. `MemHost` is a `BTreeMap`, so storage costs are the overlay's and
  the host's, **not** redb's or the state tree's hashing — those are commit-time and out of this
  spike.
- Each figure is the **median** of N repetitions (`--quick`: 3, plus 1 warmup discarded), with
  **[min–max]** alongside as the spread (sections 3, 4 and 6 print medians only). A sample is a
  batch of runs long enough for the clock (≥ 2 ms); inputs are built outside the clock and outputs
  dropped after it (`black_box`). Loop costs are per-iteration differences (N iterations minus 0),
  so instantiation and the transaction's fixed cost cancel.
- **ns per fuel** divides a measured time by the fuel the receipt reports for the same work, under
  each engine's mapping of schedule v1 (`meter::V1`). wasmi 0.38 fuels its own register IR, not
  Wasm operators, so its fuel (and ns/fuel) is not comparable with the other two — it is a
  reference engine only (`--features ref038`), and where its per-op fuel is near zero its ns/fuel
  figure is a small-denominator artifact (e.g. `br_if not taken`, 31 ns/fuel at 0.06 fuel/unit).
- The adversarial block (section 5) is a **projection**: a block of one pattern costs `60M × that
  pattern's ns/fuel`, exactly as the design sets the budget. It is checked against one real
  end-to-end 60M-fuel block of each engine's worst compute pattern. They agree on wasmi 2.0 (366 ms
  measured, 369 projected) and roughly on wasmi 0.38 (−8%). On soroban-wasmi the measured block is
  349 ms against 428 projected (−18%): its pointer chase is the candidates' noisiest case, and the
  end-to-end block implies 5.8 ns/fuel against section 1's 7.1.
- The native stack (section 7) is bisected in 4 KiB steps of a thread's requested stack from
  16 KiB. macOS rounds a request up to its 16 KiB page, so "20 KiB" means the scenario needs more
  than 16 KiB and ran in 32 KiB.

## Key findings

**The 2.5 ns-per-fuel rule holds for ordinary compute and every host function. Four cases break
it, even on this fast Mac, so the floor machines will be worse.**

1. **Cache-missing loads are underpriced on every engine.** `load` costs 1 fuel (2.5 ns), but a
   pointer chase across 2 MiB misses the cache on every step: **7.1 (soroban-wasmi), 5.2 (wasmi
   2.0), 8.1 (wasmi 0.38) ns/fuel**, 2.1–3.2×. soroban-wasmi's figure is the candidates' noisiest
   (6.2–7.5 here, 5.9 in an earlier run). Same-address loads, a pointer chase within 64 KiB and
   independent random loads (which overlap their misses) stay priced on both candidates (≤ 1.6
   ns/fuel); on wasmi 0.38 the 64 KiB chase is over too in this run (3.8; 2.0 in the earlier one). A
   block of pointer-chasing projects to 428 ms on soroban-wasmi (2.9× the 150 ms budget) and
   measured 349 ms end to end (2.3×); on wasmi 2.0 it projects to about 310 ms (2.1×). `load` needs
   a higher cost, or the block budget must assume cache misses.

2. **wasmi 2.0 underprices functions with many locals.** It zero-fills locals for free (a `call`
   with 1,024 locals is 11.6 fuel), while soroban-wasmi charges 1 fuel per 8 locals (141 fuel for
   the same call). `call`, `call_indirect` and recursion with 1,024-local frames run at **5.7–6.2
   ns/fuel on wasmi 2.0 (2.3–2.5×)**, against 0.5–0.7 on soroban-wasmi: a **9–12× engine gap**.
   256 locals is 1.7 (69% of the budget); by interpolation a plain call crosses 2.5 ns/fuel at
   about 400 locals. This answers 4.5's open question: on wasmi 2.0 the schedule needs a per-local
   cost, and since locals are zeroed on Wasm-internal calls too, it has to go into `call` and
   `call_indirect`'s own cost (bounded by the 1,024-local cap), not into a per-frame charge.
   soroban-wasmi already charges it. A block of local-heavy calls on wasmi 2.0 projects to
   363–369 ms and measured 366 ms end to end (about 2.4×).

3. **Instantiation ignores everything but memory and data.** `instantiate_fuel` = 1,000 + 2,000
   per page + 1 per data byte.
   - Memory zeroing is generously covered: 32 pages are 65k fuel, measured at 0.11–0.24 ns/fuel
     (0.11 through `call_v1`, 0.14–0.15 for a top-level transaction, 0.18–0.24 touching every
     4 KiB). A page past the first costs about 250–320 ns untouched and 320–450 ns touched,
     against the 5,000 ns its 2,000 fuel buys: 11–20× generous. Data bytes are covered too
     (0.02–0.12).
   - It counts **no exports, imports, functions, tables or segments**, each of which the engine
     builds into the store per instantiation. Per entity: about 200 ns an export (80 fuel at
     2.5 ns), 85–115 ns an import, 5–8 ns a function, 18–42 ns an element segment (with its
     4,096-entry table), and 8.3–8.7 ns a data segment (against the 1 fuel its byte pays).
   - **22,104 method exports:** about 1,500 ns per fuel of `instantiate_fuel` (which charges
     3,000). Repeated through `call_v1`, whose 20,100 fuel is charged as well, it's still **about
     190 ns/fuel (76×)**: about 4.4 ms an instantiation, charged about 23k fuel. A block of such
     calls projects to about 11.4 s.
   - **5,040 functions:** 9–14 ns per fuel of `instantiate_fuel` (3.7–5.5×), but 1.2–1.8 through
     `call_v1`, whose charge covers it (a block projects to 73–109 ms).
   - **A 4,096-entry table with 1,000 element segments:** 6.4 (soroban-wasmi) to 14.5 (wasmi 2.0)
     ns per fuel of `instantiate_fuel`; 0.8–1.9 through `call_v1`.
   - **1,000 one-byte data segments:** 2.4–2.5 ns/fuel, at the line with no headroom (2.55–2.60
     in an earlier run); 0.4–0.65 through `call_v1`.
   - **Everything at once** (12,265 exports, 2,001 functions, the table, 1,000 data segments and
     more): 37 ns/fuel for a transaction and 28–29 through `call_v1`, so a block of it projects to
     about 1.7 s. The exports are almost all of it.
   Instantiation needs per-export, per-import, per-function, per-table-entry and per-segment terms,
   or validation must cap exports and functions far below the size limit.

4. **LOAD_PER_BYTE (10) underprices translation.** On wasmi 2.0 the worst shape (6,094 functions ×
   1,024 locals, 262 KB) is **52.4 ns/B, an implied LOAD_PER_BYTE of 20.9**, and its two
   function-type shapes imply 16.1 (87,364 identical) and 13.1 (14,980 distinct). On soroban-wasmi
   the worst is 14,980 distinct function types (223 KB): 27.1 ns/B, **10.8**, and 87,364 identical
   types imply 10.3. wasmi 0.38 stays under 10 (9.4 at most). The measured LOAD_BASE (about 460–700
   fuel, warm and batched) is well under the schedule's 2,000, so the per-byte term is the exposure;
   but a single cold load of the counter guest in `tests/e2e.rs` takes about 5–8 µs more than the
   warm figure (about 2,000–3,200 fuel), so the floor machines must measure cold loads before
   LOAD_BASE comes down. Upload (50,000 + 20/B) covers validation and translation on the guests, but
   its worst shape, 87,364 identical function types, needs 24.4 (soroban-wasmi), 30.2 (wasmi 2.0)
   and 23.6 (wasmi 0.38) fuel/B, of which validation alone is 14.2. `LOAD_PER_BYTE` must be the
   measured worst-case translation ÷ 2.5 ns (the design says "never less"): at least about 11 on
   soroban-wasmi and 21–23 on wasmi 2.0 for this corpus (section 2's rerun gives 20.9; section 5's
   load row, from the full run, implies about 23). A block of distinct worst-shape loads projects to
   165 ms on soroban-wasmi (1.1×) and 344 ms on wasmi 2.0 (2.3×) in section 5; section 2's rerun
   implies about 163 ms and 314 ms.

**The engines differ by more than 2× on most compute cases (33 of 59) and about half the
translation corpus (13 of 24), and little on instantiation and host calls (D1):**
- **wasmi 2.0 spends less time per fuel on plain compute:** a median of about 3.4× less over 30
  arithmetic, compare, select, locals, globals and store cases (1.2–1.9× on division and bit
  counting, 2.5–19× on arithmetic and compare chains), 3.0–4.8× less on `br_table` and
  1.0–1.9× less on control flow. It spends 0.04–1.2 ns/fuel there, where soroban-wasmi spends
  0.6–1.6 (2.4 on an 8-target `br_table`). So at a shared schedule, wasmi 2.0 leaves more headroom
  under 2.5 ns on ordinary code.
- **It reverses on calls with many locals** (finding 2): soroban-wasmi is 1.8× cheaper per fuel at
  64 locals, 3.4× at 256 and 9–12× at 1,024, because it meters them. On cache-missing loads
  soroban-wasmi is 1.4× slower in this run (7.1 against 5.2).
- **soroban-wasmi translates faster** on 22 of the 24 modules (2.1–6.9× on 11, 1.05–1.9× on the
  rest; 1.5–2.4× on the guests) and is 2.5–3.2× slower on two (many `br_table` targets to one
  label, many distinct i64 constants). In a block engine it loads a token guest about 2.1×
  cheaper (18–20 µs against 39–40 µs). Repeated 32-page instantiation costs the same on every
  engine (0.11 ns/fuel through `call_v1`).

**What is safely priced:** every host function at schedule v1 (all 20: at most 1.2 ns/fuel on
soroban-wasmi and 1.7 on wasmi 2.0, both `abort_v1`, which runs once a transaction; repeatable
calls at most 0.35; most cases ≤ 0.3), memory zeroing and data bytes at instantiation (not
per-data-segment overhead, which is at the line), and sequential compute. `storage_*`, `transfer`,
`emit` and `call` are generously priced here, but `MemHost` isn't the node's state: the real
storage and commit cost (redb rows, tree hashing) is unmeasured, and C6 must add it.

**Cold and warm fuel are equal.** Fuel is identical cold and warm for every scenario (asserted over
10 warm repetitions and every cold sample). Time differs by 0–7% on the guest transactions and
`call_v1`, cold the slower. The 1,000-iteration loop rows swing from −20% to +60%; three 2 ms
samples (2–4 runs each of the call and pointer-chase loops, 8–20 of the division loop) can't resolve
them (an earlier 9-repetition run of this section moved the swings between cells and flipped their
signs). Creating a block engine is cheap (about 2.7 µs on soroban-wasmi, 1.5 µs on wasmi 2.0, 1.4 µs
on wasmi 0.38; about 1 µs to drop empty), but **dropping a loaded engine is unpriced**: up to about
260 µs for one module (soroban-wasmi, 14,980 distinct function types; 22,105 exports take 210–220 µs
on every engine), 0.4–7 µs per 256 KB straight-line module (about 7 only on soroban-wasmi holding
ten), and 0.4–0.7 ms for an engine holding 1,000 token codes.

**Native stack is a non-issue.** The deepest call chain the design allows (8 programs host → engine
→ host), each first recursing 1,000 Wasm frames (the cap is 1,024), runs on a 20 KiB request on
every engine. macOS rounds that up to its 16 KiB page, so the chain needs more than 16 KiB and ran
in 32 KiB. wasmi keeps Wasm frames off the native stack, so recursion doesn't move the number. The
planned 64 MiB executor thread has at least 2,000 times that; `MAX_CALL_DEPTH` 8 and
`MAX_RECURSION_DEPTH` 1,024 are nowhere near a native overflow.

---

## Run environment and measurements

- Machine: Apple M4 Max (aarch64, macos); development numbers, never prices
- rustc: rustc 1.98.1 (48a229cea 2026-09-01)
- Build: `release/bench` profile, overflow checks on, debug assertions off
- Engines: soroban-wasmi 29.0.0, wasmi 2.0.0, wasmi 0.38.0
- Repetitions: 3 (+1 warmup), samples ≥ 2 ms, loop runs ≈ 10 ms

## 1. ns per fuel by operator class

Each case is a loop whose body is `k` copies of one unit dominated by one class (plus the loop's own ~9 fuel: `br_if`, a decrement, `br`, and wasmi 2.0's hidden 1 per loop entry). ns per fuel = (time of N iterations − time of 0) / N / (fuel per iteration), so instantiation and the transaction's fixed cost cancel. Fuel per unit = fuel per iteration / k, under each engine's mapping of schedule v1 (wasmi 0.38: its own IR fuel). Cells: **ns/fuel median [min–max]** (fuel per unit). Values above 2.5 ns/fuel are marked **bold**.

| class | case | soroban-wasmi | wasmi 2.0 | wasmi 0.38 |
|---|---:|---:|---:|---:|
| control | empty loop (the loop's own overhead) | 0.60 [0.59–0.60] (9.00) | 0.32 [0.30–0.35] (9.00) | 1.50 [1.11–1.50] (4.00) |
| control | br to the next block end: (block (br 0)) | 1.63 [1.63–1.70] (1.14) | 1.06 [1.01–1.06] (1.14) | 2.27 [2.21–2.29] (1.06) |
| control | br_if not taken | 1.41 [1.37–1.43] (2.14) | 1.20 [1.19–1.21] (2.14) | **30.6 [18.6–31.1] (0.06)** |
| control | br_if to the function's end, not taken | 1.25 [1.23–1.25] (1.14) | 1.24 [1.19–1.27] (2.14) | 1.78 [1.70–1.81] (1.06) |
| control | empty loops: (loop) | 0.60 [0.59–0.60] (1.14) | 0.53 [0.52–0.53] (1.14) | 1.91 [1.90–1.92] (1.06) |
| control | if/else, unpredictable | 1.08 [1.04–1.10] (13.6) | 0.65 [0.65–0.66] (12.6) | 1.97 [1.96–2.01] (6.25) |
| br_table | br_table 8 targets, fixed index | 2.39 [2.39–2.49] (2.56) | 0.71 [0.70–0.71] (2.56) | **6.79 [6.77–6.82] (1.25)** |
| br_table | br_table 64 targets, random index | 1.27 [1.21–1.28] (13.2) | 0.42 [0.42–0.43] (13.2) | **2.69 [2.65–2.92] (6.00)** |
| br_table | br_table 1,024 targets, random index | 1.07 [1.06–1.09] (20.0) | 0.34 [0.34–0.35] (20.0) | 2.22 [2.20–2.30] (9.00) |
| br_table | br_table 4,096 targets, random index | 1.18 [1.17–1.20] (20.0) | 0.25 [0.25–0.25] (20.0) | 2.20 [2.07–2.29] (9.00) |
| br_table | br_table 4,096 entries to one label, random index | 1.15 [1.10–1.28] (20.0) | 0.24 [0.23–0.24] (20.0) | 1.92 [1.91–1.94] (9.00) |
| call | call () -> (), no locals | 0.39 [0.39–0.40] (12.6) | 0.32 [0.31–0.33] (11.6) | 2.12 [2.10–2.12] (3.25) |
| call | call, 8 locals | 0.48 [0.48–0.48] (13.6) | 0.51 [0.51–0.51] (11.6) | 1.86 [1.85–1.86] (4.25) |
| call | call, 64 locals | 0.50 [0.50–0.53] (20.6) | 0.88 [0.87–0.89] (11.6) | 1.02 [1.00–1.03] (11.2) |
| call | call, 256 locals | 0.52 [0.51–0.52] (44.6) | 1.73 [1.66–1.74] (11.6) | 0.65 [0.65–0.66] (35.2) |
| call | call, 1,024 locals | 0.54 [0.50–0.54] (141) | **5.70 [5.67–6.18] (11.6)** | 0.55 [0.54–0.59] (131) |
| call | call, 32 params | 0.48 [0.47–0.49] (49.1) | 0.36 [0.36–0.36] (44.1) | 2.28 [2.27–2.30] (7.50) |
| call | call, 32 params + 992 locals | 0.55 [0.51–0.65] (173) | 1.98 [1.94–2.23] (44.1) | 0.64 [0.60–0.69] (132) |
| call | call i64 -> i64 | 0.47 [0.46–0.47] (17.6) | 0.30 [0.30–0.30] (16.6) | 2.44 [2.44–2.44] (4.25) |
| call | recursion 64 deep, 1,024 locals a frame | 0.70 [0.69–0.71] (9,496) | **6.05 [6.04–6.06] (1,111)** | 0.83 [0.80–0.85] (8,647) |
| call | call_indirect, fixed index | 0.71 [0.61–0.78] (13.6) | 0.50 [0.49–0.50] (12.6) | **4.86 [4.83–5.25] (3.25)** |
| call | call_indirect, random index of 4,096 | 0.73 [0.70–0.80] (23.1) | 0.35 [0.35–0.36] (22.1) | **3.00 [2.99–3.05] (7.50)** |
| call | call_indirect, 1,024 locals | 0.51 [0.51–0.52] (142) | **6.15 [5.97–6.26] (12.6)** | 0.73 [0.71–0.73] (131) |
| i32 arith | i32 unary chain (clz ctz popcnt eqz) | 1.54 [1.53–1.54] (4.69) | 1.01 [0.99–1.02] (4.69) | 1.98 [1.98–1.98] (4.25) |
| i64 arith | i64 unary chain (clz ctz popcnt) | 1.62 [1.60–1.81] (3.69) | 1.10 [1.09–1.11] (3.69) | 1.96 [1.96–1.96] (3.25) |
| i32 arith | i32 binary chain, local operand | 0.76 [0.76–0.81] (24.8) | 0.16 [0.16–0.16] (24.8) | 1.69 [1.62–1.87] (12.0) |
| i64 arith | i64 binary chain, local operand | 0.81 [0.80–0.83] (24.8) | 0.16 [0.16–0.17] (24.8) | 1.72 [1.64–1.75] (12.0) |
| i32 arith | i32 binary chain, constant operand | 0.86 [0.85–0.86] (24.8) | 0.17 [0.17–0.17] (24.8) | 1.81 [1.75–1.83] (12.0) |
| i32 arith | i32 3-address: a = b op c | 0.89 [0.87–1.01] (8.56) | 0.12 [0.12–0.12] (8.56) | 1.64 [1.63–2.03] (2.25) |
| i64 arith | i64.mul chain | 0.78 [0.77–0.80] (2.17) | 0.31 [0.31–0.31] (2.17) | 2.00 [1.99–2.00] (1.06) |
| div/rem | i32.div_u by 1, chain | 1.35 [1.35–1.36] (2.17) | 0.73 [0.73–0.74] (2.17) | 1.87 [1.85–2.13] (1.06) |
| div/rem | i32.div_s by 1, chain | 0.89 [0.88–0.90] (2.17) | 0.73 [0.73–0.73] (2.17) | 2.07 [2.06–2.09] (1.06) |
| div/rem | i32.rem_u, xor chain | 1.01 [0.83–1.04] (4.34) | 0.74 [0.73–0.75] (4.34) | 2.01 [1.97–2.03] (2.12) |
| div/rem | i64.div_u by 1, chain | 0.95 [0.89–1.13] (2.17) | 0.73 [0.73–0.73] (2.17) | 2.22 [2.17–2.22] (1.06) |
| div/rem | i64.div_s by 1, chain | 0.98 [0.98–0.98] (2.17) | 0.73 [0.72–0.73] (2.17) | 2.07 [2.03–2.15] (1.06) |
| div/rem | i64.div_u by 3, xor chain | 0.83 [0.82–0.83] (4.34) | 0.52 [0.52–0.52] (4.34) | 1.99 [1.73–2.15] (2.12) |
| div/rem | i64.rem_u, xor chain | 1.13 [1.06–1.14] (4.34) | 0.74 [0.73–0.82] (4.34) | 1.84 [1.79–2.01] (2.12) |
| div/rem | i64.rem_s, xor chain | 0.94 [0.89–0.95] (4.34) | 0.64 [0.63–0.66] (4.34) | 2.09 [2.07–2.14] (2.12) |
| compare | i32 compare chain | 0.88 [0.88–0.89] (8.69) | 0.25 [0.25–0.25] (8.69) | 1.97 [1.67–2.00] (4.25) |
| compare | i64 compare chain | 0.84 [0.83–0.86] (6.69) | 0.15 [0.15–0.15] (6.69) | 2.02 [2.00–2.04] (2.25) |
| compare | i32.eqz chain | 0.79 [0.77–0.80] (1.17) | 0.04 [0.04–0.04] (1.17) | 1.96 [1.95–1.97] (1.06) |
| compare | wrap/extend chain | 1.03 [0.94–1.04] (4.69) | 0.19 [0.18–0.19] (4.69) | 1.85 [1.65–1.93] (3.25) |
| select | select chain | 0.57 [0.56–0.57] (3.34) | 0.13 [0.12–0.13] (3.34) | 2.09 [2.03–2.09] (1.12) |
| select | select, unpredictable condition | 0.71 [0.71–0.72] (12.6) | 0.21 [0.21–0.21] (12.6) | 2.01 [2.00–2.02] (4.25) |
| locals | local.get/local.set copies | 0.75 [0.74–0.75] (6.56) | 0.24 [0.24–0.24] (6.56) | 1.92 [1.91–1.92] (2.25) |
| locals | local.tee chain | 0.76 [0.76–0.77] (4.69) | 0.48 [0.47–0.48] (4.69) | 1.00 [0.90–1.01] (2.31) |
| locals | far locals (1,024 declared) | 0.75 [0.75–0.76] (4.56) | 0.18 [0.18–0.18] (4.56) | 1.89 [1.89–1.90] (2.25) |
| globals | global.get/global.set copies | 1.38 [1.38–1.39] (4.56) | 0.32 [0.32–0.33] (4.56) | 2.29 [2.29–2.30] (4.25) |
| globals | global increment | 0.79 [0.78–0.82] (4.56) | 0.23 [0.23–0.23] (4.56) | 2.11 [2.06–2.14] (3.25) |
| globals | global.get; drop | 0.72 [0.70–0.73] (2.28) | 0.18 [0.18–0.19] (2.28) | 2.35 [2.32–2.38] (1.12) |
| load | i32.load chain, one address | 1.41 [1.41–1.44] (1.17) | 0.77 [0.69–0.80] (1.17) | 2.08 [2.07–2.09] (1.06) |
| load | narrow loads chain, one address | 1.34 [1.31–1.40] (5.69) | 0.50 [0.50–0.53] (5.69) | 1.96 [1.91–2.01] (5.25) |
| load | pointer chase, 64 KiB, random | 1.60 [1.58–1.62] (1.17) | 0.97 [0.96–0.98] (1.17) | **3.82 [3.35–3.87] (1.06)** |
| load | pointer chase, 2 MiB, random | **7.13 [6.21–7.47] (1.17)** | **5.16 [5.11–5.49] (1.17)** | **8.10 [7.67–8.47] (1.06)** |
| load | random independent i64 loads, 2 MiB | 0.82 [0.81–0.85] (10.6) | 0.26 [0.26–0.26] (10.6) | 1.83 [1.83–1.97] (4.25) |
| store | i64.store, one address | 0.71 [0.70–0.71] (3.28) | 0.19 [0.18–0.19] (3.28) | 1.88 [1.88–1.89] (1.12) |
| store | narrow stores, one address | 0.72 [0.72–0.72] (9.56) | 0.18 [0.18–0.18] (9.56) | 1.96 [1.93–1.96] (3.25) |
| store | random i64 stores, 2 MiB | 0.82 [0.81–0.82] (10.6) | 0.30 [0.30–0.30] (10.6) | 1.82 [1.79–2.11] (4.25) |
| memory | memory.size; drop | 0.81 [0.79–0.89] (2.28) | 0.24 [0.24–0.25] (2.28) | 2.15 [1.82–2.16] (1.12) |

**Worst cases per engine** (median ns/fuel; the five highest):

| engine | case | ns/fuel | fuel/unit | vs 2.5 ns |
|---|---:|---:|---:|---:|
| soroban-wasmi | pointer chase, 2 MiB, random | 7.13 [6.21–7.47] | 1.17 | 2.9× |
|  | br_table 8 targets, fixed index | 2.39 [2.39–2.49] | 2.56 | 1.0× |
|  | br to the next block end: (block (br 0)) | 1.63 [1.63–1.70] | 1.14 | 0.7× |
|  | i64 unary chain (clz ctz popcnt) | 1.62 [1.60–1.81] | 3.69 | 0.6× |
|  | pointer chase, 64 KiB, random | 1.60 [1.58–1.62] | 1.17 | 0.6× |
| wasmi 2.0 | call_indirect, 1,024 locals | 6.15 [5.97–6.26] | 12.6 | 2.5× |
|  | recursion 64 deep, 1,024 locals a frame | 6.05 [6.04–6.06] | 1,111 | 2.4× |
|  | call, 1,024 locals | 5.70 [5.67–6.18] | 11.6 | 2.3× |
|  | pointer chase, 2 MiB, random | 5.16 [5.11–5.49] | 1.17 | 2.1× |
|  | call, 32 params + 992 locals | 1.98 [1.94–2.23] | 44.1 | 0.8× |
| wasmi 0.38 | br_if not taken | 30.6 [18.6–31.1] | 0.06 | 12.2× |
|  | pointer chase, 2 MiB, random | 8.10 [7.67–8.47] | 1.06 | 3.2× |
|  | br_table 8 targets, fixed index | 6.79 [6.77–6.82] | 1.25 | 2.7× |
|  | call_indirect, fixed index | 4.86 [4.83–5.25] | 3.25 | 1.9× |
|  | pointer chase, 64 KiB, random | 3.82 [3.35–3.87] | 1.06 | 1.5× |

**soroban-wasmi and wasmi 2.0 more than 2× apart** (ns/fuel, where either is above 0.5):

| case | soroban-wasmi | wasmi 2.0 | ratio |
|---|---:|---:|---:|
| br_table 8 targets, fixed index | 2.39 | 0.71 | 3.4× |
| br_table 64 targets, random index | 1.27 | 0.42 | 3.0× |
| br_table 1,024 targets, random index | 1.07 | 0.34 | 3.1× |
| br_table 4,096 targets, random index | 1.18 | 0.25 | 4.7× |
| br_table 4,096 entries to one label, random index | 1.15 | 0.24 | 4.8× |
| call, 256 locals | 0.52 | 1.73 | 3.4× |
| call, 1,024 locals | 0.54 | 5.70 | 10.6× |
| call, 32 params + 992 locals | 0.55 | 1.98 | 3.6× |
| recursion 64 deep, 1,024 locals a frame | 0.70 | 6.05 | 8.7× |
| call_indirect, random index of 4,096 | 0.73 | 0.35 | 2.1× |
| call_indirect, 1,024 locals | 0.51 | 6.15 | 12.1× |
| i32 binary chain, local operand | 0.76 | 0.16 | 4.8× |
| i64 binary chain, local operand | 0.81 | 0.16 | 4.9× |
| i32 binary chain, constant operand | 0.86 | 0.17 | 5.1× |
| i32 3-address: a = b op c | 0.89 | 0.12 | 7.4× |
| i64.mul chain | 0.78 | 0.31 | 2.5× |
| i32 compare chain | 0.88 | 0.25 | 3.5× |
| i64 compare chain | 0.84 | 0.15 | 5.6× |
| i32.eqz chain | 0.79 | 0.04 | 19.3× |
| wrap/extend chain | 1.03 | 0.19 | 5.5× |
| select chain | 0.57 | 0.13 | 4.5× |
| select, unpredictable condition | 0.71 | 0.21 | 3.4× |
| local.get/local.set copies | 0.75 | 0.24 | 3.1× |
| far locals (1,024 declared) | 0.75 | 0.18 | 4.2× |
| global.get/global.set copies | 1.38 | 0.32 | 4.3× |
| global increment | 0.79 | 0.23 | 3.4× |
| global.get; drop | 0.72 | 0.18 | 3.9× |
| narrow loads chain, one address | 1.34 | 0.50 | 2.7× |
| random independent i64 loads, 2 MiB | 0.82 | 0.26 | 3.1× |
| i64.store, one address | 0.71 | 0.19 | 3.7× |
| narrow stores, one address | 0.72 | 0.18 | 3.9× |
| random i64 stores, 2 MiB | 0.82 | 0.30 | 2.8× |
| memory.size; drop | 0.81 | 0.24 | 3.3× |

## 2. Translation and validation, ns per byte

Cold: each load is `BlockEngine::load` (a structural read of memory and data, then the engine's `Module::new`: its own parse, validation and translation) on a fresh block engine made outside the clock, as a block loads every declared code into a fresh engine (4.3). `validate` is upload validation (`validate.rs`, wasmparser 0.228 plus our rules). CPU caches are warm (the same bytes are loaded repeatedly), so these are lower bounds for a cold machine. *Implied fuel* = time / 2.5 ns. **Implied LOAD_PER_BYTE** = (load time / 2.5 ns − LOAD_BASE 2,000) / bytes, against schedule v1's 10; **implied LOAD_BASE** comes from the smallest modules.

| module | bytes | validate µs (ns/B) | soroban-wasmi load µs (ns/B) | wasmi 2.0 load µs (ns/B) | wasmi 0.38 load µs (ns/B) | soroban-wasmi implied LOAD_PER_BYTE | wasmi 2.0 implied LOAD_PER_BYTE | wasmi 0.38 implied LOAD_PER_BYTE |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| guest counter.wasm | 565 | 6.39 (11.3) | 7.08 (12.5) | 10.9 (19.3) | 8.02 (14.2) | 1.47 | 4.19 | 2.13 |
| guest token.wasm | 2,467 | 16.4 (6.67) | 18.6 (7.55) | 40.2 (16.3) | 27.3 (11.1) | 2.21 | 5.70 | 3.62 |
| guest escrow.wasm | 3,389 | 20.6 (6.08) | 22.7 (6.68) | 55.3 (16.3) | 30.6 (9.03) | 2.08 | 5.94 | 3.02 |
| guest router.wasm | 1,484 | 10.2 (6.84) | 11.2 (7.54) | 21.5 (14.5) | 14.2 (9.57) | 1.67 | 4.44 | 2.48 |
| minimal program | 51 | 1.18 (23.1) | 1.16 (22.7) | 1.76 (34.5) | 1.19 (23.3) | 0.00 | 0.00 | 0.00 |
| minimal program + 20 imports | 621 | 6.73 (10.8) | 6.67 (10.7) | 8.00 (12.9) | 6.68 (10.8) | 1.08 | 1.93 | 1.08 |
| one function, straight-line i64 arithmetic | 262,141 | 721 (2.75) | 625 (2.39) | 1,832 (6.99) | 902 (3.44) | 0.95 | 2.79 | 1.37 |
| 5040 small functions (≈40 B bodies) | 262,137 | 1,532 (5.84) | 1,767 (6.74) | 3,086 (11.8) | 2,145 (8.18) | 2.69 | 4.70 | 3.27 |
| 6094 functions with 1,024 locals each | 262,099 | 1,739 (6.64) | 1,988 (7.58) | 13,732 (52.4) | 2,653 (10.1) | 3.03 | **20.9** | 4.04 |
| br_table, 2 × 131030 targets to one label | 262,143 | 1,362 (5.20) | 4,384 (16.7) | 1,384 (5.28) | 1,446 (5.52) | 6.68 | 2.10 | 2.20 |
| br_table, 12 × 4,096 distinct nested targets | 244,343 | 1,261 (5.16) | 1,896 (7.76) | 2,595 (10.6) | 1,621 (6.63) | 3.10 | 4.24 | 2.65 |
| 87361 nested blocks | 262,142 | 1,607 (6.13) | 1,893 (7.22) | 4,555 (17.4) | 2,482 (9.47) | 2.88 | 6.94 | 3.78 |
| 32760 if/else | 262,143 | 1,516 (5.78) | 1,586 (6.05) | 5,314 (20.3) | 2,296 (8.76) | 2.41 | 8.10 | 3.50 |
| 28942 distinct i64 constants | 258,321 | 416 (1.61) | 1,408 (5.45) | 554 (2.15) | 332 (1.28) | 2.17 | 0.85 | 0.51 |
| 131035 B of data + as much code (32 pages) | 262,143 | 584 (2.23) | 509 (1.94) | 719 (2.74) | 398 (1.52) | 0.77 | 1.09 | 0.60 |
| 14980 distinct function types | 223,362 | 5,883 (26.3) | 6,056 (27.1) | 7,329 (32.8) | 4,234 (19.0) | **10.8** | **13.1** | 7.57 |
| 87364 identical function types | 262,144 | 9,277 (35.4) | 6,744 (25.7) | 10,527 (40.2) | 6,185 (23.6) | **10.3** | **16.1** | 9.43 |
| 22105 exports of one function | 254,196 | 3,622 (14.2) | 3,683 (14.5) | 3,857 (15.2) | 3,737 (14.7) | 5.79 | 6.06 | 5.87 |
| 131019 calls | 262,143 | 1,147 (4.38) | 1,476 (5.63) | 3,027 (11.5) | 1,999 (7.63) | 2.24 | 4.61 | 3.04 |
| 43679 far local.get/set pairs (1,024 locals) | 262,140 | 720 (2.75) | 681 (2.60) | 1,917 (7.31) | 993 (3.79) | 1.03 | 2.92 | 1.51 |
| 21 × (4,000 local.gets, local.set over them, 3,999 adds) | 252,148 | 1,089 (4.32) | 1,371 (5.44) | 4,078 (16.2) | 2,616 (10.4) | 2.17 | 6.46 | 4.14 |
| 16 × (4,000-deep stack, 1,000 local.sets) | 256,064 | 1,057 (4.13) | 918 (3.59) | 2,730 (10.7) | 1,279 (5.00) | 1.43 | 4.26 | 1.99 |
| 43680 dead-code units after unreachable | 262,140 | 1,097 (4.18) | 796 (3.04) | 1,313 (5.01) | 712 (2.71) | 1.21 | 2.00 | 1.08 |
| 256 globals, 52160 global.get/set pairs | 262,144 | 772 (2.94) | 698 (2.66) | 1,899 (7.24) | 918 (3.50) | 1.06 | 2.89 | 1.39 |

Spread, and the drop of an engine holding the module (less an empty engine's drop): part of the block's cost, unpriced.

| module | soroban-wasmi load [min–max] µs | soroban-wasmi drop µs | wasmi 2.0 load [min–max] µs | wasmi 2.0 drop µs | wasmi 0.38 load [min–max] µs | wasmi 0.38 drop µs | validate [min–max] µs |
|---|---:|---:|---:|---:|---:|---:|---:|
| guest counter.wasm | [6.93–7.24] | 0.25 | [10.9–11.2] | 0.60 | [7.92–8.25] | 0.63 | [6.39–6.49] |
| guest token.wasm | [18.5–18.7] | 0.42 | [39.0–40.3] | 1.94 | [27.1–29.4] | 0.78 | [16.4–16.6] |
| guest escrow.wasm | [22.5–22.8] | 0.72 | [55.0–55.4] | 0.84 | [30.0–30.7] | 0.91 | [20.1–20.6] |
| guest router.wasm | [11.1–11.3] | 0.40 | [21.4–21.6] | 0.69 | [14.2–14.5] | 0.73 | [10.1–10.2] |
| minimal program | [1.12–1.25] | 0.18 | [1.72–1.83] | 0.29 | [1.14–1.30] | 0.24 | [1.13–1.27] |
| minimal program + 20 imports | [6.61–6.95] | 0.47 | [7.97–8.13] | 0.66 | [6.54–6.76] | 0.62 | [6.71–6.81] |
| one function, straight-line i64 arithmetic | [621–629] | 0.40 | [1,831–1,969] | 0.87 | [893–905] | 0.75 | [720–761] |
| 5040 small functions (≈40 B bodies) | [1,739–1,780] | 12.2 | [2,946–3,127] | 47.9 | [2,135–2,147] | 39.6 | [1,532–1,555] |
| 6094 functions with 1,024 locals each | [1,972–1,995] | 0.71 | [13,529–13,737] | 59.2 | [2,627–2,714] | 48.3 | [1,738–1,746] |
| br_table, 2 × 131030 targets to one label | [4,338–4,398] | 30.6 | [1,370–1,389] | 0.60 | [1,430–1,446] | 0.51 | [1,351–1,365] |
| br_table, 12 × 4,096 distinct nested targets | [1,893–1,902] | 0.52 | [2,593–2,597] | 1.00 | [1,613–1,669] | 0.78 | [1,256–1,266] |
| 87361 nested blocks | [1,880–1,925] | 0.41 | [4,543–4,588] | 104 | [2,458–2,505] | 52.8 | [1,600–1,614] |
| 32760 if/else | [1,583–1,657] | 12.2 | [5,186–5,379] | 35.7 | [2,286–2,311] | 0.94 | [1,461–1,518] |
| 28942 distinct i64 constants | [1,398–1,409] | 59.8 | [549–559] | 0.74 | [331–333] | 0.57 | [412–421] |
| 131035 B of data + as much code (32 pages) | [509–512] | 0.65 | [717–719] | 0.69 | [396–400] | 0.60 | [583–586] |
| 14980 distinct function types | [6,049–6,069] | 257 | [7,183–7,367] | 84.0 | [4,195–4,294] | 78.2 | [5,866–5,970] |
| 87364 identical function types | [6,681–6,754] | 0.55 | [10,392–10,704] | 0.88 | [6,182–6,217] | 0.83 | [9,248–9,282] |
| 22105 exports of one function | [3,674–3,699] | 217 | [3,839–3,925] | 213 | [3,709–3,738] | 215 | [3,506–3,756] |
| 131019 calls | [1,469–1,487] | 0.47 | [2,984–3,030] | 50.9 | [1,998–2,008] | 0.77 | [1,140–1,148] |
| 43679 far local.get/set pairs (1,024 locals) | [680–683] | 0.47 | [1,850–1,933] | 1.10 | [992–995] | 0.81 | [719–720] |
| 21 × (4,000 local.gets, local.set over them, 3,999 adds) | [1,311–1,396] | 12.1 | [4,075–4,099] | 44.5 | [2,573–2,645] | 1.03 | [1,088–1,096] |
| 16 × (4,000-deep stack, 1,000 local.sets) | [915–926] | 11.9 | [2,717–2,750] | 1.09 | [1,276–1,280] | 0.84 | [1,056–1,060] |
| 43680 dead-code units after unreachable | [794–796] | 0.33 | [1,311–1,317] | 0.59 | [710–713] | 0.38 | [1,096–1,098] |
| 256 globals, 52160 global.get/set pairs | [697–702] | 0.64 | [1,894–1,908] | 1.19 | [900–938] | 1.02 | [771–775] |

**Summary.** Schedule v1: LOAD_BASE 2,000, LOAD_PER_BYTE 10; upload 50,000 + 20 per byte.

| engine | worst ns/B, any size | worst ns/B, modules > 100 KB | implied LOAD_PER_BYTE (worst > 100 KB) | minimal module's load (implied LOAD_BASE) | worst upload (validate + load), modules > 100 KB |
|---|---:|---:|---:|---:|---:|
| soroban-wasmi | 14980 distinct function types (27.1 ns/B) | 14980 distinct function types (27.1 ns/B) | 10.8 | 1.16 µs = 463 fuel | 87364 identical function types (61.1 ns/B = 24.4 fuel/B) |
| wasmi 2.0 | 6094 functions with 1,024 locals each (52.4 ns/B) | 6094 functions with 1,024 locals each (52.4 ns/B) | 20.9 | 1.76 µs = 703 fuel | 87364 identical function types (75.5 ns/B = 30.2 fuel/B) |
| wasmi 0.38 | 87364 identical function types (23.6 ns/B) | 87364 identical function types (23.6 ns/B) | 9.43 | 1.19 µs = 476 fuel | 87364 identical function types (59.0 ns/B = 23.6 fuel/B) |

## 3. Instantiation

Per module, three ways: **raw** (`BlockEngine::invoke` of an empty `bk_noop` with a host that refuses every import: the engine's store, linking, instance and drop), **tx** (a top-level transaction calling `bk_noop` through `execute`: also the executor's fixed cost) and **call_v1** (each call of a loop `call_v1`s `bk_noop` on the module: the repeatable pattern, charged `call` 20,100 + `instantiate_fuel`). *Schedule* = `instantiate_fuel(pages, data)` (1,000 + 2,000 per page + 1 per data byte), which counts no imports, functions, exports, globals, tables or segments. Time ÷ 2.5 ns is the fuel it would need.

| module | bytes | schedule fuel | soroban-wasmi raw µs | soroban-wasmi tx µs (ns/fuel) | wasmi 2.0 raw µs | wasmi 2.0 tx µs (ns/fuel) | wasmi 0.38 raw µs | wasmi 0.38 tx µs (ns/fuel) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 page, no imports | 52 | 3,000 | 0.81 | 1.25 (0.42) | 0.85 | 1.19 (0.40) | 0.87 | 1.32 (0.44) |
| 1 page, 20 imports | 622 | 3,000 | 2.53 | 3.03 (1.01) | 2.84 | 3.31 (1.10) | 3.04 | 3.56 (1.19) |
| 32 pages | 52 | 65,000 | 10.7 | 9.77 (0.15) | 8.72 | 9.22 (0.14) | 9.31 | 9.81 (0.15) |
| 32 pages, 20 imports, + touch every 4 KiB | 667 | 65,000 | 10.8 | 11.3 (0.17) | 11.0 | 11.1 (0.17) | 11.6 | 12.2 (0.19) |
| data: 1 segment of 120,000 B | 240,075 | 125,000 | 2.38 | 2.92 (0.02) | 2.19 | 2.73 (0.02) | 2.48 | 3.00 (0.02) |
| data: 1,000 segments of 1 B | 8,001 | 4,000 | 9.34 | 9.88 (2.47) | 9.41 | 9.92 (2.48) | 9.07 | 9.57 (2.39) |
| data: 1,000 segments of 120 B | 246,999 | 125,000 | 11.8 | 12.5 (0.10) | 13.0 | 13.0 (0.10) | 12.2 | 12.7 (0.10) |
| 256 globals | 1,337 | 3,000 | 2.23 | 2.73 (0.91) | 2.49 | 2.95 (0.98) | 2.18 | 2.66 (0.88) |
| table of 4,096, 1,000 element segments | 10,050 | 3,000 | 18.6 | **19.1 (6.37)** | 42.3 | **43.5 (14.5)** | 38.4 | **39.0 (13.0)** |
| 5040 functions | 262,137 | 3,000 | 26.9 | **27.6 (9.18)** | 41.1 | **41.2 (13.7)** | 29.3 | **30.2 (10.1)** |
| 22104 exports | 254,194 | 3,000 | 4,334 | **4,383 (1,460)** | 4,425 | **4,462 (1,487)** | 4,412 | **4,397 (1,465)** |
| all: 20 imports, 32 pages, 256 globals, table + 1,000 elems, 2,001 funcs, 1,000 data, 12265 exports | 258,924 | 66,000 | 2,439 | **2,434 (36.9)** | 2,475 | **2,458 (37.2)** | 2,447 | **2,454 (37.2)** |

**Repeated instantiation through `call_v1`** (per call: the measured fuel, then time and ns per fuel):

| callee | schedule fuel per call | soroban-wasmi fuel: µs (ns/fuel) | wasmi 2.0 fuel: µs (ns/fuel) | wasmi 0.38 fuel: µs (ns/fuel) |
|---|---:|---:|---:|---:|
| 1 page, no imports | 23,100 | 23,130: 1.09 (0.05) | 23,129: 1.09 (0.05) | 23,107: 1.19 (0.05) |
| 1 page, 20 imports | 23,100 | 23,130: 2.70 (0.12) | 23,129: 3.03 (0.13) | 23,107: 3.46 (0.15) |
| 32 pages | 85,100 | 85,130: 9.54 (0.11) | 85,129: 9.03 (0.11) | 85,107: 9.57 (0.11) |
| 32 pages, 20 imports, + touch every 4 KiB | 85,100 | 85,130: 11.4 (0.13) | 85,129: 11.1 (0.13) | 85,107: 11.9 (0.14) |
| data: 1 segment of 120,000 B | 145,100 | 145,130: 2.74 (0.02) | 145,129: 2.53 (0.02) | 145,107: 2.82 (0.02) |
| data: 1,000 segments of 1 B | 24,100 | 24,130: 9.89 (0.41) | 24,129: 9.91 (0.41) | 24,107: 15.6 (0.65) |
| data: 1,000 segments of 120 B | 145,100 | 145,130: 13.0 (0.09) | 145,129: 13.0 (0.09) | 145,107: 17.9 (0.12) |
| 256 globals | 23,100 | 23,130: 2.60 (0.11) | 23,129: 2.83 (0.12) | 23,107: 2.50 (0.11) |
| table of 4,096, 1,000 element segments | 23,100 | 23,130: 18.6 (0.80) | 23,129: 43.7 (1.89) | 23,107: 39.6 (1.71) |
| 5040 functions | 23,100 | 23,130: 27.7 (1.20) | 23,129: 42.0 (1.82) | 23,107: 29.8 (1.29) |
| 22104 exports | 23,100 | 23,130: **4,579 (198)** | 23,129: **4,398 (190)** | 23,107: **4,395 (190)** |
| all: 20 imports, 32 pages, 256 globals, table + 1,000 elems, 2,001 funcs, 1,000 data, 12265 exports | 86,100 | 86,130: **2,439 (28.3)** | 86,129: **2,485 (28.8)** | 86,107: **2,473 (28.7)** |

**Touching the memory**: a transaction whose method stores to every 4 KiB of its 2 MiB, so the zeroed pages the allocator handed out lazily are faulted in (the cost an instantiation's per-page price must cover):

| module | soroban-wasmi tx fuel: µs (ns/fuel) | wasmi 2.0 tx fuel: µs (ns/fuel) | wasmi 0.38 tx fuel: µs (ns/fuel) |
|---|---:|---:|---:|
| 32 pages, 20 imports, + touch every 4 KiB | 71,146: 17.0 (0.24) (+5.64 µs over the empty tx) | 71,145: 13.1 (0.18) (+1.98 µs over the empty tx) | 67,050: 15.8 (0.24) (+3.56 µs over the empty tx) |

## 4. Host functions (ABI v1)

Each case loops one host function N times inside a program (`bk_<case>`) through the real executor and `MemHost`; per call = (time of N − time of 0) / N, so the call's Wasm side (its arguments, the `call` itself at 10 fuel, the loop's ~9) is included, as a program would pay it. Cases bounded per transaction (events, keys written, bytes written) run that many calls a transaction. Fuel per call is measured (the schedule's charge plus that Wasm side). `MemHost` is a `BTreeMap`, not the node's state: storage costs here are the overlay's and the host's, not redb's or the tree's (those are commit-time).

| function, case | fuel per call | soroban-wasmi ns/call (ns/fuel) | wasmi 2.0 ns/call (ns/fuel) | wasmi 0.38 ns/call (ns/fuel) |
|---|---:|---:|---:|---:|
| `input_len_v1`  | 120 / 105 | 34.1 (0.28) | 32.1 (0.27) | 36.0 (0.34) |
| `input_read_v1` 44 B input | 164 / 149 | 38.9 (0.24) | 37.1 (0.23) | 40.5 (0.27) |
| `input_read_v1` 32 KiB input | 32,888 / 32,873 | 349 (0.01) | 343 (0.01) | 351 (0.01) |
| `return_data_v1` 0 B | 121 / 105 | 41.2 (0.34) | 39.3 (0.33) | 43.4 (0.41) |
| `return_data_v1` 4 KiB | 4,217 / 4,201 | 95.5 (0.02) | 108 (0.03) | 114 (0.03) |
| `debug_log_v1` 0 B | 121 / 105 | 42.7 (0.35) | 38.5 (0.32) | 44.7 (0.43) |
| `debug_log_v1` 64 KiB | 65,657 / 65,641 | 40.0 (0.00) | 37.6 (0.00) | 42.6 (0.00) |
| `abort_v1` 256 B message (one per tx: tx time − an empty tx's) | 343 / 350 | 399 (1.16) | 572 (1.67) | 435 (1.24) |
| `storage_read_v1` absent, 32 B key | 1,156 / 1,137 | 85.2 (0.07) | 80.2 (0.07) | 85.5 (0.08) |
| `storage_read_v1` 32 B key, 8 B value | 1,164 / 1,145 | 107 (0.09) | 105 (0.09) | 111 (0.10) |
| `storage_read_v1` 128 B key, 8 B value | 1,260 / 1,241 | 102 (0.08) | 107 (0.09) | 109 (0.09) |
| `storage_read_v1` 32 B key, 16 KiB value | 17,540 / 17,521 | 427 (0.02) | 420 (0.02) | 431 (0.02) |
| `storage_write_v1` overwrite, 32 B key, 0 B value | 5,444 / 5,425 | 109 (0.02) | 102 (0.02) | 110 (0.02) |
| `storage_write_v1` overwrite, 128 B key, 8 B value | 6,484 / 6,465 | 137 (0.02) | 125 (0.02) | 136 (0.02) |
| `storage_write_v1` overwrite, 32 B key, 16 KiB value (64 a tx) | 169,284 / 169,265 | 516 (0.00) | 513 (0.00) | 540 (0.00) |
| `storage_write_v1` new keys, 32 B key, 8 B value (256 a tx) | 5,527 / 5,506 | 248 (0.04) | 267 (0.05) | 244 (0.04) |
| `storage_remove_v1` absent | 3,122 / 3,105 | 77.8 (0.02) | 74.0 (0.02) | 78.7 (0.03) |
| `storage_remove_v1` present, 16 KiB value (256 a tx) | 3,125 / 3,106 | 781 (0.25) | 777 (0.25) | 789 (0.25) |
| `self_address_v1`  | 320 / 305 | 34.7 (0.11) | 33.4 (0.10) | 37.9 (0.12) |
| `caller_v1`  | 320 / 305 | 35.3 (0.11) | 33.8 (0.11) | 37.5 (0.12) |
| `value_v1`  | 320 / 305 | 33.9 (0.11) | 31.4 (0.10) | 35.5 (0.12) |
| `block_height_v1`  | 320 / 305 | 34.8 (0.11) | 31.8 (0.10) | 36.5 (0.12) |
| `self_balance_v1`  | 320 / 305 | 40.4 (0.13) | 36.1 (0.11) | 41.2 (0.14) |
| `transfer_v1` to an existing account | 10,123 / 10,105 | 58.0 (0.01) | 62.1 (0.01) | 58.6 (0.01) |
| `transfer_v1` opening an account (OPEN_DEDUCT) | 10,126 / 10,106 | 363 (0.04) | 360 (0.04) | 370 (0.04) |
| `call_v1` callee 1 page, empty method | 23,130 / 23,129 / 23,107 | 1,358 (0.06) | 1,270 (0.05) | 1,418 (0.06) |
| `call_v1` callee 1 page, 32 KiB input | 55,898 / 55,897 / 55,875 | 1,682 (0.03) | 1,578 (0.03) | 1,757 (0.03) |
| `call_v1` callee 32 pages, empty method | 85,130 / 85,129 / 85,107 | 9,854 (0.12) | 9,244 (0.11) | 9,905 (0.12) |
| `try_call_v1` callee traps (layer dropped) | 23,130 / 23,107 | 1,365 (0.06) | 1,258 (0.05) | 1,420 (0.06) |
| `last_return_v1` 0 B | 120 / 105 | 36.9 (0.31) | 35.8 (0.30) | 39.9 (0.38) |
| `last_return_v1` 4 KiB | 4,216 / 4,201 | 77.4 (0.02) | 72.3 (0.02) | 78.5 (0.02) |
| `last_return_len_v1`  | 120 / 105 | 35.4 (0.30) | 31.8 (0.26) | 36.2 (0.34) |
| `last_abort_code_v1`  | 120 / 105 | 34.4 (0.29) | 31.8 (0.27) | 37.5 (0.36) |
| `emit_v1` 8 B name, 0 B data (64 a tx) | 2,187 / 2,169 | 86.8 (0.04) | 87.1 (0.04) | 84.8 (0.04) |
| `emit_v1` 32 B name, 16,352 B data (1 a tx) | 133,195 / 133,177 | 455 (0.00) | 599 (0.00) | 427 (0.00) |

**Per byte**, from the two sizes of each function (Δtime / Δbytes, against Δfuel / Δbytes):

| function: sizes | fuel per byte | soroban-wasmi ns/B (ns/fuel) | wasmi 2.0 ns/B (ns/fuel) | wasmi 0.38 ns/B (ns/fuel) |
|---|---:|---:|---:|---:|
| `input_read_v1`: 44 B input → 32 KiB input | 1.00 | 0.01 (0.01) | 0.01 (0.01) | 0.01 (0.01) |
| `return_data_v1`: 0 B → 4 KiB | 1.00 | 0.01 (0.01) | 0.02 (0.02) | 0.02 (0.02) |
| `debug_log_v1`: 0 B → 64 KiB | 1.00 | -0.00 (-0.00) | -0.00 (-0.00) | -0.00 (-0.00) |
| `storage_read_v1`: 32 B key, 8 B value → 32 B key, 16 KiB value | 1.00 | 0.02 (0.02) | 0.02 (0.02) | 0.02 (0.02) |
| `storage_read_v1`: 32 B key, 8 B value → 128 B key, 8 B value | 1.00 | -0.05 (-0.05) | 0.02 (0.02) | -0.02 (-0.02) |
| `storage_write_v1`: overwrite, 32 B key, 0 B value → overwrite, 32 B key, 16 KiB value (64 a tx) | 10.0 | 0.02 (0.00) | 0.03 (0.00) | 0.03 (0.00) |
| `call_v1`: callee 1 page, empty method → callee 1 page, 32 KiB input | 1.00 | 0.01 (0.01) | 0.01 (0.01) | 0.01 (0.01) |
| `last_return_v1`: 0 B → 4 KiB | 1.00 | 0.01 (0.01) | 0.01 (0.01) | 0.01 (0.01) |
| `emit_v1`: 8 B name, 0 B data (64 a tx) → 32 B name, 16,352 B data (1 a tx) | 8.00 | 0.02 (0.00) | 0.03 (0.00) | 0.02 (0.00) |

## 5. An adversarial block

A `max_block_fuel` block is 60,000,000 fuel (3 transactions of `program_max_tx_fuel`, 20,000,000 each). The cost of a block made entirely of one pattern is **60M × the pattern's ns/fuel**: that is how the design sets the budget (60M × 2.5 ns = 150 ms). Each ns/fuel below is measured as in sections 1–4 (a bounded loop, not a literal 60M-fuel run, which would just be the same number × 60M); the projection is then checked against one real end-to-end 60M block per engine at the end. 150 ms is the budget; a block over it is **bold**.

| engine | block of this pattern | ns/fuel [min–max] | projected ms [min–max] | vs 150 ms |
|---|---:|---:|---:|---:|
| soroban-wasmi | compute: pointer chase, 2 MiB, random | 7.13 [6.21–7.47] | 428 [373–448] | **3×** |
| soroban-wasmi | compute: br_table 8 targets, fixed index | 2.39 [2.39–2.49] | 144 [143–149] | 1× |
| wasmi 2.0 | compute: call_indirect, 1,024 locals | 6.15 [5.97–6.26] | 369 [358–376] | **2×** |
| wasmi 2.0 | compute: recursion 64 deep, 1,024 locals a frame | 6.05 [6.04–6.06] | 363 [362–364] | **2×** |
| wasmi 0.38 | compute: br_if not taken | 30.6 [18.6–31.1] | 1836 [1116–1865] | **12×** |
| wasmi 0.38 | compute: pointer chase, 2 MiB, random | 8.10 [7.67–8.47] | 486 [460–508] | **3×** |
| soroban-wasmi | instantiation: call_v1 loop to the many-exports callee | 190 [190–191] | 11407 [11376–11453] | **76×** |
| wasmi 2.0 | instantiation: call_v1 loop to the many-exports callee | 191 [191–193] | 11432 [11431–11563] | **76×** |
| wasmi 0.38 | instantiation: call_v1 loop to the many-exports callee | 191 [190–195] | 11451 [11379–11704] | **76×** |
| soroban-wasmi | instantiation: call_v1 loop to the 5,040-function callee | 1.21 [1.20–1.21] | 73 [72–73] | 0× |
| wasmi 2.0 | instantiation: call_v1 loop to the 5,040-function callee | 1.81 [1.81–1.81] | 109 [109–109] | 1× |
| wasmi 0.38 | instantiation: call_v1 loop to the 5,040-function callee | 1.29 [1.26–1.31] | 77 [76–78] | 1× |
| soroban-wasmi | instantiation: call_v1 loop to a 32-page callee | 0.12 [0.12–0.13] | 7 [7–8] | 0× |
| wasmi 2.0 | instantiation: call_v1 loop to a 32-page callee | 0.11 [0.11–0.11] | 7 [6–7] | 0× |
| wasmi 0.38 | instantiation: call_v1 loop to a 32-page callee | 0.13 [0.12–0.13] | 8 [7–8] | 0× |
| soroban-wasmi | storage_write storm: 64 × 16 KiB overwrites a tx | 0.00 [0.00–0.00] | 0 [0–0] | 0× |
| wasmi 2.0 | storage_write storm: 64 × 16 KiB overwrites a tx | 0.00 [0.00–0.00] | 0 [0–0] | 0× |
| wasmi 0.38 | storage_write storm: 64 × 16 KiB overwrites a tx | 0.00 [0.00–0.00] | 0 [0–0] | 0× |
| soroban-wasmi | load: distinct "14980 distinct function types" × 26 (223,362 B each) | 2.75 [2.74–2.75] | 165 [165–165] | **1×** |
| soroban-wasmi | load: distinct minimal programs × 23904 (51 B each) | 0.49 [0.49–0.49] | 30 [30–30] | 0× |
| wasmi 2.0 | load: distinct "6094 functions with 1,024 locals each" × 22 (262,099 B each) | 5.74 [5.74–5.76] | 344 [344–346] | **2×** |
| wasmi 2.0 | load: distinct minimal programs × 23904 (51 B each) | 0.73 [0.73–0.75] | 44 [44–45] | 0× |
| wasmi 0.38 | load: distinct "87364 identical function types" × 22 (262,144 B each) | 2.37 [2.36–2.39] | 142 [142–144] | 1× |
| wasmi 0.38 | load: distinct minimal programs × 23904 (51 B each) | 0.49 [0.49–0.51] | 29 [29–30] | 0× |

**End-to-end check**: one real `max_block_fuel` block (three 20M-fuel transactions, each run until out of fuel) of each engine's worst compute pattern, measured whole, against the projection:

| engine | worst compute pattern | measured ms [min–max] | projected ms | vs 150 ms |
|---|---:|---:|---:|---:|
| soroban-wasmi | pointer chase, 2 MiB, random | 349 [347–355] | 428 | 2× |
| wasmi 2.0 | call_indirect, 1,024 locals | 366 [357–383] | 369 | 2× |
| wasmi 0.38 | br_if not taken | 1695 [1548–1812] | 1836 | 11× |

## 6. Cold versus warm, and an engine's cost per block

**Cold** = the first transaction on a fresh block engine that has just loaded the code (engine and load outside the clock); **warm** = the same transaction again on an engine that has run it 10 times. Fuel must be identical (asserted for every repetition).

| transaction | fuel (identical cold and warm) | soroban-wasmi cold / warm µs | wasmi 2.0 cold / warm µs | wasmi 0.38 cold / warm µs |
|---|---:|---:|---:|---:|
| counter increment | 71,711 / 71,710 / 71,573 | 10.8 / 10.6 (+2%) | 10.4 / 10.2 (+2%) | 11.2 / 11.1 (+2%) |
| token transfer | 85,139 / 85,113 / 83,082 | 15.4 / 14.7 (+5%) | 12.6 / 12.3 (+2%) | 15.1 / 14.8 (+2%) |
| escrow open (payable) | 78,771 / 78,747 / 76,793 | 14.2 / 13.2 (+7%) | 12.2 / 12.0 (+2%) | 13.9 / 13.8 (+1%) |
| loop: call, 1,024 locals × 1,000 | 2,252,317 / 188,315 / 2,103,143 | 1,161 / 1,445 (-20%) | 1,692 / 1,208 (+40%) | 1,817 / 1,382 (+31%) |
| loop: i64.div_u by 1, chain × 1,000 | 142,287 / 142,285 / 71,191 | 259 / 162 (+60%) | 104 / 101 (+3%) | 139 / 130 (+7%) |
| loop: pointer chase, 2 MiB, random × 1,000 | 861,123 / 861,121 / 428,105 | 1,056 / 1,054 (+0%) | 583 / 567 (+3%) | 1,199 / 1,030 (+16%) |
| call_v1 × 100 to a 32-page callee | 8,518,283 / 8,518,182 / 8,515,945 | 964 / 966 (-0%) | 906 / 903 (+0%) | 982 / 979 (+0%) |

**An engine's cost per block**: creating a block engine (the engine, its linker with the 20 host functions), loading N codes (each a distinct hash: translated anew) and dropping it.

| engine | what | create + load µs | drop µs |
|---|---:|---:|---:|
| soroban-wasmi | create an empty engine | 2.74 | drop 1.04 |
|  | 1 × guest token.wasm | 18.9 (18.9 per code) | drop 1.57 (1.57 per code) |
|  | 10 × guest token.wasm | 183 (18.3 per code) | drop 4.61 (0.46 per code) |
|  | 100 × guest token.wasm | 1,837 (18.4 per code) | drop 40.6 (0.41 per code) |
|  | 1000 × guest token.wasm | 20,216 (20.2 per code) | drop 388 (0.39 per code) |
|  | 1 × 256 KB straight-line | 609 (609 per code) | drop 1.57 (1.57 per code) |
|  | 10 × 256 KB straight-line | 6,760 (676 per code) | drop 69.7 (6.97 per code) |
| wasmi 2.0 | create an empty engine | 1.51 | drop 0.90 |
|  | 1 × guest token.wasm | 40.3 (40.3 per code) | drop 1.90 (1.90 per code) |
|  | 10 × guest token.wasm | 392 (39.2 per code) | drop 7.69 (0.77 per code) |
|  | 100 × guest token.wasm | 3,901 (39.0 per code) | drop 67.7 (0.68 per code) |
|  | 1000 × guest token.wasm | 39,249 (39.2 per code) | drop 623 (0.62 per code) |
|  | 1 × 256 KB straight-line | 1,787 (1,787 per code) | drop 1.96 (1.96 per code) |
|  | 10 × 256 KB straight-line | 17,864 (1,786 per code) | drop 3.98 (0.40 per code) |
| wasmi 0.38 | create an empty engine | 1.44 | drop 0.89 |
|  | 1 × guest token.wasm | 25.4 (25.4 per code) | drop 1.79 (1.79 per code) |
|  | 10 × guest token.wasm | 255 (25.5 per code) | drop 7.69 (0.77 per code) |
|  | 100 × guest token.wasm | 2,472 (24.7 per code) | drop 71.4 (0.71 per code) |
|  | 1000 × guest token.wasm | 24,429 (24.4 per code) | drop 699 (0.70 per code) |
|  | 1 × 256 KB straight-line | 913 (913 per code) | drop 1.70 (1.70 per code) |
|  | 10 × 256 KB straight-line | 9,033 (903 per code) | drop 3.66 (0.37 per code) |

## 7. Native stack

The smallest thread stack on which the scenario runs as expected, bisected to 4 KiB (the OS rounds a thread's stack up to its page: 16 KiB on this Mac) in child processes (a stack overflow aborts the process). The thread both loads the code (a fresh block engine) and executes the transaction, as the executor thread will. Programs p1→…→p8 each `call_v1` the next; with recursion, each first recurses 1,000 Wasm frames (wasmi keeps Wasm frames off the native stack: the numbers should not move). Release build.

| scenario | soroban-wasmi | wasmi 2.0 | wasmi 0.38 |
|---|---:|---:|---:|
| one program, no recursion | 16 KiB | 16 KiB | 16 KiB |
| depth 8, no Wasm recursion | 20 KiB | 20 KiB | 20 KiB |
| depth 8, 1,000 Wasm frames per program | 20 KiB | 20 KiB | 20 KiB |
| depth 9 attempted (CallDepth), 1,000 Wasm frames per program | 20 KiB | 20 KiB | 20 KiB |

_Total run time: 783 s._
