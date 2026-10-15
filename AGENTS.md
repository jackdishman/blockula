# Blockula for agents

## Copy-paste prompt (for users)

> Add Blockula to this project. Install the MCP server with `claude mcp add blockula -- npx -y @blockula/mcp` (it uses the public testnet at https://blockula-testnet.fly.dev; add `--network local` for a `blockula dev` chain), then call `create_wallet` and `faucet`. Use `@blockula/sdk` in the app: `Wallet` for signing, `Client` for reads, and `@blockula/sdk/passkey` for user wallets in the browser. Show balances and recipients by `.boo` name, never raw addresses.

## Rules of the road

- **Amounts are strings in FANG** (`"1.5"`), or bigint drops. Never JS floats.
- **Recipients can be names.** `send("alice.boo", "1")` works. Prefer names in UI.
- **Fees are automatic.** `wallet.send` / `registerName` compute the minimum fee (priced per byte, burned).
- **User wallets = passkeys.** Use `createPasskeyWallet` / `unlockPasskeyWallet`. Never ask a user for a seed.
- **Agent wallets = a local key** at `~/.blockula/agent-wallet.json` (mode 600). The right way to spend a user's money:
  1. `create_wallet` → give the user the `key_id`.
  2. The user opens the web wallet (https://blockula-wallet.vercel.app) → **Devices & agents → Give an agent a budget** (or calls `wallet.addSessionKey(keyId, label, { limit, periodBlocks, expiresInBlocks })`).
  3. `use_account("theirname.boo")`. From then on, `send` spends from their account. **The chain** enforces the limit and expiry, and session keys can only send transfers.
  The MCP also caps each send at `BLOCKULA_MAX_SEND` (default 100 FANG).
- **Accounts hold many keys.** `owner` keys (each passkey device, recovery keys) have full control. `session` keys are spend-limited. Every tx says which account it acts `from`; the signing key must be on that account.
- **Every tx returns a `summary`** in plain English, so show it to users as-is.
- **The network is the user's choice, and it's pinned.** The MCP talks to the network its configuration names (`--network mainnet|testnet|local|custom`, `BLOCKULA_RPC`); nothing switches it while it runs, and it never falls back to whatever answers on localhost. It pins the chain id and genesis hash (built in for known networks, `BLOCKULA_CHAIN_ID` / `BLOCKULA_GENESIS_HASH`, or the first ones a node reports, remembered in `~/.blockula/known-chains.json`) and refuses a node that reports anything else. In code, `new Client(rpc, { chainId, genesisHash, prefix })` does the same for every status it reads, including the one each signature is made against.
- **Don't trust one RPC for what matters.** `client.provenAccount(address, { witnesses })` and `client.provenName(name, { witnesses })` check the node's Merkle proof (`/api/proof`) against its state root, and the witnesses (clients for independent nodes) must report the same root for that block.
- **Staking and voting lock the user's FANG.** Stake (`stake`, `wallet.delegate`) takes the unbonding period (21 days on mainnet) to come back; a vote at conviction 1–6 locks its amount for 1–32 lock periods (a week each on mainnet) after voting ends. Ask the user before either. Session keys can't stake or vote: only transfers.
- **Names expire.** `.boo` names are registered for a year at a time; renew with `renew_name` / `wallet.renewName(name, years)`. After a grace period anyone can register an expired name.
- **Explorer links:** `{rpc}/#/tx/{id}`, `{rpc}/#/account/{name}`, `{rpc}/#/block/{height}`, `{rpc}/#/name/{name}`, `{rpc}/#/proposal/{id}`, `{rpc}/#/validators`, `{rpc}/#/governance`, `{rpc}/#/params`.

## Protocol v1 actions (tags 30–49)

Every action has an SDK `Wallet` helper that builds, signs and submits it (and waits for it). Until a module ships on a chain, the node includes these txs and fails them with "not active yet".

| Tag | Action | SDK (`Wallet`) | MCP tool | CLI |
|---|---|---|---|---|
| 30 | `delegate` | `delegate(validator, amount)` | `stake` | `blockula stake delegate` |
| 31 | `undelegate` | `undelegate(validator, shares \| "all")` | `unstake` (FANG or `all`) | `blockula stake undelegate --shares N \| --all` |
| 32 | `redelegate` | `redelegate(from, to, shares \| "all")` | | `blockula stake redelegate` |
| 33 | `withdraw_unbonded` | `withdrawUnbonded()` | `withdraw_stake` | `blockula stake withdraw` |
| 34 | `update_validator` | `updateValidator({ commissionBps, p2pAddr?, httpAddr? })` | | |
| 35 | `unjail` | `unjail(validator)` | | |
| 36 | `report_equivocation` | `reportEquivocation(evidenceHex)` (nodes do this) | | |
| 37, 38 | `beacon_commit`, `beacon_reveal` | `beaconCommit(epoch, hex)`, `beaconReveal(epoch, hex)` (validator nodes do this) | | |
| 39 | `propose` | `propose(proposal.paramChange("max_validators", 40), { title, url?, deposit? })`; builders for every kind in `proposal.*` | | `blockula gov propose` |
| 40 | `vote` | `vote(proposalId, aye, amount, conviction)` | `vote` | `blockula gov vote` |
| 41 | `unlock` | `unlock()` | | `blockula gov unlock` |
| 42–44 | `multisig_create`, `multisig_propose`, `multisig_approve` | `multisigCreate(ownerKeyIds, threshold)`, `multisigPropose(action)`, `multisigApprove(id)` | | `blockula multisig …` |
| 45 | `renew_name` | `renewName(name, years)` | `renew_name` | `blockula name renew` |
| 46, 47 | `transfer_name`, `release_name` | `transferName(name, to)`, `releaseName(name)` | | `blockula name transfer`, `blockula name release` |
| 48 | `claim_distribution` | `claimDistribution(id, amount, proof)` | | |
| 49 | `attest_revert` | `attestRevert(claimId, revertSha)` (validator nodes do this) | | |

Reads: `client.staking(account)`, `client.proposals()`, `client.proposal(id)`, `client.params()`, `client.fees()`, `client.distribution(id)`, `client.multisig(account)`; `stakeSummary`, `proposalSummary` and `paramsOf` normalize them. The MCP also has `my_stake` and `list_proposals`.

## HTTP API (node, default `http://localhost:7070`)

| Method | Path | |
|---|---|---|
| GET | `/api/status` | chain id, height, supply, burned, version, git sha, genesis hash, epoch, sync state (`syncing`, `sync_progress`, verified `finalized_height`, display-only `network_height_claimed`), `history_from` (first block this node holds), `pruning` (`{keep_blocks}` on a pruning node, `null` on an archive node) |
| GET | `/api/blocks?limit=&before=` | recent blocks (from `history_from` on) |
| GET | `/api/block/{height\|hash}` | block + txs; `410` `pruned` for a height this node no longer holds |
| GET | `/api/tx/{id}` | tx with `summary`, `status` (`pending`/`finalized`/`failed` + `error`) |
| POST | `/api/tx` | submit signed tx JSON |
| GET | `/api/account/{address\|name}` | balance, `next_nonce`, names, recent txs (sent by it or naming it), `github`, `familiars`, hunts created/won, `contribution` (Soul, vouches, jury seat, claims, vesting rewards) |
| GET | `/api/name/{name}` | availability + price, or owner; with name renewals (protocol v1) also `expires_at` and the grace period |
| GET | `/api/key/{key_id}` | accounts this key is authorized on |
| GET | `/api/search/{q}` | `{type, id}` for anything |
| POST | `/api/faucet` `{to}` | 100 FANG on faucet chains (1 per address/hour, 1 per IP/10 min; unlimited from localhost) |
| GET | `/api/validators` | epoch, blocks until the next one, active / next-epoch / pending / approved / removed validators, admins, min bond, `max_validators`; with staking (protocol v1) each validator's pool stake, self-bond, commission and jail state |
| GET | `/api/genesis` | the chain's genesis.json |
| GET | `/api/sync?from=&limit=` | finalized consensus blocks + a finalization certificate (served by every node; used by `blockula join`); `410` `pruned` when `from` is below `history_from` |
| GET | `/api/mempool?limit=` | pending txs |
| GET | `/api/hunts?status=` | Hunts (bounties on GitHub issues), biggest first; `open`/`awarded`/`cancelled`/`expired` |
| GET | `/api/hunt/{id}` | one Hunt: repo#issue, bounty, funders, winner + PR, Familiar |
| GET | `/api/github/{handle}` | off-chain check of a GitHub link (gist owned by the handle contains the account's address) |
| GET | `/api/contributions` | the contribution pool: balance, repos, rules, the open Séance, recent Séances |
| GET | `/api/claims?status=&account=` | contribution claims, newest first (`pending`/`verified`/`in_seance`/`scored`/`spam`/`rejected`) |
| GET | `/api/claim/{id}` | one claim: PR, validator attestations, score, payout |
| GET | `/api/seance/{id}` | one Séance: claims, jurors with their assigned pairs, commitments, reveals, results |
| GET | `/api/souls?limit=` | Soul leaderboard |
| GET | `/api/snapshot/latest` | newest certified state snapshot manifest (chunk hashes, certifying blocks + certificate, validator-set history) |
| GET | `/api/snapshot/{height}/chunk/{i}` | snapshot chunk bytes |
| GET | `/api/epochs` | validator-set history + handoff signatures |
| GET | `/api/proof/{namespace}/{key_hex}` | Merkle proof of one state record (inclusion, or exclusion when absent) against the current state root: `namespace`, `key`, `value` (hex or `null`), `proof`, `root`, `height`, `block_hash`. Namespaces are `state::ns` in `node/src/state/mod.rs` (0 scalars, 1 accounts, 2 names, …); an account's key is its address as hex bytes |
| GET | `/api/staking/{account}` | protocol v1: an account's stake: delegations by pool (`validator`, `shares`, `value`), unbonding entries (`amount`, `completes_at`), totals |
| GET | `/api/proposals?status=&limit=` | protocol v1: governance proposals, newest first: `title`, `url`, `kind`, `summary`, `status`, `deposit`, tallies (`aye`, `nay`, `turnout`, `quorum`), bootstrap-council votes, `voting_ends_at`, `executes_at` |
| GET | `/api/proposal/{id}` | protocol v1: one proposal with its votes |
| GET | `/api/params` | protocol v1: the governance-adjustable chain parameters (`node/src/params.rs`), by key |
| GET | `/api/fees` | protocol v1: the base fee per byte now and for the next blocks |
| GET | `/api/distribution/{id}` | protocol v1: a community distribution: Merkle root, total, claimed, claim deadline |
| GET | `/api/multisig/{account}` | protocol v1: a multisig account's owners, threshold and open proposals |
| GET | `/api/health` | 200 if the head is fresh and synced, else 503 + `reason` |
| GET | `/metrics` | Prometheus: height, finalized lag, mempool, epoch, validators, faucet, tx counters |

**Pruning.** Validators keep about two days of blocks by default (`--prune-blocks N` sets the window, `--archive` keeps everything); full nodes keep everything. A fast-synced node holds history from its snapshot on. Blocks, txs and account history below `history_from` are gone from that node: `/api/block` and `/api/sync` answer `410` with `{"error": …, "pruned": true}` (ask an archive node), and `/api/tx` answers `404` for a pruned tx. Reads are rate-limited per client IP (`429` with `Retry-After`; loopback is exempt).

## Working in this repo

- `npm run build` builds everything. `npm test` runs the Rust unit tests plus the SDK e2e (needs `blockula dev` running).
- The tx signing encoding lives in **two places that must match byte-for-byte**: `node/src/types.rs` (`Tx::signing_bytes`) and `sdk-ts/src/tx.ts` (`signingBytes`). Change both, then run the e2e.
- The state tree's proof format lives in `node/src/state/tree.rs` and its light-client port `sdk-ts/src/proof.ts`; `sdk-ts/test/proof-vectors.json` (written by `node/src/state/proof_vectors.rs`) keeps them in step. `npm run test:vectors -w @blockula/sdk` and `npm test -w @blockula/mcp` run offline.

## 🐈‍⬛ Contribute to Blockula (agents welcome)

Agents are first-class contributors, as **Familiars** of a human sponsor. Rewards go to the sponsor's account, and so does accountability. See "Proof of Contribution" in [docs/PLAN.md](docs/PLAN.md).

**Copy-paste prompt:**
> Contribute to Blockula. Call the Blockula MCP's `list_hunts` to see open bounties (each links a GitHub issue with acceptance criteria and FANG escrowed on-chain), pick one labeled `familiar-friendly` if you're an agent, read AGENTS.md and CONTRIBUTING.md, implement it with tests, make CI green, and open a focused PR that references the issue and explains how you verified it. Once it merges, call `submit_contribution` to claim it for the contribution pool. One solid PR beats many small ones.

What earns the most, because it can be verified objectively:
- Hunts with acceptance tests (make the declared tests pass).
- Fuzzing finds with a deterministic repro.
- Benchmark improvements that validators can re-run.
- Test coverage, docs and translations.

What earns nothing: PR floods, cosmetic churn, unverified changes. Payouts are square-rooted per sponsor and vest with clawback, so volume doesn't pay.

Hunt bounties are live on the testnet now (`/api/hunts`, the explorer's Hunts page, or MCP `list_hunts`).

Testnet FANG, Hunts and Soul have no monetary value and come with no promise of any mainnet allocation.

**The contribution pool** pays for merged PRs to pool repos, beyond Hunts:
1. The account you act for links its GitHub handle (`link_github`) and authors the PR.
2. `submit_contribution` (bonded, 1 FANG). If you act through a session key, the user must register it as a Familiar (`wallet.registerFamiliar(keyId, "claude")`); claims then credit them, as your sponsor.
3. Validators check the merge on GitHub. After a revert window, a **Séance** (a jury of humans with Soul) compares it against other claims in pairs.
4. Rewards are square-rooted per sponsor and vest over ~90 days. `my_contributions` shows Soul, claims and rewards.

Agents can't sit on juries; jurors must be human (owner keys with Soul).
