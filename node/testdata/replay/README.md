# The recorded corpus: v1, and a short v2 history

Every PR's binary replays these recordings (`blockula replay`,
[docs/REPLAY.md](../../../docs/REPLAY.md)) and must reproduce them block by block: every
certificate, every check `consensus::verify` makes without a round or the clock (the cited state
root among them), every tx's outcome, every block's state root and record, and the outcomes and
roots again through `Chain::open`'s startup replay. They're the v1 replay gate's fixed history
([docs/PROGRAMS-PLAN.md](../../../docs/PROGRAMS-PLAN.md) P2).

Each directory is an archive in `scripts/archive-chain.mjs`'s format, its closed files kept as
text (git compresses them anyway):
- **`meta.json`:** the chain, and each file's heights and SHA-256.
- **`genesis.json`.**
- **`sync-000000000001.jsonl`:** the finalized consensus blocks, in `/api/sync` batches, each
  with a finalization certificate.
- **`blocks-000000000001.jsonl`:** the recording node's block log (`meta.json` `blocks`), what
  each block did.

`node/src/replay/corpus.rs` recorded them, deterministically: recording again gives the same
bytes. The v0.5.2 release source replays them with the same result.

- **`v1-open`:** 1,999 blocks, like testnet-2 (no Season 0 admins).
- **`v1-season0`:** 200 blocks, like `blockula dev` (its validator is the admin).

- **`v2-programs`:** 40 blocks of a two-validator chain born at protocol v2 with programs on
  (below). Not part of the v1 gate: it holds v2's program rules the same way.

Don't edit or re-record them to make a replay pass: a difference means the binary changed what a
v1 block does (or, for `v2-programs`, a v2 block). Adding coverage:
[docs/REPLAY.md](../../../docs/REPLAY.md) "Changing the corpus".

## `v2-programs`

Recorded by the v1.0.9 source (`c34ac95`) with `record_programs` of `node/src/replay/corpus.rs`
(`cargo test --release -- --ignored write_v2_replay_corpus`); every later binary must replay it
and record the same bytes ([docs/REPLAY.md](../../../docs/REPLAY.md#the-v2-recording)). Its block
log holds each program tx's receipt.

| Tag | Action | applied/failed/skipped | What |
|---|---|---|---|
| 0 | `transfer` | 7/0/0 | between the program txs |
| 62 | `upload_code` | 4/3/0 | the four guests; failed: code importing `beacon_v1` (rule 5, `Import`: a host function v2 doesn't have), in two epochs, and a float (rule 4) |
| 63 | `create_program` | 4/0/0 | the four guests, the token with its `init` |
| 64 | `call_program` | 32/2/2 | counter, token, a router batch with a binding, an escrow deal with value; failed: unbound program, out of fuel; skipped: a digest not in force, a call during the pause |
| 69 | `programs_pause` | 1/0/0 | one of two validators (f + 1) |

## Coverage

Txs per tag, as applied/failed/skipped. Every tag a block can carry (0–54, 60–61; 55–59 are
reserved) applies at least once, except `unbond`, which v1 always fails. The skipped txs are one
Byzantine leader's junk block, whose txs execution skips and charges to the proposer.

| Tag | Action | `v1-open` | `v1-season0` |
|---|---|---|---|
| 0 | `transfer` | 58/2/2 | – |
| 1 | `register_name` | 3/1/1 | – |
| 2 | `add_key` | 3/1/0 | – |
| 3 | `remove_key` | 1/2/0 | – |
| 4 | `register_validator` | 3/3/0 | 1/0/0 |
| 5 | `approve_validator` | 0/1/0 | 1/1/0 |
| 6 | `remove_validator` | 0/1/0 | 1/1/0 |
| 7 | `unbond` | 0/1/0 | 0/1/0 |
| 8 | `link_github` | 5/1/0 | 4/0/0 |
| 9 | `unlink_github` | 1/1/0 | 1/1/0 |
| 10 | `create_hunt` | 3/1/0 | 1/0/0 |
| 11 | `fund_hunt` | 1/1/0 | – |
| 12 | `award_hunt` | 1/1/0 | – |
| 13 | `cancel_hunt` | 1/1/0 | 1/0/0 |
| 14 | `register_familiar` | 1/1/0 | – |
| 15 | `set_repo` | 0/1/0 | 1/1/0 |
| 16 | `grant_soul` | 0/1/0 | 6/1/0 |
| 17 | `vouch` | 1/1/0 | 1/0/0 |
| 18 | `unvouch` | 1/1/0 | 1/0/0 |
| 19 | `flag_sybil` | 0/1/0 | 1/1/0 |
| 20 | `submit_claim` | 5/2/0 | 3/0/0 |
| 21 | `attest_claim` | 7/1/0 | 2/0/0 |
| 22 | `reject_claim` | 0/1/0 | 1/1/0 |
| 23 | `join_jury` | 3/1/0 | 3/0/0 |
| 24 | `leave_jury` | 1/1/0 | – |
| 25 | `commit_votes` | 3/0/0 | 3/0/0 |
| 26 | `reveal_votes` | 3/1/0 | 3/0/0 |
| 27 | `fund_pool` | 1/0/0 | 1/0/0 |
| 28 | `withdraw_vested` | 1/1/0 | 1/0/0 |
| 29 | `clawback` | 0/1/0 | 1/2/0 |
| 30 | `delegate` | 3/2/0 | – |
| 31 | `undelegate` | 2/0/0 | – |
| 32 | `redelegate` | 1/0/0 | – |
| 33 | `withdraw_unbonded` | 2/1/0 | – |
| 34 | `update_validator` | 2/1/0 | – |
| 35 | `unjail` | 1/1/0 | – |
| 36 | `report_equivocation` | 1/2/0 | – |
| 37 | `beacon_commit` | 22/0/0 | – |
| 38 | `beacon_reveal` | 21/0/0 | – |
| 39 | `propose` | 20/1/0 | – |
| 40 | `vote` | 62/2/0 | – |
| 41 | `unlock` | 1/1/0 | – |
| 42 | `multisig_create` | 1/1/0 | – |
| 43 | `multisig_propose` | 2/0/0 | – |
| 44 | `multisig_approve` | 1/2/0 | – |
| 45 | `renew_name` | 1/2/0 | – |
| 46 | `transfer_name` | 2/1/0 | – |
| 47 | `release_name` | 1/1/0 | – |
| 48 | `claim_distribution` | 2/2/0 | – |
| 49 | `attest_revert` | 2/1/0 | – |
| 50 | `shield` | 1/0/1 | – |
| 51 | `private_transfer` | 1/0/0 | – |
| 52 | `unshield` | 1/0/1 | – |
| 53 | `set_ghost_address` | 1/0/0 | – |
| 54 | `ghost_pause` | 2/1/0 | – |
| 60 | `validator_ready` | 1/1/0 | – |
| 61 | `admit_validator` | 2/1/0 | – |
