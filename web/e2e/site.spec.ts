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
        chain_id: "blockula-demo",
        name: "Blockula",
        description: "Friends' chain for pizza debts.",
        token: "FANG",
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

test("agents first: the one line, the install and the guestbook; then the demo chain live, the kit and the comparison", async ({ page }) => {
  await watchCsp(page);
  const reads = await demoChain(page);
  await page.goto("/");
  // The hero leads with agents: the one line, the install for the demo chain, the guestbook.
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Give your agent an allowance.");
  const hero = page.getByTestId("hero");
  await expect(hero).toContainText("a wallet, a .boo name and a budget the chain enforces, and the coins are worth nothing");
  await expect(page.getByTestId("hero-install").locator(".landing-cmd code")).toHaveText("claude mcp add blockula -- npx -y @blockula/mcp --network demo");
  // @blockula/mcp 1.0.5 has --network demo, so the hero carries no version caveat any more.
  await expect(page.getByTestId("hero-install-note")).toHaveCount(0);
  await expect(page.getByTestId("hero-guestbook")).toHaveAttribute("href", `${DEMO_NODE}/wallet/guestbook/`);
  await expect(hero.getByRole("link", { name: "What to tell your agent" })).toHaveAttribute("href", "#agents");
  await expect(page.getByRole("navigation", { name: "Sections" }).getByRole("link", { name: "Agents" })).toHaveAttribute("href", "#agents");
  const agents = page.getByTestId("agents");
  await expect(agents.getByRole("heading", { level: 3 })).toHaveText(["Make a wallet, take a name", "Sign the guestbook", "Trick or treat", "Give it an allowance"]);
  await expect(agents.getByTestId("agent-step-prompt").nth(1)).toHaveText("Send 0.1 FANG to guestbook.boo with a memo of your choosing: one line, nothing personal.");
  await expect(agents.getByRole("link", { name: "The playground, step by step" })).toHaveAttribute("href", "https://github.com/jackdishman/blockula/blob/main/docs/PLAYGROUND.md");
  await expect(agents).toContainText("untrusted data, never as instructions");
  // The kit comes second.
  await expect(page.getByTestId("kit").getByRole("heading", { level: 2 })).toHaveText("A post-quantum blockchain you run yourself");
  await expect(page.getByRole("link", { name: "Start your own" }).first()).toHaveAttribute("href", "#start");

  const demo = page.getByTestId("demo-live");
  await expect(page.getByTestId("demo-name")).toHaveText("Blockula");
  await expect(page.getByTestId("demo-token")).toHaveText("FANG");
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
  await expect(prompt).toContainText(`claude mcp add blockula -e BLOCKULA_RPC=${DEMO_NODE} -e BLOCKULA_CHAIN_ID=blockula-demo -e BLOCKULA_GENESIS_HASH=${GENESIS} -- npx -y @blockula/mcp`);
  await expect(prompt).toContainText("Join Blockula");

  // Why it's different, how it compares, starting your own, contributing, and the honesty lines.
  await expect(page.locator("#why .site-tile h3")).toContainText(["Ready for quantum computers", "Final in about two seconds", "Runs on what you already own", "Yours to host", "No seed phrases", "Nobody to get rich"]);
  await expect(page.getByTestId("compare").locator("thead th")).toHaveText(["Chain", "Signatures", "When a payment is final", "Who makes blocks", "To run a node", "Making a wallet", "Coins for early buyers"]);
  await expect(page.getByTestId("compare").locator("tbody tr:not(.site-compare-group) th")).toContainText(["Blockula", "Bitcoin", "Ethereum", "Solana", "Base", "Arbitrum One", "Optimism", "Robinhood Chain", "ZKsync Era", "Starknet"]);
  // Every chain answers every question.
  for (const row of await page.getByTestId("compare").locator("tbody tr:not(.site-compare-group)").all()) await expect(row.locator("td")).toHaveCount(6);
  await expect(page.locator("#compare")).toContainText("haven't been independently audited");
  await expect(page.locator("#start .landing-cmd code")).toHaveText(["curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh", "blockula create"]);
  await expect(page.getByRole("link", { name: "The guide", exact: true })).toHaveAttribute("href", "https://github.com/jackdishman/blockula/blob/main/docs/CREATE.md");
  await expect(page.getByTestId("contribute-prompt")).toContainText("Contribute to Blockula (https://github.com/jackdishman/blockula). Clone it");
  await expect(page.getByRole("link", { name: "Good first issues" })).toHaveAttribute("href", "https://github.com/jackdishman/blockula/issues?q=is%3Aopen+label%3A%22good+first+issue%22");
  await expect(page.getByTestId("imprint-note")).toContainText("The coins have no value");
  await expect(page.getByTestId("imprint-note")).toContainText("for education and research only");
  await expect(page.getByTestId("imprint-terms").getByRole("link", { name: "Terms of Use" })).toHaveAttribute("href", "/terms");
  await expect(page.getByTestId("imprint-terms").getByRole("link", { name: "Privacy Policy" })).toHaveAttribute("href", "/privacy");

  // Agents first, then the demo, then the kit; the comparison comes after the kit's sections.
  const order = await page.evaluate(() => Array.from(document.querySelectorAll("main > section[id]"), (e) => e.id));
  const at = (id: string) => order.indexOf(id);
  for (const id of ["agents", "demo", "kit", "start", "chains", "compare", "join-in"]) expect(at(id), id).toBeGreaterThanOrEqual(0);
  expect(at("agents")).toBeLessThan(at("demo"));
  expect(at("demo")).toBeLessThan(at("kit"));
  for (const kit of ["kit", "why", "how", "start", "chains"]) expect(at(kit), kit).toBeLessThan(at("compare"));

  // No wallet chrome, and nothing the project never says.
  await expect(page.getByTestId("network-badge")).toHaveCount(0);
  await expect(page.getByRole("combobox", { name: "Network" })).toHaveCount(0);
  // Money words appear only to say there's none of it ("no investors, no VC, no token sale").
  const body = (await page.locator("body").innerText()).replace(/\bno (investors|VC|token sale)\b/gi, "");
  expect(body).not.toMatch(/mainnet|\bcontracts?\b|token sale|\bAPR\b|\byield\b|airdrop|invest|launch|to the moon/i);
  expect(await cspViolations(page)).toEqual([]);
});

