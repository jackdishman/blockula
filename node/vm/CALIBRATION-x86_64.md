<!-- Floor-machine run (docs/PROGRAMS-PLAN.md S3): GitHub Actions x86_64 Linux (shared ubuntu-24.04 runner — not the plan.s dedicated Fly performance-4x, so a noisier first pass), --quick, via .github/workflows/calibrate-vm.yml. Generated 2026-10-20. Development numbers, never prices. The Mac run is CALIBRATION.md. -->

# Calibration run

- Machine: x86_64 (x86_64, linux); development numbers, never prices
- rustc: rustc 1.98.1 (48a229cea 2026-09-01)
- Build: `release/bench` profile, overflow checks on, debug assertions off
- Engines: soroban-wasmi 29.0.0, wasmi 2.0.0, wasmi 0.38.0
- Repetitions: 3 (+1 warmup), samples ≥ 2 ms, loop runs ≈ 10 ms

## 1. ns per fuel by operator class

Each case is a loop whose body is `k` copies of one unit dominated by one class (plus the loop's own ~9 fuel: `br_if`, a decrement, `br`, and wasmi 2.0's hidden 1 per loop entry). ns per fuel = (time of N iterations − time of 0) / N / (fuel per iteration), so instantiation and the transaction's fixed cost cancel. Fuel per unit = fuel per iteration / k, under each engine's mapping of schedule v1 (wasmi 0.38: its own IR fuel). Cells: **ns/fuel median [min–max]** (fuel per unit). Values above 2.5 ns/fuel are marked **bold**.

