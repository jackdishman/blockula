// Onboarding that tells the truth on every chain: faucet copy only where there's a faucet, FANG
// said to have no value, a way forward when the node is down, and Home saying what the balance is
// made of and when to worry (one passkey, a name about to lapse).
import { parseFang } from "@blockula/sdk";
import type { Page } from "@playwright/test";
import { RPC } from "./env";
import { VALIDATOR } from "./mock-node";
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";

/** Words the wallet no longer says: absolute quantum claims, FANG as a currency, a mainnet to come. */
const OVERCLAIMS = /quantum-safe|Safe from tomorrow|Blockula's currency|mainnet/i;
const NO_VALUE = "FANG has no value: nobody sells it, buys it or gets paid in it.";

async function introAndCreate(page: Page) {
  await page.goto("/welcome");
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByRole("heading", { name: "Post-quantum signatures (ML-DSA-65)" })).toBeVisible();
  await page.getByRole("button", { name: "Let's go" }).click();
  await page.getByRole("button", { name: "Create wallet with passkey" }).click();
}

test("a faucet chain: FANG is said to have no value, and the story makes no absolute claims", async ({ page }) => {
  await watchCsp(page);
  await page.goto("/welcome");
  // Chapter I doesn't claim "many computers", or one Blockula blockchain: each chain is its own.
  await expect(page.locator(".chapter-body")).toContainText("Your FANG lives on a Blockula chain, a shared ledger kept by its validators.");
  await expect(page.locator("main")).not.toContainText(OVERCLAIMS);
  await introAndCreate(page);
  await expect(page.getByRole("heading", { name: "100 FANG landed!" })).toBeVisible();
  await expect(page.getByText("That's FANG, this chain's native unit.")).toBeVisible();
  await expect(page.getByTestId("fang-no-value")).toHaveText(NO_VALUE);
  await expect(page.locator("main")).not.toContainText(OVERCLAIMS);
  await page.getByRole("button", { name: "Next: your recovery kit" }).click();
  await page.getByRole("button", { name: /^Skip for now/ }).click();
  // 100 FANG buys a name: the claim form, not the "later" step.
  await expect(page.getByPlaceholder("vlad")).toBeVisible();
  await expect(page.getByTestId("name-later")).toHaveCount(0);
  await page.getByRole("button", { name: "Skip for now" }).click();
  await expect(page.getByRole("heading", { name: "Your wallet is ready" })).toBeVisible();
  await expect(page.getByText("Your account signs with ML-DSA-65, a post-quantum signature standard.")).toBeVisible();
  // A faucet chain needs no "How FANG reaches you".
  await expect(page.getByTestId("first-fang")).toHaveCount(0);
  await expect(page.locator("main")).not.toContainText(OVERCLAIMS);
  await page.getByRole("button", { name: "Open my wallet" }).click();
  await expect(page.getByText("The account of")).toBeVisible();
  expect(await cspViolations(page)).toEqual([]);
});

test("a chain without a faucet: no faucet copy, the name waits, and onboarding says how FANG arrives", async ({ page, node }) => {
  node.faucet = false;
  await introAndCreate(page);
  // Straight to the kit: no "Fetching some test FANG…" on the way.
  await expect(page.getByRole("heading", { name: "Make a recovery kit" })).toBeVisible();
  await page.getByRole("button", { name: /^Skip for now/ }).click();
  const later = page.getByTestId("name-later");
  await expect(later).toContainText("this wallet has none yet. Once some reaches you, claim one from Home.");
  await expect(later.getByPlaceholder("vlad")).toHaveCount(0);
  await expect(page.locator("main")).not.toContainText(/faucet/i);
  await page.getByRole("button", { name: "Skip for now" }).click();
  const card = page.getByTestId("first-fang");
  await expect(card.getByRole("heading", { name: "How FANG reaches you" })).toBeVisible();
  await expect(card.locator("li")).toHaveText(["Someone sends it to you. Share your address or its QR code from Receive. Open Receive", "This chain has no faucet.", NO_VALUE]);
  await expect(card.getByRole("link", { name: "Open Receive" })).toHaveAttribute("href", "/receive");
  // Nothing to buy, no other network to go to, no promise.
  await expect(card.getByRole("link")).toHaveCount(1);
  await expect(page.locator("main")).not.toContainText(OVERCLAIMS);
  await expect(card).not.toContainText(/airdrop|listing|price|invest|sell you/i);
  await page.getByRole("button", { name: "Open my wallet" }).click();
  await expect(page.getByText("The account of")).toBeVisible();

  // The checklist: no faucet button, and "share your address" (there's no name to share yet).
  await page.getByRole("button", { name: /Get some FANG/ }).click();
  await expect(page.locator(".card.learn")).toContainText("FANG is this chain's native unit");
  await expect(page.locator(".card.learn")).toContainText("This chain has no faucet.");
  await expect(page.getByTestId("share-to-receive")).toHaveText("Someone has to send you some: share your address from Receive.");
  await expect(page.getByRole("button", { name: "Get 100 test FANG" })).toHaveCount(0);
});

