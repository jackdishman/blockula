// lib/explorer/ledger.ts: balance effects, perspective and flows, on transactions shaped like
// the node's /api/tx output. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { bySigner, compactDrops, counterparty, deltaFor, effectsOf, flowsOf, formatDrops, fromMySide } from "./explorer/ledger.ts";

const F = 1_000_000_000n;
const label = (name, address = `boo1${name}`) => ({ address, name: `${name}.boo`, short: `${address.slice(0, 6)}…` });
const vlad = label("vlad"), mina = label("mina"), lucy = label("lucy");
const pool = { address: "boo1pool", name: "🪦 contribution pool", short: "boo1pool…" };
let n = 0;
const tx = (o) => ({ id: `t${n++}`, summary: "", status: "finalized", height: 10 + n, fee: "1000", signer_key: "k-owner", ...o });
const send = (from, to, fang, extra = {}) => tx({ kind: "transfer", from, to, amount: String(BigInt(fang) * F), ...extra });

test("a transfer: the sender pays amount + fee, the recipient gets the amount, the fee burns", () => {
  const fx = effectsOf(send(vlad, mina, 5));
  assert.deepEqual(fx.changes.map((c) => [c.who.name, c.delta]), [["vlad.boo", -(5n * F + 1000n)], ["mina.boo", 5n * F]]);
  assert.equal(fx.burned, 1000n);
  assert.equal(fx.unseen, null);
});

test("a failed transaction only charges the fee; a dropped one changes nothing", () => {
  assert.deepEqual(effectsOf(send(vlad, mina, 5, { status: "failed", error: "insufficient balance" })).changes.map((c) => c.delta), [-1000n]);
  assert.deepEqual(effectsOf({ id: "x", status: "failed", summary: "Transaction failed", height: 3 }).changes, []);
});

test("escrowed amounts go to a labeled holding, and effects the fields don't show are flagged", () => {
  const fund = effectsOf(tx({ kind: "fund_hunt", from: lucy, amount: String(2n * F), action: { type: "fund_hunt", hunt_id: 3 } }));
  assert.equal(fund.changes[1].who.name, "Hunt #3 escrow");
  assert.equal(fund.changes[1].who.holding, true);
  const name = effectsOf(tx({ kind: "register_name", from: vlad, action: { type: "register_name", name: "vlad" } }));
  assert.deepEqual(name.changes.map((c) => c.delta), [-1000n]);
  assert.match(name.unseen, /treasury/);
  const toPool = effectsOf(tx({ kind: "fund_pool", from: mina, to: pool, amount: String(4n * F) }));
  assert.equal(toPool.changes[1].who.address, "boo1pool");
});

test("a vouch bond is held by the program, not paid to the newcomer", () => {
  const v = tx({ kind: "vouch", from: vlad, to: mina, amount: String(10n * F), action: { type: "vouch", account: mina.address } });
  const fx = effectsOf(v);
  assert.equal(fx.changes[1].who.name, "Vouch bond");
  assert.equal(deltaFor(v, mina.address).direction, "none");
  assert.equal(flowsOf([v], mina.address).totalIn, 0n);
});

test("paying yourself costs only the fee", () => {
  const fx = effectsOf(send(vlad, vlad, 3));
  assert.deepEqual(fx.changes.map((c) => c.delta), [-1000n]);
  assert.equal(deltaFor(send(vlad, vlad, 3), vlad.address).direction, "self");
});

test("direction and counterparty from each side", () => {
  const t = send(vlad, mina, 2);
  assert.deepEqual(deltaFor(t, vlad.address), { delta: -(2n * F + 1000n), direction: "out", exact: true });
  assert.deepEqual(deltaFor(t, mina.address), { delta: 2n * F, direction: "in", exact: true });
  assert.equal(deltaFor(t, lucy.address).direction, "none");
  assert.equal(counterparty(t, vlad.address).name, "mina.boo");
  assert.equal(counterparty(t, mina.address).name, "vlad.boo");
});

test("flows add up per counterparty, skip pending, and count fees once", () => {
  const txs = [
    send(vlad, mina, 5),
    send(mina, vlad, 3),
    send(vlad, mina, 1),
    send(vlad, lucy, 2),
    send(lucy, vlad, 9, { status: "pending", height: null }),
    tx({ kind: "register_name", from: vlad }),
  ];
  const f = flowsOf(txs, vlad.address);
  const m = f.rows.find((r) => r.who.name === "mina.boo");
  assert.equal(m.out, 6n * F);
  assert.equal(m.in, 3n * F);
  assert.equal(m.count, 3);
  assert.equal(f.rows[0].who.name, "mina.boo");
  assert.equal(f.totalIn, 3n * F);
  assert.equal(f.totalOut, 8n * F);
  assert.equal(f.burned, 4000n);
  assert.equal(f.counted, 5);
  assert.equal(f.unseen, 1);
});

