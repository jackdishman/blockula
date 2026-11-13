# Randomness for programs

How a program gets a random number it can settle a bet, a raffle or a deal on, what that number
is worth, and what it isn't. For program authors. Programs, never contracts.

> **Status.** Three host functions (`blake3_v1`, `epoch_v1`, `beacon_v1`) join host ABI v1 with
> **protocol v3**, the preview version (`upgrade::PREVIEW_PROTOCOL_VERSION`): a dev or test chain
> whose genesis schedules v3 has them, and no other chain does. Under v1 and v2 nothing changes:
> code that imports one of them is refused at upload, as any import the ABI lacks is. Programs
> themselves are experimental and off by default ([PROGRAMS-V0.md](PROGRAMS-V0.md)). The design
> this follows is [PROGRAMS.md](PROGRAMS.md) 10.3. [Section 8](#8-proposal-fast-per-block-randomness)
> is a proposal, not something the node does.
>
> **v3 is also the audit-fix version.** Protocol v3 was reserved for the fixes an audit of
> programs asks for (`upgrade::MAINNET_PROGRAMS_ACTIVATION_VERSION`, [PROGRAMS-V0.md](PROGRAMS-V0.md)
> §1.7), and it now carries the randomness ABI as well. So an audit of programs under v3 has
> these three host functions, their gate at upload and their fuel prices in its scope
> ([AUDIT_SCOPE.md](AUDIT_SCOPE.md), "Programs"), and a release that ships v3 ships both or
> moves one of them to a later version. The node asserts at compile time that the VM's
> `abi::RANDOMNESS_VERSION` is the preview version, and `abi::FIRST_VERSION` the programs
> version (`node/src/upgrade.rs`), so the numbers can't drift apart unnoticed.

## 1. The short version

- The chain has one random value per epoch: the validators' **beacon**
  (`node/src/beacon.rs`). Every validator commits to a secret for epoch `e + 2` during epoch `e`,
  reveals it during `e + 1`, and at the end of `e + 1` the value for `e + 2` is BLAKE3 over the
  previous value, the epoch and the revealed secrets.
- A value a program can read is **public**. So a program never reads the beacon and acts on it in
  the same call. It takes the bet first, names a beacon value that doesn't exist yet, and settles
  in a later transaction.
- The helper for that is `abi::random` (`node/vm/guests/abi/src/random.rs`): `request()` when the
  bet is placed, `settle()` two epochs later.
- It is fine for things worth less than a withholding validator stands to lose (section 5). It is
  not for high-value draws.

## 2. The host functions

All three are imports from module `blockula`, in `node/vm/src/abi.rs`, importable from protocol
v3.

| Function | What it does | Fuel |
|---|---|---:|
| `blake3_v1(ptr, len, out32)` | Writes the BLAKE3 hash of `len` bytes at `ptr` to `out32`. `len` may be 0, and at most a program's whole memory (2 MiB); a longer `len` traps (`Length`), a range outside memory traps (`MemoryAccess`) | 100 + 200 + 2 per byte |
| `epoch_v1() -> i64` | The executing block's epoch (`height / epoch_blocks`) | 100 + 200 |
| `beacon_v1(epoch: i64, out32) -> i32` | If `epoch`'s beacon is derived and the state still keeps it: writes its 32 bytes to `out32` and returns how many validators revealed for it (0 or more). Otherwise returns −1 and writes nothing | 100 + 1,000 |

`beacon_v1` returns −1:

- **before the value is derived.** The newest value there is belongs to the executing block's own
  epoch. The next epoch's is derived when this epoch's last block ends, and is readable from the
  block after;
- **after it's pruned, forever.** The state keeps the last 64 epochs of values
  (`beacon::VALUES_KEPT`): about 64 hours with hour-long epochs, about an hour with minute-long
  ones, and about 10.7 hours on the project's shared chain (epochs of 300 blocks at 2 s, so
  10 minutes each). The window is part of this function. If the chain ever keeps values longer or shorter,
  that comes as a new function, and `beacon_v1` keeps answering for 64;
- for a negative `epoch`.

A return of **0 is not an error**: the value is there, but no validator revealed for it, so it is
the hash-chain fallback, which anyone could compute ahead (section 5.3). A program should treat a
low count as "no randomness" and never as a result.

