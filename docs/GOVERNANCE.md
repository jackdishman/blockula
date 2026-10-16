# Governance, treasury and multisig accounts

Protocol v1 has no admin keys on mainnet (DEC-10). Everything an admin used to decide is a proposal that FANG holders vote on, with a timelock before it runs; during the first year the validators can also pass proposals as a bootstrap council. This page specifies proposals and votes (GOV-1), the treasury and community distributions (TOK-3), multisig accounts (SF-11), and what replaced the Season 0 admin levers (GOV-2, GOV-3). The code is `node/src/governance.rs`; MAINNET-DESIGN 3.5 is the summary.

Every number below is a governance parameter (`node/src/params.rs`), shown with its mainnet default.

| Parameter | Default | |
|---|---|---|
| `proposal_deposit` | 1,000 FANG | least a proposal bonds |
| `voting_blocks` | 7 days | votes count from the proposal's block through `created_at + voting_blocks` |
| `timelock_blocks` | 2 days | a passed proposal runs this long after voting ends |
| `quorum_bps`, `quorum_floor` | 4%, 20M FANG | quorum is the larger of the two |
| `supermajority_bps` | 66.67% | for upgrades, feature activations, issuance parameters, large treasury spends |
| `treasury_large_bps` | 1% | a spend above this share of the treasury is large |
| `lock_period_blocks` | 7 days | one conviction lock period |
| `governance_spend_height` | ≈ 6 months | treasury spends and distributions are refused before it |
| `bootstrap_end_height` | 1 year | the council exists until then |
| `multisig_expiry_blocks` | 7 days | how long a multisig proposal waits for approvals |

`blockula dev` shortens the periods to minutes and opens spending at once (`ChainParams::dev_overrides`): with its 1 s blocks and 60-block epochs, votes last 60 blocks, the timelock and the lock period 20, unbonding 120 and jail 60, and the deposit is 10 FANG. A dev chain's single validator is the whole bootstrap council, so its key passes a proposal alone (the SDK e2e does).

## Proposals

`Propose { kind, deposit, title, url }` (owner keys only) escrows `deposit` (at least `proposal_deposit`) and opens a vote. What can be known to fail is refused right away: a parameter outside its bounds, a spend before `governance_spend_height`, an unknown feature, a hunt that isn't open, a claim or GitHub link that doesn't exist, an upgrade or claim deadline that would come before the proposal can run, an upgrade that wouldn't leave room for a `CancelUpgrade` after it runs, a cancellation of an upgrade that isn't scheduled (yet) or that it couldn't reach 64 blocks before the activation. Accounts named by `.boo` name are resolved to addresses when the proposal is made, so the account voters see is the account the proposal acts on, even if the name changes hands.

```
created_at ─── voting ───► voting_end ─── timelock ───► execute_at
             Vote txs         tally,         queued         runs at the end
             (through         deposit                       of that block
             voting_end)      settled
```

At the end of block `voting_end` the vote closes:

