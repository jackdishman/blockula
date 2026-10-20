# The v1 replay gate

Protocol v2's rules (programs, [PROGRAMS.md](PROGRAMS.md)) land on main as they're written,
behind version gates. Below the height that activates v2, a v2 binary must be a v1 binary:
every block it replays must end exactly as it did on v1. The replay gate checks this on every PR
and every night ([PROGRAMS-PLAN.md](PROGRAMS-PLAN.md) P2). It's engine-neutral: it replays
history, whatever executes programs later.

- **Every PR:** the PR's binary replays a recorded v1 history that uses every tx tag a block can
  carry, block by block. CI's Node job runs it, so it's part of a required check.
- **Every night:** main's binary replays the public testnet from genesis (`replay.yml`).

A red gate means the PR changed what a v1 block does. Fix the PR, or gate the change behind a
protocol version. Never re-record the corpus to make the gate pass.

## What a replay checks

`blockula replay <archive>` replays an archive in fresh homes, which it removes on success
(`--home` keeps them, `--to` stops early, `--prune-blocks` bounds the disk for long histories
and skips step 3).
An archive is what `scripts/archive-chain.mjs` writes: `meta.json`, `genesis.json`, and the
chain's finalized consensus blocks as `/api/sync` served them, in batches, each with the
finalization certificate of its last block. A recorded archive also holds the recording node's
block log: what each block did there.

It runs the paths a node runs, reusing the node's machinery rather than a replay loop of its
own:

