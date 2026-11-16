// The Séance and Soul diagram on /contribute (components/explorer/SeanceFlow.tsx), against the mock
// node's pool (e2e/explorer-world.ts) with rules set per test: every step is there with the chain's
// own numbers, the open Séance's phase or the next opening is marked, the steps run across a wide
// screen and down a 390 x 844 phone, and nothing scrolls sideways. Under the production build's CSP.
import type { Page } from "@playwright/test";
import { seedWorld } from "./explorer-world";
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";
import type { MockNode } from "./mock-node";

const PHONE = { width: 390, height: 844 };
const STEPS = ["Merged PR", "Claim", "Maturity", "Séance opens", "Sealed votes", "Reveal", "Payout", "Vesting"];
const DAY = 86_400; // The mock chain's blocks are a second apart.

type Pool = { rules: Record<string, string | number>; next_seance_at: number; open_seance: { id: number; phase: string; commit_until: number; reveal_until: number } | null };

async function open(page: Page, node: MockNode, change: (pool: Pool, height: number) => void) {
  await watchCsp(page);
  const { account: me } = await createWallet(page);
  seedWorld(node, me);
  const pool = node.contributions as Pool;
  change(pool, node.height);
  await page.goto("/contribute");
  const flow = page.getByTestId("seance-flow");
  await expect(flow).toBeVisible();
  return { flow, pool };
}

/** Each step's top-left corner, in order. */
const corners = (page: Page) => page.locator('[data-testid="seance-flow"] ol.sflow > li').evaluateAll((els) => els.map((el) => ({ x: Math.round(el.getBoundingClientRect().x), y: Math.round(el.getBoundingClientRect().y), right: Math.round(el.getBoundingClientRect().right) })));

test("the diagram shows each step with this chain's numbers and marks the open Séance's phase", async ({ page, node }) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  const { flow, pool } = await open(page, node, (p) => {
    // Rules no wallet would guess.
    Object.assign(p.rules, { claim_bond_display: "2.5 FANG", claim_maturity_blocks: 12 * DAY, seance_blocks: 3 * DAY, commit_blocks: 2 * DAY, reveal_blocks: DAY, jury_size: 9, min_jury: 4, pool_share_per_seance: "40%", juror_share: "5%", min_score: "20%", vesting_blocks: 45 * DAY, soul_half_life_blocks: 730 * DAY, juror_min_soul: "7", juror_bond_display: "12.5 FANG" });
  });
  await expect(flow.getByRole("figure", { name: "from a merged PR to a reward" })).toBeVisible();
  await expect(flow.locator("ol.sflow > li .sflow-label")).toHaveText(STEPS);
  await expect(flow.getByTestId("flow-claim")).toHaveText("2.5 FANG bond");
  await expect(flow.getByTestId("flow-maturity")).toHaveText("12 days");
  await expect(flow.getByTestId("flow-opens")).toHaveText("every 3 days");
  await expect(flow.locator('[data-step="opens"]')).toContainText("Jury of up to 9, at least 4, seated by the randomness beacon.");
  await expect(flow.getByTestId("flow-commit")).toHaveText("2 days");
  await expect(flow.getByTestId("flow-reveal")).toHaveText("1 day");
  await expect(flow.getByTestId("flow-payout")).toHaveText("up to 40% of the pool");
  await expect(flow.locator('[data-step="payout"]')).toContainText("Jurors share 5%. Claims scoring under 20% get nothing.");
  await expect(flow.getByTestId("flow-vest")).toHaveText("45 days");

  const soul = flow.getByTestId("soul-panel");
  await expect(soul.locator('[data-fact="earned"]')).toContainText("When a jury accepts your claim, by judging in line with the outcome, or by a grant.");
  await expect(soul.locator('[data-fact="halves"]')).toContainText("Halves about every 2 years.");
  await expect(soul.locator('[data-fact="jurors"]')).toContainText("7 Soul and a 12.5 FANG bond.");

  // The open Séance is sealing: marked on that step, and the opening after it on "Séance opens".
  await expect(flow.locator('[data-step="commit"]')).toHaveClass("now");
  await expect(flow.locator('[data-step="commit"]').getByTestId("flow-now")).toHaveText(/^Séance #2 is sealing votes: \d+s left$/);
  await expect(flow.locator('[data-step="opens"]').getByTestId("flow-next")).toHaveText(new RegExp(`^The one after can open about \\d\\d:\\d\\d( tomorrow)? \\(block #${pool.next_seance_at}\\)$`));

  // The prose it replaced is gone from the jury card.
  await expect(page.getByTestId("jury-how")).toHaveCount(0);
  await expect(page.getByTestId("jury-card")).not.toContainText("Jurors seal their votes");
  await expect(page.getByText("in the pool").first()).toBeVisible();
  await expect(page.getByRole("heading", { name: "How it pays" })).toHaveCount(0);

  // Wide: the first four steps share a row, the next four the row under it.
  const at = await corners(page);
  expect(new Set(at.slice(0, 4).map((c) => c.y)).size).toBe(1);
  expect(new Set(at.slice(4).map((c) => c.y)).size).toBe(1);
  expect(at[4].y).toBeGreaterThan(at[0].y);
  expect(at[1].x).toBeGreaterThan(at[0].x);
  expect(await cspViolations(page)).toEqual([]);
});

