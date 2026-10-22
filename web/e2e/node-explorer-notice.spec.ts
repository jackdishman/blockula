// The node's built-in explorer (node/src/explorer.html), as explorer.spec.ts serves it: its footer
// says what network this is (mainnet: experimental, not externally audited; test chains: test FANG
// has no value) with Terms and Risks links, and its copy makes no value or reward promises.
import { readFileSync } from "node:fs";
import path from "node:path";
import { expect, test, type Page } from "@playwright/test";

const ORIGIN = "https://node-explorer.blockula.test";
const HTML = readFileSync(path.join(__dirname, "../../node/src/explorer.html"), "utf8");
const TERMS = "https://github.com/jackdishman/blockula/blob/main/TERMS.md";
const RISKS = "https://github.com/jackdishman/blockula/blob/main/docs/RISKS.md";
const BURNED = "Fees destroyed forever. The base fee prices block space so spam costs the spammer; burning it takes that FANG out of the supply. It isn't a mechanism to manage FANG's price.";

function api(p: string, chainId: string): unknown {
  if (p === "/status") return { chain_id: chainId, height: 1000, block_time_ms: 1000, epoch: 3, active_validators: 4, mempool: 0, supply_display: "1000000000 FANG", burned_display: "12 FANG", accounts: 5, names: 1, head_age_ms: 500, history_from: 1 };
  if (p.startsWith("/blocks")) return [];
  if (p === "/hunts") return { count: 0, hunts: [], open_bounties_display: "0 FANG" };
  if (p === "/contributions") return { pool: { balance_display: "0 FANG", next_budget_display: "0 FANG" }, repos: [], jurors: { active: 0, pool: 0 }, open_seance: null, recent_seances: [], next_seance_at: 10, rules: { claim_bond_display: "1 FANG", claim_maturity_blocks: 100, jury_size: 7, juror_share: "10%", pool_share_per_seance: "25%", vesting_blocks: 1000, min_jury: 3 } };
  if (p.startsWith("/souls")) return { souls: [] };
  if (p.startsWith("/claims")) return { claims: [] };
  return null;
}

/** Serves the explorer with a node on `chainId`, then opens `hash`. */
async function open(page: Page, chainId: string, hash = "") {
  await page.route(`${ORIGIN}/`, (r) => r.fulfill({ contentType: "text/html", body: HTML }));
  await page.route(`${ORIGIN}/api/**`, (r) => {
    const u = new URL(r.request().url());
    if (u.pathname === "/api/health") return r.fulfill({ contentType: "application/json", body: JSON.stringify({ ok: true }) });
    const body = api(u.pathname.replace(/^\/api/, "") + u.search, chainId);
    return body === null ? r.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "not found" }) }) : r.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  });
  await page.route(/fonts\.(googleapis|gstatic)\.com/, (r) => r.abort());
  await page.route(/\/haunt\.js$/, (r) => r.fulfill({ contentType: "text/javascript", body: "" }));
  await page.goto(`${ORIGIN}/${hash}`);
}

test("on mainnet the footer says it's experimental and not externally audited, with Terms and Risks", async ({ page }) => {
  await open(page, "blockula-1");
  const footer = page.locator("footer");
  await expect(page.locator("#notice")).toHaveText("Experimental mainnet · not externally audited");
  // explorer.spec.ts holds the imprint itself to this.
  await expect(page.locator("#imprint")).toHaveText("Post-quantum signatures · ML-DSA-65");
  await expect(footer).not.toContainText("test chains only");
  await expect(footer).not.toContainText("no value");
  await expect(footer.getByRole("link", { name: "Terms" })).toHaveAttribute("href", TERMS);
  await expect(footer.getByRole("link", { name: "Risks" })).toHaveAttribute("href", RISKS);
});

test("a test chain's footer says it isn't audited and test FANG has no value", async ({ page }) => {
  await open(page, "blockula-testnet-2");
  // Wait for the page to have read the status (the home view shows the chain's height).
  await expect(page.locator("#app")).toContainText("#1000");
  await expect(page.locator("#imprint")).toHaveText("Post-quantum signatures · ML-DSA-65 · Not audited, test chains only");
  await expect(page.locator("#notice")).toHaveText("Test FANG has no value");
  await expect(page.locator("footer").getByRole("link", { name: "Risks" })).toHaveAttribute("href", RISKS);
  await expect(page.locator("footer")).not.toContainText("Experimental mainnet");
});

test("the glossary's 'burned' makes no price claim, and Hunts and claim bonds say what can happen", async ({ page }) => {
  await open(page, "blockula-1");
  await expect(page.locator("#app .term", { hasText: "burned" }).first()).toHaveAttribute("data-tip", BURNED);
  await expect(page.locator("body")).not.toContainText("ties the token");
  await page.goto(`${ORIGIN}/#/hunts`);
  await expect(page.locator("#app")).toContainText("Fix the issue and get the pull request merged, and its creator can award the bounty to your account");
  await expect(page.locator("#app")).not.toContainText("is awarded to your account");
  await page.goto(`${ORIGIN}/#/contribute`);
  await expect(page.locator("#app")).toContainText("with a small bond (1 FANG, more if you have several open), returned unless validators refute the claim or the jury marks it spam.");
  await expect(page.locator("#app")).not.toContainText("refundable");
});