1. **Sync, as a joining node,** on the steps of the node's own sync (`node::apply_batch_trusted`).
   Each batch is checked (`node::verify_batch_trusted`): its certificate and hash links, and the
   checkpoints and validator-set pins this binary embeds for the chain (testnet-2's at blocks
   3,599, 39,599 and 86,399). Then it's executed block by block (`node::execute_batch`,
   `Chain::execute_shared`). Before executing a block, the replay runs `consensus::verify`'s checks
   that need neither a consensus round nor the clock, with `verify`'s own functions
   (`consensus::check_header` and its neighbours), so the gate runs those rules, not a copy of
   them:
   - the protocol version in force at its height;
   - the parent link, and at least a block time after the parent;
   - the proposer: its view's leader in the epoch's election;
   - the height it cites (below its own, a checkpoint's where one is due, recent enough to price
     its txs), and the state root there;
   - its txs: canonical, distinct, passing the stateless checks (signature, Ghost Mode proof,
     expiry), within the fee slot's size limits (the block's and Ghost Mode's), each paying the
     slot's base fee, none finalized before.

   This is all an archive without a recording gets (the testnet's), and it's enough: validators
   finalized every block only after checking the root it cites, so a divergence shows at the
   first block citing a diverged root (for the archive's last few blocks, the next night).
2. **The recording, block by block**, where the archive has one. Each executed block must match
   the recorded one:
   - which txs applied, which failed and which were skipped;
   - the sizes of the skipped ones;
   - the state root;
   - the base fee;
   - then the whole record, as the node logs and serves it: below v2 a block's record is v1's
     ([PROGRAMS-PLAN.md](PROGRAMS-PLAN.md) rule 1), so a field added, dropped or derived
     otherwise fails too.

   Failure reasons aren't compared: releases reword them, and they aren't state (audit F31,
   as `Chain::replay`).
3. **Startup, as a node rebuilding its state database.** The recorded block log goes into a
   second fresh home, which `Chain::open` rebuilds from genesis (`Chain::replay`). That checks
   every tx's outcome and every block's state root along the node's other execution path.

The first difference stops it, naming the height, the tx and what differs. For example, a binary
whose `MIN_NEW_ACCOUNT` became 2 FANG fails the corpus with:

```
Error: block 1: tx 1 (transfer, 2509…f752) failed (sending to a new account needs at least 2 FANG), but the recording says it applied
```

On testnet-2's real history it fails at `block 148 cites state root 3c51… at height 147, where
this binary has 0116…`.

The sync home is scratch (`Chain::set_scratch`): blocks are written without fsyncs and committed
durably every 10,000 heights, and it takes no state snapshots (nothing fast-syncs from it). That
makes the replay CPU-bound: testnet-2's first 99,093 blocks replay in 11 s on an M4 Max (about
8,000 blocks a second), against about 50 a second with a node's fsyncs.

**What it doesn't see:**
- **Rules that never reach a block:** the mempool's own policy, proposal packing, and refusals at
  `POST /api/tx`.
- **`verify`'s checks of the round and the clock:** a block's view and parent view against its
  consensus round (the round isn't in the block: a batch's certificate signs its last block's
  round, and sync doesn't compare it with the block's view), a timestamp against the clock, and
  how far behind the cited height may be (`MAX_EXEC_LAG`, against how far each verifier had
  executed).
- **`verify`'s admission check (CV-5):** every tx admittable on the parent's state as `verify`
  rebuilds it, replaying the ancestors it hasn't executed yet. Execution admits each tx again on
  the state it runs on and skips one that isn't, so a change to the admission rules shows in the
  recording's outcomes; `verify`'s own way to the parent's state doesn't run.
- **Fast sync:** a joiner that starts from a certified snapshot (`/api/snapshot`, `/api/epochs`)
  rather than from genesis.
- **Tags 55–59:** reserved. A block carrying one doesn't decode, so `verify` rejects it and no
  history can hold one.
- **Bytes of tags 62–70 below v2:** no history can hold them for the same reason. C21 adds them
  to the corpus as txs the chain must refuse.

## Every PR: the recorded corpus

[`node/testdata/replay/`](../node/testdata/replay/README.md) holds two recorded chains, about
2,200 blocks between them. Every tag from 0 to 54 and 60–61 applies at least once, except
`Unbond` (7), which v1 always fails.

| Chain | Like | What only it covers |
|---|---|---|
| `v1-open` (1,999 blocks) | testnet-2: no Season 0 admins, operators that aren't their validators, vesting buckets, a contribution pool and founding jurors from genesis | A set of two that admits a third (tags 61 and 60), then grows by stake. Also: every kind of proposal, a cancelled upgrade, Ghost Mode switched on by a vote (shield, private transfer, unshield, pause), equivocation, a beacon miss that jails and an unjail, a junk block |
| `v1-season0` (200 blocks) | `blockula dev`: a self-owned validator that is the Season 0 admin | The admin's levers: approving and removing validators (5, 6), and `SetRepo`, `GrantSoul`, `FlagSybil`, `RejectClaim`, `Clawback` |

The two chains can't be one: `ApproveValidator` and `RemoveValidator` need admins, and admission
into a set of one or two only happens without them. Each runs a Séance.

**How it's made.** `node/src/replay/corpus.rs` (test-only) plays the network:
- each block comes from its view's leader, cites the root it last executed, and restarts its
  views every epoch, with some skipped;
- the epoch's whole set signs a certificate at the end of each batch;
- every block runs through `Chain::execute`.

Every input is fixed (seeds, times, deterministic ML-DSA signing, Ghost Mode proofs from fixed
seeds, the upgrade signal a v1 node sends), so recording again gives the same bytes. Every tx states how it must end (applied,
failed or skipped), and the recorder stops if one ends otherwise (judged as the replay judges
it). The corpus was recorded on
main after P7 (`6b75e74`), with the replay tooling that changes no v1 rule. **The v0.5.2 release
source, which testnet-2 runs, replays it with the same result: every block and every root.** So
the corpus is v1 as released, not just as main ran it.

**In CI** (`ci.yml`, the Node job):
- **`cargo test`** runs `replay::tests`:
  - `the_v1_corpus_replays` replays both recordings through sync and startup, and checks the tag
    coverage.
  - Three negative tests switch on a test-only divergence (`replay::diverge`) and check that the
    gate fails at the block where it starts, on both paths:
    - an admission rule that missed its version gate: the first tx with a memo is no longer
      admitted;
    - an action handler changed without a gate: a transfer that opens an account fails;
    - an end-of-epoch change that no tx's outcome shows: only the state root at block 29 moves.
  - The nightly's case: the same end-of-epoch change, on `v1-open` without its recording, fails
    at block 30, the first to cite the diverged root.
  - A finalized block that `verify` refuses (here, a millisecond early) fails the replay,
    whatever its certificate.
  - A recorded block whose record differs only where no outcome or root shows (its upgrade
    signal) fails at that block.
- **`v1 replay gate (the recorded corpus)`** replays both recordings with the PR's release
  binary.

**Locally:**

```
cargo build --release --locked --manifest-path node/Cargo.toml
for a in node/testdata/replay/v1-*; do node/target/release/blockula replay "$a"; done
cargo test --release --locked --manifest-path node/Cargo.toml replay::
```

**Changing the corpus** is for adding v1 coverage, never for passing a red gate. C21 adds every
gated pre-v2 behavior. To change it:
1. Edit the recorder.
2. Write the corpus with `cargo test --release --locked --manifest-path node/Cargo.toml --
   --ignored write_replay_corpus`.
3. Replay both recordings with the newest release binary
   (`for a in node/testdata/replay/v1-*; do blockula replay "$a"; done`, from the first release
   that ships the command). A difference there means the recording isn't v1.

## Every night: the testnet from genesis

`.github/workflows/replay.yml` runs daily (and on demand):
1. Pulls the staged archive. Any error fails the run, "not found" included: registries word a
   missing package, no access and an outage alike, and going on would start the archive over
   from height 1. Only the first run starts fresh (below).
2. Copies what's new from the public node's `/api/sync` with `scripts/archive-chain.mjs`.
3. Replays it all from genesis with `--prune-blocks 20000`. Its output goes to a file, not the
   run's public log (below).
4. Stages the files that closed.

If it fails, it opens an issue labeled `replay-alert` that @-mentions the maintainer, or comments
on the one already open. The next good run closes it. Its long steps have their own time limits
(the replay's is three hours) inside the job's, so a step that runs out of time fails and alerts
too. A red replay stages nothing, so a divergence on main stops the staged copy growing: after
12 days of red runs, the node no longer holds the missing heights, and the fix needs a re-seed
(below).

### A red night

A divergence or a crash on the chain's real history may be an undisclosed vulnerability: a block
the released binary can't replay is one ([SECURITY.md](../SECURITY.md)). The repository and its run
logs are public, so the replay's output never reaches the log (a good run prints its summary),
and a red run's leaves the runner only sealed to the maintainers' age keys, as `fuzz.yml` seals a
fuzzer crash: the repository variable `FUZZ_CRASH_AGE_RECIPIENTS`, through
`scripts/fuzz-seal-crash.sh`. The run's `replay-failure` artifact decrypts with
`age -d -i <identity> fuzz-crash-replay.tar.age | tar -x` and holds `replay.log`. Without the
variable nothing is kept. Either way the failure reproduces privately:

```
cargo build --release --locked --manifest-path node/Cargo.toml
cp -R ~/.blockula/archive/blockula-testnet-2 /tmp/testnet-2   # a copy: the launchd job writes the original
node/target/release/blockula replay /tmp/testnet-2 --prune-blocks 20000
```

Handle it as a security issue until it's understood. The usual cause is main's own change: fix
it, or gate it behind a protocol version.

### The archive CI reads

P0's archive lives on the maintainer's Mac (a daily launchd job writes
`~/.blockula/archive/blockula-testnet-2`), and the public node prunes at 14 days. There were two
ways to give CI the whole history:

| | The public node keeps everything (`--archive`) | Stage the closed files on GitHub's container registry |
|---|---|---|
| Storage | The node's Fly volume grows about 30 GB a year, forever, and someone grows the volume | Public packages are free. About 13 MB per 100,000 heights, about 4 GB a year |
| Each night | Download the whole history again over `/api/sync`: about 14 GB, and hours of rate-limited requests, after a year | Pull from GHCR, which is free and fast from Actions. Only under 100,000 heights come from the node |
| Testnet change | A config change and a redeploy | None |

**Staging is cheaper, and it's what the nightly does.**
- The staged copy is a public OCI artifact, `ghcr.io/jackdishman/blockula-history:<chain id>`,
  pushed with `oras` by `scripts/stage-archive.sh`.
- It holds `meta.json`, `genesis.json` and the closed `sync-*.jsonl.gz` files: the same files
  and format as P0's archive.
- CI builds its own copy from the public node rather than uploading the Mac's, so the launchd
  job doesn't change. Every file it stages has already replayed: checkpoints, certificates and
  roots checked.
- The Mac's archive stays an independent backup, and the source to re-seed from.

### The first run

Nothing is staged until the first run, and scheduled runs never start over, so run the workflow
once by hand with **`fresh`** (Actions → Replay testnet history → Run workflow → fresh). It
skips the pull, archives from height 1, replays and stages the closed files, which creates the
package. That works while the public node still holds block 1 (until about 2026-11-01); after
that, seed it (below).

**Then check the package is public:** github.com → Your profile → Packages → `blockula-history`
→ Package settings → Change visibility. A package first pushed by a workflow can start out
private. CI can read it either way, but only public packages' storage is free.

### Seeding the staged archive

A run that needs heights the node no longer holds fails at its `archive` step: a fresh run after
about 2026-11-01, or one after more than 12 days without a good run. Then stage the Mac's copy
once, from a machine that has it:

```
gh auth refresh -s write:packages
gh auth token | oras login ghcr.io -u jackdishman --password-stdin
scripts/stage-archive.sh push ~/.blockula/archive/blockula-testnet-2
```

This stages only the closed files (each checked against `meta.json`, read once, so the launchd
job can keep writing). The next nightly continues from the last one. A package first pushed by
hand doesn't let the workflow in: in its Package settings, under Manage Actions access, add
`jackdishman/blockula` with the Write role (and make it public, as above).

### Time and relaunches

- **Replay time** grows with the chain: about 86,000 blocks a day. testnet-2 at its programs
  activation (about 20M blocks) replays in about 40 minutes at the rate measured on an M4 Max. A
  GitHub runner is slower, so expect a few times that: inside the replay step's three-hour limit.
  Past that, replay from certified snapshots in shards.
- **A relaunched testnet** has a new chain id. The workflow refuses a node serving another chain
  until `CHAIN_ID` in `replay.yml` is updated. The old chain's archive stays staged under its own
  tag.
