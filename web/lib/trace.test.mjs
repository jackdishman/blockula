// lib/explorer/trace.ts: the money graph, its edges and its left-to-right layout.
// Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { buildTrace, edgesOf, movesOf, parseFollow, followParam } from "./explorer/trace.ts";

const F = 1_000_000_000n;
const label = (name) => ({ address: `boo1${name}`, name: `${name}.boo`, short: `boo1${name}…` });
const [vlad, mina, lucy, val, ren] = ["vlad", "mina", "lucy", "val", "ren"].map(label);
let n = 0;
const tx = (o) => ({ id: `t${n++}`, summary: "", status: "finalized", height: 100 + n, fee: "1000", ...o });
const send = (from, to, fang, extra = {}) => tx({ kind: "transfer", from, to, amount: String(BigInt(fang) * F), ...extra });

test("moves are what receipts show: the amount, never the fee; failed and pending ones move nothing", () => {
  const { moves, who } = movesOf([send(vlad, mina, 5), send(vlad, mina, 1, { status: "failed" }), send(vlad, lucy, 2, { status: "pending", height: null })]);
  assert.deepEqual(moves.map((m) => [m.from, m.to, m.amount]), [["boo1vlad", "boo1mina", 5n * F]]);
  assert.equal(who.get("boo1mina").name, "mina.boo");
});

test("escrow is a holding node, not an account", () => {
  const { moves, who } = movesOf([tx({ kind: "fund_hunt", from: vlad, amount: String(3n * F), action: { type: "fund_hunt", hunt_id: 2 } })]);
  assert.equal(moves[0].to, "holding:hunt:2");
  assert.equal(who.get("holding:hunt:2").holding, true);
});

test("edges sum per direction, biggest first, newest transaction first", () => {
  const a = send(vlad, mina, 2), b = send(vlad, mina, 3), c = send(mina, vlad, 1), d = send(vlad, lucy, 9);
  const edges = edgesOf(movesOf([a, b, c, d]).moves);
  assert.deepEqual(edges.map((e) => [e.from.slice(4), e.to.slice(4), e.amount / F, e.txs.length]), [["vlad", "lucy", 9n, 1], ["vlad", "mina", 5n, 2], ["mina", "vlad", 1n, 1]]);
});

test("layout: payers sit left of the root, payees right, and nobody shares a cell", () => {
  const txs = [send(val, vlad, 200), send(vlad, mina, 20), send(mina, vlad, 3), send(vlad, lucy, 7), send(vlad, ren, 1)];
  const t = buildTrace("boo1vlad", new Map([["boo1vlad", txs]]));
  const at = Object.fromEntries(t.nodes.map((x) => [x.id.slice(4), x]));
  assert.equal(at.vlad.col, 0);
  assert.equal(at.val.col, -1, "the validator paid vlad");
  assert.equal(at.mina.col, 1, "vlad paid mina more than she paid back");
  assert.equal(at.lucy.col, 1);
  // Biggest flow nearest the root's row.
  assert.equal(at.mina.row, at.vlad.row);
  const cells = t.nodes.map((x) => `${x.col}:${x.row}`);
  assert.equal(new Set(cells).size, cells.length);
  assert.equal(t.minCol, -1);
  assert.equal(t.maxCol, 1);
  assert.ok(Math.min(...t.nodes.map((x) => x.row)) === 0, "rows start at 0");
  assert.equal(t.counted, 5);
});

test("following a neighbor adds its counterparties a hop further out; unfollowing drops them", () => {
  const mine = [send(vlad, mina, 20)];
  const hers = [send(vlad, mina, 20, { id: mine[0].id }), send(mina, lucy, 4), send(ren, mina, 2)];
  const both = buildTrace("boo1vlad", new Map([["boo1vlad", mine], ["boo1mina", hers]]));
  const at = Object.fromEntries(both.nodes.map((x) => [x.id.slice(4), x]));
  assert.equal(at.lucy.col, 2);
  assert.equal(at.ren.col, 0, "ren paid mina, so ren sits left of her");
  assert.equal(at.lucy.depth, 2);
  assert.equal(both.counted, 3, "a transaction both accounts list counts once");
  assert.equal(at.mina.followed, true);
  const one = buildTrace("boo1vlad", new Map([["boo1vlad", mine]]));
  assert.deepEqual(one.nodes.map((x) => x.id).sort(), ["boo1mina", "boo1vlad"]);
});

test("the follow list round-trips through a URL without the root", () => {
  assert.equal(followParam(["boo1vlad", "boo1mina", "boo1lucy"], "boo1vlad"), "boo1mina,boo1lucy");
  assert.deepEqual(parseFollow("boo1mina, boo1lucy,"), ["boo1mina", "boo1lucy"]);
  assert.deepEqual(parseFollow(null), []);
});
