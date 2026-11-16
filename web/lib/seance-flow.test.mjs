// lib/explorer/seance-flow.ts, the Séance and Soul diagram on /contribute: the steps and their
// numbers come from the chain's rules (`/api/contributions`), a missing rule leaves its number out
// and keeps the step, and the marks say where the chain is. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { flowMarks, seanceFlow, soulFacts, span } from "./explorer/seance-flow.ts";

// The demo chain's rules (2 s blocks), as node/src/api.rs `contributions` prints them.
const DEMO = {
  seance_blocks: 302400, commit_blocks: 201600, reveal_blocks: 100800, claim_maturity_blocks: 1296000, claim_ttl_blocks: 302400, vesting_blocks: 3888000,
  vouch_lock_blocks: 3888000, soul_half_life_blocks: 15768000, claim_bond_display: "1 FANG", vouch_bond_display: "10 FANG", juror_bond_display: "10 FANG",
  vouch_min_soul: "10", juror_min_soul: "5", jury_size: 7, min_jury: 3, pairs_per_juror: 24, max_claims_per_seance: 64,
  pool_share_per_seance: "25%", juror_share: "10%", min_score: "15%", full_budget_people: 16, min_people: 2, claim_rank_divisor: 4,
};
const KEYS = ["merge", "claim", "maturity", "opens", "commit", "reveal", "payout", "vest"];
const byKey = (steps) => Object.fromEntries(steps.map((s) => [s.key, s]));

test("a length of blocks reads in the unit that fits it", () => {
  assert.equal(span(1200, 2000), "40 min");
  assert.equal(span(100800, 2000), "2.3 days");
  assert.equal(span(201600, 2000), "4.7 days");
  assert.equal(span(43200, 2000), "1 day");
  assert.equal(span(302400, 2000), "7 days");
  assert.equal(span(1296000, 2000), "30 days");
  assert.equal(span(3888000, 2000), "90 days");
  assert.equal(span(15768000, 2000), "1 year");
  assert.equal(span(23652000, 2000), "1.5 years");
});

test("the eight steps carry the demo chain's numbers", () => {
  const steps = seanceFlow(DEMO, 2000);
  assert.deepEqual(steps.map((s) => s.key), KEYS);
  const s = byKey(steps);
  assert.equal(s.merge.value, null);
  assert.equal(s.claim.value, "1 FANG bond");
  assert.equal(s.maturity.value, "30 days");
  assert.equal(s.opens.value, "every 7 days");
  assert.equal(s.opens.note, "Jury of up to 7, at least 3, seated by the randomness beacon. Needs matured claims from 2 or more people.");
  assert.equal(s.commit.value, "4.7 days");
  assert.equal(s.reveal.value, "2.3 days");
  assert.equal(s.payout.value, "up to 25% of the pool");
  assert.equal(s.payout.note, "Jurors share 10%. Claims scoring under 15% get nothing.");
  assert.equal(s.vest.value, "90 days");
});

test("the numbers are the chain's: other rules, other numbers", () => {
  const s = byKey(seanceFlow({ ...DEMO, claim_bond_display: "2.5 BOO", claim_maturity_blocks: 0, seance_blocks: 90, commit_blocks: 60, reveal_blocks: 30, jury_size: 9, min_jury: 5, pool_share_per_seance: "40%", juror_share: "5%", min_score: "20%", vesting_blocks: 600 }, 1000));
  assert.equal(s.claim.value, "2.5 BOO bond");
  assert.equal(s.maturity.value, "no wait");
  assert.equal(s.opens.value, "every 2 min");
  assert.match(s.opens.note, /^Jury of up to 9, at least 5,/);
  assert.equal(s.commit.value, "60s");
  assert.equal(s.reveal.value, "30s");
  assert.equal(s.payout.value, "up to 40% of the pool");
  assert.equal(s.payout.note, "Jurors share 5%. Claims scoring under 20% get nothing.");
  assert.equal(s.vest.value, "10 min");
});

