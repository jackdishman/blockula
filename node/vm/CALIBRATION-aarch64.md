<!-- Floor-machine run (docs/PROGRAMS-PLAN.md S3): GitHub Actions aarch64 Linux (ubuntu-24.04-arm runner), --quick, via .github/workflows/calibrate-vm.yml. Generated 2026-10-20. Development numbers, never prices. The Mac run is CALIBRATION.md. -->

# Calibration run

- Machine: aarch64 (aarch64, linux); development numbers, never prices
- rustc: rustc 1.98.1 (48a229cea 2026-09-01)
- Build: `release/bench` profile, overflow checks on, debug assertions off
- Engines: soroban-wasmi 29.0.0, wasmi 2.0.0, wasmi 0.38.0
- Repetitions: 3 (+1 warmup), samples ≥ 2 ms, loop runs ≈ 10 ms

## 1. ns per fuel by operator class

Each case is a loop whose body is `k` copies of one unit dominated by one class (plus the loop's own ~9 fuel: `br_if`, a decrement, `br`, and wasmi 2.0's hidden 1 per loop entry). ns per fuel = (time of N iterations − time of 0) / N / (fuel per iteration), so instantiation and the transaction's fixed cost cancel. Fuel per unit = fuel per iteration / k, under each engine's mapping of schedule v1 (wasmi 0.38: its own IR fuel). Cells: **ns/fuel median [min–max]** (fuel per unit). Values above 2.5 ns/fuel are marked **bold**.

