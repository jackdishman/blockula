# 🧛 Blockula: the post-quantum blockchain that never dies

**What it is.** Blockula is a new layer-1 blockchain built so that the coming generation of quantum computers can't forge its signatures. It also aims to be the easiest chain to use, for people and AI agents alike. You sign in with a passkey (Face ID or Touch ID) instead of a seed phrase, pay `alice.boo` instead of `0x7f3a…`, and an agent can start using it from a single copy-paste prompt.

## Why now
- **Quantum computers threaten today's chains.** Bitcoin and Ethereum sign with elliptic curves (ECDSA, BLS). A large enough quantum computer breaks those with Shor's algorithm. Attackers can record transactions today and forge them later.
- **Crypto UX still loses people.** Seed phrases, hex addresses and unreadable explorers drive users away.
- **Agents need money rails with guardrails.** An agent should be able to pay for things on a budget its owner set, enforced by the chain rather than by trust.

## How it works
| Layer | Choice |
|---|---|
| Signatures | **ML-DSA-65** (NIST FIPS 204) for every account, validator vote and block certificate. No elliptic curves on-chain |
| Hashing | BLAKE3-256. Grover's algorithm only halves its strength, and 256-bit hashes keep a ~128-bit margin |
| Network | Validators authenticate each other with ML-DSA-65 and key their connections with hybrid X25519 + ML-KEM-768, so recorded traffic stays private even against a future quantum computer |
| Consensus | BFT proof-of-stake (Commonware *simplex*). Blocks are final in about a second, with no waiting for confirmations |
| Wallet | A **passkey** unlocks the wallet. Its PRF output derives the ML-DSA key, so the passkey's own non-quantum-safe signature is never used on-chain |
| Accounts | Multiple keys per account: **owner** keys (each device, plus recovery) and **session** keys for agents and apps, with a spend limit per period and an expiry, all enforced on-chain |
| Names | Native `.boo` names. Shorter names cost more, and the fees fund the treasury |
| Fees | Priced per byte (post-quantum signatures are ~3.3 KB). The base fee is **burned** |

## What's live today
- **Testnet** at `https://blockula-testnet.fly.dev`: 4 validators across the US and Europe, plus a public node with a built-in explorer that reads like plain English ("vlad.boo sent 5 FANG to alice.boo").
- **Web wallet** at `https://blockula-wallet.vercel.app`: passkey sign-up, a `.boo` name, send and receive, a tutorial checklist, linking devices, and giving an agent a daily budget.
- **`@blockula/sdk`** (TypeScript) and **`@blockula/mcp`** (for Claude Code or any MCP agent): `claude mcp add blockula -- npx -y @blockula/mcp` works with zero setup against the public testnet.
- **Run your own:**
  - `blockula dev` gives you a local chain in one command.
  - `blockula summon mychain --validators 4` creates your own network.
  - `blockula join` syncs a full node, with the explorer browsable while it syncs.
  - `blockula validator register` lets you apply to validate.
- **Proof of Contribution**, live on the testnet: bounties (**Hunts**) on GitHub issues, and a contribution pool for any merged PR. Validators check claims against GitHub; random commit-reveal juries (**Séances**) score them; a person's claims count best first (each next a quarter of the one before) and rewards vest with clawback; reputation (**Souls**) can't be bought. Agents take part as **Familiars** of a human sponsor.
- **Validators are added on-chain at epoch boundaries.** For Season 0 an admin allowlist approves them, and every approval is visible in the explorer.
- **Reviewed before launch.** An internal four-track security and product review covered consensus, network/ops, client and product. Its findings were fixed and turned into regression tests. A third-party audit comes before mainnet.

## Where it's going
1. **Proof of Contribution, next steps:**
   - Hunts with acceptance tests that validators re-run, so agents can be paid without anyone's sign-off.
   - Jury-run sybil flags and revert checks, replacing the Season 0 admin levers.
   - Covens (teams) and Seasons, plus a juror UI in the web wallet.
2. **👻 Ghost Mode and 🪦 The Crypt (Phase 2):** private transfers and encrypted storage, built on hash-based STARKs and ML-KEM, so the privacy layer is quantum-safe too. Viewing keys allow selective disclosure.
3. **Open validator set (Phase 3):** staking, slashing and governance (√stake + Soul voting weight, conviction voting, private ballots), then audits and mainnet.

**Token:** FANG (1 FANG = 10⁹ drops). Issuance tapers to a 1.5% tail emission, split 70% stakers / 20% contribution pool / 10% treasury. Base fees are burned.

**Status:** testnet, unaudited by a third party, test value only. Built in the open: start with `AGENTS.md` and `docs/PLAN.md`.
