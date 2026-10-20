# Programs: implementation plan

Status: plan, 2026-10-19, against main at `2bbecde` (after v0.5.2). It turns the design in
[PROGRAMS.md](PROGRAMS.md) (revision 3, 2026-10-19) into milestones and pull requests. The design
says what to build. This plan says:
- in what order to build it;
- in which PRs;
- behind which gates;
- what has to be true before each step.

Section 2 lists where the design's revision 2 had drifted from the code; revision 3 corrects
those. Line references are to `2bbecde`, whose `node/`, `sdk-ts/`, `mcp/` and `web/` are the same
as v0.5.2's (`397061c`).

---

## 1. Summary

- **Shape.** Seven stages:
  1. Decide six things now (section 3).
  2. **Phase 0.** A six-week engine spike and its measurements, on a branch. At the same time,
     prerequisites that also help v1 merge to main.
  3. **Phase 1, the core.** Six milestones, each ending in something that runs on `blockula dev`.
  4. **Phase 2, tooling.** Parallel lanes, starting from the first milestone.
  5. **Phase 3, the testnet-2 upgrade.** Protocol v2, then `ActivateFeature { programs }`, then at
     least 8 weeks of public testing.
  6. **Phase 4, the audit.** Claude runs it with Fable agents: continuously on every consensus
     PR, at three milestones, and in full once the formats freeze.
  7. **Phase 5, mainnet.** Programs go live as soon as they're ready: on at genesis if they're
     ready first, otherwise switched on by a council vote proposed the day the last gate closes.
- **Size.** About 50 PRs:
  - 8 prerequisites;
  - 21 core PRs, holding about 10–11k lines of consensus-critical code;
  - 8 tooling lanes of several PRs each;
  - the testnet operations.

  The design's estimate stands: ~24k production lines, ~14k test lines, 18–22 engineer-months.
- **How main stays releasable.** Everything merges to main as it lands, never onto a long-lived
  branch. Three rules keep testnet-2 releases unaffected throughout (section 4):
  - every new behavior is a function of the protocol version in force;
  - a binary supports v2 only on chains born at v2 (dev and summoned chains), until the release
    that ships programs;
  - every PR replays real v1 history and must reach identical roots (P2).
- **Calendar:**
  - formats frozen about May 2027;
  - audit closed about June 2027;
  - testnet-2 activation about June 2027;
  - ready for mainnet about mid-August 2027, once the 8 weeks have passed.

  The critical path isn't writing code. It's calibration, the testnet soak, and one maintainer
  reviewing every consensus PR. With no external audit there's no audit queue to wait for.
- **Testnet history is safe.** testnet-2's public node keeps 14 days of blocks (`/api/status`
  `pruning.keep_blocks` = 1,209,600; validators keep ~2 days), so its history from genesis would
  have started disappearing around 2026-11-01. P0 archives it, from block 1, and runs daily, and
  P2's nightly replay keeps a copy CI reads ([REPLAY.md](REPLAY.md)).

---

## 2. What changed since the design

Revision 2 of the design was written against `ff1cbc4`. Twenty commits later (revision 3 applies
each correction):

| The design says | The code now | What the plan does |
|---|---|---|
| Schema 8 today, and v2 moves it to 9 (14.4) | `SCHEMA_VERSION` is already 9 (`state/codec.rs:55`) | v2 moves 9 → 10, and `SUPPORTED_SCHEMAS` becomes `9..=10` |
| "Every v2 behavior is a function of `version_at(h)`" | Nothing in execution branches on the version. `version_at` stamps and checks block headers (`consensus.rs:595`, `:714-719`), halts a node before a version its binary can't run (`upgrade.rs:386`, `:482`) and bounds a `ScheduleUpgrade`'s number (`upgrade.rs:240`). `BlockInfo` has no version (`chain.rs:664`). MAINNET-DESIGN 3.9 describes gating that no rule uses yet | P7 plumbs the version into admission, execution, `end_block` and the mempool before any gated behavior lands. Programs are the chain's first version-gated behavior |
| testnet-2 "launches soon" (14.3) | Live since 2026-10-18 at v0.5.2: 4 validators and a public node that prunes at 14 days | The v1 → v2 upgrade is a real live-chain migration. Archive the history now (P0) |
| The floor machine is Fly `performance-4x` (16 GB) | testnet-2's validators are `shared-cpu-2x` with 2 GB ([deploy/README.md](../deploy/README.md)) | Resize the validators before activation (O1) |
| Line numbers throughout | ~5k lines in `node/src`, `sdk-ts/src`, `web/lib/explorer` and `mcp/src` changed since | P1 re-cites them in revision 3 |
| New parameters | A key missing from `COUNCIL_KEYS` is tokens-only (`params.rs:259`), and testnet-2's token vote can't reach quorum until about April 2027 | Program parameters live in `ProgramParams` with their own classes (design 14.2), so the council can tune testnet-2. Tokens-only drills run on a separate chain (O8) |
| Vector coverage | `sdk-ts/test/tx-vectors.ts:15` hard-codes the tag list `[...Array(55).keys(), 60, 61]`. `web/lib/v1-vectors.json` is never regenerated in CI | C4 extends the list. T7 adds a staleness check |
| Confirmed unchanged | `KNOWN_FEATURES = ["ghost"]`; `GhostPause`; tags 55–59 reserved and 62+ free; namespaces 66+ free; `MAX_TX_BYTES` = 64 KiB; journal rollback with the fee kept; proposers include only `Applied`; execution runs inline in the executor's async task; no `catch_unwind` in the node; no receipts or events; `/api/tx` has no balance changes | As designed |

