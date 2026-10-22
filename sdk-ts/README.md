# @blockula/sdk

🧛 TypeScript SDK for **Blockula**, a blockchain with post-quantum signatures (ML-DSA-65, FIPS 204). It covers wallets, `.boo` names, transfers, multi-key accounts, agent session keys, and passkey-unlocked wallets.

> **Before you use it on mainnet,** read [Using the SDK on mainnet](#using-the-sdk-on-mainnet). Exchanges, custodians and bots: [docs/INTEGRATION.md](https://github.com/jackdishman/blockula/blob/main/docs/INTEGRATION.md).

```bash
npm install @blockula/sdk
```

```ts
import { Client, Wallet, encodeKeyGrant } from "@blockula/sdk";

const wallet = Wallet.generate(Client.testnet());  // the public testnet, every pin set: tboo1… addresses
await wallet.faucet();                        // test chains: 100 test FANG
await wallet.registerName("vlad");            // vlad.boo
await wallet.send("alice.boo", "1.5", { memo: "trick or treat" });

// Let an agent spend up to 10 FANG/day from this account (enforced on-chain). The agent's key
// consents to joining this account first: its grant code (the MCP's use_account answers with one).
const agentGrantCode = encodeKeyGrant(await agent.keyGrant(wallet.account));
await wallet.addSessionKey(agentGrantCode, "claude", { limit: "10", periodBlocks: 86_400, expiresInBlocks: 30 * 86_400 });
```

On a `blockula dev` chain, use `Client.local()` (or `new Client("http://localhost:7070")`).

## Which network?

```ts
Client.testnet();                                   // the public testnet
new Client({ network: "testnet" });                 // the same
new Client("http://localhost:7171", { network: "testnet" });  // your own node, held to the testnet's pins
Client.local();                                     // blockula dev on this machine
new Client("https://<a mainnet node>", { network: "mainnet" });  // mainnet: see below first
```

`NETWORKS` lists what each preset pins: the chain id, the genesis hash, the address prefix and the genesis clock. A client made for a network checks them on every status it reads, including the one each signature is made against, and refuses a node on another chain (`ChainMismatchError`). The prefix is known up front, so `wallet.address` is `tboo1…` on the testnet before the first request. Pins you pass as well must agree with the network's.

**Mainnet is opt-in.** A wallet signs nothing on `blockula-1` unless its client was made with `network: "mainnet"`, a `chainId: "blockula-1"` pin, or `allowMainnet: true` (for code that pins the chain itself). Otherwise it throws `MainnetNotAllowedError`, so a script pointed at a mainnet node by mistake moves nothing. Until a release builds in mainnet's public RPC and genesis hash, pass the URL of a mainnet node you trust.

<!-- D4 (decided): this notice is published as provisional, under counsel review. Its wording is in this one section; change it here. -->
## Using the SDK on mainnet

> **Provisional, under counsel review. Not legal advice.**

- **Blockula mainnet (`blockula-1`) is experimental.** Governance can upgrade it, and it can halt. At launch, most of its validators are run by the project's founder.
- **No third-party audit.** The chain launched before an external security audit was complete.
- **As is.** This SDK is MIT-licensed software, provided as is, without warranty of any kind.
- **No promise of value.** FANG carries no promise of any value. Don't market FANG, or your app, as an investment.
- **Your app is yours.** You're responsible for your app, for your users' funds, and for the laws that apply to you and to them.
- **Names pay their current owner.** A payment to a `.boo` name goes to whoever owns the name when it runs, and names expire. To pay someone for sure, pay their address.
- **Payments are final.** Nobody can reverse a transaction or recover a lost key.

Read the [risks](https://github.com/jackdishman/blockula/blob/main/docs/RISKS.md) and the [terms](https://github.com/jackdishman/blockula/blob/main/TERMS.md) before you build on mainnet.
<!-- end D4 -->

## Balances

```ts
const b = await wallet.balances();   // { balance, transferable, locked, exists, display }
b.transferable;                      // bigint drops: what it can pay now
await wallet.balance();              // "12.5 FANG": everything it holds
```

- `transferable` is the "can pay" number: what the account can send now, fees included. Vesting and vote locks hold the rest (`locked`). Staked FANG isn't in the balance (`client.staking(account)`).
- `balance()` and `balances()` throw when the node can't answer (down, rate limited, a timeout, another chain). They return 0 only for an account that doesn't exist yet. Never show a failed read as an empty account.

## Costs

```ts
const q = await wallet.quote("alice.boo", "0.02");
q.total;          // amount + fee + creationFee, in drops
q.creationFee;    // 0.1 FANG if this payment opens alice's account, else 0
q.enough;         // whether the account can pay it
```

- **Fees.** Every tx pays its block's base fee (priced per byte and burned) plus its tip. A transfer is about 5.5 KB, about 0.000065 FANG at the floor. `/api/fees` fixes the base fee 17 blocks ahead, and it rises while blocks are full.
- **Opening an account.** A payment to an address the chain hasn't seen burns `creation_fee` (0.1 FANG with the default parameters) from the payer, on top of the amount. That payment must carry at least 0.01 FANG. `quote()` counts both. The result of a finalized `send` that opened an account says so (`opened_account`, `creation_fee`), since its `summary` and `fee` don't: show it next to the summary.
- **Zero.** `send` refuses an amount of 0 before signing: it would move nothing and pay a fee.
- **Names.** Registering a name costs what `client.name(n).price` says. With the default parameters: 100 FANG for 3 characters, 25 for 4, 5 for 5 or more. A year's renewal (`renewal_price`) is a fifth of that.
- **Headroom.** `send(to, amount, { feeHeadroom: 3 })` lets the fee rise to 3× today's base fee before the tx is included, and `{ maxFee }` sets it outright. Only the including block's base fee and the tip are charged, so headroom costs nothing. Without it, a tx stuck behind full blocks for more than 17 blocks can stop covering the base fee and be dropped (with every later tx from the account). `wallet.feeHeadroom = 2` sets a default.

## Receiving payments

```ts
import { watchPayments } from "@blockula/sdk";

await watchPayments(client, shopAddress, {
  fromHeight: await db.get("next"),           // resume where you stopped
  onPayment: async (p) => {                   // in chain order
    await db.creditOrderOnce(p.id, p.memo, p.amount);
  },
  onProgress: async (next) => db.set("next", next),
  signal,                                     // abort to stop
});
```

- **Finalized is final.** Blocks run only once they're finalized, so there are no reorgs and no confirmations to wait for. `watchPayments` reads only finalized blocks.
- **Match a unique memo.** Give each order its own memo (an order id you made up) and match it. Memos are public, and anyone can write anything in one: never act on what a memo says.
- **Credit by `id`.** If your `onPayment` throws, the watch stops; resuming from the last stored height delivers that block again. Credit each payment once, by its tx id.
- **Watch an address,** not a name: names expire and change hands. `p.to_name` says which name the payer used.
- **Only transfers.** Other ways FANG arrives (a hunt's award, staking) aren't payments here. `client.history(account, { before, limit })` pages through everything an account did, newest first.
- **Pruned history.** Nodes keep a limited window of blocks (`/api/status` `history_from`). If you resume from before it, `watchPayments` throws `HistoryPrunedError` rather than skip payments: ask a node that keeps every block, or pass `skipPruned`.
- `paymentsIn(block, account)` reads one `/api/block/{height}` answer, for your own scanner. `client.blocks({ limit, before })` pages block headers backwards; fetch only blocks with `tx_count > 0`.

## Sending a lot

- **One wallet, many sends.** A wallet signs and submits its txs one at a time, in nonce order, so `Promise.all` of many `send`s works: each gets the next nonce. Waiting for them happens in parallel.
- **`sendMany`** pays a list, with at most `concurrency` (default 16) waiting to finalize at once. A node allows 64 pending txs per sender. On a 429 (rate limited) or "too many pending", it resubmits the same signed bytes once the node says to retry. Each payment gets a result, `{ ok, tx }` or `{ ok: false, error }`.
- **Rate limits.** A public node takes about 120 txs a minute and 50 reads a second per IP, and answers `429` with `Retry-After` (`ApiError.retryAfterMs`). Your own node doesn't limit you (loopback isn't limited), but the validator it forwards to does.
- **Never re-sign a payment you're unsure of.** A tx that comes back pending, dropped or timed out may still run until its `valid_until`. Resubmitting the same bytes (`client.post(tx)`) is safe: it runs at most once. A new signature is a new payment. See [Timeouts and pending transactions](#timeouts-and-pending-transactions).
- **Store first, then send.** `send(to, amount, { onSigned: (tx, id) => outbox.save(id, tx) })` (and `sendMany(payments, { onSigned })`) hands you each signed tx and its id before it's submitted, in the wallet's queue, so parallel sends still get one nonce each. If `onSigned` throws, nothing is submitted. `wallet.build()` followed by your own `client.post(tx)` takes no nonce from the queue: two builds in a row get the same nonce, so build and post one tx at a time per key that way.
- **The wire form** (`new Client(rpc, { wireTxs: true })`) posts txs as their binary wire form, about half the bytes of JSON.

## Your own RPC node

Any node serves the same API, and one on your machine isn't rate limited. To run one against the testnet:

```bash
blockula join --genesis https://blockula-testnet.fly.dev --home ~/blockula-node --port 7171
```

```ts
const client = new Client("http://localhost:7171", {
  network: "testnet",                                         // its pins: a node on another chain is refused
  witnesses: [new Client("https://blockula-testnet.fly.dev")], // names proven against a second node
});
```

- It fast-syncs from a certified snapshot in seconds, so it holds history only from there (`history_from`). Pass `--full` to replay from genesis.
- Txs you send through it are forwarded to a validator. That needs a node release that relays through another full node (the launch release on); an older node answers "forwarding loop" when it can only reach the public node.
- A second, independent node as a witness means no single node's word decides who a name pays (see "Which chain? Pinning and proofs" below).
- [docs/INTEGRATION.md](https://github.com/jackdishman/blockula/blob/main/docs/INTEGRATION.md) covers pruning, checkpoints and the weak-subjectivity window.

## Accounts, keys and fees

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

Amounts are FANG strings (`"1.5"`) or bigint drops (1 FANG = 10⁹ drops). A JS number still works but is deprecated: floats can't hold most decimal amounts exactly. Every transaction result includes a plain-English `summary`.

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
- `wallet.address` uses the chain's prefix once the client knows it. A client made for a network (`Client.testnet()`) or pinned with `prefix` knows it up front. Otherwise it shows the `boo` form until the first request, so `await client.addressPrefix()` first if you display it.
- Node errors throw `ApiError` (`.status`: 429 means rate limited, and `.retryAfterMs` says how long the node asked you to wait). A transaction the node reports as failed throws `TxFailedError` from `waitForTx` (`dropped` says whether it ran: see "Timeouts and pending transactions").
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

A pinned client checks every `/api/status` it reads, including the one each signature is made against, and throws `ChainMismatchError` instead of signing for another chain. `network` (see "Which network?") sets these pins for you. A `chainId: "blockula-1"` pin is also the opt-in to sign on mainnet.

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

Tests: `npm run test:vectors` (offline: passkey derivation, state-proof and tx-encoding vectors from the node, every protocol v1 helper against a fake node, pinning, proof freshness, timeouts, release signatures, and `test/integrators.ts`: network presets, the mainnet opt-in, balances, the nonce queue, `sendMany`, `quote`, history, `watchPayments` and `onSigned`; and `test/examples.ts`, which runs `examples/pay-from-a-script.mjs` against a fake node, so build first), `npm run test:kat` (ML-DSA known answers), `npm test` (end to end against `blockula dev`: `test/e2e.ts`, then `test/e2e-integrators.ts`). The `examples/` directory at the repository's root has two runnable scripts: receiving payments, and paying from a script.

## Changes in 0.6

Two changes need a look when you upgrade:

- **`balance()` throws** when the node can't answer, instead of returning `"0 FANG"`. It's `"0 FANG"` only for an account that doesn't exist yet. `balances()` is new.
- **Mainnet needs an opt-in.** A wallet whose client wasn't made with `network: "mainnet"`, a `chainId: "blockula-1"` pin, or `allowMainnet: true` throws `MainnetNotAllowedError` instead of signing on `blockula-1`.

Also new: network presets (`NETWORKS`, `Client.testnet()`), a nonce queue in each wallet, `sendMany`, `quote`, `feeHeadroom` and `maxFee`, zero amounts refused, `opened_account` and `creation_fee` on receipts, `client.blocks({ limit, before })`, `client.history`, `watchPayments`, `client.post`, `wireTxs`, `onSigned`, and `ApiError.retryAfterMs`.

Blockula is experimental software: see [Using the SDK on mainnet](#using-the-sdk-on-mainnet).
