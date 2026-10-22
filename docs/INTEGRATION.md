# Integrating Blockula: exchanges, custodians and bots

For anyone who holds FANG for others, credits deposits, sends withdrawals or runs a bot. It covers finality, running your own node, detecting deposits, sending withdrawals safely, fees, limits, addresses and custody. The SDK side is in [sdk-ts/README.md](../sdk-ts/README.md), and the HTTP API is in [CLAUDE.md](../CLAUDE.md#http-api-node-default-httplocalhost7070).

<!-- D4 (decided): this notice is published as provisional, under counsel review. Its wording is in this one section; change it here. -->
## Notice to exchanges, custodians and integrators

> **Provisional, under counsel review. Not legal advice.** Facts marked [spec] come from the frozen genesis spec and manifest.

- **Experimental network.** Blockula mainnet (`blockula-1`) is experimental software. It can halt, be upgraded by governance, or behave in ways nobody expected. Fixes may come as upgrades or as coordinated halts.
- **Concentrated at launch.** At launch, most validators are run by the project's founder, on one hosting provider [spec: the founder's four validators on Fly.io, plus any outside operators ready by the genesis ceremony]. With four validators, two going down halts the chain, and deposits and withdrawals with it. Until about two years after genesis, upgrades and parameter changes can pass through a bootstrap council made of those validators.
- **No third-party audit at launch.** An external security audit wasn't complete when the chain launched.
- **No warranty.** The node, the SDK and the other tools are MIT-licensed software, provided as is, without warranty of any kind.
- **No promise of value.** FANG carries no promise of any value. Blockula doesn't arrange, pay for or endorse any platform's support for FANG. Whether to support it, and how, is your decision and your risk assessment.
- **Your obligations are yours.** You're responsible for your users' funds, your systems, and the laws that apply to you.

See also the [risks](RISKS.md) and the [terms](../TERMS.md).
<!-- end D4 -->

## Finality

- **One confirmation is final.** Blocks run only once validators have finalized them (BFT), so a block never reorganizes. A tx that `/api/tx/{id}` shows as `finalized` can't be undone. There's nothing to wait for after that.
- **Time to final:** a second or two on a chain that isn't busy (measured on test chains: an SDK `send` took about 1 second from submission to finalized at the median). Allow longer while blocks are full.
- **Status:** `pending` (in a mempool), `finalized` (ran and succeeded), or `failed`. A failed tx with `dropped: false` ran in a block and its fee was charged. One with `dropped: true` was dropped from that node's mempool without running: nothing was charged, but another node may still include it until its `valid_until`.

## Run your own node

Read and submit through a node you run, not only a public one. A node on your machine isn't rate limited, and it checks finality itself.

```bash
blockula join --genesis https://<a node of the chain> --home /var/lib/blockula/full --port 7171
```

