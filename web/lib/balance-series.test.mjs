// Balance history: reconstruct a line from sealed transactions, in bigint drops, and slice it
// for the chart. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { bech32m } from "@scure/base";
import { addressBytes, hexOf } from "./address.ts";
import {
  creationFeeOf,
  decodeBalance,
  fangMoves,
  fangSeries,
  programsTouched,
  readHistory,
  tokenMoves,
  tokenSeries,
} from "./balance-series.ts";
import { formatMinor, plotPoints, sliceSeries, valueToY } from "./series.ts";

const F = 1_000_000_000n;
const DAY = 86_400_000;
const now = 1_700_000_000_000;
const clock = { head: 20, blockTimeMs: 1000, now, headAgeMs: 0 };

const addr = (n) => bech32m.encode("boo", bech32m.toWords(new Uint8Array(32).fill(n)));
const me = addr(1);
const other = addr(2);
const fresh = addr(3);
const label = (address) => ({ address, name: null, short: address.slice(0, 10) });
const le64 = (n) => {
  let s = "";
  let x = BigInt(n);
  for (let i = 0; i < 8; i++) {
    s += Number(x & 255n).toString(16).padStart(2, "0");
    x >>= 8n;
  }
  return s;
};

const transfer = (id, height, from, to, amount, extra = {}) => ({
  id,
  summary: "",
  status: "finalized",
  height,
  fee: "1000",
  from: label(from),
  to: label(to),
  amount: amount.toString(),
  kind: "transfer",
  action: { type: "transfer", to },
  ...extra,
});

const fang = (txs, balance, extra = {}) => fangSeries({ me, balance, unit: "FANG", txs, clock, creationFee: 100_000_000n, truncated: false, ...extra });

test("a receive then a send in one block rebuilds the balance, newest transaction first", () => {
  // History is newest first: the send ran after the receive. Reversing that walks through a negative.
  const txs = [transfer("send", 10, me, other, 3n * F, { fee: "0" }), transfer("recv", 10, other, me, 5n * F, { fee: "0" })];
  const series = fang(txs, 2n * F);
  assert.equal(series.points.at(-1).value, 2n * F);
  assert.ok(series.points.some((p) => p.value === 5n * F));
  assert.ok(series.points.every((p) => p.value >= 0n));
  assert.equal(series.approximate, false);
  assert.match(series.summary, /from 0 FANG to 2 FANG/);
});

test("a pending transaction is ignored, and a failed one charges only its fee", () => {
  const pending = fang([transfer("p", null, other, me, 100n * F, { status: "pending", height: null })], 0n);
  assert.equal(pending.points.length, 0);
  assert.match(pending.summary, /No transactions yet/);
  const failed = fang([transfer("f", 8, me, other, 5n * F, { status: "failed", fee: "1000" })], 4n * F);
  assert.equal(failed.points.at(-1).value, 4n * F);
  assert.ok(failed.points.some((p) => p.value === 4n * F + 1000n));
  assert.equal(failed.points.some((p) => p.value === 4n * F + 5n * F), false);
});

test("the account-creation fee burns on the payment that opens an account, and not the next one", () => {
  const fee = 1000n;
  const creation = 100_000_000n;
  const txs = [transfer("second", 12, me, fresh, F), transfer("first", 11, me, fresh, F)];
  const { moves } = fangMoves(txs, me, clock, creation);
  assert.equal(moves[0].delta, -(F + fee + creation));
  assert.equal(moves[1].delta, -(F + fee));
  const self = fangMoves([transfer("self", 5, me, me, 3n * F)], me, clock, creation);
  assert.equal(self.moves[0].delta, -fee);
});

test("a name registration is a sketch: the price isn't in the transaction, and the present stays put", () => {
  const series = fang([{ id: "n", summary: "", status: "finalized", height: 4, fee: "1000", from: label(me), kind: "register_name", action: { type: "register_name", name: "vlad" } }], 5n * F);
  assert.equal(series.approximate, true);
  assert.equal(series.points.at(-1).value, 5n * F);
  assert.match(series.summary, /may not match what the account held/);
});

test("no history draws no line, including when the balance is already above zero", () => {
  assert.match(fang([], 0n).summary, /No transactions yet/);
  const held = fang([], 5n * F);
  assert.equal(held.points.length, 0);
  assert.match(held.summary, /no transaction to draw a line from/);
});

test("a quiet week keeps the balance it entered with", () => {
  const series = fang(
    [transfer("old", 1, other, me, F, { fee: "0" }), transfer("new", 2, other, me, F, { fee: "0" })],
    2n * F,
  );
  // Place the two receipts by hand so the window falls between them.
  series.points = [
    { t: now - 10 * DAY, value: 0n },
    { t: now - 10 * DAY, value: F },
    { t: now - DAY, value: F },
    { t: now - DAY, value: 2n * F },
    { t: now, value: 2n * F },
  ];
  const full = series.points.length;
  const sliced = sliceSeries(series, "7d", now);
  assert.equal(series.points.length, full);
  assert.equal(sliced.points[0].t, now - 7 * DAY);
  assert.equal(sliced.points[0].value, F);
  assert.match(sliced.summary, /last 7 days/);
  assert.match(sliceSeries(series, "all", now).summary, /this history/);
});

