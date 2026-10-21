// lib/money.ts and lib/format.ts's money formatting: vote lock ends, lock words, what "Release
// expired locks" can release, name warnings, and amounts for reading. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { aboutWhen, displayFang, fangInput, plural } from "./format.ts";
import { changedHandsLately, lockWords, nameRisks, voteLockEnd, voteLocks } from "./money.ts";

const F = 1_000_000_000n;
const NOW = Date.UTC(2026, 9, 31, 12, 0);

test("displayFang groups thousands, rounds summaries to 4 decimals with ≈, and keeps reviews exact (USER-15)", () => {
  assert.equal(displayFang(40_000_010_495_700_940n), "≈ 40,000,010.4957 FANG");
  assert.equal(displayFang(40_000_010_495_700_940n, { exact: true }), "40,000,010.49570094 FANG");
  assert.equal(displayFang(9_999_999_993n), "≈ 10 FANG");
  assert.equal(displayFang(1_086_233_173_620n), "≈ 1,086.2332 FANG");
  assert.equal(displayFang(25n * F), "25 FANG");
  assert.equal(displayFang("1500000000"), "1.5 FANG");
  assert.equal(displayFang(0n), "0 FANG");
  assert.equal(displayFang(64_920n), "≈ 0.0001 FANG");
  assert.equal(displayFang(64_920n, { exact: true }), "0.00006492 FANG");
  assert.equal(displayFang(1234n * F, { unit: false }), "1,234");
  assert.equal(fangInput(49_999_935_080n), "49.99993508");
  assert.equal(fangInput(50n * F), "50");
  assert.equal(plural(1, "block"), "1 block");
  assert.equal(plural(1200, "block"), "1,200 blocks");
});

test("aboutWhen: a clock time today or tomorrow, a date further out", () => {
  assert.equal(aboutWhen(3_600, 1000, NOW, "UTC"), "about 13:00");
  assert.equal(aboutWhen(14 * 86_400, 1000, NOW, "UTC"), "about Nov 14");
  assert.equal(aboutWhen(100 * 86_400, 1000, NOW, "UTC"), "about Feb 8, 2027");
  assert.equal(aboutWhen(30, 1000, NOW, "UTC"), "about 12:00");
  // Past midnight in the reader's own clock: "tomorrow", not a bare time that reads as earlier
  // today (review WM-R4). NOW is 12:00 UTC, 08:00 in New York.
  assert.equal(aboutWhen(11 * 3_600, 1000, NOW, "UTC"), "about 23:00");
  assert.equal(aboutWhen(17 * 3_600, 1000, NOW, "UTC"), "about 05:00 tomorrow");
  assert.equal(aboutWhen(15 * 3_600, 1000, NOW, "America/New_York"), "about 23:00");
  assert.equal(aboutWhen(17 * 3_600, 1000, NOW, "America/New_York"), "about 01:00 tomorrow");
});

test("a vote's lock ends after voting ends, plus the conviction's lock periods; conviction 0 still holds until voting ends", () => {
  assert.equal(voteLockEnd(1_000, 0, 604_800), 1_001);
  assert.equal(voteLockEnd(1_000, 1, 604_800), 605_801);
  assert.equal(voteLockEnd(1_000, 2, 604_800), 1_210_601);
  assert.equal(voteLockEnd(1_000, 6, 10), 1_321);
});

