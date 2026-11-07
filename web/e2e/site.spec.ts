// The project site (NEXT_PUBLIC_SITE=project, components/site/): blockula.app's landing page, with
// a demo chain (NEXT_PUBLIC_DEMO_NODE, mocked here) whose stats it reads from the browser, a field
// that opens any node's own wallet, and none of the wallet's routes or chrome.
import { expect, test, type Page, type Route } from "@playwright/test";
import { DEMO_NODE } from "./env";
import { cspViolations, watchCsp } from "./helpers";

const GENESIS = "d3".repeat(32);
const cors = { "access-control-allow-origin": "*" };

// The build lists the fixture chains (e2e/fixtures/chains): their nodes are site-chains.spec.ts's
// business, so here they never answer (and nothing leaves the machine).
test.beforeEach(({ page }) => page.route(/^https:\/\/[^/]*(chains-fixture\.com|tail1234\.ts\.net)\/api\//, (r) => r.abort()));

/** A demo node behind routes: its height moves a block on every status read. */
async function demoChain(page: Page, opts: { wallet?: string | null } = {}) {
  let height = 4210;
  const reads = { status: 0 };
  await page.route(`${DEMO_NODE}/api/status`, (r) => {
    reads.status += 1;
    height += 1;
    return r.fulfill({
      headers: cors,
      json: {
        chain_id: "jack-in-the-block",
        name: "Jack in the Block",
        description: "Friends' chain for pizza debts.",
        token: "JACK",
        height,
        block_time_ms: 1000,
        active_validators: 4,
        accounts: 1234,
        names: 56,
        faucet: true,
        genesis_hash: GENESIS,
        wallet: opts.wallet === undefined ? "/wallet/" : opts.wallet,
      },
    });
  });
  await page.route(`${DEMO_NODE}/api/blocks?limit=5`, (r) =>
    r.fulfill({
      headers: cors,
      json: [0, 1, 2, 3, 4].map((i) => ({ height: height - i, timestamp_ms: Date.now() - i * 1000, tx_count: i === 1 ? 2 : 0, proposer: { name: "jack.boo", short: "boo1qq…zz" } })),
    }),
  );
  return reads;
}

test("the demo chain's live stats, its wallet a click away, and the agent prompt for it", async ({ page }) => {
  await watchCsp(page);
  const reads = await demoChain(page);
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Your own little blockchain, for you and your friends");
  await expect(page.getByRole("link", { name: "Try the demo" })).toHaveAttribute("href", "#demo");
  await expect(page.getByRole("link", { name: "Launch your own" }).first()).toHaveAttribute("href", "#launch");

  const demo = page.getByTestId("demo-live");
  await expect(page.getByTestId("demo-name")).toHaveText("Jack in the Block");
  await expect(page.getByTestId("demo-token")).toHaveText("JACK");
  await expect(page.getByTestId("demo-validators")).toHaveText("4");
  await expect(page.getByTestId("demo-accounts")).toHaveText("1,234");
  await expect(page.getByTestId("demo-names")).toHaveText("56");
  await expect(demo).toContainText("Friends' chain for pizza debts.");
  // The height ticks: the page keeps reading the node.
  const first = Number((await page.getByTestId("demo-height").textContent())?.replace(/,/g, ""));
  expect(first).toBeGreaterThan(4210);
  await expect.poll(async () => Number((await page.getByTestId("demo-height").textContent())?.replace(/,/g, "")), { timeout: 10_000 }).toBeGreaterThan(first);
  expect(reads.status).toBeGreaterThan(1);
  await expect(page.getByTestId("demo-blocks").locator("li")).toHaveCount(5);
  await expect(page.getByTestId("demo-blocks")).toContainText("2 transactions");

  await expect(page.getByTestId("demo-wallet")).toHaveText("Open the demo wallet");
  await expect(page.getByTestId("demo-wallet")).toHaveAttribute("href", `${DEMO_NODE}/wallet/`);
  await expect(page.getByTestId("demo-explorer")).toHaveAttribute("href", `${DEMO_NODE}/`);
  await expect(page.getByTestId("demo-invite")).toHaveAttribute("href", `${DEMO_NODE}/#/start/invite`);

  await demo.getByText("Bring Claude Code to the demo").click();
  const prompt = page.getByTestId("demo-agent-prompt");
  await expect(prompt).toContainText(`claude mcp add blockula -e BLOCKULA_RPC=${DEMO_NODE} -e BLOCKULA_CHAIN_ID=jack-in-the-block -e BLOCKULA_GENESIS_HASH=${GENESIS} -- npx -y @blockula/mcp`);
  await expect(prompt).toContainText("Join Jack in the Block");

  // Launch, contribute, and the honesty line.
  await expect(page.locator("#launch .landing-cmd code")).toHaveText(["curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh", "blockula launch"]);
  await expect(page.getByRole("link", { name: "The launch guide" })).toHaveAttribute("href", "https://github.com/jackdishman/blockula/blob/main/docs/LAUNCH.md");
  await expect(page.getByTestId("contribute-prompt")).toContainText("Contribute to Blockula (https://github.com/jackdishman/blockula). Clone it");
  await expect(page.getByRole("link", { name: "Good first issues" })).toHaveAttribute("href", "https://github.com/jackdishman/blockula/issues?q=is%3Aopen+label%3A%22good+first+issue%22");
  await expect(page.getByTestId("imprint-note")).toContainText("the coins have no value");

  // No wallet chrome, and nothing the project never says.
  await expect(page.getByTestId("network-badge")).toHaveCount(0);
  await expect(page.getByRole("combobox", { name: "Network" })).toHaveCount(0);
  await expect(page.locator("body")).not.toContainText(/mainnet|\bcontracts?\b|token sale|\bAPR\b|\byield\b|airdrop|invest/i);
  expect(await cspViolations(page)).toEqual([]);
});

test("the page may talk to the demo node and the listed chains' nodes only (CSP connect-src)", async ({ page }) => {
  const res = await page.goto("/");
  const csp = res?.headers()["content-security-policy"] ?? "";
  // The listed nodes follow (e2e/site-chains.spec.ts checks the whole list).
  expect(csp).toContain(`connect-src 'self' ${DEMO_NODE} https://`);
  expect(csp).not.toContain("localhost:7070");
});

test("a node that doesn't serve a wallet still gets its explorer", async ({ page }) => {
  await demoChain(page, { wallet: null });
  await page.goto("/");
  await expect(page.getByTestId("demo-live")).toBeVisible();
  await expect(page.getByTestId("demo-wallet")).toHaveCount(0);
  await expect(page.getByTestId("demo-no-wallet")).toBeVisible();
  await expect(page.getByTestId("demo-explorer")).toHaveAttribute("href", `${DEMO_NODE}/`);
});

test("an unreachable demo rests, and the page still works", async ({ page }) => {
  await watchCsp(page);
  await page.route(`${DEMO_NODE}/api/**`, (r) => r.abort());
  await page.goto("/");
  const resting = page.getByTestId("demo-resting");
  await expect(resting).toContainText("The demo chain is resting");
  await expect(resting).toContainText("isn't answering");
  await expect(resting.getByRole("link", { name: "Launch your own" })).toHaveAttribute("href", "#launch");
  await expect(page.getByTestId("demo-wallet")).toHaveCount(0);
  await expect(page.getByTestId("connect-input")).toBeVisible();
  await expect(page.locator("#launch .landing-cmd")).toHaveCount(2);
  expect(await cspViolations(page)).toEqual([]);
});

test("a demo that answers garbage rests too", async ({ page }) => {
  await page.route(`${DEMO_NODE}/api/**`, (r: Route) => r.fulfill({ headers: cors, body: "<html>not a node</html>", contentType: "text/html" }));
  await page.goto("/");
  await expect(page.getByTestId("demo-resting")).toBeVisible();
});

test("connect to any chain: checks the address, then opens that node's wallet", async ({ page }) => {
  await page.route(`${DEMO_NODE}/api/**`, (r) => r.abort());
  await page.goto("/");
  const input = page.getByTestId("connect-input");
  const open = page.getByTestId("connect-open");
  const note = page.getByTestId("connect-note");
  await open.click();
  await expect(note).toContainText("Paste the address");
  for (const [bad, says] of [
    ["http://chain.example", "plain http://"],
    ["ftp://chain.example", "starts with https://"],
    ["my friend's chain", "spaces"],
    ["https://bob:hunter2@chain.example", "password"],
  ]) {
    await input.fill(bad);
    await open.click();
    await expect(note.getByRole("alert"), bad).toContainText(says);
  }
  await expect(page).toHaveURL(/localhost:\d+\/$/);
  // Private addresses may be plain http, and a link into a node means the node.
  await input.fill("100.101.2.3:7070");
  await expect(note).toContainText("Opens http://100.101.2.3:7070/wallet/");
  await input.fill("https://jack.tail1234.ts.net/#/start/invite");
  await expect(note).toContainText("Opens https://jack.tail1234.ts.net/wallet/");
  await page.route("https://jack.tail1234.ts.net/wallet/", (r) => r.fulfill({ contentType: "text/html", body: "<title>A friend's wallet</title>" }));
  await input.press("Enter");
  await expect(page).toHaveURL("https://jack.tail1234.ts.net/wallet/");
  await expect(page).toHaveTitle("A friend's wallet");
});

test("the wallet's routes go to the landing page", async ({ page }) => {
  await page.route(`${DEMO_NODE}/api/**`, (r) => r.abort());
  for (const path of ["/welcome", "/send?to=sam.boo&amount=1", "/explore", "/tx/abc", "/nope"]) {
    const res = await page.request.get(path, { maxRedirects: 0 });
    expect(res.status(), path).toBe(307);
    expect(new URL(res.headers().location, "http://x").pathname, path).toBe("/");
  }
  await page.goto("/send");
  await expect(page).toHaveURL(/localhost:\d+\/$/);
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
});

for (const scheme of ["light", "dark"] as const)
  test(`fits a 390 px phone (${scheme})`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await page.setViewportSize({ width: 390, height: 844 });
    await demoChain(page);
    await page.goto("/");
    await expect(page.getByTestId("demo-live")).toBeVisible();
    await page.getByText("Bring Claude Code to the demo").click();
    const by = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(by).toBeLessThanOrEqual(1);
    // Dark is the site's own: night paper, bone ink.
    const bg = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--paper").trim());
    expect(bg).toBe(scheme === "dark" ? "#16110e" : "#ece3cd");
  });
