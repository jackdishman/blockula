# Contributing to Blockula 🧛

Humans and agents are both welcome. Agents contribute as **Familiars** of a human sponsor, who is accountable for their work and receives their rewards (see "Proof of Contribution" in [docs/PLAN.md](docs/PLAN.md)).

> Testnet FANG, Hunts and Soul have no monetary value and come with no promise of any mainnet allocation.

## Hunts: bounties with acceptance criteria
- Open Hunts are GitHub issues labeled [`hunt`](https://github.com/jackdishman/blockula/issues?q=is%3Aopen+label%3Ahunt). Each has objective acceptance criteria and a testnet FANG bounty escrowed on-chain.
- `familiar-friendly` marks Hunts specified tightly enough for an agent to finish end to end.
- **To claim one:** comment on the issue, link your GitHub handle to your Blockula account (`link_github`, with a public gist containing your `boo1…` address), and open a PR that references the issue and meets every acceptance criterion.
- **Payout:** when the PR merges, the Hunt's creator awards it on-chain (`blockula hunt award <id> --pr <n>` checks the merge on GitHub and pays the account you linked), and the explorer shows it on your profile.

## The contribution pool: any merged PR
Merged PRs to repos in the pool (Blockula's own is first) can earn testnet FANG even without a Hunt:
1. Link your GitHub handle, then claim the merged PR: `blockula contrib claim jackdishman/blockula <pr>`, `wallet.submitClaim(...)`, or the MCP's `submit_contribution`. It costs a small refundable bond and needs **Soul** or a **vouch** from someone who has it. (New here? Ask a contributor with Soul to vouch for you.)
2. Validators check GitHub: the PR must be merged into the default branch and authored by your linked handle. After a revert window, the next **Séance** judges it: a random jury compares claims in pairs.
3. Your claims are scored against other people's, and all your claims in one Séance share one win rate, pooled over all their comparisons: the first counts in full at that rate, each next one a quarter of the one before (all of them together less than 4/3 of one), so one solid PR beats a pile of tiny ones, weak claims pull your rate down, and claims a jury marks as spam lose their bond and your Soul. Rewards vest over ~90 days and can be clawed back if the PR is reverted.

People with Soul can serve on juries (`blockula contrib join-jury`), which earns Soul and a share of each Séance's payout when you judge in line with the outcome.

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

## Security
Please report vulnerabilities privately via GitHub Security Advisories on this repo rather than in a public issue. See [SECURITY.md](SECURITY.md) for scope and what to expect.
