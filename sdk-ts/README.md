# @blockula/sdk

🧛 TypeScript SDK for **Blockula**, a post-quantum blockchain. It covers ML-DSA-65 (FIPS 204) wallets, `.boo` names, transfers, multi-key accounts, agent session keys, and passkey-unlocked wallets.

```bash
npm install @blockula/sdk
```

```ts
import { Client, Wallet, encodeKeyGrant } from "@blockula/sdk";

const wallet = Wallet.generate(new Client("http://localhost:7070"));
await wallet.faucet();                        // test chains: 100 FANG
await wallet.registerName("vlad");            // vlad.boo
await wallet.send("alice.boo", "1.5", { memo: "trick or treat" });

// Let an agent spend up to 10 FANG/day from this account (enforced on-chain). The agent's key
// consents to joining this account first: its grant code (the MCP's use_account answers with one).
const agentGrantCode = encodeKeyGrant(await agent.keyGrant(wallet.account));
await wallet.addSessionKey(agentGrantCode, "claude", { limit: "10", periodBlocks: 86_400, expiresInBlocks: 30 * 86_400 });
```

Adding a key needs the key's consent (audit gap-7-2): `AddKey` carries the key's public key and its signature over the chain, the account and its key id, so nobody can put your key on their account (and pad `client.keyAccounts(keyId)`, which pages with `{ after }`). The key's holder makes it with `keyGrant(account)`; `encodeKeyGrant`/`decodeKeyGrant` turn it into a pasteable `bkgrant1.…` code, `verifyKeyGrant` checks one, and `addOwnerKey`/`addSessionKey` take either and refuse a grant made for another account or chain.

Votes, multisig approvals and sealed Séance judgments bind what they approve (audit gap-7-6): `vote(id, aye, amount, conviction, { proposal })` signs `proposalDigest(kind, title, url)` of the proposal you showed, `multisigApprove(id, { action })` signs `actionDigest(action)`, and `sealVotes(…, { pairs })` signs `seancePairsDigest` of the pairs and PRs the juror compared (each read from the client when not given). The chain refuses a digest that doesn't match what's stored, so a node that misreports what's being approved makes it fail instead of landing elsewhere.