test("lock words: a vote by proposal, vesting by its reason, never the raw id", () => {
  assert.match(lockWords({ id: "vote:2", reason: "vote:2", until: 952, vesting: false }, 900, 1000, NOW), /^by your vote on proposal #2 until about \d{2}:\d{2}$/);
  assert.match(lockWords({ id: "vote:0", reason: "vote on proposal #0", until: 2_000, vesting: false }, 900, 1000, NOW), /^by your vote on proposal #0 until about/);
  assert.match(lockWords({ id: "vesting", reason: "founder", until: 900 + 400 * 86_400, vesting: true }, 900, 1000, NOW), /^while it vests \(founder\), until about [A-Z][a-z]{2} \d{1,2}, 2027$/);
  assert.match(lockWords({ id: "x", reason: "a bond", until: 910, vesting: false }, 900, 1000, NOW), /^by a bond until about \d{2}:\d{2}$/);
});

const me = "boo1vlad";
const vote = (height, proposal_id, conviction) => ({ kind: "vote", status: "finalized", height, from: { address: me }, action: { type: "vote", proposal_id, conviction } });
const unlock = (height) => ({ kind: "unlock", status: "finalized", height, from: { address: me }, action: { type: "unlock" } });

test("voteLocks: a lock that ended since the last unlock can be released; one still holding can't", () => {
  const proposals = [{ id: 1, voting_ends_at: 100 }, { id: 2, voting_ends_at: 500 }];
  // Proposal 1 at conviction 1 (period 10): ends at 111. Proposal 2 at 0: ends at 501.
  const recent = [vote(50, 1, 1), vote(60, 2, 0)];
  assert.deepEqual(voteLocks({ recent, me, proposals, lockPeriodBlocks: 10, head: 105 }), { ended: [], nextEnd: 111, known: true });
  assert.deepEqual(voteLocks({ recent, me, proposals, lockPeriodBlocks: 10, head: 110 }), { ended: [1], nextEnd: 501, known: true });
  // Released at 120: nothing left until proposal 2's ends.
  assert.deepEqual(voteLocks({ recent: [...recent, unlock(120)], me, proposals, lockPeriodBlocks: 10, head: 130 }).ended, []);
  assert.deepEqual(voteLocks({ recent: [...recent, unlock(120)], me, proposals, lockPeriodBlocks: 10, head: 600 }).ended, [2]);
  // A re-vote extends the lock (the later end counts).
  assert.deepEqual(voteLocks({ recent: [vote(50, 1, 1), vote(55, 1, 0)], me, proposals, lockPeriodBlocks: 10, head: 110 }).ended, [1]);
  assert.deepEqual(voteLocks({ recent: [vote(50, 1, 0), vote(55, 1, 2)], me, proposals, lockPeriodBlocks: 10, head: 110 }).ended, []);
  // The node says the lock still holds: it isn't ended, whatever the arithmetic says.
  assert.deepEqual(voteLocks({ recent, me, proposals, lockPeriodBlocks: 10, head: 110, active: [{ id: "vote:1", reason: "vote:1", until: 140, vesting: false }] }).ended, []);
  // Someone else's votes, failed votes and pending ones don't count.
  assert.deepEqual(voteLocks({ recent: [{ ...vote(50, 1, 1), from: { address: "boo1mina" } }, { ...vote(51, 1, 1), status: "failed" }, { ...vote(52, 1, 1), height: null }], me, proposals, lockPeriodBlocks: 10, head: 500 }), { ended: [], nextEnd: null, known: true });
});

test("voteLocks: unsure when history may be cut short or a proposal is missing", () => {
  const proposals = [{ id: 1, voting_ends_at: 100 }];
  assert.equal(voteLocks({ recent: [vote(50, 9, 1)], me, proposals, lockPeriodBlocks: 10, head: 500 }).known, false);
  assert.equal(voteLocks({ recent: [], me, proposals, lockPeriodBlocks: 10, head: 500, historyFull: true }).known, false);
  assert.equal(voteLocks({ recent: [vote(50, 1, 1)], me, proposals, head: 500 }).known, false);
  // The account hasn't been read (a failed or pending /api/account): nothing is known, so no
  // "You have no vote locks" and no disabled button (review WM-R1).
  assert.deepEqual(voteLocks({ recent: null, me, proposals, lockPeriodBlocks: 10, head: 500 }), { ended: [], nextEnd: null, known: false });
});

test("nameRisks: only when the name lapses before the payment stops being valid, or changed hands lately (CRE-5)", () => {
  const head = 10_000_000;
  const o = { name: "bobby.boo", head, validUntil: head + 600, signedAsName: true, now: NOW };
  assert.deepEqual(nameRisks({ status: "active", registered_at: 1_000, expires_at: head + 1_000_000 }, o), []);
  assert.match(nameRisks({ status: "active", registered_at: 1_000, expires_at: head + 300 }, o)[0], /^bobby\.boo expires about \d{2}:\d{2}, before this payment stops being valid/);
  // What happens then, as the chain does it (review WM-R3): an expired name doesn't resolve, so the
  // payment fails (and may be charged its fee) unless renewed; nobody else gets it while the grace
  // period lasts, and only a grace period shorter than the validity window lets a newcomer in.
  const lapsing = nameRisks({ status: "active", registered_at: 1_000, expires_at: head + 300, grace_until: head + 300 + 2_592_000 }, o)[0];
  assert.match(lapsing, /If it runs after that, it fails unless the owner renews the name first, and its fee may still be charged\. Pay their address instead\.$/);
  assert.doesNotMatch(lapsing, /today's owner/);
  assert.match(nameRisks({ status: "active", registered_at: 1_000, expires_at: head + 300, grace_until: head + 400 }, o)[0], /it fails, or once the name's grace period ends it can pay whoever registers the name next\./);
  // Signed as the owner's address, expiry can't redirect it.
  assert.deepEqual(nameRisks({ status: "active", registered_at: 1_000, expires_at: head + 300 }, { ...o, signedAsName: false }), []);
  assert.match(nameRisks({ status: "grace", registered_at: 1_000, expires_at: head - 5 }, o)[0], /has expired/);
  const fresh = nameRisks({ status: "active", registered_at: head - 86_400 * 3, expires_at: head + 1_000_000 }, { ...o, signedAsName: false });
  assert.equal(fresh.length, 1);
  assert.match(fresh[0], /registered or changed hands in the last 30 days/);
  assert.deepEqual(nameRisks({ status: "active", registered_at: head - 86_400 * 31, expires_at: head + 1_000_000 }, o), []);
});

test("changedHandsLately: an accepted name or a re-registration, not every new name on a young chain", () => {
  const year = 31_557_600, grace = 2_592_000;
  const at = (registered_at, expires_at, head) => changedHandsLately({ registered_at, expires_at, registration_blocks: year, grace_blocks: grace }, head);
  // A young chain: registered at 120 for a year, nothing else could have held it.
  assert.equal(at(120, 120 + year, 500), false);
  // Renewed twice: still on the yearly grid.
  assert.equal(at(120, 120 + 3 * year, 500), false);
  // Accepted at 400 (accepting re-stamps registered_at, not the expiry): off the grid.
  assert.equal(at(400, 120 + year, 500), true);
  // Old enough for a lapse and a new registration.
  assert.equal(at(year + grace + 10, 2 * year + grace + 10, year + grace + 20), true);
  // Long ago: no.
  assert.equal(at(400, 120 + year, 400 + 31 * 86_400), false);
  // An older node without the registration length: any recent registration.
  assert.equal(changedHandsLately({ registered_at: 120, expires_at: 120 + year }, 500), true);
});