test("a rule the node didn't send leaves its number out and keeps the step", () => {
  for (const rules of [undefined, null, {}, { claim_bond_display: "", seance_blocks: "soon", jury_size: -1, pool_share_per_seance: "a quarter", min_score: 15, vesting_blocks: 1.5 }]) {
    const steps = seanceFlow(rules, 2000);
    assert.deepEqual(steps.map((s) => s.key), KEYS);
    assert.deepEqual(steps.map((s) => s.value), KEYS.map(() => null));
    for (const s of steps) assert.doesNotMatch(`${s.label} ${s.note}`, /\d|undefined|null|NaN/, s.key);
    assert.deepEqual(soulFacts(rules, 2000).map((f) => f.text), ["When a jury accepts your claim, by judging in line with the outcome, or by a grant.", "Halves over time.", "Soul and a bond."]);
  }
  const s = byKey(seanceFlow({ jury_size: 7, juror_share: "10%" }));
  assert.equal(s.opens.note, "Jury of up to 7, seated by the randomness beacon.");
  assert.equal(s.payout.note, "Jurors share 10%. Low-scoring claims get nothing.");
  assert.equal(byKey(seanceFlow({ min_jury: 3 })).opens.note, "Jury of at least 3, seated by the randomness beacon.");
});

test("Soul: how it's earned, its half-life and what a juror needs, from the rules", () => {
  assert.deepEqual(soulFacts(DEMO, 2000).map((f) => [f.key, f.text]), [
    ["earned", "When a jury accepts your claim, by judging in line with the outcome, or by a grant."],
    ["halves", "Halves about every 1 year."],
    ["jurors", "5 Soul and a 10 FANG bond."],
  ]);
  assert.equal(soulFacts({ juror_min_soul: "7.5" })[2].text, "7.5 Soul and a bond.");
  assert.equal(soulFacts({ juror_bond_display: "12 FANG", soul_half_life_blocks: 86400 * 30 })[2].text, "Soul and a 12 FANG bond.");
  assert.equal(soulFacts({ soul_half_life_blocks: 86400 * 30 })[1].text, "Halves about every 30 days.");
});

const NOW = Date.UTC(2026, 10, 16, 12);

test("with no Séance open, the next opening is marked with its block and about when", () => {
  // 27 days of 2 s blocks ahead of the head.
  const marks = flowMarks({ next_seance_at: 5_000_000 + 27 * 43_200, open_seance: null }, 5_000_000, 2000, NOW, "UTC");
  assert.deepEqual(marks, [{ step: "opens", kind: "next", text: "Next Séance can open about Dec 13 (block #6,166,400)" }]);
  assert.equal(flowMarks({ next_seance_at: 100, open_seance: null }, 100)[0].text, "Next Séance can open now (block #100)");
  assert.deepEqual(flowMarks({ open_seance: null }, 100), [], "no height from the node: no mark, and no date of this app's own");
});

test("an open Séance is marked on its phase with the time left, and the opening after it", () => {
  const open = { id: 4, phase: "sealing", commit_until: 1000 + 86_400, reveal_until: 1000 + 129_600 };
  assert.deepEqual(flowMarks({ next_seance_at: 1000 + 302_400, open_seance: open }, 1000, 2000, NOW, "UTC"), [
    { step: "commit", kind: "now", text: "Séance #4 is sealing votes: 2 days left" },
    { step: "opens", kind: "next", text: "The one after can open about Nov 23 (block #303,400)" },
  ]);
  const revealing = flowMarks({ next_seance_at: 400_000, open_seance: { ...open, phase: "revealing" } }, 1000 + 129_600 - 1800, 2000, NOW, "UTC");
  assert.deepEqual(revealing[0], { step: "reveal", kind: "now", text: "Séance #4 is revealing votes: 60 min left" });
  assert.match(flowMarks({ open_seance: { ...open, phase: "revealing" } }, 1000 + 129_600)[0].text, /closing now$/);
});

test("the diagram's copy is plain: no contracts, no em dashes, no talk of value, and no numbers of its own", () => {
  const src = readFileSync(new URL("./explorer/seance-flow.ts", import.meta.url), "utf8") + readFileSync(new URL("../components/explorer/SeanceFlow.tsx", import.meta.url), "utf8");
  assert.doesNotMatch(src, /contract|—|worth|valuable|price/i);
  const text = [...seanceFlow({}), ...soulFacts({})].map((x) => `${x.label} ${x.note ?? x.text}`).join(" ");
  assert.doesNotMatch(text, /\d/);
});