test("truncation and a short node are notes, and the line is still anchored", () => {
  const series = fang([transfer("in", 9, other, me, 4n * F, { fee: "0" })], 4n * F, { truncated: true, historyFrom: 50 });
  assert.equal(series.approximate, false);
  assert.equal(series.points.at(-1).value, 4n * F);
  assert.match(series.summary, /newest transactions/);
  assert.match(series.summary, /block 50/);
});

test("token transfers are the program's report: outgoing, two programs, and no line from a negative start", () => {
  const tokenA = addr(4);
  const tokenB = addr(5);
  const binding = addr(6);
  const meKey = hexOf(addressBytes(me));
  const otherKey = hexOf(addressBytes(other));
  const outgoing = `${meKey}${otherKey}${le64(3n)}`;
  const incoming = `${otherKey}${meKey}${le64(5n)}`;
  const call = (events, ok = true) => ({
    id: "call",
    summary: "",
    status: "finalized",
    height: 6,
    fee: "0",
    from: label(me),
    kind: "call_program",
    action: { type: "call_program", calls: [{ program: tokenA }, { program: tokenB }], bindings: [{ program: binding }] },
    program: { outcome: { ok }, claims: { events, omitted: null } },
  });
  const tx = call([
    { program: tokenA, untrusted_name: "transfer", data: outgoing },
    { program: tokenB, untrusted_name: "transfer", data: incoming },
    { program: tokenA, untrusted_name: "something-else", data: "00" },
  ]);
  assert.deepEqual(programsTouched([tx]), [tokenA, tokenB]);
  const { moves } = tokenMoves([tx], meKey, clock);
  assert.equal(moves.length, 2);
  const anchored = tokenSeries({ program: tokenA, label: "A", moves: moves.filter((m) => m.program === tokenA), balance: 7n, now, complete: false, truncated: true, omitted: false });
  assert.equal(anchored.points.at(-1).value, 7n);
  assert.ok(anchored.points.some((p) => p.value === 10n));
  assert.match(anchored.summary, /chain doesn't check/);
  const fromZero = tokenSeries({ program: tokenB, label: "B", moves: moves.filter((m) => m.program === tokenB), balance: null, now, complete: true, truncated: false, omitted: false });
  assert.equal(fromZero.points[0].value, 0n);
  assert.equal(fromZero.points.at(-1).value, 5n);
  const negative = tokenSeries({ program: tokenA, label: "A", moves: moves.filter((m) => m.program === tokenA), balance: null, now, complete: true, truncated: false, omitted: false });
  assert.equal(negative.points.length, 0);
  assert.match(negative.summary, /don't start from zero/);
  assert.equal(tokenMoves([call([], false)], meKey, clock).moves.length, 0);
  assert.equal(decodeBalance(le64(1n)), 1n);
  assert.equal(decodeBalance(`${le64(1n)}ff`), null);
  assert.equal(creationFeeOf({ account_creation_fee: "100000000" }), 100_000_000n);
});

test("pages stop at the cursor, and a node without pages is the newest slice", async () => {
  const page = await readHistory(async (q) => (q.before ? { txs: [{ id: "b", status: "finalized", height: 1 }], next: null, paged: true, history_from: 1 } : { txs: [{ id: "a", status: "finalized", height: 2 }], next: "2.0", paged: true, history_from: 1 }));
  assert.deepEqual(page.txs.map((t) => t.id), ["a", "b"]);
  assert.equal(page.truncated, false);
  const capped = await readHistory(async () => ({ txs: [{ id: "a", status: "finalized", height: 2 }], next: "2.0", paged: true }), { maxPages: 1 });
  assert.equal(capped.truncated, true);
  const old = await readHistory(async () => ({ txs: Array.from({ length: 25 }, (_, i) => ({ id: `t${i}`, status: "finalized", height: i + 1 })), next: null, paged: false }));
  assert.equal(old.truncated, true);
});

test("the chart scales a balance past 2^53 without leaving the integers", () => {
  const huge = 10n ** 24n;
  const box = { top: 16, bottom: 22, left: 78, right: 10 };
  const pts = plotPoints([{ t: 0, value: 0n }, { t: 1, value: huge }], 320, 168, box);
  assert.ok(pts[1].y < pts[0].y);
  assert.ok(pts.every((p) => Number.isFinite(p.x) && Number.isFinite(p.y)));
  assert.ok(valueToY(huge, 0n, huge, 16, 100) < valueToY(0n, 0n, huge, 16, 100));
  assert.equal(formatMinor(1500n, 1n), "1,500");
  assert.doesNotMatch(formatMinor(1n, F), /^0$/);
});

test("the wallet's chart sources never say contract", () => {
  for (const f of ["./series.ts", "./balance-series.ts", "./address.ts", "../components/TimeSeriesChart.tsx", "../components/BalanceHistory.tsx", "../app/page.tsx"]) {
    assert.doesNotMatch(readFileSync(new URL(f, import.meta.url), "utf8"), /\bcontracts?\b/i, f);
  }
});
