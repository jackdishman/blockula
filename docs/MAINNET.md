# Blockula mainnet readiness

As of 2026-10-14, v0.4.0 (commit `b1f9614`; line numbers refer to it). This is the gap between today's public testnet and a mainnet that holds real value. Every claim cites code or a doc section so it can be re-checked. Items marked ✅ were fixed after this assessment was written.

- **Paths:** Rust files are in `node/src/` and cited by file name (`chain.rs:1489`). Everything else is relative to the repo root. Doc sections are cited by heading (PLAN.md "Known limits").
- **Effort:** S ≤ 1 engineer-week · M 2–4 engineer-weeks · L 1–3 engineer-months · XL > 3 engineer-months.
- **Who:** Team (we do it in code) · Auditor · Counsel · Operators (independent validator operators) · Upstream (Commonware).

## 1. Verdict

Blockula is a solid testnet and not close to a responsible mainnet. Expect about a year with 2–3 engineers (Q3 2027 at the earliest), or about 9 months with the scope cuts in section 6. Most of the remaining work changes consensus formats, so it has to land before an audit can start. The critical path:

1. A persistent, Merkle-committed state layer (CV-1).
2. Stake-weighted validators with slashing (CV-2, CV-3), a multi-party replacement for the single admin key (GOV-1), and height-gated upgrades (OPS-3).
3. A consensus-format freeze, then external audits, then at least 3 months of incentivized testnet run by independent operators.
4. A genesis ceremony with keys generated independently by each operator.

Legal review, the post-quantum handshake, supply-chain hardening and wallet recovery run in parallel, but all must be done before genesis.

## Decisions taken (2026-10-14)

The decisions in section 6 have been made. [MAINNET-DESIGN.md](MAINNET-DESIGN.md) records them with the protocol spec and work packages, and wins where it and this assessment disagree. In one line each:

| ID | Decision |
|---|---|
| DEC-1 | Everything that can be made safe is on at genesis, contribution pool included. Admin levers become governance; the jury draw uses a randomness beacon. |
| DEC-2 | Ghost Mode ships at genesis but disabled. Governance switches it on after its own ZK audit, testnet time and counsel. |
| DEC-3 | Permissionless, stake-ranked validators with equal-weight votes. Cap 31; genesis floor 22, target 28–31; the founder runs at most 2 seats. |
| DEC-4 | Self-custodial delegation to share-accounted pools, 21-day unbonding, slashing for double-signing (heavy) and downtime (light, jail first). |
| DEC-5 | 1B FANG, no sale: community 32%, treasury 25%, contribution endowment 15%, bootstrap delegation 10%, founder 10% (1-year cliff, 4-year vest), foundation 5%, operators and security grants 3%. See [TOKENOMICS.md](TOKENOMICS.md). |
| DEC-6 | Account creation burns a fee and names renew yearly; every other record is already bonded. |
| DEC-7 | Mainnet chain id `blockula-1` with address prefix `boo`; testnets use `tboo`. |
| DEC-8 | The wallet's passkey domain is one the project's entity owns, never `*.vercel.app`. |
| DEC-9 | Post-quantum transport before launch: a hybrid X25519 + ML-KEM-768 handshake in a vendored `commonware-stream`. |
| DEC-10 | No admin keys and no faucet on mainnet. |
| DEC-11 | Names: one-year registration, renewal at 20% a year, 30-day grace, transfer and release, a governance-assigned reserved list. |
| DEC-12 | 1 s blocks, `max_block_bytes` 2 MiB at genesis, a base fee that follows fullness, 1-hour epochs. |
| DEC-13 | Voting weight is FANG locked × conviction. Unvested tokens and Soul don't vote at genesis. |
| DEC-14 | At most ⌊n/4⌋ validators per provider, ASN or country (target ⌊f/2⌋), and 15% per facility. The foundation's 2 validators run on Fly.io; Hetzner is excluded. |

The counsel brief for LEG-1 to LEG-3 is [COUNSEL_BRIEF.md](COUNSEL_BRIEF.md).

## Where things stand

| | Today |
|---|---|
| Public testnet | Runs v0.4.0 (`b1f9614`), with the contribution pool active (one repo, 10,001 FANG). Its genesis was created 2026-10-14 12:19 UTC, so the longest continuous run on the current chain is a few hours (height ~11.3k when checked). |
| Operators | 4 validators in one Fly.io org, in 4 regions. Every validator key and the one admin key were generated on one laptop (`deploy/launch.sh:44,71`). Nakamoto coefficient: 1. |
| Supply | 1.1B FANG: 250M on each validator's hot consensus key, 100M on the keyless treasury (live `/api/genesis`; `chain.rs:1370-1372`). |
| Tests | Unit tests, 3 deterministic simulator scenarios (`sim.rs`: crash and recover, onboarding, fast sync) and a 4-validator smoke test (`scripts/local-testnet.sh`). No fuzzing, property tests or Byzantine scenarios. |
| Review | Internal four-track review only (README.md "Status", docs/ONE_PAGER.md:32). |

## 2. Launch blockers

| ID | Blocker | Effort | Who |
|---|---|---|---|
| SEC-1 | No third-party audit | L | Auditor |
| SEC-2 | PQ crypto libraries are unaudited; no known-answer tests | M | Team, Auditor |
| SEC-3 | No fuzzing, invariant checks or Byzantine tests | L | Team |
| SEC-4 | Public HTTP reads can stall consensus | M | Team |
| SEC-5 | No vulnerability disclosure program or bounty | S | Team |
| CRY-1 | p2p key exchange is X25519 | L | Team, Upstream, Auditor |
| CRY-2 | Fast sync is weakly subjective; genesis keys on one laptop | M | Team |
| CRY-3 | Jury randomness is grindable (if the pool is live at genesis) | L | Team, Auditor |
| CV-1 | State is in memory, flat-hashed and replayed from genesis | XL | Team |
| CV-2 | One vote per validator; admin allowlist | XL / M | Team, Operators |
| CV-3 | No slashing or evidence; ten-minute unbonding | L | Team, Auditor |
| CV-4 | Validator-set churn isn't limited in code | S | Team |
| CV-5 | Byzantine leaders get free block space | M | Team |
| CV-6 | Crash-safety gaps | S, then M | Team |
| TOK-1 | No issuance; tips buy nothing | L | Team |
| TOK-2 | Fixed fee, first-come ordering | M | Team |
| TOK-3 | Treasury can't spend | M | Team |
| TOK-4 | No real genesis allocation or vesting | M | Team, Counsel |
| GOV-1 | One admin key controls the validator set | L / XL | Team, Auditor |
| GOV-2 | Admins can move users' escrowed bounties | S | Team |
| GOV-3 | Proof of Contribution runs on admin judgment | L | Team |
| OPS-1 | No key ceremony; no operator independence | M | Operators, Team |
| OPS-2 | Genesis is hand-built | S–M | Team, Operators |
| OPS-3 | No upgrade or hard-fork mechanism | L | Team |
| OPS-4 | No monitoring setup beyond an endpoint | M | Team, Operators |
| OPS-5 | No incident response; no exit from a deterministic halt | M | Team, Operators |
| OPS-6 | Backups, disk growth, double-sign protection | M | Team, Operators |
| OPS-7 | Builds aren't reproducible or signed; open supply chain | M | Team |
| WAL-1 | Passkey path has no browser tests | M | Team |
| WAL-2 | No account recovery | L | Team |
| WAL-3 | Web wallet origin security | M | Team, Auditor |
| WAL-4 | Clients trust one RPC and can switch networks silently | S–M | Team, Operators |
| WAL-5 | Signed txs never expire | S | Team |
| LEG-1 | Token status and distribution | L | Counsel |
| LEG-2 | Paying people for GitHub work | M | Counsel |
| LEG-3 | Names, trademarks, personal data on chain | S–M | Counsel, Team |