Show the voter or co-signer what the digest covers, rendered by the client, never the node's `summary` text (no digest covers it, so a node can make it say anything): `describeKind(kind)` and `describeAction(action)` say what a proposal or a multisig action does in plain English, every amount spelled out (`"Pay 10,000,000 FANG from the treasury to boo1…"`) and every recipient in full, and `proposalSummary(raw)` adds `does` (that description) and `digest` (what a vote binds; `null`, with `digest_error`, when the client can't read the proposal exactly, e.g. a number past 2^53: don't vote on it from JS). They never throw on content they can't read; they say so. Pass `{ freeText: false }` to leave reasons and labels out (to show them apart, marked as the proposer's words). If you show a digest and vote later, compare it with the one you sign. A multisig's pending `vote` binds its proposal by digest, not by id: `reviewAction(action, { proposal: await client.proposal(action.proposal_id) })` describes the proposal only when its content has the digest the vote binds, and says otherwise (`readable: false`, as for anything it describes as unreadable): don't offer to approve such an action. `multisigApprove(id, { action, proposal })` checks the same before signing: a vote is approved only if the proposal shown (else one read then) has exactly the content it binds.

In the browser, a passkey unlocks the wallet. There's no seed phrase, and the passkey's (non-quantum-safe) signature is never used on-chain. Its PRF output derives the ML-DSA key:

```ts
import { createPasskeyWallet, unlockPasskeyWallet } from "@blockula/sdk/passkey";
const wallet = await createPasskeyWallet({ username: "vlad" });
```

Contribution pool (Proof of Contribution):

```ts
await wallet.submitClaim("jackdishman/blockula", 34);       // a merged PR by your linked GitHub handle
const s = await client.seance((await client.contributions()).open_seance.id);   // jurors: your pairs
const sealed = await juror.sealVotes(s.id, "ab=", [17]);  // a/b/= per pair, spam flags; keep sealed.salt
await juror.revealVotes(s.id, sealed.votes, sealed.spam, sealed.salt);  // after the sealing phase
await wallet.withdrawVested();
```

Amounts are FANG strings (`"1.5"`) or bigint drops (1 FANG = 10⁹ drops). Every transaction result includes a plain-English `summary`.

Fees, tips and expiry (protocol v1):

```ts
await wallet.send("alice.boo", "1.5", { tip: "0.001", validForBlocks: 120 });
wallet.tip = 0n;                 // defaults for every tx this wallet builds
wallet.validForBlocks = 600;
```

- A transaction is charged its block's base fee (`baseFee(tx, slot)`: priced per byte, burned; it rises while blocks are full and is fixed 17 blocks ahead) plus its `tip` (paid to the block's proposer; blocks take the highest tip per byte first). The wallet sets `fee` to the base fee at `client.fees()`'s `suggested` plus the tip; only the actual charge (`txCharge(tx, slot)`) is debited. On an idle chain the base fee is `minFee(tx)`.
- A tip is at most `tip_cap_multiple` (20) × the base fee of the block that includes it (`maxTip(tx, slot, multiple)`); `wallet.build` refuses a bigger one, priced at `client.fees()`'s `lowest`. It's paid only from FANG the account could transfer (vesting and vote locks hold it), and a multisig's owner keys and a validator's consensus key acting for its owner can't tip at all.
- Every transaction carries `valid_until`, the last block it may execute in. The wallet sets it to the current height plus `validForBlocks` (default 600 blocks, at most the chain's `max_tx_validity_blocks`, an hour of blocks). A transaction past `valid_until` can never execute, so it's safe to report it as expired.
- `txBytes(tx)` is the binary wire form nodes gossip and put in blocks, and `txId(tx)` its id, known before you submit.
- Every transaction names the chain it's for by `chain_id` and `genesis_hash` (the chain instance, from `/api/status`), so it can't replay on a relaunched chain that reuses the id. `wallet.build` fills both in; to sign offline, put the chain's `genesis_hash` in the `UnsignedTx` you pass to `wallet.sign`.

Addresses are bech32m with the chain's prefix: `boo1…` on mainnet, `tboo1…` on public testnets. `isAddress(s)` accepts any known prefix and `isAddress(s, "tboo")` exactly one; `addressFromPublicKey(pk, prefix)` renders one. A `Client` learns its chain's prefix from `/api/status` (`await client.addressPrefix()`), and wallets using that client sign with it (`wallet.address`, `wallet.account`). Pin it with `new Client(rpc, { prefix: "boo" })` to refuse a node on another network (see "Which chain?" below). Nodes reject addresses of another network's prefix.

- Names in a signature (audit gap-7-1). Where the chain resolves a name itself (`send`, `awardHunt`, `vouch`/`unvouch`, `grantSoul`, `flagSybil`, `clawback`, `transferName`, proposal targets), a client with `witnesses` (below) signs the name's proven owner, and one without signs the **name itself** (`alice.boo`): the chain pays whoever owns it when the tx runs, so a lying node can't point it at another account now. It could still hold the signed tx and release it later in its validity window, which counts from the height that node reported, by when the name may have lapsed and been registered again: pay a name that's about to expire by address. `Wallet.build` refuses a height more than `maxHeadLeadBlocks` (30) ahead of the lowest head the witnesses report (every witness must answer, so one honest witness is enough), or past what a `genesisClock: { createdAtMs, blockTimeMs }` pin allows by this machine's clock (`HeightAheadError`, nothing signed); without either the height is the node's word. `client.recipient(name)` says which (`signed`, `byChain`, and `how` to show before signing); the address it reports for an unproven name is the node's word, for display. Where an address is needed (`delegate`, `undelegate`, `redelegate`, `unjail`, `actAs`, `keyGrant`), a name is resolved (`client.resolution`: `proven` and `how`), and on mainnet nothing is signed with one only a single node vouched for: it throws `UnprovenNameError` (checked against the very status each tx is signed for). Pass the address once its owner has confirmed it, or give the client witnesses.
- `wallet.address` uses the chain's prefix once the client knows it. Before the first request it shows the `boo` form, so `await client.addressPrefix()` first if you display it on a testnet.
- Node errors throw `ApiError` (`.status`: 429 means rate limited). A transaction the node reports as failed throws `TxFailedError` from `waitForTx` (`dropped` says whether it ran: see "Timeouts and pending transactions").
- Pass `rpId` to `createPasskeyWallet`/`unlockPasskeyWallet` to pin wallets to your production domain. Passkeys are bound to that hostname for good.

### Staking, governance and names (protocol v1)

```ts
import { Client, Wallet, proposal, stakeSummary } from "@blockula/sdk";

await wallet.delegate(validatorAddress, "100");               // stake in a validator's pool
const stake = stakeSummary(await client.staking(wallet.account), (await client.status()).height);
await wallet.undelegate(validatorAddress, { amount: "10" });  // the chain converts it to shares; or "all", or a share count
await wallet.withdrawUnbonded();                              // once the unbonding period is over

await wallet.propose(proposal.paramChange("max_validators", 40), { title: "More validators", url: "https://forum.example/p/1" });
await wallet.vote(7, true, "10", 2);                          // aye, 10 FANG locked at 2× conviction
await wallet.unlock();                                        // release vote locks that ended

await wallet.renewName("vlad", 2);                            // names are registered a year at a time
await wallet.transferName("vlad", "alice.boo");               // an offer: alice.boo accepts with acceptName("vlad")
await wallet.multisigCreate([phoneKeyId, laptopKeyId, kitKeyId], 2);
```

- Every tag 30–49 has a helper: `delegate`, `undelegate`, `redelegate`, `withdrawUnbonded`, `updateValidator`, `unjail`, `propose`, `vote`, `unlock`, `multisigCreate` / `multisigPropose` / `multisigApprove`, `renewName`, `transferName` (an offer the recipient takes with `acceptName`; `withdrawNameOffer`), `releaseName`, `claimDistribution`, and the low-level validator ones (`reportEquivocation`, `beaconCommit`, `beaconReveal`, `attestRevert`); tags 60–61 are a set of one or two's `admitValidator` (a member's consensus key admits a candidate) and `validatorReady` (candidate nodes send it). They check their arguments before signing, resolve `.boo` names to the addresses they sign, and wait for the block.
- `proposal.*` builds every `ProposalKind`: `paramChange`, `treasurySpend`, `distribution`, `setRepo`, `grantSoul`, `flagSybil`, `unflagSybil` (its appeal), `rejectClaim`, `clawback`, `unlinkGithub`, `refundHunt`, `scheduleUpgrade`, `cancelUpgrade`, `activateFeature`, `deactivateFeature`, `text`. `propose` bonds the chain's `proposal_deposit` unless you pass `deposit`.
- `convictionWeight(amount, conviction)` is a vote's weight: 0.1× with no lock, then 1×–6× for 1, 2, 4, 8, 16 or 32 lock periods (`CONVICTION_PERIODS`).
- Reads: `client.staking(account)`, `client.proposals()`, `client.proposal(id)`, `client.params()`, `client.fees()`, `client.upgrade()`, `client.distribution(id)`, `client.multisig(account)`. `stakeSummary`, `proposalSummary` and `paramsOf` turn them into stable shapes, tolerating renamed or missing fields.

