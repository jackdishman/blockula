// The node's built-in explorer (node/src/explorer.html) against a mocked protocol v1 API: staking
// pools on the Validators page, Governance and proposal pages, chain parameters, name expiry, and
// hostile on-chain strings rendered as text. It's a static page, so it's served from a route.
import { readFileSync } from "node:fs";
import path from "node:path";
import { expect, test, type Page } from "@playwright/test";

const ORIGIN = "https://explorer.blockula.test";
const HTML = readFileSync(path.join(__dirname, "../../node/src/explorer.html"), "utf8");
const FANG = 10n ** 9n;
const label = (address: string, name: string | null = null) => ({ address, name, short: `${address.slice(0, 10)}…${address.slice(-4)}` });
const V1 = "boo1validatoroneqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
const V2 = "boo1validatortwoqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
const V3 = "boo1validatorthreeqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
const OWNER = "boo1ownerqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
const HOSTILE = '<img src=x onerror="window.__pwned=1">';

function api(p: string, missing: string[]): unknown {
  if (missing.some((m) => p.startsWith(m))) return null;
  const validator = (address: string, extra: Record<string, unknown>) => ({ validator: label(address), owner: label(OWNER, "vlad.boo"), status: "active", p2p_addr: "203.0.113.1:30300", bond: "0", genesis: false, ...extra });
  if (p === "/status") return { chain_id: "blockula-1", height: 1000, block_time_ms: 1000, epoch: 3, active_validators: 2, mempool: 0, supply_display: "1000000000 FANG", burned_display: "12 FANG", accounts: 5, names: 1, head_age_ms: 500, history_from: 1 };
  if (p === "/validators")
    return {
      epoch: 3, epoch_blocks: 3600, blocks_until_next_epoch: 100, admins: [], min_bond: (10_000n * FANG).toString(), min_bond_display: "10000 FANG", max_validators: 31,
      active: [validator(V1, { stake: (2_000_000n * FANG).toString(), stake_display: "2000000 FANG", self_bond_display: "10000 FANG", commission_bps: 500, delegators: 12 }), validator(V2, { stake: (1_000_000n * FANG).toString(), stake_display: "1000000 FANG", commission_bps: 1000, missed: 7 })],
      next_epoch: [], pending: [], approved: [], removed: [],
      jailed: [validator(V3, { status: "candidate", jailed: true, jailed_until: 4000, stake_display: "50000 FANG", commission_bps: 250 })],
    };
  if (p.startsWith("/proposals"))
    return {
      proposals: [
        { id: 2, title: "Raise the active set to 40", summary: "set max_validators to 40", status: "voting", proposer: label(OWNER, "vlad.boo"), deposit: (1000n * FANG).toString(), tally: { aye: (300n * FANG).toString(), nay: (100n * FANG).toString(), turnout: (400n * FANG).toString(), quorum: (1000n * FANG).toString() }, council: { ayes: 15, nays: 1, of: 22 }, voting_ends_at: 5000, executes_at: 6000, url: "https://forum.blockula.test/p/2" },
        { id: 1, title: HOSTILE, summary: "<b>bold</b>", status: "rejected", url: "javascript:window.__pwned=2", tally: { aye: "0", nay: "5" } },
      ],
    };
  if (p === "/proposal/2") return { id: 2, title: "Raise the active set to 40", summary: "set max_validators to 40", status: "voting", kind: { type: "param_change", key: "max_validators", value: "40" }, proposer: label(OWNER, "vlad.boo"), created_at: 900, deposit: (1000n * FANG).toString(), tally: { aye: (300n * FANG).toString(), nay: (100n * FANG).toString() }, council: { ayes: 15, nays: 1, of: 22, votes: [{ voter: label(V1), aye: true }] }, votes: [{ voter: label(OWNER, "vlad.boo"), aye: true, amount: (100n * FANG).toString(), conviction: 3, weight: (300n * FANG).toString(), locked_until: 9000 }], voting_ends_at: 5000, url: "https://forum.blockula.test/p/2" };
  if (p === "/proposal/1") return { id: 1, title: HOSTILE, summary: "<b>bold</b>", status: "rejected", url: "javascript:window.__pwned=2", kind: { type: "text" }, votes: [] };
  if (p === "/params") return { params: { max_validators: "31", unbonding_blocks: "1814400", slash_double_sign_bps: "500", min_self_bond: (10_000n * FANG).toString(), issuance_initial_ppm: "60000", max_block_bytes: "2097152", name_price_3: (100n * FANG).toString() } };
  if (p === "/fees") return { base_fee_per_byte: "10" };
  if (p === "/name/ghoul" || p === "/name/ghoul.boo") return { name: "ghoul.boo", available: false, owner: label(OWNER, "ghoul.boo"), registered_at: 10, expires_at: 1000 + 86_400 * 30, grace_until: 1000 + 86_400 * 60 };
  if (p === "/name/" + encodeURIComponent("old.boo")) return { name: "old.boo", available: false, owner: label(OWNER, "ghoul.boo"), registered_at: 1, expires_at: 900, grace_until: 2000 };
  if (p.startsWith("/account/")) return { account: label(OWNER, "ghoul.boo"), balance_display: "5 FANG", nonce: 1, names: ["ghoul.boo", "old.boo"], hunts_won: [], hunts_created: [], familiars: [], recent_txs: [], contribution: null };
  if (p === "/blocks?limit=15") return [];
  return null;
}