test("a name step that can't sell a name yet says what the cheapest costs, from the node", async ({ page, node }) => {
  node.faucet = false;
  // /api/params with the chain's name prices (governance parameters).
  await page.route(`${RPC}/api/params`, (route) =>
    route.fulfill({ contentType: "application/json", headers: { "access-control-allow-origin": "*" }, body: JSON.stringify({ params: { name_price_3: "90000000000", name_price_4: "20000000000", name_price_5: "4000000000" } }) }),
  );
  await introAndCreate(page);
  await page.getByRole("button", { name: /^Skip for now/ }).click();
  await expect(page.getByTestId("name-later")).toContainText("Names cost FANG, from 4 FANG for a year, and this wallet has none yet.");
});

test("with the node refusing every request, onboarding offers a way forward within 10 s", async ({ page }) => {
  await page.route(`${RPC}/**`, (route) => route.abort());
  await page.goto("/welcome");
  // The node is down: the app says so, and doesn't send a testnet user to `blockula dev`.
  const banner = page.getByRole("alert").filter({ hasText: "Can't reach the node." });
  await expect(banner).toContainText("Try again in a minute, or pick another node in the menu at the top.");
  await expect(banner).not.toContainText("blockula dev");
  await page.getByRole("button", { name: "Skip intro" }).click();
  await page.getByRole("button", { name: "Create wallet with passkey" }).click();
  // Neutral until the node answers: nothing about a faucet it may not have.
  await expect(page.getByTestId("fund-waiting")).toBeVisible();
  await expect(page.getByText(/test FANG/)).toHaveCount(0);
  const next = page.getByRole("button", { name: "Next: your recovery kit" });
  await expect(next).toBeVisible({ timeout: 10_000 });
  await expect(page.getByRole("heading", { name: "Your wallet is created" })).toBeVisible();
  await expect(page.getByText("The network isn't answering right now. You can continue, and check your balance later.")).toBeVisible();
  await next.click();
  await page.getByRole("button", { name: /^Skip for now/ }).click();
  await expect(page.getByTestId("name-later")).toBeVisible();
  await page.getByRole("button", { name: "Skip for now" }).click();
  // The node never said whether this chain has a faucet, so the last step doesn't guess.
  await expect(page.getByRole("heading", { name: "Your wallet is ready" })).toBeVisible();
  await expect(page.getByTestId("first-fang")).toHaveCount(0);
  await page.getByRole("button", { name: "Open my wallet" }).click();
  await expect(page).toHaveURL(/\/$/);
});

test("with a node that never answers, the app opens after a few seconds and onboarding still goes on", async ({ page }) => {
  // Held forever: no answer, no error (the SDK gives up after 15 s).
  await page.route(`${RPC}/**`, () => {});
  await page.goto("/welcome");
  await expect(page.getByTestId("node-slow")).toBeVisible({ timeout: 10_000 });
  await page.getByRole("button", { name: "Skip intro" }).click();
  await page.getByRole("button", { name: "Create wallet with passkey" }).click();
  await expect(page.getByRole("button", { name: "Next: your recovery kit" })).toBeVisible({ timeout: 10_000 });
});