### Which chain? Pinning and proofs

```ts
const client = new Client("https://rpc.example", { chainId: "blockula-1", genesisHash: "…", prefix: "boo" });
```

A pinned client checks every `/api/status` it reads, including the one each signature is made against, and throws `ChainMismatchError` instead of signing for another chain.

A node can still lie about the state of the right chain. Give a client witnesses and the names it resolves for signatures are proven (without them, see "Names in a signature" above) (`new Client(rpc, { …pins, witnesses: [new Client(rpc2, pins), …], requireProvenNames: true })`; `requireProvenNames` refuses any name it can't prove). `Wallet.propose`'s default deposit must be the same on the node and every witness, and at most `maxDeposit` (default `DEFAULT_MAX_PROPOSAL_DEPOSIT`, 1,000 FANG): a node can report any deposit, and all of it burns if the vote misses quorum (audit gap-7-5). Session-key and hunt expiries count from the same status read as `valid_until`, so a node lying about the height can stretch them by at most `max_tx_validity_blocks` (audit gap-7-3).

Proven reads check the node's Merkle proof (`/api/proof`) against its state root, ask independent nodes (`witnesses`) for that block's root, and require the proof to be fresh:

```ts
const others = [new Client("https://rpc-2.example"), new Client("https://rpc-3.example")];
const acct = await client.provenAccount("boo1…", { witnesses: others });  // { balance, nonce, keys, height, root, head } or null
const name = await client.provenName("vlad.boo", { witnesses: others });  // { owner, registered_at, expires_at, active, … } or null
```

- **Fresh, not just genuine.** Witnesses confirm any root the chain ever had, so a lying node could answer with a real but old record: a name's previous owner, a balance before a spend, a key since removed. The proof's block may be at most `maxLagBlocks` (default 10) behind the newest head the node and its witnesses report, or it throws `StaleProofError`. Without witnesses only the node's own head is compared, which a lying node controls.
- **Expired names don't resolve.** `provenName` returns `null` once a name's `expires_at` has passed (its grace period included), like `client.resolve`. `{ includeExpired: true }` returns the record with `active: false`; never pay its `owner`.

`verifyProof(root, namespace, key, value, proof)` is the bare check (a port of `node/src/state/tree.rs`, tested against the node's own vectors), and `provenRecord(namespace, key)` works for any record (check its `height` against `head` if you call it with your own options).

### Timeouts and pending transactions

Every request has a timeout (`new Client(rpc, { timeoutMs })`, default 15 s, at most `MAX_TIMEOUT_MS` = 2^31 − 1 ms, since a longer timer fires at once): a node that never answers fails with `RequestTimeoutError` instead of hanging. `submit` (and every helper that sends) tells you what a failure means:

- `ApiError`: the node refused the tx. It wasn't admitted, so nothing was spent.
- `TxFailedError` with `dropped` false: it executed in a block and failed there (its fee was charged). It can't execute again.
- `TxFailedError` with `dropped` true: this node dropped it from its mempool without running it (evicted by a higher tip, replaced, not included, or an earlier tx from the account was dropped). Nothing was charged, but another node may still hold it and include it until its `valid_until`: check `client.tx(id)` before sending again, or the payment can go out twice.
- `TxPendingError`: it was admitted but isn't final yet, and it can still execute until its `valid_until`. Check `client.tx(err.id)` before sending again, or the payment can go out twice.
- `RequestTimeoutError` or a network error while submitting: unknown, the tx may have arrived. Same advice.

### Encoding

`signingBytes` and `txBytes` encode a tx exactly as the node does (`node/src/types.rs`, checked offline against the node's vectors in `test/tx-vectors.json`). They take integers as the node's JSON parser does, as JSON numbers or strings of digits within the field's width, and refuse anything else (a negative or oversized amount, `"0x10"`, `" 1"`, a fraction) instead of wrapping it into a different tx.

`verifyReleaseSignature(sig, checksumsFile, trustedKeyId)` checks a release's offline ML-DSA-65 signature (docs/OPERATIONS.md "Releases").

The public testnet is `https://blockula-testnet.fly.dev`, and the wallet is https://testnet.blockula.app.

Tests: `npm run test:vectors` (offline: passkey derivation, state-proof and tx-encoding vectors from the node, every protocol v1 helper against a fake node, pinning, proof freshness, timeouts, release signatures), `npm run test:kat` (ML-DSA known answers), `npm test` (end to end against `blockula dev`).

⚠️ Early and unaudited: test chains only.