/** Serves the explorer and the mocked API (`missing`: path prefixes the node doesn't serve), then opens `hash`. */
async function open(page: Page, hash: string, missing: string[] = []) {
  await page.route(`${ORIGIN}/`, (r) => r.fulfill({ contentType: "text/html", body: HTML }));
  await page.route(`${ORIGIN}/api/**`, (r) => {
    const u = new URL(r.request().url());
    const body = api(u.pathname.replace(/^\/api/, "") + u.search, missing);
    return body === null ? r.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "not found" }) }) : r.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  });
  await page.route(/fonts\.(googleapis|gstatic)\.com/, (r) => r.abort());
  await page.goto(`${ORIGIN}/${hash}`);
}

test("validators show pools, stake, commission and jail", async ({ page }) => {
  await open(page, "#/validators");
  const app = page.locator("#app");
  await expect(app).toContainText("Anyone can register a validator with a self-bond of at least 10000 FANG");
  await expect(app).toContainText("stake 2000000 FANG · self-bond 10000 FANG · commission 5% · 12 delegators");
  await expect(app).toContainText("commission 10% · missed 7 of its last turns");
  await expect(app).toContainText("Jailed (1)");
  await expect(app.locator(".pill.failed")).toHaveText(["jailed"]);
  await expect(app).toContainText("of 31");
  await expect(app).not.toContainText("Admin actions");
  // Not a test chain: the footer says nothing about test chains.
  await expect(page.locator("#imprint")).toHaveText("Post-quantum signatures · ML-DSA-65");
});

test("governance lists proposals with tallies, council votes and timelock, and escapes them", async ({ page }) => {
  await open(page, "#/governance");
  const app = page.locator("#app");
  await expect(app).toContainText("Raise the active set to 40");
  await expect(app).toContainText("aye 300 FANG · nay 100 FANG · turnout 400 FANG of 1000 FANG quorum · council: 15 of 22 validators approve, 1 reject · voting ends at block #5000 · executes at block #6000");
  await expect(app).toContainText(HOSTILE);
  await expect(app.locator("img")).toHaveCount(0);
  await expect(app.locator("b", { hasText: "bold" })).toHaveCount(0);

  await page.goto(`${ORIGIN}/#/proposal/2`);
  await expect(app).toContainText("Proposal #2");
  await expect(app.getByRole("link", { name: "https://forum.blockula.test/p/2" })).toHaveAttribute("href", "https://forum.blockula.test/p/2");
  await expect(app).toContainText("Council votes (1)");
  await expect(app).toContainText("aye with 100 FANG at 3× conviction = 300 FANG of weight");
  await expect(app).toContainText('"key": "max_validators"');

  await page.goto(`${ORIGIN}/#/proposal/1`);
  await expect(app).toContainText(HOSTILE);
  await expect(app.locator('a[href^="javascript"]')).toHaveCount(0);
  await expect(app).toContainText("javascript:window.__pwned=2");
  expect(await page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned)).toBeUndefined();
});

test("parameters are grouped and readable", async ({ page }) => {
  await open(page, "#/params");
  const app = page.locator("#app");
  await expect(app).toContainText("base fee 10 drops per byte");
  await expect(app.locator("tr", { hasText: "unbonding_blocks" })).toContainText("1,814,400 blocks (21 days)");
  await expect(app.locator("tr", { hasText: "slash_double_sign_bps" })).toContainText("5% (500 bps)");
  await expect(app.locator("tr", { hasText: "min_self_bond" })).toContainText("10000 FANG");
  await expect(app.locator("tr", { hasText: "issuance_initial_ppm" })).toContainText("6%");
  await expect(app.locator("tr", { hasText: "max_block_bytes" })).toContainText("2 MiB");
  for (const g of ["staking", "fees", "issuance", "names"]) await expect(app.getByRole("heading", { name: g })).toBeVisible();
});

test("names show when they expire", async ({ page }) => {
  await open(page, "#/name/ghoul");
  const app = page.locator("#app");
  await expect(app).toContainText("expires at block #2593000 (in about 30 days)");
  await page.goto(`${ORIGIN}/#/account/ghoul.boo`);
  await expect(app.locator(".row", { hasText: "ghoul.boo" }).first()).toContainText("in about 30 days");
  await expect(app.locator(".row", { hasText: "old.boo" })).toContainText("expired at block #900; its owner can renew it until block #2000");
});

test("a node without the v1 endpoints says so", async ({ page }) => {
  await open(page, "#/governance", ["/proposals", "/params"]);
  await expect(page.locator("#app")).toContainText("This node doesn't serve proposals");
  await page.goto(`${ORIGIN}/#/params`);
  await expect(page.locator("#app")).toContainText("This node doesn't serve its parameters yet");
});
