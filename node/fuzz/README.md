# Fuzzing the node (SEC-3)

Every decoder that takes bytes from a peer or an HTTP response is a way to crash or split the chain. This [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html) crate fuzzes them with the node's own code.

| Target | Attacker input | What the harness checks (`src/harness.rs`) |
|---|---|---|
| `cblock` | a block from marshal or broadcast (p2p) | `CBlock` decodes only canonical encodings (re-encoding gives the same bytes back), and every tx in it goes through `check_tx_stateless` |
| `tx_json` | `POST /api/tx`, mempool gossip, block bodies | JSON → `Tx`, `check_tx_stateless` (`check_format`, minimum fee, ML-DSA signature), the explorer's `describe`, and a JSON round trip that must keep the signing bytes and tx id |
| `snapshot_manifest` | `GET /api/snapshot/latest` (fast sync) | JSON → `Manifest` → `verify_manifest` (hex blocks, hash links, checkpoint commitment, validator-set history, certificate) |
| `sync_batch` | `GET /api/sync` | JSON → `SyncBatch` → `apply_batch` on a full node at genesis (certificate, links, execution); a batch that executes resets the node |
| `epochs_registry` | `GET /api/epochs` | `merge_sigs` as `exchange_epochs` runs it, plus `verify_chain` and `proven_through` |
| `certificate` | finalization certificates; simplex votes and certificates (p2p) | `pq::Certificate` (bitmap plus signatures), `simplex::types::{Certificate, Vote}` for a 4-validator set, all canonical, all verified |

Not covered yet: a "random txs → apply → conservation" target for the state machine (wait for the state rewrite, CV-1), and the Tx v3 wire form (`Tx::from_bytes`, P-WIRE), which should get its own target when it lands.

## Running

```sh
rustup toolchain install nightly && cargo install cargo-fuzz --locked
cd node
cargo +nightly fuzz run cblock fuzz/corpus/cblock fuzz/seeds/cblock -- -max_total_time=300
```

Swap `cblock` for any target. The first directory is the working corpus (git-ignored); `fuzz/seeds/<target>` holds valid inputs built by `harness::seeds()`, so the fuzzer starts from blocks, txs, certificates and manifests that pass every check. Crashes land in `fuzz/artifacts/<target>/`. To reproduce one, run `cargo +nightly fuzz run <target> <file>`.

On stable, with no fuzzer, `cargo test --manifest-path node/fuzz/Cargo.toml` checks that every seed is still accepted (a format change otherwise leaves the fuzzers mutating dead input) and that the open findings below still reproduce. After a format change, regenerate the seeds with `cargo test --manifest-path node/fuzz/Cargo.toml -- --ignored write_seeds`.

CI (`.github/workflows/fuzz.yml`) runs the stable checks on PRs that touch `node/`, and fuzzes each target for 5 minutes nightly (or on demand, with a chosen duration), keeping the corpus between runs. A crash fails that target's job and uploads the input.

### How the crate builds

The node is a binary crate with no library target, so `src/lib.rs` compiles its modules straight from `node/src` through a `#[path]` module and re-exports them, so the node's `crate::…` paths still resolve. Nothing in `node/` changes. In exchange, this crate repeats the node's module list and `[dependencies]`; `check-sync.sh` (run in CI) fails when they drift. If the node gains a `lib.rs`, this crate can depend on it and drop both copies.

## Results (2026-10-14, local, Apple M-series, nightly-2026-10-13, 90 s per target)

| Target | Executions | Result |
|---|---|---|
| `cblock` | 197,777 (2,173/s) | no crash |
| `tx_json` | 98,340 (1,080/s) | no crash |
| `snapshot_manifest` | 114,412 (1,257/s) | no crash. FUZZ-1 (below) surfaced earlier, when the first seed for this target, a checkpoint in epoch 0, crashed the harness; the committed seed uses epoch 1 so the fuzzer can get past it |
| `sync_batch` | 101,100 (1,110/s) | no crash |
| `epochs_registry` | 71,642 (1,214/s) | **FUZZ-2 after 59 s**, then 100 s more in fork mode (`-fork=1 -ignore_crashes=1`) with no other crash |
| `certificate` | 209,337 (2,300/s) | no crash |

These are smoke runs, not the two-week soak SEC-3 asks for.

## Open findings

Both are reproduced by `tests/seeds.rs` (`open_findings_still_reproduce`) from the inputs in `regressions/`. Fixing one makes that test fail on purpose: move the input into the seeds, and delete its entry in `harness::open_findings()`.

**FUZZ-1: any fast-sync manifest from epoch 0 crashes the joining node** (`regressions/snapshot_manifest/fuzz-1-manifest-in-epoch-0`).
`snapshot::verify_manifest` calls `epochs::verify_chain(g, &m.epochs, epoch)` with the epoch of the manifest's last block. For epoch 0, `verify_chain` runs `claimed.sets.range(1..=0)`, and `BTreeMap::range` panics ("range start is greater than range end"). `fast_sync` runs inside `block_on` during `join` and validator start-up, so the process dies, and does so again on every restart that asks the same peer first. Two ways in:
- an honest chain younger than one epoch (600 blocks by default) that already has a certified snapshot;
- any malicious peer, at any time. Everything `verify_manifest` checks before that call (chain id, checkpoint height, block encoding, hash links, the certifying block's state root) is peer-chosen, and none of it needs a signature.

Fix (P-SYNC, `epochs.rs`): return the genesis set when `epoch == 0` (or iterate `range(1..)` up to `epoch`), and add a regression test.

**FUZZ-2: a peer's handoff proof for epoch 0 kills `exchange_epochs`** (`regressions/epochs_registry/fuzz-2-handoff-proof-for-epoch-0`).
`EpochRegistry::merge_sigs` accepts proofs for every epoch whose set matches ours, and that includes epoch 0, whose set is public. `add_sig` then computes `epoch - 1` (`epochs.rs:83`), which overflows. Overflow checks are on in release builds, so this panics. The panic happens inside `chain.lock().merge_epochs(..)` in the spawned `exchange_epochs` task, so the task ends. From then on the node stops collecting and serving handoff proofs, and joiners can no longer verify set changes against it. The fuzzer found this one by itself, from the `handoff-epoch-1` seed.

Fix (P-SYNC): skip epoch 0 in `merge_sigs`/`add_sig` (or use `checked_sub`), and add a regression test.
