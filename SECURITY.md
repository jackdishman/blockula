# Security policy

Blockula is an unaudited testnet. Nothing on it has real value, but the code is meant to secure a mainnet one day, so we want to hear about every vulnerability now.

## Reporting

Report privately through GitHub: **[open a security advisory](https://github.com/jackdishman/blockula/security/advisories/new)**. Please don't open a public issue or PR for a vulnerability.

Include what you found, how to reproduce it (a failing test or a script is ideal), which version or commit, and the impact you expect.

## Scope

- The node (`node/`): consensus integration, block and tx validation, the state machine (`chain.rs`, `poc.rs`), sync and snapshots, the HTTP API and explorer, the CLI.
- Cryptography and its use: ML-DSA-65 signing and verification, address and key-id derivation, the Rust ⇄ TypeScript signing encoding (`node/src/types.rs` ⇄ `sdk-ts/src/tx.ts`), passkey key derivation (`sdk-ts/src/passkey.ts`).
- `@blockula/sdk`, `@blockula/mcp`, the web wallet (`web/`), `scripts/install.sh`, release workflows and `deploy/`.

Especially wanted: anything that halts or splits the chain, mints or moves FANG without authorization, bypasses a session key's limits, forges a signature or certificate, or makes nodes disagree on state.

Known limitations listed in [docs/PLAN.md](docs/PLAN.md#known-limits) and [docs/MAINNET.md](docs/MAINNET.md) are known, but a concrete exploit of one is still welcome.

## What to expect

These are targets, not guarantees:
- Acknowledgement within 3 business days.
- An assessment (severity, whether we can reproduce it) within 10 business days.
- A fix for critical and high issues as fast as we can ship one, then public credit in the advisory if you want it.

There's no paid bug bounty yet; one will open on the incentivized testnet before mainnet.

## Testing on the public testnet

Good-faith research is welcome. Please:
- Prefer a local chain (`blockula dev`, or `scripts/local-testnet.sh` for 4 validators) for anything disruptive.
- Don't degrade the public testnet for others (no flooding, sustained load or deliberate halts), and don't touch other people's accounts or data.
- Give us a reasonable chance to fix an issue before disclosing it.