### Security & audits

**SEC-1. No third-party audit.**
- **Missing:** only an internal review exists (README.md "Status"; docs/ONE_PAGER.md:32). The riskiest code is ours, not a library's: the ML-DSA certificate scheme plugged into Commonware (`pq.rs`), block verification and the checkpoint rule (`consensus.rs:346-412`), the state machine (`chain.rs`, `poc.rs`), fast-sync trust (`snapshot.rs`, `epochs.rs`), and Rust/TS signing parity (`types.rs:222-386` ⇄ `sdk-ts/src/tx.ts`).
- **Why it blocks:** one consensus or accounting bug loses funds or splits the chain, and nobody outside the team has looked.
- **Done when:** a firm with BFT and Rust experience has audited the frozen genesis commit (node, consensus integration, state machine, sync, crypto use). A separate review covers the wallet, SDK and MCP (WAL-3). All critical and high findings are fixed and re-verified, and the reports are published.
- **Effort:** L. It takes 6–10 weeks of calendar time including fixes, and firms book 1–3 months ahead, so reserve a slot now. **Who:** Auditor.

**SEC-2. The PQ crypto libraries are unaudited.**
- **Missing:**
  - `ml-dsa = "0.1.1"` (`node/Cargo.toml`) says in its README that it "has never been independently audited". It signs and verifies every vote, certificate and tx in the node.
  - `@noble/post-quantum ^0.7.1` (`sdk-ts/package.json:24`) says it "has not been independently audited yet" (self-audited at 0.6.1). It signs every tx from the SDK, MCP and wallet.
  - Commonware is pinned (`=2026.9.0`). Nobody has checked whether the modules we use (simplex, marshal, p2p discovery, stream, storage) are covered by its audits.
- **Why it blocks:** a verification bug, or a disagreement between the Rust and TS implementations, is a key-forgery or chain-split bug.
- **Done when:**
  - NIST ACVP known-answer tests for ML-DSA-65 keygen, sign and verify run in CI for both implementations.
  - A cross-implementation test signs in each and verifies in the other (extending the vectors at `types.rs:755-788`).
  - SEC-1's scope explicitly covers our use of both libraries, or we switch to audited ones.
  - Crypto dependencies are pinned to exact versions.
- **Effort:** M. **Who:** Team, Auditor.

**SEC-3. No fuzzing, invariant checks or Byzantine tests.**
- **Missing:**
  - Nothing in `node/src` is fuzzed or property-tested.
  - The FANG conservation check exists only as a test helper (`poc.rs:1500-1511`).
  - The simulator covers honest crashes, onboarding and fast sync (`sim.rs`), not misbehaving validators.
- **Why it blocks:** every network-facing decoder takes attacker input: `CBlock::read_cfg` (`consensus.rs:143-185`), tx JSON plus `check_format` (`types.rs:401-579`), `/api/sync` batches (`node.rs:555-585`), snapshot manifests (`snapshot.rs:150-193`) and finalization certificates. So does the state machine. A panic or divergence in any of them halts or splits the chain. A read-through found no panic in the decode paths, but a read-through is not evidence.
- **Done when:**
  - cargo-fuzz targets exist for each decoder, plus a "random txs → apply → `conserved()`" target. They run in CI and for at least 2 weeks on dedicated machines without findings.
  - Conservation and nonce invariants are asserted after every block in the simulator.
  - Simulator scenarios use Commonware's deterministic runtime and simulated network to cover an equivocating leader, a wrong `exec_root`, junk-tx blocks (CV-5), withheld votes, partitions and clock skew.
- **Effort:** L. **Who:** Team.

**SEC-4. Public HTTP reads can stall consensus.**
- **Missing:**
  - Every handler locks the same `Mutex<Chain>` that propose, verify and execute use.
  - Several handlers do O(state) work under that lock:
    - `/api/status` recomputes the flat state root (`api.rs:396`).
    - `/api/key/{id}` scans every account (`api.rs:612`).
    - `/api/account` scans every name and hunt (`api.rs:581, 603-604`).
    - `/api/sync` reads up to 8 MiB from disk (`api.rs:880-944`).
  - Only `POST /api/tx` and the faucet are rate limited (`api.rs:478, 666-673`).
  - The "Become a validator" guide has validators advertise a public HTTP address (README.md "Become a validator"). Full nodes forward txs to validators over HTTP (`api.rs:192-218`).
- **Why it blocks:** a modest request rate against a validator's HTTP delays its votes. Doing that to a third of the set halts the chain.
- **Done when:**
  - Validators don't expose HTTP publicly by default. Sentry or RPC full nodes sit in front.
  - Reads run against a snapshot of state published after each block (or the store from CV-1), never under the consensus lock.
  - Reads have per-client rate limits and timeouts.
  - A load test shows finalization is unaffected at a stated request rate.
- **Effort:** M. **Who:** Team.

**SEC-5. No vulnerability disclosure program.**
- **Missing:** CONTRIBUTING.md "Security" points to GitHub Security Advisories, and that's all. There's no SECURITY.md, no contact, no response timeline and no bounty.
- **Done when:** a SECURITY.md gives scope, contact and timelines, and a funded bounty (self-run or through a platform) has been live on the incentivized testnet for at least a month before genesis. ✅ SECURITY.md exists (private advisories, response targets, testnet safe harbor); the bounty doesn't yet.
- **Effort:** S, plus a bounty budget. **Who:** Team.

### Cryptography & networking

**CRY-1. The p2p transport isn't post-quantum.** (known)
- **Missing:** Commonware's handshake authenticates peers with ML-DSA-65, but the session key comes from X25519 and traffic is encrypted with ChaCha20-Poly1305 (PLAN.md "Known limits"; `/api/status` says so, `api.rs:414`). Meanwhile README.md says "no elliptic curves anywhere".
- **Why it blocks:** recorded validator traffic can be decrypted later, including the mempool and which IP sent which tx. More importantly, the headline claim is false at launch unless this lands.
- **Done when:**
  - The Commonware stream handshake uses a hybrid X25519 + ML-KEM-768 exchange, with both secrets fed into the KDF and the transcript bound. It's upstreamed, or carried as a pinned, reviewed fork.
  - Downgrade and interop tests pass, and the change is in SEC-1's scope.
  - Alternatively, a recorded decision (DEC-9) to launch without it, with README and ONE_PAGER corrected.
- **Effort:** L. **Who:** Team, Upstream, Auditor.

**CRY-2. Fast sync is weakly subjective, and the genesis keys sit on one laptop.** (known, plus the key detail)
- **Missing:**
  - A joiner trusts the genesis set plus 2f+1 handoff signatures for every set change (`epochs.rs:148-177`; PLAN.md "Known limits"). Keys of a long-gone set can sign a fake handoff, even retroactively.
  - The epoch anchor a node fast-syncs into comes from the peer and isn't certified (`snapshot.rs:266-277`).
  - Every testnet genesis key was generated on one machine (`deploy/launch.sh:71`). Whoever holds that machine can forge an alternate history for any fresh joiner, forever.
  - `join --genesis <URL>` without `--genesis-hash` trusts whatever that URL serves, and only prints a warning (`node.rs:390-405`).
