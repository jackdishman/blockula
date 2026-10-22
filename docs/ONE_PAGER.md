# 🧛 Blockula: launch your own quantum-safe ledger

**What it is.** Blockula is a kit for running your own ledger: one command starts a chain, and your friends' computers join it, with no hosting to pay for and nothing to buy ([docs/LAUNCH.md](LAUNCH.md)). Its signatures are built so that the coming generation of quantum computers can't forge them. It also aims to be the easiest chain to use, for people and AI agents alike. You sign in with a passkey (Face ID or Touch ID) instead of a seed phrase, pay `alice.boo` instead of `0x7f3a…`, and an agent can start using it from a single copy-paste prompt.

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
| Consensus | BFT proof-of-stake (Commonware *simplex*). A block is final once it's made, with no waiting for confirmations: every 5 seconds on chains you launch (lighter on home connections), every second on the public testnet |
| Wallet | A **passkey** unlocks the wallet. Its PRF output derives the ML-DSA key, so the passkey's own non-quantum-safe signature is never used on-chain |
| Accounts | Multiple keys per account: **owner** keys (each device, plus recovery) and **session** keys for agents and apps, with a spend limit per period and an expiry, all enforced on-chain |
| Names | Native `.boo` names. Shorter names cost more, and the fees fund the treasury |
| Fees | Priced per byte (post-quantum signatures are ~3.3 KB). The base fee is **burned** |

## What's live today
- **Testnet** at `https://blockula-testnet.fly.dev`: 4 validators across the US and Europe, plus a public node with a built-in explorer that reads like plain English ("vlad.boo sent 5 FANG to alice.boo").
- **Web wallet** at `https://testnet.blockula.app`: passkey sign-up, a `.boo` name, send and receive, a tutorial checklist, linking devices, and giving an agent a daily budget.
- **`@blockula/sdk`** (TypeScript) and **`@blockula/mcp`** (for Claude Code or any MCP agent): `claude mcp add blockula -- npx -y @blockula/mcp` works with zero setup against the public testnet.
- **Run your own:**
  - `blockula dev` gives you a local chain in one command.
  - `blockula summon mychain --host <your address>` creates your own network, and friends' machines reach it (a Tailscale address works).
  - `blockula join` syncs a full node, with the explorer browsable while it syncs.
  - `blockula validator register` registers a validator with a bond, and the chain's admin approves it.
  - The whole flow, step by step: [docs/LAUNCH.md](LAUNCH.md).
- **Proof of Contribution**, live on the testnet: bounties (**Hunts**) on GitHub issues, and a contribution pool for any merged PR. Validators check claims against GitHub; random commit-reveal juries (**Séances**) score them; a person's claims share one pooled win rate (each after the first counting a quarter of the one before) and rewards vest with clawback; reputation (**Souls**) can't be bought. Agents take part as **Familiars** of a human sponsor.
- **Validators are permissionless.** Anyone can register one with a self-bond, and at each epoch boundary the top validators by stake make the set (newcomers at most a third of it). The testnet has no admin key: nobody approves a validator, and every registration is visible in the explorer.
- **Reviewed before launch.** An internal four-track security and product review covered consensus, network/ops, client and product. Its findings were fixed and turned into regression tests. There's no third-party audit.

## Where it's going
1. **Proof of Contribution, next steps:**
   - Hunts with acceptance tests that validators re-run, so agents can be paid without anyone's sign-off.
   - Jury-run sybil flags, replacing the governance vote that flags a sybil today.
   - Covens (teams) and Seasons, plus a juror UI in the web wallet.
2. **👻 Ghost Mode and 🪦 The Crypt (Phase 2):** private transfers and encrypted storage, built on hash-based STARKs and ML-KEM, so the privacy layer is quantum-safe too. Viewing keys allow selective disclosure.
3. **Lighter launches:** blocks only when there are transactions, a guided launch on the web, and an optional mode without a unit at all. **Programs** (small WebAssembly programs that run on the chain) are built into the node and switched off, as an option for later. There's no mainnet: Blockula is software for running your own ledger.

**Unit:** FANG (1 FANG = 10⁹ drops), each chain's own. It pays fees (the base fee is burned), bonds and names, and a chain's faucet hands it out. It has no value: nobody sells it, buys it or gets paid in it.

**Status:** software and a public testnet, unaudited by a third party, no value anywhere. Built in the open: start with `AGENTS.md` and `docs/PLAN.md`.
