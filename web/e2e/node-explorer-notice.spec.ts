// The node's built-in explorer (node/src/explorer.html), as explorer.spec.ts serves it: its footer
// says the chain's FANG has no value (test chains: test FANG; a chain someone launched: its own),
// its copy makes no value or reward promises, and its Hunts and contribution pool copy says who
// pays and what a pool without repositories pays.
import { readFileSync } from "node:fs";
import path from "node:path";
import { expect, test, type Page } from "@playwright/test";

const ORIGIN = "https://node-explorer.blockula.test";
const HTML = readFileSync(path.join(__dirname, "../../node/src/explorer.html"), "utf8");
// A chain someone launched with `blockula summon` (docs/LAUNCH.md).
const OWN_CHAIN = "mychain";
const BURNED = "Fees destroyed forever. The base fee prices block space so spam costs the spammer; burning it takes that FANG out of the supply. It isn't a mechanism to manage FANG's price.";
const HUNT_TIP = "A bounty on a GitHub issue. Its FANG sits in escrow until the creator awards it. The chain doesn't check the pull request: the creator decides who's paid. Once it expires the creator can cancel it, refunding every funder exactly; before that only a governance refund can.";
const CREATOR = { address: "boo1creatorqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq", name: "maker.boo", short: "boo1creato…qqqq" };

/** A node's answers. `repos`: the contribution pool's repositories (none: it pays nothing). */
function api(p: string, chainId: string, repos: { repo: string; weight: number }[]): unknown {
  if (p === "/status") return { chain_id: chainId, height: 1000, block_time_ms: 1000, epoch: 3, active_validators: 4, mempool: 0, supply_display: "1000000000 FANG", burned_display: "12 FANG", accounts: 5, names: 1, head_age_ms: 500, history_from: 1 };
  if (p.startsWith("/blocks")) return [];
  if (p.startsWith("/hunts")) return { count: 0, hunts: [], open_bounties_display: "0 FANG" };
  if (p === "/hunt/7") return { id: 7, amount_display: "100 FANG", repo: "jackdishman/blockula", issue: 12, issue_url: "https://github.com/jackdishman/blockula/issues/12", title: "Fix the thing", status: "open", creator: CREATOR, created_at: 900, expires_at: 5000, funders: [{ account: CREATOR, amount_display: "100 FANG" }] };
  if (p === "/contributions") return { pool: { balance_display: "400 FANG", next_budget_display: "100 FANG" }, repos, jurors: { active: 0, pool: 0 }, open_seance: null, recent_seances: [], next_seance_at: 10, rules: { claim_bond_display: "1 FANG", claim_maturity_blocks: 100, jury_size: 7, juror_share: "10%", pool_share_per_seance: "25%", full_budget_people: 16, vesting_blocks: 1000, min_jury: 3 } };
  if (p.startsWith("/souls")) return { souls: [] };
  if (p.startsWith("/claims")) return { claims: [] };
  return null;
}

/** Serves the explorer with a node on `chainId`, then opens `hash`. */
async function open(page: Page, chainId: string, hash = "", repos: { repo: string; weight: number }[] = []) {
  await page.route(`${ORIGIN}/`, (r) => r.fulfill({ contentType: "text/html", body: HTML }));
  await page.route(`${ORIGIN}/api/**`, (r) => {
    const u = new URL(r.request().url());
    if (u.pathname === "/api/health") return r.fulfill({ contentType: "application/json", body: JSON.stringify({ ok: true }) });
    const body = api(u.pathname.replace(/^\/api/, "") + u.search, chainId, repos);
    return body === null ? r.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "not found" }) }) : r.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  });
  await page.route(/fonts\.(googleapis|gstatic)\.com/, (r) => r.abort());
  await page.route(/\/haunt\.js$/, (r) => r.fulfill({ contentType: "text/javascript", body: "" }));
  await page.goto(`${ORIGIN}/${hash}`);
}

test("a chain someone launched says it isn't audited and its FANG has no value", async ({ page }) => {
  await open(page, OWN_CHAIN);
  // explorer.spec.ts holds the imprint itself to this.
  await expect(page.locator("#imprint")).toHaveText("Post-quantum signatures · ML-DSA-65 · Not audited");
  await expect(page.locator("#notice")).toHaveText("This chain's FANG has no value");
  await expect(page.locator("footer")).not.toContainText("test chains only");
  await expect(page.locator("footer").getByRole("link")).toHaveCount(0);
});

for (const chain of ["blockula-testnet-2", "blockula-dev"]) {
  test(`a test chain's footer (${chain}) says it isn't audited and test FANG has no value`, async ({ page }) => {
    await open(page, chain);
    // Wait for the page to have read the status (the home view shows the chain's height).
    await expect(page.locator("#app")).toContainText("#1000");
    await expect(page.locator("#imprint")).toHaveText("Post-quantum signatures · ML-DSA-65 · Not audited, test chains only");
    await expect(page.locator("#notice")).toHaveText("Test FANG has no value");
    await expect(page.locator("footer").getByRole("link")).toHaveCount(0);
  });
}

