# @blockula/sdk

🧛 TypeScript SDK for **Blockula**, a post-quantum blockchain. It covers ML-DSA-65 (FIPS 204) wallets, `.boo` names, transfers, multi-key accounts, agent session keys, and passkey-unlocked wallets.

```bash
npm install @blockula/sdk
```

```ts
import { Client, Wallet } from "@blockula/sdk";

const wallet = Wallet.generate(new Client("http://localhost:7070"));
await wallet.faucet();                        // test chains: 100 FANG
await wallet.registerName("vlad");            // vlad.boo
await wallet.send("alice.boo", "1.5", { memo: "trick or treat" });

// Let an agent spend up to 10 FANG/day from this account (enforced on-chain)
await wallet.addSessionKey(agentKeyId, "claude", { limit: "10", periodBlocks: 86_400, expiresInBlocks: 30 * 86_400 });
```

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
- Every transaction carries `valid_until`, the last block it may execute in. The wallet sets it to the current height plus `validForBlocks` (default 600 blocks, at most the chain's `max_tx_validity_blocks`, an hour of blocks). A transaction past `valid_until` can never execute, so it's safe to report it as expired.
- `txBytes(tx)` is the binary wire form nodes gossip and put in blocks, and `txId(tx)` its id, known before you submit.
- Every transaction names the chain it's for by `chain_id` and `genesis_hash` (the chain instance, from `/api/status`), so it can't replay on a relaunched chain that reuses the id. `wallet.build` fills both in; to sign offline, put the chain's `genesis_hash` in the `UnsignedTx` you pass to `wallet.sign`.

Addresses are bech32m with the chain's prefix: `boo1…` on mainnet, `tboo1…` on public testnets. `isAddress(s)` accepts any known prefix and `isAddress(s, "tboo")` exactly one; `addressFromPublicKey(pk, prefix)` renders one. A `Client` learns its chain's prefix from `/api/status` (`await client.addressPrefix()`), and wallets using that client sign with it (`wallet.address`, `wallet.account`). Pin it with `new Client(rpc, { prefix: "boo" })` to refuse a node on another network (see "Which chain?" below). Nodes reject addresses of another network's prefix.

- `send("alice.boo", …)` resolves the name first and signs the **address**, so whoever you checked is exactly who gets paid.
- `wallet.address` uses the chain's prefix once the client knows it. Before the first request it shows the `boo` form, so `await client.addressPrefix()` first if you display it on a testnet.
- Node errors throw `ApiError` (`.status`: 429 means rate limited). A transaction that fails at execution throws `TxFailedError` from `waitForTx`.
- Pass `rpId` to `createPasskeyWallet`/`unlockPasskeyWallet` to pin wallets to your production domain. Passkeys are bound to that hostname for good.

### Staking, governance and names (protocol v1)

```ts
import { Client, Wallet, proposal, stakeSummary } from "@blockula/sdk";

await wallet.delegate(validatorAddress, "100");               // stake in a validator's pool
const stake = stakeSummary(await client.staking(wallet.account), (await client.status()).height);
await wallet.undelegate(validatorAddress, "all");             // or a number of pool shares
await wallet.withdrawUnbonded();                              // once the unbonding period is over

await wallet.propose(proposal.paramChange("max_validators", 40), { title: "More validators", url: "https://forum.example/p/1" });
await wallet.vote(7, true, "10", 2);                          // aye, 10 FANG locked at 2× conviction
await wallet.unlock();                                        // release vote locks that ended

await wallet.renewName("vlad", 2);                            // names are registered a year at a time
await wallet.transferName("vlad", "alice.boo");
await wallet.multisigCreate([phoneKeyId, laptopKeyId, kitKeyId], 2);
```

- Every tag 30–49 has a helper: `delegate`, `undelegate`, `redelegate`, `withdrawUnbonded`, `updateValidator`, `unjail`, `propose`, `vote`, `unlock`, `multisigCreate` / `multisigPropose` / `multisigApprove`, `renewName`, `transferName`, `releaseName`, `claimDistribution`, and the low-level validator ones (`reportEquivocation`, `beaconCommit`, `beaconReveal`, `attestRevert`). They check their arguments before signing, resolve `.boo` names to the addresses they sign, and wait for the block.
- `proposal.*` builds every `ProposalKind`: `paramChange`, `treasurySpend`, `distribution`, `setRepo`, `grantSoul`, `flagSybil`, `rejectClaim`, `clawback`, `unlinkGithub`, `refundHunt`, `scheduleUpgrade`, `cancelUpgrade`, `activateFeature`, `deactivateFeature`, `text`. `propose` bonds the chain's `proposal_deposit` unless you pass `deposit`.
- `convictionWeight(amount, conviction)` is a vote's weight: 0.1× with no lock, then 1×–6× for 1, 2, 4, 8, 16 or 32 lock periods (`CONVICTION_PERIODS`).
- Reads: `client.staking(account)`, `client.proposals()`, `client.proposal(id)`, `client.params()`, `client.fees()`, `client.upgrade()`, `client.distribution(id)`, `client.multisig(account)`. `stakeSummary`, `proposalSummary` and `paramsOf` turn them into stable shapes, tolerating renamed or missing fields.

### Which chain? Pinning and proofs

```ts
const client = new Client("https://rpc.example", { chainId: "blockula-1", genesisHash: "…", prefix: "boo" });
```

A pinned client checks every `/api/status` it reads, including the one each signature is made against, and throws `ChainMismatchError` instead of signing for another chain.

A node can still lie about the state of the right chain. Proven reads check the node's Merkle proof (`/api/proof`) against its state root, and ask independent nodes (`witnesses`) for that block's root:

```ts
const others = [new Client("https://rpc-2.example"), new Client("https://rpc-3.example")];
const acct = await client.provenAccount("boo1…", { witnesses: others });  // { balance, nonce, keys, height, root } or null
const name = await client.provenName("vlad.boo", { witnesses: others });  // { owner, registered_at, … } or null
```

`verifyProof(root, namespace, key, value, proof)` is the bare check (a port of `node/src/state/tree.rs`, tested against the node's own vectors), and `provenRecord(namespace, key)` works for any record.

`verifyReleaseSignature(sig, checksumsFile, trustedKeyId)` checks a release's offline ML-DSA-65 signature (docs/OPERATIONS.md "Releases").

The public testnet is `https://blockula-testnet.fly.dev`, and the wallet is https://blockula-wallet.vercel.app.

Tests: `npm run test:vectors` (offline: passkey derivation, state-proof vectors from the node, every protocol v1 helper against a fake node, pinning, release signatures), `npm run test:kat` (ML-DSA known answers), `npm test` (end to end against `blockula dev`).

⚠️ Early and unaudited: test chains only.