- **Why it blocks:** new nodes, exchanges and RPC providers can be fed a fake chain.
- **Done when:**
  - Each release ships a signed checkpoint (height, block hash, validator-set hash). `join` requires it by default.
  - The weak-subjectivity period is documented and no longer than the unbonding period (CV-3).
  - The epoch anchor is certified in the snapshot manifest.
  - The mainnet genesis hash is compiled into the binary for the mainnet chain id.
  - Genesis keys are generated by independent operators (OPS-1).
- **Effort:** M. **Who:** Team.

**CRY-3. Jury randomness is grindable.** (known; a blocker only if the contribution pool is live at genesis, see DEC-1)
- **Missing:** the Séance seed chains block hashes (`poc.rs:1076-1092`). The proposer of the block that opens a Séance can reorder its txs to choose the jurors (PLAN.md "Known limits"). Commonware's `Random` elector needs BLS threshold signatures, which aren't post-quantum, so there's no beacon to borrow.
- **Why it blocks:** a validator that is also a claimant can choose its own jury and pay itself from the pool.
- **Done when:** jury draws are seeded by a PQ-safe beacon (for example, per-epoch commit-reveal among validators with slashing for withholding, or a hash-based VRF). The opening height is fixed in advance. Tests show a proposer can't change the jury.
- **Effort:** L. **Who:** Team, Auditor.

### Consensus, state & validator set

**CV-1. The state layer doesn't scale and can't produce proofs.**
- **Missing:**
  - All state is in-memory `BTreeMap`s (`chain.rs:400-428`).
  - The state root is a flat BLAKE3 over all of it, with structs serialized by serde_json (`chain.rs:1029-1077`). It's recomputed for every executed block, every proposal (`consensus.rs:281, 296`) and every `/api/status` call (`api.rs:396`). PLAN.md "Known limits" puts the cost at ~10 ms per 100k accounts.
  - State isn't persisted. A validator restart re-executes every block since genesis (`chain.rs:1403-1512`).
  - The tx index and per-account history are unbounded in-memory maps (`chain.rs:1278-1279`).
  - Every 100 blocks, a new thread clones the whole state and writes it as one JSON blob (`chain.rs:1826-1833`, `snapshot.rs:61-63`). Snapshots are capped at 4 GiB (`snapshot.rs:32`).
  - There's no state rent or deposit, though PLAN.md "Tokenomics" promises it.
  - The root already needs compatibility branches for older chains (`chain.rs:1044-1075`).
- **Why it blocks:**
  - **Cheap DoS.** A new account costs one minimum fee (~64k drops) plus 0.01 FANG moved into it (`types.rs:682`). At ~388 txs per block, about 1.4 hours and ~130 FANG of fees create 2M accounts. That likely exhausts a 512 MB validator (`deploy/launch.sh:126-127`) through state plus indexes. At 10M accounts (~650 FANG, ~7 hours), the root alone takes ~1 s per block. That's past the 1 s block time and the checkpoint wait (`consensus.rs:416-433`). See the appendix for the numbers.
  - **Restarts get slower forever.** Upgrades restart every validator at once (`deploy/launch.sh:131`), so the whole chain is down for the full replay.
  - **No light clients.** Without Merkle proofs, wallets must trust one RPC (WAL-4).
  - **Hard to change later.** The state root and encoding are consensus rules, so changing them after genesis is a hard fork.
- **Done when:**
  - State lives in a disk-backed authenticated store with incremental roots and inclusion proofs. commonware-storage's `qmdb` and `merkle` modules are already in the pinned dependency set.
  - State uses a canonical binary encoding.
  - State is committed per block, atomically with the block, and restart time doesn't depend on history length.
  - The tx and history index moves to disk or to a separate indexer (PLAN.md "Roadmap": "Remaining: a standalone indexer").
  - Snapshots stream instead of being built in memory.
  - Accounts, names and other records pay deposits or rent (DEC-6).
  - A benchmark at 10M accounts shows execute plus commit well under the block time on the reference validator spec.
- **Effort:** XL. It's new storage and a new root, every module migrates, and it must land before the audit. **Who:** Team.

**CV-2. One vote per validator, and validators are an admin allowlist.** (known)
- **Missing:** the quorum counts validators (`pq.rs:243-251`, `quorum::<N3f1>()`). The bond weights nothing. Validators join only through `approve_validator` (`chain.rs:820-831`). PLAN.md "Roadmap" lists permissionless staking for Phase 3.
- **Why it blocks:** security comes from whoever the admin approves, not from stake at risk. Sybil resistance is one person's judgment.
- **Done when:** one of two paths is built and tested in the simulator with set changes every epoch:
  - **(a) Staking:** stake-weighted voting with permissionless registration, delegation rules decided (DEC-4), a cap on the active set, and stake-weighted quorum certificates.
  - **(b) Permissioned launch:** equal weights, with the operator list set and changed by governance (GOV-1), operators meeting published independence criteria (DEC-3), and docs that say so plainly.
- **Effort:** XL for (a). M for (b), on top of GOV-1. **Who:** Team, Operators.

**CV-3. No slashing, no evidence, and a ten-minute unbonding period.** (known, plus the unbonding detail)
- **Missing:**
  - The only consensus reporters are marshal and a certificate capture that keeps finalizations (`consensus.rs:437-455, 789`).
  - Simplex's `ConflictingNotarize`, `ConflictingFinalize` and `NullifyFinalize` reports are dropped, and `track_historical_votes` is off (`consensus.rs:805`). Evidence isn't even stored.
  - A removed validator can unbond at `epoch + 2` (`chain.rs:472`). With the default 600-block epochs (`chain.rs:68-70`), that's 10–20 minutes.
- **Why it blocks:** a validator can equivocate at no cost. Even with slashing, it could withdraw its bond before anyone acted.
- **Done when:**
  - An evidence tx carries two conflicting signed votes. The chain verifies it, slashes the bond and jails the validator.
  - Downtime is measured from certificate signer bitmaps (the node already reads them, `api.rs:569-575`), with a light penalty.
  - Unbonding lasts at least as long as the evidence window and the weak-subjectivity period (e.g. 14–21 days, DEC-4).
  - The simulator runs an equivocating validator and checks that every node slashes it identically.
- **Effort:** L. **Who:** Team, Auditor.

**CV-4. Validator-set churn isn't limited in code.**
- **Missing:** `next_set` adds every approved validator up to `max_validators` (`chain.rs:442-454`). PLAN.md "Known limits" says to approve at most ⌊(n−1)/3⌋ per epoch, because newcomers count as offline until synced, but nothing enforces it. Removals stop only at "never empty" (`chain.rs:848-851`).
- **Why it blocks:** one batch of approvals or removals can halt the chain by adding too many unsynced members. With a compromised admin key, it can also hand over a 2/3 majority in one epoch (GOV-1).
- **Done when:** adds and removals are capped per epoch (by count, or by weight after CV-2) so each new set overlaps the old one by more than 2/3. Covered by a simulator test.
- **Effort:** S. **Who:** Team.