The first 100 of each price is the base every host call pays. `blake3_v1`'s 2 per byte is twice
[PROGRAMS.md](PROGRAMS.md) 4.5's figure: on short inputs BLAKE3 runs one block at a time, the slowest
it gets. A local run of the calibration bench (`cargo bench --bench calibrate -- --only host` in
`node/vm`, on an Apple M-series laptop) measured at most 0.36 ns per fuel for `blake3_v1` and 0.04
for `beacon_v1`, against the 2.5 ns budget. The floor machines haven't run these cases yet.

The answers come from the chain's state at the executing block (`View::epoch`, `View::beacon` in
`node/vm/src/host.rs`; the node's `ProgramView`), so every node computes the same ones, and every
transaction in a block reads the same ones: values are filed and pruned only when a block ends.
A `view` method may call all three.

## 3. The safe pattern

**Commit before you know. Settle later.**

```text
epoch e      roll      the program fixes the player, the stake and the pick, and makes a
                       request for the beacon of epoch e + 2
epoch e + 1            validators reveal their secrets for e + 2 (second half of the epoch)
epoch e + 2  settle    the value is on-chain: anyone settles the roll on it, and the program
                       pays whoever does
…
epoch e + 34           the deadline (e + 2 + 32): the value is certainly still kept until here
…
epoch e + 65           the last epoch the chain keeps the value in (e + 2 + 63): the roll
                       still settles on it, as it would have in epoch e + 2
epoch e + 66           the value is gone. A roll nobody settled is closed by the rule the
                       program fixed before the roll: forfeit, never a refund
```

Why two epochs: during epoch `e` the validators are still committing for `e + 2`, and none of
them has revealed anything. A request for `e + 1` would be too late: its reveals are already
landing during `e`.

### The helper

`abi::random`, for programs built like the guests in `node/vm/guests` (no allocator):

```rust
use abi::random::{self, Outcome, Request};

// In the transaction that takes the bet. Fix everything the value will decide first.
let request: Request = random::request(min_reveals);
put(&key, &record_with(request.to_bytes()));          // 28 bytes

// In a later transaction.
match random::settle(&request, b"dice/roll") {
    Outcome::Settled(value) => pay_out(1 + random::below(&value, 6)),
    Outcome::Pending        => abort(NOT_YET, b"not yet"),   // change nothing
    Outcome::TooFewReveals  => refund(),                     // no randomness: no result
    Outcome::Expired        => forfeit(),                    // never a refund (rule 3)
}
```

- `request(min_reveals) -> Request { id, target_epoch, deadline_epoch, min_reveals }`.
  `target_epoch` is the current epoch + 2, `deadline_epoch` is `target_epoch` + 32. `id` comes
  from a counter the module keeps in the program's own storage (key `"\0random/next"`), so two
  requests never share one. That is one small storage write: the signer bonds its deposit the
  first time.
- `settle(&request, domain) -> Outcome`:
  - `Settled(value)`: `value` = BLAKE3(`domain` ‖ the program's address ‖ `id` (8 bytes, LE) ‖
    the beacon value). The same on every call for as long as the chain keeps the target's
    value: its own epoch and the 63 after it (`KEPT_EPOCHS`), before the deadline and after it.
    `domain` (at most 64 bytes) says what the value is for, so one request can feed two draws
    that don't repeat each other;
  - `Pending`: the target epoch's value isn't derived yet;
  - `TooFewReveals`: the value is there, with fewer than `min_reveals` reveals;
  - `Expired`: the current epoch is past `deadline_epoch` **and** the chain no longer keeps the
    value. Nobody can tell any more what the outcome was, or whether it had enough reveals. A
    value that is still kept past the deadline settles as usual: when a request stops being
    settleable depends on the chain's state alone, never on who looks or when they choose to.
- `settle` only reads. The program removes or marks the request once it applied a final outcome,
  so it's applied once.
- `below(&value, n)` is the value's first 8 bytes modulo `n`. The bias is under `n / 2^64`.

`dice` (`node/vm/guests/dice`) is the worked example. `roll` (payable) records a request.
`settle`, which anyone may send:

| The roll's outcome | The player | Whoever sent the `settle` | The bank |
|---|---|---|---|
| a hit | six stakes | the bounty, from the bank | pays both |
| a miss | nothing | the bounty, out of the stake | keeps the rest of the stake |
| `TooFewReveals` | the whole stake back | nothing | unchanged |
| `Expired` (nobody settled in 64 epochs) | nothing: the stake is forfeit | the bounty, out of the stake | keeps the rest of the stake |

