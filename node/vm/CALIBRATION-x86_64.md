<!-- Floor-runner run (docs/PROGRAMS-V0.md 5.2 items 1-2): GitHub Actions x86_64 Linux (ubuntu-24.04, 4 vCPU AMD EPYC 7763), full (9 repetitions), via .github/workflows/calibrate-vm.yml run 40742671981 on 2026-10-28, at the programs integration branch (021ffece). Development numbers, never prices. The Mac run is CALIBRATION.md; the node's own patterns are CALIBRATION-node.md. -->

# Calibration run

- Machine: x86_64 (x86_64, linux); development numbers, never prices
- rustc: rustc 1.98.1 (48a229cea 2026-09-01)
- Build: `release/bench` profile, overflow checks on, debug assertions off
- Engines: soroban-wasmi 29.0.0
- Repetitions: 9 (+2 warmup), samples ≥ 5 ms, loop runs ≈ 30 ms

## 1. ns per fuel by operator class

Each case is a loop whose body is `k` copies of one unit dominated by one class (plus the loop's own fuel: `br_if`, a decrement, `br`). ns per fuel = (time of N iterations − time of 0) / N / (fuel per iteration), so instantiation and the transaction's fixed cost cancel. Fuel per unit = fuel per iteration / k, under each engine's mapping of the schedule under test (wasmi 0.38: its own IR fuel). Cells: **ns/fuel median [min–max]** (fuel per unit). Values above 2.5 ns/fuel are marked **bold**.

| class | case | soroban-wasmi |
|---|---:|---:|
| control | empty loop (the loop's own overhead) | 0.51 [0.50–0.61] (27.0) |
| control | br to the next block end: (block (br 0)) | 0.64 [0.64–0.64] (3.42) |
| control | br_if not taken | 0.92 [0.92–0.92] (6.42) |
| control | br_if to the function's end, not taken | 1.14 [1.14–1.15] (3.42) |
| control | empty loops: (loop) | 0.66 [0.66–0.68] (3.42) |
| control | if/else, unpredictable | 0.65 [0.65–0.66] (40.7) |
| br_table | br_table 8 targets, fixed index | 1.03 [1.03–1.04] (7.69) |
| br_table | br_table 64 targets, random index | 0.66 [0.65–0.67] (39.8) |
| br_table | br_table 1,024 targets, random index | 0.60 [0.60–0.60] (60.0) |
| br_table | br_table 4,096 targets, random index | 0.60 [0.60–0.60] (60.0) |
| br_table | br_table 4,096 entries to one label, random index | 0.61 [0.60–0.61] (60.0) |
| call | call () -> (), no locals | 0.22 [0.22–0.23] (37.7) |
| call | call, 8 locals | 0.29 [0.28–0.29] (38.7) |
| call | call, 64 locals | 0.30 [0.30–0.31] (45.7) |
| call | call, 256 locals | 0.36 [0.36–0.36] (69.7) |
| call | call, 1,024 locals | 0.54 [0.53–0.54] (166) |
| call | call, 32 params | 0.62 [0.62–0.63] (139) |
| call | call, 32 params + 992 locals | 0.51 [0.51–0.51] (263) |
| call | call i64 -> i64 | 0.34 [0.34–0.34] (52.7) |
| call | recursion 64 deep, 1,024 locals a frame | 0.53 [0.52–0.56] (11,848) |
| call | call_indirect, fixed index | 0.40 [0.40–0.40] (40.7) |
| call | call_indirect, random index of 4,096 | 0.43 [0.42–0.50] (69.4) |
| call | call_indirect, 1,024 locals | 0.56 [0.56–0.57] (169) |
| i32 arith | i32 unary chain (clz ctz popcnt eqz) | 0.88 [0.87–0.88] (14.1) |
| i64 arith | i64 unary chain (clz ctz popcnt) | 0.91 [0.89–0.92] (11.1) |
| i32 arith | i32 binary chain, local operand | 0.86 [0.84–0.88] (74.2) |
| i64 arith | i64 binary chain, local operand | 0.96 [0.95–0.97] (74.2) |
| i32 arith | i32 binary chain, constant operand | 0.85 [0.83–0.86] (74.2) |
| i32 arith | i32 3-address: a = b op c | 1.18 [1.18–1.21] (25.7) |
| i64 arith | i64.mul chain | 0.76 [0.75–0.76] (6.52) |
| div/rem | i32.div_u by 1, chain | 0.76 [0.75–0.77] (6.52) |
| div/rem | i32.div_s by 1, chain | 0.85 [0.84–0.85] (6.52) |
| div/rem | i32.rem_u, xor chain | 0.78 [0.78–0.78] (13.0) |
| div/rem | i64.div_u by 1, chain | 0.85 [0.84–0.85] (6.52) |
| div/rem | i64.div_s by 1, chain | 0.80 [0.79–0.81] (6.52) |
| div/rem | i64.div_u by 3, xor chain | 0.74 [0.74–0.76] (13.0) |
| div/rem | i64.rem_u, xor chain | 0.78 [0.78–0.78] (13.0) |
| div/rem | i64.rem_s, xor chain | 0.82 [0.78–0.83] (13.0) |
| compare | i32 compare chain | 1.16 [1.15–1.17] (26.1) |
| compare | i64 compare chain | 1.31 [1.30–1.31] (20.1) |
| compare | i32.eqz chain | 0.86 [0.85–0.88] (3.52) |
| compare | wrap/extend chain | 0.91 [0.91–0.91] (14.1) |
| select | select chain | 0.63 [0.63–0.63] (10.0) |
| select | select, unpredictable condition | 0.81 [0.80–0.83] (37.7) |
| locals | local.get/local.set copies | 0.74 [0.74–0.75] (19.7) |
| locals | local.tee chain | 0.35 [0.35–0.35] (14.1) |
| locals | far locals (1,024 declared) | 0.77 [0.77–0.77] (13.7) |
| globals | global.get/global.set copies | 0.74 [0.74–0.74] (17.7) |
| globals | global increment | 0.58 [0.56–0.58] (15.7) |
| globals | global.get; drop | 0.51 [0.51–0.60] (7.84) |
| load | i32.load chain, one address | 0.18 [0.18–0.18] (20.5) |
| load | narrow loads chain, one address | 0.19 [0.19–0.20] (102) |
| load | pointer chase, 64 KiB, random | 0.22 [0.22–0.22] (20.5) |
| load | pointer chase, 2 MiB, random | 0.65 [0.65–0.66] (20.5) |
| load | random independent i64 loads, 2 MiB | 0.43 [0.42–0.43] (48.7) |
| store | i64.store, one address | 0.71 [0.71–0.71] (10.8) |
| store | narrow stores, one address | 0.70 [0.70–0.70] (31.7) |
| store | random i64 stores, 2 MiB | 1.04 [1.04–1.05] (32.7) |
| memory | memory.size; drop | 0.51 [0.51–0.51] (7.84) |

**Worst cases per engine** (median ns/fuel; the five highest):

| engine | case | ns/fuel | fuel/unit | vs 2.5 ns |
|---|---:|---:|---:|---:|
| soroban-wasmi | i64 compare chain | 1.31 [1.30–1.31] | 20.1 | 0.5× |
|  | i32 3-address: a = b op c | 1.18 [1.18–1.21] | 25.7 | 0.5× |
|  | i32 compare chain | 1.16 [1.15–1.17] | 26.1 | 0.5× |
|  | br_if to the function's end, not taken | 1.14 [1.14–1.15] | 3.42 | 0.5× |
|  | random i64 stores, 2 MiB | 1.04 [1.04–1.05] | 32.7 | 0.4× |

## 2. Translation and validation, ns per byte

Cold: each load is `BlockEngine::load` (a structural read of the module's sections, then the engine's `Module::new`: its own parse, validation and translation) on a fresh block engine made outside the clock, as a block loads every declared code into a fresh engine (4.3). `validate` is upload validation (`validate.rs`, wasmparser 0.228 plus our rules). CPU caches are warm (the same bytes are loaded repeatedly), so these are lower bounds for a cold machine. *Implied fuel* = time / 2.5 ns. **Implied LOAD_PER_BYTE** = (load time / 2.5 ns − LOAD_BASE 10,000) / bytes, against schedule v0's 40; **implied LOAD_BASE** comes from the smallest modules.

| module | bytes | validate µs (ns/B) | soroban-wasmi load µs (ns/B) | soroban-wasmi implied LOAD_PER_BYTE |
|---|---:|---:|---:|---:|
| guest counter.wasm | 565 | 11.7 (20.7) | 13.3 (23.5) | 0.00 |
| guest token.wasm | 2,467 | 35.0 (14.2) | 37.3 (15.1) | 2.00 |
| guest escrow.wasm | 3,389 | 44.0 (13.0) | 43.9 (13.0) | 2.23 |
| guest router.wasm | 1,484 | 20.6 (13.9) | 22.9 (15.4) | 0.00 |
| minimal program | 51 | 2.23 (43.7) | 2.24 (43.8) | 0.00 |
| minimal program + 20 imports | 621 | 13.3 (21.4) | 13.4 (21.6) | 0.00 |
| one function, straight-line i64 arithmetic | 262,141 | 1,674 (6.39) | 1,184 (4.52) | 1.77 |
| 5040 small functions (≈40 B bodies) | 262,137 | 3,105 (11.8) | 3,541 (13.5) | 5.37 |
| 6094 functions with 1,024 locals each | 262,099 | 3,470 (13.2) | 4,073 (15.5) | 6.18 |
| br_table, 2 × 131030 targets to one label | 262,143 | 1,672 (6.38) | 9,233 (35.2) | 14.0 |
| br_table, 12 × 4,096 distinct nested targets | 244,343 | 2,776 (11.4) | 3,373 (13.8) | 5.48 |
| 87361 nested blocks | 262,142 | 4,224 (16.1) | 3,021 (11.5) | 4.57 |
| 32760 if/else | 262,143 | 3,472 (13.2) | 2,877 (11.0) | 4.35 |
| 28942 distinct i64 constants | 258,321 | 749 (2.90) | 2,161 (8.36) | 3.31 |
| 131035 B of data + as much code (32 pages) | 262,143 | 1,084 (4.14) | 882 (3.37) | 1.31 |
| 14980 distinct function types | 223,362 | 8,989 (40.2) | 8,563 (38.3) | 15.3 |
| 87364 identical function types | 262,144 | 19,647 (74.9) | 12,580 (48.0) | 19.2 |
| 64 exports of one function | 609 | 15.5 (25.5) | 17.3 (28.4) | 0.00 |
| 131019 calls | 262,143 | 3,085 (11.8) | 2,665 (10.2) | 4.03 |
| 43679 far local.get/set pairs (1,024 locals) | 262,140 | 1,899 (7.24) | 1,182 (4.51) | 1.77 |
| 21 × (4,000 local.gets, local.set over them, 3,999 adds) | 252,148 | 2,535 (10.1) | 1,728 (6.85) | 2.70 |
| 16 × (4,000-deep stack, 1,000 local.sets) | 256,064 | 2,537 (9.91) | 1,666 (6.51) | 2.56 |
| 43680 dead-code units after unreachable | 262,140 | 2,117 (8.07) | 1,305 (4.98) | 1.95 |
| 256 globals, 52160 global.get/set pairs | 262,144 | 2,254 (8.60) | 1,232 (4.70) | 1.84 |

Spread, and the drop of an engine holding the module (less an empty engine's drop): part of the block's cost, unpriced.

| module | soroban-wasmi load [min–max] µs | soroban-wasmi drop µs | validate [min–max] µs |
|---|---:|---:|---:|
| guest counter.wasm | [13.3–13.6] | 1.27 | [11.6–11.8] |
| guest token.wasm | [37.0–37.7] | 3.74 | [34.9–35.3] |
| guest escrow.wasm | [43.6–49.4] | 3.86 | [43.9–44.6] |
| guest router.wasm | [22.7–25.5] | 2.19 | [20.5–20.9] |
| minimal program | [2.18–2.28] | 0.83 | [2.22–2.25] |
| minimal program + 20 imports | [13.4–13.5] | 0.99 | [13.3–13.4] |
| one function, straight-line i64 arithmetic | [1,167–1,220] | 54.9 | [1,667–1,757] |
| 5040 small functions (≈40 B bodies) | [3,519–3,552] | 4.06 | [3,093–3,187] |
| 6094 functions with 1,024 locals each | [4,056–4,113] | 4.36 | [3,459–3,508] |
| br_table, 2 × 131030 targets to one label | [9,161–9,274] | 119 | [1,661–1,745] |
| br_table, 12 × 4,096 distinct nested targets | [3,362–3,382] | 0.75 | [2,767–2,777] |
| 87361 nested blocks | [2,951–3,082] | 4.79 | [4,210–4,271] |
| 32760 if/else | [2,866–2,910] | 13.2 | [3,458–3,486] |
| 28942 distinct i64 constants | [2,148–2,177] | 169 | [748–751] |
| 131035 B of data + as much code (32 pages) | [878–914] | 3.30 | [1,083–1,103] |
| 14980 distinct function types | [8,505–8,676] | 767 | [8,972–9,073] |
| 87364 identical function types | [12,548–12,686] | 3.83 | [19,587–19,778] |
| 64 exports of one function | [17.3–17.7] | 2.55 | [15.5–15.6] |
| 131019 calls | [2,657–2,727] | 20.0 | [3,063–3,092] |
| 43679 far local.get/set pairs (1,024 locals) | [1,178–1,198] | 33.1 | [1,896–1,917] |
| 21 × (4,000 local.gets, local.set over them, 3,999 adds) | [1,727–1,735] | 73.6 | [2,532–2,549] |
| 16 × (4,000-deep stack, 1,000 local.sets) | [1,659–1,736] | 64.6 | [2,535–2,557] |
| 43680 dead-code units after unreachable | [1,304–1,310] | 2.93 | [2,108–2,136] |
| 256 globals, 52160 global.get/set pairs | [1,224–1,249] | 38.8 | [2,252–2,260] |

**Summary.** Schedule v0: LOAD_BASE 10,000, LOAD_PER_BYTE 40; upload 50,000 + 71 per byte.

| engine | worst ns/B, any size | worst ns/B, modules > 100 KB | implied LOAD_PER_BYTE (worst > 100 KB) | minimal module's load (implied LOAD_BASE) | worst upload (validate + load), modules > 100 KB |
|---|---:|---:|---:|---:|---:|
| soroban-wasmi | 87364 identical function types (48.0 ns/B) | 87364 identical function types (48.0 ns/B) | 19.2 | 2.24 µs = 894 fuel | 87364 identical function types (123 ns/B = 49.2 fuel/B) |

### Upload, ns per fuel (go/no-go)

Each upload is `upload::check` (validation, a fresh soroban-wasmi engine, the test translation and the engine's drop) plus the code's hash, as the node runs `UploadCode` on new code, over `upload_fuel(bytes, 0)` = 50,000 + 71 × bytes. Upload burns no engine fuel, so this is wall time over the price. Schedule v0's 71 per byte is the most that keeps a maximum upload within `MAX_TX_FUEL`, so it has the schedule's thinnest margin; values above 2.5 ns/fuel are marked **bold**.

| module | bytes | upload µs | upload fuel | ns/fuel |
|---|---:|---:|---:|---:|
| guest counter.wasm | 565 | 32.4 | 90,115 | 0.36 [0.36–0.37] |
| guest token.wasm | 2,467 | 83.2 | 225,157 | 0.37 [0.37–0.38] |
| guest escrow.wasm | 3,389 | 99.1 | 290,619 | 0.34 [0.34–0.35] |
| guest router.wasm | 1,484 | 52.3 | 155,364 | 0.34 [0.33–0.34] |
| minimal program | 51 | 10.3 | 53,621 | 0.19 [0.19–0.19] |
| minimal program + 20 imports | 621 | 35.0 | 94,091 | 0.37 [0.37–0.37] |
| one function, straight-line i64 arithmetic | 262,141 | 2,837 | 18,662,011 | 0.15 [0.15–0.15] |
| 5040 small functions (≈40 B bodies) | 262,137 | 6,585 | 18,661,727 | 0.35 [0.35–0.36] |
| 6094 functions with 1,024 locals each | 262,099 | 7,550 | 18,659,029 | 0.40 [0.40–0.41] |
| br_table, 2 × 131030 targets to one label | 262,143 | 10,713 | 18,662,153 | 0.57 [0.57–0.58] |
| br_table, 12 × 4,096 distinct nested targets | 244,343 | 6,127 | 17,398,353 | 0.35 [0.35–0.36] |
| 87361 nested blocks | 262,142 | 7,122 | 18,662,082 | 0.38 [0.38–0.39] |
| 32760 if/else | 262,143 | 6,366 | 18,662,153 | 0.34 [0.34–0.34] |
| 28942 distinct i64 constants | 258,321 | 3,037 | 18,390,791 | 0.17 [0.16–0.17] |
| 131035 B of data + as much code (32 pages) | 262,143 | 2,023 | 18,662,153 | 0.11 [0.11–0.11] |
| 14980 distinct function types | 223,362 | 18,081 | 15,908,702 | 1.14 [1.13–1.15] |
| 87364 identical function types | 262,144 | 32,366 | 18,662,224 | 1.73 [1.72–1.74] |
| 64 exports of one function | 609 | 43.6 | 93,239 | 0.47 [0.47–0.47] |
| 131019 calls | 262,143 | 5,686 | 18,662,153 | 0.30 [0.30–0.31] |
| 43679 far local.get/set pairs (1,024 locals) | 262,140 | 3,077 | 18,661,940 | 0.16 [0.16–0.17] |
| 21 × (4,000 local.gets, local.set over them, 3,999 adds) | 252,148 | 4,224 | 17,952,508 | 0.24 [0.23–0.24] |
| 16 × (4,000-deep stack, 1,000 local.sets) | 256,064 | 4,152 | 18,230,544 | 0.23 [0.23–0.23] |
| 43680 dead-code units after unreachable | 262,140 | 3,475 | 18,661,940 | 0.19 [0.19–0.19] |
| 256 globals, 52160 global.get/set pairs | 262,144 | 3,360 | 18,662,224 | 0.18 [0.18–0.18] |

**Upload go/no-go: GO.** Worst: 87364 identical function types (262,144 B), 1.73 ns per fuel against 2.5 (49.4 fuel per byte against the price's 71).

## 3. Instantiation

Per module, three ways: **raw** (`BlockEngine::invoke` of an empty `bk_noop` with a host that refuses every import: the engine's store, linking, instance and drop), **tx** (a top-level transaction calling `bk_noop` through `execute`: also the executor's fixed cost) and **call_v1** (each call of a loop `call_v1`s `bk_noop` on the module: the repeatable pattern, charged `call` + `instantiate_fuel`). *Schedule* = `instantiate_fuel` (meter.rs: a base, plus a price per import, export, function, global, element segment and element, data segment, page and data byte). Time ÷ 2.5 ns is the fuel it would need.

| module | bytes | schedule fuel | soroban-wasmi raw µs | soroban-wasmi tx µs (ns/fuel) |
|---|---:|---:|---:|---:|
| 1 page, no imports | 52 | 5,015 | 1.06 | 2.64 (0.53) |
| 1 page, 20 imports | 622 | 11,015 | 4.62 | 6.19 (0.56) |
| 32 pages | 52 | 67,015 | 22.7 | 24.7 (0.37) |
| 32 pages, 20 imports, + touch every 4 KiB | 667 | 73,530 | 26.6 | 28.5 (0.39) |
| data: 1 segment of 120,000 B | 240,075 | 127,050 | 3.84 | 5.44 (0.04) |
| data: 1,000 segments of 1 B | 8,001 | 26,030 | 15.4 | 17.4 (0.67) |
| data: 1,000 segments of 120 B | 246,999 | 147,030 | 16.1 | 17.8 (0.12) |
| 256 globals | 1,337 | 10,135 | 4.15 | 5.73 (0.56) |
| table of 4,096, 1,000 element segments | 10,050 | 63,015 | 31.7 | 33.8 (0.54) |
| 5040 functions | 262,137 | 80,615 | 53.2 | 55.1 (0.68) |
| 65 exports | 610 | 36,515 | 8.58 | 10.6 (0.29) |
| all: 20 imports, 32 pages, 256 globals, table + 1,000 elems, 2,001 funcs, 1,000 data, 65 exports | 123,409 | 218,635 | 103 | 106 (0.49) |

**Repeated instantiation through `call_v1`** (per call: the measured fuel, then time and ns per fuel):

| callee | schedule fuel per call | soroban-wasmi fuel: µs (ns/fuel) |
|---|---:|---:|
| 1 page, no imports | 25,115 | 25,205: 1.99 (0.08) |
| 1 page, 20 imports | 31,115 | 31,205: 5.56 (0.18) |
| 32 pages | 87,115 | 87,205: 23.9 (0.27) |
| 32 pages, 20 imports, + touch every 4 KiB | 93,630 | 93,720: 28.0 (0.30) |
| data: 1 segment of 120,000 B | 147,150 | 147,240: 4.78 (0.03) |
| data: 1,000 segments of 1 B | 46,130 | 46,220: 16.6 (0.36) |
| data: 1,000 segments of 120 B | 167,130 | 167,220: 17.1 (0.10) |
| 256 globals | 30,235 | 30,325: 5.17 (0.17) |
| table of 4,096, 1,000 element segments | 83,115 | 83,205: 33.4 (0.40) |
| 5040 functions | 100,715 | 100,805: 54.4 (0.54) |
| 65 exports | 56,615 | 56,705: 10.00 (0.18) |
| all: 20 imports, 32 pages, 256 globals, table + 1,000 elems, 2,001 funcs, 1,000 data, 65 exports | 238,735 | 238,825: 106 (0.44) |

**Touching the memory**: a transaction whose method stores to every 4 KiB of its 2 MiB, so the zeroed pages the allocator handed out lazily are faulted in (the cost an instantiation's per-page price must cover):

| module | soroban-wasmi tx fuel: µs (ns/fuel) |
|---|---:|
| 32 pages, 20 imports, + touch every 4 KiB | 92,480: 41.7 (0.45) (+13.2 µs over the empty tx) |

## 4. Host functions (ABI v1)

Each case loops one host function N times inside a program (`bk_<case>`) through the real executor and `MemHost`; per call = (time of N − time of 0) / N, so the call's Wasm side (its arguments, the `call` itself at 10 fuel, the loop's ~9) is included, as a program would pay it. Cases bounded per transaction (events, keys written, bytes written) run that many calls a transaction. Fuel per call is measured (the schedule's charge plus that Wasm side). `MemHost` is a `BTreeMap`, not the node's state: storage costs here are the overlay's and the host's, not redb's or the tree's (those are commit-time).

| function, case | fuel per call | soroban-wasmi ns/call (ns/fuel) |
|---|---:|---:|
| `input_len_v1`  | 160 | 72.4 (0.45) |
| `input_read_v1` 44 B input | 204 | 73.1 (0.36) |
| `input_read_v1` 32 KiB input | 32,928 | 685 (0.02) |
| `return_data_v1` 0 B | 163 | 76.6 (0.47) |
| `return_data_v1` 4 KiB | 4,259 | 157 (0.04) |
| `debug_log_v1` 0 B | 2,163 | 74.1 (0.03) |
| `debug_log_v1` 64 KiB | 526,451 | 74.1 (0.00) |
| `abort_v1` 256 B message (one per tx: tx time − an empty tx's) | 1,283 | 667 (0.52) |
| `storage_read_v1` absent, 32 B key | 1,204 | 127 (0.11) |
| `storage_read_v1` 32 B key, 8 B value | 1,212 | 156 (0.13) |
| `storage_read_v1` 128 B key, 8 B value | 1,308 | 157 (0.12) |
| `storage_read_v1` 32 B key, 16 KiB value | 17,588 | 657 (0.04) |
| `storage_write_v1` overwrite, 32 B key, 0 B value | 5,492 | 181 (0.03) |
| `storage_write_v1` overwrite, 128 B key, 8 B value | 6,532 | 213 (0.03) |
| `storage_write_v1` overwrite, 32 B key, 16 KiB value (64 a tx) | 169,332 | 974 (0.01) |
| `storage_write_v1` new keys, 32 B key, 8 B value (256 a tx) | 5,582 | 476 (0.09) |
| `storage_remove_v1` absent | 3,166 | 113 (0.04) |
| `storage_remove_v1` present, 16 KiB value (256 a tx) | 3,176 | 1,459 (0.46) |
| `self_address_v1`  | 360 | 71.9 (0.20) |
| `caller_v1`  | 360 | 71.5 (0.20) |
| `value_v1`  | 360 | 72.4 (0.20) |
| `block_height_v1`  | 360 | 72.6 (0.20) |
| `self_balance_v1`  | 360 | 87.6 (0.24) |
| `transfer_v1` to an existing account | 10,169 | 121 (0.01) |
| `transfer_v1` opening an account (OPEN_DEDUCT) | 10,179 | 755 (0.07) |
| `call_v1` callee 1 page, empty method | 27,050 | 2,735 (0.10) |
| `call_v1` callee 1 page, 32 KiB input | 59,818 | 3,342 (0.06) |
| `call_v1` callee 32 pages, empty method | 89,050 | 24,697 (0.28) |
| `try_call_v1` callee traps (layer dropped) | 27,050 | 2,628 (0.10) |
| `last_return_v1` 0 B | 160 | 73.3 (0.46) |
| `last_return_v1` 4 KiB | 4,256 | 116 (0.03) |
| `last_return_len_v1`  | 160 | 71.0 (0.44) |
| `last_abort_code_v1`  | 160 | 71.0 (0.44) |
| `emit_v1` 8 B name, 0 B data (64 a tx) | 2,233 | 161 (0.07) |
| `emit_v1` 32 B name, 16,352 B data (1 a tx) | 133,241 | 1,186 (0.01) |

**Per byte**, from the two sizes of each function (Δtime / Δbytes, against Δfuel / Δbytes):

| function: sizes | fuel per byte | soroban-wasmi ns/B (ns/fuel) |
|---|---:|---:|
| `input_read_v1`: 44 B input → 32 KiB input | 1.00 | 0.02 (0.02) |
| `return_data_v1`: 0 B → 4 KiB | 1.00 | 0.02 (0.02) |
| `debug_log_v1`: 0 B → 64 KiB | 8.00 | 0.00 (0.00) |
| `storage_read_v1`: 32 B key, 8 B value → 32 B key, 16 KiB value | 1.00 | 0.03 (0.03) |
| `storage_read_v1`: 32 B key, 8 B value → 128 B key, 8 B value | 1.00 | 0.01 (0.01) |
| `storage_write_v1`: overwrite, 32 B key, 0 B value → overwrite, 32 B key, 16 KiB value (64 a tx) | 10.0 | 0.05 (0.00) |
| `call_v1`: callee 1 page, empty method → callee 1 page, 32 KiB input | 1.00 | 0.02 (0.02) |
| `last_return_v1`: 0 B → 4 KiB | 1.00 | 0.01 (0.01) |
| `emit_v1`: 8 B name, 0 B data (64 a tx) → 32 B name, 16,352 B data (1 a tx) | 8.00 | 0.06 (0.01) |

## 5. An adversarial block

A `max_block_fuel` block is 60,000,000 fuel (3 transactions of `program_max_tx_fuel`, 20,000,000 each). The cost of a block made entirely of one pattern is **60M × the pattern's ns/fuel**: that is how the design sets the budget (60M × 2.5 ns = 150 ms). Each ns/fuel below is measured as in sections 1–4 (a bounded loop, not a literal 60M-fuel run, which would just be the same number × 60M); the projection is then checked against one real end-to-end 60M block per engine at the end. 150 ms is the budget; a block over it is **bold**.

| engine | block of this pattern | ns/fuel [min–max] | projected ms [min–max] | vs 150 ms |
|---|---:|---:|---:|---:|
| soroban-wasmi | compute: i64 compare chain | 1.31 [1.30–1.31] | 79 [78–79] | 1× |
| soroban-wasmi | compute: i32 3-address: a = b op c | 1.18 [1.18–1.21] | 71 [71–72] | 0× |
| soroban-wasmi | instantiation: call_v1 loop to the many-exports callee | 0.18 [0.18–0.18] | 11 [11–11] | 0× |
| soroban-wasmi | instantiation: call_v1 loop to the 5,040-function callee | 0.54 [0.54–0.54] | 32 [32–32] | 0× |
| soroban-wasmi | instantiation: call_v1 loop to a 32-page callee | 0.28 [0.28–0.28] | 17 [17–17] | 0× |
| soroban-wasmi | storage_write storm: 64 × 16 KiB overwrites a tx | 0.01 [0.01–0.01] | 0 [0–0] | 0× |
| soroban-wasmi | load: distinct "87364 identical function types" × 5 (262,144 B each) | 1.20 [1.20–2.47] | 72 [72–148] | 0× |
| soroban-wasmi | load: distinct minimal programs × 4983 (51 B each) | 0.18 [0.18–0.18] | 11 [11–11] | 0× |

**End-to-end check**: one real `max_block_fuel` block (three 20M-fuel transactions, each run until out of fuel) of each engine's worst compute pattern, measured whole, against the projection:

| engine | worst compute pattern | measured ms [min–max] | projected ms | vs 150 ms |
|---|---:|---:|---:|---:|
| soroban-wasmi | i64 compare chain | 79 [79–79] | 79 | 1× |

## 6. Cold versus warm, and an engine's cost per block

**Cold** = the first transaction on a fresh block engine that has just loaded the code (engine and load outside the clock); **warm** = the same transaction again on an engine that has run it 10 times. Fuel must be identical (asserted for every repetition).

| transaction | fuel (identical cold and warm) | soroban-wasmi cold / warm µs |
|---|---:|---:|
| counter increment | 76,273 | 29.8 / 28.3 (+5%) |
| token transfer | 100,486 | 41.2 / 38.8 (+6%) |
| escrow open (payable) | 92,801 | 38.8 / 35.7 (+9%) |
| loop: call, 1,024 locals × 1,000 | 2,657,209 | 1,446 / 1,423 (+2%) |
| loop: i64.div_u by 1, chain × 1,000 | 422,984 | 356 / 354 (+1%) |
| loop: pointer chase, 2 MiB, random × 1,000 | 3,577,348 | 2,454 / 2,449 (+0%) |
| call_v1 × 100 to a 32-page callee | 8,929,879 | 2,515 / 2,522 (-0%) |

**An engine's cost per block**: creating a block engine (the engine, its linker with the 20 host functions), loading N codes (each a distinct hash: translated anew) and dropping it.

| engine | what | create + load µs | drop µs |
|---|---:|---:|---:|
| soroban-wasmi | create an empty engine | 4.52 | drop 2.28 |
|  | 1 × guest token.wasm | 40.4 (40.4 per code) | drop 9.62 (9.62 per code) |
|  | 10 × guest token.wasm | 363 (36.3 per code) | drop 21.8 (2.18 per code) |
|  | 100 × guest token.wasm | 3,622 (36.2 per code) | drop 156 (1.56 per code) |
|  | 1000 × guest token.wasm | 36,082 (36.1 per code) | drop 1,828 (1.83 per code) |
|  | 1 × 256 KB straight-line | 1,171 (1,171 per code) | drop 41.3 (41.3 per code) |
|  | 10 × 256 KB straight-line | 11,672 (1,167 per code) | drop 9.01 (0.90 per code) |

## 7. Native stack

The smallest thread stack on which the scenario runs as expected, bisected to 4 KiB (the OS rounds a thread's stack up to its page: 16 KiB on this Mac) in child processes (a stack overflow aborts the process). The thread both loads the code (a fresh block engine) and executes the transaction, as the executor thread will. Programs p1→…→p8 each `call_v1` the next; with recursion, each first recurses 1,000 Wasm frames (wasmi keeps Wasm frames off the native stack: the numbers should not move). Release build.

| scenario | soroban-wasmi |
|---|---:|
| one program, no recursion | 16 KiB |
| depth 8, no Wasm recursion | 40 KiB |
| depth 8, 1,000 Wasm frames per program | 40 KiB |
| depth 9 attempted (CallDepth), 1,000 Wasm frames per program | 40 KiB |

_Total run time: 1061 s._
