// End to end against a running node (`blockula dev`, or BLOCKULA_RPC): what integrators lean on.
// Parallel sends from one wallet, honest balances, quote() against the real cost of opening an
// account, refused zero amounts, fee headroom, sendMany, paged blocks and history, the wire
// form, and watchPayments resuming from a stored height. `npm test -w @blockula/sdk` runs it
// after e2e.ts.
import assert from "node:assert/strict";
import { Client, Wallet, formatFang, parseFang, watchPayments, type Payment } from "../src/index.js";

const RPC = process.env.BLOCKULA_RPC ?? "http://localhost:7070";
const client = new Client(RPC, { network: "local" });
await client.status();

const payer = Wallet.generate(client);
await payer.faucet();
console.log("payer:", payer.address, (await payer.balances()).display.transferable, "transferable");

// balance(): an account that doesn't exist yet is "0 FANG"; a node that can't answer is an error.
const nobody = Wallet.generate(client);
assert.equal(await nobody.balance(), "0 FANG");
assert.equal((await nobody.balances()).exists, false);
await assert.rejects(Wallet.generate(new Client("http://127.0.0.1:9", { timeoutMs: 3_000 })).balance());
console.log("✓ balance() throws when the node is down, and is 0 FANG for an account that doesn't exist");

// quote() vs what really leaves the account when a payment opens one (0.1 FANG burned + the fee).
const shop = Wallet.generate(client);
const memo = "opens the shop's account";
const q = await payer.quote(shop.address, "1", { memo });
const creationFee = BigInt((await client.fees()).account_creation_fee);
assert.deepEqual([q.opensAccount, q.creationFee, q.total], [true, creationFee, q.amount + q.fee + creationFee]);
const before = (await payer.balances()).balance;
const opened = await payer.send(shop.address, "1", { memo });
const after = (await payer.balances()).balance;
assert.equal(opened.opened_account, true);
assert.equal(opened.creation_fee, creationFee.toString());
assert.ok(BigInt(opened.fee) <= q.fee, "charged at most the quoted fee");
assert.equal(before - after, q.amount + BigInt(opened.fee) + q.creationFee, "the balance dropped by the amount, the fee and the opening burn");
if (BigInt(opened.fee) === q.fee) assert.equal(before - after, q.total);
console.log(`✓ quote: ${q.display.total} to open an account with 1 FANG (${q.display.creationFee} burned); the balance dropped ${formatFang(before - after)}`);

// Zero is refused before anything is signed; so is less than 0.01 FANG to a new account.
await assert.rejects(payer.send(shop.address, "0"), /send more than 0/);
await assert.rejects(payer.send(Wallet.generate(client).address, "0.001"), /new account needs at least 0\.01 FANG/);
console.log("✓ zero and below-minimum payments refused before signing");

// Five parallel sends from one wallet, each stored before it's submitted: one nonce each, all final.
const outbox = new Map<string, number>();
const parallel = await Promise.all([1, 2, 3, 4, 5].map((i) => payer.send(shop.address, "0.5", { memo: `parallel ${i}`, onSigned: (tx, id) => void outbox.set(id, tx.nonce) })));
assert.ok(parallel.every((t) => t.status === "finalized"), JSON.stringify(parallel.map((t) => t.status)));
const nonces = parallel.map((t) => t.nonce).sort((a, b) => a - b);
assert.deepEqual(nonces, [0, 1, 2, 3, 4].map((k) => nonces[0] + k));
assert.deepEqual(parallel.map((t) => outbox.get(t.id)), parallel.map((t) => t.nonce), "each finalized tx is the one onSigned stored");
console.log(`✓ 5 parallel sends from one wallet, each stored by onSigned first, finalized (nonces ${nonces.join(", ")})`);

// Fee headroom: the fee field allows 3× the base fee; only the base fee is charged.
const roomy = await payer.send(shop.address, "0.1", { feeHeadroom: 3 });
assert.ok(BigInt(roomy.max_fee) >= 3n * BigInt(roomy.fee) - 3n && BigInt(roomy.fee) < BigInt(roomy.max_fee), `${roomy.fee} of ${roomy.max_fee}`);
console.log(`✓ fee headroom: allowed ${formatFang(BigInt(roomy.max_fee))}, charged ${roomy.fee_display}`);

// sendMany: a batch, three at a time.
const batch = await payer.sendMany([1, 2, 3, 4, 5, 6].map((i) => ({ to: shop.address, amount: "0.01", memo: `batch ${i}` })), { concurrency: 3 });
assert.ok(batch.every((r) => r.ok), JSON.stringify(batch.map((r) => (r.ok ? "ok" : String((r.error as Error).message)))));
console.log(`✓ sendMany: ${batch.length} payments, at most 3 pending at a time`);

// The wire form.
const wire = Wallet.fromSeed(payer.seed, new Client(RPC, { wireTxs: true }));
assert.equal((await wire.send(shop.address, "0.01", { memo: "wire form" })).status, "finalized");
console.log("✓ a tx submitted in its wire form finalizes");

// Paged blocks and history.
const top = await client.blocks({ limit: 3 });
assert.equal(top.length, 3);
const older = await client.blocks({ limit: 3, before: top[2].height });
assert.equal(older[0].height, top[2].height - 1);
const page = await client.history(payer.address, { limit: 2 });
assert.ok(page.txs.length >= 1 && page.txs.length <= (page.paged ? 2 : 25));
if (page.paged && page.next) assert.ok((await client.history(payer.address, { limit: 2, before: page.next })).txs.length >= 1);
console.log(`✓ blocks page with before; history ${page.paged ? "pages" : "falls back to the newest txs (a node without paged history)"}`);

// watchPayments: sees a payment, then resumes from the stored height and sees only what's new.
const carol = Wallet.generate(client).address;
const from = (await client.status()).height + 1;
const first = await payer.send(carol, "0.25", { memo: "order-1" });
let stored = from;
const seen: Payment[] = [];
{
  const stop = new AbortController();
  const timer = setTimeout(() => stop.abort(), 30_000);
  await watchPayments(client, carol, {
    fromHeight: from,
    pollMs: 250,
    onPayment: (p) => void seen.push(p),
    onProgress: (next) => {
      stored = next;
      if (seen.length) stop.abort();
    },
    signal: stop.signal,
  });
  clearTimeout(timer);
}
assert.deepEqual(seen.map((p) => [p.id, p.amount, p.memo, p.to]), [[first.id, parseFang("0.25"), "order-1", carol]]);
assert.ok(stored > first.height);
const second = await payer.send(carol, "0.5", { memo: "order-2" });
const resumed: Payment[] = [];
{
  const stop = new AbortController();
  const timer = setTimeout(() => stop.abort(), 30_000);
  await watchPayments(client, carol, {
    fromHeight: stored,
    pollMs: 250,
    onPayment: (p) => void resumed.push(p),
    onProgress: () => {
      if (resumed.length) stop.abort();
    },
    signal: stop.signal,
  });
  clearTimeout(timer);
}
assert.deepEqual(resumed.map((p) => [p.id, p.memo]), [[second.id, "order-2"]]);
console.log(`✓ watchPayments saw ${seen[0].amount_display} (order-1) in block #${seen[0].height}, then resumed from #${stored} and saw only order-2`);

console.log("✓ integrator e2e passed");
