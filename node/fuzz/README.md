# Fuzzing the node (SEC-3)

Every decoder that takes bytes from a peer or an HTTP response is a way to crash or split the chain. This [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html) crate fuzzes them with the node's own code.

| Target | Attacker input | What the harness checks (`src/harness.rs`) |
|---|---|---|
| `cblock` | a block from marshal or broadcast (p2p) | `CBlock` (v2) decodes only canonical encodings (re-encoding gives the same bytes back), and every wire-form tx in it goes through `check_tx_stateless` |
| `tx_json` | `POST /api/tx` | strict JSON → `Tx`, `check_tx_stateless` (`check_format` with its wire round trip, address prefix, fee, ML-DSA signature), the explorer's `describe`, and a JSON round trip that must give back the same tx |
| `tx_wire` | block bodies, mempool gossip | `Tx::from_bytes` (Tx v3 wire form) accepts only the exact bytes `to_bytes` writes, `size()` is their length, the API's JSON for it parses back to the same tx, then the `tx_json` checks |
| `snapshot_manifest` | `GET /api/snapshot/latest` (fast sync) | JSON → `Manifest` → `verify_manifest` (chunk bounds, hex blocks, hash links, checkpoint commitment, validator-set history, certificate) |
| `state_snapshot` | snapshot chunks, after zstd | `snapshot::read_state` (canonical records → state and tree) accepts only the canonical encoding (`write_state` gives the same bytes back); the rebuilt tree equals a from-scratch root and records prove against it |
| `sync_batch` | `GET /api/sync` | JSON → `SyncBatch` → `apply_batch` on a full node at genesis (certificate, links, execution); a batch that executes resets the node |
| `epochs_registry` | `GET /api/epochs` | `merge_sigs` as `exchange_epochs` runs it, plus `verify_chain` (epoch 0 included) and `proven_through` |
| `certificate` | finalization certificates; simplex votes and certificates (p2p) | `pq::Certificate` (bitmap plus signatures), `simplex::types::{Certificate, Vote}` for a 4-validator set, all canonical, all verified |
| `state_machine` | every tx a proposer can put in a block | Up to 300 ops, 5 bytes each (kind, two account indexes, an amount; a raw-action op takes up to 64 more bytes, decoded with `Action::from_bytes`), become txs of every tag from 6 users (one with a vesting lock), 3 agent session keys and the admin validator, in blocks across 4-block epochs and Séances of a few blocks. Each block is first dry-run on a journal and rolled back (the state must be unchanged), then applied as execution does (`State::apply`, `end_block`, `commit_block`). After every block: incremental root == full rebuild, `check_invariants` (supply conservation, escrows, cross-references, nested-collection caps) and every record within the snapshot limits. At the end the state must survive the snapshot encoding; one input in four runs twice and must give the same roots |

## Running

```sh
rustup toolchain install nightly && cargo install cargo-fuzz --locked
cd node
cargo +nightly fuzz run cblock fuzz/corpus/cblock fuzz/seeds/cblock -- -max_total_time=300
```

Swap `cblock` for any target. The first directory is the working corpus (git-ignored); `fuzz/seeds/<target>` holds valid inputs built by `harness::seeds()`, so the fuzzer starts from blocks, txs, certificates and manifests that pass every check. Crashes land in `fuzz/artifacts/<target>/`. To reproduce one, run `cargo +nightly fuzz run <target> <file>`.

`state_machine` is stateful and slow per input (tens of executions a second per core); run it with `-fork=N` for throughput.

On stable, with no fuzzer, `cargo test --manifest-path node/fuzz/Cargo.toml` checks that every seed is still accepted (a format change otherwise leaves the fuzzers mutating dead input) and that open findings (`harness::open_findings()`, inputs in `regressions/`; none today) still reproduce. After a format change, regenerate the seeds with `cargo test --manifest-path node/fuzz/Cargo.toml -- --ignored write_seeds`.

CI (`.github/workflows/fuzz.yml`) runs the stable checks on PRs that touch `node/`, and fuzzes each target for 5 minutes nightly (or on demand, with a chosen duration), keeping the corpus between runs. A crash fails that target's job and uploads the input.

### How the crate builds

The node is a binary crate with no library target, so `src/lib.rs` compiles its modules straight from `node/src` through a `#[path]` module and re-exports them, so the node's `crate::…` paths still resolve. The node crate itself doesn't change. In exchange, this crate repeats the node's module list and `[dependencies]`; `check-sync.sh` (run in CI) fails when they drift. If the node gains a `lib.rs`, this crate can depend on it and drop both copies.

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

Open findings live in `harness::open_findings()` with their inputs in `regressions/`, and `tests/seeds.rs` (`open_findings_still_reproduce`) checks they still crash. Fixing one makes that test fail on purpose: move the input into the seeds and delete its entry. None are open.

**FUZZ-1 (fixed): any fast-sync manifest from epoch 0 crashed the joining node** (now the seed `seeds/snapshot_manifest/fuzz-1-manifest-in-epoch-0`).
`snapshot::verify_manifest` calls `epochs::verify_chain(g, &m.epochs, epoch)` with the epoch of the manifest's last block. For epoch 0, `verify_chain` ran `claimed.sets.range(1..=0)`, and `BTreeMap::range` panicked ("range start is greater than range end"). `fast_sync` runs inside `block_on` during `join` and validator start-up, so the process died, and did so again on every restart that asked the same peer first. Two ways in:
- an honest chain younger than one epoch (600 blocks by default) that already has a certified snapshot;
- any malicious peer, at any time. Everything `verify_manifest` checks before that call (chain id, checkpoint height, block encoding, hash links, the certifying block's state root) is peer-chosen, and none of it needs a signature.

Fixed: `verify_chain` walks `range(1..)` up to `epoch`, so epoch 0 gets the genesis set (regression test `epochs::tests::verify_chain_at_epoch_0_is_the_genesis_set`).

**FUZZ-2 (fixed): a peer's handoff proof for epoch 0 killed `exchange_epochs`** (now the seed `seeds/epochs_registry/fuzz-2-handoff-proof-for-epoch-0`).
`EpochRegistry::merge_sigs` accepted proofs for every epoch whose set matches ours, and that included epoch 0, whose set is public. `add_sig` then computed `epoch - 1`, which overflows. Overflow checks are on in release builds, so this panicked inside `chain.lock().merge_epochs(..)` in the spawned `exchange_epochs` task, and the task ended. From then on the node stopped collecting and serving handoff proofs, and joiners could no longer verify set changes against it. The fuzzer found this one by itself, from the `handoff-epoch-1` seed.

Fixed: `add_sig` refuses epoch 0 (`checked_sub`) and `merge_sigs` skips it (regression test `epochs::tests::handoff_proofs_for_epoch_0_are_ignored`).
