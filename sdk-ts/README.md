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

- A transaction is charged `minFee(tx)` (priced per byte, burned) plus its `tip` (paid to the block's proposer; blocks take the highest tip per byte first). The wallet sets `fee` to exactly that. `txCharge(tx)` computes it.
- Every transaction carries `valid_until`, the last block it may execute in. The wallet sets it to the current height plus `validForBlocks` (default 600 blocks, at most the chain's `max_tx_validity_blocks`, an hour of blocks). A transaction past `valid_until` can never execute, so it's safe to report it as expired.
- `txBytes(tx)` is the binary wire form nodes gossip and put in blocks, and `txId(tx)` its id, known before you submit.
- Every transaction names the chain it's for by `chain_id` and `genesis_hash` (the chain instance, from `/api/status`), so it can't replay on a relaunched chain that reuses the id. `wallet.build` fills both in; to sign offline, put the chain's `genesis_hash` in the `UnsignedTx` you pass to `wallet.sign`.

Addresses are bech32m with the chain's prefix: `boo1…` on mainnet, `tboo1…` on public testnets. `isAddress(s)` accepts any known prefix and `isAddress(s, "tboo")` exactly one; `addressFromPublicKey(pk, prefix)` renders one. A `Client` learns its chain's prefix from `/api/status` (`await client.addressPrefix()`), and wallets using that client sign with it (`wallet.address`, `wallet.account`). Pin it with `new Client(rpc, { prefix: "boo" })` to refuse a node on another network. Nodes reject addresses of another network's prefix.

- `send("alice.boo", …)` resolves the name first and signs the **address**, so whoever you checked is exactly who gets paid.
- `wallet.address` uses the chain's prefix once the client knows it. Before the first request it shows the `boo` form, so `await client.addressPrefix()` first if you display it on a testnet.
- Node errors throw `ApiError` (`.status`: 429 means rate limited). A transaction that fails at execution throws `TxFailedError` from `waitForTx`.
- Pass `rpId` to `createPasskeyWallet`/`unlockPasskeyWallet` to pin wallets to your production domain. Passkeys are bound to that hostname for good.

The public testnet is `https://blockula-testnet.fly.dev`, and the wallet is https://blockula-wallet.vercel.app.

⚠️ Early and unaudited: test chains only.