| class | case | soroban-wasmi | wasmi 2.0 | wasmi 0.38 |
|---|---:|---:|---:|---:|
| control | empty loop (the loop's own overhead) | 2.18 [2.18–2.18] (9.00) | 0.64 [0.64–0.64] (9.00) | **3.05 [3.03–3.09] (4.00)** |
| control | br to the next block end: (block (br 0)) | 2.15 [2.15–2.15] (1.14) | 1.92 [1.92–1.92] (1.14) | 2.28 [2.28–2.28] (1.06) |
| control | br_if not taken | **3.21 [3.21–3.23] (2.14)** | 1.76 [1.76–1.77] (2.14) | **57.5 [57.5–57.5] (0.06)** |
| control | br_if to the function's end, not taken | **4.80 [4.79–4.80] (1.14)** | 1.77 [1.77–1.77] (2.14) | **3.57 [3.57–3.57] (1.06)** |
| control | empty loops: (loop) | 2.44 [2.44–2.44] (1.14) | 2.02 [2.01–2.02] (1.14) | **4.19 [4.18–4.20] (1.06)** |
| control | if/else, unpredictable | 2.19 [2.19–2.19] (13.6) | 0.90 [0.90–0.90] (12.6) | **4.92 [4.91–4.93] (6.25)** |
| br_table | br_table 8 targets, fixed index | **3.73 [3.73–3.77] (2.56)** | 2.34 [2.33–2.34] (2.56) | **6.37 [6.34–6.41] (1.25)** |
| br_table | br_table 64 targets, random index | **2.69 [2.68–2.70] (13.2)** | 0.96 [0.96–0.96] (13.2) | **5.95 [5.94–5.96] (6.00)** |
| br_table | br_table 1,024 targets, random index | 2.29 [2.26–2.32] (20.0) | 0.68 [0.68–0.68] (20.0) | **4.01 [3.99–4.02] (9.00)** |
| br_table | br_table 4,096 targets, random index | 2.23 [2.23–2.23] (20.0) | 0.68 [0.68–0.69] (20.0) | **4.03 [4.02–4.03] (9.00)** |
| br_table | br_table 4,096 entries to one label, random index | 2.23 [2.21–2.23] (20.0) | 0.49 [0.49–0.49] (20.0) | **3.46 [3.46–3.48] (9.00)** |
| call | call () -> (), no locals | 1.10 [1.09–1.10] (12.6) | 0.88 [0.88–0.89] (11.6) | **5.91 [5.69–5.98] (3.25)** |
| call | call, 8 locals | 1.20 [1.20–1.20] (13.6) | 1.38 [1.37–1.38] (11.6) | **5.07 [5.03–5.10] (4.25)** |
| call | call, 64 locals | 0.84 [0.84–0.84] (20.6) | 1.77 [1.76–1.77] (11.6) | 2.06 [1.99–2.07] (11.2) |
| call | call, 256 locals | 0.62 [0.61–0.62] (44.6) | **3.06 [3.06–3.06] (11.6)** | 1.03 [1.03–1.03] (35.2) |
| call | call, 1,024 locals | 0.92 [0.92–0.93] (141) | **11.3 [11.3–11.4] (11.6)** | 1.04 [1.04–1.04] (131) |
| call | call, 32 params | 2.25 [2.25–2.26] (49.1) | 1.43 [1.43–1.43] (44.1) | **5.34 [5.33–5.35] (7.50)** |
| call | call, 32 params + 992 locals | 1.29 [1.29–1.29] (173) | **3.72 [3.70–3.75] (44.1)** | 1.18 [1.18–1.18] (132) |
| call | call i64 -> i64 | 1.33 [1.33–1.33] (17.6) | 1.04 [1.04–1.05] (16.6) | **6.44 [6.42–6.53] (4.25)** |
| call | recursion 64 deep, 1,024 locals a frame | 1.03 [1.03–1.04] (9,496) | **9.06 [9.06–9.18] (1,111)** | 1.22 [1.22–1.23] (8,647) |
| call | call_indirect, fixed index | 1.44 [1.43–1.44] (13.6) | 1.30 [1.29–1.30] (12.6) | **17.7 [17.7–17.7] (3.25)** |
| call | call_indirect, random index of 4,096 | 1.89 [1.89–1.90] (23.1) | 0.85 [0.85–0.85] (22.1) | **10.3 [10.2–10.3] (7.50)** |
| call | call_indirect, 1,024 locals | 0.97 [0.97–0.97] (142) | **11.3 [11.3–11.4] (12.6)** | 1.38 [1.37–1.38] (131) |
| i32 arith | i32 unary chain (clz ctz popcnt eqz) | **3.70 [3.68–3.71] (4.69)** | 1.48 [1.48–1.49] (4.69) | **4.68 [4.68–4.69] (4.25)** |
| i64 arith | i64 unary chain (clz ctz popcnt) | **3.56 [3.56–3.57] (3.69)** | 1.72 [1.72–1.72] (3.69) | **5.00 [5.00–5.00] (3.25)** |
| i32 arith | i32 binary chain, local operand | **3.04 [2.99–3.10] (24.8)** | 0.56 [0.56–0.56] (24.8) | **5.04 [4.91–5.19] (12.0)** |
| i64 arith | i64 binary chain, local operand | **3.52 [3.52–3.53] (24.8)** | 0.49 [0.49–0.50] (24.8) | **5.43 [5.31–5.60] (12.0)** |
| i32 arith | i32 binary chain, constant operand | **2.99 [2.98–2.99] (24.8)** | 0.61 [0.58–0.62] (24.8) | **4.80 [4.77–4.83] (12.0)** |
| i32 arith | i32 3-address: a = b op c | **4.22 [4.14–4.28] (8.56)** | 0.68 [0.68–0.68] (8.56) | **5.02 [5.01–5.02] (2.25)** |
| i64 arith | i64.mul chain | **3.07 [3.05–3.08] (2.17)** | 0.76 [0.76–0.76] (2.17) | **5.07 [5.06–5.08] (1.06)** |
| div/rem | i32.div_u by 1, chain | **3.33 [3.32–3.34] (2.17)** | 1.73 [1.73–1.73] (2.17) | **5.37 [5.36–5.43] (1.06)** |
| div/rem | i32.div_s by 1, chain | **3.32 [3.32–3.32] (2.17)** | 1.73 [1.73–1.73] (2.17) | **4.96 [4.96–4.97] (1.06)** |
| div/rem | i32.rem_u, xor chain | **2.77 [2.77–2.78] (4.34)** | 0.81 [0.80–0.81] (4.34) | **5.00 [4.99–5.01] (2.12)** |
| div/rem | i64.div_u by 1, chain | **3.36 [3.32–3.56] (2.17)** | 2.30 [2.30–2.31] (2.17) | **5.65 [5.64–5.78] (1.06)** |
| div/rem | i64.div_s by 1, chain | **3.48 [3.48–3.48] (2.17)** | 2.17 [2.17–2.17] (2.17) | **5.64 [5.64–5.67] (1.06)** |
| div/rem | i64.div_u by 3, xor chain | **3.00 [3.00–3.00] (4.34)** | 1.16 [1.16–1.16] (4.34) | **4.60 [4.60–4.60] (2.12)** |
| div/rem | i64.rem_u, xor chain | **3.15 [2.84–3.15] (4.34)** | 0.87 [0.87–0.88] (4.34) | **4.75 [4.75–4.75] (2.12)** |
| div/rem | i64.rem_s, xor chain | **2.91 [2.85–2.92] (4.34)** | 0.80 [0.80–0.80] (4.34) | **5.20 [5.19–5.21] (2.12)** |
| compare | i32 compare chain | **4.05 [4.05–4.06] (8.69)** | 0.71 [0.71–0.71] (8.69) | **4.82 [4.81–4.82] (4.25)** |
| compare | i64 compare chain | **4.85 [4.85–4.85] (6.69)** | 0.46 [0.46–0.46] (6.69) | **4.09 [4.09–4.36] (2.25)** |
| compare | i32.eqz chain | **3.81 [3.81–3.81] (1.17)** | 0.09 [0.09–0.09] (1.17) | **4.76 [4.75–4.76] (1.06)** |
| compare | wrap/extend chain | **3.37 [3.37–3.38] (4.69)** | 0.86 [0.86–0.86] (4.69) | **9.14 [9.13–9.16] (3.25)** |
| select | select chain | 2.48 [2.48–2.49] (3.34) | 0.64 [0.63–0.64] (3.34) | **3.63 [3.63–3.70] (1.12)** |
| select | select, unpredictable condition | **3.27 [3.26–3.28] (12.6)** | 0.56 [0.56–0.56] (12.6) | **5.91 [5.91–5.92] (4.25)** |
| locals | local.get/local.set copies | **2.62 [2.62–2.63] (6.56)** | 0.69 [0.69–0.70] (6.56) | **2.70 [2.68–2.71] (2.25)** |
| locals | local.tee chain | 2.16 [2.16–2.17] (4.69) | 1.07 [1.06–1.07] (4.69) | **2.53 [2.51–2.55] (2.31)** |
| locals | far locals (1,024 declared) | **2.69 [2.69–2.71] (4.56)** | 0.80 [0.80–0.80] (4.56) | **3.23 [3.23–3.24] (2.25)** |
| globals | global.get/global.set copies | **3.65 [3.63–3.69] (4.56)** | 1.48 [1.47–1.48] (4.56) | **5.93 [5.92–5.96] (4.25)** |
| globals | global increment | **2.74 [2.60–3.08] (4.56)** | 1.22 [1.22–1.22] (4.56) | **6.00 [5.82–6.00] (3.25)** |
| globals | global.get; drop | 2.38 [2.38–2.38] (2.28) | 0.79 [0.79–0.79] (2.28) | **5.69 [5.65–5.94] (1.12)** |
| load | i32.load chain, one address | **4.38 [4.37–4.38] (1.17)** | 1.67 [1.67–1.67] (1.17) | **5.36 [5.36–5.36] (1.06)** |
| load | narrow loads chain, one address | **4.07 [4.07–4.09] (5.69)** | 1.56 [1.55–1.56] (5.69) | **5.78 [5.77–5.87] (5.25)** |
| load | pointer chase, 64 KiB, random | **4.37 [4.37–4.39] (1.17)** | **3.43 [3.43–3.43] (1.17)** | **5.39 [5.39–5.40] (1.06)** |
| load | pointer chase, 2 MiB, random | **13.7 [13.6–13.8] (1.17)** | **13.4 [13.4–13.5] (1.17)** | **15.3 [15.3–15.4] (1.06)** |
| load | random independent i64 loads, 2 MiB | **2.72 [2.71–2.73] (10.6)** | 0.77 [0.77–0.77] (10.6) | **5.37 [5.36–5.37] (4.25)** |
| store | i64.store, one address | **2.91 [2.91–2.91] (3.28)** | 0.67 [0.67–0.68] (3.28) | **4.70 [4.70–4.70] (1.12)** |
| store | narrow stores, one address | **2.78 [2.77–2.78] (9.56)** | 0.68 [0.63–0.75] (9.56) | **4.89 [4.87–4.91] (3.25)** |
| store | random i64 stores, 2 MiB | **4.06 [4.05–4.06] (10.6)** | 0.78 [0.78–0.81] (10.6) | **5.05 [5.03–5.07] (4.25)** |
| memory | memory.size; drop | 2.41 [2.38–2.47] (2.28) | 0.93 [0.93–0.93] (2.28) | **3.32 [3.32–3.32] (1.12)** |

**Worst cases per engine** (median ns/fuel; the five highest):

| engine | case | ns/fuel | fuel/unit | vs 2.5 ns |
|---|---:|---:|---:|---:|
| soroban-wasmi | pointer chase, 2 MiB, random | 13.7 [13.6–13.8] | 1.17 | 5.5× |
|  | i64 compare chain | 4.85 [4.85–4.85] | 6.69 | 1.9× |
|  | br_if to the function's end, not taken | 4.80 [4.79–4.80] | 1.14 | 1.9× |
|  | i32.load chain, one address | 4.38 [4.37–4.38] | 1.17 | 1.8× |
|  | pointer chase, 64 KiB, random | 4.37 [4.37–4.39] | 1.17 | 1.7× |
| wasmi 2.0 | pointer chase, 2 MiB, random | 13.4 [13.4–13.5] | 1.17 | 5.4× |
|  | call_indirect, 1,024 locals | 11.3 [11.3–11.4] | 12.6 | 4.5× |
|  | call, 1,024 locals | 11.3 [11.3–11.4] | 11.6 | 4.5× |
|  | recursion 64 deep, 1,024 locals a frame | 9.06 [9.06–9.18] | 1,111 | 3.6× |
|  | call, 32 params + 992 locals | 3.72 [3.70–3.75] | 44.1 | 1.5× |
| wasmi 0.38 | br_if not taken | 57.5 [57.5–57.5] | 0.06 | 23.0× |
|  | call_indirect, fixed index | 17.7 [17.7–17.7] | 3.25 | 7.1× |
|  | pointer chase, 2 MiB, random | 15.3 [15.3–15.4] | 1.06 | 6.1× |
|  | call_indirect, random index of 4,096 | 10.3 [10.2–10.3] | 7.50 | 4.1× |
|  | wrap/extend chain | 9.14 [9.13–9.16] | 3.25 | 3.7× |

**soroban-wasmi and wasmi 2.0 more than 2× apart** (ns/fuel, where either is above 0.5):

| case | soroban-wasmi | wasmi 2.0 | ratio |
|---|---:|---:|---:|
| empty loop (the loop's own overhead) | 2.18 | 0.64 | 3.4× |
| br_if to the function's end, not taken | 4.80 | 1.77 | 2.7× |
| if/else, unpredictable | 2.19 | 0.90 | 2.4× |
| br_table 64 targets, random index | 2.69 | 0.96 | 2.8× |
| br_table 1,024 targets, random index | 2.29 | 0.68 | 3.4× |
| br_table 4,096 targets, random index | 2.23 | 0.68 | 3.3× |
| br_table 4,096 entries to one label, random index | 2.23 | 0.49 | 4.6× |
| call, 64 locals | 0.84 | 1.77 | 2.1× |
| call, 256 locals | 0.62 | 3.06 | 5.0× |
| call, 1,024 locals | 0.92 | 11.3 | 12.4× |
| call, 32 params + 992 locals | 1.29 | 3.72 | 2.9× |
| recursion 64 deep, 1,024 locals a frame | 1.03 | 9.06 | 8.8× |
| call_indirect, random index of 4,096 | 1.89 | 0.85 | 2.2× |
| call_indirect, 1,024 locals | 0.97 | 11.3 | 11.7× |
| i32 unary chain (clz ctz popcnt eqz) | 3.70 | 1.48 | 2.5× |
| i64 unary chain (clz ctz popcnt) | 3.56 | 1.72 | 2.1× |
| i32 binary chain, local operand | 3.04 | 0.56 | 5.4× |
| i64 binary chain, local operand | 3.52 | 0.49 | 7.2× |
| i32 binary chain, constant operand | 2.99 | 0.61 | 4.9× |
| i32 3-address: a = b op c | 4.22 | 0.68 | 6.2× |
| i64.mul chain | 3.07 | 0.76 | 4.1× |
| i32.rem_u, xor chain | 2.77 | 0.81 | 3.4× |
| i64.div_u by 3, xor chain | 3.00 | 1.16 | 2.6× |
| i64.rem_u, xor chain | 3.15 | 0.87 | 3.6× |
| i64.rem_s, xor chain | 2.91 | 0.80 | 3.6× |
| i32 compare chain | 4.05 | 0.71 | 5.7× |
| i64 compare chain | 4.85 | 0.46 | 10.5× |
| i32.eqz chain | 3.81 | 0.09 | 43.2× |
| wrap/extend chain | 3.37 | 0.86 | 3.9× |
| select chain | 2.48 | 0.64 | 3.9× |
| select, unpredictable condition | 3.27 | 0.56 | 5.8× |
| local.get/local.set copies | 2.62 | 0.69 | 3.8× |
| local.tee chain | 2.16 | 1.07 | 2.0× |
| far locals (1,024 declared) | 2.69 | 0.80 | 3.4× |
| global.get/global.set copies | 3.65 | 1.48 | 2.5× |
| global increment | 2.74 | 1.22 | 2.2× |
| global.get; drop | 2.38 | 0.79 | 3.0× |
| i32.load chain, one address | 4.38 | 1.67 | 2.6× |
| narrow loads chain, one address | 4.07 | 1.56 | 2.6× |
| random independent i64 loads, 2 MiB | 2.72 | 0.77 | 3.5× |
| i64.store, one address | 2.91 | 0.67 | 4.3× |
| narrow stores, one address | 2.78 | 0.68 | 4.1× |
| random i64 stores, 2 MiB | 4.06 | 0.78 | 5.2× |
| memory.size; drop | 2.41 | 0.93 | 2.6× |

## 2. Translation and validation, ns per byte

Cold: each load is `BlockEngine::load` (a structural read of memory and data, then the engine's `Module::new`: its own parse, validation and translation) on a fresh block engine made outside the clock, as a block loads every declared code into a fresh engine (4.3). `validate` is upload validation (`validate.rs`, wasmparser 0.228 plus our rules). CPU caches are warm (the same bytes are loaded repeatedly), so these are lower bounds for a cold machine. *Implied fuel* = time / 2.5 ns. **Implied LOAD_PER_BYTE** = (load time / 2.5 ns − LOAD_BASE 2,000) / bytes, against schedule v1's 10; **implied LOAD_BASE** comes from the smallest modules.

| module | bytes | validate µs (ns/B) | soroban-wasmi load µs (ns/B) | wasmi 2.0 load µs (ns/B) | wasmi 0.38 load µs (ns/B) | soroban-wasmi implied LOAD_PER_BYTE | wasmi 2.0 implied LOAD_PER_BYTE | wasmi 0.38 implied LOAD_PER_BYTE |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| guest counter.wasm | 565 | 14.2 (25.1) | 16.8 (29.8) | 34.7 (61.4) | 23.2 (41.1) | 8.37 | **21.0** | **12.9** |
| guest token.wasm | 2,467 | 44.7 (18.1) | 51.3 (20.8) | 125 (50.6) | 74.0 (30.0) | 7.51 | **19.4** | **11.2** |
| guest escrow.wasm | 3,389 | 55.9 (16.5) | 61.4 (18.1) | 160 (47.2) | 88.9 (26.2) | 6.66 | **18.3** | 9.90 |
| guest router.wasm | 1,484 | 25.9 (17.5) | 30.7 (20.7) | 63.2 (42.6) | 41.3 (27.8) | 6.93 | **15.7** | 9.78 |
| minimal program | 51 | 2.44 (47.9) | 2.76 (54.2) | 4.28 (83.9) | 3.04 (59.7) | 0.00 | 0.00 | 0.00 |
| minimal program + 20 imports | 621 | 16.8 (27.0) | 17.1 (27.6) | 22.3 (35.9) | 19.0 (30.6) | 7.80 | **11.1** | 9.03 |
| one function, straight-line i64 arithmetic | 262,141 | 2,004 (7.65) | 1,425 (5.44) | 5,071 (19.3) | 2,570 (9.80) | 2.17 | 7.73 | 3.91 |
| 5040 small functions (≈40 B bodies) | 262,137 | 3,566 (13.6) | 4,227 (16.1) | 7,610 (29.0) | 5,611 (21.4) | 6.44 | **11.6** | 8.55 |
| 6094 functions with 1,024 locals each | 262,099 | 3,980 (15.2) | 5,013 (19.1) | 25,090 (95.7) | 6,713 (25.6) | 7.64 | **38.3** | **10.2** |
| br_table, 2 × 131030 targets to one label | 262,143 | 2,138 (8.16) | 10,762 (41.1) | 2,159 (8.24) | 2,060 (7.86) | **16.4** | 3.29 | 3.14 |
| br_table, 12 × 4,096 distinct nested targets | 244,343 | 3,345 (13.7) | 4,311 (17.6) | 5,402 (22.1) | 3,299 (13.5) | 7.05 | 8.84 | 5.39 |
| 87361 nested blocks | 262,142 | 5,138 (19.6) | 3,900 (14.9) | 9,233 (35.2) | 5,110 (19.5) | 5.94 | **14.1** | 7.79 |
| 32760 if/else | 262,143 | 4,145 (15.8) | 3,600 (13.7) | 14,134 (53.9) | 6,058 (23.1) | 5.49 | **21.6** | 9.24 |
| 28942 distinct i64 constants | 258,321 | 946 (3.66) | 2,628 (10.2) | 1,180 (4.57) | 672 (2.60) | 4.06 | 1.82 | 1.03 |
| 131035 B of data + as much code (32 pages) | 262,143 | 1,378 (5.26) | 1,151 (4.39) | 1,715 (6.54) | 970 (3.70) | 1.75 | 2.61 | 1.47 |
| 14980 distinct function types | 223,362 | 11,313 (50.6) | 9,955 (44.6) | 14,282 (63.9) | 7,769 (34.8) | **17.8** | **25.6** | **13.9** |
| 87364 identical function types | 262,144 | 23,063 (88.0) | 14,973 (57.1) | 25,864 (98.7) | 14,173 (54.1) | **22.8** | **39.5** | **21.6** |
| 22105 exports of one function | 254,196 | 8,347 (32.8) | 8,386 (33.0) | 8,578 (33.7) | 8,482 (33.4) | **13.2** | **13.5** | **13.3** |
| 131019 calls | 262,143 | 3,518 (13.4) | 3,265 (12.5) | 7,160 (27.3) | 4,762 (18.2) | 4.98 | **10.9** | 7.26 |
| 43679 far local.get/set pairs (1,024 locals) | 262,140 | 2,328 (8.88) | 1,475 (5.63) | 4,319 (16.5) | 2,281 (8.70) | 2.24 | 6.58 | 3.47 |
| 21 × (4,000 local.gets, local.set over them, 3,999 adds) | 252,148 | 2,934 (11.6) | 2,181 (8.65) | 12,248 (48.6) | 5,527 (21.9) | 3.45 | **19.4** | 8.76 |
| 16 × (4,000-deep stack, 1,000 local.sets) | 256,064 | 2,973 (11.6) | 2,091 (8.17) | 7,464 (29.1) | 3,132 (12.2) | 3.26 | **11.7** | 4.88 |
| 43680 dead-code units after unreachable | 262,140 | 2,543 (9.70) | 1,600 (6.10) | 3,110 (11.9) | 1,623 (6.19) | 2.43 | 4.74 | 2.47 |
| 256 globals, 52160 global.get/set pairs | 262,144 | 2,467 (9.41) | 1,537 (5.86) | 4,437 (16.9) | 2,109 (8.05) | 2.34 | 6.76 | 3.21 |

Spread, and the drop of an engine holding the module (less an empty engine's drop): part of the block's cost, unpriced.

| module | soroban-wasmi load [min–max] µs | soroban-wasmi drop µs | wasmi 2.0 load [min–max] µs | wasmi 2.0 drop µs | wasmi 0.38 load [min–max] µs | wasmi 0.38 drop µs | validate [min–max] µs |
|---|---:|---:|---:|---:|---:|---:|---:|
| guest counter.wasm | [16.8–16.9] | 2.46 | [32.4–39.4] | 0.89 | [22.2–23.9] | 2.33 | [14.1–14.3] |
| guest token.wasm | [50.8–52.0] | 2.99 | [124–126] | 4.00 | [73.8–74.6] | 3.53 | [44.6–45.0] |
| guest escrow.wasm | [61.0–63.1] | 4.42 | [158–161] | 5.45 | [88.6–90.4] | 1.92 | [55.7–56.3] |
| guest router.wasm | [30.2–30.8] | 4.01 | [62.7–63.5] | 4.01 | [40.8–41.8] | 1.29 | [25.5–28.1] |
| minimal program | [2.74–2.79] | 1.35 | [4.27–4.28] | 1.49 | [3.04–3.06] | 1.40 | [2.41–2.52] |
| minimal program + 20 imports | [17.0–17.4] | 2.33 | [21.8–22.3] | 1.06 | [18.0–22.2] | 2.70 | [16.7–16.9] |
| one function, straight-line i64 arithmetic | [1,421–1,427] | 36.9 | [4,949–5,242] | 50.8 | [2,540–2,575] | 4.03 | [1,989–2,007] |
| 5040 small functions (≈40 B bodies) | [4,223–4,260] | 4.96 | [7,588–7,626] | 117 | [5,607–5,649] | 117 | [3,562–3,573] |
| 6094 functions with 1,024 locals each | [5,000–5,017] | 4.19 | [25,000–25,110] | 143 | [6,713–6,718] | 141 | [3,968–3,982] |
| br_table, 2 × 131030 targets to one label | [10,719–11,031] | 58.0 | [2,154–2,184] | 5.85 | [2,060–2,061] | 5.14 | [2,137–2,151] |
| br_table, 12 × 4,096 distinct nested targets | [4,303–4,313] | 3.24 | [5,389–5,406] | 4.28 | [3,256–3,308] | 4.40 | [3,343–3,350] |
| 87361 nested blocks | [3,886–3,906] | 3.42 | [9,187–9,238] | 186 | [5,098–5,144] | 238 | [5,107–5,148] |
| 32760 if/else | [3,589–3,606] | 12.3 | [14,125–14,134] | 139 | [6,057–6,074] | 42.2 | [4,130–4,325] |
| 28942 distinct i64 constants | [2,627–2,649] | 135 | [1,179–1,183] | 5.47 | [672–673] | 4.77 | [934–961] |
| 131035 B of data + as much code (32 pages) | [1,138–1,157] | 3.15 | [1,715–1,726] | 3.95 | [968–975] | 3.74 | [1,375–1,383] |
| 14980 distinct function types | [9,938–9,965] | 454 | [14,152–14,316] | 190 | [7,761–7,828] | 240 | [11,299–11,314] |
| 87364 identical function types | [14,905–14,991] | 4.11 | [25,848–25,934] | 4.95 | [14,169–14,207] | 4.29 | [23,037–23,174] |
| 22105 exports of one function | [8,341–8,392] | 607 | [8,481–8,599] | 608 | [8,439–8,518] | 609 | [8,244–8,363] |
| 131019 calls | [3,258–3,286] | 3.14 | [7,131–7,235] | 77.9 | [4,760–4,766] | 4.02 | [3,512–3,524] |
| 43679 far local.get/set pairs (1,024 locals) | [1,474–1,483] | 3.05 | [4,299–4,337] | 4.74 | [2,274–2,292] | 4.24 | [2,321–2,358] |
| 21 × (4,000 local.gets, local.set over them, 3,999 adds) | [2,157–2,186] | 17.0 | [12,245–12,264] | 79.8 | [5,522–5,543] | 5.27 | [2,933–2,959] |
| 16 × (4,000-deep stack, 1,000 local.sets) | [2,073–2,114] | 3.10 | [7,451–8,071] | 5.19 | [3,121–3,179] | 4.76 | [2,971–2,976] |
| 43680 dead-code units after unreachable | [1,598–1,623] | 4.43 | [3,058–3,174] | 3.75 | [1,622–1,626] | 5.08 | [2,536–2,550] |
| 256 globals, 52160 global.get/set pairs | [1,534–1,540] | 3.17 | [4,424–4,438] | 54.6 | [2,094–2,138] | 4.55 | [2,463–2,473] |

**Summary.** Schedule v1: LOAD_BASE 2,000, LOAD_PER_BYTE 10; upload 50,000 + 20 per byte.

| engine | worst ns/B, any size | worst ns/B, modules > 100 KB | implied LOAD_PER_BYTE (worst > 100 KB) | minimal module's load (implied LOAD_BASE) | worst upload (validate + load), modules > 100 KB |
|---|---:|---:|---:|---:|---:|
| soroban-wasmi | 87364 identical function types (57.1 ns/B) | 87364 identical function types (57.1 ns/B) | 22.8 | 2.76 µs = 1,106 fuel | 87364 identical function types (145 ns/B = 58.0 fuel/B) |
| wasmi 2.0 | 87364 identical function types (98.7 ns/B) | 87364 identical function types (98.7 ns/B) | 39.5 | 4.28 µs = 1,712 fuel | 87364 identical function types (187 ns/B = 74.7 fuel/B) |
| wasmi 0.38 | minimal program (59.7 ns/B) | 87364 identical function types (54.1 ns/B) | 21.6 | 3.04 µs = 1,218 fuel | 87364 identical function types (142 ns/B = 56.8 fuel/B) |

## 3. Instantiation

Per module, three ways: **raw** (`BlockEngine::invoke` of an empty `bk_noop` with a host that refuses every import: the engine's store, linking, instance and drop), **tx** (a top-level transaction calling `bk_noop` through `execute`: also the executor's fixed cost) and **call_v1** (each call of a loop `call_v1`s `bk_noop` on the module: the repeatable pattern, charged `call` 20,100 + `instantiate_fuel`). *Schedule* = `instantiate_fuel(pages, data)` (1,000 + 2,000 per page + 1 per data byte), which counts no imports, functions, exports, globals, tables or segments. Time ÷ 2.5 ns is the fuel it would need.

| module | bytes | schedule fuel | soroban-wasmi raw µs | soroban-wasmi tx µs (ns/fuel) | wasmi 2.0 raw µs | wasmi 2.0 tx µs (ns/fuel) | wasmi 0.38 raw µs | wasmi 0.38 tx µs (ns/fuel) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 page, no imports | 52 | 3,000 | 1.36 | 2.49 (0.83) | 1.59 | 2.69 (0.89) | 1.39 | 2.51 (0.84) |
| 1 page, 20 imports | 622 | 3,000 | 5.81 | 6.82 (2.27) | 6.47 | **7.58 (2.53)** | 6.26 | 7.41 (2.47) |
| 32 pages | 52 | 65,000 | 44.8 | 35.2 (0.54) | 34.3 | 35.2 (0.54) | 33.2 | 34.9 (0.54) |
| 32 pages, 20 imports, + touch every 4 KiB | 667 | 65,000 | 38.4 | 39.8 (0.61) | 39.2 | 40.6 (0.62) | 38.8 | 40.2 (0.62) |
| data: 1 segment of 120,000 B | 240,075 | 125,000 | 4.70 | 5.79 (0.05) | 5.16 | 6.27 (0.05) | 4.97 | 6.07 (0.05) |
| data: 1,000 segments of 1 B | 8,001 | 4,000 | 18.2 | **19.6 (4.90)** | 24.6 | **25.8 (6.44)** | 23.6 | **24.5 (6.11)** |
| data: 1,000 segments of 120 B | 246,999 | 125,000 | 20.1 | 21.5 (0.17) | 26.4 | 27.9 (0.22) | 21.7 | 23.1 (0.18) |
| 256 globals | 1,337 | 3,000 | 5.58 | 6.23 (2.07) | 9.04 | **10.2 (3.40)** | 5.15 | 6.76 (2.25) |
| table of 4,096, 1,000 element segments | 10,050 | 3,000 | 42.8 | **44.7 (14.9)** | 93.5 | **94.0 (31.3)** | 96.2 | **97.2 (32.4)** |
| 5040 functions | 262,137 | 3,000 | 62.4 | **63.9 (21.3)** | 103 | **104 (34.7)** | 64.0 | **67.9 (22.6)** |
| 22104 exports | 254,194 | 3,000 | 9,966 | **9,984 (3,326)** | 10,098 | **10,186 (3,394)** | 10,129 | **10,235 (3,409)** |
| all: 20 imports, 32 pages, 256 globals, table + 1,000 elems, 2,001 funcs, 1,000 data, 12265 exports | 258,924 | 66,000 | 5,395 | **5,312 (80.5)** | 5,669 | **5,711 (86.5)** | 5,602 | **5,565 (84.3)** |

**Repeated instantiation through `call_v1`** (per call: the measured fuel, then time and ns per fuel):

| callee | schedule fuel per call | soroban-wasmi fuel: µs (ns/fuel) | wasmi 2.0 fuel: µs (ns/fuel) | wasmi 0.38 fuel: µs (ns/fuel) |
|---|---:|---:|---:|---:|
| 1 page, no imports | 23,100 | 23,130: 2.10 (0.09) | 23,129: 2.36 (0.10) | 23,107: 2.13 (0.09) |
| 1 page, 20 imports | 23,100 | 23,130: 6.50 (0.28) | 23,129: 7.28 (0.31) | 23,107: 7.05 (0.31) |
| 32 pages | 85,100 | 85,130: 34.2 (0.40) | 85,129: 35.0 (0.41) | 85,107: 34.4 (0.40) |
| 32 pages, 20 imports, + touch every 4 KiB | 85,100 | 85,130: 39.4 (0.46) | 85,129: 40.4 (0.47) | 85,107: 40.0 (0.47) |
| data: 1 segment of 120,000 B | 145,100 | 145,130: 5.39 (0.04) | 145,129: 6.02 (0.04) | 145,107: 5.77 (0.04) |
| data: 1,000 segments of 1 B | 24,100 | 24,130: 19.2 (0.79) | 24,129: 25.6 (1.06) | 24,107: 21.8 (0.91) |
| data: 1,000 segments of 120 B | 145,100 | 145,130: 21.2 (0.15) | 145,129: 27.4 (0.19) | 145,107: 23.0 (0.16) |
| 256 globals | 23,100 | 23,130: 5.84 (0.25) | 23,129: 10.8 (0.47) | 23,107: 6.46 (0.28) |
| table of 4,096, 1,000 element segments | 23,100 | 23,130: 44.6 (1.93) | 23,129: **94.3 (4.08)** | 23,107: **97.2 (4.21)** |
| 5040 functions | 23,100 | 23,130: **63.6 (2.75)** | 23,129: **105 (4.52)** | 23,107: **68.1 (2.95)** |
| 22104 exports | 23,100 | 23,130: **10,041 (434)** | 23,129: **10,083 (436)** | 23,107: **10,015 (433)** |
| all: 20 imports, 32 pages, 256 globals, table + 1,000 elems, 2,001 funcs, 1,000 data, 12265 exports | 86,100 | 86,130: **5,565 (64.6)** | 86,129: **5,707 (66.3)** | 86,107: **5,568 (64.7)** |

**Touching the memory**: a transaction whose method stores to every 4 KiB of its 2 MiB, so the zeroed pages the allocator handed out lazily are faulted in (the cost an instantiation's per-page price must cover):

| module | soroban-wasmi tx fuel: µs (ns/fuel) | wasmi 2.0 tx fuel: µs (ns/fuel) | wasmi 0.38 tx fuel: µs (ns/fuel) |
|---|---:|---:|---:|
| 32 pages, 20 imports, + touch every 4 KiB | 71,146: 56.1 (0.79) (+16.3 µs over the empty tx) | 71,145: 43.3 (0.61) (+2.70 µs over the empty tx) | 67,050: 50.4 (0.75) (+10.2 µs over the empty tx) |

## 4. Host functions (ABI v1)

Each case loops one host function N times inside a program (`bk_<case>`) through the real executor and `MemHost`; per call = (time of N − time of 0) / N, so the call's Wasm side (its arguments, the `call` itself at 10 fuel, the loop's ~9) is included, as a program would pay it. Cases bounded per transaction (events, keys written, bytes written) run that many calls a transaction. Fuel per call is measured (the schedule's charge plus that Wasm side). `MemHost` is a `BTreeMap`, not the node's state: storage costs here are the overlay's and the host's, not redb's or the tree's (those are commit-time).

| function, case | fuel per call | soroban-wasmi ns/call (ns/fuel) | wasmi 2.0 ns/call (ns/fuel) | wasmi 0.38 ns/call (ns/fuel) |
|---|---:|---:|---:|---:|
| `input_len_v1`  | 120 / 105 | 83.6 (0.70) | 65.7 (0.55) | 74.1 (0.71) |
| `input_read_v1` 44 B input | 164 / 149 | 81.9 (0.50) | 71.2 (0.43) | 79.6 (0.53) |
| `input_read_v1` 32 KiB input | 32,888 / 32,873 | 761 (0.02) | 752 (0.02) | 762 (0.02) |
| `return_data_v1` 0 B | 121 / 105 | 86.1 (0.71) | 75.9 (0.63) | 84.4 (0.80) |
| `return_data_v1` 4 KiB | 4,217 / 4,201 | 198 (0.05) | 183 (0.04) | 195 (0.05) |
| `debug_log_v1` 0 B | 121 / 105 | 83.9 (0.69) | 74.2 (0.61) | 79.3 (0.76) |
| `debug_log_v1` 64 KiB | 65,657 / 65,641 | 84.2 (0.00) | 74.2 (0.00) | 79.2 (0.00) |
| `abort_v1` 256 B message (one per tx: tx time − an empty tx's) | 343 / 350 | 846 (2.47) | 647 (1.89) | **955 (2.73)** |
| `storage_read_v1` absent, 32 B key | 1,156 / 1,137 | 145 (0.13) | 122 (0.11) | 131 (0.11) |
| `storage_read_v1` 32 B key, 8 B value | 1,164 / 1,145 | 178 (0.15) | 166 (0.14) | 176 (0.15) |
| `storage_read_v1` 128 B key, 8 B value | 1,260 / 1,241 | 181 (0.14) | 166 (0.13) | 180 (0.14) |
| `storage_read_v1` 32 B key, 16 KiB value | 17,540 / 17,521 | 805 (0.05) | 792 (0.05) | 792 (0.05) |
| `storage_write_v1` overwrite, 32 B key, 0 B value | 5,444 / 5,425 | 209 (0.04) | 190 (0.03) | 183 (0.03) |
| `storage_write_v1` overwrite, 128 B key, 8 B value | 6,484 / 6,465 | 247 (0.04) | 204 (0.03) | 218 (0.03) |
| `storage_write_v1` overwrite, 32 B key, 16 KiB value (64 a tx) | 169,284 / 169,265 | 2,126 (0.01) | 2,275 (0.01) | 2,316 (0.01) |
| `storage_write_v1` new keys, 32 B key, 8 B value (256 a tx) | 5,527 / 5,506 | 593 (0.11) | 554 (0.10) | 574 (0.10) |
| `storage_remove_v1` absent | 3,122 / 3,105 | 124 (0.04) | 116 (0.04) | 122 (0.04) |
| `storage_remove_v1` present, 16 KiB value (256 a tx) | 3,125 / 3,106 | 1,857 (0.59) | 1,840 (0.59) | 1,845 (0.59) |
| `self_address_v1`  | 320 / 305 | 80.5 (0.25) | 67.5 (0.21) | 77.5 (0.25) |
| `caller_v1`  | 320 / 305 | 80.4 (0.25) | 67.7 (0.21) | 78.1 (0.26) |
| `value_v1`  | 320 / 305 | 82.2 (0.26) | 69.4 (0.22) | 74.5 (0.24) |
| `block_height_v1`  | 320 / 305 | 82.5 (0.26) | 74.0 (0.23) | 74.9 (0.25) |
| `self_balance_v1`  | 320 / 305 | 96.2 (0.30) | 85.6 (0.27) | 90.5 (0.30) |
| `transfer_v1` to an existing account | 10,123 / 10,105 | 152 (0.01) | 131 (0.01) | 139 (0.01) |
| `transfer_v1` opening an account (OPEN_DEDUCT) | 10,126 / 10,106 | 927 (0.09) | 1,005 (0.10) | 944 (0.09) |
| `call_v1` callee 1 page, empty method | 23,130 / 23,129 / 23,107 | 2,863 (0.12) | 3,223 (0.14) | 2,889 (0.13) |
| `call_v1` callee 1 page, 32 KiB input | 55,898 / 55,897 / 55,875 | 3,650 (0.07) | 4,012 (0.07) | 3,812 (0.07) |
| `call_v1` callee 32 pages, empty method | 85,130 / 85,129 / 85,107 | 35,538 (0.42) | 35,589 (0.42) | 35,419 (0.42) |
| `try_call_v1` callee traps (layer dropped) | 23,130 / 23,107 | 2,887 (0.12) | 3,201 (0.14) | 2,873 (0.12) |
| `last_return_v1` 0 B | 120 / 105 | 82.3 (0.69) | 70.9 (0.59) | 80.9 (0.77) |
| `last_return_v1` 4 KiB | 4,216 / 4,201 | 134 (0.03) | 123 (0.03) | 134 (0.03) |
| `last_return_len_v1`  | 120 / 105 | 81.2 (0.68) | 64.4 (0.54) | 74.3 (0.71) |
| `last_abort_code_v1`  | 120 / 105 | 82.3 (0.69) | 64.7 (0.54) | 74.3 (0.71) |
| `emit_v1` 8 B name, 0 B data (64 a tx) | 2,187 / 2,169 | 201 (0.09) | 181 (0.08) | 195 (0.09) |
| `emit_v1` 32 B name, 16,352 B data (1 a tx) | 133,195 / 133,177 | 1,711 (0.01) | 1,447 (0.01) | 1,260 (0.01) |

**Per byte**, from the two sizes of each function (Δtime / Δbytes, against Δfuel / Δbytes):

| function: sizes | fuel per byte | soroban-wasmi ns/B (ns/fuel) | wasmi 2.0 ns/B (ns/fuel) | wasmi 0.38 ns/B (ns/fuel) |
|---|---:|---:|---:|---:|
| `input_read_v1`: 44 B input → 32 KiB input | 1.00 | 0.02 (0.02) | 0.02 (0.02) | 0.02 (0.02) |
| `return_data_v1`: 0 B → 4 KiB | 1.00 | 0.03 (0.03) | 0.03 (0.03) | 0.03 (0.03) |
| `debug_log_v1`: 0 B → 64 KiB | 1.00 | 0.00 (0.00) | -0.00 (-0.00) | -0.00 (-0.00) |
| `storage_read_v1`: 32 B key, 8 B value → 32 B key, 16 KiB value | 1.00 | 0.04 (0.04) | 0.04 (0.04) | 0.04 (0.04) |
| `storage_read_v1`: 32 B key, 8 B value → 128 B key, 8 B value | 1.00 | 0.03 (0.03) | 0.01 (0.01) | 0.03 (0.03) |
| `storage_write_v1`: overwrite, 32 B key, 0 B value → overwrite, 32 B key, 16 KiB value (64 a tx) | 10.0 | 0.12 (0.01) | 0.13 (0.01) | 0.13 (0.01) |
| `call_v1`: callee 1 page, empty method → callee 1 page, 32 KiB input | 1.00 | 0.02 (0.02) | 0.02 (0.02) | 0.03 (0.03) |
| `last_return_v1`: 0 B → 4 KiB | 1.00 | 0.01 (0.01) | 0.01 (0.01) | 0.01 (0.01) |
| `emit_v1`: 8 B name, 0 B data (64 a tx) → 32 B name, 16,352 B data (1 a tx) | 8.00 | 0.09 (0.01) | 0.08 (0.01) | 0.07 (0.01) |

## 5. An adversarial block

A `max_block_fuel` block is 60,000,000 fuel (3 transactions of `program_max_tx_fuel`, 20,000,000 each). The cost of a block made entirely of one pattern is **60M × the pattern's ns/fuel**: that is how the design sets the budget (60M × 2.5 ns = 150 ms). Each ns/fuel below is measured as in sections 1–4 (a bounded loop, not a literal 60M-fuel run, which would just be the same number × 60M); the projection is then checked against one real end-to-end 60M block per engine at the end. 150 ms is the budget; a block over it is **bold**.

| engine | block of this pattern | ns/fuel [min–max] | projected ms [min–max] | vs 150 ms |
|---|---:|---:|---:|---:|
| soroban-wasmi | compute: pointer chase, 2 MiB, random | 13.7 [13.6–13.8] | 824 [815–826] | **5×** |
| soroban-wasmi | compute: i64 compare chain | 4.85 [4.85–4.85] | 291 [291–291] | **2×** |
| wasmi 2.0 | compute: pointer chase, 2 MiB, random | 13.4 [13.4–13.5] | 805 [803–809] | **5×** |
| wasmi 2.0 | compute: call_indirect, 1,024 locals | 11.3 [11.3–11.4] | 681 [678–682] | **5×** |
| wasmi 0.38 | compute: br_if not taken | 57.5 [57.5–57.5] | 3450 [3450–3453] | **23×** |
| wasmi 0.38 | compute: call_indirect, fixed index | 17.7 [17.7–17.7] | 1061 [1061–1062] | **7×** |
| soroban-wasmi | instantiation: call_v1 loop to the many-exports callee | 438 [437–440] | 26284 [26237–26399] | **175×** |
| wasmi 2.0 | instantiation: call_v1 loop to the many-exports callee | 433 [433–434] | 25988 [25964–26053] | **173×** |
| wasmi 0.38 | instantiation: call_v1 loop to the many-exports callee | 433 [433–434] | 25987 [25974–26056] | **173×** |
| soroban-wasmi | instantiation: call_v1 loop to the 5,040-function callee | 2.75 [2.74–2.75] | 165 [165–165] | **1×** |
| wasmi 2.0 | instantiation: call_v1 loop to the 5,040-function callee | 4.52 [4.51–4.52] | 271 [271–271] | **2×** |
| wasmi 0.38 | instantiation: call_v1 loop to the 5,040-function callee | 2.91 [2.90–2.92] | 175 [174–175] | **1×** |
| soroban-wasmi | instantiation: call_v1 loop to a 32-page callee | 0.42 [0.41–0.42] | 25 [25–25] | 0× |
| wasmi 2.0 | instantiation: call_v1 loop to a 32-page callee | 0.42 [0.42–0.42] | 25 [25–25] | 0× |
| wasmi 0.38 | instantiation: call_v1 loop to a 32-page callee | 0.42 [0.42–0.42] | 25 [25–25] | 0× |
| soroban-wasmi | storage_write storm: 64 × 16 KiB overwrites a tx | 0.01 [0.01–0.01] | 1 [1–1] | 0× |
| wasmi 2.0 | storage_write storm: 64 × 16 KiB overwrites a tx | 0.01 [0.01–0.01] | 1 [1–1] | 0× |
| wasmi 0.38 | storage_write storm: 64 × 16 KiB overwrites a tx | 0.01 [0.01–0.01] | 1 [1–1] | 0× |
| soroban-wasmi | load: distinct "87364 identical function types" × 22 (262,144 B each) | 5.71 [5.71–5.89] | 343 [343–353] | **2×** |
| soroban-wasmi | load: distinct minimal programs × 23904 (51 B each) | 1.11 [1.10–1.11] | 67 [66–67] | 0× |
| wasmi 2.0 | load: distinct "87364 identical function types" × 22 (262,144 B each) | 10.0 [10.0–10.1] | 602 [602–604] | **4×** |
| wasmi 2.0 | load: distinct minimal programs × 23904 (51 B each) | 1.71 [1.70–1.72] | 103 [102–103] | 1× |
| wasmi 0.38 | load: distinct "87364 identical function types" × 22 (262,144 B each) | 5.39 [5.38–5.39] | 323 [323–324] | **2×** |
| wasmi 0.38 | load: distinct minimal programs × 23904 (51 B each) | 1.19 [1.15–1.24] | 71 [69–74] | 0× |

**End-to-end check**: one real `max_block_fuel` block (three 20M-fuel transactions, each run until out of fuel) of each engine's worst compute pattern, measured whole, against the projection:

| engine | worst compute pattern | measured ms [min–max] | projected ms | vs 150 ms |
|---|---:|---:|---:|---:|
| soroban-wasmi | pointer chase, 2 MiB, random | 682 [681–682] | 824 | 5× |
| wasmi 2.0 | pointer chase, 2 MiB, random | 665 [664–665] | 805 | 4× |
| wasmi 0.38 | br_if not taken | 3468 [3460–3470] | 3450 | 23× |

## 6. Cold versus warm, and an engine's cost per block

**Cold** = the first transaction on a fresh block engine that has just loaded the code (engine and load outside the clock); **warm** = the same transaction again on an engine that has run it 10 times. Fuel must be identical (asserted for every repetition).

| transaction | fuel (identical cold and warm) | soroban-wasmi cold / warm µs | wasmi 2.0 cold / warm µs | wasmi 0.38 cold / warm µs |
|---|---:|---:|---:|---:|
| counter increment | 71,711 / 71,710 / 71,573 | 49.0 / 36.1 (+36%) | 35.2 / 33.4 (+5%) | 36.8 / 33.7 (+9%) |
| token transfer | 85,139 / 85,113 / 83,082 | 57.4 / 48.0 (+20%) | 44.1 / 39.0 (+13%) | 45.5 / 42.7 (+6%) |
| escrow open (payable) | 78,771 / 78,747 / 76,793 | 47.5 / 43.0 (+10%) | 42.0 / 37.3 (+13%) | 42.7 / 39.5 (+8%) |
| loop: call, 1,024 locals × 1,000 | 2,252,317 / 188,315 / 2,103,143 | 2,072 / 2,063 (+0%) | 2,126 / 2,112 (+1%) | 2,198 / 2,193 (+0%) |
| loop: i64.div_u by 1, chain × 1,000 | 142,287 / 142,285 / 71,191 | 490 / 486 (+1%) | 327 / 323 (+1%) | 391 / 388 (+1%) |
| loop: pointer chase, 2 MiB, random × 1,000 | 861,123 / 861,121 / 428,105 | 2,659 / 2,733 (-3%) | 1,198 / 1,241 (-3%) | 2,172 / 2,160 (+1%) |
| call_v1 × 100 to a 32-page callee | 8,518,283 / 8,518,182 / 8,515,945 | 3,073 / 3,047 (+1%) | 3,066 / 3,010 (+2%) | 3,030 / 3,030 (-0%) |

**An engine's cost per block**: creating a block engine (the engine, its linker with the 20 host functions), loading N codes (each a distinct hash: translated anew) and dropping it.

| engine | what | create + load µs | drop µs |
|---|---:|---:|---:|
| soroban-wasmi | create an empty engine | 6.87 | drop 1.42 |
|  | 1 × guest token.wasm | 53.6 (53.6 per code) | drop 5.81 (5.81 per code) |
|  | 10 × guest token.wasm | 496 (49.6 per code) | drop 13.4 (1.34 per code) |
|  | 100 × guest token.wasm | 4,884 (48.8 per code) | drop 105 (1.05 per code) |
|  | 1000 × guest token.wasm | 47,503 (47.5 per code) | drop 1,255 (1.25 per code) |
|  | 1 × 256 KB straight-line | 1,429 (1,429 per code) | drop 44.1 (44.1 per code) |
|  | 10 × 256 KB straight-line | 14,839 (1,484 per code) | drop 105 (10.5 per code) |
| wasmi 2.0 | create an empty engine | 4.30 | drop 1.20 |
|  | 1 × guest token.wasm | 129 (129 per code) | drop 5.19 (5.19 per code) |
|  | 10 × guest token.wasm | 1,152 (115 per code) | drop 34.4 (3.44 per code) |
|  | 100 × guest token.wasm | 11,408 (114 per code) | drop 194 (1.94 per code) |
|  | 1000 × guest token.wasm | 114,413 (114 per code) | drop 2,269 (2.27 per code) |
|  | 1 × 256 KB straight-line | 4,831 (4,831 per code) | drop 68.4 (68.4 per code) |
|  | 10 × 256 KB straight-line | 48,306 (4,831 per code) | drop 390 (39.0 per code) |
| wasmi 0.38 | create an empty engine | 4.18 | drop 1.17 |
|  | 1 × guest token.wasm | 78.0 (78.0 per code) | drop 8.60 (8.60 per code) |
|  | 10 × guest token.wasm | 688 (68.8 per code) | drop 27.3 (2.73 per code) |
|  | 100 × guest token.wasm | 6,635 (66.3 per code) | drop 184 (1.84 per code) |
|  | 1000 × guest token.wasm | 66,096 (66.1 per code) | drop 2,246 (2.25 per code) |
|  | 1 × 256 KB straight-line | 2,561 (2,561 per code) | drop 10.8 (10.8 per code) |
|  | 10 × 256 KB straight-line | 24,893 (2,489 per code) | drop 88.7 (8.87 per code) |

## 7. Native stack

The smallest thread stack on which the scenario runs as expected, bisected to 4 KiB (the OS rounds a thread's stack up to its page: 16 KiB on this Mac) in child processes (a stack overflow aborts the process). The thread both loads the code (a fresh block engine) and executes the transaction, as the executor thread will. Programs p1→…→p8 each `call_v1` the next; with recursion, each first recurses 1,000 Wasm frames (wasmi keeps Wasm frames off the native stack: the numbers should not move). Release build.

| scenario | soroban-wasmi | wasmi 2.0 | wasmi 0.38 |
|---|---:|---:|---:|
| one program, no recursion | 16 KiB | 16 KiB | 16 KiB |
| depth 8, no Wasm recursion | 36 KiB | 52 KiB | 36 KiB |
| depth 8, 1,000 Wasm frames per program | 36 KiB | 52 KiB | 36 KiB |
| depth 9 attempted (CallDepth), 1,000 Wasm frames per program | 36 KiB | 52 KiB | 40 KiB |

_Total run time: 1022 s._
