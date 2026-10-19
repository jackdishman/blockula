# Testnet v1 and mainnet launch gates

This is an execution plan, not a mainnet launch announcement. The public testnet still runs
v0.4.0 as of October 17, 2026. The v0.5.0 preparation and final internal-review fixes must pass
review and CI before deployment. Testnet FANG, Hunts and Soul have no monetary value and carry
no promise of a mainnet allocation.

## Relaunch the public testnet

1. Merge the explorer alignment, final audit fixes and relaunch preparation after the nine
   required CI checks pass and the code owner reviews them. Verify the combined tree, including
   the local multi-validator testnet, wallet browser tests and `scripts/rehearse-testnet.sh`.
2. Back up the existing testnet keys and confirm the five target apps in `deploy/launch.sh`:
   `blockula-testnet` and `blockula-v0` through `blockula-v3`. Run `RESET=1 deploy/launch.sh`
   once, following [the relaunch runbook](../deploy/README.md#relaunch-replacing-blockula-testnet-with-blockula-testnet-2).
   This destroys the old testnet's machines, volumes and balances. Keep the new keys private;
   subsequent deployments run without `RESET`.
3. Confirm `blockula-testnet-2`, prefix `tboo`, the new genesis hash, version 0.5.0, advancing
   finalized height and four active validators. Check matching finalized block hashes and roots,
   the faucet, fees, names, contribution-pool configuration and the first epoch transition.
   The built-in explorer injection fix ships in this deployment.
4. Pin the new genesis hash in Vercel's `NEXT_PUBLIC_TESTNET_GENESIS_HASH` and redeploy the wallet.
   Keep its production origin stable so existing passkeys remain usable. Verify an actual
   testnet payment and its receipt against the node. Old balances do not carry over.
5. Use `scripts/pin-network.sh` with the public genesis and a recent timed checkpoint to prepare
   the release commit. The node, MCP and wallet must agree on the chain and genesis; the MCP and
   wallet also pin that genesis's clock. Commit only public genesis inputs, never wallet or
   validator keys. Review and test this commit before tagging v0.5.0.
6. Follow [the release runbook](OPERATIONS.md#releases) to build and publish the release. Before
   the offline release-key ceremony, describe the testnet release as unsigned; do not claim a
   signature exists. Redeploy the nodes from the reviewed release commit without resetting.
7. Run `npm-publish.yml` from the release tag, first as a dry run, then for publication. Publish
   the SDK before the MCP; verify package integrity, provenance, the MCP shrinkwrap and a clean
   `npx @blockula/mcp@0.5.0` connection to the pinned new testnet. Each npm package needs its own
   trusted-publisher entry for owner `jackdishman`, repository `blockula`, workflow
   `npm-publish.yml`, environment `npm`. Account 2FA alone does not configure that trust.
8. Publish the internal-review report after the live explorer fix is verified. Describe its AI
   authorship and residual risks accurately. An internal review does not close the external-audit
   gate below. Configure validators' read-only GitHub token for sustained contribution checks;
   `deploy/set-github-token.sh` accepts it through stdin.

Treat successful deployment as the start of observation. Monitor finality, missed slots, jails,
memory/disk growth, API health, and alert delivery. Rehearse a restart, late public-node join,
validator recovery and a scheduled protocol upgrade. Record incidents and the exact release
commit; repeated manual intervention means the testnet has not met the stability gate.

## Mainnet gates after testnet stabilization

The detailed requirements remain in [MAINNET.md](MAINNET.md). These gates require evidence;
passing the local suite or operating four validators in one Fly.io organization cannot replace it.

| Gate | Evidence required before genesis |
|---|---|
| Frozen protocol | Reviewed transaction/state/genesis formats and economics; release pinned; no unresolved critical or high findings. Substantial consensus changes restart the affected audit and test period. |
| External security review | Independent protocol and wallet/SDK/MCP audits, fixes reverified and reports published. Explicit review of post-quantum crypto integration and upstream library risk. Ghost Mode remains off until its separate gates pass. |
| Independent operators | Recruit the genesis set and satisfy the builder's operator/provider/ASN/facility/entity independence policy. Consensus seats remain equal-weight and selected by stake; they are not stake-weighted quorums. Operators generate and control their own keys. |
| Sustained public testing | At least three months with independent operators, including 30 consecutive days without a halt or manual intervention; two operator-led upgrades, fault injection, mainnet-scale load, external checkpoint sync and a response exercise. See MAINNET.md section 5. |
| Operations and release trust | Working alerts, recovery and double-signing procedures; recent independently checked checkpoints; two reproducible builders; offline signing-key ceremony and independently distributed release-key pin. |
| Wallet and legal readiness | A project-owned permanent passkey domain; verified recovery and network pins; counsel's review of allocation, distribution, contribution rewards and on-chain name/data policy. Permissionlessness alone does not settle token classification. |
| Genesis rehearsal | Final allocation and vesting inputs; no faucet or Season 0 admin powers; independent rebuilds agree on genesis and binary hashes; all intended operators complete a rehearsal on a distinct throwaway chain id. |

Set a launch date only after these gates have owners, evidence and no outstanding blockers. The
original assessment's engineering estimates are historical estimates, not a revised commitment.

## Third-party programs

Third-party, Turing-complete **programs** are a protocol-v2 feature. They're designed but not
built: nobody can deploy one on v0.5.x. [PROGRAMS.md](PROGRAMS.md) is the design and
[PROGRAMS-PLAN.md](PROGRAMS-PLAN.md) the implementation plan. Programs are WebAssembly, run by a
fuel-metered interpreter and written in Rust. The engine defaults to soroban-wasmi 29.x, and
phase 0 measures it beside wasmi 2.0 before the exact pin is chosen. The maintainer's decisions
of 2026-10-19 (PROGRAMS-PLAN.md section 3) set how they ship:

- **On main, behind version gates.** Below protocol v2 a new binary behaves exactly as v1, and
  until the release that ships programs it supports v2 only on dev chains. testnet-2 gets them
  as a scheduled upgrade to v2, then `ActivateFeature { programs }`.
- **Live on mainnet as soon as they're ready (D2).** Ready means three gates: the consensus
  formats are frozen, the audit has closed with no open critical or high finding, and testnet-2
  has run programs for at least eight weeks. If those close before mainnet's genesis, programs
  are on from block 1. Otherwise mainnet launches with them off, and the council proposes their
  activation the day the last gate closes; it takes effect about nine days later. (The bootstrap
  council can pass it in mainnet's first year; after that, activation takes the ⅔ token vote.)
  Programs aren't a mainnet gate: the launch date doesn't wait for them.
- **Reviewed by AI agents, not externally audited (D3).** Blockula is an experiment. Claude
  audits programs with Fable agents: a review of every consensus PR, multi-agent audits at three
  milestones and a full audit after the format freeze. A finding counts only once a second agent
  reproduces it with a failing test, and the reports are published. No external firm reviews the
  VM, the engine or their integration. Of programs, the external protocol audit above sees only
  the transaction codec for tags 62–70, which the mainnet binary carries even while programs are
  off. Describe programs as "reviewed by AI agents, not externally audited" wherever they're
  offered, as Ghost Mode is marked "NOT AUDITED".
- **The training wheels stay,** and matter more without an external audit: a 10M FANG cap on
  what programs hold, a validators' pause that lapses after three days, exit-only when governance
  switches programs off, and a tokens-only freeze. The bug bounty is to cover the VM from
  testnet-2's activation, and Hunts are to pay for breaking programs there.

Until programs are live, don't advertise deployable third-party programs or promise them by a
date. The plan's calendar (formats frozen about May 2027, ready for mainnet about mid-August
2027) is an estimate, not a commitment.