test("Home says what can be sent, what's staked and what's locked", async ({ page, node }) => {
  const { account } = await createWallet(page);
  await expect(page.getByText("The account of")).toBeVisible();
  // All of it can move: no line under the balance.
  await expect(page.getByTestId("balance-parts")).toHaveCount(0);
  // 10 FANG staked (the pool's shares are worth a drop each), and 50 FANG held by a vote.
  node.shares.set(account, new Map([[VALIDATOR, parseFang("10")]]));
  node.locks.set(account, [{ id: "vote:2", reason: "vote on proposal #2", amount: parseFang("50"), until: node.height + 7200, vesting: false }]);
  // Home re-reads the stake every 30 blocks or so.
  node.height += 30;
  const parts = page.getByTestId("balance-parts");
  await expect(parts).toContainText("Can send now 50 FANG");
  await expect(parts).toContainText("Staked 10 FANG");
  // Two hours of blocks away: a clock time, as Send says it for a lock.
  await expect(parts).toContainText(/Locked 50 FANG until about \d{2}:\d{2}( tomorrow)?/);
  await expect(parts.getByRole("link", { name: "Staked" })).toHaveAttribute("href", "/stake");
});

test("Home warns while one passkey is the only way in, until there's a second key", async ({ page, node }) => {
  const { account } = await createWallet(page);
  const banner = page.getByTestId("one-passkey");
  // A test chain: a plain banner.
  await expect(banner).toContainText("Only this passkey opens your wallet. If you lose it, nothing can get you back in.");
  await expect(banner).not.toHaveClass(/danger/);
  await expect(banner.getByRole("link", { name: "Make a recovery kit" })).toHaveAttribute("href", "/devices#recovery");
  await expect(banner.getByRole("link", { name: "link a second device" })).toHaveAttribute("href", "/devices#device");
  // Hidden for this session.
  await banner.getByRole("button", { name: "Not now" }).click();
  await expect(banner).toHaveCount(0);
  await page.reload();
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(page.getByText("The account of")).toBeVisible();
  await expect(banner).toHaveCount(0);
  // A new session, and a second owner key: no banner.
  await page.evaluate(() => sessionStorage.clear());
  await page.reload();
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(banner).toBeVisible();
  const acct = node.acct(account);
  acct.keys = new Map(node.keysOf(account));
  acct.keys.set("cd".repeat(32), { label: "Laptop", role: { type: "owner" }, added_at: 101 });
  await expect(banner).toHaveCount(0);
});

test("Home doesn't nag a wallet with nothing in it", async ({ page, node }) => {
  node.faucet = false;
  await createWallet(page);
  await expect(page.getByText("The account of")).toBeVisible();
  await page.waitForTimeout(500);
  await expect(page.getByTestId("one-passkey")).toHaveCount(0);
});

test("Home warns about a name that's about to lapse, or has, with a way to renew it", async ({ page, node }) => {
  const { account } = await createWallet(page);
  node.names.set("vlad", account);
  node.nameInfo.set("vlad", { expires_at: node.height + 10 * 86_400 });
  const banner = page.getByTestId("name-expiry");
  await expect(banner).toContainText("vlad.boo expires in about 10 days. Renew it to keep it (1 FANG a year).");
  await expect(banner.getByRole("link", { name: "Renew vlad.boo" })).toHaveAttribute("href", "/programs/names?do=renew_name&name=vlad");
  await expect(banner).not.toHaveClass(/danger/);

  // A name that lapsed: only its owner can renew it, for the rest of its grace period.
  node.names.set("mina", account);
  node.nameInfo.set("mina", { status: "grace", expires_at: 50, grace_until: node.height + 5 * 86_400 });
  const grace = banner.filter({ hasText: "mina.boo" });
  await page.reload();
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(grace).toContainText("mina.boo has expired. Only you can renew it, for about 5 days more");
  await expect(grace).toHaveClass(/danger/);

  // A year away: nothing to say.
  node.nameInfo.set("vlad", { expires_at: node.height + 300 * 86_400 });
  node.names.delete("mina");
  await page.reload();
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(page.getByText("The account of")).toBeVisible();
  await page.waitForTimeout(500);
  await expect(banner).toHaveCount(0);
});