- **Pins.** `join` refuses a genesis nobody pinned: a release knows its chains' genesis hashes, or you pass `--genesis-hash`. In code, pin the chain too: `new Client("http://localhost:7171", { network: "testnet" })` (or `"mainnet"`) checks the chain id, genesis hash and address prefix on every status.
- **Weak subjectivity.** A node that joins from scratch trusts a recent checkpoint, not just the genesis. It refuses to sync from a root older than the weak-subjectivity period (`weak_subjectivity_blocks` in `/api/status`: about 10.5 days with the default parameters, never less than a week). Take checkpoints from several independent sources: [OPERATIONS.md, Weak subjectivity](OPERATIONS.md#weak-subjectivity).
- **History.** `join` fast-syncs from a certified snapshot and holds history only from there (`history_from` in `/api/status`). For a full record from genesis, pass `--full` to replay everything (a full node keeps every block it has). Validators keep about two days of blocks or less, and public nodes keep a limited window (14 days on the public testnet).
- **Pruned data.** `/api/block/{height}` and `/api/sync` answer `410` with `"pruned": true` below `history_from`, and `/api/tx/{id}` answers `404` for a pruned tx. A 410 means "ask a node that keeps it", never "it didn't happen".
- **Readiness.** `/api/health` is the probe: 200 when the head is fresh and synced, 503 with a `reason` otherwise (and `halted` when the node stopped on purpose, for an upgrade or a coordinated halt). When it says halted, stop crediting and sending, and say "network upgrading".
- **Witnesses.** Give your client a second, independent node as a witness (`witnesses: [new Client(…)]`). Names it resolves for a signature are then proven against both, and a status height ahead of the witness is refused.
- **Submitting.** Your node forwards txs to a validator. That needs a node release that relays through another full node (the launch release on). Its forwarded txs count against your node's IP at the validator (below).

## Deposits

Credit a deposit only when **all** of these hold:

1. The tx is in a block, with `status` `"finalized"` and `error` `null`. A failed tx sits in its block with `to` and `amount` filled in, and its fee charged, but it moved nothing.
2. Its `kind` is `"transfer"`.
3. Its `to.address` is your deposit address. Match `to.address`, never `action.to`: a tx can be signed to a `.boo` name, and `to` is the account the name pointed to when it ran.

With the SDK:

```ts
import { Client, watchPayments } from "@blockula/sdk";

const client = new Client("http://localhost:7171", { network: "testnet" });
await watchPayments(client, depositAddress, {
  fromHeight: await store.get("next"),        // resume where you stopped
  onPayment: async (p) => store.creditOnce(p.id, p.memo, p.amount),
  onProgress: async (next) => store.set("next", next),
});
```

Without it, scan blocks yourself: page headers with `/api/blocks?limit=100&before=<height>`, fetch `/api/block/{height}` only where `tx_count > 0`, and apply the rules above to its `txs`. `paymentsIn(block, address)` in the SDK does exactly that for one block.

- **Credit each deposit once, by its tx id.** Tx ids are unique.
- **Omnibus or one address per user.** Memos are up to 256 bytes, public and written by the payer: use one you issued (an order or user id) to match deposits to an omnibus address, and never act on what a memo says. One address per user needs no memo. Either way, the first payment to a new address opens its account (below).
- **History.** `/api/account/{id}` lists an account's newest 25 txs. `client.history(account, { before, limit })` pages through all of them on nodes that serve `/api/account/{id}/txs`. Reconcile from blocks, not from the newest-25 list: anyone can send dust to push a payment out of it.

## Withdrawals

- **Store first, then send.** Sign the tx, store its signed bytes and its id (`txId(tx)`, known before you send it), and only then submit it. Every outcome is then recoverable. With the SDK, `wallet.send(to, amount, { onSigned: (tx, id) => outbox.save(id, tx) })` hands you each signed tx before it's submitted, in the wallet's nonce queue, so parallel withdrawals from one key work. Building with `wallet.build()` and posting with `client.post(tx)` yourself bypasses that queue (a build doesn't take its nonce): do that one withdrawal at a time per key.
- **Rebroadcast; never re-sign.** If you don't know what happened (a timeout, a 5xx, `pending` for long), submit the **same bytes** again: a node answers with the same id, and a tx runs at most once. A new signature is a new payment, and both could run. Re-sign only once the old tx can no longer run: its nonce was used by another tx, or its `valid_until` height has passed with the nonce still unused.
- **Validity window.** Every tx carries `valid_until`, at most an hour of blocks ahead (`max_tx_validity_blocks`); the SDK uses 600 blocks (10 minutes) by default. A tx past it never runs.
- **Nonces are strict.** Each account's txs run in nonce order, with no gaps. A node holds at most **64** pending txs per sender (more are refused: "too many pending"). A wallet in the SDK numbers the txs it submits (`send`, `sendMany` and the other helpers), so parallel sends from one wallet work; `wallet.sendMany(payments, { concurrency })` keeps at most that many pending and waits out a 429.
- **The cascade.** When a node drops a tx from its mempool (it stopped covering the base fee, it expired, a tx before it was dropped), it drops every later tx from that sender too. Check `client.tx(id)` for each, and rebroadcast or replace in nonce order.
- **Opening accounts.** A payment to an address that has no account yet burns `creation_fee` from the sender, on top of the amount: 0.1 FANG with the default parameters. Such a payment must carry at least **0.01 FANG**. `/api/account/{address}` says `exists` and `creation_fee` before you send, and `wallet.quote(to, amount)` adds it up. 200 withdrawals to new addresses burn 20 FANG of opening fees.

## Fees

- **What's charged:** the including block's base fee (a flat part plus a rate per byte, burned) plus the tx's tip (to the block's proposer). The tx's `fee` field is the most it may be charged; only the actual charge is debited.
- **Predictable:** `/api/fees` lists the base fee of each of the next 17 blocks (`slots`), fixed in advance, and `suggested`, which covers any of them. The base fee rises about 12.5% per full block and falls back as blocks empty.
- **Headroom.** A tx priced exactly at `suggested` can stop covering the base fee if blocks stay full for longer than 17 blocks, and be dropped (with the sender's later txs). Give withdrawals 2–3× headroom: `send(to, amount, { feeHeadroom: 3 })`, or `{ maxFee }`. Headroom costs nothing, since only the base fee and the tip are charged.
- **Tips** are optional, at most 20× the base fee (`tip_cap_multiple`), and paid from transferable FANG. Blocks take the highest tip per byte first.
- A transfer is about 5.5 KB (most of it the post-quantum signature and key), about 0.000065 FANG at the floor.

## Limits

- **Per IP, on public nodes:** about 120 txs a minute and 50 reads a second (bursts of 200). Over that, `429` with `Retry-After`; the SDK's `ApiError.retryAfterMs` reads it.
- **Your own node** doesn't limit its own machine, but the validator it forwards your txs to limits your node's IP like any other.
- **Capacity:** blocks come every second, up to `max_block_bytes` (in `/api/fees`): about 381 transfers at 2 MiB, about 190 at 1 MiB.

## Addresses and names

- **Format:** bech32m, lowercase. `boo1…` on mainnet, `tboo1…` on public testnets, 32 bytes (the BLAKE3 hash of the account's first public key). `isAddress(s, "boo")` checks one in the SDK. Nodes refuse another network's prefix.
- **Names expire.** A `.boo` name is registered for a year at a time, then has a grace period, then anyone can register it. A payment to a name goes to whoever owns it when the payment runs. **Pay addresses**, and show users the address a name resolves to before they confirm.

## Custody

- **Keys.** Every signature is ML-DSA-65 (FIPS 204) over the tx's signing bytes, with an empty context. The encoding is in `sdk-ts/src/tx.ts` and `node/src/types.rs`, with shared vectors in `sdk-ts/test/tx-vectors.json`. Seeds are 32 random bytes; there's no derivation scheme yet, so back up one seed per address.
- **Cold storage: multisig.** `multisig_create` turns an account into an M-of-N of its owner keys. Owner actions then need a proposal and approvals (`blockula multisig …` or the SDK's `multisigPropose` / `multisigApprove`), and each approval binds the exact action. Prefer it to long-lived pre-signed txs, which the one-hour validity window doesn't allow anyway.
- **Hot wallets: session keys.** A session key can only send transfers, within a limit per period and until an expiry, all enforced by the chain. Give your hot wallet's signing service a session key on an account that an owner key (kept offline) controls, so a leaked hot key can only spend its budget.
- **Accounts hold many keys.** Every tx names the account it acts for (`from`); the signing key must be on that account.

## Checklist

- [ ] Your own node, pinned, with a recent checkpoint and a witness.
- [ ] Deposits credited only when finalized, with no error, by `to.address`, once per tx id.
- [ ] Withdrawals stored (bytes and id) before they're sent; rebroadcast, never re-signed.
- [ ] Fee headroom on withdrawals; the opening burn and the 0.01 FANG minimum in your fee schedule.
- [ ] At most 64 pending per sender; 429s waited out.
- [ ] Addresses, not names.
- [ ] `/api/health` watched; crediting and sending stop while the chain is halted.
