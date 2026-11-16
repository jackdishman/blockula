// lib/explorer/jury.ts and what the wallet's /contribute page hangs on: the route, the pool's
// join_jury and leave_jury specs, the juror rules read from the chain's own answer, where an
// account stands, when the next Séance can open, and why joining or leaving isn't offered.
// Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { blockWhen, bondInput, bondToPost, displayToDrops, JOIN_JURY, joinBlockers, jurorStatus, juryRules, LEAVE_JURY, leaveBlockers, seanceTiming, soulPoints } from "./explorer/jury.ts";
import { isExplorerPath, to } from "./explorer/routes.ts";

const F = 10n ** 9n;
const RULES = { juror_min_soul: "5", juror_bond_display: "10 FANG", min_jury: 3, jury_size: 7 };
const codes = (list) => list.map((b) => b.code);

test("/contribute is a wide chain page, beside the pool's other pages", () => {
  assert.equal(to.contribute(), "/contribute");
  for (const path of ["/contribute", "/contribute/"]) assert.equal(isExplorerPath(path), true, path);
  assert.equal(isExplorerPath("/contributed"), false);
  assert.equal(isExplorerPath("/send"), false);
});

test("joining and leaving the jury are the pool's specs, each for owner keys only", () => {
  assert.equal(JOIN_JURY.kind, "join_jury");
  assert.equal(JOIN_JURY.ownerOnly, true);
  assert.deepEqual(JOIN_JURY.params.map((p) => [p.key, p.type]), [["bond", "fang"]]);
  assert.deepEqual(JOIN_JURY.build({ bond: (10n * F).toString() }, { height: 1, account: "boo1me" }), { type: "join_jury", bond: "10000000000" });

  assert.equal(LEAVE_JURY.kind, "leave_jury");
  assert.equal(LEAVE_JURY.id, undefined, "its id is its kind: /programs/pool?do=leave_jury opens it");
  assert.equal(LEAVE_JURY.ownerOnly, true, "a session key never leaves (or joins) a jury");
  assert.deepEqual(LEAVE_JURY.params, []);
  assert.deepEqual(LEAVE_JURY.build({}, { height: 1, account: "boo1me" }), { type: "leave_jury" });
  assert.match(LEAVE_JURY.describe({}), /take your bond back/);
  assert.match(LEAVE_JURY.blurb, /bond back/);
});

test("the pool program lists both, leaving right after joining", () => {
  // programs.ts reaches the app's config, so it's read as text here (as e2e/env.ts reads lib/config.ts).
  const src = readFileSync(new URL("./explorer/programs.ts", import.meta.url), "utf8");
  assert.match(src, /\n      JOIN_JURY,\n      LEAVE_JURY,\n/);
  assert.match(src, /calls: \[[^\]]*"join_jury", "leave_jury"/);
});

test("amounts and Soul are read as the node prints them", () => {
  assert.equal(displayToDrops("10 FANG"), 10n * F);
  assert.equal(displayToDrops("1,250.5 FANG"), 1250n * F + F / 2n);
  assert.equal(displayToDrops("0.000000001 FANG"), 1n);
  for (const bad of ["", "FANG", "ten FANG", null, undefined, "-1 FANG", ".5 FANG"]) assert.equal(displayToDrops(bad), null, String(bad));
  assert.equal(soulPoints("5"), 5000);
  assert.equal(soulPoints("2.5"), 2500);
  assert.equal(soulPoints("0.125"), 125);
  for (const bad of ["", "five", "1e3", "-1", undefined]) assert.equal(soulPoints(bad), null, String(bad));
});

test("the juror minimum and bond come from the chain's rules, and nothing is filled in when it doesn't say", () => {
  assert.deepEqual(juryRules(RULES), { minSoul: "5", minSoulPoints: 5000, bondDisplay: "10 FANG", bond: 10n * F, bondInput: "10", minJury: 3 });
  // Another chain's rules are shown as that chain's.
  assert.deepEqual(juryRules({ juror_min_soul: "2.5", juror_bond_display: "1,250.5 FANG", min_jury: 5 }), { minSoul: "2.5", minSoulPoints: 2500, bondDisplay: "1,250.5 FANG", bond: 1250n * F + F / 2n, bondInput: "1250.5", minJury: 5 });
  for (const none of [{}, null, undefined]) assert.deepEqual(juryRules(none), { minSoul: null, minSoulPoints: null, bondDisplay: null, bond: null, bondInput: "", minJury: null });
});

test("a benched juror tops the bond back up; anyone else posts all of it", () => {
  const rules = juryRules(RULES);
  assert.equal(bondToPost(rules, null), 10n * F);
  assert.equal(bondToPost(rules, 0n), 10n * F);
  assert.equal(bondToPost(rules, 9n * F), F, "a no-show cut it by 1");
  assert.equal(bondToPost(rules, 10n * F), 10n * F, "nothing is missing: the field still starts from the rule");
  assert.equal(bondToPost(juryRules({}), 9n * F), null);
  assert.equal(bondInput(F / 2n), "0.5");
  assert.equal(bondInput(null), "");
});

test("where an account stands in the juror pool", () => {
  assert.equal(jurorStatus(null), "none");
  assert.equal(jurorStatus(undefined), "none");
  assert.equal(jurorStatus({ active: true }), "enrolled");
  assert.equal(jurorStatus({ active: false }), "benched");
});

