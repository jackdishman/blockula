# Fuzzing the node (SEC-3)

Every decoder that takes bytes from a peer or an HTTP response is a way to crash or split the chain. This [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html) crate fuzzes them with the node's own code.

| Target | Attacker input | What the harness checks (`src/harness.rs`) |
|---|---|---|
| `cblock` | a block from marshal or broadcast (p2p) | `CBlock` (v2) decodes only canonical encodings (re-encoding gives the same bytes back), and every wire-form tx in it goes through `check_tx_stateless` |
| `tx_json` | `POST /api/tx` | strict JSON → `Tx`, `check_tx_stateless` (`check_format` with its wire round trip, address prefix, fee, ML-DSA signature), the explorer's `describe`, and a JSON round trip that must give back the same tx |
| `tx_wire` | block bodies, mempool gossip | `Tx::from_bytes` (Tx v3 wire form) accepts only the exact bytes `to_bytes` writes, `size()` is their length, the API's JSON for it parses back to the same tx, then the `tx_json` checks |
| `ghost_tx` | Ghost Mode txs in blocks and gossip (tags 50–52, ~135 KiB each) | both wire forms (signed shields, the unsigned `BLOCKULA_GHOST_V1` form) decode only canonically; the proof's statement derives from the tx alone; `check_tx_stateless`, Action proof included, never panics (Plonky3's verifier runs under `catch_unwind`). Seeds carry real proofs |
| `snapshot_manifest` | `GET /api/snapshot/latest` (fast sync) | JSON → `Manifest` (the `/api/epochs` history it's checked against rides in the same input as `epochs`) → `verify_manifest` (chunk bounds, hex blocks, hash links, checkpoint commitment, validator-set history, certificate, epoch anchor; nothing past the certified epoch kept) → `check_trust` against trusted checkpoints |
| `state_snapshot` | snapshot chunks, after zstd | `snapshot::read_state` (canonical records → state and tree) accepts only the canonical encoding (`write_state` gives the same bytes back); the rebuilt tree equals a from-scratch root and records prove against it |
| `sync_batch` | `GET /api/sync` | JSON → `SyncBatch` → `apply_batch` on a full node at genesis (certificate, links, execution); a batch that executes resets the node |
| `epochs_registry` | `GET /api/epochs` | a page as `exchange_epochs` takes signatures from it (`candidates`, verified, `add_verified`), as fast sync assembles pages (`append_page`) and verifies them (`verify_chain`, `verify_from` a pinned set, epoch 0 included), and the node's own paging (`slice` must reassemble), `rebase`, `truncate` and `proven_through` |
| `certificate` | finalization certificates; simplex votes and certificates (p2p) | `pq::Certificate` (bitmap plus signatures), `simplex::types::{Certificate, Vote}` for a 4-validator set, all canonical, all verified |
| `state_machine` | every tx a proposer can put in a block | Up to 300 ops, 5 bytes each (kind, two account indexes, an amount; a raw-action op takes up to 64 more bytes, decoded with `Action::from_bytes`), become txs of every tag from 6 users (one with a vesting lock), 3 agent session keys and the admin validator, in blocks across 4-block epochs and Séances of a few blocks. Each block is first dry-run on a journal and rolled back (the state must be unchanged), then applied as execution does (`State::apply`, `end_block`, `commit_block`). After every block: incremental root == full rebuild, `check_invariants` (supply conservation, escrows, cross-references, nested-collection caps) and every record within the snapshot limits. At the end the state must survive the snapshot encoding; one input in four runs twice and must give the same roots. Protocol v1, no programs: this target is the one it has always been |
| `state_machine_programs` | the same, on a chain born at protocol v2 with programs on and the four guests created | The state machine on the **v2 fixture** (see below): its first byte picks one of four profiles, the other bytes are ops 0–39 as `state_machine`'s and 40–46 program ops (call, batch, create, upload, pause, pay into a program, a governance step). Every block keeps `state_machine`'s checks and adds the oracles of `src/harness/oracle.rs`: each program transaction's charge, fuel and receipt against the spec's formulas and the state's own deltas, every account's balance, the guests' storage against models |
| `evidence` | `ReportEquivocation` evidence (any account may send it) | `staking::decode_evidence` accepts only canonical encodings of the three fault kinds (`ConflictingNotarize`, `ConflictingFinalize`, `NullifyFinalize`); it, `verify_evidence`, `evidence_status` and the explorer's description never panic. Evidence that verifies against a fixture state (the 4-validator set, bonded, active in epochs 0–3) is reported in a tx: it applies, the offender is tombstoned, no FANG is minted, `check_invariants` holds, and the same fault reported again fails. Seeds are real double-signs of each kind |
| `program_tx_codec` | programs' txs (tags 62–64 and 69, docs/PROGRAMS-V0.md §2) in blocks, gossip and `POST /api/tx`, which every node decodes and checks, programs on or off | An input is an action's wire encoding or, from a `{`, its JSON. The wire decoder accepts only canonical encodings, and whatever decodes survives its JSON form; inside a multisig proposal a program action never decodes; in a tx's place the gossip decoder (`Tx::from_bytes`) takes every action the action decoder takes but a private transfer or an unshield (which travel unsigned only). `check_tx_stateless` (an upload's interface decoded) and the explorer's summary never panic, and a tx that passes `check_format` survives its wire form. On two chains where programs are dormant (v1, and born at v2 with programs off, as `blockula-1` is), admission refuses every program action and every `program_*` parameter change, in programs' own words whenever the rest of the tx is well formed, and charges nobody. Seeds: the SDK's program vectors (`sdk-ts/test/tx-vectors.json`, every tag and the cases at the bounds), the four guests' uploads with their interfaces, and a `program_*` parameter change, bare and in a multisig proposal, each in both forms |
| `program_receipt` | a program transaction's receipt as JSON (`Block.programs` in the block log, `/api/block` and `/api/tx`'s `program` section: docs/PROGRAMS-V0.md §8.1), a block's map of them, or a whole block | each receipt reads back from the JSON it wrote (bytes and value, one encoding), its outcome carries its number and code, `tx_json::section` never panics and shows each fact and claim as the receipt has it, `charged` is the fee's sum, the claims budget (`apply_claims_budget`) cuts a block's receipts as an independent model of the rule says (and again changes nothing), and `for_block` hands a block its receipts by transaction index (a failed one that says it applied becomes `native` with nothing of its action left, a stranger's is dropped). Seeds: a receipt of every outcome, the receipts the v2 fixture's own seeds leave, a block's map and a block |
| `program_commit` | what a program transaction's VM work hands the node to write: a `Diff` (docs/PROGRAMS-V0.md §6.1 steps 6.1–6.4) | an input is a plan (the fixture's state in one of three profiles, a signer, storage writes and FANG moves) whose diff is derived as the VM derives one (totals from the writes against the state's entries, deposits as moves to and from escrow, net balances, accounts opened and what they burn), so `Diff::check` holds by construction. Through `programs::commit::apply_diff` and the control's check: refused, or every invariant holds (`check_invariants`, `check_totals`); and the journal undoes it, refused or not, down to the state root. Seeds: the shapes the guests' calls make |

## Running

```sh
rustup toolchain install nightly-2026-10-06 && cargo install cargo-fuzz --locked
cd node
cargo +nightly-2026-10-06 fuzz run cblock fuzz/corpus/cblock fuzz/seeds/cblock -- -max_total_time=300
```

Use the nightly CI pins (`FUZZ_NIGHTLY` in `.github/workflows/fuzz.yml`). Since Ghost Mode's prover joined the dependencies, nightly-2026-10-13 panics compiling `p3-monty-31` (a rustc ICE in `compare_impl_item`), so the pin moved back to nightly-2026-10-06.

Swap `cblock` for any target. The first directory is the working corpus (git-ignored); `fuzz/seeds/<target>` holds valid inputs built by `harness::seeds()`, so the fuzzer starts from blocks, txs, certificates and manifests that pass every check. Crashes land in `fuzz/artifacts/<target>/`. To reproduce one, run `cargo +nightly fuzz run <target> <file>`.

`state_machine` is stateful and slow per input (tens of executions a second per core); run it with `-fork=N` for throughput.

### The v2 fixture (`state_machine_programs`, docs/PROGRAMS-V0.md §10.4)

Every input of `state_machine_programs` runs on a chain born at protocol v2 with the `programs` feature on and the four guests of `node/vm/tests/programs` (counter, token, escrow, router) uploaded in one block and created, with their interfaces, in the next (a blob is usable from the block after its upload, a program from the block after its creation, §2.1). The first byte, modulo 4, picks the state the ops start from (a mutated first byte is merely another profile):

| Byte | Profile | |
|---|---|---|
| 0 | `G` | programs' parameters at their defaults |
| 1 | `T` | the value cap at 500 FANG and the block's fuel at its floor (20 million): a genesis can't set programs' parameters, so a tight cap is META's |
| 2 | `X` | programs switched off after the guests exist (exit-only) |
| 3 | `P` | blocks of 2.4 hours (a day is 10 blocks, so a pause's day-long vote and its 30-day window and cap are 10 and 300 blocks) and four genesis validators (a pause needs `f + 1` = 2 signatures) |

Seven ops follow v1's (kinds 40–46, taken modulo 47 here): **call** a guest's method with input, value, fuel and a storage limit the fuzzer chooses; a **batch** of 1–8 calls (all or nothing); a **create** (the blobs, an authority, `init` present, absent or invented, a policy, whether it takes transfers); an **upload** of a guest's code and interface with the fuzzer's edits; a validator's **pause** signature (a genesis validator, a key a user registered a validator with, or a user for its own); a **transfer** into a program, or into the address a create will make (which the create adopts); and a **governance** step, what the executor does when a proposal passes: switch programs off (exit-only) or on again, set a program parameter, or let up to 63 blocks pass. Each program op takes its five bytes and then a length byte and that many payload bytes, read in order (`harness/v2.rs`'s docs have the layout), and ends with the fee it signs: far above the charge, or exactly the charge, or a drop less, or an idle chain's. An input stops signing for fuel once it has asked for 40 million, and has at most 800 blocks (empty ones past the eighth are made without the per-block checks): a legal input that runs long is not a finding.

Each block keeps the v1 checks (the dry run's rollback leaves no trace, the incremental root equals a full rebuild, `check_invariants` and the record sizes hold, the roots are deterministic, the snapshot round trip), and adds:

- **no program transaction faults the node** (§6.2: a fault is never a valid outcome). A *Setup*-class fault (the OS wouldn't start a program thread) is the machine's, not the code's: the block is run once more, and if it comes again the input ends unjudged;
- **the oracles** (`harness/oracle.rs`), which don't call the code they check. The expected charge of every program transaction is written from §5.1 over raw state (the fee ring's slot, META's fuel rate and floor, the stored code's size) and must be its receipt's `fee` and `fuel`, its signer's balance change, and what admission took (a refusal "too cheap" must have been). Every account's balance moves by exactly its entry in the receipt's `balance_changes` (the proposer's also by the tip). The receipt's facts must be the state's deltas: `created` is the one program that now exists, `upload` the blobs now stored and their deposits, `storage` META's `state_bytes` and `escrow`. A `value_cap` or `exit_only` refusal is re-run where the rule isn't and must have been right. The block's counted fuel is the sum of what its admitted transactions reserved, and the rate it leaves is §5.6's. The counter's count, the token's balances against its supply and the escrow's open deals are checked against models of the guests after every block;
- **the second run is `State::run_block`**: a quarter of the inputs run twice, the second time through `State::run_block` itself instead of the unrolled copy the oracles need, and the roots and the digests of each block's receipts must be equal.

For the campaign, seed `state_machine_programs` with its own seeds (`seeds/state_machine_programs`, one of them 1.4 KB) and run it, like `state_machine`, with `-len_control=0 -max_len=4096`: libFuzzer's length control otherwise holds a run to the shortest seed's length (a first run of S8f stayed under 229 bytes for 75,599 executions). `state_machine`'s runs are all v1: its corpus is never shared with this target's.

### The campaign (docs/PROGRAMS-V0.md §10.6)

The release gate asks for **at least 14 CPU-days per family in total**, summed over the four configurations (ASan and plain, x86-64 and aarch64), **none of them under 2 CPU-days**, on **one shared corpus per target**, and the last 24 hours of wall-clock without a finding in any configuration. The families are the VM's validation and translation (`node/vm/fuzz`'s `program_validate`, `interface_codec`), the VM's execution (`program_exec`), and the node's targets with the v2 fixture (`program_tx_codec`, `program_receipt`, `program_commit`, `state_machine_programs`; `state_machine` is v1's and rides along). So three families, 42 CPU-days at least, and at 14 CPU-days each of the four configurations would be 168: that is the stricter reading, and not what §10.6 says.

Per target and configuration, with the pinned toolchain and cargo-fuzz (`fuzz.yml`'s):

```sh
cd node
cargo +nightly-2026-10-06 fuzz build <target>                      # ASan (the default)
cargo +nightly-2026-10-06 fuzz build --sanitizer none <target>     # plain
# one machine, N workers on one corpus; keep each binary's sha256 with the run's record
<binary> corpus/<target> seeds/<target> -jobs=N -workers=N -timeout=10 -rss_limit_mb=2048 \
    -artifact_prefix=artifacts/<target>/ -print_final_stats=1 -max_total_time=<seconds> \
    -len_control=0 -max_len=4096          # the state machines only (see below)
```

- Every target's flags are the campaign's: `-timeout=10` (a run past 10 seconds is a hang, and a hang is a finding) and `-rss_limit_mb=2048`. The state machines also take `-len_control=0 -max_len=4096`: with libFuzzer's default length control an input never grows past the shortest seed's length unless a run goes hundreds of executions without new coverage, and a v2 target finds coverage constantly. Record the corpus's longest unit with the run (`ls -S corpus/<target> | head -1`, then its size).
- **Share the corpus.** Merge the machines' corpora every 12 hours or so, into one directory per target (`<binary> -merge=1 merged/ a/ b/ c/ d/`), hand the merged set back to every worker, and keep the number of units and the edges (`cov:` in libFuzzer's lines, or `cargo fuzz coverage` for the line coverage of `node/src/programs` and `node/vm`) for `docs/AUDIT_SCOPE.md`.
- **CPU-days** are the sum over workers of their wall-clock time: a 16-worker run of 6 hours is 4 CPU-days. The state machine is the slow family (a run of its quick programs ran 10-40 executions a second per worker under ASan), so give it the most cores; `program_receipt` runs a thousand times faster.
- **The last 24 hours** of the campaign run on the frozen release candidate in every configuration at once, and none of them may find anything. A finding restarts it, after the fix.
- A crash is a vulnerability: keep it out of anything public (artifacts under the campaign machine's private directory; `private/` here; `SECURITY.md`).

The nightly job is a smoke test of the same targets (5 minutes each, the cached corpus bounded to `CORPUS_MAX_UNITS` units so that replaying it can't take the time), not part of the campaign's total.

On stable, with no fuzzer, `cargo test --manifest-path node/fuzz/Cargo.toml` checks that every seed is still accepted (a format change otherwise leaves the fuzzers mutating dead input), that the harnesses pass over the inputs `tests/harness.rs` holds them to (a harness that panics on input the node handles correctly reports a finding that isn't one) and, where they're present, that open findings still reproduce (see [Findings](#findings): their inputs are never in this tree). After a format change, regenerate the seeds with `cargo test --manifest-path node/fuzz/Cargo.toml -- --ignored write_seeds`.

CI (`.github/workflows/fuzz.yml`) runs the stable checks on every PR (a required check), and fuzzes each target for 5 minutes nightly (or on demand, with a chosen duration), keeping the corpus between runs. The repository is public, and so are its workflow logs and artifacts, so a crash stays private (audit supply-chain-B-1):

- The fuzzer's output (the panic, its file and line, the input in base64) goes to a file, never to the log. A crash fails that target's job with only `Fuzz <target>: crash found`.
- The crash files and that log leave the runner only encrypted with [age](https://age-encryption.org) to the maintainers' keys, the repository variable `FUZZ_CRASH_AGE_RECIPIENTS` (`scripts/fuzz-seal-crash.sh`; [deploy/github/README.md](../../deploy/github/README.md) sets it). The artifact `fuzz-crash-<target>` holds only `fuzz-crash-<target>.tar.age`. With the variable unset, nothing is kept: the run is red, and a maintainer fuzzes that target privately to find the crash again.
- The corpus is cached only after a run without a crash, so it never holds a crashing input. It isn't secret either (fork PRs can restore the default branch's caches), so treat it as public.
- `scripts/check-fuzz-workflow.sh` (CI, `ops` job) fails if a fuzz run's output reaches the log, if anything but `*.age` is uploaded, or if open findings' inputs enter the tree; `scripts/test-fuzz-seal.sh` tests the sealing (a real age round trip in CI).

### How the crate builds

The node is a binary crate with no library target, so `src/lib.rs` compiles its modules straight from `node/src` through a `#[path]` module and re-exports them, so the node's `crate::…` paths still resolve. The node crate itself doesn't change. In exchange, this crate repeats the node's module list and `[dependencies]`; `check-sync.sh` (run in CI) fails when they drift. If the node gains a `lib.rs`, this crate can depend on it and drop both copies.

## Results (2026-10-29, local, Apple M-series, nightly-2026-10-06): the fixed harness

AddressSanitizer builds (`cargo fuzz build`'s defaults), one worker, `nice -n 10`, `-timeout=10 -rss_limit_mb=2048`; the state machines with `-len_control=0 -max_len=4096`. The seeds are the committed ones (`state_machine_programs`: 15, one of 1.4 KB); a corpus starts empty. These replace the first build's table (2026-10-28), whose oracles were too weak to carry the campaign (S8f's review: of 18 planted bugs in node code, 10 passed every oracle and a real libFuzzer run; each is now caught on seed replay, and `tests/oracles.rs` holds each oracle to the change a bug would make).

| Target | Run | Executions | Edges (start → end) | Corpus | Result |
|---|---|---|---|---|---|
| `state_machine_programs` | 1,201 s | 14,103 (11/s) | 26,107 → 34,727 | 2,766 units (2293Kb), longest 4,078 B, peak RSS 1,013 MB | no crash, no failed check |
| `program_receipt` | 1,201 s | 2,240,389 (1,865/s) | 9,693 → 12,552 | 3,593 units (1138Kb), longest 4,621 B, peak RSS 1,083 MB | no crash, no failed check |
| `program_commit` | 1,201 s | 315,577 (262/s) | 18,670 → 19,479 | 413 units (22Kb), longest 267 B, peak RSS 593 MB | no crash, no failed check |
| `state_machine` (v1) | 601 s | 11,018 (18/s) | 15,801 → 20,704 | 2,124 units (956Kb), longest 2,750 B, peak RSS 812 MB | no crash, no failed check |

Confirmation runs on the tree with stage A3 merged (one program thread and one engine per block pass; the binaries rebuilt from it): `state_machine_programs` 600 s, 11,748 executions (19/s), edges 26,284 → 34,359, 2,364 units, longest 3,232 B; `state_machine` (v1) 300 s, 7,160 executions (23/s), edges 15,830 → 20,515: no crash, no failed check.

The slowest unit of each run took under a second. The state machine on the v2 fixture runs about a dozen executions a second per worker: each program transaction makes a node thread, an engine and a translation of every declared code, in a build with AddressSanitizer and coverage in the engine's loops, and the inputs are now hundreds of bytes long (the longest unit grew to 4,078 of the 4,096 allowed; the first build's never passed 229). The v1 `state_machine` ran from its four seeds with the same length flags, 18 executions a second.

## Results (2026-10-21, local, Apple M-series, nightly-2026-10-06): programs' tx codec

| Target | Executions | Result |
|---|---|---|
| `program_tx_codec` | 699,915 (600 s, 1 worker, `-timeout=10 -rss_limit_mb=2048`), its first run, from the committed seeds; 9,834 edges, corpus 2,021, peak RSS 569 MB | no crash, no failed check |

## Results (2026-10-15, local, Apple M-series, nightly-2026-10-06): equivocation evidence

| Target | Executions | Result |
|---|---|---|
| `evidence` | 186,811 (120 s, 1 worker), its first run, from the committed seeds; corpus 182 | no crash, no invariant failure |

Certificate-derived conflicts (a validator's direct vote against its signature inside another block's certificate) aren't evidence the chain accepts yet: `ReportEquivocation` carries simplex's own `ConflictingNotarize`/`ConflictingFinalize`/`NullifyFinalize`, which pair two votes. Extracting a vote from a certificate needs simplex's per-signer certificate layout (`pq::Certificate`'s signer bitmap and signatures) to be decoded and re-verified per signer on chain, a new evidence format with its own golden vector and fuzzing. It isn't built (CV-3's pending item).

## Results (2026-10-15, local, Apple M-series, nightly-2026-10-06): the integrated node

After all nine wave 3 packages were merged (schema 4: staking, governance, econ, beacon, upgrade and Ghost Mode state), from the committed seeds:

| Target | Executions | Result |
|---|---|---|
| `state_machine` | 6,417 (300 s, 1 worker); 17,167 edges, corpus 1,397 | no crash, no invariant or determinism failure |
| `tx_wire` | 106,704 (60 s) | no crash |
| `state_snapshot` | 107,997 (60 s) | no crash |
| `ghost_tx` | 12,167 (60 s), its first run | no crash |

## Results (2026-10-15, local, Apple M-series): the state machine and the tx genesis hash

| Target | Executions | Result |
|---|---|---|
| `state_machine` | 2,920 (7 min, 1 worker) + 116,778 (20 min, `-fork=6`), from the committed seeds; 13,783 edges, corpus 2,838 | no crash, no invariant or determinism failure |
| `tx_wire` | 94,116 (60 s), after Txs gained `genesis_hash` | no crash |
| `tx_json` | 143,749 (60 s) | no crash |
| `cblock` | 27,614 (60 s) | no crash |

## Results (2026-10-14, local, Apple M-series, nightly-2026-10-13)

Protocol v1 formats (Tx v3, CBlock v2, state records), after the FUZZ-1/FUZZ-2 fixes, from the committed seeds:

| Target | Executions | Result |
|---|---|---|
| `tx_wire` | 167,682 (90 s) | no crash |
| `state_snapshot` | 246,933 (90 s) | no crash |
| `epochs_registry` | 166,404 (60 s) | no crash (FUZZ-2 fixed) |
| `snapshot_manifest` | 177,643 (60 s) | no crash (FUZZ-1 fixed) |

Before protocol v1 (JSON txs in blocks), two rounds per target, 90 s and then 180 s, both from the committed seeds and the corpus built so far:

| Target | Executions (90 s + 180 s) | Result |
|---|---|---|
| `cblock` | 197,777 + 396,536 | no crash |
| `tx_json` | 98,340 + 376,658 | no crash |
| `snapshot_manifest` | 114,412 + 400,812 | no crash. FUZZ-1 (below) showed up before fuzzing started: this target's first seed, a checkpoint in epoch 0, crashed the harness. The committed seed uses epoch 1 so the fuzzer can get past it |
| `sync_batch` | 101,100 + 378,575 | no crash |
| `epochs_registry` | 71,642 + 301,970 | **FUZZ-2**, found on its own after 59 s. The second round ran in fork mode (`-fork=1 -ignore_crashes=1`) to fuzz past it; all 4 crash inputs saved (counting one more 100 s fork-mode run) were FUZZ-2, and nothing else crashed |
| `certificate` | 209,337 + 455,263 | no crash |

These are smoke runs, not the two-week soak SEC-3 asks for.

## Findings

A fuzzer crash is an undisclosed vulnerability, often a remote crash of every validator at the same height, so it's handled as a [private security advisory](../../SECURITY.md#fuzzing-findings) until the fix has shipped. Nothing about an open one goes into this public tree, an issue, a PR or a public log.

1. **Get the crash.** A red `Fuzz <target>` job means a crash. Download its sealed artifact and decrypt it with your age identity: `gh run download <run id> --repo jackdishman/blockula -n fuzz-crash-<target>`, then `age -d -i ~/.config/blockula/fuzz-age.key fuzz-crash-<target>.tar.age | tar -x -C <private dir>` (the crash files under `artifacts/`, the fuzzer's log in `fuzz.log`). Without `FUZZ_CRASH_AGE_RECIPIENTS` there's no artifact: fuzz the target locally from the same seeds until it crashes again.
2. **Open a private advisory** (GitHub's "Report a vulnerability", or the maintainers' channel), and fix it in the advisory's temporary private fork ([OPERATIONS.md "Patch distribution"](../../docs/OPERATIONS.md#patch-distribution)).
3. **Keep the reproducer private.** Put the input in `node/fuzz/private/<target>/` (git-ignored) or in the private fork, pointed to by `BLOCKULA_FUZZ_FINDINGS`. `tests/seeds.rs` (`open_findings_still_reproduce`) checks every input there still crashes; in public CI the directory doesn't exist and the test checks nothing. Fixing one makes that test fail on purpose.
4. **After the fix is public**, the input becomes a regression seed: add it to `harness::seeds()` (as `fuzz-<n>-…`), run `write_seeds`, and write it up below.

None are open.

**FUZZ-1 (fixed): any fast-sync manifest from epoch 0 crashed the joining node** (now the seed `seeds/snapshot_manifest/fuzz-1-manifest-in-epoch-0`).
`snapshot::verify_manifest` calls `epochs::verify_chain(g, &m.epochs, epoch)` with the epoch of the manifest's last block. For epoch 0, `verify_chain` ran `claimed.sets.range(1..=0)`, and `BTreeMap::range` panicked ("range start is greater than range end"). `fast_sync` runs inside `block_on` during `join` and validator start-up, so the process died, and did so again on every restart that asked the same peer first. Two ways in:
- an honest chain younger than one epoch (600 blocks by default) that already has a certified snapshot;
- any malicious peer, at any time. Everything `verify_manifest` checks before that call (chain id, checkpoint height, block encoding, hash links, the certifying block's state root) is peer-chosen, and none of it needs a signature.

Fixed: `verify_chain` walks `range(1..)` up to `epoch`, so epoch 0 gets the genesis set (regression test `epochs::tests::verify_chain_at_epoch_0_is_the_genesis_set`).

**FUZZ-2 (fixed): a peer's handoff proof for epoch 0 killed `exchange_epochs`** (now the seed `seeds/epochs_registry/fuzz-2-handoff-proof-for-epoch-0`).
`EpochRegistry::merge_sigs` accepted proofs for every epoch whose set matches ours, and that included epoch 0, whose set is public. `add_sig` then computed `epoch - 1`, which overflows. Overflow checks are on in release builds, so this panicked inside `chain.lock().merge_epochs(..)` in the spawned `exchange_epochs` task, and the task ended. From then on the node stopped collecting and serving handoff proofs, and joiners could no longer verify set changes against it. The fuzzer found this one by itself, from the `handoff-epoch-1` seed.

Fixed: `add_sig` refuses epoch 0 (`checked_sub`) and `merge_sigs` skips it (regression test `epochs::tests::handoff_proofs_for_epoch_0_are_ignored`).

**FUZZ-3 (fixed): a state with an unknown address prefix panicked `check_invariants`** (the nightly `state_snapshot` run of 2026-10-18).
`snapshot::read_state` accepted a canonical state whose parameters carried an address prefix no network uses (`b#o`), and `State::check_invariants`, which the harness runs on what it decodes, then panicked rendering a system address with it (`crypto::render_address` asserts a known prefix). No running node reached it: every restore compares the rebuilt root with the certified root right after decoding, the prefix is a genesis constant the root commits to, and `check_invariants` runs only in tests and here. It isn't a seed, since the harness now refuses the input.

Fixed: `State::load_records` refuses an unknown prefix and `check_invariants` reports one instead of panicking (regression test `state::tests::a_state_with_an_unknown_address_prefix_is_refused_not_a_panic`).
