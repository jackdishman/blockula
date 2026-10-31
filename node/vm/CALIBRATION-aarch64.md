<!-- Floor-runner run (docs/PROGRAMS-V0.md 5.2 items 1-2): GitHub Actions aarch64 Linux (ubuntu-24.04-arm, 4 vCPU), full (9 repetitions), via .github/workflows/calibrate-vm.yml run 40742671981 on 2026-10-28, at the programs integration branch (021ffece). Development numbers, never prices. The Mac run is CALIBRATION.md; the node's own patterns are CALIBRATION-node.md. -->

# Calibration run

- Machine: aarch64 (aarch64, linux); development numbers, never prices
- rustc: rustc 1.98.1 (48a229cea 2026-09-01)
- Build: `release/bench` profile, overflow checks on, debug assertions off
- Engines: soroban-wasmi 29.0.0
- Repetitions: 9 (+2 warmup), samples ≥ 5 ms, loop runs ≈ 30 ms

## 1. ns per fuel by operator class

Each case is a loop whose body is `k` copies of one unit dominated by one class (plus the loop's own fuel: `br_if`, a decrement, `br`). ns per fuel = (time of N iterations − time of 0) / N / (fuel per iteration), so instantiation and the transaction's fixed cost cancel. Fuel per unit = fuel per iteration / k, under each engine's mapping of the schedule under test (wasmi 0.38: its own IR fuel). Cells: **ns/fuel median [min–max]** (fuel per unit). Values above 2.5 ns/fuel are marked **bold**.

| class | case | soroban-wasmi |
|---|---:|---:|
| control | empty loop (the loop's own overhead) | 0.50 [0.50–0.50] (27.0) |
| control | br to the next block end: (block (br 0)) | 0.53 [0.53–0.53] (3.42) |
| control | br_if not taken | 0.79 [0.79–0.79] (6.42) |
| control | br_if to the function's end, not taken | 0.95 [0.94–0.95] (3.42) |
| control | empty loops: (loop) | 0.55 [0.54–0.55] (3.42) |
| control | if/else, unpredictable | 0.56 [0.56–0.56] (40.7) |
| br_table | br_table 8 targets, fixed index | 1.08 [1.07–1.08] (7.69) |
| br_table | br_table 64 targets, random index | 0.59 [0.59–0.59] (39.8) |
| br_table | br_table 1,024 targets, random index | 0.54 [0.54–0.54] (60.0) |
| br_table | br_table 4,096 targets, random index | 0.54 [0.54–0.54] (60.0) |
| br_table | br_table 4,096 entries to one label, random index | 0.54 [0.54–0.54] (60.0) |
| call | call () -> (), no locals | 0.25 [0.25–0.26] (37.7) |
| call | call, 8 locals | 0.28 [0.28–0.28] (38.7) |
| call | call, 64 locals | 0.30 [0.30–0.30] (45.7) |
| call | call, 256 locals | 0.35 [0.35–0.35] (69.7) |
| call | call, 1,024 locals | 0.33 [0.33–0.34] (166) |
| call | call, 32 params | 0.41 [0.41–0.41] (139) |
| call | call, 32 params + 992 locals | 0.37 [0.36–0.37] (263) |
| call | call i64 -> i64 | 0.30 [0.30–0.30] (52.7) |
| call | recursion 64 deep, 1,024 locals a frame | 0.40 [0.40–0.41] (11,848) |
| call | call_indirect, fixed index | 0.44 [0.43–0.44] (40.7) |
| call | call_indirect, random index of 4,096 | 0.42 [0.42–0.42] (69.4) |
| call | call_indirect, 1,024 locals | 0.42 [0.41–0.42] (169) |
| i32 arith | i32 unary chain (clz ctz popcnt eqz) | 0.79 [0.79–0.79] (14.1) |
| i64 arith | i64 unary chain (clz ctz popcnt) | 0.83 [0.83–0.84] (11.1) |
| i32 arith | i32 binary chain, local operand | 0.55 [0.55–0.55] (74.2) |
| i64 arith | i64 binary chain, local operand | 0.55 [0.55–0.55] (74.2) |
| i32 arith | i32 binary chain, constant operand | 0.55 [0.55–0.55] (74.2) |
| i32 arith | i32 3-address: a = b op c | 0.53 [0.53–0.53] (25.7) |
| i64 arith | i64.mul chain | 0.55 [0.55–0.55] (6.52) |
| div/rem | i32.div_u by 1, chain | 0.75 [0.75–0.75] (6.52) |
| div/rem | i32.div_s by 1, chain | 0.76 [0.76–0.76] (6.52) |
| div/rem | i32.rem_u, xor chain | 0.58 [0.58–0.58] (13.0) |
| div/rem | i64.div_u by 1, chain | 0.75 [0.75–0.75] (6.52) |
| div/rem | i64.div_s by 1, chain | 0.76 [0.76–0.76] (6.52) |
| div/rem | i64.div_u by 3, xor chain | 0.84 [0.84–0.84] (13.0) |
| div/rem | i64.rem_u, xor chain | 0.58 [0.58–0.58] (13.0) |
| div/rem | i64.rem_s, xor chain | 0.57 [0.57–0.59] (13.0) |
| compare | i32 compare chain | 0.55 [0.55–0.55] (26.1) |
| compare | i64 compare chain | 0.57 [0.57–0.57] (20.1) |
| compare | i32.eqz chain | 0.57 [0.57–0.57] (3.52) |
| compare | wrap/extend chain | 0.60 [0.60–0.60] (14.1) |
| select | select chain | 0.51 [0.51–0.51] (10.0) |
| select | select, unpredictable condition | 0.52 [0.52–0.52] (37.7) |
| locals | local.get/local.set copies | 0.51 [0.51–0.51] (19.7) |
| locals | local.tee chain | 0.51 [0.51–0.51] (14.1) |
| locals | far locals (1,024 declared) | 0.52 [0.52–0.52] (13.7) |
| globals | global.get/global.set copies | 0.63 [0.63–0.63] (17.7) |
| globals | global increment | 0.49 [0.49–0.49] (15.7) |
| globals | global.get; drop | 0.45 [0.45–0.45] (7.84) |
| load | i32.load chain, one address | 0.22 [0.22–0.22] (20.5) |
| load | narrow loads chain, one address | 0.23 [0.23–0.23] (102) |
| load | pointer chase, 64 KiB, random | 0.23 [0.23–0.23] (20.5) |
| load | pointer chase, 2 MiB, random | 1.24 [1.23–1.26] (20.5) |
| load | random independent i64 loads, 2 MiB | 0.59 [0.58–0.59] (48.7) |
| store | i64.store, one address | 0.52 [0.51–0.52] (10.8) |
| store | narrow stores, one address | 0.50 [0.50–0.50] (31.7) |
| store | random i64 stores, 2 MiB | 0.53 [0.53–0.53] (32.7) |
| memory | memory.size; drop | 0.49 [0.49–0.49] (7.84) |

**Worst cases per engine** (median ns/fuel; the five highest):

| engine | case | ns/fuel | fuel/unit | vs 2.5 ns |
|---|---:|---:|---:|---:|
| soroban-wasmi | pointer chase, 2 MiB, random | 1.24 [1.23–1.26] | 20.5 | 0.5× |
|  | br_table 8 targets, fixed index | 1.08 [1.07–1.08] | 7.69 | 0.4× |
|  | br_if to the function's end, not taken | 0.95 [0.94–0.95] | 3.42 | 0.4× |
|  | i64.div_u by 3, xor chain | 0.84 [0.84–0.84] | 13.0 | 0.3× |
|  | i64 unary chain (clz ctz popcnt) | 0.83 [0.83–0.84] | 11.1 | 0.3× |

## 2. Translation and validation, ns per byte

Cold: each load is `BlockEngine::load` (a structural read of the module's sections, then the engine's `Module::new`: its own parse, validation and translation) on a fresh block engine made outside the clock, as a block loads every declared code into a fresh engine (4.3). `validate` is upload validation (`validate.rs`, wasmparser 0.228 plus our rules). CPU caches are warm (the same bytes are loaded repeatedly), so these are lower bounds for a cold machine. *Implied fuel* = time / 2.5 ns. **Implied LOAD_PER_BYTE** = (load time / 2.5 ns − LOAD_BASE 10,000) / bytes, against schedule v0's 40; **implied LOAD_BASE** comes from the smallest modules.

| module | bytes | validate µs (ns/B) | soroban-wasmi load µs (ns/B) | soroban-wasmi implied LOAD_PER_BYTE |
|---|---:|---:|---:|---:|
| guest counter.wasm | 565 | 13.2 (23.3) | 15.5 (27.4) | 0.00 |
| guest token.wasm | 2,467 | 38.0 (15.4) | 42.9 (17.4) | 2.91 |
| guest escrow.wasm | 3,389 | 47.1 (13.9) | 50.6 (14.9) | 3.02 |
| guest router.wasm | 1,484 | 23.0 (15.5) | 26.2 (17.6) | 0.32 |
| minimal program | 51 | 2.53 (49.5) | 2.73 (53.6) | 0.00 |
| minimal program + 20 imports | 621 | 14.9 (23.9) | 15.1 (24.4) | 0.00 |
| one function, straight-line i64 arithmetic | 262,141 | 1,479 (5.64) | 1,382 (5.27) | 2.07 |
| 5040 small functions (≈40 B bodies) | 262,137 | 3,146 (12.0) | 4,251 (16.2) | 6.45 |
| 6094 functions with 1,024 locals each | 262,099 | 3,577 (13.6) | 4,880 (18.6) | 7.41 |
| br_table, 2 × 131030 targets to one label | 262,143 | 3,183 (12.1) | 11,154 (42.5) | 17.0 |
| br_table, 12 × 4,096 distinct nested targets | 244,343 | 2,674 (10.9) | 4,306 (17.6) | 7.01 |
| 87361 nested blocks | 262,142 | 3,552 (13.5) | 3,751 (14.3) | 5.69 |
| 32760 if/else | 262,143 | 3,847 (14.7) | 3,513 (13.4) | 5.32 |
| 28942 distinct i64 constants | 258,321 | 710 (2.75) | 2,551 (9.87) | 3.91 |
| 131035 B of data + as much code (32 pages) | 262,143 | 987 (3.77) | 1,071 (4.09) | 1.60 |
| 14980 distinct function types | 223,362 | 10,513 (47.1) | 9,417 (42.2) | 16.8 |
| 87364 identical function types | 262,144 | 21,822 (83.2) | 13,920 (53.1) | 21.2 |
| 64 exports of one function | 609 | 18.6 (30.5) | 20.1 (33.1) | 0.00 |
| 131019 calls | 262,143 | 2,594 (9.90) | 4,748 (18.1) | 7.21 |
| 43679 far local.get/set pairs (1,024 locals) | 262,140 | 1,751 (6.68) | 1,508 (5.75) | 2.26 |
| 21 × (4,000 local.gets, local.set over them, 3,999 adds) | 252,148 | 2,211 (8.77) | 2,139 (8.48) | 3.35 |
| 16 × (4,000-deep stack, 1,000 local.sets) | 256,064 | 2,192 (8.56) | 2,039 (7.96) | 3.15 |
| 43680 dead-code units after unreachable | 262,140 | 1,884 (7.19) | 1,335 (5.09) | 2.00 |
| 256 globals, 52160 global.get/set pairs | 262,144 | 2,133 (8.14) | 1,505 (5.74) | 2.26 |

Spread, and the drop of an engine holding the module (less an empty engine's drop): part of the block's cost, unpriced.

| module | soroban-wasmi load [min–max] µs | soroban-wasmi drop µs | validate [min–max] µs |
|---|---:|---:|---:|
| guest counter.wasm | [15.4–15.6] | 2.35 | [13.1–13.3] |
| guest token.wasm | [42.6–43.1] | 3.99 | [37.9–38.1] |
| guest escrow.wasm | [50.5–51.0] | 3.82 | [47.0–47.2] |
| guest router.wasm | [25.9–26.4] | 3.18 | [23.0–23.0] |
| minimal program | [2.68–2.86] | 1.71 | [2.52–2.53] |
| minimal program + 20 imports | [15.0–15.3] | 2.86 | [14.8–15.0] |
| one function, straight-line i64 arithmetic | [1,375–1,385] | 45.7 | [1,467–1,493] |
| 5040 small functions (≈40 B bodies) | [4,234–4,270] | 3.08 | [3,125–3,150] |
| 6094 functions with 1,024 locals each | [4,865–4,920] | 3.22 | [3,572–3,587] |
| br_table, 2 × 131030 targets to one label | [11,132–11,245] | 169 | [3,130–3,213] |
| br_table, 12 × 4,096 distinct nested targets | [4,297–4,322] | 1.98 | [2,670–2,696] |
| 87361 nested blocks | [3,733–3,767] | 1.16 | [3,542–3,563] |
| 32760 if/else | [3,502–3,532] | 46.8 | [3,842–3,851] |
| 28942 distinct i64 constants | [2,541–2,567] | 162 | [707–713] |
| 131035 B of data + as much code (32 pages) | [1,068–1,076] | 2.59 | [984–990] |
| 14980 distinct function types | [9,378–9,429] | 1,145 | [10,467–10,554] |
| 87364 identical function types | [13,874–14,276] | 2.30 | [21,770–21,956] |
| 64 exports of one function | [19.2–20.5] | 3.94 | [18.5–18.7] |
| 131019 calls | [4,740–4,753] | 41.9 | [2,591–2,608] |
| 43679 far local.get/set pairs (1,024 locals) | [1,504–1,519] | 21.0 | [1,742–1,761] |
| 21 × (4,000 local.gets, local.set over them, 3,999 adds) | [2,128–2,144] | 87.1 | [2,205–2,221] |
| 16 × (4,000-deep stack, 1,000 local.sets) | [2,034–2,068] | 80.6 | [2,190–2,203] |
| 43680 dead-code units after unreachable | [1,333–1,338] | 2.72 | [1,872–1,894] |
| 256 globals, 52160 global.get/set pairs | [1,502–1,508] | 47.6 | [2,122–2,139] |

**Summary.** Schedule v0: LOAD_BASE 10,000, LOAD_PER_BYTE 40; upload 50,000 + 71 per byte.

| engine | worst ns/B, any size | worst ns/B, modules > 100 KB | implied LOAD_PER_BYTE (worst > 100 KB) | minimal module's load (implied LOAD_BASE) | worst upload (validate + load), modules > 100 KB |
|---|---:|---:|---:|---:|---:|
| soroban-wasmi | minimal program (53.6 ns/B) | 87364 identical function types (53.1 ns/B) | 21.2 | 2.73 µs = 1,094 fuel | 87364 identical function types (136 ns/B = 54.5 fuel/B) |

### Upload, ns per fuel (go/no-go)

Each upload is `upload::check` (validation, a fresh soroban-wasmi engine, the test translation and the engine's drop) plus the code's hash, as the node runs `UploadCode` on new code, over `upload_fuel(bytes, 0)` = 50,000 + 71 × bytes. Upload burns no engine fuel, so this is wall time over the price. Schedule v0's 71 per byte is the most that keeps a maximum upload within `MAX_TX_FUEL`, so it has the schedule's thinnest margin; values above 2.5 ns/fuel are marked **bold**.

| module | bytes | upload µs | upload fuel | ns/fuel |
|---|---:|---:|---:|---:|
| guest counter.wasm | 565 | 38.0 | 90,115 | 0.42 [0.42–0.43] |
| guest token.wasm | 2,467 | 93.9 | 225,157 | 0.42 [0.42–0.42] |
| guest escrow.wasm | 3,389 | 112 | 290,619 | 0.39 [0.38–0.39] |
| guest router.wasm | 1,484 | 59.9 | 155,364 | 0.39 [0.38–0.39] |
| minimal program | 51 | 13.4 | 53,621 | 0.25 [0.25–0.25] |
| minimal program + 20 imports | 621 | 40.7 | 94,091 | 0.43 [0.43–0.43] |
| one function, straight-line i64 arithmetic | 262,141 | 3,157 | 18,662,011 | 0.17 [0.17–0.17] |
| 5040 small functions (≈40 B bodies) | 262,137 | 7,465 | 18,661,727 | 0.40 [0.40–0.40] |
| 6094 functions with 1,024 locals each | 262,099 | 8,551 | 18,659,029 | 0.46 [0.46–0.46] |
| br_table, 2 × 131030 targets to one label | 262,143 | 14,934 | 18,662,153 | 0.80 [0.80–0.81] |
| br_table, 12 × 4,096 distinct nested targets | 244,343 | 7,307 | 17,398,353 | 0.42 [0.42–0.42] |
| 87361 nested blocks | 262,142 | 7,407 | 18,662,082 | 0.40 [0.40–0.40] |
| 32760 if/else | 262,143 | 7,605 | 18,662,153 | 0.41 [0.41–0.41] |
| 28942 distinct i64 constants | 258,321 | 3,620 | 18,390,791 | 0.20 [0.20–0.20] |
| 131035 B of data + as much code (32 pages) | 262,143 | 2,272 | 18,662,153 | 0.12 [0.12–0.12] |
| 14980 distinct function types | 223,362 | 20,786 | 15,908,702 | 1.31 [1.30–1.31] |
| 87364 identical function types | 262,144 | 35,281 | 18,662,224 | 1.89 [1.89–1.90] |
| 64 exports of one function | 609 | 50.7 | 93,239 | 0.54 [0.54–0.55] |
| 131019 calls | 262,143 | 7,567 | 18,662,153 | 0.41 [0.40–0.41] |
| 43679 far local.get/set pairs (1,024 locals) | 262,140 | 3,655 | 18,661,940 | 0.20 [0.19–0.20] |
| 21 × (4,000 local.gets, local.set over them, 3,999 adds) | 252,148 | 4,547 | 17,952,508 | 0.25 [0.25–0.25] |
| 16 × (4,000-deep stack, 1,000 local.sets) | 256,064 | 4,507 | 18,230,544 | 0.25 [0.25–0.25] |
| 43680 dead-code units after unreachable | 262,140 | 3,404 | 18,661,940 | 0.18 [0.18–0.18] |
| 256 globals, 52160 global.get/set pairs | 262,144 | 3,830 | 18,662,224 | 0.21 [0.20–0.21] |

**Upload go/no-go: GO.** Worst: 87364 identical function types (262,144 B), 1.89 ns per fuel against 2.5 (53.8 fuel per byte against the price's 71).

## 3. Instantiation

Per module, three ways: **raw** (`BlockEngine::invoke` of an empty `bk_noop` with a host that refuses every import: the engine's store, linking, instance and drop), **tx** (a top-level transaction calling `bk_noop` through `execute`: also the executor's fixed cost) and **call_v1** (each call of a loop `call_v1`s `bk_noop` on the module: the repeatable pattern, charged `call` + `instantiate_fuel`). *Schedule* = `instantiate_fuel` (meter.rs: a base, plus a price per import, export, function, global, element segment and element, data segment, page and data byte). Time ÷ 2.5 ns is the fuel it would need.

| module | bytes | schedule fuel | soroban-wasmi raw µs | soroban-wasmi tx µs (ns/fuel) |
|---|---:|---:|---:|---:|
| 1 page, no imports | 52 | 5,015 | 0.97 | 2.61 (0.52) |
| 1 page, 20 imports | 622 | 11,015 | 6.41 | 7.89 (0.72) |
| 32 pages | 52 | 67,015 | 26.9 | 31.5 (0.47) |
| 32 pages, 20 imports, + touch every 4 KiB | 667 | 73,530 | 30.8 | 35.4 (0.48) |
| data: 1 segment of 120,000 B | 240,075 | 127,050 | 3.35 | 5.20 (0.04) |
| data: 1,000 segments of 1 B | 8,001 | 26,030 | 14.2 | 16.0 (0.61) |
| data: 1,000 segments of 120 B | 246,999 | 147,030 | 17.3 | 19.9 (0.14) |
| 256 globals | 1,337 | 10,135 | 4.01 | 5.62 (0.55) |
| table of 4,096, 1,000 element segments | 10,050 | 63,015 | 38.8 | 41.0 (0.65) |
| 5040 functions | 262,137 | 80,615 | 45.0 | 46.5 (0.58) |
| 65 exports | 610 | 36,515 | 10.3 | 12.0 (0.33) |
| all: 20 imports, 32 pages, 256 globals, table + 1,000 elems, 2,001 funcs, 1,000 data, 65 exports | 123,409 | 218,635 | 130 | 125 (0.57) |

**Repeated instantiation through `call_v1`** (per call: the measured fuel, then time and ns per fuel):

| callee | schedule fuel per call | soroban-wasmi fuel: µs (ns/fuel) |
|---|---:|---:|
| 1 page, no imports | 25,115 | 25,205: 1.92 (0.08) |
| 1 page, 20 imports | 31,115 | 31,205: 7.25 (0.23) |
| 32 pages | 87,115 | 87,205: 24.9 (0.29) |
| 32 pages, 20 imports, + touch every 4 KiB | 93,630 | 93,720: 35.7 (0.38) |
| data: 1 segment of 120,000 B | 147,150 | 147,240: 4.45 (0.03) |
| data: 1,000 segments of 1 B | 46,130 | 46,220: 15.1 (0.33) |
| data: 1,000 segments of 120 B | 167,130 | 167,220: 18.9 (0.11) |
| 256 globals | 30,235 | 30,325: 4.87 (0.16) |
| table of 4,096, 1,000 element segments | 83,115 | 83,205: 40.7 (0.49) |
| 5040 functions | 100,715 | 100,805: 45.6 (0.45) |
| 65 exports | 56,615 | 56,705: 11.7 (0.21) |
| all: 20 imports, 32 pages, 256 globals, table + 1,000 elems, 2,001 funcs, 1,000 data, 65 exports | 238,735 | 238,825: 133 (0.56) |

**Touching the memory**: a transaction whose method stores to every 4 KiB of its 2 MiB, so the zeroed pages the allocator handed out lazily are faulted in (the cost an instantiation's per-page price must cover):

| module | soroban-wasmi tx fuel: µs (ns/fuel) |
|---|---:|
| 32 pages, 20 imports, + touch every 4 KiB | 92,480: 41.0 (0.44) (+5.64 µs over the empty tx) |

## 4. Host functions (ABI v1)

Each case loops one host function N times inside a program (`bk_<case>`) through the real executor and `MemHost`; per call = (time of N − time of 0) / N, so the call's Wasm side (its arguments, the `call` itself at 10 fuel, the loop's ~9) is included, as a program would pay it. Cases bounded per transaction (events, keys written, bytes written) run that many calls a transaction. Fuel per call is measured (the schedule's charge plus that Wasm side). `MemHost` is a `BTreeMap`, not the node's state: storage costs here are the overlay's and the host's, not redb's or the tree's (those are commit-time).

| function, case | fuel per call | soroban-wasmi ns/call (ns/fuel) |
|---|---:|---:|
| `input_len_v1`  | 160 | 82.8 (0.52) |
| `input_read_v1` 44 B input | 204 | 85.9 (0.42) |
| `input_read_v1` 32 KiB input | 32,928 | 479 (0.01) |
| `return_data_v1` 0 B | 163 | 88.9 (0.55) |
| `return_data_v1` 4 KiB | 4,259 | 196 (0.05) |
| `debug_log_v1` 0 B | 2,163 | 85.2 (0.04) |
| `debug_log_v1` 64 KiB | 526,451 | 86.1 (0.00) |
| `abort_v1` 256 B message (one per tx: tx time − an empty tx's) | 1,283 | 614 (0.48) |
| `storage_read_v1` absent, 32 B key | 1,204 | 141 (0.12) |
| `storage_read_v1` 32 B key, 8 B value | 1,212 | 178 (0.15) |
| `storage_read_v1` 128 B key, 8 B value | 1,308 | 183 (0.14) |
| `storage_read_v1` 32 B key, 16 KiB value | 17,588 | 642 (0.04) |
| `storage_write_v1` overwrite, 32 B key, 0 B value | 5,492 | 191 (0.03) |
| `storage_write_v1` overwrite, 128 B key, 8 B value | 6,532 | 232 (0.04) |
| `storage_write_v1` overwrite, 32 B key, 16 KiB value (64 a tx) | 169,332 | 779 (0.00) |
| `storage_write_v1` new keys, 32 B key, 8 B value (256 a tx) | 5,582 | 510 (0.09) |
| `storage_remove_v1` absent | 3,166 | 132 (0.04) |
| `storage_remove_v1` present, 16 KiB value (256 a tx) | 3,176 | 1,726 (0.54) |
| `self_address_v1`  | 360 | 83.1 (0.23) |
| `caller_v1`  | 360 | 83.1 (0.23) |
| `value_v1`  | 360 | 83.0 (0.23) |
| `block_height_v1`  | 360 | 83.3 (0.23) |
| `self_balance_v1`  | 360 | 102 (0.28) |
| `transfer_v1` to an existing account | 10,169 | 138 (0.01) |
| `transfer_v1` opening an account (OPEN_DEDUCT) | 10,179 | 733 (0.07) |
| `call_v1` callee 1 page, empty method | 27,050 | 2,604 (0.10) |
| `call_v1` callee 1 page, 32 KiB input | 59,818 | 3,011 (0.05) |
| `call_v1` callee 32 pages, empty method | 89,050 | 30,505 (0.34) |
| `try_call_v1` callee traps (layer dropped) | 27,050 | 2,528 (0.09) |
| `last_return_v1` 0 B | 160 | 85.6 (0.53) |
| `last_return_v1` 4 KiB | 4,256 | 136 (0.03) |
| `last_return_len_v1`  | 160 | 82.7 (0.52) |
| `last_abort_code_v1`  | 160 | 82.7 (0.52) |
| `emit_v1` 8 B name, 0 B data (64 a tx) | 2,233 | 175 (0.08) |
| `emit_v1` 32 B name, 16,352 B data (1 a tx) | 133,241 | 1,343 (0.01) |

**Per byte**, from the two sizes of each function (Δtime / Δbytes, against Δfuel / Δbytes):

| function: sizes | fuel per byte | soroban-wasmi ns/B (ns/fuel) |
|---|---:|---:|
| `input_read_v1`: 44 B input → 32 KiB input | 1.00 | 0.01 (0.01) |
| `return_data_v1`: 0 B → 4 KiB | 1.00 | 0.03 (0.03) |
| `debug_log_v1`: 0 B → 64 KiB | 8.00 | 0.00 (0.00) |
| `storage_read_v1`: 32 B key, 8 B value → 32 B key, 16 KiB value | 1.00 | 0.03 (0.03) |
| `storage_read_v1`: 32 B key, 8 B value → 128 B key, 8 B value | 1.00 | 0.05 (0.05) |
| `storage_write_v1`: overwrite, 32 B key, 0 B value → overwrite, 32 B key, 16 KiB value (64 a tx) | 10.0 | 0.04 (0.00) |
| `call_v1`: callee 1 page, empty method → callee 1 page, 32 KiB input | 1.00 | 0.01 (0.01) |
| `last_return_v1`: 0 B → 4 KiB | 1.00 | 0.01 (0.01) |
| `emit_v1`: 8 B name, 0 B data (64 a tx) → 32 B name, 16,352 B data (1 a tx) | 8.00 | 0.07 (0.01) |

## 5. An adversarial block

A `max_block_fuel` block is 60,000,000 fuel (3 transactions of `program_max_tx_fuel`, 20,000,000 each). The cost of a block made entirely of one pattern is **60M × the pattern's ns/fuel**: that is how the design sets the budget (60M × 2.5 ns = 150 ms). Each ns/fuel below is measured as in sections 1–4 (a bounded loop, not a literal 60M-fuel run, which would just be the same number × 60M); the projection is then checked against one real end-to-end 60M block per engine at the end. 150 ms is the budget; a block over it is **bold**.

| engine | block of this pattern | ns/fuel [min–max] | projected ms [min–max] | vs 150 ms |
|---|---:|---:|---:|---:|
| soroban-wasmi | compute: pointer chase, 2 MiB, random | 1.24 [1.23–1.26] | 74 [74–75] | 0× |
| soroban-wasmi | compute: br_table 8 targets, fixed index | 1.08 [1.07–1.08] | 65 [64–65] | 0× |
| soroban-wasmi | instantiation: call_v1 loop to the many-exports callee | 0.20 [0.20–0.21] | 12 [12–12] | 0× |
| soroban-wasmi | instantiation: call_v1 loop to the 5,040-function callee | 0.47 [0.47–0.47] | 28 [28–28] | 0× |
| soroban-wasmi | instantiation: call_v1 loop to a 32-page callee | 0.30 [0.27–0.38] | 18 [16–23] | 0× |
| soroban-wasmi | storage_write storm: 64 × 16 KiB overwrites a tx | 0.01 [0.01–0.01] | 0 [0–0] | 0× |
| soroban-wasmi | load: distinct "87364 identical function types" × 5 (262,144 B each) | 1.36 [1.36–1.39] | 82 [81–83] | 1× |
| soroban-wasmi | load: distinct minimal programs × 4983 (51 B each) | 0.23 [0.22–0.23] | 14 [13–14] | 0× |

**End-to-end check**: one real `max_block_fuel` block (three 20M-fuel transactions, each run until out of fuel) of each engine's worst compute pattern, measured whole, against the projection:

| engine | worst compute pattern | measured ms [min–max] | projected ms | vs 150 ms |
|---|---:|---:|---:|---:|
| soroban-wasmi | pointer chase, 2 MiB, random | 68 [67–68] | 74 | 0× |

## 6. Cold versus warm, and an engine's cost per block

**Cold** = the first transaction on a fresh block engine that has just loaded the code (engine and load outside the clock); **warm** = the same transaction again on an engine that has run it 10 times. Fuel must be identical (asserted for every repetition).

| transaction | fuel (identical cold and warm) | soroban-wasmi cold / warm µs |
|---|---:|---:|
| counter increment | 76,273 | 43.8 / 38.0 (+15%) |
| token transfer | 100,486 | 45.1 / 39.1 (+15%) |
| escrow open (payable) | 92,801 | 52.7 / 43.4 (+22%) |
| loop: call, 1,024 locals × 1,000 | 2,657,209 | 888 / 932 (-5%) |
| loop: i64.div_u by 1, chain × 1,000 | 422,984 | 317 / 314 (+1%) |
| loop: pointer chase, 2 MiB, random × 1,000 | 3,577,348 | 2,819 / 2,807 (+0%) |
| call_v1 × 100 to a 32-page callee | 8,929,879 | 3,051 / 3,330 (-8%) |

**An engine's cost per block**: creating a block engine (the engine, its linker with the 20 host functions), loading N codes (each a distinct hash: translated anew) and dropping it.

| engine | what | create + load µs | drop µs |
|---|---:|---:|---:|
| soroban-wasmi | create an empty engine | 6.29 | drop 2.50 |
|  | 1 × guest token.wasm | 42.4 (42.4 per code) | drop 6.92 (6.92 per code) |
|  | 10 × guest token.wasm | 409 (40.9 per code) | drop 15.3 (1.53 per code) |
|  | 100 × guest token.wasm | 4,100 (41.0 per code) | drop 126 (1.26 per code) |
|  | 1000 × guest token.wasm | 41,055 (41.1 per code) | drop 1,963 (1.96 per code) |
|  | 1 × 256 KB straight-line | 1,375 (1,375 per code) | drop 53.5 (53.5 per code) |
|  | 10 × 256 KB straight-line | 13,972 (1,397 per code) | drop 507 (50.7 per code) |

## 7. Native stack

The smallest thread stack on which the scenario runs as expected, bisected to 4 KiB (the OS rounds a thread's stack up to its page: 16 KiB on this Mac) in child processes (a stack overflow aborts the process). The thread both loads the code (a fresh block engine) and executes the transaction, as the executor thread will. Programs p1→…→p8 each `call_v1` the next; with recursion, each first recurses 1,000 Wasm frames (wasmi keeps Wasm frames off the native stack: the numbers should not move). Release build.

| scenario | soroban-wasmi |
|---|---:|
| one program, no recursion | 16 KiB |
| depth 8, no Wasm recursion | 16 KiB |
| depth 8, 1,000 Wasm frames per program | 16 KiB |
| depth 9 attempted (CallDepth), 1,000 Wasm frames per program | 16 KiB |

_Total run time: 1218 s._
