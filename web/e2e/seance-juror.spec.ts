// A juror's part of a Séance page (components/explorer/JurorBallot.tsx, issue #159), against the mock node's
// Séance with the wallet seated (e2e/explorer-world.ts `seatJuror`): the juror sees their assigned pairs, each
// claim with its repo, PR and a link to the PR on GitHub (a repo named with markup stays text); seals a vote on
// every pair and a spam flag after the passkey check, binding the pairs shown, with the salt kept in this
// browser; then reveals with one button once the Séance reveals. Every tx's summary shows as returned. A juror
// whose salt isn't in this browser, and an agent key, are told what to do instead. All under the production
// build's strict CSP.
import { seanceCommitment } from "@blockula/sdk";
import type { Page } from "@playwright/test";
import { ADDR, HOSTILE_REPO, seatJuror } from "./explorer-world";
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";

const F = 10n ** 9n;

/** Errors the page logs, and node reads that failed, from now on. */
function watchErrors(page: Page) {
  const errors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error" && !/^Failed to load resource/.test(m.text())) errors.push(`console: ${m.text()}`);
  });
  page.on("pageerror", (e) => errors.push(`pageerror: ${e.message}`));
  page.on("response", (r) => {
    // Multisig reads answer 404 for an account that isn't shared: that's how the page learns it.
    if (r.status() >= 400 && !/\/api\/multisig\//.test(r.url())) errors.push(`${r.status()} ${r.url()}`);
  });
  return errors;
}

/** Nothing a stranger wrote ran, and the page broke none of the CSP's rules. */
async function clean(page: Page, errors: string[]) {
  expect(await cspViolations(page)).toEqual([]);
  expect(errors).toEqual([]);
  expect(await page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned)).toBeUndefined();
  await expect(page.locator("main img[src='x']")).toHaveCount(0);
  await expect(page.locator('a[href^="javascript:" i]')).toHaveCount(0);
}

/** The ballots this browser keeps, by storage key. */
const ballots = (page: Page) =>
  page.evaluate(() => Object.fromEntries(Object.entries({ ...localStorage }).filter(([k]) => k.startsWith("blockula.ballot:")))) as Promise<Record<string, string>>;