---

## 3. Decisions

### Decided 2026-10-19 (the maintainer)

| # | Decision | Decided |
|---|---|---|
| N1 | Build on PROGRAMS.md revision 2: wasmi, Rust programs, and the specification in sections 4–14 | **Yes** |
| N2 | Where the work lands | **Main, behind version gates, with v2 dev-only until the release** (section 4) |
| D2 | When programs go live on mainnet | **As soon as they're ready.** "Ready" means: the formats are frozen (M6), the audit has closed with no open critical or high finding (section 9), and testnet-2 has run them for at least 8 weeks (section 8). If that happens before mainnet's genesis, the genesis lists `programs` in `features`, so they're on from block 1. Otherwise the council's activation vote opens the day the last gate closes and takes about 9 days (section 9.2). Either way the mainnet binary carries v2 |
| D3 | Who audits | **Claude, with Fable agents, not an external firm. Blockula is an experiment.** Section 9 describes the audit. The docs label programs "reviewed by AI agents, not externally audited", the same honesty as Ghost Mode's "NOT AUDITED" banner |
| N3 | Calibration hardware | **The existing stack** (below) |
| N4 | Archive testnet-2's history | **Yes. Done**: P0 runs daily |

**Does the existing stack cover calibration (N3)? Yes:**
- **x86 timing:** an ephemeral Fly `performance-4x` machine with 16 GB
  ([OPERATIONS.md](OPERATIONS.md)'s floor; Fly's default for that size is 8 GB). The bench job
  starts it and destroys it afterwards, so it costs minutes of machine time, not a standing server.
- **aarch64:** GitHub's `ubuntu-24.04-arm` runners, which `release.yml` already builds on and
  which are free for this public repo. Fly offers no ARM machines. These runners carry the
  cross-architecture correctness checks (identical receipts, fuel and state roots) and the
  aarch64 timing. Hosted runners vary from run to run, so the aarch64 gate gets a 20% margin
  and a failure is re-run once before it counts.
- **The maintainer's M4 Max** is for development only. It's several times faster than the floor and
  runs macOS, so it never sets a price.

**And the software stack:**
- **The engine** is a pure-Rust crate. `node/vm` sits beside the node the way `node/ghost`
  does.
- **The SDK, MCP, CLI, wallet and explorer** grow along the patterns they already use.
- **Two things are new:**
  - publishing the Rust program crates to crates.io, with trusted publishing like npm's;
  - a pinned builder image for reproducible program builds, which can reuse `release.yml`'s
    pinned `rust:1.98.1-bookworm` image.
- **Hosting is unchanged.** Vercel and the explorer stay as they are, and Fly still runs the
  testnet.
- **Testnet machines grow.** testnet-2's validators move up to the floor spec before activation
  (O1), and the testnet's cost rises with them.

### Settled by phase 0

- **D1, the engine pin:** wasmi 2.0.x, soroban-wasmi 29.x or wasmi 0.38.0, chosen from the
  measurements in S5's memo.
- **D3 changes the default.** The design picked wasmi 2.0 on the condition that it got a scoped
  external review, with soroban-wasmi as the fallback otherwise. With no external review,
  **soroban-wasmi 29.x is now the default.** It's the engine a production chain runs today, and
  its line was audited. wasmi 2.0 has never been audited and no chain runs it. Phase 0 picks
  wasmi 2.0 only if its measured advantages are large and S4's fuzzing of it comes back clean.
  Either way the engine sits behind `engine.rs`, so this choice doesn't touch the ABI, the
  fees, the state or the tools.

### Built as the design recommends, cheap to change until the freeze

- **D4–D19** (PROGRAMS.md section 15). Each is a constant or a handler choice until M6 freezes
  the formats, so they can be revisited while the core is built.

---

## 4. Rules for every PR

1. **Below v2, a v2 binary is a v1 binary.** Every behavior change is a function of
   `version_at(h)`. Records, encodings and outcomes below the activation height are
   byte-identical to v1's. P2's replay gate checks this on every PR.