**CV-5. Byzantine leaders get free block space.**
- **Missing:**
  - `verify` checks only stateless validity (`consensus.rs:393-406`). At execution, txs that fail admission (bad nonce, no balance) are skipped without charge (`chain.rs:1752`), but their bytes are kept forever in `cblocks.bin`.
  - `Tx` accepts unknown JSON fields, and size limits apply to the re-serialized form (`types.rs:402`). So a leader can also pad valid txs to 64 KB each and pay only for their canonical size.
- **Why it blocks:** a leader can fill each of its blocks, up to 4 MiB (`consensus.rs:51`), with txs that never pay. With 32 validators, one bad validator adds ~10 GB a day that every node stores.
- **Done when:** verifiers apply the proposal on top of their executed state plus not-yet-executed ancestors, as `propose` already does (`consensus.rs:288-330`), and reject blocks with unadmittable or non-canonical txs. Or such blocks become slashable. Covered by a simulator test with a junk-filling leader.
- **Effort:** M. **Who:** Team.

**CV-6. Crash-safety gaps.**
- **Missing:**
  - A torn last line in `blocks.jsonl` stops the node from starting. `Chain::open` parses each line with `?` (`chain.rs:1489`). `RecordFile` repairs torn tails (`chain.rs:1215-1223`), but `blocks.jsonl` isn't a `RecordFile`.
  - `base.json` and `base.state.zst` are written in place without fsync (`chain.rs:1841-1845`).
  - `epochs.json` uses tmp-then-rename, but never fsyncs (`chain.rs:1816-1823`).
  - If `consensus/` is ahead of `blocks.jsonl`, the node only logs an error, and an operator must delete `consensus/` (`consensus.rs:679-680`).