test.describe("judging a Séance from the wallet", () => {
  let errors: string[];
  test.beforeEach(async ({ page }) => {
    errors = watchErrors(page);
    await watchCsp(page);
  });

  test("a seated juror sees their pairs, seals a vote on each with a passkey, and reveals it with one button", async ({ page, node }) => {
    const { account: me } = await createWallet(page);
    const world = seatJuror(node, me);
    await page.goto("/seance/3");
    const ballot = page.getByTestId("juror-ballot");
    await ballot.getByRole("button", { name: "Unlock to judge" }).click();

    // The assigned pairs, in order, each claim by repo and PR; a link to GitHub only for a plain owner/name.
    const pairs = ballot.getByTestId("ballot-pair");
    await expect(pairs).toHaveCount(3);
    await expect(pairs.nth(0)).toContainText("jackdishman/blockula#151");
    await expect(pairs.nth(0)).toContainText("jackdishman/blockula#152");
    await expect(pairs.nth(1)).toContainText(`${HOSTILE_REPO}#9`);
    const links = ballot.getByTestId("pr-link");
    await expect(links).toHaveCount(4);
    expect(new Set(await links.evaluateAll((as) => as.map((a) => (a as HTMLAnchorElement).href)))).toEqual(new Set(["https://github.com/jackdishman/blockula/pull/151", "https://github.com/jackdishman/blockula/pull/152"]));
    await expect(links.first()).toHaveAttribute("rel", /noopener/);
    await expect(ballot.getByTestId("salt-warning")).toContainText("clear this site's data");
    await expect(ballot.getByTestId("salt-warning")).toContainText("no-show penalty");

    // Every pair needs a vote before anything is signed.
    await ballot.getByRole("button", { name: "Seal my votes on 3 pairs" }).click();
    await expect(ballot.getByRole("alert")).toContainText("3 still need a vote");
    expect(node.submitted).toHaveLength(0);

    await ballot.getByRole("radiogroup", { name: "Pair 1" }).getByLabel("A mattered more").check();
    await ballot.getByRole("radiogroup", { name: "Pair 2" }).getByLabel("About the same").check();
    await ballot.getByRole("radiogroup", { name: "Pair 3" }).getByLabel("B mattered more").check();
    await ballot.locator("label", { hasText: "#9 is spam" }).getByRole("checkbox").check();
    await ballot.getByRole("button", { name: "Seal my votes on 3 pairs" }).click();
    await expect(ballot.getByTestId("ballot-done")).toContainText("sealed their votes in Séance #3");
    expect(node.submitted.map((t) => t.action.type)).toEqual(["commit_votes"]);
    // Once sent, there's no second seal to make from this page: a new salt mustn't stand in for the one on its way.
    await expect(ballot.getByTestId("sealed-here")).toBeVisible();
    await expect(ballot.getByRole("button", { name: /^Seal/ })).toHaveCount(0);

    // The salt stayed in this browser, and what the chain holds is the commitment to exactly this ballot.
    const kept = Object.entries(await ballots(page));
    expect(kept).toHaveLength(1);
    expect(kept[0][0]).toContain(`:${me}:3`);
    const [b] = (JSON.parse(kept[0][1]) as { ballots: { votes: string; spam: number[]; salt: string }[] }).ballots;
    expect(b).toMatchObject({ votes: "a=b", spam: [12] });
    expect(world.seat().committed).toBe(true);
    expect(world.seat().commitment).toBe(seanceCommitment(3, me, "a=b", [12], b.salt));
    await page.reload();
    await ballot.getByRole("button", { name: "Unlock to judge" }).click();
    await expect(ballot.getByTestId("sealed-here")).toBeVisible();

    // The Séance reveals: one button opens the seal.
    world.reveal();
    await page.reload();
    await ballot.getByRole("button", { name: "Unlock to judge" }).click();
    await ballot.getByRole("button", { name: "Reveal my votes" }).click();
    await expect(ballot.getByTestId("ballot-done")).toContainText("revealed their votes in Séance #3");
    expect(world.seat()).toMatchObject({ revealed: true, votes: "a=b", spam: [12] });
    expect(node.submitted.map((t) => t.action.type)).toEqual(["commit_votes", "reveal_votes"]);
    await expect(ballot.getByText("Your votes are revealed")).toBeVisible({ timeout: 15_000 });
    await clean(page, errors);
  });

  test("a juror who sealed elsewhere is told to reveal from there, and an agent key can't judge", async ({ page, node }) => {
    const { account: me, keyId } = await createWallet(page);
    const world = seatJuror(node, me);
    // Sealed from another device: the chain holds a commitment this browser has no salt for.
    Object.assign(world.seat(), { committed: true, commitment: "ab".repeat(32) });
    world.reveal();
    await page.goto("/seance/3");
    const ballot = page.getByTestId("juror-ballot");
    await ballot.getByRole("button", { name: "Unlock to judge" }).click();
    await expect(ballot.getByTestId("salt-elsewhere")).toContainText("Reveal from the device that sealed it");
    await expect(ballot.getByTestId("salt-elsewhere")).toContainText("blockula contrib reveal");
    await expect(ballot.getByRole("button", { name: "Reveal my votes" })).toHaveCount(0);

    // This browser's key made an agent's: it's told so, and offered nothing to sign.
    node.acct(me).keys = new Map([
      ["0e".repeat(32), { label: "Vlad's laptop", role: { type: "owner" as const }, added_at: 1 }],
      [keyId, { label: "claude", role: { type: "session" as const, limit: (10n * F).toString(), period_blocks: 86_400, expires_at: 2_000_000 }, added_at: 2 }],
    ]);
    Object.assign(world.seance, { phase: "sealing" });
    Object.assign(world.seat(), { committed: false, commitment: null });
    await page.reload();
    await ballot.getByRole("button", { name: "Unlock to judge" }).click();
    await expect(ballot.getByTestId("juror-agent-key")).toContainText("agent key (“claude”)");
    await expect(ballot.getByRole("button", { name: /^Seal/ })).toHaveCount(0);
    expect(node.submitted).toHaveLength(0);
    await clean(page, errors);
  });

  test("someone not on the jury sees the Séance as before, with no ballot", async ({ page, node }) => {
    await createWallet(page);
    seatJuror(node, ADDR.renfield);
    await page.goto("/seance/3");
    await expect(page.getByText(/^Sealing votes until block/)).toBeVisible();
    await expect(page.getByTestId("juror-ballot")).toHaveCount(0);
    await clean(page, errors);
  });
});