test("the next Séance is a block and about when it's due, in the reader's clock", () => {
  const now = Date.UTC(2026, 9, 31, 20, 0, 0);
  assert.equal(blockWhen(1_900, 1_000, 1000, now, "UTC"), "block #1,900 (about 20:15, in 15 min)");
  assert.equal(blockWhen(1_000 + 5 * 3600, 1_000, 1000, now, "UTC"), "block #19,000 (about 01:00 tomorrow, in 5 h)");
  assert.equal(blockWhen(1_000 + 3 * 86_400, 1_000, 1000, now, "UTC"), "block #260,200 (about Nov 3, in 3 days)");
  // The chain's own block time, never a second assumed.
  assert.equal(blockWhen(1_900, 1_000, 2000, now, "UTC"), "block #1,900 (about 20:30, in 30 min)");
  for (const past of [1_000, 999]) assert.equal(blockWhen(past, 1_000, 1000, now, "UTC"), `block #${past.toLocaleString("en-US")} (due now)`);

  assert.deepEqual(seanceTiming({ next_seance_at: 1_900, open_seance: null }, 1_000, 1000, now, "UTC"), { open: null, next: "block #1,900 (about 20:15, in 15 min)" });
  const open = { id: 4, phase: "sealing", commit_until: 1_060, reveal_until: 1_120 };
  assert.deepEqual(seanceTiming({ next_seance_at: 1_900, open_seance: open }, 1_000, 1000, now, "UTC").open, { id: 4, text: "Séance #4 is open: jurors seal their votes until block #1,060 (about 20:01, in 60s)." });
  assert.match(seanceTiming({ next_seance_at: 1_900, open_seance: { ...open, phase: "revealing" } }, 1_070, 1000, now, "UTC").open.text, /^Séance #4 is open: jurors reveal their votes until block #1,120 /);
});

test("joining is explained away when Soul, balance, the key or the account's standing won't do", () => {
  const rules = juryRules(RULES);
  const ok = { rules, status: "none", soul: { points: 9000, display: "9" }, transferable: 50n * F };
  assert.deepEqual(joinBlockers(ok), []);
  assert.deepEqual(joinBlockers({ ...ok, soul: { points: 5000, display: "5" } }), [], "exactly the minimum is enough");

  const low = joinBlockers({ ...ok, soul: { points: 4999, display: "4.999" } });
  assert.deepEqual(codes(low), ["low_soul"]);
  assert.match(low[0].text, /at least 5 Soul, and this account has 4\.999/);

  // The fee comes on top of the bond, so exactly the bond isn't enough.
  const broke = joinBlockers({ ...ok, transferable: 10n * F, unit: "BAT" });
  assert.deepEqual(codes(broke), ["low_balance"]);
  assert.match(broke[0].text, /The bond is 10 BAT and this account can move 10 BAT now: not enough for the bond and the fee\./);
  assert.deepEqual(joinBlockers({ ...ok, transferable: 10n * F + 1n }), []);
  assert.deepEqual(joinBlockers({ ...ok, transferable: null }), [], "not read yet: nothing is claimed");

  const agent = joinBlockers({ ...ok, agentKeyLabel: "claude" });
  assert.deepEqual(codes(agent), ["agent_key"]);
  assert.match(agent[0].text, /agent key \(“claude”\).*owner device/);

  assert.deepEqual(codes(joinBlockers({ ...ok, status: "enrolled", transferable: 0n })), ["enrolled"], "an enrolled juror owes no second bond");
  assert.deepEqual(codes(joinBlockers({ ...ok, sybil: true })), ["sybil"]);
  // A benched juror coming back owes only what's missing.
  assert.deepEqual(joinBlockers({ ...ok, status: "benched", post: F, transferable: 2n * F }), []);
  assert.deepEqual(codes(joinBlockers({ ...ok, status: "benched", post: F, transferable: F })), ["low_balance"]);
  // Everything at once, the key first.
  assert.deepEqual(codes(joinBlockers({ ...ok, agentKeyLabel: "", soul: { points: 0, display: "0" }, transferable: 0n })), ["agent_key", "low_soul", "low_balance"]);
  // A node that didn't give the rules: nothing is assumed, and the chain decides.
  assert.deepEqual(joinBlockers({ ...ok, rules: juryRules({}), soul: { points: 0, display: "0" }, transferable: 0n }), []);
});

test("leaving is for an enrolled juror with an owner key who isn't seated on the open Séance", () => {
  assert.deepEqual(leaveBlockers({ status: "enrolled" }), []);
  assert.deepEqual(leaveBlockers({ status: "benched" }), [], "a benched juror takes back what's left");
  assert.deepEqual(codes(leaveBlockers({ status: "none" })), ["not_enrolled"]);
  assert.deepEqual(codes(leaveBlockers({ status: "enrolled", agentKeyLabel: "claude" })), ["agent_key"]);
  const seated = leaveBlockers({ status: "enrolled", seated: { id: 2, reveal_until: 1_180 } });
  assert.deepEqual(codes(seated), ["seated"]);
  assert.equal(seated[0].text, "You're on the jury of Séance #2. Leave after it closes (block #1,180).");
});
