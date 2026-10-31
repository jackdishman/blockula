# The node's calibration patterns (S4c)

W1's `calibrate` bench (`CALIBRATION.md`, `-x86_64.md`, `-aarch64.md`) times the VM over an
in-memory host. These are the patterns it can't see, timed through the node's own path:
`State::run_block` and `State::commit_block` (the state tree's updates), and apart from them the
writer's `persist` (the block logs and the database write, fsynced as in production), on a dev
chain with programs on. Spec `docs/PROGRAMS-V0.md` §5.2 item 3; the source is
`node/src/programs/bench.rs`, three `#[ignore]`d patterns in one test:

| Pattern | The block | The rule is judged on |
|---|---|---|
| minimal calls | `MAX_BLOCK_TXS` (512) calls of a one-instruction `nop` to the smallest valid code | the VM's share of `run_block` + `commit_block`: the block's, less the same block of 512 native transfers'; `persist` printed apart |
| write storm, small keys | 24 calls, each writing 256 distinct one-byte keys (the most one transaction may), each to a program of its own so every write is a new entry | the whole block, `persist` included (also printed apart) |
| write storm, large values | 4 calls, each writing 48 values of 16 KiB (a transaction may write 1 MiB) | the whole block, `persist` included (also printed apart) |

The rule (PRG 4.5): no pattern slower than **2.5 ns per counted fuel** on either floor runner (so a
`CALIBRATED_BLOCK_FUEL` block runs in 150 ms). Counted fuel is what the schedule says the work was:
per transaction, its code's load fuel plus the fuel the VM metered (`FuelUse::load + used`); the
block's budget counts the reservations (`limit + load`), printed beside it. The worst of the
repetitions decides. The test fails on a `NO-GO`.

Why the minimal calls are judged on the VM's share: a call pays, before and after any program
runs, the native work of every transaction (`Tx::check_format`, the fee phase, the account writes),
which no fuel prices and which `MAX_BLOCK_TXS` and the byte limits bound. The share the schedule
has to cover is the rest, so the bench times a block of as many native transfers on the same state
(a warm-up run first) and subtracts it; the native cost per transaction is printed beside every
pattern. `persist` is mostly `fsync` on a Mac (an empty block's is 18 ms here): it makes a
per-fuel figure that includes it noisy, and for the minimal calls it says nothing about the VM. The
write storms keep it in their verdict because §5.2 asks for the database write.

```
cd node && cargo test --release --locked programs::bench -- --ignored --nocapture --test-threads=1
```

## Local runs (not the floor)

An Apple M4 Max (16 cores, macOS 26.6) with the machine's other builders running, two runs of this
commit's bench, the first under a load average of about 5 and the second of about 4. The floor
runners' numbers are what count: this Mac is faster than the floor machines in compute, and its
`fsync` (`persist`) is slower than a Linux runner's. Each cell is the median of the runs' blocks
(5 for the minimal calls, 3 for the storms); the last column is the median, then the worst.

| Pattern | counted fuel | run_block | commit_block | native transfers (same count) | persist | ns per counted fuel (judged: median, worst) |
|---|---|---|---|---|---|---|
| minimal calls, 512 txs (run 1) | 8,755,712 (17,101 a tx; reserved 9,256,960) | 79.20 ms | 0.05 ms | 40.39 ms | 28.99 ms | **VM share 4.44, 5.44: NO-GO** |
| minimal calls, 512 txs (run 2) | same | 73.23 ms | 0.05 ms | 40.86 ms | 24.86 ms | **VM share 3.70, 4.33: NO-GO** |
| small keys, 24 txs (run 1) | 32,521,752 (reserved 38,771,520) | 12.76 ms | 5.31 ms | 2.13 ms | 39.91 ms | whole block 1.78, 1.79: GO |
| small keys, 24 txs (run 2) | same | 11.42 ms | 4.97 ms | 1.96 ms | 36.56 ms | whole block 1.63, 1.70: GO |
| large values, 4 txs (run 1) | 32,537,348 (reserved 38,061,920) | 1.16 ms | 1.83 ms | 0.43 ms | 46.59 ms | whole block 1.52, **3.26: NO-GO** (one repetition's `fsync`) |
| large values, 4 txs (run 2) | same | 1.13 ms | 2.01 ms | 0.34 ms | 33.64 ms | whole block 1.13, 1.20: GO |

Per call of the minimal-call block: run 1 block work 154.8 µs, a native transfer 78.9 µs, the
share 75.9 µs; run 2 143.1, 79.8, 63.3. As ns per counted fuel (run 1, run 2): the block's work
9.05 and 8.37, the VM share 4.44 and 3.70, `persist` 3.31 and 2.84.

A block of the default budget (40M fuel) can't be built from minimal calls: 512 transactions count
8.76M fuel, 22% of it, and `MAX_BLOCK_TXS` bounds the block first. The block's time is 512 times
the per-transaction cost, 73 to 79 ms of work here, none of it scaling with fuel. For the storms,
which do fill the budget, a block of 40M fuel at the rates above takes about 60 to 70 ms, and one
of `CALIBRATED_BLOCK_FUEL` (60M) 90 to 107 ms, inside the 150 ms (`persist`'s fixed cost is scaled
along with the work, so these overstate).

The bench's context table times each fixed cost on its own (per call, over 512 repetitions; run 1,
run 2):

| | per call |
|---|---|
| a thread with the 64 MiB stack, started and joined | 39.6 µs, 23.3 µs (25 to 33 µs in earlier runs: it moves with the machine's load) |
| a new engine (its linker) | 7.4 µs, 4.4 µs |
| a new engine and the load (translation) of the smallest code | 9.1 µs, 6.1 µs |
| `Tx::check_format` of a call | 45.8 µs, 51.4 µs |
| `Tx::id` of a call | 12.8 µs, 13.9 µs |
| a native transfer, `run_block` (512 of them: 40.3 and 40.5 ms) | 78.7 µs, 79.2 µs |

## Where the time goes

S4c's review profiled 120 repetitions of `State::run_block` on the 512-call block (78.6 ms, 153.5
µs a call) with `sample(1)` and timed the pure functions directly (a call transaction is 5,589
bytes). A minimal call counts 17,101 fuel: 12,080 of load (10,000 + 40 a byte of its 52-byte
module) and 5,021 of instantiation and the `nop`.

| What | per call | share of the call |
|---|---|---|
| `Tx::check_format`: decodes the 1,952-byte key and the 3,309-byte signature from hex three times (`to_bytes`, `from_bytes`, and the re-encoding inside `from_bytes`) | 62.6 µs | 41% |
| `Tx::id()` inside `programs::exec::Run::new`: S4a's code hex-decodes the signature again, just to name the transaction for fault attribution | 26.4 µs | 17% |
| the program thread and its join (spawn, engine, load, instantiate, run, exit) | 29.5 µs | 19% |
| the rest of `call::apply` | 7.7 µs | 5% |
| the rest of `apply_j` | about 19 µs | 13% |

Inside the program thread: engine creation 28%, load 19%, execution 18%, the thread's own start and
exit 27%. One sender or 128 makes no difference (`run_block` +9%).

So about 58% of a minimal call is two pure functions every path runs for every transaction
(mempool, trial, verification, replay, execution), and `Tx::id` is not paid by a native transfer.
The part the schedule has to cover, the thread, the engine, the load, the instantiation, the run
and the bookkeeping, is about 37 µs: **2.2 ns per counted fuel on this machine**, near the 2.5 line.
This replaces the first diagnosis (the builder's 80 µs for a native transfer subtracted from
155 µs, which left "75 µs of VM" at 4.4 ns and so charged the call for the `Tx::id` a transfer
doesn't pay).

The bench's own verdict, the block less 512 native transfers, still contains what a transfer
doesn't pay and isn't the VM's: that `Tx::id` (13 to 14 µs a call here, 0.8 ns per counted fuel)
and the call's other bookkeeping. That is why it reads 3.7 to 4.4 here against the profile's 2.2:
the profile counts the program thread and the rest of `call::apply`, the difference of two blocks
counts everything a call does that a transfer doesn't, and under load the thread's own cost moves
from 23 to 40 µs. The two don't disagree about the order of magnitude; the floor runners decide.

## What the NO-GO means

**The NO-GO stands as measured, and it is mostly v1's native per-transaction work, not VM setup.**
Of the 143 to 155 µs a minimal call takes in `run_block`, about 79 µs is a native transfer's cost,
and `Tx::id` adds another 13 to 26. Neither of the spec's fallbacks addresses that:

- **Raising `LOAD_BASE` does not fix it.** For the whole 153 µs call to pass at 2.5 ns per counted
  fuel it would have to reach about 55,000 to 60,000 (5.5 times today's 10,000), pricing hex
  decoding as VM work: a native transfer pays the same 79 µs and is not charged any fuel. Every
  program call's load fuel would be 5 times too dear for the VM's own cost.
- **One thread and one engine per block pass (A3) does not fix it either.** It removes at most about
  28 µs a call, 18% of it (the thread and engine setup, and the loads of code repeated within the
  block), which takes the VM's share to about 0.5 to 1 ns per counted fuel. It costs no version, and
  is worth taking for margin on the VM share, but the block's time stays what 512 transactions'
  native work makes it.

*Superseded by "A3: one thread and one engine per block pass", below.* A3 took about 150 to 180 µs
a call out of the VM's share on the floor runners, not 28, and the floors then measured the
minimal calls at 1.50 to 1.66 ns per counted fuel, every pattern GO (run 40763852590). The two
bullets above are kept as the reading made before A3; the one about `LOAD_BASE` still holds (it
isn't the fix for native work).

## After stage N: the native work made cheap

Stage N (WBS "N. Native per-transaction cost") did step 1 below, with no result changing: the key
and the signature are decoded once and no longer written out a second time to check the wire form
(`Tx::to_bytes`, `from_bytes` and `id` use a table decoder for the lowercase hex every valid
transaction carries, `is_lower_hex` is a fold over the bytes, and anything else still goes to the
`hex` crate; the
differential tests in `types::tests` hold each to the old function's answers and error text), and
the proposer's and a verification's trials hand the id they already hashed for the trial marker to
`programs::apply` and `Run::new`. A block's own run (`run_block`: execution, the replay of
ancestors) hashes a program transaction's id once, where it is needed (the receipt's key), because
nothing before it holds the ids.

Back to back on the same Mac, the commit before stage N and the one after, five pairs (load
average 8.6 falling to 4.8 as the pairs went; the spread is min to max over the five):

| | before | after |
|---|---|---|
| `Tx::check_format`, a v1 transfer / a program call | 47.8 µs (46.2 to 50.0) / 48.6 µs (48.0 to 49.6) | 7.4 µs (7.2 to 7.5) / 8.2 µs (7.9 to 8.4) |
| `Tx::id`, a v1 transfer / a program call | 13.2 µs (13.1 to 13.6) / 13.7 µs (12.7 to 13.9) | 5.1 µs (5.1 to 5.2) / 5.2 µs (5.1 to 5.3) |
| a native transfer, `run_block` (the context table; 512 of them) | 81.8 µs (77.5 to 82.9): 41.9 ms | 22.6 µs (22.2 to 24.3): 11.6 ms |
| minimal calls, `run_block`, 512 txs | 80.1 ms (74.8 to 84.7), 156.6 µs a call | 37.7 ms (36.1 to 40.0), 73.7 µs a call |
| the same block of native transfers | 40.6 ms (39.3 to 41.5), 79.3 µs a tx | 11.4 ms (11.3 to 12.0), 22.3 µs a tx |
| **VM share** (the difference) | 39.6 ms, 77.3 µs a call | 26.1 ms (24.9 to 28.1), 50.9 µs a call |
| ns per counted fuel (17,101 a call): block work / **VM share**, median (worst of the five reps in a run, median of runs) | 9.16 / **4.52** (4.80) | 4.31 / **2.98** (3.19) |
| `persist` of the minimal-call block (fsync) | 25.4 ms (24.7 to 51.8) | 26.6 ms (24.7 to 30.5) |
| write storm, 256 keys: `run_block` / `commit_block` | 12.6 / 5.3 ms | 10.3 / 4.9 ms |
| write storm, 48 values of 16 KiB: `run_block` / `commit_block` | 1.5 / 1.9 ms | 1.1 / 1.9 ms |

The storms' verdicts (judged with `persist`, 38 to 40 ms of it, 1.7 to 1.8 ns per fuel for the
256-key block, 1.2 to 1.3 for the large values, before and after) are `fsync`'s: a repetition's
`persist` made a NO-GO in 2 of the 5 runs before and 3 of the 5 after (a worst repetition of 3.0 to
3.7 ns against those medians).

**What this says.** The minimal calls' VM share is still over the 2.5 line on this Mac, at 2.98
(worst 3.19), a third lower than before. It still contains what a call does that a transfer
doesn't and isn't the VM's: `Tx::id` inside `Run::new` (5.2 µs, 0.30 ns per fuel; without it the
share is 2.67). What remains of a call's 73.7 µs, in the context table's figures: a native
transfer's 22.3 µs (`check_format` 8.3 of it), `Tx::id` 5.2, the program thread 23.8 (20.8 to
27.6: it moves with the machine's load), the engine 4.7, loading the code 1.7, and what's left, about
16 µs (the remainder of the 73.7, not measured on its own), for instantiation, execution, the
receipt and the commit. A3 (one thread and engine per block pass) takes at
most the thread and the engine, about 28 µs a call, and so takes the VM share to about 1.3 ns per
counted fuel here; the floor runners decide, as before. (Measured since, in "A3" below: 1.00 on this
Mac, and 1.50 to 1.66 on the floor runners.)

The stage's review measured again on a quieter machine (2026-10-28): a VM share of 2.57, 2.69 and
2.80 ns per counted fuel after stage N (3.61 to 4.09 before), and 2.27, 2.39 and 2.50 without
`Run::new`'s id. Minimal calls stay over the line in every run, but narrowly: the margin moves with
the machine's load, which is one more reason the floor runners decide.

## Next steps

1. **Done by stage N** (above): `Tx::check_format` and `Tx::id` are cheap, with the replay gate and
   the recorded corpora as the proof nothing changed. Left for a stage that may touch more of
   `chain.rs`: ids are still hashed separately by `block_meta`, `receipt::for_block` (for every
   transaction of a block with a program transaction) and `decode_txs`, and `verify_signature` and
   `signer_key_id` still use the `hex` crate on the key and the signature (the signature's
   verification itself dominates the first).
2. **Done** (below, "The floor runners before A3"): the benches ran on both floor runners with the native
   work cheap. `calibrate-vm.yml`'s `node-benches` job runs them.
3. **Then A3** (one thread and one engine per block pass): done, see "A3" at the end. The floors
   passed after it (run 40763852590, "The floor runners after A3"), so the fallbacks the spec lists
   next, a per-block cap on program transactions and then `LOAD_BASE`, are not needed for the
   minimal calls. Leave `LOAD_BASE` alone.

What these numbers don't show: floor-runner behaviour (Linux thread creation and `fsync` differ from
macOS); a 40M-fuel block of mixed calls; the cost of the proposer's fourth run of a block; and
whether `LOAD_BASE`'s price is right for large codes (only a 52-byte module was timed). The NO-GO
isn't frozen input to `SCHEDULE_V0` or `LOAD_BASE`: calibration (S8) waits for steps 1 and 2.

## The floor runners before A3 (2026-10-28)

`calibrate-vm.yml` run 40742671981 (profile `full`: three runs of the bench on each runner, back to
back), at the programs integration branch after stage N (021ffece). The runners are 4-vCPU Linux
VMs: `ubuntu-24.04` (AMD EPYC 7763) and `ubuntu-24.04-arm`. The load averages ran from 3.7 to 4.9.
Each cell is a run's median, given as the range over the three runs (the verdict's worst
repetition in brackets).