The bounty is a hundredth of the stake, rounded down, fixed with the roll. The bank counts it
in what it must hold for every open roll, so a hit's settler is always paid. The odds are even
(one face in six pays six stakes), so a hit's bounty is the bank's loss, a hundredth of a stake
per roll on average: a real game would price it in. A stake under 100 drops carries no bounty.

### The rules

1. **Everything the value decides is fixed in the request's transaction.** The player, the stake,
   the pick, the odds. Nothing that can change later may decide who wins.
2. **The rule for a request that can't be decided is fixed then too:** what happens on `Expired`
   and on `TooFewReveals`, written in the code, never chosen at settlement.
3. **`Expired` forfeits. It never refunds.** A request's value is public from its target epoch.
   If letting a request expire gave the stake back, a requester would settle the ones that won
   and wait out the ones that lost: hits paid, misses returned. That is true even when anyone
   may settle, because nobody has a reason to settle a stranger's loss for nothing. So a request
   whose value was usable and that nobody settled while the chain kept it costs the requester
   what a loss would.
4. **Let anyone settle, and pay whoever does.** A small bounty fixed with the request (`dice`: a
   hundredth of the stake, out of the stake on a miss and a forfeit, from the bank on a hit)
   gives third parties a reason to settle losses, so a request expires only if nobody at all
   came in 64 epochs. If only the requester could settle, every loss would end `Expired`.
5. **`TooFewReveals` may refund.** The value wasn't randomness, so there is no result to dodge,
   and the reveal count is on the chain for everyone. `dice` gives the whole stake back and
   pays no bounty for it (the player has the reason to send that `settle`). Such a request
   still ends `Expired` if it is left until the value is gone. What a refund gives validators
   who withhold is in section 5.2.
6. **`min_reveals` is the program's, never the caller's.** A caller who could pass 0 would bet on
   the hash-chain fallback.
7. **A `Pending` settle changes nothing.** Abort, so the transaction leaves no trace but its fee.
8. **Never use `tx_hash`, `block_height`, `epoch`, a storage counter, the caller's address or the
   order of transactions as randomness.** The sender or the block's leader picks or predicts every
   one of them.
9. **Never read the beacon and act on it in one call.** A caller would look first and call only
   when it wins.

## 4. Latency, retention and deadlines

- **Latency: more than one epoch, at most two.** A request made in epoch `e` settles from the
  first block of `e + 2`. With hour-long epochs that is one to two hours; with a dev chain's
  minute-long epochs, one to two minutes; on the project's shared chain (epochs of 300 blocks
  at 2 s: 10 minutes) a request settles 10 to 20 minutes after it is made. There is nothing
  faster today (section 8 proposes it).
- **Retention: 64 epochs**, the value's own and the 63 after it. After that the value is gone
  and `beacon_v1` answers −1. On the shared chain that is about 10.7 hours (64 × 10 minutes).
- **The deadline: 32 epochs after the target**, half the retention window (5.3 hours on the
  shared chain). It tells "not yet" from "gone": through the deadline a missing value isn't
  derived yet (`Pending`); past it, a missing value was dropped (`Expired`). A request settles
  normally all the way to the end of the retention window, past the deadline too, so a program
  can tell its users "settle within 64 epochs" and promise the first 32 whatever a later
  version does to retention.

## 5. What the value is worth

### 5.1 The last revealer chooses between two values

A validator that sees everyone else's reveals can decide not to send its own. The value is then
derived without it. So it chooses between two values: the one with its secret and the one
without. That is one bit of influence.

- **k validators working together choose among 2^k values.** A die has six faces: three
  colluding validators can usually pick a winning one.
- **One operator with two validators chooses among four.** On the project's shared chain the
  project itself runs 2 of the 7 validators. Alone, it can reveal both secrets, either one or
  neither: four values. The chance that at least one of four shows a chosen die face is
  1 − (5/6)^4, about 52%, against 17% for an honest roll. Every withheld reveal is on the chain
  (`missed`, a slash, a jail), so it can't be done quietly or often, but it can be done. On that
  chain, treat the beacon as fair only as far as you trust the project not to do that, and
  only for things that aren't worth those slashes.
- **All committers together know the value when they commit**, two epochs ahead. Randomness from
  the beacon assumes at least one validator that reveals is honest.

What a withheld reveal costs the validator (`beacon::end_epoch`):

- a slash of `beacon_miss_slash_bps` of its pool, times one more for each miss it already has in
  the kept values, up to the parameter's ceiling;
- jail, from the block the value is derived in, so it earns nothing for that epoch;
- its seat in the epoch it biased: the set for it is chosen after the jail;
- and a jailed validator can't commit to another round.