2. **v2 stays dev-only until the release.**
   - `supported_version()` returns 2 only for a chain whose genesis schedules v2
     (`Genesis::upgrades`, from block 1: dev and test chains a binary summons; `blockula dev`
     from C10), and 1 for every other chain. A public network's genesis can't schedule one
     (`blockula genesis build` has no `[[upgrade]]`): a preview version's rules change between
     releases.
   - So testnet-2's blocks keep signaling 1, and nobody can schedule v2 there by mistake. O3's
     release flips this for testnet-2.
   - Mainnet's genesis refuses v2 until the audit closes.
3. **Formats get their own PRs.** Tx tags, state records, the receipt, the interface and the host
   ABI are consensus formats. Each lands in one PR with Rust-written vectors that TypeScript
   checks, and a fuzz target. After M6 they change only with a new protocol version.
4. **Two places, one encoding.** Each pair must match byte for byte:
   - `node/src/types.rs` and `sdk-ts/src/tx.ts`;
   - `node/vm/src/interface.rs` and `sdk-ts/src/program.ts`;
   - the receipt codec in Rust and in TypeScript.

   Rust writes the vectors and both sides check them, as with `tx-vectors.json` and
   `proof-vectors.json` today.
5. **Consensus PRs carry their own tests** ([MAINNET-DESIGN.md](MAINNET-DESIGN.md) section 2):
   - a simulator scenario for anything that touches consensus;
   - a fuzz target for anything that parses or commits;
   - `check_invariants` coverage;
   - the fuzz harness's module list kept in step (`node/fuzz/check-sync.sh`).
6. **Reviewable.** Keep production code to about 1,500 lines a PR. Every consensus PR's
   description has a "what's inert below v2" section and the replay gate's result. Run
   `/code-review` before asking for review.
7. **Programs, never contracts:** in code, docs, UI, errors and commit messages.

---

## 5. Phase 0: spike and prerequisites (November to mid-December 2026)

Two tracks run at once. The spike lives on a branch and isn't merged. The prerequisites merge to
main and ship to testnet-2 in ordinary releases. The format PRs C3, C4 and C5 can start in this
phase too, because they don't depend on the engine.

### 5.1 Prerequisites (merged to main)

| PR | What | Where | Done when |
|---|---|---|---|
| **P0** ✅ | Archive testnet-2's history | [`scripts/archive-chain.mjs`](../scripts/archive-chain.mjs). It pages `/api/sync` and keeps each batch as served, with its certificate, after checking every block's height. Running since 2026-10-19 into `~/.blockula/archive/blockula-testnet-2/` (on the maintainer's Mac, outside Fly), daily at 09:20 through the launchd job `com.blockula.archive-chain`. **CI's copy (P2):** the closed segments are staged on GitHub's container registry, the cheaper of the two options (an archive node's volume and nightly downloads grow without end). The nightly replay builds that copy itself from the public node with this script, so the launchd job is unchanged and the Mac's copy stays the backup ([REPLAY.md](REPLAY.md)) | Done: heights 1–99,093 on the first run, and new ones added daily. The script is committed with P1 |
| **P1** | Land the design | `docs/PROGRAMS.md` revision 3: section 2's corrections, re-cited lines, the decisions recorded. MAINNET-DESIGN gets a section 7 that points to it, plus tags 62–70 and namespaces 66–73 (74–79 reserved) in its registries. This plan becomes `docs/PROGRAMS-PLAN.md`. The "contracts" wording is renamed: EXPLORER.md, the `web/lib/explorer/programs.ts` header, the `/programs` page's "Your contract here", the glossary, AGENTS.md/CLAUDE.md line 108, PLAN.md | Merged. No code |
| **P2** ✅ | The v1 replay gate | [REPLAY.md](REPLAY.md). `blockula replay` runs an archive through the node's own paths: sync (`node::verify_batch_trusted`: certificates and the embedded checkpoints; then `node::execute_batch`, each block after `consensus::verify`'s own checks that need no round or clock: version, spacing, leader, cited root, txs), a block-by-block comparison with the recording (outcomes, roots, whole records), and startup (`Chain::replay`). **Per PR:** two recorded chains in `node/testdata/replay` (about 2,200 blocks) using every tag a block can carry (0–54, 60–61), recorded deterministically by `node/src/replay/corpus.rs`; the v0.5.2 source replays them identically. **Nightly** (`replay.yml`): testnet-2 from genesis, from the staged copy (P0) | Done: green on main; test-only divergences (an ungated admission rule, an ungated handler change, a state-only change, with and without a recording) fail it at the block they start, on both paths, and so do a block `verify` refuses and a real divergence (`MIN_NEW_ACCOUNT` raised) |
| **P3** | One executor thread | `consensus.rs`, `node.rs`. These paths move to one thread with a 64 MiB stack, fed by a channel: `verify`'s pass, proposal packing, finalized execution (`Executor::report`), sync, and startup replay. The API gets a pool with the same stack size. The node refuses to start without them | The simulator suite is green, and no execution path runs on a stack sized by the host |
| **P4** | A replay cache | `consensus.rs`: post-states of verified and proposed blocks, keyed by digest, at most `MAX_REPLAY` | Each block's txs run once in verification, however many descendants are verified. A simulator scenario with forks and view changes |
| **P5** | Packing off the chain lock | `consensus.rs` (`propose`) | HTTP submits and finalization never wait out a proposal's execution |
| **P6** | `POST /api/simulate` for native txs | `api.rs`, `view.rs`, SDK `client.simulate`. Optionally, the explorer's preflight uses it | Returns the outcome, the refusal reason and the balance changes from a `ReadView`. Off on validators unless `--serve-simulate` |
| **P7** ✅ | Version plumbing | `chain.rs`: `BlockInfo.version`, and the version reachable in `admit`, `apply_j`, `end_block`, `dry_run` and `MempoolCheck`. `upgrade.rs`: `supported_version()` and an optional genesis `upgrades` list. `genesis_builder.rs` | A test-only gated rule flips at its scheduled height on every path (execute, verify, propose, mempool, replay) and nowhere else |