| | x86_64 | aarch64 | the M4 Max after N |
|---|---|---|---|
| **minimal calls, VM share, ns per counted fuel** | **10.62 to 10.95 (worst 12.47): NO-GO** | **11.26 to 11.54 (worst 11.91): NO-GO** | 2.57 to 2.98 |
| block work per call / a native transfer / the VM's share | 220.5-226.8 / 38.6-39.5 / 181.7-187.3 µs | 228.5-233.4 / 35.8-36.0 / 192.6-197.4 µs | 73.7 / 22.3 / 50.9 µs |
| the 512-call block: `run_block` + `commit_block` / `persist` | 112.9-116.1 / 16.5-21.3 ms | 117.0-119.5 / 27.7-32.2 ms | 37.7 / 26.6 ms |
| write storm, 256 one-byte keys: whole block | 1.59 to 1.67 (worst 1.75): GO | 1.68 to 1.73 (worst 1.76): GO | 1.7 to 1.8 |
| write storm, 48 values of 16 KiB: whole block | 0.43 to 0.53 (worst 0.58): GO | 0.91 to 0.93 (worst 0.96): GO | 1.2 to 1.3 |

The context table, per call:

| | x86_64 | aarch64 | the M4 Max after N |
|---|---|---|---|
| a thread with the 64 MiB stack, started and joined | 87.5-99.1 µs | 70.7-72.7 µs | 20.8-27.6 µs |
| a new engine / and the load of the smallest code | 7.2 / 12.1 µs | 7.6-7.7 / 11.3-11.4 µs | 4.7 / 6.4 µs |
| `Tx::check_format` / `Tx::id` of a call | 13.6-14.0 / 6.5-6.6 µs | 13.1-13.2 / 8.1-8.2 µs | 8.2 / 5.2 µs |
| a native transfer, `run_block` | 38.5-38.9 µs | 35.5-35.8 µs | 22.6 µs |
| `persist` of an empty block | 1.59-1.66 ms | 2.28-2.61 ms | 18 ms |
| what's left of the VM's share (not timed alone: instantiation, the run, the view, the receipt, the commit) | 70-79 µs | 100-106 µs | about 16 µs |

