// lib/explorer/leaderboard.ts: the three boards' ranking. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { playersOf, refOf, topSigners } from "./explorer/leaderboard.ts";

const BOT = "boo1bot";
const F = 1_000_000_000n;
const who = (address, name = null) => ({ address, name, short: address });
const tx = (id, from, to, drops, o = {}) => ({ id, summary: "", kind: "transfer", status: "finalized", error: null, height: 10, from: who(from), to: who(to), amount: String(drops), ...o });
const sig = (from) => ({ id: from, height: 1, from: who(from), memo: "", amount: null, amount_display: null });

test("signers are ranked by signatures, ties keep the newest first", () => {
  const rows = topSigners([sig("a"), sig("b"), sig("c"), sig("b"), sig("c"), sig("d")]);
  assert.deepEqual(rows.map((r) => [r.who.address, r.signatures]), [["b", 2], ["c", 2], ["a", 1], ["d", 1]]);
  assert.deepEqual(topSigners([]), []);
});

test("players: plays in, winnings out, ranked by net", () => {
  const rows = playersOf(
    [
      tx("1", BOT, "boo1ann", 3n * F),
      tx("2", "boo1ann", BOT, F),
      tx("3", "boo1bob", BOT, F),
      tx("4", "boo1bob", BOT, 2n * F),
      tx("5", "boo1cyd", BOT, F),
      tx("6", BOT, "boo1cyd", F),
    ],
    BOT,
  );
  assert.deepEqual(rows.map((r) => [r.who.address, r.plays, r.won, r.net]), [["boo1ann", 1, 3n * F, 2n * F], ["boo1cyd", 1, F, 0n], ["boo1bob", 2, 0n, -3n * F]]);
});

test("a player with plays and no winnings has a negative net; ties go to more plays", () => {
  const rows = playersOf([tx("1", "boo1x", BOT, F), tx("2", "boo1y", BOT, F), tx("3", "boo1y", BOT, F), tx("4", BOT, "boo1y", F)], BOT);
  assert.deepEqual(rows.map((r) => [r.who.address, r.net]), [["boo1y", -F], ["boo1x", -F]]);
});

test("the bot's own transfers, dust, failed, pending and other kinds are ignored; a repeat counts once", () => {
  const rows = playersOf(
    [
      tx("1", BOT, BOT, 5n * F),
      tx("2", "boo1x", BOT, F - 1n),
      tx("3", "boo1x", BOT, F, { status: "failed", error: "nope" }),
      tx("4", "boo1x", BOT, F, { status: "pending", height: null }),
      tx("5", "boo1x", BOT, F, { kind: "register_name" }),
      tx("6", "boo1x", "boo1other", F),
      tx("7", "boo1x", BOT, F),
      tx("7", "boo1x", BOT, F),
    ],
    BOT,
  );
  assert.deepEqual(rows.map((r) => [r.who.address, r.plays, r.net]), [["boo1x", 1, -F]]);
});

test("amounts beyond 2^53 drops stay exact", () => {
  const huge = 2n ** 60n + 1n;
  const [r] = playersOf([tx("1", "boo1x", BOT, huge), tx("2", BOT, "boo1x", huge - 7n)], BOT);
  assert.equal(r.staked, huge);
  assert.equal(r.net, -7n);
});

test("rows name an account by its name, or its short address; system labels and hostile names stay inert", () => {
  assert.deepEqual(refOf(who("boo1a", "vlad.boo")), { ref: "vlad.boo", text: "vlad.boo", named: true });
  assert.deepEqual(refOf(who("boo1a")), { ref: "boo1a", text: "boo1a", named: false });
  assert.equal(refOf(who("boo1a", "🧛 validator")).named, false);
  assert.equal(refOf(who("boo1a", "a‮b.boo")).text, "a b.boo");
});