test("on a 390 x 844 phone the steps stack, the next Séance is marked with its date, and nothing scrolls sideways", async ({ page, node }) => {
  await page.setViewportSize(PHONE);
  const { flow, pool } = await open(page, node, (p, height) => {
    // The demo chain's rules (its 2 s blocks, in this chain's 1 s ones), with no Séance open.
    Object.assign(p.rules, { claim_bond_display: "1 FANG", claim_maturity_blocks: 30 * DAY, seance_blocks: 7 * DAY, commit_blocks: 403_200, reveal_blocks: 201_600, vesting_blocks: 90 * DAY, soul_half_life_blocks: 365 * DAY });
    p.open_seance = null;
    p.next_seance_at = height + 27 * DAY;
  });
  await expect(flow.locator("ol.sflow > li .sflow-label")).toHaveText(STEPS);
  await expect(flow.locator("ol.sflow > li .sflow-value")).toHaveText(["1 FANG bond", "30 days", "every 7 days", "4.7 days", "2.3 days", "up to 25% of the pool", "90 days"]);
  await expect(flow.getByTestId("soul-panel")).toContainText("Halves about every 1 year.");
  await expect(flow.getByTestId("soul-panel")).toContainText("5 Soul and a 10 FANG bond.");

  // No Séance open: the next opening, as about a date and its block, on "Séance opens" alone.
  await expect(flow.getByTestId("flow-now")).toHaveCount(0);
  const next = flow.locator('[data-step="opens"]').getByTestId("flow-next");
  await expect(next).toHaveText(/^Next Séance can open about [A-Z][a-z]{2} \d{1,2}(, \d{4})? \(block #[\d,]+\)$/);
  await expect(next).toContainText(`block #${pool.next_seance_at.toLocaleString("en-US")}`);
  await expect(flow.locator('[data-step="opens"]')).toHaveClass("next");

  // Stacked: one column, each step under the one before, all inside the screen's width.
  const at = await corners(page);
  expect(at).toHaveLength(STEPS.length);
  for (let i = 1; i < at.length; i++) {
    expect(at[i].x, `step ${i + 1} starts in the same column`).toBe(at[0].x);
    expect(at[i].y, `step ${i + 1} is under step ${i}`).toBeGreaterThan(at[i - 1].y);
  }
  for (const c of at) expect(c.right).toBeLessThanOrEqual(PHONE.width);
  expect(await page.evaluate(() => document.documentElement.scrollWidth), "the page is no wider than the phone").toBeLessThanOrEqual(PHONE.width);

  expect(await cspViolations(page)).toEqual([]);
});
