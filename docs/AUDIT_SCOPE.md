# Blockula security audit: scope and request for proposals

Blockula is a post-quantum layer-1 blockchain: ML-DSA-65 (FIPS 204) signatures for every transaction, vote and certificate, BLAKE3 hashing, and Commonware's simplex BFT consensus with an ML-DSA certificate scheme we wrote. This document is the scope for SEC-1 in [MAINNET.md](MAINNET.md): the external audit of the frozen genesis commit, followed by fixes and a re-review. Send it to firms as the request for proposals.

The protocol being audited is specified in [MAINNET-DESIGN.md](MAINNET-DESIGN.md) ("protocol v1"). Background: [PLAN.md](PLAN.md) (architecture), [MAINNET.md](MAINNET.md) (the known gaps, with code citations), [OPERATIONS.md](OPERATIONS.md) (how it's run), [GENESIS_CEREMONY.md](GENESIS_CEREMONY.md).

## Engagements

| | Engagement A: protocol | Engagement B: clients |
|---|---|---|
| What | The node: consensus integration, block validity, the state machine, staking and slashing, governance, economics, sync, p2p admission, the HTTP API's effect on consensus, upgrades and halts, cryptographic use | The TypeScript SDK (signing, encoding, passkey key derivation), the passkey web wallet (origin security, WAL-3), the MCP server (agent wallets, spending caps) |
| Language | Rust (edition 2024) | TypeScript (Node ≥ 20, Next.js) |
| Size | see below | see below |
| Skills | BFT consensus, Rust, state machines with economic invariants, lattice signatures in practice | Web security, WebAuthn/passkeys, key handling in browsers, supply chain |
| Calendar | 6–10 weeks including fixes and re-review | 2–4 weeks |

One firm may bid on both. Ghost Mode (the shielded pool, DEC-2) gets its own audit before it's activated and is **not** in this scope.

## Commit and size

The audit runs on the **consensus-format freeze commit** (MAINNET.md section 5, the end of phase 1), which we'll tag `audit-1` and share at kickoff. Until then, the sizes below are a snapshot of `jackdishman/mainnet-prep` at `4309382` (before the protocol v1 packages land), counted with `wc -l` including inline tests; rerun at the freeze.

**Node (`node/src`), 10,875 lines today:**

| File | Lines | What | Priority |
|---|---|---|---|
| `chain.rs` | 2,603 | State, execution, mempool, admission, the state root, persistence | P0 |
| `poc.rs` | 1,843 | Proof of Contribution: claims, attestations, Séances (juries), payouts, vesting | P0 |
| `api.rs` | 1,294 | HTTP API, tx submission and forwarding, sync batches, metrics | P1 |
| `consensus.rs` | 935 | Commonware integration: block format (`CBlock`), propose, **verify**, execution acknowledgement, checkpoint rule, marshal and simplex wiring | P0 |
| `types.rs` | 798 | Tx and action types, **signing bytes**, format checks, fees | P0 |
| `node.rs` | 665 | Node startup, join and fast sync, double-sign guards, p2p config | P1 |
| `contrib_cli.rs`, `validator_cli.rs`, `main.rs` | 1,199 | CLIs | P2 |
| `sim.rs` | 408 | Deterministic simulator (test-only) | context |
| `pq.rs` | 324 | **The ML-DSA-65 certificate scheme** implementing Commonware's `Scheme` | P0 |
| `snapshot.rs` | 281 | Certified state snapshots and their verification | P1 |
| `epochs.rs` | 228 | Validator-set history, epoch handoffs | P1 |
| `github.rs` | 168 | GitHub checks for the contribution pool (an oracle) | P1 |
| `crypto.rs` | 129 | ML-DSA keys, addresses (bech32m of BLAKE3) | P0 |
| `explorer.html` | 454 | The block explorer, served by the node | P2 |

Protocol v1 adds, in new modules (estimates): staking, slashing and evidence (~2,000), governance, treasury and multisig (~1,500), fees, issuance, vesting and the genesis builder (~1,500), the randomness beacon (~800), upgrades, halts and slashing protection (~800), and the authenticated state store with proofs (~3,000). Expect **~20,000–25,000 lines of Rust** at the freeze, of which ~12,000 are P0.

**Clients, ~3,100 lines today:** `sdk-ts/src` 686 (`tx.ts` 253, `index.ts` 362, `passkey.ts` 71), `mcp/src` 327, `web/` ~2,100 (Next.js pages and components).

**Supply chain (review only, light):** `.github/workflows/`, `scripts/install.sh`, `scripts/release-build.sh`, `scripts/release-sig.mjs`, `scripts/check-release-files.sh`, `node/src/release_cli.rs` (`blockula release install|fetch|pin`, the operators' install path), `deploy/`, ~1,900 lines.

**Dependencies in scope for their use, not their internals:** Commonware `=2026.9.0` (`consensus::simplex` and `marshal` (BETA; the `Application` trait and `marshal::application` are ALPHA), `p2p::authenticated::discovery`, `stream`, `storage` journals and archives, `cryptography`, `codec`, `runtime`); `ml-dsa 0.1.1` (RustCrypto, self-described as never independently audited); `@noble/post-quantum 0.7.1` (self-audited); `blake3`, `bech32`. We'd like the firm's view on whether the Commonware modules we use are covered by Commonware's own audits, and on our use of both ML-DSA implementations (SEC-2).

## Threat model

**Assets:** FANG balances, escrows (Hunts, bonds, stakes, the contribution pool, the treasury); the integrity of names and accounts; consensus safety (one finalized chain) and liveness; validator, owner and user keys; the outcome of governance votes and juries.

**Trust assumptions:**
- Fewer than ⅓ of the active validators (by count: simplex quorums are count-based, DEC-3) are Byzantine in any epoch.
- At least ⅔ of the active set upgrades honestly when an upgrade is scheduled.
- ML-DSA-65 and BLAKE3 are secure. The p2p session key exchange is X25519 (classical) until CRY-1; a quantum adversary recording traffic today is a known, documented risk, not a finding.
- GitHub is honest about merges for the contribution pool (it's an oracle; a manipulated GitHub is in the threat model only through what validators attest).

**Adversaries:**
1. **Byzantine validators** (< ⅓): equivocating; proposing invalid, junk-filled or unexecutable blocks; withholding votes; grinding leader slots or the randomness beacon; colluding in juries.
2. **Any tx sender:** malformed or adversarial txs, fee and mempool games, replay across chains or after expiry, exploiting accounting edge cases, session-key limit bypasses.
3. **Network peers and sync sources:** malformed p2p messages, forged certificates or snapshots, eclipse attempts, an attacker serving `/api/sync` or snapshots to a joining node.
4. **Anyone on the internet:** HTTP requests that stall consensus through the API (SEC-4), resource exhaustion.
5. **A malicious RPC node** serving wallets and agents (WAL-4); a malicious web origin or script attacking the passkey wallet (WAL-3).
6. **A supply-chain attacker** controlling a dependency release, a CI runner or a maintainer account (OPS-7).

## Priorities

**P0: a bug here loses funds or splits the chain.**
- `pq.rs`: the certificate scheme. Signer bitmaps, quorum assembly, attributable vs batch verification, namespace binding, decoding bounds, equivocation evidence.
- `consensus.rs` block validity: `verify` must reject exactly the permanently invalid blocks (the `Application` contract) and never vote on a block whose execution it can't reproduce; the execution lag and checkpoint rules; views and leader accounting.
- Determinism: anything that could make honest nodes compute different state (iteration order, floats, time, platform-dependent behaviour, panics on some nodes only). The simulator and fuzzers exist; we want the auditors' own attempts.
- The state machine: every action's checks and accounting. Conservation of supply (`State::check_invariants`), stake and share accounting, slashing, unbonding, locks and vesting, fees and issuance, governance execution, juries and payouts.
- Signing bytes: `types.rs` (`Tx::signing_bytes`, the wire form, `check_format`) against `sdk-ts/src/tx.ts`. Any input two implementations encode differently, or one encoding with two meanings.
- Key handling: address and key-id derivation, proof of possession, session-key roles and limits, multisig.

**P1: a bug here halts nodes, weakens trust or costs money indirectly.**
- Sync trust (`snapshot.rs`, `epochs.rs`, `node.rs` join): what a joining node accepts from a peer, the embedded checkpoints, the weak-subjectivity window.
- Equivocation evidence and slashing: verification against the right epoch's set; no double penalty; no honest validator slashable after a disk restore (slashing protection).
- Upgrades and halts: version gating, `--halt-height`, behaviour at activation heights.
- The HTTP API's interaction with consensus (locking, rate limits, response sizes), p2p admission and peer-set limits, decoders for every network input.
- The randomness beacon and the jury draw (grinding, withholding).

**P2:** CLIs, the explorer (XSS; it renders on-chain strings), deployment scripts, the release pipeline.

Questions we'd like answered explicitly:
1. Can a < ⅓ coalition finalize two different blocks, or get honest validators slashed?
2. Can any tx, block or p2p message make honest nodes diverge, or panic all of them at the same height?
3. Is supply conserved under every action and every end-of-block and end-of-epoch step?
4. Can a joining node be fed a state or validator set that no honest quorum certified?
5. Do the Rust and TypeScript encoders agree on every input, and is every encoding injective?
6. Is our use of `ml-dsa` and `@noble/post-quantum` (key generation from seeds, deterministic signing, verification of malformed inputs) correct and constant-time where it needs to be?

## What we provide

- Repository access at the audit tag, and a walkthrough call per engagement.
- The design docs above, a threat-model session, and a channel to the maintainers with answers within one business day.
- Build and test: `cargo test --locked --manifest-path node/Cargo.toml` (unit tests and the deterministic simulator), `bash scripts/local-testnet.sh` (4 validators, a full node, the SDK end-to-end tests), fuzz targets under `node/fuzz/` and NIST known-answer tests for ML-DSA in both implementations (SEC-2, SEC-3).
- A private testnet on request.

## Deliverables

1. A report: each finding with severity (critical, high, medium, low, informational), affected code, impact, a reproduction or proof of concept where feasible, and a recommended fix. Plus coverage notes: what was reviewed, how deeply, and what wasn't.
2. Interim findings shared as they're found (critical and high within 24 hours, privately).
3. **Fix review:** every fix for a critical, high or medium finding re-verified on a new commit.
4. **Re-review of the diff** between the audited commit and the genesis release, so the code that launches is the code that was audited. Any consensus-format change after the audit goes through re-review again.
5. A final report we publish, after fixes are deployed.

## Proposal

Please include: the team and their experience with BFT consensus, Rust and post-quantum or lattice cryptography (prior reports welcome); the approach and tooling (manual review, fuzzing, model checking, differential testing); an estimate in engineer-weeks per engagement and per priority level; available start dates; the fix-review and re-review terms; and pricing. Contact: open a private security advisory on the repository, or message the maintainer (@jackdishman on GitHub).
