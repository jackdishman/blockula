# Security policy

Blockula is an unaudited testnet: it has had an internal review but no third-party audit yet (see the README). Nothing on it has real value, but the code is meant to secure a mainnet one day, so we want to hear about every vulnerability now.

## Reporting

Report privately through GitHub: **[open a security advisory](https://github.com/jackdishman/blockula/security/advisories/new)**. Please don't open a public issue or PR for a vulnerability.

Include what you found, how to reproduce it (a deterministic repro, such as a failing test or a script, is ideal), which version or commit, and the impact you expect.

## Scope

- The node (`node/`): consensus integration, block and tx validation, state transitions (`chain.rs`, `poc.rs`), p2p, sync and snapshots, the HTTP API and explorer, the CLI.
- Cryptography and its use: ML-DSA-65 signing and verification, address and key-id derivation, the Rust ⇄ TypeScript tx encoding (`node/src/types.rs` ⇄ `sdk-ts/src/tx.ts`), passkey key derivation (`sdk-ts/src/passkey.ts`).
- The contribution machinery: GitHub links, claims and validator attestations, Séances, payouts and vesting.
- `@blockula/sdk`, the passkey web wallet (`web/`), `@blockula/mcp`, `scripts/install.sh`, the release workflows and signature, and `deploy/`.

Especially wanted: anything that halts or splits the chain, mints or moves FANG without authorization, bypasses a session key's limits, forges a signature or certificate, or makes nodes disagree on state.

Known limitations listed in [docs/PLAN.md](docs/PLAN.md#known-limits) and [docs/MAINNET.md](docs/MAINNET.md) are known, but a concrete exploit of one is still welcome.

## Fuzzing findings

The node's decoders are fuzzed nightly in this public repository's CI ([node/fuzz](node/fuzz/README.md)). A crash the fuzzers find is handled exactly like a private report: a private security advisory, fixed in its private fork, disclosed only after the fix has shipped. The job's log says only which target crashed; the crash input and the fuzzer's output leave the runner only encrypted to the maintainers' keys, and open findings' inputs are never committed. If you fuzz Blockula yourself and find a crash, report it here the same way, with the input attached to the advisory, not in an issue or PR.

## What to expect

These are targets, not guarantees:
- Acknowledgement within 3 business days.
- An assessment (severity, whether we can reproduce it) within 10 business days.
- A fix for critical and high issues as fast as we can ship one, then public credit in the advisory if you want it.

## Bug bounty

[docs/BUG_BOUNTY.md](docs/BUG_BOUNTY.md) has the scope, severity tiers, rewards, rules and safe harbor. It opens, funded, on the incentivized testnet at least a month before mainnet; until the amounts are confirmed there, valid reports are rewarded under its table once it is.

## Audits

No third-party audit yet. The scope for the external audit of the mainnet code (SEC-1) is [docs/AUDIT_SCOPE.md](docs/AUDIT_SCOPE.md); reports will be published here when they're done.

## Verifying releases

Release archives carry SLSA build provenance, the Linux builds are reproducible, and `checksums.txt` is signed offline with the Blockula release key (ML-DSA-65). How to check all three: [docs/OPERATIONS.md "Releases"](docs/OPERATIONS.md#releases). `blockula release install vX.Y.Z` installs a release only if its signature checks against the release key id you pinned at your first install (`~/.blockula/release-key-id`), and a new key id is accepted only with a rotation statement signed by the pinned key. Once the key exists, a `blockula` binary also knows it, and won't pin any other key id you give it. The MCP's `install_node` checks against the same pin. This file is in a repository a compromised GitHub account could edit, so take the key id from here and from a channel GitHub doesn't control (the website, the operators' channel), check they agree, and pin it once. Release key id: *not generated yet*; it will be published here, in OPERATIONS.md, in `scripts/install.sh`, in `node/src/release_cli.rs` and in `mcp/src/release.ts`.

## Testing on the public testnet

Good-faith research is welcome. Please:
- Prefer a local chain (`blockula dev`, or `scripts/local-testnet.sh` for 4 validators) for anything disruptive.
- Don't degrade the public testnet for others (no flooding, sustained load or deliberate halts), and don't touch other people's accounts or data.
- Give us a reasonable chance to fix an issue before disclosing it.