**What this says.** The write storms pass on both floors, with room. The minimal calls fail by a
factor of four to five, and not because of the VM. W1's bench on the same runners
(`CALIBRATION-x86_64.md`, `-aarch64.md`, section 3) times a transaction calling a no-op on a
one-page module through `execute`, the executor's fixed cost included, at 2.6 µs. The VM's own
patterns stay under 1.9 ns per fuel, and a whole `max_block_fuel` block takes 68 to 79 ms.

The cost is the node's fixed work around each call. Pure compute (`check_format`, an engine) costs
these runners about 1.5 times what it costs the Mac. The program thread and the untimed rest cost
four to six times as much, which points at the OS:
- a fresh 64 MiB stack is mapped and unmapped for every call (glibc's stack cache holds 40 MiB by
  default, so it never reuses one);
- every page the call touches is a first-touch fault;
- the unmap is a TLB shootdown on a multi-vCPU VM.

These are hypotheses, for stage A3 to measure. The same W1 run bisects the native stack the deepest
call needs at 40 KiB on x86_64 and 16 KiB on aarch64 (section 7).

A3 takes the thread and the engine: 80 µs (aarch64) to 105 µs (x86_64) of a call's 182 to 197.
That leaves about 80 to 115 µs, roughly 5 to 7 ns per counted fuel, still over the line, unless the untimed rest is mostly
the fresh stack's faults and goes with it. So the per-block cap or `LOAD_BASE` is likely to be
needed after A3. Which one is the maintainer's choice, made on A3's numbers.

*Superseded by "A3: one thread and one engine per block pass", below.* The untimed rest did go with
the thread: on the floors it fell from 61 to 68 µs (x86-64) and 88 to 106 µs (aarch64) to about 10 and
8, and the minimal calls came out GO at 1.50 to 1.66 ns per counted fuel. The per-block cap and
`LOAD_BASE` are not needed for them.

The 512-call block itself takes 113 to 120 ms of block work, inside the 150 ms the rule sizes a
block for. A block can, though, carry both the 512 calls' fixed cost and a full fuel budget, which
the rule prices at 2.5 ns a fuel: about 97 ms + 100 ms at the default 40M. That sum is what a cap
or `LOAD_BASE` has to bound.

## A3: one thread and one engine per block pass

Spec §5.2 item 3's first fallback, built on the integration head after the floor runners' NO-GO
(`5a8aded4`). Executing a block, replaying an ancestor, a verification's apply loop, a proposer's
packing and the startup replay each run their transactions as a **block pass** (`programs/pass.rs`):
when it holds a program transaction (tags 62-64) the whole loop runs on **one thread** with the
64 MiB stack, started in a `std::thread::scope` and joined at its end, and each program
transaction's VM work runs inline on it, on **one engine** built when the first transaction that needs
one runs. Every transaction still loads its declared code in hash order (code the engine holds is
W1's no-op) and is still counted its whole load at admission, so no outcome, fuel figure, receipt,
error text, fault record or state root changes, and no protocol version is needed. A pass with no
program transaction (every v1 block) runs as before, with no thread and no engine; `/view` and a lone
`State::apply` keep a thread and an engine per run; a pass thread the OS refuses three times falls
back to a thread per transaction, the first meeting the refusals and the `Setup` fault as it always
did. No `unsafe`: the loop's captures cross to the pass thread (`State` is `Send + Sync`, asserted),
a panic comes out at the join on the caller's thread, and the engine, which isn't `Send`, lives in a
thread-local of the thread that built it.

`calibrate-vm.yml`'s `node-benches` job runs the benches on both floor runners (this stage brings
the job into the integration branch; it ran from the calibration branch for run 40763852590):
three runs, each timing the block passes and then the path before A3 (`BLOCKULA_BENCH_PER_TX=1`),
back to back.

