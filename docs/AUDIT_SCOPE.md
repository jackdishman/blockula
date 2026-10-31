# Blockula security audit: scope and request for proposals

> **There's no mainnet** (decided Oct 22, 2026). Blockula is a kit for running your own ledger ([LAUNCH.md](LAUNCH.md)). This scope was written for the mainnet's audit; it still describes what an external audit of the protocol every Blockula chain runs would cover. MAINNET.md, the mainnet readiness list it cites, was removed with the plan (`git show a7288977:docs/MAINNET.md` shows it).

Blockula is a post-quantum layer-1 blockchain: ML-DSA-65 (FIPS 204) signatures for every transaction, vote and certificate, BLAKE3 hashing, and Commonware's simplex BFT consensus with an ML-DSA certificate scheme we wrote. This document is the scope for SEC-1 in MAINNET.md: the external audit of the frozen genesis commit, followed by fixes and a re-review. Send it to firms as the request for proposals.

The protocol being audited is specified in [MAINNET-DESIGN.md](MAINNET-DESIGN.md) ("protocol v1"). Background: [PLAN.md](PLAN.md) (architecture), MAINNET.md (the known gaps, with code citations), [OPERATIONS.md](OPERATIONS.md) (how it's run), [GENESIS_CEREMONY.md](GENESIS_CEREMONY.md).

## Engagements

| | Engagement A: protocol | Engagement B: clients |
|---|---|---|
| What | The node: consensus integration, block validity, the state machine, staking and slashing, governance, economics, sync, p2p admission, the HTTP API's effect on consensus, upgrades and halts, cryptographic use | The TypeScript SDK (signing, encoding, passkey key derivation), the passkey web wallet (origin security, WAL-3), the MCP server (agent wallets, spending caps) |
| Language | Rust (edition 2024) | TypeScript (Node ≥ 20, Next.js) |
| Size | see below | see below |
| Skills | BFT consensus, Rust, state machines with economic invariants, lattice signatures in practice | Web security, WebAuthn/passkeys, key handling in browsers, supply chain |
| Calendar | 6–10 weeks including fixes and re-review | 2–4 weeks |

One firm may bid on both. Ghost Mode (the shielded pool, DEC-2) gets its own audit before it's activated and is **not** in this scope. Programs (optional, off by default) have [their own scope below](#programs-optional-off-by-default).

## Commit and size

The audit runs on the **consensus-format freeze commit** (MAINNET.md section 5, the end of phase 1), which we'll tag `audit-1` and share at kickoff. Until then, the sizes below are a snapshot of `jackdishman/mainnet-prep` at `4309382` (before the protocol v1 packages land), counted with `wc -l` including inline tests; rerun at the freeze.

**Node (`node/src`), 10,875 lines today:**

| File | Lines | What | Priority |
|---|---|---|---|
| `chain.rs` | 2,603 | State, execution, mempool, admission, the state root, persistence | P0 |
| `poc.rs` | 1,843 | Proof of Contribution: claims, attestations, Séances (juries), payouts, vesting | P0 |
| `api.rs` | 1,294 | HTTP API, tx submission and forwarding, sync batches, metrics | P1 |
| `consensus.rs` | 935 | Commonware integration: block format (`CBlock`), propose, **verify**, execution acknowledgement, checkpoint rule, marshal and simplex wiring | P0 |
| `types.rs` | 798 | Tx and action types, **signing bytes**, format checks, fees | P0 |
| `node.rs` | 665 | Node startup, join and fast sync, double-sign guards, p2p config | P1 |
| `contrib_cli.rs`, `validator_cli.rs`, `main.rs` | 1,199 | CLIs | P2 |
| `sim.rs` | 408 | Deterministic simulator (test-only) | context |
| `pq.rs` | 324 | **The ML-DSA-65 certificate scheme** implementing Commonware's `Scheme` | P0 |
| `snapshot.rs` | 281 | Certified state snapshots and their verification | P1 |
| `epochs.rs` | 228 | Validator-set history, epoch handoffs | P1 |
| `github.rs` | 168 | GitHub checks for the contribution pool (an oracle) | P1 |
| `crypto.rs` | 129 | ML-DSA keys, addresses (bech32m of BLAKE3) | P0 |
| `explorer.html` | 454 | The block explorer, served by the node | P2 |

Protocol v1 adds, in new modules (estimates): staking, slashing and evidence (~2,000), governance, treasury and multisig (~1,500), fees, issuance, vesting and the genesis builder (~1,500), the randomness beacon (~800), upgrades, halts and slashing protection (~800), and the authenticated state store with proofs (~3,000). Expect **~20,000–25,000 lines of Rust** at the freeze, of which ~12,000 are P0.

**Clients, ~3,100 lines today:** `sdk-ts/src` 686 (`tx.ts` 253, `index.ts` 362, `passkey.ts` 71), `mcp/src` 327, `web/` ~2,100 (Next.js pages and components).

**Supply chain (review only, light):** `.github/workflows/`, `scripts/install.sh`, `scripts/release-build.sh`, `scripts/release-sig.mjs`, `scripts/check-release-files.sh`, `node/src/release_cli.rs` (`blockula release install|fetch|pin`, the operators' install path), `deploy/`, ~1,900 lines.

**Dependencies in scope for their use, not their internals:** Commonware `=2026.9.0` (`consensus::simplex` and `marshal` (BETA; the `Application` trait and `marshal::application` are ALPHA), `p2p::authenticated::discovery`, `stream`, `storage` journals and archives, `cryptography`, `codec`, `runtime`); `ml-dsa 0.1.1` (RustCrypto, self-described as never independently audited); `@noble/post-quantum 0.7.1` (self-audited); `blake3`, `bech32`. We'd like the firm's view on whether the Commonware modules we use are covered by Commonware's own audits, and on our use of both ML-DSA implementations (SEC-2).

## Threat model

**Assets:** FANG balances, escrows (Hunts, bonds, stakes, the contribution pool, the treasury); the integrity of names and accounts; consensus safety (one finalized chain) and liveness; validator, owner and user keys; the outcome of governance votes and juries.

**Trust assumptions:**
- Fewer than ⅓ of the active validators (by count: simplex quorums are count-based, DEC-3) are Byzantine in any epoch.
- At least ⅔ of the active set upgrades honestly when an upgrade is scheduled.
- ML-DSA-65 and BLAKE3 are secure. The p2p session key exchange is X25519 (classical) until CRY-1; a quantum adversary recording traffic today is a known, documented risk, not a finding.
- GitHub is honest about merges for the contribution pool (it's an oracle; a manipulated GitHub is in the threat model only through what validators attest).

**Adversaries:**
1. **Byzantine validators** (< ⅓): equivocating; proposing invalid, junk-filled or unexecutable blocks; withholding votes; grinding leader slots or the randomness beacon; colluding in juries.
2. **Any tx sender:** malformed or adversarial txs, fee and mempool games, replay across chains or after expiry, exploiting accounting edge cases, session-key limit bypasses.
3. **Network peers and sync sources:** malformed p2p messages, forged certificates or snapshots, eclipse attempts, an attacker serving `/api/sync` or snapshots to a joining node.
4. **Anyone on the internet:** HTTP requests that stall consensus through the API (SEC-4), resource exhaustion.
5. **A malicious RPC node** serving wallets and agents (WAL-4); a malicious web origin or script attacking the passkey wallet (WAL-3).
6. **A supply-chain attacker** controlling a dependency release, a CI runner or a maintainer account (OPS-7).

## Priorities

**P0: a bug here loses funds or splits the chain.**
- `pq.rs`: the certificate scheme. Signer bitmaps, quorum assembly, attributable vs batch verification, namespace binding, decoding bounds, equivocation evidence.
- `consensus.rs` block validity: `verify` must reject exactly the permanently invalid blocks (the `Application` contract) and never vote on a block whose execution it can't reproduce; the execution lag and checkpoint rules; views and leader accounting.
- Determinism: anything that could make honest nodes compute different state (iteration order, floats, time, platform-dependent behaviour, panics on some nodes only). The simulator and fuzzers exist; we want the auditors' own attempts.
- The state machine: every action's checks and accounting. Conservation of supply (`State::check_invariants`), stake and share accounting, slashing, unbonding, locks and vesting, fees and issuance, governance execution, juries and payouts.
- Signing bytes: `types.rs` (`Tx::signing_bytes`, the wire form, `check_format`) against `sdk-ts/src/tx.ts`. Any input two implementations encode differently, or one encoding with two meanings.
- Key handling: address and key-id derivation, proof of possession, session-key roles and limits, multisig.

**P1: a bug here halts nodes, weakens trust or costs money indirectly.**
- Sync trust (`snapshot.rs`, `epochs.rs`, `node.rs` join): what a joining node accepts from a peer, the embedded checkpoints, the weak-subjectivity window.
- Equivocation evidence and slashing: verification against the right epoch's set; no double penalty; no honest validator slashable after a disk restore (slashing protection).
- Upgrades and halts: version gating, `--halt-height`, behaviour at activation heights.
- The HTTP API's interaction with consensus (locking, rate limits, response sizes), p2p admission and peer-set limits, decoders for every network input.
- The randomness beacon and the jury draw (grinding, withholding).

**P2:** CLIs, the explorer (XSS; it renders on-chain strings), deployment scripts, the release pipeline.

Questions we'd like answered explicitly:
1. Can a < ⅓ coalition finalize two different blocks, or get honest validators slashed?
2. Can any tx, block or p2p message make honest nodes diverge, or panic all of them at the same height?
3. Is supply conserved under every action and every end-of-block and end-of-epoch step?
4. Can a joining node be fed a state or validator set that no honest quorum certified?
5. Do the Rust and TypeScript encoders agree on every input, and is every encoding injective?
6. Is our use of `ml-dsa` and `@noble/post-quantum` (key generation from seeds, deterministic signing, verification of malformed inputs) correct and constant-time where it needs to be?

## What we provide

- Repository access at the audit tag, and a walkthrough call per engagement.
- The design docs above, a threat-model session, and a channel to the maintainers with answers within one business day.
- Build and test: `cargo test --locked --manifest-path node/Cargo.toml` (unit tests and the deterministic simulator), `bash scripts/local-testnet.sh` (4 validators, a full node, the SDK end-to-end tests), fuzz targets under `node/fuzz/` and NIST known-answer tests for ML-DSA in both implementations (SEC-2, SEC-3).
- A private testnet on request.

## Deliverables

1. A report: each finding with severity (critical, high, medium, low, informational), affected code, impact, a reproduction or proof of concept where feasible, and a recommended fix. Plus coverage notes: what was reviewed, how deeply, and what wasn't.
2. Interim findings shared as they're found (critical and high within 24 hours, privately).
3. **Fix review:** every fix for a critical, high or medium finding re-verified on a new commit.
4. **Re-review of the diff** between the audited commit and the genesis release, so the code that launches is the code that was audited. Any consensus-format change after the audit goes through re-review again.
5. A final report we publish, after fixes are deployed.

## Programs (optional, off by default)

Programs (protocol v2) let anyone upload and run WebAssembly on a chain that has switched them on. The feature is built, optional and off by default, and it is **not in Engagements A and B above**: it has its own scope here, an external audit of it is required before a supported programs-on release (a launch kit chain may turn them on now, as an experiment), and none has been done. The specification the code is audited against is [PROGRAMS-V0.md](PROGRAMS-V0.md) (§10.6 lists the release gates; its appendix is the review log of the two adversarial reviews and the stage reviews so far). Operators' view: [OPERATIONS.md](OPERATIONS.md#programs-optional-off-by-default).

Where v2 differs from v1 only through programs, the v1 chain is untouched by construction ("v2 = v1 + dormant programs", PROGRAMS-V0.md §0 and §1.2), and the binary replays the recorded v1 history byte for byte (`docs/REPLAY.md`). An auditor of Engagement A reads the dormant hooks (§1.2 lists them) as part of the node; this scope is the code that runs once the feature is on.

### Scope

| Area | Where | What to look for |
|---|---|---|
| **The VM crate** | `node/vm` (its own workspace; `blockula-vm`) | The `HostPtr` `unsafe`; upload validation (`validate.rs`, the numbered rules, wasmparser pinned `=0.228.0`) and `upload::check`'s test translation; the executor, frames and call depth; the host ABI v1 (`abi.rs`, `host.rs`) and `host::sanitize`, which is consensus; metering (`meter.rs`, `SCHEDULE_V0`) and its calibration records; the overlay, the commit checks and `Diff::check` (conservation, who was debited); storage accounting and the price curve (`storage.rs`); the interface codec (`interface.rs`); soroban-wasmi's enabled paths at the pin (`=29.0.0`), its configuration, and the engine's memory use (translated code stays resident until an engine is dropped) |
| **The node's programs module** | `node/src/programs/` | State and its invariants (`state.rs`, `check_invariants`, `check_totals`), the digests and address derivation (`digest.rs`), `upload`, `create` (adoption), `call`, `exec` and `view`, `commit` (the order, the control `value_held + sponsored`, the cap and exit-only), `credit` (the four native-credit sites), `params`, `activate`, `receipt` (the claims budget), `describe` |
| **The pass thread** | `programs/pass.rs`, `programs/exec.rs` | One 64 MiB-stack thread and one engine per block pass: when the engine is dropped (a fault, 1 MiB of loaded code, a schedule change, the pass's end), what crosses to the thread (`State` is `Send` and `Sync`), the `catch_unwind` backstops, the fallback when the OS refuses the thread. That load is counted per transaction, so no outcome depends on what an engine already holds (the argument is in `node/vm/CALIBRATION-node.md`, "A3") |
| **Admission and the hooks** | `programs/admit.rs`, the hooks listed in PROGRAMS-V0.md §1.2 and the stages' one-line additions in `chain.rs`, `consensus.rs`, `node.rs`, `api.rs`, `governance.rs`, `staking.rs`, `ghost.rs`, `upgrade.rs` | `programs::status` (version, feature, pause, exit-only) gating every path; the signer rules (owner keys only, no session or multisig key, a program as `from`); the codec of tags 62 to 64 and 69 (`types.rs`: `check_format`, the refusal to nest, the Rust ⇄ TypeScript byte match with `sdk-ts/src/tx.ts`); `ApplyMode` and the mempool never running the VM |
| **Fees and fuel** | `programs/fuel.rs`, `chain.rs`/`econ.rs` fee hooks | The fuel fee (`rate × reserved / 1000`), the reservation (`exec + load`, bounded by `MAX_RESERVATION`), checked arithmetic at both rate ceilings, the controller (`next_rate`, `MAX_FUEL_RATE`), the per-block budget, `lowest_fuel_fee`, proposal order (`TipRate`), and whether the work a block does is bounded by what it was paid for |
| **Commit and receipts** | `programs/commit.rs`, `programs/receipt.rs`, `Block::programs` | Balance conservation across upload, create (record deposit, account creation), call and sponsored storage; the receipt as a display-only record that replay must still reproduce; the claims budget |
| **Pause, cap and exit-only** | `programs/pause.rs`, `programs/credit.rs`, `staking::duty_of` and `State::admit` for tag 69 | The threshold, vote life, window and cap (PROGRAMS-V0.md §7.2); which key forms are admitted (§2.5); the cap on what programs control; that exit-only still lets value leave |
| **Fault handling** | `programs/fault.rs` and its `quarantine`, `marker`, `totals`; `consensus.rs` (propose, verify, the replay cache); `chain.rs` (`apply_block`, `replay`); `upgrade.rs` (halts, exit codes 13 and 14) | A node fault is never an outcome (§6.2): the proposer's exclusion, verification's abstentions, the halts, the verified-clean retry, the quarantine, strikes and the trial marker as node-local safeguards that cannot be used to censor or to split consensus; the replay cache (`consensus.rs`) |
| **The API surface** | `programs/view_api.rs` (`POST /api/program/{address}/view`), `programs/api.rs` (`/api/program`, `/api/code`, `/api/interface`, `/api/params`, `/api/fees`), `programs/tx_json.rs` | `/view` runs a VM for whoever asks: its budgets (60 a minute per client, two at once, 250 ms of waiting, the fuel bound), that it can't write, and that a node fault is a 500 and never a halt. Off on validators unless `--program-views` |
| **Clients** | `sdk-ts/src/programs.ts`, `mcp/src` (the read tools and `untrusted.ts`), `node/src/program_cli.rs`, `web/` (the programs pages) | They overlap Engagement B. Programs add: digests mirrored in TypeScript, `provenProgram`, that every string a deployer or program chose (method names, event names, abort messages) is shown as untrusted data and cannot reach a tool description or a signing prompt, and that the CLI and SDK sign only what was shown (the whole version digest, the reservation checked against a block's budget) |

### What an audit of programs should answer

1. Can any program, upload or call make two honest nodes of one release compute different outcomes, fuel or state (iteration order, an engine's behaviour across architectures, memory)?
2. Can a program or a transaction create FANG, take FANG that is not its balance, or get around the cap or exit-only?
3. Can a transaction crash, hang or halt a validator, or make the node-local safeguards (quarantine, strikes, the trial marker, abstentions) censor an honest sender, or split the set?
4. Is the work a block does bounded by what it paid for: is `SCHEDULE_V0` an upper bound on measured time on the floor machines, translation included?
5. Does v2 change a v1 chain in any way (state, bytes, outcomes), before programs are switched on?
6. Do the Rust and TypeScript encoders and digests agree on every input?

### The fuzz record

Honestly: **the campaign has not been run.** What has been run is a set of smoke runs and the codec targets' 24-hour runs, all on one aarch64 Mac under AddressSanitizer. Nothing has run on x86-64 or without the sanitizer, and no run has been shared across machines. No run has found a crash in the node or the VM.

**The targets.** `node/vm/fuzz`: `program_validate` (validation, then the test translation of whatever it accepts, with `wasm-smith` modules in v0's subset), `program_exec` (batches of calls to the four guests, the host and overlay), `interface_codec`. `node/fuzz` (`TARGETS` in `src/harness.rs`): `program_tx_codec` (tags 62 to 64 and 69), `program_receipt`, `program_commit`, `state_machine_programs` (the state machine on a v2 fixture, with its own oracles), and v1's `state_machine`, unchanged. Each has committed seeds, and CI checks the seeds on every pull request and runs each target for five minutes nightly (`fuzz.yml`), with crashes sealed to the maintainers' keys. Their descriptions and results tables are `node/fuzz/README.md` and `node/vm/fuzz/README.md`.

**Local runs so far** (`node/fuzz/README.md`, "Results (2026-10-29)": one worker, `-timeout=10 -rss_limit_mb=2048`; the state machines with `-len_control=0 -max_len=4096`; the fixed oracles, after a review showed the first ones too weak):

| Target | Run | Executions | Edges (start → end) | Corpus | Result |
|---|---|---|---|---|---|
| `state_machine_programs` | 1,201 s | 14,103 (11/s) | 26,107 → 34,727 | 2,766 units, longest 4,078 B | no crash, no failed check |
| `program_receipt` | 1,201 s | 2,240,389 (1,865/s) | 9,693 → 12,552 | 3,593 units, longest 4,621 B | no crash, no failed check |
| `program_commit` | 1,201 s | 315,577 (262/s) | 18,670 → 19,479 | 413 units, longest 267 B | no crash, no failed check |
| `state_machine` (v1) | 601 s | 11,018 (18/s) | 15,801 → 20,704 | 2,124 units, longest 2,750 B | no crash, no failed check |

After the pass-thread change (one thread and one engine per block pass) the binaries were rebuilt and run again: `state_machine_programs` 600 s, 11,748 executions (19/s), edges 26,284 → 34,359, 2,364 units; `state_machine` 300 s, 7,160 executions, edges 15,830 → 20,515: no crash, no failed check. The slowest unit of every run took under a second. The oracles were checked by planting 18 bugs in the node's code: each is now caught on seed replay by an oracle, and `tests/oracles.rs` holds each oracle to the change a bug would make.

**The codec targets' 24-hour runs** (the codec is reachable even while programs are off, since every node decodes every gossiped transaction):

| Target | Built from | Result |
|---|---|---|
| `program_tx_codec`, run 1 | the first codec | Stopped after about 9 hours (53,276,452 runs) on a failed check in the harness's own assertion, a false positive on a private-transfer action (not a node bug); fixed in the harness |
| `program_tx_codec`, run 2 | the fixed harness | 160,102,729 runs in 86,401 s, no artifact |
| `program_tx_codec`, run 3 | the integration head once the pause and the credit guard had landed | 153,332,338 runs in 86,401 s (1,774/s), coverage 13,434 edges, no artifact |
| `program_tx_codec`, run 4 | the final codec (after the cheaper transaction check) | 163,552,034 runs in 86,401 s (1,892/s), coverage 14,601 edges, corpus 4,396 units, longest 6,861 B, no artifact |
| `interface_codec` | the final interface codec | A 24-hour run (2026-10-21 to 22) ended clean; its counts were not kept (its logs were in a temporary directory that was cleaned), so the number to cite is the 1-hour run on the same binary: 34,370,618 runs in 3,601 s (9,544/s), corpus 1,516 units, no artifact. Its code has not changed since |

Together that is about 3.4 CPU-days of `program_tx_codec` and about 1 of `interface_codec`, on aarch64 under AddressSanitizer. Neither counts toward the others' families. `program_validate` and `program_exec` have had only short runs: ten minutes each in the stage that wrote their harnesses (91,185 and 244,535 executions, no artifact), and the nightly five minutes since.

**The campaign, as planned and not yet run** (PROGRAMS-V0.md §10.6; `node/fuzz/README.md`, "The campaign"): at least **14 CPU-days per family in total**, summed over four configurations (AddressSanitizer and plain, x86-64 and aarch64) with none under 2 CPU-days, on one shared corpus per target, and the last 24 hours of wall-clock without a finding in any configuration. The three families are the VM's validation and translation (`program_validate`, `interface_codec`), the VM's execution (`program_exec`), and the node's targets with the v2 fixture. So **42 CPU-days at least** (the stricter reading, 14 in each configuration, would be 168, and is not what the gate says). The planned split of each family's 14: 5 on x86-64 with the sanitizer, 3 on aarch64 with it, 4 on x86-64 without, 2 on aarch64 without. The node family splits `state_machine_programs` 8, `program_commit` 3, `program_receipt` 1.5 and `program_tx_codec` 1.5. Corpora are merged across configurations about every 12 hours. At the measured speeds (the state machine runs 10 to 40 executions a second per worker) that is about three days of wall-clock on one 16-worker x86-64 machine and one aarch64 machine. Coverage, corpus size and the longest unit are to be recorded here when it runs. A finding restarts the last 24 hours after its fix; a finding is handled privately ([SECURITY.md](../SECURITY.md#fuzzing-findings)), and none is open.

### Evidence the auditor can re-run

`cargo test --release --locked` in `node` and in `node/vm` (also `--features ref038`, an older engine kept as a differential oracle); `bash scripts/test-program-cli.sh` and `blockula dev --programs` for the CLI end to end; `npm run test:vectors -w @blockula/sdk` for the byte-for-byte match with the TypeScript encoders; `blockula replay node/testdata/replay/v1-open` for the v1 corpus; the calibration records `node/vm/CALIBRATION-x86_64.md`, `CALIBRATION-aarch64.md` and `CALIBRATION-node.md` (every item within the 2.5 ns per fuel rule on both floor runners); and `node scripts/check-spec-citations.mjs`, which checks that the code's references to the specification all resolve (citations that name the file, bare `§` and amendment ids in the programs code, and the item and step numbers they give).

## Proposal

Please include: the team and their experience with BFT consensus, Rust and post-quantum or lattice cryptography (prior reports welcome); the approach and tooling (manual review, fuzzing, model checking, differential testing); an estimate in engineer-weeks per engagement and per priority level; available start dates; the fix-review and re-review terms; and pricing. Contact: open a private security advisory on the repository, or message the maintainer (@jackdishman on GitHub).
