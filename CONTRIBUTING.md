# Contributing to Blockula 🧛

Humans and agents are both welcome. Agents contribute as **Familiars** of a human sponsor, who is accountable for their work and receives their rewards (see "Proof of Contribution" in [docs/PLAN.md](docs/PLAN.md)).

> Testnet FANG, Hunts and Soul have no value.

## Hunts: bounties with acceptance criteria
- Open Hunts are GitHub issues labeled [`hunt`](https://github.com/jackdishman/blockula/issues?q=is%3Aopen+label%3Ahunt). Each has objective acceptance criteria and a testnet FANG bounty escrowed on-chain.
- `familiar-friendly` marks Hunts specified tightly enough for an agent to finish end to end.
- **To claim one:** comment on the issue, link your GitHub handle to your Blockula account (`link_github`, with a public gist containing your `tboo1…` address), and open a PR that references the issue and meets every acceptance criterion.
- **Payout:** when the PR merges, the Hunt's creator awards it on-chain (`blockula hunt award <id> --pr <n>` checks the merge on GitHub and pays the account you linked), and the explorer shows it on your profile.

## The contribution pool: any merged PR
Merged PRs to repos in the pool (Blockula's own is first) can earn testnet FANG even without a Hunt:
1. Link your GitHub handle, then claim the merged PR: `blockula contrib claim jackdishman/blockula <pr>`, `wallet.submitClaim(...)`, or the MCP's `submit_contribution`. It costs a small refundable bond and needs **Soul** or a **vouch** from someone who has it. (New here? Ask for a vouch with the [vouch form](https://github.com/jackdishman/blockula/issues/new?template=vouch-request.yml).)
2. Validators check GitHub: the PR must be merged into the default branch and authored by your linked handle. After a revert window, the next **Séance** judges it: a random jury compares claims in pairs.
3. Your claims are scored against other people's, and all your claims in one Séance share one win rate, pooled over all their comparisons: the first counts in full at that rate, each next one a quarter of the one before (all of them together less than 4/3 of one), so one solid PR beats a pile of tiny ones, weak claims pull your rate down, and claims a jury marks as spam lose their bond and your Soul. Rewards vest over ~90 days and can be clawed back if the PR is reverted.

People with Soul can serve on juries (`blockula contrib join-jury`), which earns Soul and a share of each Séance's payout when you judge in line with the outcome.

## Other ways to help
- **Bug reports** with a repro, through the [bug form](https://github.com/jackdishman/blockula/issues/new?template=bug.yml).
- **Run a node.** A full node of the public testnet takes one command (`blockula join`), and a testnet validator a few more ([docs/RUN-A-NODE.md](docs/RUN-A-NODE.md)). Ask for a testnet self-bond with the [node form](https://github.com/jackdishman/blockula/issues/new?template=node-operator.yml).
- **Fuzzing.** Report crashes privately ([SECURITY.md](SECURITY.md#fuzzing-findings)).

## What to run
You don't need Rust for web, SDK or MCP work: use the release binary (`curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh`) wherever a chain is needed.

| You changed | Run | Rust? |
|---|---|---|
| Docs only | Nothing to build: check that every link resolves | No |
| The wallet or explorer (`web/`) | `npm run build -w @blockula/sdk`, then `npm test -w @blockula/web`, `npm run build -w @blockula/web` and `npm run test:e2e -w @blockula/web` (Playwright, against a mocked node) | No |
| The SDK (`sdk-ts/`) | `npm run build -w @blockula/sdk`, `npm run test:kat -w @blockula/sdk`, `npm run test:vectors -w @blockula/sdk`, then the e2e: `blockula dev` in one terminal and `npm test -w @blockula/sdk` in another (`BLOCKULA_RPC` points it at another node) | No |
| The MCP server (`mcp/`) | `npm run build -w @blockula/sdk`, then `npm test -w @blockula/mcp` (offline) | No |
| SDK or MCP, end to end | `SKIP_BUILD=1 BIN=$(command -v blockula) bash scripts/local-testnet.sh`, with a release of the version `main` is on: 4 validators, a full node and the SDK e2e | No |
| The node (`node/`) | `cargo test --release --locked --manifest-path node/Cargo.toml`, the v1 replay gate (`for a in node/testdata/replay/v1-*; do node/target/release/blockula replay "$a"; done`, [docs/REPLAY.md](docs/REPLAY.md)) and `bash scripts/local-testnet.sh` | Yes |
| The tx encoding | Both sides (`node/src/types.rs` and `sdk-ts/src/tx.ts`) must match byte for byte: the node's tests, the SDK's vectors and the e2e | Yes |
| The explorer art (`web/lib/ascii`) | `npm run build:haunt`, and commit `node/src/haunt.js` | No |

Root `npm test` runs a debug `cargo test` and the SDK e2e, which needs `blockula dev` already running on port 7070.

**Copy.** Short sentences in plain English. Say "programs", never "contracts". Give amounts as strings in FANG. Never quote an APR, APY or yield, a reward rate or a return, and never write about FANG's value, a listing or an airdrop.

## What a good PR looks like
- One focused change, with tests. `cargo test --manifest-path node/Cargo.toml` and `bash scripts/local-testnet.sh` pass (CI runs both).
- The Rust and TypeScript transaction encodings stay byte-identical (`node/src/types.rs` ⇄ `sdk-ts/src/tx.ts`).
- Explain what changed and how you verified it.
- Volume barely pays: a sponsor's claims in a Séance share one pooled win rate, each after the first counts a quarter of the one before (so in expectation more claims of the same quality add at most a third), and rewards vest with clawback. One solid PR beats ten small ones.

## How PRs merge
- Everything reaches `main` through a PR: a maintainer's approval, both CI jobs green, review threads resolved, and the branch up to date with `main`. Pushing new commits after an approval needs a fresh one.
- PRs are squash-merged, so each one becomes a single commit on `main` titled `<PR title> (#n)`. That commit is the merge SHA validators attest to for your claim, and what gets reverted if the change is backed out.
- `main`'s history is never rewritten (no force pushes), so a merge SHA stays valid for the whole revert window.
- CI for a first-time contributor's PR waits for a maintainer to approve the run. Leave "Allow edits by maintainers" on so we can update your branch.

## Conduct and help
This project follows a [code of conduct](CODE_OF_CONDUCT.md). [SUPPORT.md](SUPPORT.md) says where to ask what.

## Security
Please report vulnerabilities privately via GitHub Security Advisories on this repo rather than in a public issue. See [SECURITY.md](SECURITY.md) for scope and what to expect.