**So: fine for things worth less than a withholding validator stands to lose. Not for high-value
draws.** A program can't raise that bound. It can only keep what one request decides small. FANG
has no value; the bound is about whatever a chain's own users care about.

### 5.2 `min_reveals`, and what a refund gives a withholder

`min_reveals` says how many reveals a value needs before the program uses it. A value is
unpredictable at request time if at least one of its revealers was honest, so set it **above the
number of validators you assume could collude**. In a set of `n` that tolerates `f` faulty
validators (`f` under a third of `n`), `f + 1` is the least that helps.

Two limits:

- A program can't read the size of the validator set. `min_reveals` is a number fixed when the
  program is made. If the set shrinks below it, every request ends `TooFewReveals`.
- If `TooFewReveals` refunds (as in `dice`), validators who can push the count below `min_reveals` hold a veto:
  they can turn a result they dislike into a refund, at the price of section 5.1's slashes. The
  higher `min_reveals`, the fewer withholders that takes. `f + 1` needs `n − f` of them, which
  `f` colluders don't have.

### 5.3 Liveness, and the hash-chain fallback

The beacon never stalls. If some validators don't reveal, the value is derived from the rest. If
nobody committed, the value is BLAKE3 over the previous value and the epoch: a **hash chain that
anyone can compute ahead**. `beacon_v1` then returns 0. Epoch 1 of every chain is like that (it has
no round), and so is any chain whose validators don't take part.

A program that accepted such a value would hand the result to whoever computes it first. That is
what `min_reveals` is for: `settle` refuses with `TooFewReveals`.

### 5.4 Other things to know

- **The value is public the moment it's derived.** Anyone can compute a request's outcome before
  it settles. That's fine: nothing can be changed any more. It is why rule 1 matters.
- **A leader can leave a settle out of its block.** It can't change the outcome, and the next
  leader includes it. With `dice`'s rules, 64 epochs of leaders would have to agree, each turning down a bounty.
- **The hash chain binds epochs together.** Each value includes the one before, so a withheld
  reveal also changes every later value. That gives a withholder nothing more: it can't see
  later reveals.
- **`blake3_v1` is a hash, not a secret.** A program has no secrets: its code and storage are
  public.

## 6. Trying it on a local chain

Programs with these imports upload only on a chain running protocol v3. That means a dev or test
chain whose genesis schedules it: after `blockula summon <name> --programs` (or
`blockula dev --programs`), and before the chain's first start, set `upgrades` in every home's
`genesis.json` to `[{ "version": 3, "activation_height": 1 }]`. A preview version's rules can
change from release to release, so such a chain is for testing and may need recreating
([PROGRAMS-V0.md](PROGRAMS-V0.md), section 9.2).

Then, as for any program ([BUILD-ON-IT.md](BUILD-ON-IT.md)):

```bash
blockula program upload node/vm/tests/programs/dice.wasm --interface node/vm/tests/programs/dice.iface.json --yes
blockula program create <wasm_hash> <interface_hash> --init init --input 01000000 --storage-limit 1 --yes   # min_reveals = 1
blockula program call <address> fund --value 60 --yes
blockula program call <address> roll --input 03 --value 1 --storage-limit 1 --yes   # pick 3
# two epochs later
blockula program call <address> settle --input 0000000000000000                     # roll 0
```

`scripts/randomness-e2e.sh` does all of this on four validators it summons into a temporary
directory: a normal roll settled by a third party for the bounty, a roll whose epoch one
validator can't reveal for, a roll settled past its deadline, and a roll left until its value
is gone (forfeit).

A one-validator dev chain has one reveal per epoch from epoch 3 on, so `min_reveals` of 1 is the
most that settles there.

## 7. Where it's tested