### 5.2 Spike (branch `spike/programs-vm`, not merged)

| Item | What |
|---|---|
| **S1** | **An engine prototype.** `node/vm` with the design's configuration and upload validation (4.2), a minimal host (storage, transfer, call), the overlay (4.4), and an engine per block, on soroban-wasmi 29.x (the default, section 3) and wasmi 2.0.0 side by side. The same harness runs on wasmi 0.38.0 for reference |
| **S2** | **The guest toolchain.** `wasm32v1-none` on Rust 1.98.1 (`rust-toolchain.toml`). An escrow and a token written by hand against the ABI, with their sizes and float-free builds |
| **S3** | **Calibration** on N3's x86 and aarch64 machines. It measures: ns per fuel by operator class; translation and validation ns per byte; instantiation; every host function; ML-DSA verification; the adversarial corpus (4.5); cold-versus-warm fuel equality; an engine's cost per block; native stack per frame on P3's thread. This benchmark becomes C19's CI gate |
| **S4** | **Robustness.** wasmi and the Wasm spec suite with `overflow-checks`. Validation and execution fuzzed under AddressSanitizer for two weeks. A wasm-smith differential across engines and architectures |
| **S5** | **The go/no-go memo**, `docs/programs/phase0.md`. It settles D1's pin, the measured fuel schedule, `LOAD_PER_BYTE`, `CALIBRATED_BLOCK_FUEL`, `MAX_PAGES` and the executor's stack size, and lists any metering gap with its fix |

**Exit:** S5 merged; P0–P7 merged and running on testnet-2.

---

## 6. Phase 1: the core (mid-December 2026 to May 2027)

Six milestones. Each ends in a demo on `blockula dev`, which starts at v2 with `programs` on, so
progress is visible and testable long before testnet. The consensus-safety rules are in from M1,
not bolted on at the end:
- the mempool never runs the VM;
- the `catch_unwind` backstop;
- an engine per block;
- the executor thread.

### M1: Hello, program

**Demo:** a counter program uploaded, created and called on `blockula dev` from the CLI. Its
storage is proven with `/api/proof/69/…`, and the call's receipt against the block's receipts
root.

