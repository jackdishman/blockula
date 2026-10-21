// lib/creators.ts: memo hygiene, GitHub checks, multisig wording, claim timelines and the
// contribution notes. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  approveCommand,
  approxDate,
  BOND_TERMS,
  claimTimeline,
  CONTRIBUTION_NOTES,
  CONTRIBUTION_NOTES_BOTH,
  GITHUB_HANDLE,
  memoLooksLikeContact,
  memoView,
  multisigRule,
  multisigShape,
  readGithubCheck,
  tinyPayment,
  TINY_PAYMENT_DROPS,
} from "./creators.ts";

const F = 1_000_000_000n;
const FEE = "64920";
const vlad = { address: "boo1vlad", name: "vlad.boo", short: "boo1vlad…" };
const pay = (amount, memo, o = {}) => ({ amount: String(amount), fee: FEE, memo, from: vlad, ...o });

test("a payment under 0.01 FANG, or under ten times its fee, is tiny", () => {
  assert.equal(TINY_PAYMENT_DROPS, 10_000_000n);
  assert.equal(tinyPayment(pay(1n, "")), true); // 0.000000001 FANG
  assert.equal(tinyPayment(pay(TINY_PAYMENT_DROPS - 1n, "")), true);
  assert.equal(tinyPayment(pay(TINY_PAYMENT_DROPS, "")), false);
  // A fee that rose: ten times it is the bar.
  assert.equal(tinyPayment(pay(F / 10n, "", { fee: String(F / 50n) })), true);
  assert.equal(tinyPayment(pay(F, "", { fee: String(F / 50n) })), false);
  // Pending: what it allows to be charged.
  assert.equal(tinyPayment({ amount: String(F / 10n), max_fee: String(F / 50n) }), true);
  // No amount (not a payment) folds too.
  assert.equal(tinyPayment({ amount: null, fee: FEE }), true);
});

test("a tiny payment's memo is folded; a real one's isn't; the viewer's own never is", () => {
  assert.deepEqual(memoView(pay(1n, "hi")), { text: "hi", folded: true, warn: false });
  assert.deepEqual(memoView(pay(3n * F, "for the pumpkins")), { text: "for the pumpkins", folded: false, warn: false });
  assert.equal(memoView(pay(1n, "hi"), "boo1vlad").folded, false);
  assert.equal(memoView(pay(1n, "")), null);
});

test("memos that read like someone getting in touch carry the warning", () => {
  for (const m of [
    "URGENT: your name expires today, renew at https://blockula-renew.example",
    "visit www.example.com",
    "Blockula support: reply here",
    "Your refund is waiting",
    "verify your wallet at fang-claims.xyz",
    "t.me/blockula_help",
    "RENEWAL notice",
  ])
    assert.equal(memoLooksLikeContact(m), true, m);
  for (const m of ["for the pumpkins", "thanks mina.boo!", "coffee ☕", "for PR #12"]) assert.equal(memoLooksLikeContact(m), false, m);
  assert.equal(memoView(pay(3n * F, "renew at https://x.example")).warn, true);
  // Nobody needs warning about their own words.
  assert.equal(memoView(pay(3n * F, "renew at https://x.example"), "boo1vlad").warn, false);
});

test("GitHub handles are what the chain accepts, and a check is verified only for this handle and account", () => {
  for (const h of ["octocat", "a", "a-b", "A1-b2-c3", "x".repeat(39)]) assert.equal(GITHUB_HANDLE.test(h), true, h);
  for (const h of ["", "-a", "a-", "a--b", "x".repeat(40), "vlad<img src=x>", "a b", "a_b", "../x"]) assert.equal(GITHUB_HANDLE.test(h), false, h);
  const ok = { handle: "Octocat", account: { address: "boo1a" }, verified: true, reason: "gist owned by the handle contains the account's address" };
  assert.deepEqual(readGithubCheck(ok, "octocat", "boo1a"), { verified: true, reason: ok.reason });
  assert.equal(readGithubCheck({ ...ok, verified: "true" }, "octocat", "boo1a").verified, false);
  assert.equal(readGithubCheck({ ...ok, verified: false, reason: "gist not found" }, "octocat", "boo1a").reason, "gist not found");
  assert.deepEqual(readGithubCheck(ok, "octocat", "boo1b"), { verified: false, reason: "the node's check is for another account" });
  assert.deepEqual(readGithubCheck(ok, "torvalds", "boo1a"), { verified: false, reason: "the node answered for another handle" });
  assert.equal(readGithubCheck(null, "octocat", "boo1a").verified, false);
});

test("a multisig says how many owners must approve", () => {
  assert.equal(multisigShape(2, 3), "2-of-3 multisig");
  assert.equal(multisigRule(2, 3), "Any 2 of these 3 owners must approve every action");
  assert.equal(multisigRule(2, 2), "Both owners must approve every action");
  assert.equal(multisigRule(3, 3), "All 3 owners must approve every action");
  assert.equal(multisigRule(1, 3), "Any one of these 3 owners can approve an action alone");
  assert.equal(approveCommand(4, "boo1coven", "ab".repeat(32)), `blockula multisig approve 4 --from boo1coven --digest ${"ab".repeat(32)}`);
});