| What | Where |
|---|---|
| The gate: refused before v3, accepted from it; v2 code unchanged | `node/vm/src/upload.rs` (`an_import_is_refused_until_its_protocol_version`), `node/vm/tests/randomness.rs`, `node/src/programs/exec/tests/randomness.rs` (on a chain that activates v3 at a height) |
| `beacon_v1`: −1 before derivation, −1 after pruning, reveal counts, the same on every engine and every run | `node/vm/tests/randomness.rs`; against the real beacon's filing and pruning, `the_view_reads_the_beacon_from_derivation_until_it_is_pruned` |
| `blake3_v1`, `epoch_v1`: results, bounds, fuel | `node/vm/tests/randomness.rs` |
| `dice` and `abi::random` | `node/vm/tests/randomness.rs` (in memory), `node/src/programs/exec/tests/randomness.rs` (one validator) |
| No free option: a loser who waits past the deadline gets no refund (a miss while the value is kept, a forfeit after) | `a_loser_who_waits_past_the_deadline_gets_no_refund`; on a chain, `the_dice_settles_refunds_and_forfeits_on_the_chains_beacon` |
| A third party who settles is paid the bounty (from the stake on a miss, the bank on a hit) | `a_third_party_settling_earns_the_bounty` |
| Too few reveals still refund the whole stake | `too_few_reveals_refund_the_stake` |
| Four validators: a normal settle, a withheld reveal (jailed, three reveals), no reveals (refund), a settle past the deadline, a forfeit once the value is gone, the same blocks, roots and receipts on every node | `node/src/sim/programs.rs`, `the_dice_settles_identically_on_four_validators` |
| Four real nodes on local ports, driven with `blockula program`: a settle on 4 reveals with its bounty, a settle on 3 with the withholder jailed, a settle past the deadline, a forfeit | `scripts/randomness-e2e.sh` (about 12 minutes; not in CI) |
| v1 history replays unchanged | `node/testdata/replay/v1-*` ([REPLAY.md](REPLAY.md)) |
| v2 history replays unchanged, the refusal of code importing `beacon_v1` included | `node/testdata/replay/v2-programs`, `the_v2_corpus_replays` |
| The version numbers can't drift | `node/src/upgrade.rs`: compile-time asserts on `abi::FIRST_VERSION` and `abi::RANDOMNESS_VERSION` |
| Fuzzing | `node/vm/fuzz` deploys `dice` and takes the height and the reveal count from its input |

## 8. Proposal: fast per-block randomness

> **A proposal. Nothing in this section is implemented, and no consensus code exists for it.**
> It is here to be reviewed before anyone builds it.

### 8.1 Why

Two epochs is a long wait for a card game. The epoch beacon can't go faster: its rounds are an
epoch long so that every validator has time to commit and reveal. A value per **block** would let
a program settle a few seconds after the request.

### 8.2 The construction

**Each validator commits to a hash chain in advance. Each block's leader reveals its next link.**

- **Setup.** A validator picks a secret seed `s_N` and computes `s_i = BLAKE3(s_{i+1})` down to
  `s_0`. It publishes `s_0`, the **anchor**, in a transaction (`RandomnessAnchor { anchor, length }`),
  signed by its consensus key like its other duties. `N` is a few million: enough links for months
  of its leader slots. The anchor takes effect at the start of the epoch after next, so a validator
  can't choose it knowing which slots it will lead (the leader order of an epoch comes from that
  epoch's beacon value, which isn't known yet).
- **Each block.** The leader of height `h` puts its next unrevealed link `link_h` in the block
  header. The chain keeps, per validator, the last link it revealed (`last`, starting at the
  anchor).
- **The value.**

  ```text
  value_h = BLAKE3("blockula block random v1" ‖ value_{h−1} ‖ h (8 bytes, LE) ‖ link_h)
  ```

  `value_0` is the genesis beacon value.

### 8.3 Validation rules

A block at height `h` from leader `L` is valid only if:

1. `BLAKE3(link_h) == last[L]`. One hash. Then `last[L] = link_h`.
2. `L` has an anchor in force. A validator without one can't lead: it is skipped as if it had
   timed out. (A new validator publishes its anchor when it registers; it joins the set an epoch
   later anyway.)
3. `value_h` in the header equals the formula above.
4. A chain that runs out gets a new anchor the same way, an epoch ahead. A validator whose chain
   runs out without one stops leading until it has one.

The link is **forced**: there is exactly one preimage the leader can reveal. It can't grind it.
That is the whole point of committing to the chain in advance.

A validator must never reveal a link twice in two different blocks at one height: that is
equivocation, which already costs it its stake. It must also persist how far down its chain it is
before it proposes, as it persists its beacon secrets today.

### 8.4 When a leader times out or abstains

Blockula's consensus is Simplex: a view whose leader doesn't propose in time is nullified, and the
next view's leader proposes at the same height.

- **A timeout reveals nothing.** The height's value then comes from the next leader's link. The
  timed-out leader's chain doesn't advance.
- **So a leader has one bit.** It knows `value_{h−1}` and its own link, so it knows what `value_h`
  would be. It can propose, or stay silent and let the next leader's link decide. Two outcomes:
  the same bias as the epoch beacon's last revealer, per block.