### Why no outcome, fuel, receipt, fault or state root changes

Consensus counts a transaction's load from its own declaration (`programs::fuel::load`, `fuel.rs:94`,
summed into `reserved`, `fuel.rs:103`, and `count_fuel`), never from what an engine holds, and a
receipt's `fuel_used` is the VM's metered use alone (load fuel "is the node's", `vm/src/exec.rs`,
`Receipt`). What an engine returns to a transaction is a function of the transaction, as follows
(line numbers are at this commit; soroban-wasmi's are 29.0.0's).

1. **Compilation is eager, and meters nothing.** `compilation_mode(Eager)` is inherent: `load`
   translates every body in `Module::new(&self.engine, wasm)` (`vm/src/engine/soroban.rs:216`), and
   nothing translates while a call runs (the deadlock notes, `soroban.rs:24-33`, are why `load`
   takes `&mut self`). "Translation and instantiation never consume fuel; `exec.rs` charges
   `instantiate_fuel`" (the table at the top of `soroban.rs`). So no lazy translation is metered, or
   deferred into some later call: a transaction that finds its code already translated skips work
   that was never charged.
2. **`info()` and instantiation cost don't depend on what else is loaded.** `info` returns
   `modules[hash]`'s `CodeInfo` (`soroban.rs:221`), which `load` read from that code's own sections
   (`CodeInfo::read`, `vm/src/engine/mod.rs`); the executor charges
   `schedule.instantiate_fuel(&frame.info)` from it (`vm/src/exec.rs:317`). Every frame runs in a
   store of its own (`Store::new`, `soroban.rs:229`) holding its own fuel (`add_fuel(fuel)`), instance
   and memory, dropped when the frame returns, so none of it outlives a frame. What an engine keeps
   between frames is the translated code, the deduplicated function types (compared only for
   equality, so their order is invisible), the host linker and at most two recycled value stacks,
   and a recycled stack is cleared first (`self.stack.reset()`, soroban-wasmi `engine/mod.rs:649`,
   the first line of `EngineExecutor::execute_func`). Within one transaction the engine already
   serves many frames this way.
3. **`execute` reaches only the declared set, never what the engine has loaded.** `reachable` is
   built from the calls and the bindings alone (`vm/src/exec.rs:186`); `resolve` returns
   `UnboundProgram` for anything else before it asks the view or the engine a thing (`exec.rs:376`);
   a declared program's code comes only from `view.code_of` (`exec.rs:383`), and the node's
   `ProgramView` answers for its declared set alone (`programs/view.rs:69-91`, `:137-149`).
   `engine.info(&code)` (`exec.rs:388`) is therefore asked only of a code the transaction itself
   declared, which `ProgramView::codes` (`view.rs:105`) lists and `load_and_execute`
   (`programs/exec.rs:184`) loaded just before. `the_declared_set_decides_what_a_transaction_reaches_not_the_engine`
   makes the engine hold a code the transaction doesn't declare and shows it fails exactly as it does
   with an engine of its own.
4. **Loading a code again is a no-op, and a hash names one byte string.** `load` returns early when the
   hash is present (`soroban.rs:212`). The hash is `digest::wasm_hash`, BLAKE3's `derive_key` of the
   code bytes (`programs/digest.rs:39`), and stored code is written once (`programs/upload.rs`:
   inserted only when `new_code`), so the bytes behind a hash can't differ within a pass. A load that
   would fail does so the first time (a `Tx` fault: the engine is dropped with it); one that succeeded
   succeeds on every engine with the same configuration, translation being a function of the bytes
   and the schedule's op costs. The pass never shares an engine across schedules (`pass.rs`,
   `with_engine`).
