// lib/explorer/broadcast.ts: one signed transaction through several nodes at once (the blackjack
// table's seed reveal, docs/BLACKJACK_REVIEW.md BJ-04). Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { firstAccepted } from "./explorer/broadcast.ts";

class Refusal extends Error {}
const refused = (e) => e instanceof Refusal;
const later = (ms, f) => new Promise((resolve, reject) => setTimeout(() => f(resolve, reject), ms));

test("one node taking it is enough, and every node is still tried", async () => {
  const tried = [];
  const done = [];
  const post = (node) => {
    tried.push(node);
    if (node === "a") return later(1, (_, no) => (done.push(node), no(new Error("a is down"))));
    if (node === "b") return later(5, (ok) => (done.push(node), ok("id-b")));
    return later(30, (ok) => (done.push(node), ok("id-c")));
  };
  assert.equal(await firstAccepted(["a", "b", "c"], post, refused), "id-b");
  // All three were posted to at once, before any answered; the slow one still finishes.
  assert.deepEqual(tried, ["a", "b", "c"]);
  assert.deepEqual(done, ["a", "b"]);
  await later(40, (ok) => ok());
  assert.deepEqual(done, ["a", "b", "c"]);
});

test("the selected node keeping it back doesn't stop the others", async () => {
  // The selected node never answers (it holds the transaction): a witness's acceptance returns.
  const post = (node) => (node === "selected" ? new Promise(() => undefined) : Promise.resolve(node));
  assert.equal(await firstAccepted(["selected", "w1"], post, refused), "w1");
  // The selected node refuses it, a witness takes it: accepted.
  assert.equal(await firstAccepted(["selected", "w1"], (n) => (n === "selected" ? Promise.reject(new Refusal("no")) : Promise.resolve(n)), refused), "w1");
});

test("no node took it: a refusal only when every node refused", async () => {
  const no = new Refusal("refused at admission");
  const down = new Error("unreachable");
  // Every node refused: the first node's (the selected one's) refusal.
  await assert.rejects(firstAccepted(["a", "b"], (n) => Promise.reject(n === "a" ? no : new Refusal("b says no")), refused), (e) => e === no);
  // One may have received it: that error, so the caller treats the transaction as possibly sent.
  await assert.rejects(firstAccepted(["a", "b"], (n) => Promise.reject(n === "a" ? no : down), refused), (e) => e === down);
  await assert.rejects(firstAccepted(["a", "b"], (n) => Promise.reject(n === "a" ? down : no), refused), (e) => e === down);
  // A post that throws outright counts as that node's failure; no node at all is an error.
  await assert.rejects(firstAccepted(["a"], () => { throw down; }, refused), (e) => e === down);
  await assert.rejects(firstAccepted([], () => Promise.resolve("x"), refused), /no node/);
});
