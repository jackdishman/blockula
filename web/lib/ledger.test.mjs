// lib/explorer/ledger.ts: balance effects, perspective and flows, on transactions shaped like
// the node's /api/tx output. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { amountLeaves, bySigner, compactDrops, contextsOf, counterparty, deltaFor, effectsOf, flowsOf, formatDrops, fromMySide, showsAccount } from "./explorer/ledger.ts";

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
  // A later protocol's kind may pay its amount *to* the sender: guessing would invert it.
  const fx = effectsOf(tx({ kind: "summon_bats", from: vlad, amount: String(F) }));
  assert.deepEqual(fx.changes.map((c) => [c.who.name, c.delta]), [["vlad.boo", -1000n]]);
  assert.match(fx.unseen, /doesn't know this kind/);
  assert.equal(deltaFor(tx({ kind: "summon_bats", from: vlad, amount: String(F) }), vlad.address).exact, false);
  assert.equal(effectsOf(tx({ kind: "add_key", from: vlad })).unseen, null, "known kinds that move nothing stay exact");
  assert.match(effectsOf(tx({ kind: "reject_claim", from: vlad })).unseen, /forfeited/);
});

test("a fee above the minimum tips the block's proposer on old nodes; v1 reports the exact tip", () => {
  // Before v1: min_fee(size) = 10,000 + 10 per byte (node/src/types.rs) burned, the rest tipped.
  const exact = effectsOf(send(vlad, mina, 1, { size_bytes: 5000, fee: "60000" }));
  assert.equal(exact.burned, 60000n);
  assert.equal(exact.changes.length, 2);
  const tipped = effectsOf(send(vlad, mina, 1, { size_bytes: 5000, fee: "70000" }));
  assert.equal(tipped.burned, 60000n);
  assert.deepEqual(tipped.changes.map((c) => [c.who.name, c.delta]), [["vlad.boo", -(F + 70000n)], ["mina.boo", F], ["The block's proposer", 10000n]]);
  // Protocol v1 charges exactly base fee + tip (node/src/econ.rs check_fee), so `tip` of `fee`
  // went to the proposer however the charge compares with max_fee (audit main13-protocol-10).
  const v1 = effectsOf(send(vlad, mina, 1, { fee: "50000", max_fee: "90000", tip: "2000" }));
  assert.equal(v1.burned, 48000n);
  for (const t of [send(vlad, mina, 1, { fee: "164440", max_fee: "164440", tip: "100000" }), send(vlad, mina, 1, { status: "failed", fee: "164440", max_fee: "164440", tip: "100000" })]) {
    const fx = effectsOf(t);
    assert.equal(fx.burned, 64440n, "the SDK signs fee = base + tip, the usual case");
    assert.equal(fx.changes.find((c) => c.who.name === "The block's proposer").delta, 100000n);
    assert.equal(fx.unseen, null);
    assert.equal(deltaFor(t, vlad.address).exact, true);
  }
  assert.equal(effectsOf(send(vlad, mina, 1, { fee: "64440", max_fee: "64440", tip: "0" })).burned, 64440n);
  // A tip never counts as money the recipient got.
  const flows = flowsOf([send(vlad, mina, 1, { size_bytes: 5000, fee: "70000" })], mina.address);
  assert.equal(flows.totalIn, F);
});

// ---- protocol v1 ----

const addr = (c) => `boo1${c.repeat(58)}`;
const fresh = { address: addr("q"), name: null, short: "boo1qqqq…qqqq" };

test("paying an address may open its account: the sender's side says so, the recipient's stays exact", () => {
  const t = send(vlad, fresh, 1, { fee: "64920", max_fee: "64920", tip: "0", to_name: null, action: { type: "transfer", to: fresh.address, amount: String(F) } });
  const fx = effectsOf(t);
  assert.match(fx.unseen, /account-creation fee/);
  assert.equal(fx.unseenFor, "sender");
  assert.equal(deltaFor(t, vlad.address).exact, false);
  assert.equal(deltaFor(t, fresh.address).exact, true);
  // Known to have had an account already (it shows up earlier): nothing to flag.
  assert.equal(effectsOf(t, { recipientExisted: true }).unseen, null);
  // review-main13-3: only a transaction that shows an account counts as that evidence: one the
  // address sent, or a finalized payment to it. A name offer, a vouch or a failed payment to it
  // opens none, so the payment after it may still have.
  const at = (h, extra) => ({ ...t, id: `e${h}`, height: h, ...extra });
  const named = [
    at(3, { kind: "transfer_name", to: fresh, amount: null, action: { type: "transfer_name", name: "castle", to: fresh.address } }),
    at(4, { kind: "vouch", to: fresh, action: { type: "vouch", account: fresh.address } }),
    at(5, { status: "failed", error: "insufficient balance" }),
  ];
  assert.equal(contextsOf([t, ...named])(t).recipientExisted, false);
  assert.equal(deltaFor(t, vlad.address, contextsOf([t, ...named])(t)).exact, false);
  assert.equal(contextsOf([t, at(5, {})])(t).recipientExisted, true, "an earlier finalized payment to it");
  assert.equal(contextsOf([t, at(6, { from: fresh, to: vlad })])(t).recipientExisted, true, "an earlier transaction it sent");
  assert.ok(!showsAccount({ ...t, status: "pending" }, fresh.address));
  // Known to open one (a preflight reads it): the burn is booked.
  const opening = effectsOf({ ...t, status: "pending", height: null }, { creationFee: F / 10n });
  assert.deepEqual(opening.changes.map((c) => c.delta), [-(F + 64920n + F / 10n), F]);
  assert.equal(opening.burned, 64920n + F / 10n);
  // A name's owner always has an account; paying a name opens none.
  assert.equal(effectsOf(send(vlad, mina, 1, { to_name: "mina.boo", action: { type: "transfer", to: "mina.boo", amount: String(F) } })).unseen, null);
});