- **Why it blocks:** power loss or an OOM kill at the wrong moment needs manual repair. During an incident, that's many operators doing surgery at once.
- **Done when:** crash-injection tests (a kill at every write point, plus Commonware's deterministic storage faults) restart unaided, and each block commits atomically (this part folds into CV-1).
- **Effort:** S for the torn line now, M as part of CV-1. **Who:** Team.
- ✅ **Torn last line:** `Chain::open` now drops a torn, unparseable final line (that block was never acknowledged to consensus) and starts; a corrupt line elsewhere still refuses to start.

### Token economics & distribution

**TOK-1. No issuance, and tips buy nothing.** (known)
- **Missing:** supply only changes through the fee burn (`chain.rs:696-700`). The 6% → 1.5% tail schedule and the 70/20/10 split (PLAN.md "Tokenomics") aren't built. Tips (fee minus minimum) go to the proposer (`chain.rs:700`), but proposals take mempool txs in arrival order (`consensus.rs:305-327`), so a tip doesn't buy priority.
- **Why it blocks:** nobody has a reason to run a validator except goodwill. There's no yield behind staking (CV-2), and the contribution pool has no inflow.
- **Done when:** issuance runs in integer math with the decided parameters (DEC-5) and pays stakers, the pool and the treasury each epoch. Tests show total supply follows the schedule exactly. Or: a recorded decision to launch with zero issuance and another way to pay validators.
- **Effort:** L. **Who:** Team.

**TOK-2. The fee market is a fixed price with first-come ordering.**
- **Missing:** the minimum fee is a constant, `10_000 + 10 × bytes` (`types.rs:613-615`). Blocks fill in arrival order. Each validator keeps its own 5,000-tx mempool and evicts by fee per byte only when full (`chain.rs:1943-1955, 2067`).
- **Why it blocks:**
  - A transfer is ~5.4 KB (minimum fee ≈ 64k drops) and ~10.8 KB as JSON, so ~388 fit in a 4 MiB block.
  - Filling every block for a day costs ≈ 2,200 FANG.
  - Honest users have no predictable way to outbid spam.
  - The fee can only change by hard fork.
- **Done when:** a per-byte base fee adjusts with block fullness (and is burned), and the tip buys priority. A spam load test shows honest txs confirm within a bounded number of blocks at a bounded cost. Fee parameters are either changeable by governance (GOV-1) or fixed by decision.
- **Effort:** M. **Who:** Team.

**TOK-3. The treasury can't spend.**
- **Missing:** the treasury is a keyless system address (`chain.rs:14-16`). It receives name fees (`chain.rs:740`) and 100M FANG at genesis (`chain.rs:1372`). Nothing spends from it, though PLAN.md "Tokenomics" says it pays for sponsored onboarding, faucets, audits and grants.
- **Why it blocks:** today it's a burn with a misleading label. If a spend path gets bolted on later, that's an unbounded power nobody designed.
- **Done when:** spending goes through a governed action that is timelocked, approved by quorum or multisig, and publicly queued. Or the genesis allocation leaves the treasury out.
- **Effort:** M, after GOV-1. **Who:** Team.

**TOK-4. No real genesis allocation or vesting.**
- **Missing:** `summon` gives 1e9/N FANG to each validator's consensus key and 100M to the treasury (`chain.rs:1370-1372`). Nothing vests or locks up except contribution grants (`poc.rs:290-310`). The only genesis tooling is hand-editing JSON (README.md "Run your own chain").
- **Done when:**
  - The allocation is decided and reviewed by counsel (DEC-5, LEG-1).
  - A vesting-account type exists.
  - A deterministic genesis builder takes a reviewed allocation file.
  - The allocation table is published alongside the genesis hash.
- **Effort:** M. **Who:** Team, Counsel.

### Governance (replacing the Season 0 admin levers)

**GOV-1. One admin key controls the validator set.**
- **Missing:**
  - `validator_admins` is fixed in genesis (`chain.rs:54-56, 155`), and no tx can change it.
  - Any single owner key on an admin account can act. A tx carries one signature, and accounts have no M-of-N threshold (`chain.rs:628-666`).
  - Admins approve and remove validators with no churn limit (`chain.rs:820-852`; CV-4).
  - The live testnet has one admin.
- **Why it blocks:** with 4 active validators, a leaked admin key can approve 28 attacker validators (up to the 32 cap). That's more than 2/3 of the next epoch's set, in under 10 minutes. It controls safety, not just liveness.
- **Done when:**
  - Admin powers either don't exist on mainnet, or sit behind an M-of-N multisig account with a timelock and a public queue. That's the minimum.
  - The long-term path is on-chain governance as planned (PLAN.md "Forks and governance": √stake + Soul, conviction voting, timelock).
  - The admin set itself can be changed through that process.
  - Churn limits (CV-4) bound what any one approval can do.
- **Effort:** L for multisig plus timelock, XL for full governance. **Who:** Team, Auditor.

**GOV-2. Admins can move users' escrowed bounties.**
- **Missing:** an admin's `award_hunt` pays any open hunt's whole escrow to any address (`chain.rs:945-972`, admin check at 950-953). Admins can also cancel any hunt (`chain.rs:973-998`) and unlink any GitHub handle (`chain.rs:888-901`).
- **Why it blocks:** one key has custody of other people's money.
- **Done when:** only the creator, or a mechanism funders agreed to (e.g. acceptance-test Hunts), can award. Admin intervention is limited to refunding funders, via governance with a timelock.
- **Effort:** S. **Who:** Team.

**GOV-3. Proof of Contribution runs on admin judgment.**
- **Missing:** Season 0 admins can:
  - Grant Soul: at most 100 per grant, but unlimited grants (`poc.rs:690-697`).
  - Set repo weights (`poc.rs:673-689`).
  - Flag sybils. This zeroes Soul, rejects claims, claws back rewards and slashes the voucher (`poc.rs:744-754, 1004-1036`).
  - Reject claims (`poc.rs:865-888`).
  - Claw back rewards (`poc.rs:992-998`).

  PLAN.md "Known limits": "Phase 3 moves these to governance and juries."
- **Why it blocks:** whoever holds the key decides who earns from the pool. If Soul feeds voting weight, as PLAN.md plans, it also decides governance.
- **Done when:** either the pool is off at genesis and turned on at an activation height once jury-run flags and revert checks exist (DEC-1), or every lever goes through governance with a timelock and an appeal path. Either way, Soul is bootstrapped by a one-time, published genesis ledger, not an ongoing grant power.
- **Effort:** L. **Who:** Team.

### Operations

**OPS-1. No key ceremony, and no operator independence.**
- **Missing:**
  - `launch.sh` generates every validator key and the admin key on one laptop (`deploy/launch.sh:44, 69-85`).
  - All validators run in one Fly org. Any org member can `fly ssh` into a validator and read `validator.key`, a plaintext hex seed (`node.rs:271`).
  - Genesis validators own themselves (`chain.rs:164-185`). So the hot consensus key also holds the validator's allocation (250M FANG each on the testnet), pays attestation fees (PLAN.md "Known limits") and pays the faucet (`chain.rs:2019-2031`).
- **Done when:**
  - Each genesis operator generates its key offline and submits its public key, proof of possession and addresses.
  - Each operator registers a separate cold owner account and keeps only a fee float on the consensus key. The attestation fee payer becomes configurable.
  - Any admin or treasury keys that survive GOV-1 are M-of-N, held by different people on offline machines.
  - Operators meet published independence criteria: entity, hosting provider, jurisdiction (DEC-3, DEC-14).
  - The ceremony is recorded.
- **Effort:** M, plus months of calendar time to recruit operators. **Who:** Operators, Team.

**OPS-2. Genesis is hand-built.**
- **Missing:**
  - Genesis is `summon` output edited with jq (`deploy/launch.sh:74-76`) or by hand (README.md "Run your own chain", step 2).
  - `summon` defaults to `faucet: true` (`chain.rs:1176`).
  - The genesis hash depends on `created_at_ms` and on the serde_json of the validator list (`consensus.rs:204-208`).
  - `join` only warns when no hash is pinned (`node.rs:403-405`).
- **Done when:**
  - A deterministic genesis builder works from reviewed inputs: allocations, operators, parameters, `faucet: false`.
  - Several parties rebuild the same genesis hash.
  - The genesis file and hash are published in the signed release and through other channels.
  - The mainnet hash is compiled into the binary (CRY-2).
  - A full dry run happens on a throwaway chain id.
- **Effort:** S–M. **Who:** Team, Operators.

**OPS-3. No upgrade or hard-fork mechanism.** (known)
- **Missing:**
  - Blocks carry no protocol version (`consensus.rs:73-86`), and nothing gates behavior by height.
  - Constants in code are consensus rules (PLAN.md "Known limits": "changing them is a coordinated upgrade").
  - Upgrades redeploy every validator at once (deploy/README.md "Caveats"; `deploy/launch.sh:131`).
  - On-disk layout changes force a resync (`node.rs:245-258`).
  - PLAN.md's "a vote, then a timelock, then validators signal adoption" isn't built.
- **Why it blocks:** with independent operators, "everyone restarts at once" means a halt or a split on every upgrade.
- **Done when:**
  - The state machine and block validity carry a protocol version and activation-height gating.
  - Validators signal readiness in blocks.
  - A node won't run past an activation height it doesn't support. It halts cleanly instead of diverging.
  - The hard-fork process is written down: announce, release, signal ≥ 2/3, activate.
  - On-disk formats are versioned, with migrations.
  - A simulator test upgrades validators one at a time across an activation height.
- **Effort:** L. **Who:** Team.

**OPS-4. Monitoring is an endpoint, not a setup.**
- **Missing:** `/metrics` and `/api/health` exist (`api.rs:516-566`). There are no dashboards, alert rules, on-call rotation or status page. There are no metrics for per-validator missed votes, peer count, clock skew, disk or memory. Logs only go to stderr (`main.rs:166-169`).
- **Done when:**
  - A reference Prometheus and Grafana stack ships with alert rules: head age, finalized lag, signers on the last certificate vs quorum, mempool full, disk > 80%, memory, clock skew.
  - Every operator runs it.
  - There's a public status page and an on-call rotation with escalation.
- **Effort:** M. **Who:** Team, Operators.

**OPS-5. No incident response, and no way out of a deterministic halt.**
- **Missing:**
  - If a finalized block fails to execute, the executor stops acknowledging it and the node halts (`consensus.rs:511-512`).
  - A panic in end-of-block code (e.g. the `expect`s at `chain.rs:461` and `poc.rs:1180`) would stop every validator at the same height, and again on every restart.
  - There's no halt-at-height flag, no private channel to operators and no runbook.
  - ✅ ~~The documented admin command fails.~~ `--key` now also accepts `keygen` JSON and wallet.json, so deploy/README.md's command works.
- **Done when:**
  - An incident-response runbook covers severities, who decides, operator contacts, private patch distribution, and a coordinated restart at a height.
  - `--halt-height` and an emergency-patch path exist and are tested.
  - Every operator runbook has been run end to end on the testnet.
  - One tabletop exercise has been held.
- **Effort:** M. **Who:** Team, Operators.

**OPS-6. Backups, disk growth and double-sign protection.**
- **Missing:**
  - Restoring a volume snapshot is forbidden because it can double-sign (deploy/README.md "Caveats").
  - The double-sign guard is a heuristic: an empty journal or a `machine.lock` mismatch (`node.rs:216-243`). No record of the last view signed survives a restore.
  - Disk grows ~0.6 KB per *empty* block with 4 validators. That's ~330 B in `blocks.jsonl`, ~170 B in `cblocks.bin` and a ~10 KB certificate every 100 heights: about 50 MB/day, or ~19 GB/year before any txs.
  - Old epochs' simplex journals are never deleted (PLAN.md "Known limits").
  - Testnet volumes are 1 GB (`deploy/launch.sh:49`).
- **Done when:**
  - Keys are backed up offline and encrypted.
  - A slashing-protection record (last signed view per epoch) can't be rolled back by a restore.
  - Restore-from-peers via fast sync is tested.
  - Disk sizing is published per validator count, and old epoch journals are pruned.
  - Restore drills run quarterly.
- **Effort:** M. **Who:** Team, Operators.

**OPS-7. Builds aren't reproducible or signed, and the supply chain is open.**
- **Missing:**
  - Release builds use floating toolchains and images: `dtolnay/rust-toolchain@stable` and `rust:1-bookworm` (`.github/workflows/release.yml:16-17, 26`).
  - Actions are pinned by tag, not SHA (`release.yml:23, 41, 50-51`), and `contents: write` applies to every job (`release.yml:7-8`).
  - Artifacts are checksummed in the same release that serves them (`release.yml:53-60`). `install.sh` and the MCP's `install_node` trust that `checksums.txt` (`scripts/install.sh:26-35`; `mcp/src/index.ts:152-173`). A compromised GitHub account or workflow can ship a malicious binary with a matching checksum.
  - The Dockerfile uses unpinned base images and builds without `--locked` (`deploy/Dockerfile:2, 7, 9`).
  - npm packages are published by hand, with no publish workflow and no provenance. Users run `npx -y @blockula/mcp` unpinned (AGENTS.md; README.md "For agents"). The SDK takes caret ranges on its crypto dependencies (`sdk-ts/package.json:23-25`).
  - CI runs no `cargo audit`, `cargo deny`, `npm audit` or clippy (`.github/workflows/ci.yml`).
- **Done when:**
  - The toolchain is pinned (`rust-toolchain.toml`) and images are pinned by digest.
  - Actions are pinned by SHA, with least-privilege tokens.
  - At least two independent builders reproduce the build.
  - Artifacts are signed (Sigstore, or minisign with an offline key), and `install.sh` and `install_node` verify the signature.
  - Releases carry SLSA provenance.
  - npm uses trusted publishing with provenance and 2FA, and crypto dependencies use exact versions.
  - CI runs dependency audits, with Renovate or Dependabot.
- **Effort:** M. **Who:** Team.

### Wallets & clients

**WAL-1. The passkey path has no browser tests.** (known)
- **Missing:** PLAN.md "Known limits" says passkey.ts "typechecks but hasn't been exercised in a real browser yet", yet the deployed wallet calls it (`web/lib/keys.ts:24-33`). PRF support differs by platform. The key comes from one fixed PRF salt (`sdk-ts/src/passkey.ts:10`), so every device a passkey syncs to holds the same wallet key.
- **Done when:**
  - Playwright tests run against a virtual authenticator with PRF.
  - A manual test matrix is recorded for each release: iOS and macOS Safari, Chrome on Android, macOS and Windows, Windows Hello, 1Password, a hardware key.
  - The SDK and wallet share derivation test vectors.
  - Behavior without PRF is documented.
- **Effort:** M. **Who:** Team.

**WAL-2. No account recovery.**
- **Missing:** PLAN.md "Accounts" promises SLH-DSA recovery keys, guardians and a password fallback. None exist: `crypto.rs` is ML-DSA only, and the wallet's only safeguard is linking another device ahead of time (`web/app/devices/page.tsx`).
- **Why it blocks:** a user whose only passkey provider is lost, or locks them out, loses the account. With real money, this will be the most common way users lose funds.
- **Done when:** onboarding adds a recovery owner key (SLH-DSA as planned, or an offline ML-DSA key) with a downloadable or printable kit, and the restore flow is tested. Time-delayed social recovery can follow later.
- **Effort:** L. **Who:** Team.

**WAL-3. Whoever controls the web wallet's origin controls the keys.**
- **Missing:**
  - The passkey rpId is `blockula-wallet.vercel.app` (`web/lib/config.ts:20`), a subdomain of a domain we don't control. The code itself notes that wallets are "bound to one hostname for good" (`web/lib/keys.ts:11-22`).
  - There's no CSP and no security headers (`web/next.config.ts:4-11`).
  - Keys are derived and held in page JS. Any XSS, malicious dependency or compromised Vercel account can exfiltrate each user's seed when they unlock.
  - The "browser-only key" keeps its seed in localStorage (`web/lib/keys.ts:35-38`; `web/lib/session.ts:17-18`).
- **Done when:**
  - The mainnet rpId is a domain we own, chosen before the first mainnet passkey is created (DEC-8).
  - A strict CSP is in place, with no third-party scripts, and SRI.
  - Production deploys need review and come from a separate, locked-down project.
  - Local-key mode is off on mainnet.
  - The wallet is in audit scope.
- **Effort:** M. **Who:** Team, Auditor.

**WAL-4. Clients trust one RPC and can switch networks silently.**
- **Missing:**
  - The SDK, MCP and wallet default to `blockula-testnet.fly.dev` (`web/lib/config.ts:7`; `mcp/src/index.ts:24, 56`).
  - The MCP silently uses whatever answers on `localhost:7070` (`mcp/src/index.ts:56`).
  - Responses carry no proofs (CV-1).
  - Addresses are the same on every chain: one `boo` prefix (`crypto.rs:11`), and the passkey derivation doesn't involve the chain (`sdk-ts/src/index.ts:78-79`).
- **Why it blocks:** a fake or wrong-network node can show false balances, names and key lists, and users can mistake testnet for mainnet.
- **Done when:**
  - At least 3 operators run mainnet RPC endpoints.
  - Clients pin the mainnet chain id and genesis hash, and refuse a node that reports anything else.
  - The MCP no longer auto-switches networks.
  - Every screen shows the network.
  - A distinct testnet address prefix has been decided (DEC-7).
- **Effort:** S–M. **Who:** Team, Operators.

**WAL-5. Signed txs never expire.**
- **Missing:** a tx has no validity window (`types.rs:179-194`). The mempool drops a tx after 600 blocks, and `/api/tx` then reports it as "failed … expired" (`chain.rs:1888-1890`). But the signed tx stays valid until its nonce is used, so anyone who saw it can submit it later.
- **Why it blocks:** a wallet or agent tells the user a payment failed, and it goes through hours later. Adding a field changes the signing bytes, so it has to happen before genesis.
- **Done when:** a `valid_until` height is in the signing bytes, byte-identical in Rust and TS (`types.rs` ⇄ `sdk-ts/src/tx.ts`), and enforced at admission and execution. Wallets say "expired" only when the tx can no longer execute.
- **Effort:** S. **Who:** Team.

### Legal & compliance

**LEG-1. Token status and distribution.**
- **Missing:** PLAN.md "Tokenomics" says "Get legal review before any sale or distribution." Nothing shows that review has happened.
- **Done when:**
  - Written opinions for the launch jurisdictions cover FANG, the genesis allocation and vesting, any sale, staking and validator rewards.
  - A legal entity (foundation or company) owns the treasury, trademarks and domains.
  - Terms of use and risk disclosures cover the wallet, MCP and RPC.
- **Effort:** L (calendar). **Who:** Counsel.

**LEG-2. Paying people for GitHub work.**
- **Missing:** Hunts and the contribution pool pay FANG to GitHub identities and their agents (PLAN.md "Proof of Contribution"). There's no policy on tax reporting, sanctions screening of recipients, contributor terms, or who legally the payer is.
- **Done when:**
  - Counsel has reviewed Hunts and the pool in the form they'll take on mainnet.
  - Sanctions and tax policies exist, and contributors agree to terms.
  - GitHub's API terms have been checked for our use of the API as a payment oracle (`github.rs`).
- **Effort:** M (calendar). **Who:** Counsel.

**LEG-3. Names, trademarks and personal data on chain.**
- **Missing:**
  - `.boo` names are first-come, with no reserved list or dispute policy, and can't be transferred or expire (SF-9). Whoever is first at genesis keeps a brand's name for good.
  - To our knowledge, `.boo` is also a live ICANN TLD (Google Registry), so `alice.boo` can also be a website. Confirm this.
  - GitHub handles, and free-text admin reasons like "flagged as a sybil: …", are written on chain forever (`types.rs:64-68, 111-113`).
- **Done when:** genesis has a reserved or brand list (or a sunrise period) and a dispute policy, and counsel has advised on permanent personal data (GDPR erasure requests) and on publishing accusations on chain.
- **Effort:** S–M. **Who:** Counsel, Team.

## 3. Should-fix before launch (not strictly blocking)

**SF-1. Predictable leaders.**
- **Missing:** `RoundRobin` (`consensus.rs:785`) publishes the leader schedule, and validator p2p addresses are public in genesis. That makes it cheap to DoS whoever leads next.
- **Done when:** the leader order is seeded per epoch from CRY-3's beacon, and sentry nodes hide validator IPs.
- **Effort:** M.

**SF-2. Clock skew.**
- **Missing:** a block more than 1 s ahead of the verifier's clock is rejected (`consensus.rs:54, 358-360`). A validator whose clock lags by over 1 s can't vote, and nothing measures skew.
- **Done when:** operator docs require NTP, skew has a metric and an alert, and the tolerance is set using a simulator test with skewed clocks.
- **Effort:** S.

**SF-3. Binary tx and block encoding.**
- **Missing:** txs travel and are stored as hex JSON (`consensus.rs:71-86, 312`). That's about twice the binary size fees are priced on (`types.rs:592-594`), and the JSON isn't canonical (CV-5).
- **Done when:** a canonical binary encoding replaces it. This is a consensus format: change it before the freeze or live with it.
- **Effort:** M.

**SF-4. Execution holds the global lock through fsync and mempool revalidation.**
- **Missing:** `execute` appends and fsyncs two files, recomputes the root and re-applies up to 5,000 mempool txs (`chain.rs:1735-1782, 1874-1905`). Every API call and every consensus verify waits for it.
- **Done when:** I/O and revalidation run outside the lock, and latency is measured.
- **Effort:** M (overlaps CV-1 and SEC-4).

✅ **SF-5. Snapshot threads can pile up.**
- **Missing:** each checkpoint spawns a new thread holding a full state clone (`chain.rs:1826-1833`). If writing takes longer than 100 blocks, threads accumulate.
- **Done when:** one snapshot worker handles every checkpoint and skips while busy. ✅ Done: a checkpoint is skipped (with a warning) while the previous snapshot is still being written.
- **Effort:** S.

✅ **SF-6. Release builds don't check overflow.**
- **Missing:** `node/Cargo.toml` has no `[profile.release]`, so `+=` and `-=` on balances and supply wrap silently (`chain.rs:162-163, 508, 537, 698-699`).
- **Done when:** release builds set `overflow-checks = true`, or every balance and supply path uses checked arithmetic, with tests. ✅ Done for the node crate (`[profile.release.package.blockula]`); checked arithmetic on balance paths is still worth doing.
- **Effort:** S.

**SF-7. One misconfiguration turns off every rate limit.**
- **Missing:** loopback callers are never limited (`api.rs:105, 671`), so a reverse proxy on localhost without `--client-ip-header` disables every limit. Limits are also in memory and per node (PLAN.md "Known limits").
- **Done when:** nothing is unlimited by default, and the node warns at startup when it looks like it's behind a local proxy.
- **Effort:** S.

**SF-8. Closed records live forever in the root.**
- **Missing:** hunts, claims, Séances, grants and every validator registration are never pruned, and are hashed into every block (`chain.rs:1045-1075`; `poc.rs:312-339`). Some admission checks scan every claim (`poc.rs:347-349, 715`).
- **Done when:** archived records leave the hot state (with CV-1), and admission is O(log n).
- **Effort:** M.

**SF-9. Names: renewal, transfer and pricing.**
- **Missing:** there's no renewal, though PLAN.md "Tokenomics" promised it for Phase 1. There's no action to transfer or release a name (`types.rs:32-156`). Prices are fixed in FANG (`types.rs:618-624`). Adding renewal after genesis changes the terms for existing holders.
- **Done when:** renewal, transfer and pricing are decided (DEC-11) and built before genesis.
- **Effort:** M.

**SF-10. Full nodes can't use p2p.**
- **Missing:** discovery only admits tracked validators (PLAN.md "Network and sync"), so full nodes sync and forward over HTTP. Every testnet validator's `http_addr` is the one public RPC (`deploy/launch.sh:74-76`).
- **Done when:** a documented sentry architecture exists, or full nodes are admitted as secondary peers.
- **Effort:** L.

**SF-11. No multisig accounts or scoped session keys.**
- **Missing:** any one owner key fully controls its account (`chain.rs:628-666`). Session keys can't be limited to particular modules (PLAN.md "Accounts"). Treasuries, foundations and exchanges need M-of-N.
- **Done when:** M-of-N accounts exist (GOV-1 needs a minimal version anyway) and session keys can be scoped.
- **Effort:** M.

**SF-12. Secret material isn't zeroized.**
- **Missing:** seeds sit in plain byte arrays (`crypto.rs:55-58`; `pq.rs:124-128`).
- **Done when:** key types use `zeroize`, and key files can optionally be encrypted at rest.
- **Effort:** S.

**SF-13. Contribution attestations depend on validators' balances and GitHub tokens.**
- **Missing:** attestation fees come from each validator's consensus-key account. If a third of the set can't pay, claims never settle (PLAN.md "Known limits"). Each validator also needs a `GITHUB_TOKEN` (`github.rs:15-17`). For the testnet, `deploy/set-github-token.sh` sets it on every app (public-read-only tokens only).
- **Done when:** the attestation fee payer is configurable (or attestations are fee-free with a cap), and each operator's token handling is documented.
- **Effort:** S.

**SF-14. Operator documentation.**
- **Missing:** there's no reference hardware spec, port and firewall guide, NTP requirement, disk sizing or sentry setup.
- **Done when:** all of those are documented and the commands work.
- **Effort:** S–M.

**SF-15. The explorer has no CSP.**
- **Missing:** the explorer is served from the API's origin (`api.rs:259`). It escapes on-chain strings consistently (`esc()` in `explorer.html`), but sets no CSP.
- **Done when:** the node serves the explorer with a CSP.
- **Effort:** S.

## 4. Fine to ship after launch

- STARK signature aggregation (PLAN.md "Roadmap", Phase 4).
- Light-client mode, once CV-1 provides proofs.
- Background history backfill behind fast-synced nodes, and STARK history proofs (PLAN.md "Network and sync", "Next").
- WASM/RISC-V contracts.
- Ghost Mode and The Crypt, if DEC-2 says launch without them.
- Covens, Seasons, the juror UI, acceptance-test Hunts, fork royalties, conviction-voting refinements and private ballots. This assumes the pool is off or governed at genesis.
- More SDKs, explorer polish, and indexer features beyond what wallets need.

## 5. Proposed sequence

| Phase | Work | Exit gate | Rough length |
|---|---|---|---|
| 0. Decide and book | Make the section 6 decisions. Book auditors and engage counsel; both have lead times. Ship the S items: CV-4, GOV-2, WAL-5, SF-7 (done: the CV-6 torn-line fix, SF-5, SF-6, SEC-5's SECURITY.md, the runbook command). Start OPS-7. | Decisions written into PLAN.md. Audit slot booked. | 2–4 weeks |
| 1. Code-complete | Tracks A–G below, in parallel. | Every blocker closed in code. Consensus formats frozen: tx encoding (including expiry), state encoding and root, genesis schema, fee and issuance rules. Fuzzers clean for 2 weeks. The frozen build has run on a public testnet for ≥ 4 weeks, including one height-activated upgrade. | 5–7 months |
| 2. Audit | Protocol audit (node, consensus, state, sync, crypto use) and a wallet/SDK/MCP review. Fix, then re-review. The bug bounty opens. | No open critical or high findings. Reports published. | 2–3 months, overlapping phase 3 |
| 3. Incentivized testnet | Independent operators run the audited build with real stake weighting. Exercise: injected equivocation gets slashed; two upgrades through OPS-3; chaos (kills, partitions, clock skew); spam at the fee-market limits; state seeded to mainnet scale; outsiders fast-sync from signed checkpoints; an incident-response tabletop. | 30 consecutive days with no halt or manual intervention. Every operator upgraded on their own twice. No state-root divergence. Alerting caught every injected fault. | ≥ 3 months |
| 4. Genesis ceremony | Counsel signs off. Allocation finalized. Operators generate keys offline and submit them. Several parties rebuild the deterministic genesis. Two builders reproduce the release, which is then signed. Dry run on a throwaway chain id. | Independent parties get the same genesis hash and binary hash. All operators online for the dry run. | 2–4 weeks |
| 5. Launch | Start at the announced time. Features that aren't ready (DEC-1, DEC-2) stay off until an activation height. | | |

Phase 1 tracks:
- **A. State (critical path):** CV-1 and CV-6, then SF-4, SF-5 and SF-8, then SEC-4's read path on the new store. Fuzzing (SEC-3) runs from day one.
- **B. Validators (critical path):** CV-2, CV-3, CV-4 and CV-5, then GOV-1 (it needs CV-2's weights if governance votes by stake), then OPS-3.
- **C. Crypto and network:** CRY-1 with Commonware; CRY-2; CRY-3 only if the pool is live at genesis; SF-1; SF-10.
- **D. Economics and tx format:** TOK-1, TOK-2, TOK-4, SF-3 and SF-9, then TOK-3 once GOV-1 exists. Each one changes a consensus format, so this track gates the freeze.
- **E. Clients:** WAL-1 to WAL-4.
- **F. Ops:** OPS-1 recruiting (start in phase 0), OPS-2, OPS-4 to OPS-7.
- **G. Legal:** LEG-1 to LEG-3. Independent of code, but must finish before phase 4.

Tracks A and B set the date. Scope cuts shorten B and D, but not A: no contribution pool at genesis (DEC-1), no Ghost Mode (DEC-2), or a permissioned equal-weight set (CV-2 option b). With them, the total drops from about 12 months to about 9.

## 6. Decisions the maintainers must make

| ID | Decision | Options and notes | Needed by |
|---|---|---|---|
| DEC-1 | Scope at genesis | (a) Transfers, names and session keys only. (b) Plus Hunts. (c) Plus the contribution pool. The pool brings GOV-3, CRY-3, GitHub-as-oracle and LEG-2. Suggest (a), or (b) with creator-only awards, then turn on the pool at an activation height once juries replace the admin levers. | Phase 0 |
| DEC-2 | Ghost Mode | It's Phase 2 and not started. Suggest launching without it and saying so. Adding it later is a new module plus its own legal review. | Phase 0 |
| DEC-3 | Validator set composition | Genesis size, the cap (`max_validators` is 32), independence criteria and a Nakamoto target. With equal weights, halting the chain takes ⌊(n−1)/3⌋+1 validators, so PLAN.md's target of ≥ 10 needs ≥ 28 independent operators. With no signature aggregation, a 32-validator certificate is ~22 × 3.3 KB ≈ 73 KB. | Phase 0 (recruiting takes months) |
| DEC-4 | Staking model | Stake-weighted or permissioned equal-weight (CV-2); delegation; minimum bond (10,000 FANG by default without a faucet, `chain.rs:71-73`); unbonding period (≥ the evidence and weak-subjectivity window); epoch length (600 blocks = 10 minutes today); slashing amounts (PLAN.md: heavy for double-signing, light for downtime). | Phase 0 |
| DEC-5 | Supply and allocation | Initial supply (the testnet uses 1.1B). The split: foundation and treasury, team with vesting, operators, community, pool. Whether there's any sale (counsel). Issuance parameters (6% → 1.5% tail; 70/20/10). Fee parameters. | Phase 0; final in phase 4 |
| DEC-6 | State rent and deposits | Which records pay, how much, and whether deposits are refundable (PLAN.md says refundable). This shapes CV-1's design. | Start of phase 1 |
| DEC-7 | Chain id and address prefix | For example `blockula-1`. The chain id is in every signature and in the consensus namespace (`consensus.rs:65-67`), so it's permanent. Decide whether testnets get a prefix other than `boo` (`crypto.rs:11`) so their addresses can't be confused with mainnet ones. The mainnet prefix can't change after genesis. | Start of phase 1 |
| DEC-8 | Wallet domain (passkey rpId) | Permanent. It must be a domain the project's entity owns, not `*.vercel.app`. | Phase 1, before the first mainnet passkey |
| DEC-9 | PQ transport | Either launch waits for CRY-1, or launch on X25519 and drop "no elliptic curves anywhere" from README and ONE_PAGER. | Phase 0 |
| DEC-10 | Admin powers and faucet on mainnet | Admin powers: none; M-of-N with a timelock and a sunset date; or full governance at genesis. Faucet: suggest none (`faucet: false`). | Phase 0 |
| DEC-11 | Name policy | Reserved or brand list, auctions for 3- and 4-character names, renewal and transfer (SF-9, LEG-3). | Phase 1 |
| DEC-12 | Block time and throughput | Today: 1 s blocks, ~5.4 KB txs, ~388 txs per block. This sets disk growth (OPS-6), fee-market parameters and validator hardware. | Phase 1 |
| DEC-13 | Governance voting weight | PLAN.md's √stake + Soul gives admin-granted Soul voting power during bootstrap. Options: stake only at first, or a published Soul genesis ledger. | Phase 1 |
| DEC-14 | Hosting policy | Whether foundation nodes stay on Fly, and whether one provider may host more than ⌊(n−1)/3⌋ of the set. | Phase 0 |

## Appendix: numbers used here

| Quantity | Estimate | How |
|---|---|---|
| Transfer size | ~5.44 KB | ~181 B of signing bytes + 1,952 B public key + 3,309 B signature (`types.rs:592-594`) |
| Minimum fee for a transfer | ~64,400 drops | `10_000 + 10 × 5,442` (`types.rs:613-615`) |
| Transfers per block | ~388 | Hex JSON is ~10.8 KB per tx; 4 MiB block cap (`consensus.rs:51`). The 512-tx cap doesn't bind. |
| Cost to fill every block for a day | ~2,200 FANG | 86,400 × 388 × 64,400 drops |
| 2M new accounts | ~1.4 h, ~130 FANG in fees, plus 20,000 FANG moved into the accounts | 0.01 FANG minimum per new account (`types.rs:682`) |
| 10M new accounts | ~7.2 h, ~650 FANG in fees, plus 100,000 FANG moved | The flat root at ~10 ms per 100k accounts (PLAN.md) reaches ~1 s |
| Disk per empty block, 4 validators | ~0.6 KB (~50 MB/day, ~19 GB/year) | ~330 B `blocks.jsonl` + ~170 B `cblocks.bin` + a ~10 KB certificate per 100 heights. With 32 validators, certificates alone add ~0.73 KB per block. |
| Junk from one Byzantine leader among 32 | ~10 GB/day | 2,700 leader slots/day × 4 MiB (CV-5) |
| Validators needed to halt (equal weight) | ⌊(n−1)/3⌋+1 | n=4 → 2; n=32 → 11 |
| Admin takeover | 28 of 32 | 4 active + 28 approved = the 32 cap; 28/32 > 2/3 (GOV-1) |

The testnet facts come from `GET /api/status` and `/api/genesis` on blockula-testnet.fly.dev, 2026-10-14 around 15:30 UTC.