| PR | What | Key files | Tests |
|---|---|---|---|
| **C1** | VM core: engine configuration, upload validation, the versioned fuel schedule, the engine wrapper with its per-block lifetime | `node/vm/` (a new crate in its own workspace, pinned exactly to S5's pick: soroban-wasmi 29.x unless phase 0 picks wasmi 2.0); `node/Cargo.toml` (`exclude`, overflow-check profiles for every engine crate, and a test that they're all listed) | The Wasm spec suite under this configuration; the `program_validate` fuzz target |
| **C2** | Host ABI v1 and execution: the engine-neutral host table, written in full because it's the contract with `program-sdk`; frames; the meter; the overlay; `MemHost` | `node/vm/src/{abi,exec,testing}.rs` | Unit tests for each function on `MemHost`; the `host_abi` fuzz target |
| **C3** | The interface format: codec, checks, digests, selectors | `node/vm/src/interface.rs`; `sdk-ts/src/program.ts` (the codec only); `sdk-ts/test/interface-vectors.json` | Rust-written vectors checked by TypeScript; the `interface_codec` fuzz target |
| **C4** | Tx formats for tags 62–70 | `types.rs` (`encode`, `decode` with the nested refusals, `check_format`, `json_ints`, `kind`, `addresses`, `put_bytes`, `Tx::max_bytes`); `sdk-ts/src/{tx,describe}.ts`; `tx-vectors.json`; the tag list in `tx-vectors.ts`; the hash group in `e2e.ts`; `node/fuzz` (`program_tx_codec`, seeds, the program tags in `state_machine`) | Every tag round-trips. Every one is `Err` at admission below v2 |
| **C5** | State: namespaces 66–73, the `Programs` sub-state and its journal, the optional META `programs` record, `ByteKey`, schema 10, `begin_block`, `known_features(version)` | `state/{mod,codec}.rs`; `chain.rs` (`modules()` from 6 to 7, `absorb`, `rollback`); `snapshot.rs`; `upgrade.rs`; `proof_vectors.rs`; `sdk-ts/src/proof.ts` (`NAMESPACES`, decoders) | A v2 binary opens schema-9 state and snapshots. `begin_block` is idempotent on every path. Invariants hold |
| **C6** | Fuel and fees: the fuel ring; reservations, multisig cases included; load pricing; `check_fee` and `check_tip` in u128; verify's stateless Σ; the block counters; the fuel controller and its clamp; `TipRate`; a fuel section in `/api/fees` | `programs/fuel.rs`, `econ.rs`, `consensus.rs`, `chain.rs`, `api.rs` | 10,000 consecutive full blocks with no overflow. Fees agree across the activation boundary |
| **C7** | Upload and create: validation in a throwaway engine; `BlobRef`; the deposit curve and escrow accounting; program addresses; adoption; init; no program as a tx's `from` | `programs/{mod,lifecycle}.rs`; `chain.rs` (`admit`, the `apply_action` dispatch) | Mallory's front-run-then-remove case; an adopted balance counted once; vectors for program addresses |
| **C8** | Calls: the host over `&State` (input, return, abort, storage with deposits and `storage_limit`, context and chain reads, hashes); commit with the conservation and who-was-debited checks; `CallProgram` with batches; the `catch_unwind` backstop and an engine rebuilt after a fault; mempool paths that skip the VM; proposers including `Failed` program txs | `programs/{host,commit,exec}.rs`; `chain.rs` (`apply_j`'s `run_programs`); `consensus.rs` | The `program_exec` and `program_commit` fuzz targets; an injected fault compared against a fresh node |
| **C9** | Receipts: the codec and fail codes; the receipts root (namespace 70) and its sweep; `BlockMeta`; `/api/receipt/{tx}`; a program section in `/api/tx`, with facts kept apart from the program's claims | `programs/receipt.rs`, `store.rs`, `api.rs`; the SDK's receipt codec; receipt vectors | Rewording every programs error moves no receipt digest; the `receipt_codec` fuzz target |
| **C10** | Reads and a minimal CLI: `/api/program/{address}` with `/interface` and `/storage`; `/api/code/{hash}`; `POST …/view`; program txs in `/api/simulate`; `blockula program upload\|create\|call\|view\|show`; `blockula dev` at v2 | `api.rs`, `view.rs`, `program_cli.rs`, `main.rs`, `explorer.html` | A CLI run on a v2 dev chain in `local-testnet.sh` |

### M2: programs hold FANG

**Demo:** the escrow template. A deal opens with attached FANG and releases to a new account,
which pays its own opening fee.

| PR | What |
|---|---|
| **C11** | **Value and transfers.** `value` on payable methods, `self_balance`, and `transfer` with `OPEN_DEDUCT`/`OPEN_PAY`. Also `opened_by_program`, `value_held` by account kind, the value cap, and `check_locks` (locked FANG can't go in). From v2, `charge_new_accounts` checks a session key's limit |
| **C12** | **Native credits into programs.** `accepts_transfers` on `Transfer` and `AwardHunt`; `TreasurySpend` against the cap; `Unshield` refused in `ghost::apply_unsigned` |

### M3: programs compose

**Demo:** a BTS-1 token and a batch auction that settles in it, in one atomic batch, simulated
with a trace.

| PR | What |
|---|---|
| **C13** | **Cross-program calls:**<br>• `call`, `try_call` and `last_return`<br>• the declared program set and bindings<br>• version digests, checked at admission and at first run<br>• no reentrancy, a depth of 8, read-only `View` frames<br>• `Init` only from create<br>• payability |
| **C14** | **The rest of host ABI v1:**<br>• events and `debug_log` (dev chains)<br>• the beacon with its retention, and `resolve_name`<br>• `ml_dsa65_verify` in the program-message domain<br>• `verify_account_sig` (current owner keys only)<br>• `sha256` and `keccak256`<br>• `caller_auth` and `origin_session` |

### M4: lifecycle

**Demo:** on a dev chain, the token gets a 1-hour upgrade delay. A pending call to the old
version becomes unadmittable. The authority renounces, and an empty program closes.

| PR | What |
|---|---|
| **C15** | **Upgrades and authority:** `UpgradeProgram`; `ProgramAdmin`; `in_force` and `fold`; `PROGRAM_DUE` and folding in `end_block`; the `Additive` check |
| **C16** | **Removal:** `CloseProgram`, which leaves a tombstone that nothing anywhere can credit; `RemoveCode`, once its refs are 0 and its 1-day delay has passed |

### M5: agents and training wheels

**Demo:**
- An agent with a scoped key calls `swap` within its budget.
- The same key is refused on another method, on another program, and past its limit.
- f + 1 validators pause programs, and the pause lapses.

| PR | What | Tests |
|---|---|---|
| **C17** | **Scoped keys:**<br>• `AddScopedKey` (tag 70) through `AddKey`'s handler<br>• `KeyGrants` (namespace 71) with its deposit<br>• admission's session arm for `CallProgram`<br>• `spent` counting value and deposits<br>• `RemoveKey` deleting the grants | The action × signer matrix (design 9.1) |
| **C18** | **Training wheels and parameters:**<br>• `ProgramsPause` (tag 69): consensus keys, f + 1, a 3-day lapse, once per 30 days per validator<br>• exit-only under `DeactivateFeature { programs }`<br>• `programs_freeze`: tokens-only, a 30-day lapse, `council_may_pass` false<br>• `ParamChange` routing and classes for `program_*` keys<br>• a programs section in `/api/params` | Simulator: the pause lapses, and exit-only lets every user out |

### M6: hardening and the format freeze

**Exit:** the consensus formats are frozen:
- tags 62–70;
- namespaces 66–73;
- the receipt and the interface;
- host ABI v1;
- fuel schedule v1.

Tag that commit. After it, a format changes only with a new protocol version.

| PR | What |
|---|---|
| **C19** | **The worst-case gate.** S3's benchmark becomes a CI job on the floor machines, on both architectures, running the adversarial corpus. It adds three differentials (cold versus warm, two architectures, and `MemHost` versus the node) and nightly AddressSanitizer fuzzing |
| **C20** | **Simulator scenarios and a soak** (design section 13):<br>• max-fuel blocks at the floor spec<br>• a Byzantine proposer stuffing junk<br>• the replay cache under forks<br>• snapshot restore with program state<br>• a 1-of-1 multisig at the fuel limit<br>• v1 → v2 with programs off, then on<br>• the pause, exit-only and freeze lapses<br>• a million distinct code loads with flat memory |
| **C21** | **The mixed-binary harness.** v0.5.x release binaries and the candidate (with v2 forced on for its test chain) run on one local network through activation. It checks:<br>• roots agree below H<br>• v1 nodes halt with exit code 11<br>• restarts and log rebuilds work on both sides of H<br>• a v1 snapshot restores past H<br><br>P2's corpus also gains every gated pre-v2 behavior: tag 62–70 bytes, nested ones, `ActivateFeature { programs }`, a `program_*` `ParamChange`, and a session key opening accounts |

---

## 7. Phase 2: tooling (January to June 2027, parallel lanes)

| Lane | What | Can start after |
|---|---|---|
| **T1** | **The `program-sdk/` workspace,** with its own CI job building Wasm and native:<br>• `blockula-program`: a Soroban-shaped `Ctx`, collections, `Fang` and `Address`, an arena allocator that never grows memory, panic as abort, `transfer` and `transfer_opening`, `verify_account_sig`, `Randomness`, `AgentLimits`<br>• `blockula-program-macros`: generates the exports, Borsh dispatch and the interface from Rust<br>• `blockula-program-test`: a `TestChain` on `MemHost`, scenarios, native mode | C2 |
| **T2** | **The std library and templates:**<br>• BTS-1 `fungible`, `ownable`, `pausable`<br>• templates: counter (M1), escrow (M2), token and batch auction (M3), multisig vault<br>• `blockula program new` | T1, then per milestone |
| **T3** | **The full CLI:**<br>• `build` in a pinned builder image (reproducible) and `verify --source`<br>• `test`; `dev` (watch, rebuild, upgrade)<br>• `events`, `storage`, `upgrade`, `admin`, `close`, `remove-code`, `codegen --ts`<br>• signing commands pin the network like the existing ones | C10, C15 |
| **T4** | **The SDK and MCP reader release.** It decodes and describes tags 62–70, `KeyGrants`, receipts and program records, so released clients can read a v2 chain. **It ships at least 4 weeks before testnet-2's activation** | M6's freeze |
| **T5** | **`@blockula/sdk/program`:**<br>• Borsh arguments with `bigint`<br>• `versionDigest`, `programAddress`, selectors<br>• `client.program()` with `.view()` and `.simulate()`<br>• `wallet.call`, which sizes the call, builds it, re-simulates the exact bytes and refuses on any difference<br>• `provenProgram`, `provenProgramStorage`, `provenGrants`, `provenReceipt`<br>• helpers for tags 62–68 and 70<br>• e2e for every tag | C10, C13 |
| **T6** | **The MCP:**<br>• `program_show`, `program_view`, `program_simulate`, `program_call`, `program_events`, `program_upload`, `program_create`<br>• elicitation and the confirm digest<br>• `BLOCKULA_PROGRAMS` (empty by default) and `BLOCKULA_UNCONFIRMED_MAX`<br>• the refusals and the `programText` pass<br>• the spend ledger counting `storage_limit`<br>• `use_account` with programs | T5 |
| **T7** | **The web wallet and explorer:**<br>• deployed programs in `/programs`; `/program/{address}`; `/code/{hash}`; `Program.id` widened<br>• `specFromInterface`<br>• new `ParamType`s in `fields.ts` and `ActionForm`: `bool`, 64- and 128-bit integers as `bigint`, `hex`, `select`, `list`, `json`<br>• `ActionSpec.prepare` with re-simulation<br>• previews and receipts that keep facts apart from claims; 6-word fingerprints<br>• ledger and preflight for tags 62–70<br>• agent scopes in "Give an agent a budget"<br>• the e2e world, mock node and page list<br>• a CI check that `v1-vectors.json` is current | T5 |
| **T8** | **Docs and developer experience:**<br>• AGENTS.md/CLAUDE.md rows and rules<br>• GOVERNANCE.md keys<br>• OPERATIONS.md: memory, `--serve-simulate`, the replay policy, the schema change<br>• COUNSEL_BRIEF.md questions; PLAN.md known limits; `llms.txt`; the READMEs<br>• BUG_BOUNTY.md: the VM in scope from testnet-2's activation<br>• AUDIT_SCOPE.md: the tags 62–70 codec in engagement A; the VM and its integration are audited by AI agents (section 9), outside its scope<br>• `seed-dev.mjs` deploys the templates and a scoped agent<br>• `local-testnet.sh` exercises programs | Per milestone |

**Exit** (design 16.1):
- An outside developer ships a program built from a template on `blockula dev` in a day, from
  the docs alone.
- An agent calls a program through the MCP on a scoped key, confirmed by elicitation, end to
  end.

---

## 8. Phase 3: the testnet-2 upgrade (about June to August 2027)

| Step | What | Who |
|---|---|---|
| **O1** | **Size the testnet for programs.** Move the validators to the floor spec: they're `shared-cpu-2x` with 2 GB today, and program state alone can reach ~3 GiB of RAM at its ceiling (twice the default soft cap). Add one aarch64 validator | Operator. The testnet's cost rises |
| **O2** | **Ship T4's reader release** at least 4 weeks before activation | Maintainer |
| **O3** | **The programs release.** `supported_version()` returns 2 for testnet-2, while mainnet stays refused. Operators install it with `blockula release install` and signal 2 | The release session; operators |
| **O4** | **`ScheduleUpgrade { version: 2, activation_height }`** once ⅔ of validators signal. The council may pass it (`governance.rs:748-757`) | Council |
| **O5** | **At H.** `begin_block` adds META `programs` and moves the schema to 10, and v1-only nodes halt with exit code 11 | The chain |
| **O6** | **`ActivateFeature { programs }`** | Council |
| **O7** | **At least 8 weeks of testing:**<br>• Hunts for programs, and for breaking them<br>• the bug bounty covering the VM<br>• spam at the fuel limits; storage-cap and value-cap griefing<br>• chaos: cold caches, a lagging verifier, a node rebuilt from genesis across H | Everyone |
| **O8** | **Drills:**<br>• a pause and its lapse. It needs 2 of the 4 validators' consensus keys, so ask before using them<br>• exit-only and reactivation (council)<br>• a `program_max_block_fuel` change<br>• a v3 that recalibrates fuel and carries both schedules<br>• **the freeze is tokens-only**, and testnet-2 can't reach token quorum before its cliff (~2027-04-19). Even after it, a vote needs ~85% of the project's keys. So drill the freeze on a separate `summon`ed chain | Maintainer |

**Exit:**
- at least 8 weeks with no halt, divergence or manual intervention;
- calibration holds on the testnet's hardware;
- no open high finding.

---

## 9. Phases 4 and 5

### 9.1 The audit: Claude with Fable agents (D3)

The code is written mainly by one model, and Fable audits it. A different model, an
adversarial brief and executable evidence are what keep the audit from sharing the author's
blind spots. It runs at three levels.

| When | What | Output |
|---|---|---|
| **Every consensus PR** | Before review, a Fable pass over the diff checks the design and section 4's rules: version gating, determinism, conservation, panics, metering, encodings. Each finding is checked by reproducing it, then fixed in the PR or answered in it | The findings and how each was settled, in the PR description |
| **The end of M1, M3 and M5** | **A multi-agent workflow.** Fable auditors take one surface each:<br>• the VM crate<br>• the host ABI, overlay and commit<br>• fuel and fees<br>• state and the version gates<br>• lifecycle and authority<br>• grants and agent budgets<br>• the clients' preview path<br><br>Adversaries take the attacker's brief: steal or mint FANG, escape a lock or an agent budget, halt or split validators, bloat state cheaply, trick a preview. A second agent then has to make each finding fail with a test before it counts | `docs/audits/programs-m{1,3,5}.md`. Every confirmed finding gets a regression test |
| **After M6's freeze, before testnet-2's release** | **The full audit:**<br>• everything in the design's section 13 audit surface<br>• the v1 → v2 transition<br>• a read of the engine's enabled paths (translator, executor and fuel for MVP plus mutable globals) in the pinned upstream source<br>• the macros<br>• the TypeScript preview and re-simulation<br>• the MCP's elicitation<br><br>Its evidence includes the fuzzing campaigns, the differentials and C19's gate results. Fixes are re-verified by a fresh agent | `docs/audits/programs-<date>.md`, published like the October 2026 internal review of protocol v1. It's a gate: no open critical or high finding |

**Running it.** The milestone and final audits use the Workflow tool with Fable agents
(`claude-fable-5-1`), launched with the maintainer's go-ahead each time.

**What it can't replace, and the backstops that stay:**
- a model reading code misses some things however it's prompted. The fuzzing, the differentials
  and C19's gate are independent evidence, not opinion;
- the bug bounty covers the VM from testnet-2's activation on ([BUG_BOUNTY.md](BUG_BOUNTY.md), a
  draft that opens with the incentivized testnet, gains a programs row: T8);
- Hunts for breaking programs during the 8 weeks;
- the training wheels:
  - the 10M FANG value cap;
  - the f + 1 pause;
  - exit-only;
  - the freeze.

  They matter more without an external audit. Retire the cap only after a year on mainnet
  without an incident (design D4).

### 9.2 Mainnet (D2: as soon as they're ready)

- **The mainnet binary carries v2,** whichever path applies. The tx codec for tags 62–70 is
  reachable in it even while programs are off, so it joins the protocol's own review scope.
- **If programs are ready before mainnet's genesis,** the genesis lists `programs` in
  `features`. Today `chain.rs:516` refuses Ghost Mode in `blockula-1`'s genesis. Programs get no
  such refusal once their gates have closed, and the genesis builder checks that the audit
  report exists and that testnet-2's 8 weeks have passed.
- **If not,** mainnet launches at v2 with `programs` off, and the council proposes activating
  them the day the last gate closes. That vote takes about 9 days, and the bootstrap council can
  pass it only in mainnet's first year (after that, the ⅔ token vote).
- **If M6's format freeze lands after mainnet's own,** mainnet launches at v1 and upgrades to
  v2 as testnet-2 did.
- **Counsel's questions** (design 9.7) are still asked before activation. They're legal
  questions, not code review.

---

## 10. Schedule

| When | What |
|---|---|
| 2026-10-19 | Decisions made (section 3). P0's archive running |
| To 2026-10-31 | P1 merged |
| November to mid-December | Phase 0: S1–S5 and P2–P7. C3–C5 started |
| Mid-December to February | M1 (demo about late January; its audit), then M2 |
| February to April | M3 (its audit), M4, M5 (its audit) |
| April to May | M6: formats frozen |
| May to June | The full audit and its fixes |
| January to June | Tooling. T4 ships by about mid-May |
| June | testnet-2 activation |
| Mid-August | Its 8 weeks pass: programs are ready for mainnet (D2) |

**The critical path:** S3's calibration → C6's numbers → C19's gate → the freeze → the full
audit → the testnet release → 8 weeks. The review queue runs alongside it: about 30 consensus
PRs and one approver.

---

## 11. Execution risks (beyond the design's R1–R8)

| Risk | Mitigation |
|---|---|
| One reviewer for about 30 consensus PRs | The size rule, the "inert below v2" section, `/code-review` first, and milestone demos so behavior can be reviewed as well as diffs |
| Lost v1 history leaves the replay gate partial | P0 (done), and P2's nightly keeps a second copy, staged on GitHub's container registry and replayed before it's staged |
| Calibration hardware isn't representative | N3, then re-measure on O1's testnet machines |
| The audit shares the author's blind spots | A different model (Fable), adversarial briefs, a test required before any finding counts, and independent evidence (fuzzing, differentials, C19). The training wheels and the bug bounty stay (9.1) |
| An engine bug no review caught | soroban-wasmi by default (section 3). The engine-neutral ABI (C2) keeps a swap inside `engine.rs`. An exact pin and a watch on advisories |
| main keeps moving | Merge continuously, rebase every PR on main, and land formats in small PRs that merge fast |
| A v2 behavior wakes before activation | P7's test pattern, a below-v2 test for every gated rule, and C21 |

---

## 12. First steps

1. ~~Decide N1–N4, D2 and D3~~ (done, section 3).
2. ~~P0: archive testnet-2's history~~ (done).
3. P1: land PROGRAMS.md revision 3 and this plan in `docs/`, together with
   `scripts/archive-chain.mjs`. Revision 3 records D2 and D3 and updates LAUNCH-PLAN.md's
   "separate VM/host-interface audit" to match.
4. Start S1 (soroban-wasmi and wasmi 2.0 side by side) and P7 in parallel.
