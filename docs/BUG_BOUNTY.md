# Blockula bug bounty

Blockula pays for vulnerabilities in the code that will secure its mainnet. This program closes the bounty half of SEC-5 in [MAINNET.md](MAINNET.md): it must be funded and live on the incentivized testnet for at least a month before genesis.

> **Status: draft.** Reward amounts are placeholders until the maintainer confirms the budget, and the program opens with the incentivized testnet. Until then, report as in [SECURITY.md](../SECURITY.md); valid reports made before launch are rewarded under this table once it's confirmed.

## How to report

Privately, through GitHub: **[open a security advisory](https://github.com/jackdishman/blockula/security/advisories/new)**. Never in a public issue, PR, chat or social post.

A good report has: the affected component and commit or release; the impact you expect; a **deterministic reproduction** (a failing test, a script against `blockula dev` or `scripts/local-testnet.sh`, or a simulator scenario); and, if you have one, a suggested fix. One vulnerability per report. Reports that chain several bugs are welcome as one report when the chain is the impact.

## Scope

**In scope**

| Component | Where |
|---|---|
| Node: consensus integration, block verification and execution, the state machine (transfers, names, accounts and session keys, Hunts, the contribution pool, staking and slashing, governance, fees and issuance), p2p, sync and snapshots, the HTTP API and explorer, the CLI | `node/` |
| Cryptography and its use: ML-DSA-65 signing and verification, the certificate scheme plugged into Commonware (`pq.rs`), address and key-id derivation, the Rust ⇄ TypeScript tx encoding, passkey key derivation | `node/src`, `sdk-ts/src` |
| `@blockula/sdk`, the passkey web wallet, `@blockula/mcp` | `sdk-ts/`, `web/`, `mcp/` |
| The supply chain we control: release workflows, `scripts/install.sh`, the release signature, `deploy/` | `.github/`, `scripts/`, `deploy/` |
| Ghost Mode, once its activation proposal is live (separate tier, announced then) | `node/src/ghost/` |

**Out of scope**

- Attacks that need ⅓ or more of the active validator set's votes, or control of the release key, or of a maintainer's GitHub or npm account (report weaknesses in how we protect those, though).
- Volumetric DoS (flooding with bandwidth or requests) against the public testnet or anyone's infrastructure. A cheap, asymmetric DoS (one small tx or message that halts or slows nodes) is in scope.
- Third-party services and their configuration: Fly.io, Vercel, GitHub, npm, DNS providers. Bugs in dependencies (Commonware, `ml-dsa`, `@noble/post-quantum`, …) go to their maintainers; they're in scope here only with a working exploit against Blockula.
- Known issues listed in [MAINNET.md](MAINNET.md) and [PLAN.md "Known limits"](PLAN.md#known-limits), unless you show a concrete exploit that's worse than described.
- The p2p transport using X25519 key exchange (CRY-1, documented). A practical classical break of the handshake is in scope.
- Social engineering, phishing, physical attacks; reports from automated scanners without a demonstrated impact; missing best-practice headers without an exploit; testnet faucet abuse; UI and typo issues.

## Severity and rewards

Severity is impact first, then how hard it is to pull off. The maintainers decide, explain their reasoning, and pay the tier's range according to the quality of the report and the likelihood. Amounts are in US dollars, paid in fiat or USDC (not FANG).

| Severity | Examples | Reward (placeholder, to confirm) |
|---|---|---|
| **Critical** | Two conflicting finalized blocks (safety failure). Minting FANG, or moving anyone's funds or escrow, without their key. Forging an ML-DSA signature or a certificate as the node checks it. Halting or splitting the chain with resources far below ⅓ of the set (e.g. one tx or one p2p message). Remote code execution on a node. Extracting a passkey wallet's key. A soundness bug in Ghost Mode. | $50,000 – $250,000 |
| **High** | Nodes disagreeing on state for the same block. Permanently freezing funds. Bypassing a session key's limit or role. Slashing an honest validator, or escaping slashing for equivocation. Fast sync accepting a forged state or validator set. A release, install or update path that runs an unverified binary despite `BLOCKULA_REQUIRE_VERIFY=1`. Governance or the contribution pool paying out without the votes or verification the rules require. | $10,000 – $50,000 |
| **Medium** | Stalling one node, or delaying votes, with modest resources (e.g. an expensive HTTP or p2p request). Temporarily freezing funds. Griefing that costs others fees or bonds. Bypassing rate limits at scale. Leaking private data the API shouldn't expose. | $2,000 – $10,000 |
| **Low** | Bugs with a security angle but little impact: misleading tx summaries that could trick a careful user, minor information leaks, hardening gaps with a plausible path to something worse. | $250 – $2,000 |

Same root cause, one reward: duplicates of a report we already have earn nothing, and the first complete report wins. If a fix for your report is incomplete, the follow-up is a new report.

## Rules

- **Testnet first.** Do your testing on a local chain (`blockula dev`, `scripts/local-testnet.sh`) or a private testnet of your own. On the public testnet, use only your own accounts, keep load low, and stop as soon as you've shown the problem: no deliberate halts, no flooding, no touching other people's accounts or data.
- **Never test against mainnet** once it exists. Reproduce on a local chain started from a copy of mainnet state instead.
- Don't keep or use anything you got through a bug beyond what's needed to show it; return funds and delete data. Don't disclose before the timeline below.
- You can't be rewarded if you're a current or recent (last 6 months) Blockula maintainer, contractor or auditor of the affected code, if you introduced the bug, or where the law forbids payment (sanctions). We may need identity and tax information before paying, as the law requires.

## Safe harbor

If you follow this policy in good faith, we consider your research authorized, we won't pursue or support legal action against you for it (including under anti-hacking and anti-circumvention laws), and we'll say so publicly if a third party does. If in doubt about whether something is allowed, ask through a security advisory first. This doesn't authorize anything against third parties' systems.

## Timeline

These are targets, as in SECURITY.md:

| | |
|---|---|
| Acknowledgement | 3 business days |
| Triage (severity, reproduced or not) | 10 business days |
| Fix | Critical: as fast as a patched release can reach ⅔ of the validators ([OPERATIONS.md "Patch distribution"](OPERATIONS.md#patch-distribution)). High: within 30 days. Others: the next release. |
| Reward | Within 30 days of the fix being released |
| Disclosure | Coordinated: once the fix is deployed on ≥ ⅔ of the active set, or 90 days after the report, whichever is first, unless we agree otherwise. You're credited in the advisory if you want to be. |

## Running the program (maintainers)

- **Budget:** fund the pool before opening (the top critical payout at least, held in a dedicated account) and publish its size.
- **Platform:** self-run through GitHub advisories, or a bounty platform (Immunefi, HackerOne). A platform adds triage help and payment handling; decide before the incentivized testnet.
- **Triage rota:** two maintainers, one the security lead. Every report gets a severity rationale in writing.
- **Launch gate (SEC-5):** live on the incentivized testnet for ≥ 1 month before genesis, with the confirmed amounts in this file.