test("contribution notes: testnet's is the house line, mainnet's isn't a testnet disclaimer, and the bond isn't 'refundable'", () => {
  assert.match(CONTRIBUTION_NOTES.testnet, /^Testnet FANG, Hunts and Soul have no monetary value/);
  assert.doesNotMatch(CONTRIBUTION_NOTES.mainnet, /Testnet FANG/);
  assert.match(CONTRIBUTION_NOTES_BOTH, /On the testnet: .* On mainnet: /);
  assert.equal(BOND_TERMS, "returned unless validators refute the claim or the jury marks it spam");
  for (const s of [CONTRIBUTION_NOTES.testnet, CONTRIBUTION_NOTES.mainnet, BOND_TERMS]) assert.doesNotMatch(s, /contract|refundable/i);
});

test("an approximate date is a calendar day in UTC", () => {
  const now = Date.UTC(2026, 9, 31, 12);
  assert.equal(approxDate(0, 1000, now), "Oct 31, 2026");
  assert.equal(approxDate(86_400 * 365, 1000, now), "Oct 31, 2027");
});

test("a claim's timeline: from submission to a fully vested reward", () => {
  const now = Date.UTC(2026, 9, 31, 12);
  const rules = { claim_maturity_blocks: 2_592_000, vesting_blocks: 7_776_000, seance_blocks: 604_800 };
  const base = { submitted_at: 100, verified_at: null, eligible_at: null, expires_at: 604_900, seance: null, payout: "0", closed_at: null };
  const labels = (steps) => steps.map((s) => `${s.label}: ${s.state}`);

  // Waiting for validators.
  const pending = claimTimeline({ ...base, status: "pending" }, rules, 200, 1000, now);
  assert.deepEqual(labels(pending), ["Submitted: done", "Merge checked by validators: now", "Eligible for a Séance: later", "Séance: later", "Reward fully vested: later"]);
  assert.match(pending[1].when, /^waiting, until block #604,900 \(in about 7 days, around Nov 7, 2026\); if they don't settle it by then, the bond comes back$/);
  assert.equal(pending[2].when, "about 30 days after the merge is checked (the revert window)");
  assert.equal(pending[3].when, "the first one after it's eligible (one opens about every 7 days)");
  assert.equal(pending[4].when, "about 90 days after its Séance pays it (it vests gradually until then, and can be clawed back)");

  // Verified, in its revert window.
  const verified = claimTimeline({ ...base, status: "verified", verified_at: 150, eligible_at: 150 + 2_592_000 }, rules, 200, 1000, now);
  assert.equal(verified[1].state, "done");
  assert.match(verified[2].when, /^after the revert window, block #2,592,150 \(in about 30 days, around Nov 30, 2026\)$/);

  // In a Séance.
  const judging = claimTimeline({ ...base, status: "in_seance", verified_at: 150, eligible_at: 150, seance: 2 }, rules, 9_000, 1000, now);
  assert.deepEqual(labels(judging).slice(2), ["Eligible for a Séance: done", "Séance: now", "Reward fully vested: later"]);
  assert.equal(judging[3].seance, 2);

  // Scored and paid: vesting ends `vesting_blocks` after the Séance closed.
  const paid = claimTimeline({ ...base, status: "scored", verified_at: 150, eligible_at: 150, seance: 1, closed_at: 1_000, payout: String(12n * F) }, rules, 2_000, 1000, now);
  assert.equal(paid[3].when, "judged, closed at block #1,000");
  assert.equal(paid[4].state, "later");
  assert.match(paid[4].when, /^block #7,777,000 \(in about 90 days/);
  // Scored below the threshold, marked spam, refuted: no reward.
  assert.equal(claimTimeline({ ...base, status: "scored", seance: 1, closed_at: 1_000, verified_at: 150 }, rules, 2_000)[4].state, "skipped");
  const spam = claimTimeline({ ...base, status: "spam", seance: 1, closed_at: 1_000, verified_at: 150 }, rules, 2_000);
  assert.deepEqual(labels(spam).slice(3), ["Séance: done", "Reward fully vested: skipped"]);
  const refuted = claimTimeline({ ...base, status: "rejected", expires_at: null }, rules, 2_000);
  assert.deepEqual(labels(refuted).slice(1), ["Merge checked by validators: skipped", "Eligible for a Séance: skipped", "Séance: skipped", "Reward fully vested: skipped"]);
  // Without the pool's rules, it still reads.
  assert.equal(claimTimeline({ ...base, status: "pending" }, null, undefined)[2].when, "after the merge is checked (the revert window)");
});