test("spending by signing key", () => {
  const txs = [send(vlad, mina, 1, { signer_key: "agent" }), send(vlad, lucy, 2, { signer_key: "agent" }), send(vlad, mina, 4), send(mina, vlad, 1, { signer_key: "agent" })];
  const s = bySigner(txs, vlad.address);
  assert.equal(s.get("agent").count, 2);
  assert.equal(s.get("agent").spent, 3n * F + 2000n);
  assert.equal(s.get("k-owner").count, 1);
});

test("summaries from my side", () => {
  const me = { address: vlad.address, names: ["vlad.boo"], short: vlad.short };
  assert.equal(fromMySide("vlad.boo sent 1 FANG to mina.boo", me), "You sent 1 FANG to mina.boo");
  assert.equal(fromMySide("mina.boo sent 1 FANG to vlad.boo", me), "mina.boo sent 1 FANG to you");
  // The object of "claimed the name", quoted labels, and longer names are left alone.
  assert.equal(fromMySide(`${vlad.short} claimed the name vlad.boo`, me), "You claimed the name vlad.boo");
  assert.equal(fromMySide("mina.boo put 9 FANG on a/b#1: “pay vlad.boo”", me), "mina.boo put 9 FANG on a/b#1: “pay vlad.boo”");
  assert.equal(fromMySide("mina.boo sent 1 FANG to vlad.boost", me), "mina.boo sent 1 FANG to vlad.boost");
  assert.equal(fromMySide("vlad.boo sent 1 FANG to mina.boo", null), "vlad.boo sent 1 FANG to mina.boo");
});

test("formatting drops", () => {
  assert.equal(formatDrops(1_500_000_000n), "1.5 FANG");
  assert.equal(formatDrops(-64_690n), "-0.00006469 FANG");
  assert.equal(compactDrops(1_099_999_999_996_598_400n), "1,099,999,999");
  assert.equal(compactDrops(12_345_678_900_000n), "12,345.67");
  assert.equal(compactDrops(128_349_225_960n), "128.3492");
  assert.equal(compactDrops(64_690n), "0.00006469");
  assert.equal(compactDrops(1n), "0.000000001");
  assert.equal(compactDrops(0n), "0");
});

test("a kind this explorer doesn't know claims only its fee, and is never reported as exact", () => {
  // A newer protocol's claim_distribution pays its amount *to* the sender: guessing would invert it.
  const fx = effectsOf(tx({ kind: "claim_distribution", from: vlad, amount: String(F) }));
  assert.deepEqual(fx.changes.map((c) => [c.who.name, c.delta]), [["vlad.boo", -1000n]]);
  assert.match(fx.unseen, /doesn't know this kind/);
  assert.equal(deltaFor(tx({ kind: "shield", from: vlad, amount: String(F) }), vlad.address).exact, false);
  assert.equal(effectsOf(tx({ kind: "add_key", from: vlad })).unseen, null, "known kinds that move nothing stay exact");
  assert.match(effectsOf(tx({ kind: "reject_claim", from: vlad })).unseen, /forfeited/);
});

test("a fee above the minimum tips the block's proposer; only the minimum burns", () => {
  // min_fee(size) = 10,000 + 10 per byte (node/src/types.rs).
  const exact = effectsOf(send(vlad, mina, 1, { size_bytes: 5000, fee: "60000" }));
  assert.equal(exact.burned, 60000n);
  assert.equal(exact.changes.length, 2);
  const tipped = effectsOf(send(vlad, mina, 1, { size_bytes: 5000, fee: "70000" }));
  assert.equal(tipped.burned, 60000n);
  assert.deepEqual(tipped.changes.map((c) => [c.who.name, c.delta]), [["vlad.boo", -(F + 70000n)], ["mina.boo", F], ["The block's proposer", 10000n]]);
  // Later protocols report the tip; a charge capped at max_fee hides the split, and says so.
  const v1 = effectsOf(send(vlad, mina, 1, { fee: "50000", max_fee: "90000", tip: "2000" }));
  assert.equal(v1.burned, 48000n);
  const capped = effectsOf(send(vlad, mina, 1, { fee: "90000", max_fee: "90000", tip: "2000" }));
  assert.equal(capped.burned, 90000n);
  assert.match(capped.unseen, /may have tipped/);
  // A tip never counts as money the recipient got.
  const flows = flowsOf([send(vlad, mina, 1, { size_bytes: 5000, fee: "70000" })], mina.address);
  assert.equal(flows.totalIn, F);
});