test("a node that says it's on blockula-1 gets no mainnet footer", async ({ page }) => {
  await open(page, "blockula-1");
  await expect(page.locator("#notice")).toHaveText("This chain's FANG has no value");
  await expect(page.locator("footer")).not.toContainText(/mainnet|Terms|Risks/i);
});

test("the glossary's 'burned' makes no price claim, and a Hunt says who pays it", async ({ page }) => {
  await open(page, OWN_CHAIN);
  await expect(page.locator("#app .term", { hasText: "burned" }).first()).toHaveAttribute("data-tip", BURNED);
  await expect(page.locator("body")).not.toContainText("ties the token");
  await page.goto(`${ORIGIN}/#/hunts`);
  const app = page.locator("#app");
  await expect(app).toContainText("Hunts are bounties on GitHub issues, paid by whoever created and funded them. Fix the issue and get the pull request merged, and the creator can award you the bounty (or your Familiar's sponsor). The chain doesn't check the pull request, and anyone can add FANG to an open Hunt. Before you start, check who created a Hunt and what its rules are. Open bounties right now: 0 FANG.");
  await expect(app).toContainText("FANG paid here has no value.");
  await expect(app).not.toContainText(/taxable|promise|HUNTS-PROGRAM/);
  await expect(app.locator(".term", { hasText: "Hunts" }).first()).toHaveAttribute("data-tip", HUNT_TIP);
  await expect(app).not.toContainText("is awarded to your account");
  await page.goto(`${ORIGIN}/#/hunt/7`);
  await expect(app.locator("dt", { hasText: "Paid by" })).toBeVisible();
  await expect(app).toContainText("Paid byits creator and funders. The chain doesn't check the pull request.");
});

test("a pool with no repositories says it pays nothing, and keeps how it would work folded", async ({ page }) => {
  await open(page, OWN_CHAIN, "#/contribute");
  const app = page.locator("#app");
  await expect(app.locator("h1")).toHaveText("The contribution pool");
  await expect(app).not.toContainText("Get paid for useful work");
  await expect(app).toContainText("This chain's contribution pool has no repositories yet, so it pays nothing. It keeps filling from issuance and the endowment. Only governance can add a repository.");
  await expect(app.locator(".stat", { hasText: "next Séance pays up to" })).toContainText("nothing yetnext Séance pays up tono repositories");
  await expect(app.locator(".term", { hasText: "contribution pool" }).first()).toHaveAttribute("data-tip", /It pays nothing while it has no repositories\.$/);
  const how = app.locator("details", { hasText: "How it would work" });
  await expect(how.getByText("returned unless validators refute the claim")).toBeHidden();
  await how.locator("summary").click();
  await expect(how).toContainText("with a small bond (1 FANG, more if you have several open), returned unless validators refute the claim or the jury marks it spam.");
  await expect(app).not.toContainText("refundable");
});

test("a pool with repositories says how it works and the most a Séance can pay", async ({ page }) => {
  await open(page, "blockula-testnet-2", "#/contribute", [{ repo: "jackdishman/blockula", weight: 10 }]);
  const app = page.locator("#app");
  await expect(app.locator("h1")).toHaveText("Get paid for useful work");
  await expect(app.locator("h2", { hasText: "How it works" })).toBeVisible();
  await expect(app).not.toContainText("has no repositories yet");
  await expect(app.locator(".stat", { hasText: "most a Séance can pay" })).toContainText("100FANGmost a Séance can pay");
  await expect(app.locator(".stat .sub")).toHaveText("25% of the pool, with claims from 16 or more people; less with fewer");
  await expect(app).toContainText("with a small bond (1 FANG, more if you have several open), returned unless validators refute the claim or the jury marks it spam.");
});

test("Learn says who pays for useful work, and that a pool without repositories pays nothing", async ({ page }) => {
  await open(page, OWN_CHAIN, "#/learn");
  const app = page.locator("#app");
  await expect(app).toContainText("Useful work can be paid. Hunts are bounties on GitHub issues, paid by whoever posts them. The contribution pool pays for merged pull requests to repositories governance adds, judged by juries of people with Soul; it pays nothing until it has one, and its payments unlock slowly.");
  await expect(app.getByRole("link", { name: "Hunts →" })).toHaveAttribute("href", "#/hunts");
  await expect(app.getByRole("link", { name: "The pool →" })).toHaveAttribute("href", "#/contribute");
  await expect(app).not.toContainText("Useful work gets paid");
});