- **What that costs it:** its slot. No block, so no tip, and a missed leader slot counts toward
  downtime (`blockula_missed_leader_slots_total`, and jail past the limit). A proposal could add a
  small slash for a missed slot at a height a program settled on; that needs care, because honest
  nodes miss slots too.
- **k leaders in a row** that collude choose among `k + 1` values (which of them proposes). Leader
  order is shuffled per epoch by the beacon, so a run of colluders is luck, not a plan.
- **The next leader can't exploit a timeout.** Its link is forced too. Its only choice is the same
  one: propose or not.

### 8.5 What programs would get

```text
block_random_v1(height: i64, out32) -> i32      1 written | −1 not available
```

- **Only for heights after the request, plus a delay.** A program asks at height `h` and names
  `h + d`. `d` must be large enough that the leader of `h + d` isn't the one deciding whether to
  include the request, and that finality has passed. A first figure: `d ≥ 4` blocks, and a helper
  that refuses less.
- **−1 until that block is final**, and −1 once pruned. Retention would be counted in blocks (a
  day, say) and frozen with the function, as `beacon_v1`'s is.
- The same `request` / `settle` helper, with a height in place of an epoch, and the same rules:
  commit first, settle later, a deadline, a rule fixed in advance.
- **Mixing both** is stronger than either: `BLAKE3(block value ‖ epoch beacon value)` for a
  request that can wait. For a card game, the block value alone is the trade.

It is worth one bit per leader slot. The bound of section 5.1 applies, with "a slot" in place of
"a slash and a seat": fine for small stakes, settled fast.

### 8.6 Why not the usual tools

- **BLS threshold signatures** (drand, Dfinity) give an unbiasable, unpredictable value per round:
  nobody can withhold. They rest on pairings over elliptic curves, which a quantum computer
  breaks. Blockula's signatures are ML-DSA-65 because the chain is meant to stay sound after that,
  and a randomness beacon that isn't would be the weakest part. A threshold scheme also needs a
  distributed key generation at every change of the validator set.
- **ECVRF** (Algorand, Cardano) lets a leader prove a value is the only one its key could produce.
  It is elliptic-curve too: the same quantum break. Post-quantum VRFs exist on paper (lattice and
  hash-based), with proofs of tens to hundreds of kilobytes or no mature implementations. Not yet.
- **ML-DSA-65 signatures can't stand in for a VRF.** A VRF needs **uniqueness**: one valid output
  per key and input. ML-DSA signing is hedged: it mixes fresh randomness into every signature
  (FIPS 204's default), and even its deterministic mode uses rejection sampling, where a signer
  who ignores the standard can pick among many valid signatures. A verifier accepts all of them.
  So a leader could sign the same message again and again until the hash of the signature suits
  it: it **grinds**. `BLAKE3(signature)` is therefore chosen by the signer, not random.
- **A hash chain is unique by construction.** There is one preimage of `last[L]` that the leader
  committed to. It uses only BLAKE3, which the chain already relies on, and which a quantum
  computer weakens only by the generic square-root speedup (256-bit outputs leave 128 bits).
- **A VDF** would remove the withholding bit (nobody can compute the outcome in time to decide).
  It needs special hardware assumptions and a verifier in consensus; the known ones rest on
  groups of unknown order, which aren't post-quantum either.

### 8.7 Activation

Behind a protocol version, like everything that changes what a block is:

1. **Version N: anchors.** The `RandomnessAnchor` action and its state (one record per
   validator). Nodes publish an anchor as a duty. Nothing reads them yet. Old binaries halt at
   `N`'s activation height, as for any upgrade.
2. **Version N + 1: headers.** Blocks carry `link` and `value`, validated as in 8.3, from an
   activation height at which every active validator has an anchor in force (the upgrade
   proposal names the height; a leader without an anchor is skipped). The block header and its
   hash change, so this is the version light clients, the SDK's proofs and the explorers must
   follow.
3. **Version N + 2: programs.** `block_random_v1` joins the host ABI, gated at upload as
   `beacon_v1` is today. The guest helper and a new calibration case come with it.

Each step is replayed against recorded history first ([REPLAY.md](REPLAY.md)): a v1, v2 or v3
block must end as it always has.

**Open points for the design review:** the delay `d`; whether a missed slot at a requested
height should be slashed; how a validator recovers a lost chain position (a new anchor an epoch
ahead, and no leading until then, is the safe answer); the cost of 32 more bytes and one hash
per header; and whether the leader's link should also feed the epoch beacon.