- **Quorum:** the raw FANG voted (ayes + nays, before conviction) must reach `max(quorum_floor, quorum_bps × unlocked supply)`. The unlocked supply is what could have voted: the FANG held by every account except the keyless system accounts (treasury, community, endowment, bootstrap delegation, contribution pool), liquid, staked (each pool at its stake, so delegators' shares count at their exact pro-rata value) or unbonding, less what vesting locks still hold of each vesting account's holdings. Balances, pool stake and unbonding are running totals the state maps keep as they change, and only genesis allocations carry vesting locks, so it costs O(vesting accounts), never a walk over every account (`governance::unlocked_supply`; `check_invariants` compares it with a full walk). It's taken once an epoch, when the epoch's first vote closes, and every vote closing in that epoch uses that figure.
- **Majority** on conviction-weighted votes: ayes must outweigh nays, or, for a supermajority kind, make up at least `supermajority_bps` of the weight. Supermajority kinds: `ScheduleUpgrade` and `CancelUpgrade` (a simple majority can't undo what a supermajority scheduled; to block an upgrade, vote nay on its `ScheduleUpgrade`), `ActivateFeature` (a security-critical switch such as Ghost Mode; `DeactivateFeature` takes a simple majority), `ParamChange` of an `issuance_*` parameter (whose code bounds also keep the 0.5% floor and the stakers' 50%), and `TreasurySpend` above `treasury_large_bps` of the treasury's balance at the close.
- **Bootstrap council** (until `bootstrap_end_height`): a proposal also passes if ⅔ of the active validator set at the close voted aye and the token-weighted nays don't exceed the token-weighted ayes. A validator's council vote is any `Vote` sent from its consensus key's account or its owner's account (`staking::validator_for_signer`); one per validator, the latest replaces earlier ones. The council is 22–31 independent operators with at most 2 founder seats, so no single party can pass anything.
- **Deposit:** refunded if the vote reached quorum, or if ⅔ of the council voted (either way); burned otherwise. That keeps spam expensive without punishing a proposal that was considered and voted down.

A passed proposal is queued and runs at the end of block `voting_end + timelock_blocks`. A `CancelUpgrade` has no timelock and runs at the end of block `voting_end + 1`: the timelock gives whoever disagrees time to act before a change, and a cancellation changes nothing (the rules in force stay), while it has to land before the upgrade it stops. If its action fails then (say, the treasury can't cover it yet), nothing it did stays and it's marked failed, with the reason.

| Kind | Runs |
|---|---|
| `ParamChange { key, value }` | `State::exec_set_param` (bounds and cross-parameter rules in params.rs) |
| `TreasurySpend { to, amount }` | pays from the keyless treasury, at most what it can transfer (its vesting) |
| `Distribution { merkle_root, total, claim_until }` | moves `total` from the community account into a distribution (below) |
| `SetRepo`, `GrantSoul` (≤ 100 Soul), `FlagSybil`, `RejectClaim`, `Clawback`, `UnlinkGithub` | the former admin levers (`poc.rs`, `chain.rs`) |
| `RefundHunt { hunt_id }` | refunds every funder of an open hunt exactly what they put in |
| `ScheduleUpgrade { version, activation_height }` | `upgrade.rs`: appends to the schedule. The height must be at least 64 blocks after it runs, and late enough that a `CancelUpgrade` proposed the block after it runs still lands 64 blocks before it: `voting_end + timelock_blocks + voting_blocks + 66` or later |
| `CancelUpgrade { version, activation_height }` | `upgrade.rs`: withdraws that schedule entry and any scheduled after it (they were scheduled to follow it), while it's at least 64 blocks away. Proposed once the `ScheduleUpgrade` has run, at least `voting_blocks + 65` blocks before the activation; a cancellation of something no longer scheduled (a second one, say) fails when it runs and changes nothing. Too late for a vote: OPERATIONS.md "Upgrades" |
| `ActivateFeature`, `DeactivateFeature` | `upgrade.rs` (Ghost Mode switches on this way) |
| `Text {}` | nothing: a recorded signal |

Proposal ids count from 0; a distribution's id is its proposal's.

## Votes

`Vote { proposal_id, aye, amount, conviction }` (owner keys only) while the proposal is voting. `amount` can't exceed the voter's `econ::voting_balance`: unvested tokens don't vote, staked ones do. The vote locks `amount` with `econ::lock` (id `vote:<proposal>`) while the height is below `voting_end + 1 + periods × lock_period_blocks`:

| `conviction` | 0 | 1 | 2 | 3 | 4 | 5 | 6 |
|---|---|---|---|---|---|---|---|
| weight | 0.1× | 1× | 2× | 3× | 4× | 5× | 6× |
| lock periods after voting ends | 0 | 1 | 2 | 4 | 8 | 16 | 32 |

Even at conviction 0 the tokens stay locked until voting ends, so they can't vote twice from two accounts. Locks overlap: the same tokens may vote on several proposals at once, and a lock constrains transfers out (P-ECON), never staking. Voting again on the same proposal replaces the vote (and the council vote it carried); its lock only grows, to the larger amount and the later expiry. `Unlock {}` releases expired locks (`econ::release_expired`).

## Treasury and community distributions

The treasury and the community account are keyless system accounts. Only a passed `TreasurySpend` moves treasury funds, and only `Distribution` proposals move community funds. Both are refused before `governance_spend_height`, because at launch almost nothing can vote.

A distribution escrows `total` from the community account. Each recipient claims once with `ClaimDistribution { distribution_id, amount, proof }` until block `claim_until`, as the account the tree names (the tx's `from`); claims can't add up to more than `total`. At the end of the first block after `claim_until`, what's left goes back to the community account. A recipient with nothing to pay the fee with (a new or empty account) pays it out of the claim: admission counts the claim toward the fee once it checks out against the stored root (open, not claimed yet, the proof leads to `merkle_root`, enough left), the claim runs first in the fee phase, and a claim that opens the account also burns `account_creation_fee` from it (`governance::claim_toward_fee`). A claim that doesn't check out is no help, so from an empty account it isn't admitted at all: a bad proof is never free.

### Merkle tree format

- **Leaf:** `BLAKE3("BLOCKULA_DISTRIBUTION_LEAF_V1" ‖ distribution_id u64 LE ‖ len(address) u16 LE ‖ address ‖ amount u64 LE)`. `address` is the bech32m string as the claimant's tx spells it (lowercase, with the chain's prefix); `amount` is in drops.
- **Node:** `BLAKE3("BLOCKULA_DISTRIBUTION_NODE_V1" ‖ min(a, b) ‖ max(a, b))`, comparing the two 32-byte children as byte strings. Sorted pairs make a proof just the sibling hashes from the leaf up, with no positions; the two domain strings keep a node from passing as a leaf.
- **Building** (`governance::build_distribution`, `blockula gov tree`): sort the leaf hashes ascending, then pair them left to right level by level, carrying an odd last node up unchanged; one leaf is its own root. Addresses must be distinct and amounts non-zero. Any tree shape verifies; this one is the reference so independent builders agree on the root.
- **Verifying:** fold the proof: `h = leaf; for s in proof: h = node(h, s)`; the claim is valid if `h == merkle_root`. Proofs are at most 64 hashes (`MAX_PROOF`).
- **The id is in the leaf**, so a proof for one distribution never verifies in another. Build the tree for the id the proposal will get (`next_id` in `/api/proposals`) just before proposing; `blockula gov propose distribution` refuses a tree built for another id. `Distribution` carries no id of its own (adding one would change the tx encoding on both sides): if another proposal takes the id between building the tree and inclusion, nobody can claim from that distribution and its `total` returns to the community account after `claim_until`, so propose again with a tree for the new id.

Vectors (`governance::tests::merkle_trees_build_and_verify`), distribution 7:

| Address (bytes) | Amount (drops) | Leaf |
|---|---|---|
| `boo1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursz6hc9p` ([7; 32]) | 1000000000 | `17031a8f14226964c8df49241c3572e7b17e3f492ae175f6b0b7e3d68e7b9515` |
| `boo1pqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyq66rhlm` ([8; 32]) | 2500000000 | `b60bfc85af7ea12a7375f737eaf2ffed0f7768d57b9952d5994d16fb774b32c2` |
| `tboo1pyysjzgfpyysjzgfpyysjzgfpyysjzgfpyysjzgfpyysjzgfpyysj0amg2` ([9; 32]) | 1 | `52da68dc24ed02c8ef334e1c3d788582aef1b1b949a7be762ddf94725cb84364` |

Root `f1ccd2c1386b5f7bcffeb39b226cbef621a1f11c3ee919109d05bd8b5aeced3b`. Proofs, in the table's order: `[52da68dc…84364, b60bfc85…b32c2]`, `[ab68c5cbdeed677ec1daf469d1fe2818daca608866456091bef5c1382f186333]`, `[17031a8f…b9515, b60bfc85…b32c2]`.

### SDK helper

`@blockula/sdk` exports these (`sdk-ts/src/distribution.ts`, with `@noble/hashes/blake3`); `npm run test:vectors` checks them against `sdk-ts/test/distribution-vectors.json` (distribution 7 above and trees of 1–9 entries), which the node's `governance::tests::distribution_vectors_match_the_sdk_file` checks too:

```ts
/** The leaf of `address`'s `amount` (drops) in distribution `id`. */
export function distributionLeaf(id: bigint | number, address: string, amount: bigint): Uint8Array;
/** Sorted-pair inner node. */
export function distributionNode(a: Uint8Array, b: Uint8Array): Uint8Array;
/** Root and per-entry proofs (hex), entries in input order; the reference shape above. */
export function buildDistribution(id: bigint | number, entries: { address: string; amount: bigint }[]): { root: string; proofs: string[][] };
/** The root (hex) a leaf and its proof lead to. */
export function distributionRoot(id: bigint | number, address: string, amount: bigint, proof: string[]): string;
/** Whether `proof` leads from the leaf to `root` (hex). */
export function verifyDistributionProof(root: string, id: bigint | number, address: string, amount: bigint, proof: string[]): boolean;
// Wallet: claimDistribution(id, amount, proof) builds { type: "claim_distribution", distribution_id, amount: String(amount), proof }.
```

The tree file `blockula gov tree` writes (`{ distribution_id, merkle_root, total, claims: [{ address, amount, proof }] }`, amounts in drops as strings) is what proposals publish; wallets look the user's address up in it.

## Multisig accounts

`MultisigCreate { owners, threshold }` makes the sending account M-of-N over its owner keys. `owners` must list exactly the account's owner keys (add or remove keys first), and `1 ≤ threshold ≤ owners`. From then on:

- The account's owner keys may only sign `MultisigPropose` and `MultisigApprove`; admission refuses any other tx they sign (`governance::check_multisig_admission`, called from `State::admit`). Session keys are unaffected and keep spending within their limits.
- `MultisigPropose { action }` records the action with the proposer's approval. `MultisigApprove { id }` adds another owner key's. The tx that brings the approvals of current owner keys to `threshold` runs the action as the account (with an owner key's authority, paying from the account); if the action fails, that approval fails too and the proposal stays open, so it can be retried.
- Proposals expire after `multisig_expiry_blocks`; at most 32 are open per account (expired ones are cleared by the next proposal).
- The owner set is always the account's current owner keys: an approved `AddKey`/`RemoveKey` changes it, but never below `threshold`. A multisig changes its threshold by proposing `MultisigCreate` again.

Multisig accounts can propose, vote, register validators and do everything else an account can, through their approvals. The foundation's allocation (DEC-5) is meant to sit in one with at least two non-founder signers.

## What replaced the admin levers

- **GOV-2 (Hunts):** only a hunt's creator can award it, on every chain. Nobody can redirect escrow: cancelling (the creator after expiry, an admin on dev and test chains, or a `RefundHunt` proposal) refunds every funder exactly.
- **GOV-3 (Proof of Contribution):** Soul starts from the published genesis `soul_ledger`; afterwards only juries and `GrantSoul` proposals (≤ 100 Soul each) change it. `SetRepo`, `GrantSoul`, `FlagSybil`, `RejectClaim` and `Clawback` as txs work only on chains with admins (dev and test); elsewhere they fail with "this chain has none: put it to a governance proposal". Mainnet's genesis has no admins (DEC-10).

## State and interfaces

- Records (`state::ns`): proposals (37), votes keyed `(proposal, voter)` (38), multisigs (39), multisig proposals (40), distributions (41), claims keyed `(distribution, address)` (42); the META scalar `governance` holds the id counters, the open proposal and distribution lists `end_block` walks, and the epoch's unlocked supply. A proposal's kind and a multisig proposal's action are stored in their tx encoding.
- `State::check_invariants` covers deposits of proposals still voting and what's left in open distributions as escrow, tallies as the sum of their votes, council votes matching their voters', multisigs keeping `threshold` owner keys, and distributions matching their claims.
- Other packages provide `econ::{voting_balance, transferable, lock, release_expired}` (vesting and vote locks), `staking::{holdings, validator_for_signer}` (stake and the council's members), and the lever bodies in `chain.rs`, `poc.rs` and `upgrade.rs`.
- HTTP: `/api/proposals`, `/api/proposal/{id}`, `/api/distribution/{id}?address=`, `/api/multisig/{account}`. CLI: `blockula gov list | show | propose <kind> | vote | unlock | tree | claim`, `blockula multisig create | propose | approve | show`.