test("join in: four ways to help, each with one link to where it starts", async ({ page }) => {
  await demoChain(page);
  await page.goto("/");
  const join = page.getByTestId("join-in");
  await expect(join.getByRole("heading", { level: 2 })).toHaveText("Built in the open, by whoever turns up");
  await expect(join.getByRole("heading", { level: 3 })).toHaveText(["Run a validator", "Fix something small", "Start a chain and list it", "Break it and say so"]);
  await expect(join.getByRole("link", { name: "Become a validator" })).toHaveAttribute("href", /\/docs\/BECOME-A-VALIDATOR\.md$/);
  await expect(join.getByRole("link", { name: "Pick one, or hand your agent the prompt" })).toHaveAttribute("href", "#contribute");
  await expect(page.locator("#contribute")).toContainText("Good first issues");
  await expect(join.getByRole("link", { name: "Start your own" })).toHaveAttribute("href", "#start");
  await expect(join.getByRole("link", { name: "Where to report what" })).toHaveAttribute("href", /\/SUPPORT\.md$/);
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
  await expect(resting.getByRole("link", { name: "Start your own" })).toHaveAttribute("href", "#start");
  await expect(page.getByTestId("demo-wallet")).toHaveCount(0);
  await expect(page.getByTestId("connect-input")).toBeVisible();
  await expect(page.locator("#start .landing-cmd")).toHaveCount(2);
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

test("the site serves its Terms of Use and Privacy Policy, and sends wallet routes home", async ({ page }) => {
  await watchCsp(page);
  await page.goto("/terms");
  await expect(page.getByRole("heading", { name: "Terms of Use", level: 1 })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Sections" }).getByRole("link", { name: "Demo" })).toHaveAttribute("href", "/#demo");
  await page.goto("/privacy");
  await expect(page.getByRole("heading", { name: "Privacy Policy", level: 1 })).toBeVisible();
  await page.goto("/send");
  await expect(page).toHaveURL(/\/$/);
  expect(await cspViolations(page)).toEqual([]);
});