5. **After any fault or caught panic the engine is dropped**, by two routes, each with a test that
   fails without it while the other is in place. `with_engine` takes the engine out of its slot
   while a transaction runs and puts it back only on `Ok`, so a panic unwinds with it:
   `a_run_that_faults_or_panics_leaves_no_engine_for_the_next` calls `with_engine` itself with a
   run that returns, one that faults and one that panics, and fails when the put-back is
   unconditional (the other route notwithstanding). `programs::apply` discards the engine when it
   records a fault, whichever step raised it: `a_fault_outside_the_vms_run_drops_the_engine_too`
   meets a fault raised before the VM is reached, which the first route never sees, and fails when
   `apply` stops discarding. `a_fault_means_the_next_program_transaction_gets_a_new_engine` (a fault
   inside the VM, in a proposal's packing, the one pass that goes on after a fault) is covered by
   both and fails only when both are gone. A pass's engine that can't be built is a `Setup` fault
   for that transaction with nothing kept (`an_engine_that_cannot_be_built_is_a_setup_fault_and_the_next_call_gets_a_new_one`).

The differential tests hold the rest: `diff_whole_blocks_read_the_same_as_a_pass_and_as_a_thread_per_transaction`
(two nodes of one chain, one executing as passes and one the old way, over uploads of new and held
code, creates with and without `init`, call batches over shared and disjoint code, payable calls, a
payout, every way a call fails and each per-transaction bound, native transfers between them, a
transaction that never runs: the blocks compare equal as JSON, `programs` field and state root
included), `diff_a_fault_at_the_first_a_middle_and_the_last_program_tx_reads_the_same` (every result,
the recorded fault, the refused commit and the receipts, for a panic in the VM and for a thread the
OS refuses twice and three times) and
`diff_proposals_and_verifications_read_the_same_with_a_fault_at_the_first_a_middle_and_the_last_program_tx`
(what a proposal packs, the quarantine, what stays queued, the trial marker's file as each
transaction's view is made, the verdicts and the digests recorded as verified clean).

### The engine's memory bound

soroban-wasmi keeps every translated function until its engine is dropped, so an engine shared across
a block grows with the distinct code its transactions declare, where a thread per transaction held one
transaction's at a time (at most `MAX_TX_CODE_BYTES`, 512 KiB). `pass::ENGINE_CODE_BYTES` (**1 MiB**,
twice that; first built as 2 MiB, lowered after the fix's measurement below) drops the pass's engine
when the code about to be loaded would pass it, **before** the next transaction's code is translated
(the engine it replaces is not kept until `with_engine` returns, as the first version did, holding both while the new transaction's code translated; and
`the_memory_bound_rebuilds_the_engine` now counts the engines alive beside a transaction's work); the
next transaction gets a new one, and a transaction's whole declared set always fits an empty engine.
The bound is node policy: it changes when an engine is rebuilt (a few microseconds about every second
maximal transaction, and the load it re-translates was charged to each of them already), no outcome.
A const assert keeps it at least `MAX_TX_CODE_BYTES`.

How much a full engine holds was measured with a counting global allocator (scratch programs outside
the repo, on this Mac, release build: the build's `measure-heap-main.rs` and the fix's
`measure-heap-fix-main.rs`, beside the build report), through `BlockEngine::load`, on code that
passes `blockula_vm::validate::validate` (each row is checked). **The build's table was wrong**:
its densest row, a `br_table` of 250,000 targets, is refused by the engine (wasmparser allows 20,000),
so the row dropped out and the table's worst, 11 bytes per byte, was an engine's cost for 6,250
one-byte-operator functions. `br_table`s of up to 20,000 targets, repeated, are valid and are the
densest code: each one-byte target becomes two 8-byte instructions (`BrAdjust` and `Return`,
soroban-wasmi `translator.rs`), and an element segment's one-byte function index becomes a 24-byte
entry. Modules of about 250,000 bytes, loaded into a fresh engine:

| shape of the code | heap held after `load`, per byte of code | peak while loading, per byte |
|---|---|---|
| straight-line `i32.const; i32.const; i32.add; drop`, one function / 250 functions | 5.4 / 5.5 | 13.7 / 5.5 |
| a local updated in a loop body; `call`s; `if … end`; loads; a deep operand stack (250 functions each) | 4.7; 4.2; 5.0; 4.2; 5.5 | 4.2 to 5.5 |
| 64-bit constants, all distinct, 20 functions | 5.7 | 6.0 |
| `i32.eqz` chains, one function / 250 functions | 8.0 / 8.2 | 16.4 / 8.3 |
| 6,250 functions of 40 bytes of one-byte operators (the most functions validation allows) | 11.1 | 11.2 |
| 13,000 distinct function types of 16 parameters | 6.1 | 14.8 |
| 1,000 data segments | 0.8 | 0.8 |
| deep block nesting (83,000 levels, one function) | 0.0 | **35.7** |
| deep nesting with a `br_table` to every level (20,000 levels, 2 functions, 207,000 bytes) | 3.1 | 20.2 |
| `br_table`s of 20,000 targets (to the enclosing block, or to the function), one function | 16.0 | **43.3** (to the function: 34.9) |
| the same in 12 / 15 / 250 / 1,000 functions | 21.3 / 17.1 / 16.3 / 15.9 | 26.6 / 22.3 / 16.4 / 15.9 |
| 5,500 functions of about 42 bytes, each a 33-target `br_table` | 20.4 | 20.6 |
| element segments (61 of 4,096 items / 1,000 of 244) | **24.0** / 23.7 | 24.4 / 23.8 |

The same code holds more than its own translation when its functions arrive in the wrong order,
because the engine's code map is one `Vec<Instruction>` for every module it holds
(`CodeMap::init_func` extends it, soroban-wasmi `engine/code_map.rs`): a `Vec` that doesn't fit what
it is given grows to the larger of twice its capacity and what is needed, so its capacity steps by
doubling from whatever the first function made it, and stays under twice the instructions loaded.
A `br_table` target is two 8-byte instructions and nothing is denser, so the code map holds under
2 × 2 × 8 = 32 bytes per code byte whatever the code, and code whose last function just crosses a
step holds nearly that. Filling an engine to the bound (heap held, per byte of code loaded), at the
2 MiB the build chose and at the 1 MiB it is now:

| engine filled with | 2 MiB: code, heap, per code byte | 1 MiB: code, heap, per code byte |
|---|---|---|
| `br_table` codes of 262 KB, one function each | 2.00 MiB, 32.0 MiB, 16.0 | 1.00 MiB, 16.0 MiB, 16.0 |
| `br_table` codes of 246 KB, 12 functions each (the review's) | 1.88 MiB, 40.0 MiB, 21.3 | 0.94 MiB, 20.0 MiB, 21.3 |
| codes of 5,500 functions with a 33-target `br_table` | 1.89 MiB, 38.5 MiB, 20.4 | 0.94 MiB, 19.3 MiB, 20.4 |
| codes of 61 element segments of 4,096 items | 1.91 MiB, 45.8 MiB, 24.0 | 0.95 MiB, 22.9 MiB, 24.0 |
| codes of deep nesting with a `br_table` at the bottom | 1.58 MiB, 4.9 MiB, 3.1 | 0.79 MiB, 2.5 MiB, 3.1 |
| **`br_table` codes of 260 KB, one function each, then a 149-byte one** (8 / 4 of them) | 1.98 MiB, **63.5 MiB**, **32.0** | 0.99 MiB, **31.7 MiB**, **32.0** |
| the same with the small one first (no step) | 31.7 MiB | 15.9 MiB |
| what one transaction held under a thread per transaction (2 such codes and the small one) | 0.50 MiB, 15.9 MiB, 32.0 | |

The bold row is the worst found: identical 260 KB codes (the scratch program loads one module under
several hashes; codes that differ in one operand translate to the same size) make the code map's
capacity N, 2N, 4N (8N at 2 MiB), and a code of any size that still fits the bound takes it to twice
that: **31.7 MiB for 1 MiB of code, 32 bytes per code byte** (63.5 MiB at 2 MiB). Building it costs an
uploader four uploads of 260 KB and blocks that declare them in that order (each is valid). That
is the whole of the structural bound, so **a full engine holds under 32 MiB** at 1 MiB, twice the
16 MiB a thread per transaction held at most, where the build's table said 22 MiB at 2 MiB.

Two transients ride on top. While one function translates the engine holds more than it will keep:
up to 43 bytes per byte of it (10.8 MB for a 250 KB `br_table` function; about 22 MB for the 512 KiB
a transaction may declare), freed when the function is done. And at a step the code map is a new
block of twice the size: an allocator that copies holds the old one beside it (+15.9 MiB, 47.6 MiB
peak at 1 MiB: what the counting allocator's `realloc` models). That peak is the Mac's, not Linux's.
The worst fill run alone on Linux (OrbStack, aarch64, glibc 2.36, `/usr/bin/time -v`) reads
**19.9 MiB resident** after the small code and peaks at 26.7 MiB (max RSS 27,316 kB; at 2 MiB, 36 and
43 MiB): the doubled capacity is address space the vector has not written, and glibc remaps the old
block instead of copying it. (Element segments are written in full, so their 23 MiB is resident.) A
pass at its worst is therefore under 32 MiB allocated and about 22 MB more while its largest set
translates, and **two passes at once, the most a validator runs (next subsection), are under 64 MiB
allocated, about 105 MiB with both translating, under a twentieth of a 2 GB machine**, against 2 × 16
MiB with a thread per transaction.

Everything an engine holds is freed when it drops (the scratch programs checked that the
allocator's live bytes return to where they started). `the_memory_bound_rebuilds_the_engine` derives
its codes from `ENGINE_CODE_BYTES` (as many of the largest upload, 256 KiB, as fill it: four): the
four fill the engine exactly, the fifth gets a new one that holds none of the earlier four, with
the engine it replaces already gone, and declaring code the engine holds adds nothing; then three
codes and one of 256 KiB plus a byte, the bound plus one, which is a rebuild too.

#### How many passes run at once

Nothing in the node limits the number of passes (no semaphore, no limiter), and nothing was added.
What bounds it is where they run. A pass runs synchronously inside a task and blocks the runtime
worker thread that polls it until the pass thread is joined (`block_pass`, `pass.rs`). A validator's
consensus runtime is `commonware_runtime::tokio`, built from `cw_tokio::Config::new()` with only its
storage directory set (`node.rs:1192`, `:1196`); the default is **two worker threads**
(commonware-runtime 2026.9.0, `src/tokio/runtime.rs:197`, applied to tokio's multi-thread builder at
`:451-455`), and the consensus tasks are spawned onto it with the default execution, `runtime.spawn`
(`runtime.rs:709`, `utils/mod.rs:39-42`; marshal and simplex never ask for `dedicated()` or
`shared(true)`). So **a validator runs at most two passes at once**, one per worker. The bound is
incidental: a node configured with more workers would run that many.

Where a pass starts, and what serializes it (`parking_lot::Mutex` is the chain lock):
- **Proposing**: `App::propose` (`consensus.rs:678`), one task per round that the consensus engine
  asks for (marshal's `Inline::propose`, `inline.rs:274`). The replay of unexecuted ancestors (`:723`
  into `replay`, `:1078`, one `run_block` pass per ancestor, `:1101`) holds no chain lock. The
  packing pass (`:757`) holds the chain lock, taken at `:742`.
- **Verifying**: `App::verify` (`consensus.rs:863`), one task per block (marshal's `Inline::verify`,
  `inline.rs:459`). The engine can have verifications of several rounds in flight and the node does
  not serialize them. Its ancestor replay (`:967`) and apply loop (`:978`) hold no chain lock: it
  takes the lock for short reads and, on the pass thread, for `trial.begin()` (`:984`), which waits
  while a packing or execution pass holds it.
- **Executing**: `Executor::report` (`consensus.rs:1264`, `:1279`) calls `Chain::execute_shared`
  (`chain.rs:3256`, whose doc says "One executor at a time", `:3255`), whose `apply_block` (`:3295`, `run_block` `:3304`)
  runs under the chain lock taken at `:3258`.
- **Catching up over HTTP**: `sync_once` (`node.rs:1449`) into `execute_batch` (`:1812`) and
  `execute_shared`, in a runtime of its own (`node.rs:1132`) that ends before consensus starts, or
  on a full node (`node.rs:1350`), which has no consensus: the only pass there.
- **Startup**: `Chain::replay` (`chain.rs:3176`), before any runtime.

So packing and executing never overlap each other (one chain lock), while a verification can run
beside either, or beside another verification: the largest overlaps are two verifications, a
verification and a packing, a verification and an execution, never three. `/view` is no pass (a
thread and an engine of its own) and has its own limit, two at once
(`MAX_CONCURRENT_VIEWS`, `view_api.rs:94`).

"The marker has room for 16 trials" has nothing to do with this. `MARKER_BYTES` (`marker.rs:40`, doc
`:37-39`) is the size of the hint file, four kilobytes for sixteen records of at most 256 bytes, and
the file's own doc says it holds every trial in flight because proposals and verifications of
different rounds can overlap (`marker.rs:18-20`); a seventeenth would only be left out of the file. A pass names one
program transaction at a time, so the trials in flight are as many as the passes: two here.

### Numbers (2026-10-28)

The bench now takes `BLOCKULA_BENCH_PER_TX=1` to run its blocks the way they ran before A3 (a thread
and an engine per program transaction, `pass::per_transaction`), so one binary times both back to
back. The binary at `5a8aded4` (before any of this) was run beside it: the old path in the new
binary reads as the old binary does (below), so the comparison is the code's, not the build's. Four
rounds of base, per-transaction and pass, interleaved; each cell is the median of the four runs'
medians (the range in brackets), the minimal-call block of 512 calls, 17,101 counted fuel a call.

**An Apple M4 Max** (macOS 26.6, other builders running: load average 11 to 15 at the start of each
run):

| | block `run_block` | 512 native transfers | VM share | per call | ns per counted fuel (median, worst rep) |
|---|---|---|---|---|---|
| `5a8aded4` | 36.00 ms | 11.77 ms | 24.24 ms | 47.3 us | 2.77 (2.66 to 2.89), 2.97 |
| this tree, per transaction | 36.28 ms | 11.94 ms | 23.80 ms | 46.5 us | 2.72 (2.65 to 2.94), 3.46 |
| this tree, a pass | 20.57 ms | 11.77 ms | 8.80 ms | **17.2 us** | **1.00** (0.98 to 1.01), 1.27: GO |

**Linux** (OrbStack's VM, aarch64, kernel 7.0.14, glibc 2.36, Debian bookworm `rust:1.98.1`, the
container capped at `--cpus 4` (`cpu.max` 400000/100000); the VM's load average 1.9 to 2.9 at the start
of each run):

| | block `run_block` | 512 native transfers | VM share | per call | ns per counted fuel (median, worst rep) |
|---|---|---|---|---|---|
| `5a8aded4` | 55.12 ms | 11.35 ms | 43.83 ms | 85.6 us | 5.00 (4.78 to 5.42), 7.07 |
| this tree, per transaction | 54.72 ms | 11.32 ms | 43.45 ms | 84.9 us | 4.96 (4.60 to 5.11), 7.12 |
| this tree, a pass | 19.70 ms | 11.64 ms | 8.16 ms | **15.9 us** | **0.94** (0.92 to 0.98), 1.21: GO |

So the VM share falls to a third (Mac) and a fifth (Linux) of what it was, from 2.8 and 5.0 ns per
counted fuel to 1.0 and 0.9. This VM is not a floor runner (its native transfer is 22 us where the
floors take 36 to 39, and its thread 44 to 47 us where they take 71 to 99): it says what A3 removes,
not where the floors land (projection below).

The context rows of the same runs (per call; the old path's, then the pass's):

| per call, us | Mac, per transaction | Mac, a pass | Linux, per transaction | Linux, a pass |
|---|---|---|---|---|
| a thread with the 64 MiB stack, started and joined | 21.3 | not paid per call | 47.1 | not paid per call |
| a new engine / and the load of the smallest code | 4.7 / 6.5 | not paid per call | 3.6 / 5.8 | not paid per call |
| `Tx::id` of a call (hashed in `programs::apply`) | 5.2 | 5.3 | 5.2 | 5.3 |
| `programs::admit`, the program rules of admission | 0.75 | 0.75 | 0.6 | 0.6 |
| `econ::check_fee`, the fee with fuel | 0.4 | 0.45 | 0.3 | 0.3 |
| `named_by`, the accounts the action names | 0.7 | 0.65 | 0.6 | 0.6 |
| the call decoded for the VM (addresses, digest, bindings) | 1.4 | 1.4 | 1.3 | 1.3 |
| `Run::new`, the run's notes | 0.6 | 0.65 | 0.45 | 0.4 |
| the block's fuel count | 0.2 | 0.2 | 0.1 | 0.1 |
| `ProgramView::new`, the call's view | 0.6 | 0.6 | 0.55 | 0.6 |
| the executor on a warm engine (instantiation, the frame, `nop`, the checks) | 1.6 | 1.6 | 1.2 | 1.2 |
| the commit of its empty diff | 0.3 | 0.3 | 0.2 | 0.2 |
| the receipt (the facts noted, recorded) | 0.1 | 0.1 | 0.2 | 0.2 |
| **VM share of the block, per call** | **46.5** | **17.2** | **84.9** | **15.9** |
| left over: in none of the rows above | 6.9 | 5.2 | 21.4 | 5.1 |

### What was left, and what it was

Before A3 a minimal call on this Linux VM took 85 us of VM share: 47 for its thread, 6 for its engine
and the load, 5 for `Tx::id` and 27 for everything else (the executor, the view, the commit, the
receipt, the bookkeeping). After it, 16 us: 5 for `Tx::id`, 5.5 for those pieces timed alone, and 5
that no row names (the call's bookkeeping spread thin: the entry of `programs::apply`, the engine
slot, cache effects). The pieces timed alone cost the same on both paths (5.5 us); what no row names
fell from 21.4 to 5.1 us. **What fell by 16 us, from 27 to 10.6 of "everything else", was the
thread's side effects on the work that runs on it** (on the Mac it is 1.7 us: 13.6 to 11.9):
- **First-touch page faults.** A thread per call takes 6.0 minor page faults a call (3,073 in the
  block, from `/proc/self/stat` around `run_block`; the same block of native transfers takes 0, a
  pass 11 in all). Its fresh stack and its first allocations (a new thread starts its allocator's
  per-thread caches and its stack's pages cold) are mapped and touched anew each time.
- **Scheduling.** `perf stat` over the minimal-call bench alone, per-transaction against pass: 36,277
  against 5,787 page faults, 5,547 against 436 context switches (one a call: the caller sleeps in
  `join` while the thread runs and wakes when it ends, each way an IPI to an idle vCPU), 351 against
  42 CPU migrations (a call's thread starts on another core, with cold caches, about one time in
  sixteen).
- **The kernel's share.** `perf record` (`cpu-clock`) over the same runs puts 4.8% of the on-CPU
  samples in the kernel for the per-transaction path (173 ms) and 1.5% for the pass (52 ms): about 24
  us of kernel work a call that a pass doesn't do (`clone`, the stack's `mmap` and `munmap`, the faults,
  the futex and the scheduler).

So the hypotheses in the brief hold in part, and not where it guessed:
- **The fresh stack is not the main cost of the thread.** The stack-size rows (a thread started and
  joined, by `stack_size`) on this VM read 43 to 49 us at 64 MiB and 28 to 44 at 64 KiB to 16 MiB, a
  saving of 3 to 16 us a thread (it varied between runs) for a stack that glibc's 40 MiB cache can
  hold, which a 64 MiB one never is; on the Mac 21 against 15 us. The other 30 us or more of a
  thread is `clone`, the exit, the wake-ups and the scheduler, and no stack size changes those.
  **The floor runners say more** (run 40763852590, below): a thread of 64 MiB reads 85 to 88 us on
  x86-64 (one row of 117) and 67 to 73 us on aarch64, against 50 to 53 and 41 to 48 at 16 MiB or
  less: **25 to 35 us a thread** (35 to 37 on x86-64, 22 to 28 on aarch64), 35 to 40% of a thread,
  not the 3 to 16 of this VM. It matters only where a thread is still started per call: the
  fallback path (no pass thread) and `/view`, since A3 starts one a block. `PROGRAM_STACK_BYTES`
  stays as it is: a stack overflow aborts the node, so its size is the maintainer's call (the
  deepest scenario needs 40 KiB on x86_64 and 16 KiB on aarch64, `CALIBRATION-x86_64.md` section 7).
- **First-touch faults are real** (6 a call), but I did not time one alone: at the fraction of a
  microsecond to a microsecond a minor fault takes on a VM they are a few us of the 21 left over, and
  the cold caches, the wake-ups and the kernel work above are the rest (not separated further).
- **TLB shootdown on munmap** needs more vCPUs to show than this VM's 4-CPU quota gives; the floor
  runners' larger thread cost (71 to 99 us against 44 to 47 here) is where it would be.

What remains after A3 is the transaction's own work: `Tx::id`, hashed again in `programs::apply` for
the receipt's key (5.3 us: a block's own run doesn't have the ids; the proposer's trial and a
verification hand theirs on), the program rules of admission, the fee with fuel, the call decoded for
the VM (bech32 and hex), the view, the executor's instantiation and `nop` (1.2 us warm), the commit
and the receipt. A minimal call is now about 1.7 times a native transfer's cost on this VM (38 us
against 22).

### Projection for the floor runners

The floors measured before A3 (above): VM share 182 to 187 us a call on x86-64 and 193 to 197 on
aarch64, of which the thread 87.5 to 99.1 and 70.7 to 72.7, the engine and load 12.1 and 11.3,
`Tx::id` 6.6 and 8.1, and what's left 70 to 79 and 100 to 106. A3 pays one thread and one engine for
the whole block (0.2 us a call) and takes the thread's side effects with the thread. How much of
"what's left" those were on the floors can't be measured here, so four readings, from the least A3
can do to the most, each of the 17,101 counted fuel a call:

| reading | x86-64 | aarch64 |
|---|---|---|
| S0. only the thread, engine and load go, the rest unchanged | 71 to 87 us: 4.1 to 5.1 ns | 109 to 115 us: 6.4 to 6.7 ns |
| S1. the saving is 1.31 times thread + engine + load, as on the VM above (69 of 53 us) | 36 to 57 us: 2.1 to 3.3 ns | 83 to 90 us: 4.9 to 5.2 ns |
| S3. "what's left" falls by the VM's 61% (27 to 10.6 us), `Tx::id` stays | 34 to 38 us: **2.0 to 2.2 ns** | 47 to 50 us: **2.8 to 2.9 ns** |
| S2. the pass-path call costs what it does on the VM (15.9 us), scaled by the floors' native-transfer cost (38.7 and 35.6 against 22.2) | 28 us: 1.6 ns | 25 us: 1.5 ns |

S0 is a floor on A3's effect that the VM's numbers contradict (there, 16 of 27 us of "what's left"
went with the thread, and 27 us of VM remainder against the floors' 70 to 106 suggests more
thread-induced cost on the floors, not less). The floor re-run decided (next subsection), and S2 held:
the pass-path call measured on the VM (15.9 us), scaled by the floors' native-transfer cost (38.7
and 35.6 us in run 40742671981, against 22.2 on the VM), was the model that predicted it. S2 said 28 us (1.6 ns) on
x86-64 and 25 us (1.5 ns) on aarch64; the floors read 27.6 to 28.5 us (1.62 to 1.66 ns) and 25.7 to
25.9 us (1.50 to 1.51 ns), within 0 to 4%. S3, which this section took as its central reading
(34 to 38 us, 2.0 to 2.2 ns; 47 to 50 us, 2.8 to 2.9 ns, "0.35 over the line" on aarch64), was
16 to 27% (x86-64) and 45 to 49% (aarch64) above what the floors measured, and aarch64 came in 1.0
ns *under* the line, not over it. S1
(36 to 57 and 83 to 90 us) and S0 (71 to 87 and 109 to 115 us) were 1.3 to 3.5 and 2.5 to 4.5
times what the floors measured: the cost the thread's side effects had on the work that ran on it
went with the thread, on the floors as on the VM. The range above, 1.6 to 3.3 and 1.5 to 5.2, held
the result at its lower edge, which made it honest as a range and wrong as a forecast. The advice
that followed it, to raise `LOAD_BASE` or cap the block's program transactions if aarch64 stayed
over, is moot.

### The floor runners after A3 (run 40763852590)

`calibrate-vm.yml`'s `node-benches` job, profile `full`, on the calibration branch
`automation/programs-v0-calibrate-a3` (6b0243cc), whose binary is this stage's commit before its
review fixes (2059f21c). Each of the three runs times the block passes (A3) and then the path
before it (`BLOCKULA_BENCH_PER_TX=1`: a thread and an engine per program transaction), back to
back. The minimal calls are the 512-call block of the first table of this file; the per-fuel figure is
the VM's share (the block less 512 native transfers), 17,101 counted fuel a call.

**x86-64 (AMD EPYC 7763, 4 vCPU)**, one run after another, the block passes and then the path before A3, each pair back to back (load averages at the start of each, 1, 5 and 15 minutes):

| run | path | load | block work per call | a native transfer | VM share per call | ns per counted fuel, judged (median, worst of 5) | `persist` | minimal calls |
|---|---|---|---|---|---|---|---|---|
| 1 | a pass | 5.10 3.83 1.93 | 64.8 µs | 37.2 µs | **27.6 µs** | **1.62** (1.67) | 21.8 ms | GO |
| 1 | a thread and an engine per transaction | 4.19 3.69 1.92 | 215.8 µs | 37.6 µs | **178.2 µs** | **10.42** (10.59) | 18.3 ms | NO-GO |
| 2 | a pass | 3.70 3.60 1.91 | 65.3 µs | 37.5 µs | **27.8 µs** | **1.62** (1.78) | 16.2 ms | GO |
| 2 | a thread and an engine per transaction | 3.29 3.51 1.90 | 218.6 µs | 37.5 µs | **181.1 µs** | **10.59** (10.88) | 17.3 ms | NO-GO |
| 3 | a pass | 2.93 3.43 1.89 | 65.9 µs | 37.4 µs | **28.5 µs** | **1.66** (1.78) | 16.7 ms | GO |
| 3 | a thread and an engine per transaction | 2.64 3.35 1.88 | 221.8 µs | 37.7 µs | **184.1 µs** | **10.77** (11.80) | 18.0 ms | NO-GO |

**aarch64 (4 vCPU)**, one run after another, the block passes and then the path before A3, each pair back to back (load averages at the start of each, 1, 5 and 15 minutes):

| run | path | load | block work per call | a native transfer | VM share per call | ns per counted fuel, judged (median, worst of 5) | `persist` | minimal calls |
|---|---|---|---|---|---|---|---|---|
| 1 | a pass | 4.95 3.33 1.56 | 63.3 µs | 37.4 µs | **25.9 µs** | **1.51** (1.54) | 18.6 ms | GO |
| 1 | a thread and an engine per transaction | 4.34 3.25 1.55 | 225.1 µs | 37.3 µs | **187.8 µs** | **10.98** (11.79) | 21.9 ms | NO-GO |
| 2 | a pass | 3.83 3.18 1.55 | 63.1 µs | 37.4 µs | **25.7 µs** | **1.51** (1.57) | 18.3 ms | GO |
| 2 | a thread and an engine per transaction | 3.39 3.10 1.54 | 243.3 µs | 37.4 µs | **205.8 µs** | **12.04** (12.21) | 19.5 ms | NO-GO |
| 3 | a pass | 3.02 3.03 1.54 | 63.0 µs | 37.4 µs | **25.7 µs** | **1.50** (1.56) | 17.6 ms | GO |
| 3 | a thread and an engine per transaction | 2.71 2.97 1.53 | 233.0 µs | 37.3 µs | **195.7 µs** | **11.44** (11.71) | 14.1 ms | NO-GO |


In every one of the six pass runs **every pattern is GO**, on both runners: the minimal calls at 1.62
to 1.66 ns per counted fuel on x86-64 (the worst repetition anywhere, 1.78) and 1.50 to 1.51 on
aarch64 (worst 1.57), against the 2.5 line; the write storms (medians) at 1.46 to 1.49 and 0.42 to
0.43 (x86-64), 1.33 to 1.34 and 0.59 to 0.66 (aarch64). The per-transaction rows of the same runs read
as the binary before A3 did on run 40742671981 (10.6 to 10.95 and 11.3 to 11.5): 10.4 to 10.8 and
11.0 to 12.0, NO-GO each time, so the saving is the code's and not the runner's. A3 took 150 to 156
us a call out of the VM's share on x86-64 and 162 to 180 on aarch64, 85% and 86 to 88% of it.

Per call, timed alone where a row says so (the ranges are over the six runs, a pass and a thread
per transaction alike, except where the path is named):

| | x86-64 | aarch64 |
|---|---|---|
| VM share, a pass / a thread and an engine per transaction | 27.6 to 28.5 / 178.2 to 184.1 | 25.7 to 25.9 / 187.8 to 205.8 |
| `Tx::id` | 6.7 | 8.1 to 8.2 |
| the ten pieces below, together | 11.1 to 11.7 | 9.5 to 9.8 |
| admission (`programs::admit`) / the fee with fuel / the accounts named | 1.2 to 1.3 / 0.6 / 1.0 to 1.2 | 1.1 to 1.2 / 0.6 to 0.7 / 0.9 |
| the call decoded for the VM / `Run::new` / the block's fuel count | 2.2 / 0.9 to 1.0 / 0.3 | 1.9 / 0.8 to 0.9 / 0.3 |
| the call's view / the executor on a warm engine / the commit of an empty diff / the receipt | 1.0 / 2.8 to 3.2 / 0.4 to 0.5 / 0.5 to 0.6 | 1.0 / 2.0 to 2.2 / 0.5 / 0.4 to 0.5 |
| **in no row, a pass** (the share less `Tx::id` and the ten pieces) | **9.4 to 10.6** | **7.8 to 8.2** |
| a thread with the 64 MiB stack, started and joined / with 16 MiB or less | 85 to 88 (one row 117) / 50 to 53 | 67 to 73 / 41 to 48 |
| a new engine / and the load of the smallest code | 7.2 to 7.6 / 12.0 to 12.4 | 7.6 to 7.8 / 11.1 to 11.3 |
| in no row, a thread and an engine per transaction (also less the thread, the engine and the load) | 61 to 68 | 88 to 106 |
| minor page faults in `run_block`, per transaction path / a pass's whole block | 5.0 a call / 10 | 6.0 a call / 11 |

What was left on the floors after A3 is the call's own work: `Tx::id` (6.7 and 8.1 us, a quarter to a
third of the share), the ten pieces (11 and 9.7), and **8 to 10 us in no row**, about a third of the
share, as on the VM (5.1 of 15.9). The rest of what a thread per call cost, 61 to 68 and 88 to 106
us in no row, went with the thread: that is the first-touch faults, wake-ups and kernel work of the section
above, and they are larger on the floors than the 21 us they were on the VM. The stack-size rows
are in "What was left, and what it was".

