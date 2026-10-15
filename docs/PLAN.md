# Blockula plan

## Decisions (locked)

| Decision | Choice |
|---|---|
| Name / token | Blockula / FANG (1 FANG = 10⁹ drops) |
| Names | `.boo`, native to the protocol |
| Node | Rust; multi-validator consensus on **Commonware** primitives (Phase 1) |
| Execution | **Native modules only** in v1 (accounts, transfers, names, shielded pool, storage, staking). WASM/RISC-V contracts in v2. No EVM (it's bound to ECDSA). |
| L1 vs L2 | Standalone L1, because settling on Ethereum would bring back its ECDSA/BLS weakness |

## Why this is quantum-safe

- **Signatures:** ML-DSA-65 (FIPS 204) for users and validators. SLH-DSA (FIPS 205, hash-only) for cold recovery keys.
- **Hashes:** BLAKE3-256. Grover's algorithm only halves security, and 256-bit addresses keep ~128-bit collision resistance even against quantum attacks.
- **Encryption:** ML-KEM-768 (FIPS 203) for note encryption and The Crypt.
- **Network:** validators authenticate with ML-DSA-65 and derive p2p session keys from hybrid X25519 + ML-KEM-768, so recorded traffic stays private unless both break. The node's outbound HTTPS prefers `X25519MLKEM768`. Details: [PQ-TRANSPORT.md](PQ-TRANSPORT.md).
- **ZK:** hash-based STARKs (FRI + Poseidon2, e.g. Plonky3) only. No Groth16/KZG, because pairings fall to Shor's algorithm.
- **Passkeys:** WebAuthn signs with P-256, which is not quantum-safe, so the passkey never signs on-chain. Its **PRF extension** output (symmetric) derives the ML-DSA key: `seed = BLAKE3-KDF("blockula passkey wallet v1", prf)`. Implemented in `sdk-ts/src/passkey.ts`.

## Accounts

Native account abstraction (Phase 1). Each account holds a list of authorized keys with roles:
- **device:** passkey-derived ML-DSA keys (one per passkey provider)
- **recovery:** SLH-DSA cold key (downloadable recovery kit) and/or guardians (social recovery)
- **agent session:** ML-DSA key with spend limit per day, expiry and allowed modules. Replaces today's client-side `BLOCKULA_MAX_SEND`.
- **Fallback:** for authenticators without PRF, a password-encrypted key, clearly labeled as weaker.

## 👻 Ghost Mode (private transfers) and 🪦 The Crypt (private storage), Phase 2

- An Orchard-style shielded pool: notes are Poseidon2 commitments, spending publishes a nullifier, and a STARK proves validity. Note contents are encrypted to the recipient with ML-KEM.
- **Viewing keys** for selective disclosure (accountants, auditors, compliance).
- Proofs are about 50–200 KB, so validators **recursively aggregate** a block's proofs into one.
- The explorer shows "👻 a ghost transaction happened" (nullifier, fee, proof size), never parties or amounts. The tutorial walks users through exactly this.
- **The Crypt:** blobs are ML-KEM-encrypted and stored by nodes. On-chain: commitment + access list + storage deposit. Sharing re-encrypts the data key to another user.
- Later: private names, private voting, selective-disclosure credentials ("over 18" without revealing a birthday).

## Tokenomics (FANG)

- **Fees:** the per-byte base fee is **burned** 🔥; tips go to the proposer. Priced by bytes because post-quantum signatures are about 3.3 KB. Protocol v1: it moves with block fullness (12.5% a block at most, never below the old minimum) and is fixed 17 blocks ahead (MAINNET-DESIGN 3.6).
- **Issuance:** starts at 6% a year, decays 15% a year to a permanent **1.5% tail emission** so security never depends on fees alone, minted every epoch. Split 70% stakers / 20% contribution pool / 10% treasury (see Proof of Contribution and [TOKENOMICS.md](TOKENOMICS.md)).
- **Names:** length-based price (3 chars: 100, 4 chars: 25, 5+ chars: 5 FANG) to the treasury, for a year; renewal costs 20% of the price a year, with a 30-day grace, so squatters bleed (protocol v1, DEC-11).
- **State growth:** opening an account burns 0.1 FANG and names renew yearly; every other record is bonded (DEC-6).
- **Treasury:** 10% of issuance plus name fees. Pays for sponsored onboarding txs (new users need zero FANG), faucets, audits and grants.
- **Ghost Mode:** a small verification premium, partly offset by aggregation.
- **Health targets:** ≥50% staked, Nakamoto coefficient ≥10, slashing for double-signing (heavy) and downtime (light).
- Get legal review before any sale or distribution.

## 🪦 Proof of Contribution (rewarding useful work)

**Principle:** anything counted automatically gets farmed (commit counts, lines changed, even merged PRs). So rewards come from **people with something at stake**, are paid from a **fixed per-epoch pool** (spam dilutes other contributors instead of printing tokens), and **vest slowly** so they can be clawed back. This is separate from consensus: block production stays pure proof-of-stake, because mixing in reputation would give attackers a cheaper path to control.

### Pieces
- **Souls:** non-transferable reputation. Earned slowly, decays over time, can't be bought. It boosts reward share, jury eligibility and voting weight.
- **Identity:** link GitHub by posting your key id in a public gist; validators check it independently. New identities need a **vouch** from someone with Soul. Vouchers earn a small cut of the newcomer's early rewards, and lose stake and Soul if the newcomer turns out to be a sybil.
- **Repos:** registered with a stake plus a governance vote. Blockula's own repo is #1, so building Blockula earns FANG.
- **Claims:** "PR #123, merge SHA, my GitHub id" plus a small refundable bond. Validators each check the GitHub API, and a claim needs most of them (e.g. 2/3) to agree. A PR pays only after **30 days without a revert**.
- **Séances (review juries):** each epoch, a random jury of staked reviewers is drawn. The randomness comes from block hashes (quantum-safe). Jurors **commit, then reveal** pairwise judgments ("which PR mattered more?"). Jurors who keep diverging badly from consensus lose Soul. Reviewing is itself rewarded work.
- **Payout:** governance splits the pool across repos; maintainer weights and jury scores split each repo's share across PRs. Each person's payout is **square-rooted**, so 50 tiny PRs earn less than one solid PR. Rewards **vest over 90 days with clawback**.
- **Hunts:** bounties on specific issues, funded by anyone. Explicit demand, and the least gameable reward type, so they ship first.
- **Covens:** teams claiming Hunts together, with the payout split recorded on-chain and shared streaks. **Seasons** (Season 1: October) with leaderboards ranked by Soul, not tokens.
- **Profiles** in the explorer: merged PRs, Soul, who vouched for whom.

### Status: Hunts v0 and Proof of Contribution v1 are live on the testnet
**Hunts**
- `create_hunt` escrows a bounty on a GitHub issue (min 10 FANG, ≤ ~1 year). `fund_hunt` lets anyone add to it. `award_hunt` (creator or Season 0 admin) pays it out once and records the PR; self-awards are allowed but flagged. `cancel_hunt` (creator after expiry, admin any time) refunds every funder exactly what they put in.
- `blockula hunt award <id> --pr <n>` checks on GitHub that the PR merged into the hunt's repo's default branch, looks up the account its author linked, confirms, and signs the award (`--to` names a recipient instead, `--familiar` credits their agent). `hunt list|show|create|fund|cancel` cover the rest.
- `link_github` is an on-chain claim: one handle per account, one account per handle. The holder or an admin can unlink, so admins can clear squatters. Nodes check it off-chain: a public gist owned by the handle must contain the account's address (`/api/github/{handle}`). Validators re-check it for every contribution claim.
- `register_familiar` marks a session key as an agent Familiar. Awards can credit it, and profiles show "won by vlad.boo's Familiar 'claude'".

**Proof of Contribution v1** (`node/src/poc.rs`; explorer Contribute page; `blockula contrib …`; SDK; MCP `contribution_pool`, `submit_contribution`, `my_contributions`)
- **Souls:** milli-Soul per account, never transferable. It decays (half-life ~1 year, integer math) and is earned only from jury outcomes: up to 10 Soul for an accepted claim (scaled by its score), +1 for a juror who agrees with the outcome. It's lost for spam claims (−20), diverging badly as a juror (−2), not revealing (−2, so holding back a bad ballot never pays), or vouching for a sybil (−50). Season 0 admins bootstrap it with `grant_soul` (≤ 100 per grant); governance replaces that in Phase 3. Soul gates vouching (10) and jury duty (5), raises the open-claim cap (3 + 1 per 10 Soul, max 20) and boosts payouts up to 1.5× (at 100 Soul).
- **Vouching:** `vouch` bonds 10 FANG on a newcomer: someone with no Soul and no judged work (one vouch per 10 Soul held, max 10). It's locked for ~90 days, then `unvouch` returns the bond; the newcomer can decline it any time. While the vouch is young, the voucher gets 10% of the newcomer's rewards. `flag_sybil` (Season 0 admins) zeroes the account's Soul, rejects its open claims, claws back its unvested rewards, removes it from the jury pool, voids its vouches, and slashes its voucher's bond and 50 Soul.
- **Repos:** `set_repo` (admins) puts a repo in the pool with a weight of 1–100. Removing one refunds the claims still waiting on it.
- **Claims:** `submit_claim(repo, pr)` needs a linked GitHub handle and Soul or a vouch, and bonds 1 FANG × (1 + open claims). A registered Familiar session key may sign it for its sponsor; the bond counts against its spend limit. One claim per PR.
- **Validator-attested GitHub facts:** every validator's node checks pending claims (the PR is merged into that repo's default branch, authored by the claimant's handle, and the handle's gist contains the claimant's address) and signs `attest_claim` with what it found. A BFT quorum (n − ⌊(n−1)/3⌋) agreeing on the merge commit verifies the claim; a quorum refuting it rejects it (bond to the pool; the PR can be claimed again). The first verified claim locks the GitHub handle to that account: relinking the handle elsewhere can't split one person's work across accounts. Open PRs are re-checked later, and claims never settled within ~7 days are refunded.
- **Revert window:** a verified claim waits ~30 days before it can be judged; `reject_claim` (admins) handles reverts and claws back a scored claim's rewards (the voucher's cut too) and Soul. Rejecting a claim that was never verified frees the PR for its real author.
- **Séances:** when no Séance is open and ≥ 2 matured claims and ≥ 3 eligible jurors exist, one opens (claims oldest first, ≤ 64). Up to 7 jurors are drawn from the `join_jury` pool (5 Soul, 10 FANG bond, owner keys only, so no Familiars) using a seed that chains every block hash. Each juror gets up to 24 random pairs, never involving their own claims or anyone they vouched for (or who vouched for them). For the first ⅔ of the Séance (~7 days in total) jurors `commit_votes` BLAKE3(id, juror, votes, spam flags, salt); in the last ⅓ they `reveal_votes`. Votes are `a`, `b` or `=` per pair, plus claims they flag as spam.
- **Scoring:** fewer than 3 reveals and nothing is decided: claims carry over, the budget goes back, and no-shows still pay. Otherwise a claim's score is its win rate across revealed comparisons (a tie counts half). Flagged by ≥ 2 jurors and by more than half of those who saw it: spam (bond to the pool, −20 Soul). Below 15%: bond back, no reward. No comparisons at all: carried over to the next Séance. Jurors are graded against the outcome: ≥ 60% agreement earns Soul and a share of the jurors' budget, < 40% loses Soul, and voting `=` on everything (50%) earns nothing. No-shows lose 10% of their bond and 2 Soul, and are benched. Jurors flagged as sybils mid-Séance don't count.
- **Payout:** each Séance reserves up to 25% of the pool's balance when it opens, scaled by its claims (the full share from 16), so a tiny Séance can't drain the pool. Jurors get 10% (by agreement); claims get the rest. Per person (GitHub handle), a weight is √(Σ repo weight × score) × Soul boost. A sponsor's Familiars count as the sponsor, so 50 tiny PRs that lose their comparisons earn less than one solid PR. Rewards are grants that vest linearly over ~90 days: `withdraw_vested` pulls what has vested, `clawback` (admins) returns the rest to the pool. Rounding dust and unused budget go back to the pool.
- **The pool** is a system account (`🪦 contribution pool`) funded by `fund_pool`, plain transfers to it, and every slashed bond. The 20%-of-issuance stream starts when issuance does (Phase 3).
- Timings come from genesis (`contribution`) or default to the durations above. `blockula dev` uses minutes instead (90-block Séances, no maturity wait) so the whole loop can be tried locally, and `summon` takes `--seance-blocks`, `--claim-maturity-blocks` and `--vesting-blocks`.
- API: `/api/contributions`, `/api/claims`, `/api/claim/{id}`, `/api/seance/{id}` (each juror's assigned pairs), `/api/souls`, plus a `contribution` section on `/api/account`.

### 🐈‍⬛ Familiars: agent contributions, encouraged
Agents are first-class contributors, but every agent is the **Familiar of a human sponsor** who has Soul.
- **Binding:** a Familiar is a session key on the sponsor's account plus an on-chain `familiar_of` link. Its rewards go to the sponsor (who can set a split), and its mistakes cost the sponsor's Soul.
- **Volume doesn't pay:** all of a sponsor's Familiars are pooled for the square-root payout. There's a per-Soul-tier cap on Familiars and open claims, and the claim bond grows with the number of open claims.
- **Agent-shaped work pays best, because it can be checked objectively:**
  - Hunts with **acceptance tests**: the bounty pays when validators re-run the declared tests and they pass.
  - **Fuzzing and bug repros**: a crash that reproduces deterministically from the submitted input.
  - **Benchmarks**: validators re-run them to measure the gain.
  - **Test coverage**, docs, translations and spec conformance.
- **Network work:** Familiars can run nodes and validators through `start_dev_node` and `blockula join`. Validator rewards come from stake and uptime, so they can't be farmed.
- **Triage:** Familiars can pre-screen claims and flag spam. They earn only when a human jury upholds the flag; jurors themselves must be human.
- **MCP tools** (Phase 1.5): `list_hunts`, `claim_hunt`, `submit_contribution`, `my_contributions`, `become_familiar`. AGENTS.md gets a "contribute to Blockula" prompt: *find an open Hunt, implement it, make its acceptance tests pass, submit.*

### Why farming fails
| Attack | Defense |
|---|---|
| Bot or AI spam PRs | Must be merged and survive 30 days, bond slashed if the jury marks spam, square-root payout pooled per sponsor |
| Fake GitHub accounts (sybils) | Vouch required, voucher slashed if the account is fake, rewards start small until Soul builds up |
| Maintainer merging their own fake accounts' PRs | Juries randomly audit each repo, repo budget slashed if caught, vesting with clawback |
| Jurors colluding | Random selection, commit-reveal, no reviewing people you vouched for, flagging of suspicious reviewer–author pairs |
| Typo and whitespace PRs | Minimum score threshold |
| Fleets of agents | Pooled per sponsor, capped by Soul tier, sponsor slashed for spam |

### Forks and governance
- **Code forks:** a share (e.g. 10%) of a fork's rewards flows back to the original repo's contributors.
- **Chain forks:** `blockula summon` can start from a Soul-ledger snapshot, so a forked network keeps its community. Forking stays a credible exit.
- **Voting weight:** about √stake + Soul. **Conviction voting** for treasury and repo budgets. Protocol upgrades: validators signal readiness in the blocks they propose, then a vote and a timelock schedule the activation height (OPERATIONS.md "Upgrades"). **Private ballots** via Ghost Mode later (tallies public, votes hidden).
- **Issuance split:** 70% stakers / 20% contribution pool / 10% treasury. The pool is fixed per epoch, so contribution rewards never add inflation.

## Network and sync

What runs today (Commonware `=2026.9.0`: consensus, p2p, broadcast, storage, runtime):
- **Consensus:** simplex BFT. Votes and finality certificates are ML-DSA-65 (deterministic FIPS 204 signing, the Commonware namespace is the FIPS 204 context string; `node/src/pq.rs`). A certificate is a signer bitmap plus one 3,309-byte signature per signer, with no aggregation. Leaders rotate round-robin; `block_time_ms` paces proposals.
- **Deferred execution:** consensus orders blocks (parent, height, proposer = leader, timestamp, txs as signed JSON, plus the proposer's latest executed `(height, state root)`). Before voting, validators check structure, size limits (512 txs / 4 MiB), timestamps, proposer, tx signatures, chain id and minimum fee, and the attached state root if they've executed that height. Finalized blocks are executed in order: invalid txs are skipped, and `blocks.jsonl` is synced before marshal is acknowledged.
- **Dissemination and backfill:** marshal (standard/inline) broadcasts full blocks, delivers finalized blocks in order, and backfills anything a restarted validator missed.
- **p2p:** `authenticated::discovery` over tokio with ML-DSA-65 identities (the validator's account seed). The genesis lists every genesis validator's `public_key`, `p2p_addr` and `http_addr`.
  - `--bind` sets the local interface (127.0.0.1 by default; 0.0.0.0 or :: on servers). Peers dial the advertised address, which comes from genesis or from a registration.
  - The `recommended` preset is used for public advertised addresses and `local` for private ones.
- **Epochs and validator onboarding (Season 0 allowlist):**
  - Genesis has `epoch_blocks`, `validator_admins`, `min_bond` and `max_validators` (32).
  - Txs: `register_validator` (bonds FANG, creates a *pending* validator owned by the sender), `approve_validator` / `remove_validator` (admins only), and `unbond` (one full epoch after the validator left).
  - After executing the last block of epoch *e*, every node computes the set for *e+1* from state: current members minus removed ones, plus approved ones in approval order, capped. It is never empty, and removals that would leave no validator are rejected.
  - The set is part of state and of the state root.
  - At the boundary, the node `track`s the new peer set and starts a new simplex engine and scheme for *e+1*, with the boundary block as its floor. This follows the Commonware reshare orchestrator, minus the DKG.
  - Vote, certificate and resolver channels are muxed by epoch. A node that sees votes from a later epoch hints marshal to fetch our epoch's boundary finalization, so it catches up epoch by epoch.
  - Marshal spans epochs through an epoch-scoped scheme provider.
  - Nodes outside the active set observe (verify-only scheme) until their activation epoch.
  - CLI: `blockula validator init|register|approve|remove|unbond|list`.
  - API: `/api/validators`. The explorer has a Validators page with every admin action.
- **Mempool:** a tx submitted to any validator is checked against state + mempool, then gossiped to all validators on its own channel and de-duplicated by id. After each finalized block, included or now-invalid txs are evicted.
- **Faucet (testnets):** each validator's genesis-funded account pays its own faucet drips, so there's no shared secret and no nonce races. Full nodes forward faucet requests.
- `blockula summon <id> [--validators N]`, `blockula run --home`, `blockula dev` (one-validator simplex).
- **HTTP hardening:**
  - 128 KB request bodies.
  - Per-client tx rate limit. The client is the socket address unless `--client-ip-header` names the reverse proxy's header (e.g. `fly-client-ip`). Forwarded client IPs are honored only from `--trusted-forwarder` IPs or with `BLOCKULA_FORWARDER_SECRET`. IPv6 clients are counted per /64.
  - Faucet: 1 drip per address per hour and per client per 10 minutes, counted only on success. At most 32 drips pending, otherwise `503 faucet busy`.
  - Mempool: capped at 5,000 txs and 64 per sender, evicting the lowest fee-per-byte tx when full.
  - CORS stays permissive for the web wallet.
- **`blockula join --genesis <file|validator URL> --home <dir> [--genesis-hash <hash>] [--checkpoint HEIGHT:HASH[:SET_HASH]]`:** a non-validator full node. The genesis must be pinned (`--genesis-hash`, or compiled in for a known chain id), and the newest checkpoint younger than the weak-subjectivity period (OPERATIONS.md "Weak subjectivity"). It serves the API + explorer immediately (`/api/status` shows `syncing`, `sync_progress` and `finalized_height`). It pulls finalized blocks from validators over HTTP (`/api/sync`), checks the hash links and an ML-DSA quorum finalization certificate per batch, and executes them. Batches never cross an epoch boundary, so each one is checked against that epoch's set, which the node learned by executing the previous boundary. Txs and faucet requests are forwarded to a validator.
- **Fast sync (default for `join` and new validators; `--full` replays from genesis):**
  - Every 100 blocks is a checkpoint C. Consensus requires the block at C+4 to carry `exec_height = C` and `exec_root = state_root(C)`. Every validator must have executed C and checked the root before voting; it waits up to 3 block times rather than vote unchecked. So any finalization certificate on C+4 or a descendant certifies `state_root(C)`.
  - Every node writes its last two checkpoints' state (JSON → zstd → 1 MiB chunks, each BLAKE3-hashed) and serves `/api/snapshot/latest` plus the chunks.
  - The manifest carries blocks C+1..F, F's certificate, the validator-set history, and the epoch anchor (the previous epoch's last block, where consensus starts the current epoch) with its certificate from that epoch's set when it's before C.
  - The joiner checks the set history from genesis, the certificates, the hash links and the C+4 commitment, and checks all of it against its trusted checkpoints. It then downloads and hash-checks the chunks (4 in parallel), rebuilds the state, requires its root to equal the certified one, executes C+1..F and follows normally.
  - History before C isn't stored on that node; the explorer and `/api/block` say so.
- **Validator-set history:** when a boundary changes the set, members of the outgoing set sign a handoff `(chain, epoch, hash(new set))`, retroactively too. Nodes swap signatures over `/api/epochs` every 30 s. A joiner needs a 2f+1 handoff for every change. Unchanged epochs need no proof: hiding a change only makes verification fail.

Next:
1. Backfill history behind a fast-synced node in the background, and later a STARK proof of the chain's history.
2. Full nodes on p2p. Discovery only admits peers in the tracked set, so permissionless joining needs a secondary-peer admission policy.
3. A light-client mode that keeps no full state at all.
- Validators: about 100, ML-DSA signatures. There is no BLS-style aggregation (a 67-of-100 certificate is ~222 KB), so keep the set small and add STARK signature aggregation later.

## Roadmap

| Phase | Scope | Status |
|---|---|---|
| 0 | Dev node, ML-DSA accounts, transfers, `.boo` names, fee burn, built-in explorer + tutorial, TS SDK, passkey helpers, MCP server, `summon`/`run` | ✅ built |
| 1 | Multi-validator BFT (Commonware) ✅, p2p ✅, `join` ✅ (snapshot sync next), epochs + on-chain validator onboarding (Season 0 allowlist) ✅, public networking + HTTP hardening ✅, account abstraction + session keys ✅, passkey web wallet ✅ (blockula-wallet.vercel.app), public testnet on Fly.io ✅ (blockula-testnet.fly.dev), npm `@blockula/sdk` + `@blockula/mcp` ✅, internal security/product review + fixes ✅, CLI wallet ✅, health/metrics ✅. certified snapshots + fast sync ✅. Remaining: a standalone indexer | mostly done |
| 1.5 | GitHub linking ✅ (on-chain claim + node-side gist check), Hunts v0 ✅ (escrow, crowdfunding, award/cancel/refund on-chain, `blockula hunt award` checks the merge), Familiars ✅ (registered agent session keys credited on awards and allowed to claim), validator-attested GitHub facts ✅, MCP contribution tools ✅. Remaining: acceptance-test Hunts | mostly done |
| 2 | Proof of Contribution v1 ✅: Souls, vouching, claims, Séances (commit-reveal pairwise juries), contribution pool with √-per-sponsor payouts, vesting and clawback. Remaining: a juror UI in the web wallet, Covens and Seasons, jury-run sybil flags and revert checks (replacing the Season 0 admin levers), Ghost Mode + viewing keys, The Crypt, proof aggregation | in progress |
| 3 | Permissionless staking/slashing (replaces the Season 0 allowlist), tokenomics live, governance (conviction voting, repo budgets, fork royalties), audits, mainnet. The full gap analysis, blockers and sequence are in [MAINNET.md](MAINNET.md) | |
| 4 | WASM/RISC-V contracts, light clients, STARK signature aggregation | |

## Known limits

- Fast sync rebuilds before it can check. Only the snapshot's state root is certified, not the manifest's chunk list, so a malicious peer the joiner asks can serve chunks that decompress to up to `MAX_STATE_BYTES` (4 GiB) of tiny records (~140M), and the joiner builds the maps and the tree for all of them (tens of GB) before the root mismatch shows. A joiner with less memory dies and, restarted, tries the next peer. Until the checkpoint-certifying block also commits to a snapshot digest (the chunk-hash list and the record count, CRY-2), pin `--genesis-hash`, prefer known peers, and size joiners for the largest plausible state.
- Sync is weakly subjective, like any proof-of-stake chain: keys of a validator set that has since unbonded can sign handoffs to a made-up set (retroactively, and handoffs aren't slashable), so a joiner that trusts only the genesis could be fed an alternate history. Joiners pin the genesis (required) and recent checkpoints that fix a validator set (compiled into each release, or `--checkpoint`), and refuse roots older than the weak-subjectivity period: half the evidence window, ~10.5 days at genesis parameters (OPERATIONS.md "Weak subjectivity"). The checkpoints are only as good as the operators who agree on them and the release signature.
- Hunts: awards are trusted to the creator (or an admin). `blockula hunt award` checks GitHub before signing, but the chain doesn't. GitHub links are claims plus a per-node off-chain gist check (cached 10 min, 30 GitHub calls a minute per node). Squatted handles need an admin to unlink.
- Proof of Contribution v1:
  - **Season 0 trust.** Admins bootstrap Soul, set repo weights, flag sybils, reject reverted claims and claw back rewards. Validators are trusted to report GitHub honestly (a BFT quorum must agree). Phase 3 moves these to governance and juries.
  - **Jury randomness** chains block hashes, so the proposer of the block that opens a Séance can grind its hash (by reordering txs) to bias which jurors are drawn. A VRF or threshold beacon comes later.
  - **Collusion.** Most of a jury agreeing to inflate a claim succeeds: Soul and juror agreement are measured against the jury's own outcome, which includes each juror's vote. Random audits of repos and reviewer–author pair flagging aren't built yet.
  - **Reverts** aren't detected automatically; the ~30-day window relies on an admin's `reject_claim`.
  - **GitHub rate limits:** each validator makes 2 GitHub calls per pending claim (set `GITHUB_TOKEN` on validators). If GitHub is unreachable, claims stay pending and are refunded after ~7 days.
  - **Funding:** until issuance starts (Phase 3), the pool only holds donations and slashed bonds.
  - **Cost:** the contribution state is hashed whole into every block's state root (like Hunts), and some lookups scan all claims or grants.
  - **Attestation fees** come from each validator's own (consensus-key) account. A validator registered from a separate owner account needs some FANG at its own address, or its attestations fail; if a third of the set can't attest, claims never reach quorum.
  - **Mempool estimates:** the proposer's dry run of not-yet-executed blocks skips end-of-block Séance steps (like epoch ends). That only delays or fails a tx (fee charged); it doesn't affect consensus.
  - **Default timings are consensus rules.** Chains whose genesis omits `contribution` (the public testnet included) take them from code, as they do every constant in `poc.rs`, so changing them is a coordinated upgrade.

- **The PQ p2p transport is a vendored fork.** The hybrid X25519 + ML-KEM-768 handshake lives in a patched copy of `commonware-stream` 2026.9.0 (`node/vendor/`) until Commonware ships a pluggable KEM (#5183), and nodes on the older X25519-only transport can't peer with it. The public RPC's TLS is only post-quantum behind an edge that negotiates `X25519MLKEM768` (Fly's didn't when measured), and HTTPS certificates are still classical ([PQ-TRANSPORT.md](PQ-TRANSPORT.md)).
- Validator onboarding is a Season 0 allowlist (admin approval), not permissionless staking. The bond isn't slashable yet: simplex reports attributable faults, but nothing acts on them. Voting power is one vote per validator.
- An approved validator can't connect over p2p until approval (pending registrations aren't admitted as peers). It syncs from genesis via marshal once it's admitted, and votes once caught up. Until then it counts as offline, so approve at most ⌊(n−1)/3⌋ new validators per epoch.
- If every genesis validator is removed, new nodes still bootstrap from genesis addresses plus the active validators in their *local* state. A brand-new node then needs at least one genesis validator to be reachable, or to start from a peer's copy of the chain.
- Simplex journals are kept for the current epoch and the two before it (older ones are deleted). Only the last 8 epochs' sets are kept in state, so `/api/sync` can only certify batches from recent epochs for nodes that are far behind.
- Rate limits are in-memory and per node, so restarting a node resets them, and each node in a pool counts separately. No proxy header is trusted by default.
- `join` fully backfills from genesis over HTTP and trusts no single peer (certificates are verified), but there's no snapshot sync yet. Full nodes don't serve `/api/sync` themselves.
- Behind NAT (e.g. Fly dedicated IPs), peers see the NATed source address. If the recommended preset rejects such peers, run with `--p2p-preset local`.
- The state root covers state *before* the block it's embedded in (deferred execution), so a block's own post-state root is in `blocks.jsonl`, not in the certified consensus block.
- If `consensus/` is ahead of `blocks.jsonl` (e.g. `blocks.jsonl` deleted), the node logs an error; delete `consensus/` to resync.
- The state root is a flat hash over all state, computed once per block (~10 ms per 100k accounts). Phase 1 moves to a Merkle tree for light-client proofs and incremental roots.
- Txs apply through an undo journal; nothing clones the state per tx. Mempool admission dry-runs a tx after its sender's queued txs (in nonce order, as a block runs them), so a tx that can't execute after them (behind a key removal, past a session limit) never takes a slot. Each recently active sender's simulation is kept (a structure-sharing copy of the state, rebuilt once per block), so admission costs one dry run per tx whatever the size of the mempool or of the queue.
- Failed actions are charged their fee. A failed tx keeps its place in the block, flagged in `failed`.
- Transfers that create a new account must carry at least 0.01 FANG.
- `exec_root` is still a best-effort check. Each block must cite a state root at most 16 heights behind the verifier's executed height, and verifiers that have executed that height check it. A root that no honest verifier has executed yet goes unchecked until one has.
- Storage:
  - Marshal keeps 2,000 heights of blocks and certificates and prunes the rest.
  - Every node keeps all consensus blocks (`cblocks.bin`) plus finalization certificates every 100 heights and at epoch ends (`certs.bin`), so it can serve verified `/api/sync` batches.
  - Only the last 2,048 executed blocks and 1,024 certificates stay in memory.
  - A node whose consensus storage predates this layout must re-sync (`consensus.version`).
- The faucet is rate limited per address and per client (/64 for IPv6), but a determined attacker with many IPs can still drain it slowly. It's for testnets only.
- `sdk-ts/src/passkey.ts` typechecks but hasn't been exercised in a real browser yet.
