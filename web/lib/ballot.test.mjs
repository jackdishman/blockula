// lib/explorer/ballot.ts: a juror's Séance ballot kept in the browser between sealing and
// revealing (components/explorer/JurorBallot.tsx). Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { seanceCommitment } from "@blockula/sdk";
import { KEPT, ballotKey, forgetBallots, loadBallots, newSalt, prUrl, saveBallot, sealedBallot, spamIds, voteString } from "./explorer/ballot.ts";

const ME = "boo1k3cts6u53yggzevug89yxy5u5n73xmxjglkt6zua8uph75s4lmwshek62a";
const OTHER = "boo1dcggj8cmxq2u0kzswcgcmkpm0yfu7wrlnyvxhjx6fq2gsu3hhw9qlcec4k";
const CHAIN = { chainId: "blockula-dev", genesisHash: "ab".repeat(32) };

/** A Map-backed `Storage`, or one that refuses every write, as a private window can. */
function memory({ refuse = false } = {}) {
  const m = new Map();
  return {
    m,
    getItem: (k) => (m.has(k) ? m.get(k) : null),
    setItem: (k, v) => {
      if (refuse) throw new DOMException("quota", "QuotaExceededError");
      m.set(k, String(v));
    },
    removeItem: (k) => m.delete(k),
  };
}

test("a ballot is kept per chain, account and Séance, and read back as it was", () => {
  const key = ballotKey(CHAIN, ME, 3);
  assert.notEqual(key, ballotKey(CHAIN, OTHER, 3));
  assert.notEqual(key, ballotKey(CHAIN, ME, 4));
  assert.notEqual(key, ballotKey({ ...CHAIN, genesisHash: "cd".repeat(32) }, ME, 3));
  const s = memory();
  const b = { votes: "ab=", spam: [7], salt: newSalt() };
  assert.match(b.salt, /^[0-9a-f]{32}$/);
  assert.notEqual(b.salt, newSalt());
  assert.equal(saveBallot(s, key, b), true);
  assert.deepEqual(loadBallots(s, key), [b]);
  forgetBallots(s, key);
  assert.deepEqual(loadBallots(s, key), []);
});

test("a new seal is kept beside the earlier ones, never over them", () => {
  // A seal still on its way when the juror seals again must keep its salt: whichever lands is revealed.
  const key = ballotKey(CHAIN, ME, 3);
  const s = memory();
  const first = { votes: "ab", spam: [], salt: "11".repeat(16) };
  const second = { votes: "ba", spam: [2], salt: "22".repeat(16) };
  saveBallot(s, key, first);
  saveBallot(s, key, second);
  assert.deepEqual(loadBallots(s, key), [first, second]);
  const landed = seanceCommitment(3, ME, first.votes, first.spam, first.salt);
  assert.deepEqual(sealedBallot(loadBallots(s, key), 3, ME, landed), first);
  // Only the newest few are kept.
  for (let i = 0; i < KEPT + 2; i++) saveBallot(s, key, { votes: "=", spam: [], salt: i.toString(16).padStart(32, "0") });
  assert.equal(loadBallots(s, key).length, KEPT);
});

test("storage that refuses, or holds something else, never throws", () => {
  const key = ballotKey(CHAIN, ME, 3);
  assert.equal(saveBallot(memory({ refuse: true }), key, { votes: "a", spam: [], salt: "00" }), false);
  assert.equal(saveBallot(null, key, { votes: "a", spam: [], salt: "00" }), false);
  assert.deepEqual(loadBallots(null, key), []);
  const s = memory();
  for (const junk of ["{", "null", "[]", '{"v":2,"ballots":[{"votes":"a","spam":[],"salt":"00"}]}', '{"v":1,"ballots":{}}']) {
    s.m.set(key, junk);
    assert.deepEqual(loadBallots(s, key), [], junk);
  }
  // Entries that aren't ballots are dropped; good ones beside them are kept.
  s.m.set(key, JSON.stringify({ v: 1, ballots: [{ votes: "x", spam: [], salt: "00" }, { votes: "a", spam: [-1], salt: "00" }, { votes: "a", spam: [], salt: "<b>" }, null, { votes: "a", spam: [1], salt: "0f" }] }));
  assert.deepEqual(loadBallots(s, key), [{ votes: "a", spam: [1], salt: "0f" }]);
  forgetBallots({ removeItem: () => { throw new Error("blocked"); } }, key);
});

test("a stored ballot counts only if it recomputes to the commitment the node lists", () => {
  const b = { votes: "ab", spam: [2], salt: "11".repeat(16) };
  const commitment = seanceCommitment(3, ME, b.votes, b.spam, b.salt);
  assert.deepEqual(sealedBallot([b], 3, ME, commitment), b);
  // Sealed from another device (another salt), not sealed yet, another Séance or another juror.
  assert.equal(sealedBallot([{ ...b, salt: "22".repeat(16) }], 3, ME, commitment), null);
  assert.equal(sealedBallot([b], 3, ME, null), null);
  assert.equal(sealedBallot([b], 4, ME, commitment), null);
  assert.equal(sealedBallot([b], 3, OTHER, commitment), null);
  assert.equal(sealedBallot([], 3, ME, commitment), null);
});

test("every pair needs a vote, and spam flags are limited to the claims shown", () => {
  assert.deepEqual(voteString(["a", "=", "b"], 3), { votes: "a=b" });
  assert.deepEqual(voteString(["a", undefined, "b"], 3), { missing: 1 });
  assert.deepEqual(voteString(["a"], 3), { missing: 2 });
  assert.deepEqual(voteString(["a", "x"], 2), { missing: 1 });
  assert.deepEqual(spamIds([9, 2, 2, 5], [2, 5, 7]), [2, 5]);
});

test("a PR link is made only from a plain owner/name repo", () => {
  assert.equal(prUrl("jackdishman/blockula", 159), "https://github.com/jackdishman/blockula/pull/159");
  assert.equal(prUrl("a-b/c.d_e", 1), "https://github.com/a-b/c.d_e/pull/1");
  for (const [repo, pr] of [["javascript:alert(1)//x", 1], ["evil.example/x", 1], ["o/..", 1], ["o/r?x=1", 1], ["o/r/../../x", 1], ["-o/r", 1], ['o/r"><img src=x>', 1], ["o/r", 0], ["o/r", 1.5]]) {
    assert.equal(prUrl(repo, pr), null, `${repo} #${pr}`);
  }
});