| class | case | soroban-wasmi | wasmi 2.0 | wasmi 0.38 |
|---|---:|---:|---:|---:|
| control | empty loop (the loop's own overhead) | 1.51 [1.51–1.51] (9.00) | 0.61 [0.60–0.61] (9.00) | 2.13 [2.13–2.13] (4.00) |
| control | br to the next block end: (block (br 0)) | 1.59 [1.59–1.59] (1.14) | 1.43 [1.43–1.43] (1.14) | **2.96 [2.95–2.96] (1.06)** |
| control | br_if not taken | 2.50 [2.50–2.50] (2.14) | 1.57 [1.56–1.57] (2.14) | **37.3 [37.3–37.4] (0.06)** |
| control | br_if to the function's end, not taken | **2.83 [2.83–2.83] (1.14)** | 1.57 [1.57–1.57] (2.14) | 2.18 [2.18–2.18] (1.06) |
| control | empty loops: (loop) | 1.58 [1.58–1.58] (1.14) | 1.58 [1.58–1.59] (1.14) | **2.73 [2.73–2.73] (1.06)** |
| control | if/else, unpredictable | 1.69 [1.69–1.70] (13.6) | 0.69 [0.69–0.69] (12.6) | **2.90 [2.90–2.91] (6.25)** |
| br_table | br_table 8 targets, fixed index | **3.23 [3.23–3.24] (2.56)** | 2.00 [2.00–2.00] (2.56) | **7.71 [7.70–7.72] (1.25)** |
| br_table | br_table 64 targets, random index | 1.78 [1.78–1.79] (13.2) | 0.60 [0.60–0.60] (13.2) | **3.69 [3.68–3.70] (6.00)** |
| br_table | br_table 1,024 targets, random index | 1.62 [1.62–1.63] (20.0) | 0.60 [0.60–0.61] (20.0) | **3.02 [3.02–3.04] (9.00)** |
| br_table | br_table 4,096 targets, random index | 1.62 [1.62–1.64] (20.0) | 0.60 [0.60–0.60] (20.0) | **3.03 [3.03–3.05] (9.00)** |
| br_table | br_table 4,096 entries to one label, random index | 1.62 [1.62–1.62] (20.0) | 0.42 [0.42–0.42] (20.0) | 2.39 [2.38–2.39] (9.00) |
| call | call () -> (), no locals | 0.75 [0.75–0.76] (12.6) | 0.74 [0.74–0.74] (11.6) | **5.19 [5.18–5.19] (3.25)** |
| call | call, 8 locals | 0.79 [0.78–0.80] (13.6) | 1.01 [1.01–1.01] (11.6) | **4.10 [4.10–4.10] (4.25)** |
| call | call, 64 locals | 0.66 [0.66–0.66] (20.6) | 1.46 [1.46–1.47] (11.6) | 2.09 [2.09–2.09] (11.2) |
| call | call, 256 locals | 0.54 [0.54–0.54] (44.6) | **2.54 [2.54–2.54] (11.6)** | 1.03 [1.03–1.03] (35.2) |
| call | call, 1,024 locals | 0.39 [0.39–0.39] (141) | **5.09 [5.08–5.09] (11.6)** | 0.57 [0.57–0.57] (131) |
| call | call, 32 params | 1.16 [1.16–1.16] (49.1) | 1.01 [1.01–1.01] (44.1) | **4.72 [4.72–4.73] (7.50)** |
| call | call, 32 params + 992 locals | 0.55 [0.55–0.55] (173) | 1.90 [1.89–1.90] (44.1) | 0.60 [0.60–0.60] (132) |
| call | call i64 -> i64 | 0.92 [0.92–0.92] (17.6) | 0.68 [0.68–0.68] (16.6) | **5.39 [5.39–5.39] (4.25)** |
| call | recursion 64 deep, 1,024 locals a frame | 0.46 [0.46–0.46] (9,496) | **4.46 [4.44–4.47] (1,111)** | 0.69 [0.68–0.69] (8,647) |
| call | call_indirect, fixed index | 1.32 [1.32–1.32] (13.6) | 1.37 [1.37–1.37] (12.6) | **13.8 [13.8–13.9] (3.25)** |
| call | call_indirect, random index of 4,096 | 1.26 [1.26–1.27] (23.1) | 0.87 [0.87–0.87] (22.1) | **7.74 [7.73–7.76] (7.50)** |
| call | call_indirect, 1,024 locals | 0.49 [0.49–0.49] (142) | **5.40 [5.36–5.42] (12.6)** | 0.71 [0.71–0.72] (131) |
| i32 arith | i32 unary chain (clz ctz popcnt eqz) | 2.31 [2.30–2.31] (4.69) | 1.03 [1.03–1.03] (4.69) | **2.77 [2.77–2.78] (4.25)** |
| i64 arith | i64 unary chain (clz ctz popcnt) | 2.45 [2.45–2.45] (3.69) | 1.22 [1.22–1.22] (3.69) | **2.97 [2.97–2.97] (3.25)** |
| i32 arith | i32 binary chain, local operand | 1.66 [1.66–1.67] (24.8) | 0.25 [0.25–0.25] (24.8) | **3.16 [3.15–3.17] (12.0)** |
| i64 arith | i64 binary chain, local operand | 1.66 [1.66–1.66] (24.8) | 0.25 [0.25–0.25] (24.8) | **3.19 [3.19–3.19] (12.0)** |
| i32 arith | i32 binary chain, constant operand | 1.66 [1.66–1.66] (24.8) | 0.24 [0.24–0.24] (24.8) | **2.98 [2.98–2.99] (12.0)** |
| i32 arith | i32 3-address: a = b op c | 1.58 [1.58–1.58] (8.56) | 0.21 [0.21–0.21] (8.56) | **2.93 [2.93–2.94] (2.25)** |
| i64 arith | i64.mul chain | 1.65 [1.64–1.69] (2.17) | 0.57 [0.57–0.57] (2.17) | **2.87 [2.86–2.87] (1.06)** |
| div/rem | i32.div_u by 1, chain | 2.08 [2.08–2.08] (2.17) | 0.69 [0.69–0.69] (2.17) | **4.53 [4.53–4.54] (1.06)** |
| div/rem | i32.div_s by 1, chain | 2.17 [2.17–2.18] (2.17) | 0.72 [0.72–0.72] (2.17) | **4.66 [4.66–4.66] (1.06)** |
| div/rem | i32.rem_u, xor chain | 1.74 [1.73–1.74] (4.34) | 0.62 [0.62–0.62] (4.34) | **3.58 [3.58–3.58] (2.12)** |
| div/rem | i64.div_u by 1, chain | 2.10 [2.10–2.10] (2.17) | 0.69 [0.69–0.69] (2.17) | **4.54 [4.52–4.54] (1.06)** |
| div/rem | i64.div_s by 1, chain | 2.12 [2.12–2.12] (2.17) | 0.72 [0.72–0.72] (2.17) | **4.67 [4.66–4.70] (1.06)** |
| div/rem | i64.div_u by 3, xor chain | 2.48 [2.48–2.48] (4.34) | 1.44 [1.44–1.44] (4.34) | **5.19 [5.18–5.20] (2.12)** |
| div/rem | i64.rem_u, xor chain | 1.72 [1.72–1.72] (4.34) | 0.62 [0.62–0.62] (4.34) | **3.58 [3.58–3.59] (2.12)** |
| div/rem | i64.rem_s, xor chain | 1.77 [1.72–1.78] (4.34) | 0.56 [0.56–0.56] (4.34) | **3.43 [3.43–3.44] (2.12)** |
| compare | i32 compare chain | 1.65 [1.65–1.66] (8.69) | 0.24 [0.24–0.24] (8.69) | **2.83 [2.82–2.88] (4.25)** |
| compare | i64 compare chain | 1.70 [1.70–1.70] (6.69) | 0.25 [0.25–0.25] (6.69) | **2.95 [2.93–2.96] (2.25)** |
| compare | i32.eqz chain | 1.64 [1.64–1.64] (1.17) | 0.08 [0.08–0.08] (1.17) | **2.79 [2.79–2.79] (1.06)** |
| compare | wrap/extend chain | 1.80 [1.79–1.80] (4.69) | 0.60 [0.60–0.60] (4.69) | **2.65 [2.65–2.66] (3.25)** |
| select | select chain | 1.61 [1.61–1.61] (3.34) | 0.39 [0.39–0.39] (3.34) | 2.40 [2.39–2.40] (1.12) |
| select | select, unpredictable condition | 1.59 [1.59–1.59] (12.6) | 0.27 [0.27–0.27] (12.6) | **2.67 [2.66–2.67] (4.25)** |
| locals | local.get/local.set copies | 1.52 [1.52–1.53] (6.56) | 0.69 [0.68–0.69] (6.56) | 2.16 [2.16–2.16] (2.25) |
| locals | local.tee chain | 1.54 [1.54–1.55] (4.69) | 1.26 [1.26–1.27] (4.69) | 1.80 [1.80–1.80] (2.31) |
| locals | far locals (1,024 declared) | 1.55 [1.54–1.55] (4.56) | 0.77 [0.77–0.77] (4.56) | 2.11 [2.10–2.11] (2.25) |
| globals | global.get/global.set copies | 2.44 [2.42–2.44] (4.56) | 0.69 [0.69–0.69] (4.56) | **4.16 [4.14–4.16] (4.25)** |
| globals | global increment | 1.61 [1.61–1.65] (4.56) | 0.40 [0.39–0.40] (4.56) | **3.76 [3.74–3.77] (3.25)** |
| globals | global.get; drop | 1.54 [1.54–1.54] (2.28) | 0.56 [0.56–0.56] (2.28) | **4.19 [4.18–4.19] (1.12)** |
| load | i32.load chain, one address | **3.85 [3.85–3.86] (1.17)** | 1.56 [1.55–1.56] (1.17) | **4.50 [4.50–4.50] (1.06)** |
| load | narrow loads chain, one address | **4.07 [4.07–4.08] (5.69)** | 1.58 [1.58–1.58] (5.69) | **4.56 [4.55–4.56] (5.25)** |
| load | pointer chase, 64 KiB, random | **3.94 [3.94–3.94] (1.17)** | 1.68 [1.68–1.68] (1.17) | **4.71 [4.70–4.84] (1.06)** |
| load | pointer chase, 2 MiB, random | **24.4 [24.1–24.6] (1.17)** | **22.2 [22.1–23.0] (1.17)** | **27.2 [27.0–28.9] (1.06)** |
| load | random independent i64 loads, 2 MiB | 2.44 [2.44–2.45] (10.6) | 0.77 [0.77–0.77] (10.6) | **6.09 [6.07–6.10] (4.25)** |
| store | i64.store, one address | 1.61 [1.61–1.61] (3.28) | 0.48 [0.48–0.48] (3.28) | **3.11 [3.11–3.11] (1.12)** |
| store | narrow stores, one address | 1.70 [1.69–1.70] (9.56) | 0.39 [0.39–0.39] (9.56) | **2.97 [2.97–2.98] (3.25)** |
| store | random i64 stores, 2 MiB | 1.65 [1.64–1.66] (10.6) | 0.30 [0.28–0.33] (10.6) | **3.33 [3.32–3.34] (4.25)** |
| memory | memory.size; drop | 1.67 [1.67–1.68] (2.28) | 0.69 [0.69–0.69] (2.28) | **3.36 [3.35–3.37] (1.12)** |

**Worst cases per engine** (median ns/fuel; the five highest):

| engine | case | ns/fuel | fuel/unit | vs 2.5 ns |
|---|---:|---:|---:|---:|
| soroban-wasmi | pointer chase, 2 MiB, random | 24.4 [24.1–24.6] | 1.17 | 9.7× |
|  | narrow loads chain, one address | 4.07 [4.07–4.08] | 5.69 | 1.6× |
|  | pointer chase, 64 KiB, random | 3.94 [3.94–3.94] | 1.17 | 1.6× |
|  | i32.load chain, one address | 3.85 [3.85–3.86] | 1.17 | 1.5× |
|  | br_table 8 targets, fixed index | 3.23 [3.23–3.24] | 2.56 | 1.3× |
| wasmi 2.0 | pointer chase, 2 MiB, random | 22.2 [22.1–23.0] | 1.17 | 8.9× |
|  | call_indirect, 1,024 locals | 5.40 [5.36–5.42] | 12.6 | 2.2× |
|  | call, 1,024 locals | 5.09 [5.08–5.09] | 11.6 | 2.0× |
|  | recursion 64 deep, 1,024 locals a frame | 4.46 [4.44–4.47] | 1,111 | 1.8× |
|  | call, 256 locals | 2.54 [2.54–2.54] | 11.6 | 1.0× |
| wasmi 0.38 | br_if not taken | 37.3 [37.3–37.4] | 0.06 | 14.9× |
|  | pointer chase, 2 MiB, random | 27.2 [27.0–28.9] | 1.06 | 10.9× |
|  | call_indirect, fixed index | 13.8 [13.8–13.9] | 3.25 | 5.5× |
|  | call_indirect, random index of 4,096 | 7.74 [7.73–7.76] | 7.50 | 3.1× |
|  | br_table 8 targets, fixed index | 7.71 [7.70–7.72] | 1.25 | 3.1× |

**soroban-wasmi and wasmi 2.0 more than 2× apart** (ns/fuel, where either is above 0.5):

| case | soroban-wasmi | wasmi 2.0 | ratio |
|---|---:|---:|---:|
| empty loop (the loop's own overhead) | 1.51 | 0.61 | 2.5× |
| if/else, unpredictable | 1.69 | 0.69 | 2.5× |
| br_table 64 targets, random index | 1.78 | 0.60 | 3.0× |
| br_table 1,024 targets, random index | 1.62 | 0.60 | 2.7× |
| br_table 4,096 targets, random index | 1.62 | 0.60 | 2.7× |
| br_table 4,096 entries to one label, random index | 1.62 | 0.42 | 3.9× |
| call, 64 locals | 0.66 | 1.46 | 2.2× |
| call, 256 locals | 0.54 | 2.54 | 4.7× |
| call, 1,024 locals | 0.39 | 5.09 | 13.0× |
| call, 32 params + 992 locals | 0.55 | 1.90 | 3.4× |
| recursion 64 deep, 1,024 locals a frame | 0.46 | 4.46 | 9.7× |
| call_indirect, 1,024 locals | 0.49 | 5.40 | 11.0× |
| i32 unary chain (clz ctz popcnt eqz) | 2.31 | 1.03 | 2.2× |
| i64 unary chain (clz ctz popcnt) | 2.45 | 1.22 | 2.0× |
| i32 binary chain, local operand | 1.66 | 0.25 | 6.6× |
| i64 binary chain, local operand | 1.66 | 0.25 | 6.6× |
| i32 binary chain, constant operand | 1.66 | 0.24 | 6.9× |
| i32 3-address: a = b op c | 1.58 | 0.21 | 7.6× |
| i64.mul chain | 1.65 | 0.57 | 2.9× |
| i32.div_u by 1, chain | 2.08 | 0.69 | 3.0× |
| i32.div_s by 1, chain | 2.17 | 0.72 | 3.0× |
| i32.rem_u, xor chain | 1.74 | 0.62 | 2.8× |
| i64.div_u by 1, chain | 2.10 | 0.69 | 3.0× |
| i64.div_s by 1, chain | 2.12 | 0.72 | 3.0× |
| i64.rem_u, xor chain | 1.72 | 0.62 | 2.8× |
| i64.rem_s, xor chain | 1.77 | 0.56 | 3.2× |
| i32 compare chain | 1.65 | 0.24 | 6.8× |
| i64 compare chain | 1.70 | 0.25 | 6.7× |
| i32.eqz chain | 1.64 | 0.08 | 20.4× |
| wrap/extend chain | 1.80 | 0.60 | 3.0× |
| select chain | 1.61 | 0.39 | 4.1× |
| select, unpredictable condition | 1.59 | 0.27 | 5.9× |
| local.get/local.set copies | 1.52 | 0.69 | 2.2× |
| global.get/global.set copies | 2.44 | 0.69 | 3.5× |
| global increment | 1.61 | 0.40 | 4.1× |
| global.get; drop | 1.54 | 0.56 | 2.7× |
| i32.load chain, one address | 3.85 | 1.56 | 2.5× |
| narrow loads chain, one address | 4.07 | 1.58 | 2.6× |
| pointer chase, 64 KiB, random | 3.94 | 1.68 | 2.3× |
| random independent i64 loads, 2 MiB | 2.44 | 0.77 | 3.2× |
| i64.store, one address | 1.61 | 0.48 | 3.4× |
| narrow stores, one address | 1.70 | 0.39 | 4.4× |
| random i64 stores, 2 MiB | 1.65 | 0.30 | 5.5× |
| memory.size; drop | 1.67 | 0.69 | 2.4× |

## 2. Translation and validation, ns per byte

Cold: each load is `BlockEngine::load` (a structural read of memory and data, then the engine's `Module::new`: its own parse, validation and translation) on a fresh block engine made outside the clock, as a block loads every declared code into a fresh engine (4.3). `validate` is upload validation (`validate.rs`, wasmparser 0.228 plus our rules). CPU caches are warm (the same bytes are loaded repeatedly), so these are lower bounds for a cold machine. *Implied fuel* = time / 2.5 ns. **Implied LOAD_PER_BYTE** = (load time / 2.5 ns − LOAD_BASE 2,000) / bytes, against schedule v1's 10; **implied LOAD_BASE** comes from the smallest modules.

| module | bytes | validate µs (ns/B) | soroban-wasmi load µs (ns/B) | wasmi 2.0 load µs (ns/B) | wasmi 0.38 load µs (ns/B) | soroban-wasmi implied LOAD_PER_BYTE | wasmi 2.0 implied LOAD_PER_BYTE | wasmi 0.38 implied LOAD_PER_BYTE |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| guest counter.wasm | 565 | 12.4 (21.9) | 16.2 (28.7) | 25.5 (45.1) | 18.2 (32.3) | 7.94 | **14.5** | 9.37 |
| guest token.wasm | 2,467 | 36.4 (14.8) | 43.0 (17.4) | 87.5 (35.5) | 55.5 (22.5) | 6.16 | **13.4** | 8.18 |
| guest escrow.wasm | 3,389 | 44.9 (13.3) | 51.7 (15.3) | 113 (33.3) | 68.4 (20.2) | 5.52 | **12.7** | 7.48 |
| guest router.wasm | 1,484 | 23.1 (15.6) | 27.0 (18.2) | 49.0 (33.0) | 33.0 (22.2) | 5.93 | **11.9** | 7.54 |
| minimal program | 51 | 2.34 (45.8) | 2.85 (55.9) | 4.36 (85.5) | 3.04 (59.7) | 0.00 | 0.00 | 0.00 |
| minimal program + 20 imports | 621 | 15.0 (24.1) | 15.4 (24.8) | 19.7 (31.7) | 16.7 (26.8) | 6.70 | 9.47 | 7.51 |
| one function, straight-line i64 arithmetic | 262,141 | 1,463 (5.58) | 1,384 (5.28) | 4,278 (16.3) | 2,198 (8.38) | 2.10 | 6.52 | 3.35 |
| 5040 small functions (≈40 B bodies) | 262,137 | 2,963 (11.3) | 4,210 (16.1) | 7,029 (26.8) | 4,744 (18.1) | 6.42 | **10.7** | 7.23 |
| 6094 functions with 1,024 locals each | 262,099 | 3,375 (12.9) | 4,897 (18.7) | 19,354 (73.8) | 5,560 (21.2) | 7.47 | **29.5** | 8.48 |
| br_table, 2 × 131030 targets to one label | 262,143 | 3,650 (13.9) | 12,004 (45.8) | 3,698 (14.1) | 3,601 (13.7) | **18.3** | 5.64 | 5.49 |
| br_table, 12 × 4,096 distinct nested targets | 244,343 | 2,652 (10.9) | 4,420 (18.1) | 5,403 (22.1) | 3,520 (14.4) | 7.23 | 8.84 | 5.75 |
| 87361 nested blocks | 262,142 | 3,581 (13.7) | 3,823 (14.6) | 9,268 (35.4) | 5,074 (19.4) | 5.83 | **14.1** | 7.74 |
| 32760 if/else | 262,143 | 3,747 (14.3) | 3,567 (13.6) | 11,546 (44.0) | 5,402 (20.6) | 5.44 | **17.6** | 8.24 |
| 28942 distinct i64 constants | 258,321 | 698 (2.70) | 2,572 (9.96) | 1,154 (4.47) | 560 (2.17) | 3.97 | 1.78 | 0.86 |
| 131035 B of data + as much code (32 pages) | 262,143 | 974 (3.72) | 1,083 (4.13) | 1,503 (5.73) | 745 (2.84) | 1.64 | 2.29 | 1.13 |
| 14980 distinct function types | 223,362 | 10,708 (47.9) | 9,644 (43.2) | 13,232 (59.2) | 7,416 (33.2) | **17.3** | **23.7** | **13.3** |
| 87364 identical function types | 262,144 | 21,534 (82.1) | 14,713 (56.1) | 25,330 (96.6) | 12,586 (48.0) | **22.4** | **38.6** | **19.2** |
| 22105 exports of one function | 254,196 | 6,927 (27.2) | 6,934 (27.3) | 6,978 (27.5) | 6,883 (27.1) | **10.9** | **11.0** | **10.8** |
| 131019 calls | 262,143 | 2,586 (9.86) | 4,827 (18.4) | 7,440 (28.4) | 5,462 (20.8) | 7.36 | **11.3** | 8.33 |
| 43679 far local.get/set pairs (1,024 locals) | 262,140 | 1,857 (7.09) | 1,513 (5.77) | 4,238 (16.2) | 2,202 (8.40) | 2.30 | 6.46 | 3.35 |
| 21 × (4,000 local.gets, local.set over them, 3,999 adds) | 252,148 | 2,157 (8.56) | 2,179 (8.64) | 10,455 (41.5) | 4,537 (18.0) | 3.45 | **16.6** | 7.19 |
| 16 × (4,000-deep stack, 1,000 local.sets) | 256,064 | 2,145 (8.38) | 2,084 (8.14) | 6,035 (23.6) | 2,620 (10.2) | 3.25 | 9.42 | 4.08 |
| 43680 dead-code units after unreachable | 262,140 | 1,870 (7.13) | 1,337 (5.10) | 2,543 (9.70) | 1,219 (4.65) | 2.03 | 3.87 | 1.85 |
| 256 globals, 52160 global.get/set pairs | 262,144 | 1,961 (7.48) | 1,516 (5.78) | 4,318 (16.5) | 1,953 (7.45) | 2.31 | 6.58 | 2.97 |

Spread, and the drop of an engine holding the module (less an empty engine's drop): part of the block's cost, unpriced.

| module | soroban-wasmi load [min–max] µs | soroban-wasmi drop µs | wasmi 2.0 load [min–max] µs | wasmi 2.0 drop µs | wasmi 0.38 load [min–max] µs | wasmi 0.38 drop µs | validate [min–max] µs |
|---|---:|---:|---:|---:|---:|---:|---:|
| guest counter.wasm | [15.8–16.9] | 2.52 | [24.7–25.9] | 4.43 | [17.9–18.6] | 3.03 | [12.3–12.5] |
| guest token.wasm | [42.4–43.9] | 4.75 | [87.0–88.8] | 8.38 | [54.3–55.8] | 7.11 | [36.2–36.6] |
| guest escrow.wasm | [51.3–52.3] | 4.16 | [111–113] | 7.76 | [67.3–68.4] | 3.69 | [44.7–45.2] |
| guest router.wasm | [27.0–27.7] | 3.67 | [48.4–49.6] | 5.20 | [31.9–33.6] | 2.80 | [22.8–24.1] |
| minimal program | [2.82–2.90] | 1.87 | [4.32–4.93] | 1.83 | [3.04–3.15] | 1.91 | [2.33–2.55] |
| minimal program + 20 imports | [15.4–15.9] | 3.25 | [19.4–19.7] | 1.76 | [16.1–23.8] | 3.93 | [14.9–15.2] |
| one function, straight-line i64 arithmetic | [1,372–1,388] | 31.7 | [4,258–4,316] | 48.7 | [2,196–2,258] | 3.09 | [1,461–1,483] |
| 5040 small functions (≈40 B bodies) | [4,174–4,226] | 3.29 | [7,027–7,060] | 114 | [4,742–4,750] | 98.1 | [2,961–2,975] |
| 6094 functions with 1,024 locals each | [4,887–4,911] | 3.35 | [19,338–19,355] | 131 | [5,556–5,578] | 118 | [3,361–3,380] |
| br_table, 2 × 131030 targets to one label | [11,923–12,007] | 111 | [3,672–3,872] | 3.42 | [3,601–3,608] | 2.77 | [3,647–3,684] |
| br_table, 12 × 4,096 distinct nested targets | [4,418–4,456] | 2.42 | [5,399–5,405] | 3.53 | [3,501–3,522] | 3.62 | [2,648–2,658] |
| 87361 nested blocks | [3,789–3,827] | 0.97 | [9,256–9,272] | 617 | [5,068–5,099] | 293 | [3,561–3,584] |
| 32760 if/else | [3,549–3,570] | 27.0 | [11,544–11,546] | 258 | [5,398–5,409] | 53.2 | [3,746–3,897] |
| 28942 distinct i64 constants | [2,568–2,611] | 161 | [1,152–1,154] | 3.68 | [558–562] | 3.40 | [695–699] |
| 131035 B of data + as much code (32 pages) | [1,080–1,084] | 2.79 | [1,501–1,509] | 2.67 | [737–746] | 2.66 | [972–980] |
| 14980 distinct function types | [9,637–9,652] | 1,138 | [13,198–13,243] | 277 | [7,413–7,433] | 249 | [10,673–10,709] |
| 87364 identical function types | [14,639–14,731] | 2.41 | [25,317–25,393] | 3.35 | [12,578–12,665] | 2.81 | [21,515–21,580] |
| 22105 exports of one function | [6,903–6,941] | 508 | [6,974–7,005] | 504 | [6,882–6,901] | 500 | [6,915–6,941] |
| 131019 calls | [4,798–4,831] | 33.2 | [7,433–7,495] | 258 | [5,447–5,476] | 41.7 | [2,583–2,590] |
| 43679 far local.get/set pairs (1,024 locals) | [1,506–1,521] | 12.9 | [4,224–4,259] | 26.1 | [2,199–2,224] | 2.79 | [1,855–1,873] |
| 21 × (4,000 local.gets, local.set over them, 3,999 adds) | [2,172–2,183] | 46.2 | [10,446–10,460] | 173 | [4,528–4,590] | 3.87 | [2,154–2,159] |
| 16 × (4,000-deep stack, 1,000 local.sets) | [2,066–2,085] | 28.8 | [6,034–6,051] | 4.02 | [2,619–2,623] | 3.54 | [2,125–2,193] |
| 43680 dead-code units after unreachable | [1,334–1,350] | 2.98 | [2,541–2,548] | 3.45 | [1,216–1,220] | 3.31 | [1,866–1,887] |
| 256 globals, 52160 global.get/set pairs | [1,516–1,524] | 2.88 | [4,309–4,327] | 73.9 | [1,942–1,988] | 3.45 | [1,936–1,978] |

**Summary.** Schedule v1: LOAD_BASE 2,000, LOAD_PER_BYTE 10; upload 50,000 + 20 per byte.

| engine | worst ns/B, any size | worst ns/B, modules > 100 KB | implied LOAD_PER_BYTE (worst > 100 KB) | minimal module's load (implied LOAD_BASE) | worst upload (validate + load), modules > 100 KB |
|---|---:|---:|---:|---:|---:|
| soroban-wasmi | 87364 identical function types (56.1 ns/B) | 87364 identical function types (56.1 ns/B) | 22.4 | 2.85 µs = 1,140 fuel | 87364 identical function types (138 ns/B = 55.3 fuel/B) |
| wasmi 2.0 | 87364 identical function types (96.6 ns/B) | 87364 identical function types (96.6 ns/B) | 38.6 | 4.36 µs = 1,745 fuel | 87364 identical function types (179 ns/B = 71.5 fuel/B) |
| wasmi 0.38 | minimal program (59.7 ns/B) | 87364 identical function types (48.0 ns/B) | 19.2 | 3.04 µs = 1,218 fuel | 87364 identical function types (130 ns/B = 52.1 fuel/B) |

## 3. Instantiation

Per module, three ways: **raw** (`BlockEngine::invoke` of an empty `bk_noop` with a host that refuses every import: the engine's store, linking, instance and drop), **tx** (a top-level transaction calling `bk_noop` through `execute`: also the executor's fixed cost) and **call_v1** (each call of a loop `call_v1`s `bk_noop` on the module: the repeatable pattern, charged `call` 20,100 + `instantiate_fuel`). *Schedule* = `instantiate_fuel(pages, data)` (1,000 + 2,000 per page + 1 per data byte), which counts no imports, functions, exports, globals, tables or segments. Time ÷ 2.5 ns is the fuel it would need.

| module | bytes | schedule fuel | soroban-wasmi raw µs | soroban-wasmi tx µs (ns/fuel) | wasmi 2.0 raw µs | wasmi 2.0 tx µs (ns/fuel) | wasmi 0.38 raw µs | wasmi 0.38 tx µs (ns/fuel) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 page, no imports | 52 | 3,000 | 0.92 | 1.89 (0.63) | 1.14 | 2.00 (0.67) | 0.90 | 1.83 (0.61) |
| 1 page, 20 imports | 622 | 3,000 | 6.34 | 7.18 (2.39) | 5.74 | 6.62 (2.21) | 5.82 | 6.81 (2.27) |
| 32 pages | 52 | 65,000 | 29.0 | 33.3 (0.51) | 29.0 | 30.9 (0.48) | 32.4 | 36.6 (0.56) |
| 32 pages, 20 imports, + touch every 4 KiB | 667 | 65,000 | 44.7 | 45.9 (0.71) | 39.3 | 32.2 (0.50) | 31.3 | 27.8 (0.43) |
| data: 1 segment of 120,000 B | 240,075 | 125,000 | 3.33 | 4.40 (0.04) | 3.55 | 4.57 (0.04) | 3.33 | 4.36 (0.03) |
| data: 1,000 segments of 1 B | 8,001 | 4,000 | 14.0 | **15.3 (3.82)** | 19.8 | **20.8 (5.20)** | 16.9 | **18.2 (4.55)** |
| data: 1,000 segments of 120 B | 246,999 | 125,000 | 17.7 | 19.5 (0.16) | 22.7 | 23.7 (0.19) | 19.7 | 21.0 (0.17) |
| 256 globals | 1,337 | 3,000 | 3.99 | 4.98 (1.66) | 5.79 | 6.69 (2.23) | 4.00 | 4.92 (1.64) |
| table of 4,096, 1,000 element segments | 10,050 | 3,000 | 41.0 | **42.5 (14.1)** | 83.8 | **85.1 (28.4)** | 67.0 | **68.0 (22.7)** |
| 5040 functions | 262,137 | 3,000 | 43.1 | **44.5 (14.8)** | 77.6 | **79.2 (26.4)** | 53.8 | **56.0 (18.6)** |
| 22104 exports | 254,194 | 3,000 | 6,575 | **6,634 (2,210)** | 6,599 | **6,618 (2,205)** | 6,459 | **6,472 (2,156)** |
| all: 20 imports, 32 pages, 256 globals, table + 1,000 elems, 2,001 funcs, 1,000 data, 12265 exports | 258,924 | 66,000 | 3,662 | **3,627 (55.0)** | 3,710 | **3,723 (56.4)** | 3,604 | **3,618 (54.8)** |

**Repeated instantiation through `call_v1`** (per call: the measured fuel, then time and ns per fuel):

| callee | schedule fuel per call | soroban-wasmi fuel: µs (ns/fuel) | wasmi 2.0 fuel: µs (ns/fuel) | wasmi 0.38 fuel: µs (ns/fuel) |
|---|---:|---:|---:|---:|
| 1 page, no imports | 23,100 | 23,130: 1.43 (0.06) | 23,129: 1.65 (0.07) | 23,107: 1.53 (0.07) |
| 1 page, 20 imports | 23,100 | 23,130: 6.84 (0.30) | 23,129: 6.31 (0.27) | 23,107: 6.48 (0.28) |
| 32 pages | 85,100 | 85,130: 31.6 (0.37) | 85,129: 34.8 (0.41) | 85,107: 35.6 (0.42) |
| 32 pages, 20 imports, + touch every 4 KiB | 85,100 | 85,130: 32.3 (0.38) | 85,129: 27.8 (0.33) | 85,107: 30.6 (0.36) |
| data: 1 segment of 120,000 B | 145,100 | 145,130: 3.91 (0.03) | 145,129: 4.11 (0.03) | 145,107: 3.92 (0.03) |
| data: 1,000 segments of 1 B | 24,100 | 24,130: 14.7 (0.61) | 24,129: 20.1 (0.83) | 24,107: 17.9 (0.74) |
| data: 1,000 segments of 120 B | 145,100 | 145,130: 18.7 (0.13) | 145,129: 23.4 (0.16) | 145,107: 20.7 (0.14) |
| 256 globals | 23,100 | 23,130: 4.51 (0.19) | 23,129: 6.11 (0.26) | 23,107: 4.51 (0.20) |
| table of 4,096, 1,000 element segments | 23,100 | 23,130: 41.7 (1.80) | 23,129: **85.3 (3.69)** | 23,107: **68.1 (2.95)** |
| 5040 functions | 23,100 | 23,130: 44.2 (1.91) | 23,129: **80.4 (3.48)** | 23,107: 55.2 (2.39) |
| 22104 exports | 23,100 | 23,130: **6,563 (284)** | 23,129: **6,605 (286)** | 23,107: **6,443 (279)** |
| all: 20 imports, 32 pages, 256 globals, table + 1,000 elems, 2,001 funcs, 1,000 data, 12265 exports | 86,100 | 86,130: **3,635 (42.2)** | 86,129: **3,719 (43.2)** | 86,107: **3,615 (42.0)** |

**Touching the memory**: a transaction whose method stores to every 4 KiB of its 2 MiB, so the zeroed pages the allocator handed out lazily are faulted in (the cost an instantiation's per-page price must cover):

| module | soroban-wasmi tx fuel: µs (ns/fuel) | wasmi 2.0 tx fuel: µs (ns/fuel) | wasmi 0.38 tx fuel: µs (ns/fuel) |
|---|---:|---:|---:|
| 32 pages, 20 imports, + touch every 4 KiB | 71,146: 40.2 (0.57) (+-5.68 µs over the empty tx) | 71,145: 42.1 (0.59) (+9.86 µs over the empty tx) | 67,050: 47.1 (0.70) (+19.3 µs over the empty tx) |

## 4. Host functions (ABI v1)

Each case loops one host function N times inside a program (`bk_<case>`) through the real executor and `MemHost`; per call = (time of N − time of 0) / N, so the call's Wasm side (its arguments, the `call` itself at 10 fuel, the loop's ~9) is included, as a program would pay it. Cases bounded per transaction (events, keys written, bytes written) run that many calls a transaction. Fuel per call is measured (the schedule's charge plus that Wasm side). `MemHost` is a `BTreeMap`, not the node's state: storage costs here are the overlay's and the host's, not redb's or the tree's (those are commit-time).

| function, case | fuel per call | soroban-wasmi ns/call (ns/fuel) | wasmi 2.0 ns/call (ns/fuel) | wasmi 0.38 ns/call (ns/fuel) |
|---|---:|---:|---:|---:|
| `input_len_v1`  | 120 / 105 | 78.8 (0.66) | 66.2 (0.55) | 75.2 (0.72) |
| `input_read_v1` 44 B input | 164 / 149 | 81.3 (0.50) | 71.4 (0.44) | 80.2 (0.54) |
| `input_read_v1` 32 KiB input | 32,888 / 32,873 | 473 (0.01) | 459 (0.01) | 465 (0.01) |
| `return_data_v1` 0 B | 121 / 105 | 86.1 (0.71) | 75.7 (0.63) | 83.6 (0.80) |
| `return_data_v1` 4 KiB | 4,217 / 4,201 | 190 (0.05) | 184 (0.04) | 192 (0.05) |
| `debug_log_v1` 0 B | 121 / 105 | 81.8 (0.68) | 71.7 (0.59) | 79.8 (0.76) |
| `debug_log_v1` 64 KiB | 65,657 / 65,641 | 81.9 (0.00) | 71.7 (0.00) | 80.0 (0.00) |
| `abort_v1` 256 B message (one per tx: tx time − an empty tx's) | 343 / 350 | 699 (2.04) | 603 (1.76) | 645 (1.84) |
| `storage_read_v1` absent, 32 B key | 1,156 / 1,137 | 136 (0.12) | 125 (0.11) | 132 (0.12) |
| `storage_read_v1` 32 B key, 8 B value | 1,164 / 1,145 | 168 (0.14) | 160 (0.14) | 166 (0.15) |
| `storage_read_v1` 128 B key, 8 B value | 1,260 / 1,241 | 173 (0.14) | 165 (0.13) | 172 (0.14) |
| `storage_read_v1` 32 B key, 16 KiB value | 17,540 / 17,521 | 626 (0.04) | 617 (0.04) | 605 (0.03) |
| `storage_write_v1` overwrite, 32 B key, 0 B value | 5,444 / 5,425 | 181 (0.03) | 171 (0.03) | 177 (0.03) |
| `storage_write_v1` overwrite, 128 B key, 8 B value | 6,484 / 6,465 | 224 (0.03) | 213 (0.03) | 219 (0.03) |
| `storage_write_v1` overwrite, 32 B key, 16 KiB value (64 a tx) | 169,284 / 169,265 | 769 (0.00) | 757 (0.00) | 750 (0.00) |
| `storage_write_v1` new keys, 32 B key, 8 B value (256 a tx) | 5,527 / 5,506 | 475 (0.09) | 452 (0.08) | 468 (0.08) |
| `storage_remove_v1` absent | 3,122 / 3,105 | 129 (0.04) | 118 (0.04) | 127 (0.04) |
| `storage_remove_v1` present, 16 KiB value (256 a tx) | 3,125 / 3,106 | 1,797 (0.58) | 1,805 (0.58) | 1,811 (0.58) |
| `self_address_v1`  | 320 / 305 | 78.0 (0.24) | 69.2 (0.22) | 77.8 (0.26) |
| `caller_v1`  | 320 / 305 | 78.5 (0.25) | 69.0 (0.22) | 78.8 (0.26) |
| `value_v1`  | 320 / 305 | 78.7 (0.25) | 67.4 (0.21) | 73.4 (0.24) |
| `block_height_v1`  | 320 / 305 | 79.9 (0.25) | 66.7 (0.21) | 73.5 (0.24) |
| `self_balance_v1`  | 320 / 305 | 93.7 (0.29) | 81.1 (0.25) | 88.3 (0.29) |
| `transfer_v1` to an existing account | 10,123 / 10,105 | 132 (0.01) | 123 (0.01) | 128 (0.01) |
| `transfer_v1` opening an account (OPEN_DEDUCT) | 10,126 / 10,106 | 641 (0.06) | 626 (0.06) | 641 (0.06) |
| `call_v1` callee 1 page, empty method | 23,130 / 23,129 / 23,107 | 2,094 (0.09) | 2,384 (0.10) | 2,146 (0.09) |
| `call_v1` callee 1 page, 32 KiB input | 55,898 / 55,897 / 55,875 | 2,638 (0.05) | 2,942 (0.05) | 2,567 (0.05) |
| `call_v1` callee 32 pages, empty method | 85,130 / 85,129 / 85,107 | 35,731 (0.42) | 38,716 (0.45) | 24,176 (0.28) |
| `try_call_v1` callee traps (layer dropped) | 23,130 / 23,107 | 2,128 (0.09) | 2,361 (0.10) | 2,149 (0.09) |
| `last_return_v1` 0 B | 120 / 105 | 80.6 (0.67) | 70.9 (0.59) | 79.9 (0.76) |
| `last_return_v1` 4 KiB | 4,216 / 4,201 | 130 (0.03) | 120 (0.03) | 128 (0.03) |
| `last_return_len_v1`  | 120 / 105 | 79.5 (0.66) | 66.1 (0.55) | 72.9 (0.69) |
| `last_abort_code_v1`  | 120 / 105 | 79.6 (0.66) | 66.6 (0.55) | 72.1 (0.69) |
| `emit_v1` 8 B name, 0 B data (64 a tx) | 2,187 / 2,169 | 173 (0.08) | 165 (0.08) | 175 (0.08) |
| `emit_v1` 32 B name, 16,352 B data (1 a tx) | 133,195 / 133,177 | 1,501 (0.01) | 1,332 (0.01) | 1,372 (0.01) |

**Per byte**, from the two sizes of each function (Δtime / Δbytes, against Δfuel / Δbytes):

| function: sizes | fuel per byte | soroban-wasmi ns/B (ns/fuel) | wasmi 2.0 ns/B (ns/fuel) | wasmi 0.38 ns/B (ns/fuel) |
|---|---:|---:|---:|---:|
| `input_read_v1`: 44 B input → 32 KiB input | 1.00 | 0.01 (0.01) | 0.01 (0.01) | 0.01 (0.01) |
| `return_data_v1`: 0 B → 4 KiB | 1.00 | 0.03 (0.03) | 0.03 (0.03) | 0.03 (0.03) |
| `debug_log_v1`: 0 B → 64 KiB | 1.00 | 0.00 (0.00) | -0.00 (-0.00) | 0.00 (0.00) |
| `storage_read_v1`: 32 B key, 8 B value → 32 B key, 16 KiB value | 1.00 | 0.03 (0.03) | 0.03 (0.03) | 0.03 (0.03) |
| `storage_read_v1`: 32 B key, 8 B value → 128 B key, 8 B value | 1.00 | 0.06 (0.06) | 0.06 (0.06) | 0.06 (0.06) |
| `storage_write_v1`: overwrite, 32 B key, 0 B value → overwrite, 32 B key, 16 KiB value (64 a tx) | 10.0 | 0.04 (0.00) | 0.04 (0.00) | 0.03 (0.00) |
| `call_v1`: callee 1 page, empty method → callee 1 page, 32 KiB input | 1.00 | 0.02 (0.02) | 0.02 (0.02) | 0.01 (0.01) |
| `last_return_v1`: 0 B → 4 KiB | 1.00 | 0.01 (0.01) | 0.01 (0.01) | 0.01 (0.01) |
| `emit_v1`: 8 B name, 0 B data (64 a tx) → 32 B name, 16,352 B data (1 a tx) | 8.00 | 0.08 (0.01) | 0.07 (0.01) | 0.07 (0.01) |

## 5. An adversarial block

A `max_block_fuel` block is 60,000,000 fuel (3 transactions of `program_max_tx_fuel`, 20,000,000 each). The cost of a block made entirely of one pattern is **60M × the pattern's ns/fuel**: that is how the design sets the budget (60M × 2.5 ns = 150 ms). Each ns/fuel below is measured as in sections 1–4 (a bounded loop, not a literal 60M-fuel run, which would just be the same number × 60M); the projection is then checked against one real end-to-end 60M block per engine at the end. 150 ms is the budget; a block over it is **bold**.

| engine | block of this pattern | ns/fuel [min–max] | projected ms [min–max] | vs 150 ms |
|---|---:|---:|---:|---:|
| soroban-wasmi | compute: pointer chase, 2 MiB, random | 24.4 [24.1–24.6] | 1462 [1448–1477] | **10×** |
| soroban-wasmi | compute: narrow loads chain, one address | 4.07 [4.07–4.08] | 244 [244–245] | **2×** |
| wasmi 2.0 | compute: pointer chase, 2 MiB, random | 22.2 [22.1–23.0] | 1335 [1328–1379] | **9×** |
| wasmi 2.0 | compute: call_indirect, 1,024 locals | 5.40 [5.36–5.42] | 324 [322–325] | **2×** |
| wasmi 0.38 | compute: br_if not taken | 37.3 [37.3–37.4] | 2238 [2235–2244] | **15×** |
| wasmi 0.38 | compute: pointer chase, 2 MiB, random | 27.2 [27.0–28.9] | 1631 [1620–1736] | **11×** |
| soroban-wasmi | instantiation: call_v1 loop to the many-exports callee | 284 [284–284] | 17061 [17011–17067] | **114×** |
| wasmi 2.0 | instantiation: call_v1 loop to the many-exports callee | 288 [287–288] | 17272 [17247–17302] | **115×** |
| wasmi 0.38 | instantiation: call_v1 loop to the many-exports callee | 280 [280–280] | 16807 [16806–16809] | **112×** |
| soroban-wasmi | instantiation: call_v1 loop to the 5,040-function callee | 1.98 [1.97–1.99] | 119 [118–119] | 1× |
| wasmi 2.0 | instantiation: call_v1 loop to the 5,040-function callee | 3.41 [3.40–3.41] | 204 [204–205] | **1×** |
| wasmi 0.38 | instantiation: call_v1 loop to the 5,040-function callee | 2.25 [2.25–2.26] | 135 [135–135] | 1× |
| soroban-wasmi | instantiation: call_v1 loop to a 32-page callee | 0.41 [0.39–0.43] | 25 [24–26] | 0× |
| wasmi 2.0 | instantiation: call_v1 loop to a 32-page callee | 0.41 [0.41–0.43] | 25 [24–26] | 0× |
| wasmi 0.38 | instantiation: call_v1 loop to a 32-page callee | 0.41 [0.33–0.44] | 25 [20–27] | 0× |
| soroban-wasmi | storage_write storm: 64 × 16 KiB overwrites a tx | 0.01 [0.01–0.01] | 0 [0–0] | 0× |
| wasmi 2.0 | storage_write storm: 64 × 16 KiB overwrites a tx | 0.01 [0.01–0.01] | 0 [0–0] | 0× |
| wasmi 0.38 | storage_write storm: 64 × 16 KiB overwrites a tx | 0.01 [0.01–0.01] | 0 [0–0] | 0× |
| soroban-wasmi | load: distinct "87364 identical function types" × 22 (262,144 B each) | 5.50 [5.46–5.51] | 330 [328–330] | **2×** |
| soroban-wasmi | load: distinct minimal programs × 23904 (51 B each) | 1.13 [1.13–1.19] | 68 [68–71] | 0× |
| wasmi 2.0 | load: distinct "87364 identical function types" × 22 (262,144 B each) | 9.40 [9.36–9.42] | 564 [561–565] | **4×** |
| wasmi 2.0 | load: distinct minimal programs × 23904 (51 B each) | 1.92 [1.92–1.95] | 115 [115–117] | 1× |
| wasmi 0.38 | load: distinct "87364 identical function types" × 22 (262,144 B each) | 4.87 [4.87–4.89] | 292 [292–293] | **2×** |
| wasmi 0.38 | load: distinct minimal programs × 23904 (51 B each) | 1.15 [1.15–1.20] | 69 [69–72] | 0× |

**End-to-end check**: one real `max_block_fuel` block (three 20M-fuel transactions, each run until out of fuel) of each engine's worst compute pattern, measured whole, against the projection:

| engine | worst compute pattern | measured ms [min–max] | projected ms | vs 150 ms |
|---|---:|---:|---:|---:|
| soroban-wasmi | pointer chase, 2 MiB, random | 1389 [1385–1400] | 1462 | 9× |
| wasmi 2.0 | pointer chase, 2 MiB, random | 1270 [1263–1281] | 1335 | 8× |
| wasmi 0.38 | br_if not taken | 2226 [2226–2226] | 2238 | 15× |

## 6. Cold versus warm, and an engine's cost per block

**Cold** = the first transaction on a fresh block engine that has just loaded the code (engine and load outside the clock); **warm** = the same transaction again on an engine that has run it 10 times. Fuel must be identical (asserted for every repetition).

| transaction | fuel (identical cold and warm) | soroban-wasmi cold / warm µs | wasmi 2.0 cold / warm µs | wasmi 0.38 cold / warm µs |
|---|---:|---:|---:|---:|
| counter increment | 71,711 / 71,710 / 71,573 | 45.0 / 41.6 (+8%) | 43.0 / 38.5 (+12%) | 45.4 / 46.0 (-1%) |
| token transfer | 85,139 / 85,113 / 83,082 | 55.0 / 46.3 (+19%) | 44.7 / 39.3 (+14%) | 45.8 / 34.6 (+32%) |
| escrow open (payable) | 78,771 / 78,747 / 76,793 | 43.7 / 35.6 (+23%) | 41.8 / 39.0 (+7%) | 41.0 / 38.1 (+8%) |
| loop: call, 1,024 locals × 1,000 | 2,252,317 / 188,315 / 2,103,143 | 881 / 907 (-3%) | 985 / 996 (-1%) | 1,114 / 1,081 (+3%) |
| loop: i64.div_u by 1, chain × 1,000 | 142,287 / 142,285 / 71,191 | 297 / 294 (+1%) | 99.4 / 98.3 (+1%) | 312 / 309 (+1%) |
| loop: pointer chase, 2 MiB, random × 1,000 | 861,123 / 861,121 / 428,105 | 3,111 / 3,022 (+3%) | 1,963 / 1,974 (-1%) | 2,677 / 2,676 (+0%) |
| call_v1 × 100 to a 32-page callee | 8,518,283 / 8,518,182 / 8,515,945 | 3,398 / 3,555 (-4%) | 3,569 / 2,385 (+50%) | 2,465 / 2,446 (+1%) |

**An engine's cost per block**: creating a block engine (the engine, its linker with the 20 host functions), loading N codes (each a distinct hash: translated anew) and dropping it.

| engine | what | create + load µs | drop µs |
|---|---:|---:|---:|
| soroban-wasmi | create an empty engine | 6.49 | drop 2.61 |
|  | 1 × guest token.wasm | 43.2 (43.2 per code) | drop 7.48 (7.48 per code) |
|  | 10 × guest token.wasm | 415 (41.5 per code) | drop 15.9 (1.59 per code) |
|  | 100 × guest token.wasm | 4,171 (41.7 per code) | drop 136 (1.36 per code) |
|  | 1000 × guest token.wasm | 41,892 (41.9 per code) | drop 2,172 (2.17 per code) |
|  | 1 × 256 KB straight-line | 1,385 (1,385 per code) | drop 5.11 (5.11 per code) |
|  | 10 × 256 KB straight-line | 14,291 (1,429 per code) | drop 241 (24.1 per code) |
| wasmi 2.0 | create an empty engine | 3.58 | drop 2.26 |
|  | 1 × guest token.wasm | 91.3 (91.3 per code) | drop 10.7 (10.7 per code) |
|  | 10 × guest token.wasm | 832 (83.2 per code) | drop 42.1 (4.21 per code) |
|  | 100 × guest token.wasm | 8,253 (82.5 per code) | drop 293 (2.93 per code) |
|  | 1000 × guest token.wasm | 82,766 (82.8 per code) | drop 4,673 (4.67 per code) |
|  | 1 × 256 KB straight-line | 4,236 (4,236 per code) | drop 5.47 (5.47 per code) |
|  | 10 × 256 KB straight-line | 41,985 (4,199 per code) | drop 12.1 (1.21 per code) |
| wasmi 0.38 | create an empty engine | 3.88 | drop 2.31 |
|  | 1 × guest token.wasm | 56.6 (56.6 per code) | drop 5.95 (5.95 per code) |
|  | 10 × guest token.wasm | 521 (52.1 per code) | drop 40.4 (4.04 per code) |
|  | 100 × guest token.wasm | 5,115 (51.1 per code) | drop 258 (2.58 per code) |
|  | 1000 × guest token.wasm | 51,525 (51.5 per code) | drop 3,884 (3.88 per code) |
|  | 1 × 256 KB straight-line | 2,200 (2,200 per code) | drop 5.17 (5.17 per code) |
|  | 10 × 256 KB straight-line | 21,902 (2,190 per code) | drop 12.6 (1.26 per code) |

## 7. Native stack

The smallest thread stack on which the scenario runs as expected, bisected to 4 KiB (the OS rounds a thread's stack up to its page: 16 KiB on this Mac) in child processes (a stack overflow aborts the process). The thread both loads the code (a fresh block engine) and executes the transaction, as the executor thread will. Programs p1→…→p8 each `call_v1` the next; with recursion, each first recurses 1,000 Wasm frames (wasmi keeps Wasm frames off the native stack: the numbers should not move). Release build.

| scenario | soroban-wasmi | wasmi 2.0 | wasmi 0.38 |
|---|---:|---:|---:|
| one program, no recursion | 16 KiB | 16 KiB | 16 KiB |
| depth 8, no Wasm recursion | 16 KiB | 16 KiB | 16 KiB |
| depth 8, 1,000 Wasm frames per program | 16 KiB | 16 KiB | 16 KiB |
| depth 9 attempted (CallDepth), 1,000 Wasm frames per program | 16 KiB | 16 KiB | 16 KiB |

_Total run time: 551 s._