test("an older node's to for a name is its owner now; a v1 node's is who it paid when it ran", () => {
  const signedToName = { action: { type: "transfer", to: "mina.boo", amount: String(F) } };
  const old = effectsOf(send(vlad, mina, 5, signedToName));
  assert.match(old.unseen, /owned it when this ran/);
  assert.equal(effectsOf(send(vlad, mina, 5, { ...signedToName, to_name: "mina.boo" })).unseen, null);
});

test("v1 kinds: a vote and unlock move nothing, a claim comes in, stake and deposits are held", () => {
  const vote = tx({ kind: "vote", from: vlad, amount: String(150n * F), action: { type: "vote", proposal_id: 1, aye: true, amount: String(150n * F), conviction: 1 } });
  assert.deepEqual(effectsOf(vote).changes.map((c) => c.delta), [-1000n]);
  assert.match(effectsOf(vote).note, /locks 150 FANG/);
  assert.equal(deltaFor(vote, vlad.address).exact, true);
  assert.equal(amountLeaves("vote"), false);
  const claim = tx({ kind: "claim_distribution", from: vlad, nonce: 3, amount: String(1000n * F), action: { type: "claim_distribution", distribution_id: 0, amount: String(1000n * F) } });
  assert.deepEqual(deltaFor(claim, vlad.address), { delta: 1000n * F - 1000n, direction: "in", exact: true });
  // review-main13-7: an account's first transaction may be a claim that opens it, burning the
  // creation fee out of the claim: not exact on the claimant's side, unless it's known to exist.
  const first = { ...claim, nonce: 0 };
  assert.match(effectsOf(first).unseen, /If this claim opened vlad.boo's account, the chain also burned the account-creation fee/);
  assert.equal(deltaFor(first, vlad.address).exact, false);
  assert.equal(deltaFor(first, vlad.address, { senderExisted: true }).exact, true);
  assert.equal(effectsOf(claim).changes[1].who.name, "Community distribution #0");
  assert.equal(flowsOf([claim], vlad.address).totalIn, 1000n * F);
  const val = label("val");
  const del = tx({ kind: "delegate", from: vlad, to: val, amount: String(20n * F), action: { type: "delegate", validator: val.address, amount: String(20n * F) } });
  assert.deepEqual(effectsOf(del).changes.map((c) => [c.who.name, c.delta]), [["vlad.boo", -(20n * F + 1000n)], ["Stake with val.boo", 20n * F]]);
  assert.equal(deltaFor(del, val.address).direction, "none", "stake is held in the pool, not paid to the validator");
  const prop = tx({ kind: "propose", from: vlad, amount: String(1000n * F), action: { type: "propose", deposit: String(1000n * F) } });
  assert.equal(effectsOf(prop).changes[1].who.name, "Proposal deposit");
  assert.match(effectsOf(prop).note, /refunded once the proposal reaches quorum/);
  for (const kind of ["undelegate", "redelegate", "unlock", "transfer_name", "release_name", "multisig_create", "update_validator", "unjail"]) {
    const fx = effectsOf(tx({ kind, from: vlad }));
    assert.deepEqual(fx.changes.map((c) => c.delta), [-1000n], kind);
    assert.equal(fx.unseen, null, kind);
  }
  for (const kind of ["renew_name", "withdraw_unbonded", "multisig_approve", "report_equivocation"]) assert.ok(effectsOf(tx({ kind, from: vlad })).unseen, kind);
});

test("a failure this node remembers without its transaction: dropped changed nothing, one that ran was charged", () => {
  const dropped = effectsOf({ id: "d", status: "failed", dropped: true, summary: "Transaction dropped without running: x", height: 7 });
  assert.deepEqual([dropped.changes, dropped.unseen], [[], null]);
  const ran = effectsOf({ id: "r", status: "failed", dropped: false, summary: "Transaction failed: x", height: 7 });
  assert.deepEqual(ran.changes, []);
  assert.match(ran.unseen, /fee was charged/);
});

test("summaries from my side leave the names I act on alone (audit main13-render-6)", () => {
  const me = { address: vlad.address, names: ["vlad.boo", "castle.boo", "tomb.boo"], short: vlad.short };
  assert.equal(fromMySide("vlad.boo offered castle.boo to mina.boo", me), "You offered castle.boo to mina.boo");
  assert.equal(fromMySide("vlad.boo withdrew its offer of tomb.boo", me), "You withdrew its offer of tomb.boo");
  assert.equal(fromMySide("vlad.boo renewed vlad.boo for 1 year", me), "You renewed vlad.boo for 1 year");
  assert.equal(fromMySide("vlad.boo accepted castle.boo", me), "You accepted castle.boo");
  assert.equal(fromMySide("vlad.boo released tomb.boo", me), "You released tomb.boo");
  const mina2 = { address: mina.address, names: ["mina.boo", "castle.boo"] };
  assert.equal(fromMySide("vlad.boo offered castle.boo to mina.boo", mina2), "vlad.boo offered castle.boo to you");
});
