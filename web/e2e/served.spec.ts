// The wallet a node serves at /wallet/ (node/src/wallet.rs), on a real chain (e2e/served-node.sh: a
// node built with `npm run build:wallet`, on a chain named "Served Ledger"). The headline: one link
// from the chain's owner, and a friend's browser makes a passkey wallet, takes coins from the
// faucet and pays a name, with nothing else to install or host. Runs with E2E_SERVED=1.
import { expect, test, type Page } from "@playwright/test";
import { addAuthenticator, cspViolations, watchCsp } from "./helpers";

test.describe.configure({ mode: "serial" });

/**
 * Script errors (hydration mismatches included), wallet files the node didn't serve, and any
 * request for the placeholder id, from now on. (The node's own 404s, such as `/api/multisig/…` for
 * an account that isn't one, are answers.)
 */
function watchErrors(page: Page) {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(`pageerror: ${e.message}`));
  page.on("console", (m) => m.type() === "error" && !m.text().startsWith("Failed to load resource") && errors.push(`console: ${m.text()}`));
  page.on("response", (r) => {
    const path = new URL(r.url()).pathname;
    if ((r.status() >= 400 && path.startsWith("/wallet/")) || (path.startsWith("/api/") && /\/_$/.test(path))) errors.push(`${r.status()} ${path}`);
  });
  return errors;
}

/** From the node's link to an open wallet: passkey, faucet, an optional name. */
async function onboard(page: Page, name?: string) {
  await page.goto("/wallet/");
  const badge = page.getByTestId("network-badge");
  await expect(badge).toHaveText("Served Ledger");
  await expect(page.locator("header")).not.toContainText(/testnet|mainnet|custom node/i);
  await expect(page.getByRole("combobox", { name: "Network" })).toHaveCount(0);
  await page.getByRole("link", { name: "Make a wallet" }).first().click();
  await expect(page).toHaveURL(/\/wallet\/welcome\/?$/);
  await page.getByRole("button", { name: "Skip intro" }).click();
  await page.getByRole("button", { name: "Create wallet with passkey" }).click();
  await expect(page.getByRole("heading", { name: /landed!$/ })).toBeVisible({ timeout: 30_000 });
  await page.getByRole("button", { name: "Next: your recovery kit" }).click();
  await page.getByRole("button", { name: /^Skip for now/ }).click();
  await expect(page.getByRole("heading", { name: "Claim your .boo name" })).toBeVisible();
  if (name) {
    await page.getByPlaceholder("vlad").fill(name);
    await page.getByRole("button", { name: `Claim ${name}.boo` }).click();
    await expect(page.getByRole("heading", { name: /^Welcome to the night/ })).toBeVisible({ timeout: 30_000 });
  } else {
    await page.getByRole("button", { name: "Skip for now" }).click();
  }
  await page.getByRole("button", { name: "Open my wallet" }).click();
  await expect(page.getByText("The account of")).toBeVisible();
}

test("one link: a friend makes a passkey wallet, takes faucet coins and pays a name", async ({ browser }) => {
  test.setTimeout(180_000);
  const name = `ghoul${Date.now().toString(36).slice(-5)}`;

  // The chain's owner, with a name to be paid at.
  const owner = await browser.newPage();
  await addAuthenticator(owner);
  await watchCsp(owner);
  const ownerErrors = watchErrors(owner);
  await onboard(owner, name);
  // The invite is this wallet's own address: on localhost it says to put the node online first.
  await owner.goto("/wallet/");
  await expect(owner.getByTestId("network-badge")).toHaveText("Served Ledger");
  expect(await owner.title()).toBe("Served Ledger Wallet");

  // A friend, in another browser profile with its own passkeys.
  const friend = await (await browser.newContext()).newPage();
  await addAuthenticator(friend);
  await watchCsp(friend);
  const errors = watchErrors(friend);
  await onboard(friend);

  // Pay the owner by name.
  await friend.getByRole("link", { name: "Send", exact: true }).first().click();
  await expect(friend).toHaveURL(/\/wallet\/send\/?$/);
  await friend.getByPlaceholder(/^alice\.boo or /).fill(`${name}.boo`);
  await friend.getByPlaceholder("0.0").fill("1");
  await friend.getByRole("button", { name: "Review" }).click();
  await friend.getByRole("button", { name: "Confirm & send" }).click();
  await expect(friend.getByRole("heading", { name: "Sent" })).toBeVisible({ timeout: 30_000 });

  // The receipt and an account page, by client-side links: one page per route, any id.
  await friend.evaluate(() => ((window as unknown as { __spa: number }).__spa = 1));
  await friend.getByRole("link", { name: "View the receipt" }).click();
  await expect(friend).toHaveURL(/\/wallet\/tx\/[0-9a-f]{64}\/$/);
  const txId = friend.url().match(/\/tx\/([0-9a-f]{64})\//)?.[1];
  await expect(friend.locator("main")).toContainText(`${name}.boo`, { timeout: 30_000 });
  await expect(friend.locator("main")).toContainText(/Finalized|finalized/, { timeout: 30_000 });
  await friend.locator("main").getByRole("link", { name: `${name}.boo` }).first().click();
  // A name looks like a file to Next.js, so its link has no trailing slash.
  await expect(friend).toHaveURL(new RegExp(`/wallet/account/${name}\\.boo/?$`));
  await expect(friend.getByRole("heading", { level: 1 })).toContainText(name);
  // Back to the receipt, then to a block: still the same document, no reload.
  await friend.goBack();
  await expect(friend).toHaveURL(new RegExp(`/wallet/tx/${txId}/$`));
  await friend.locator("main").getByRole("link", { name: /^block #[\d,]+$/i }).first().click();
  await expect(friend).toHaveURL(/\/wallet\/block\/\d+\/$/);
  await expect(friend.getByRole("heading", { level: 1 })).toHaveText(/^#\d+$/);
  expect(await friend.evaluate(() => (window as unknown as { __spa?: number }).__spa)).toBe(1);

  // A fresh load of an id's page works too (the node serves the placeholder's page for it).
  await friend.goto(`/wallet/tx/${txId}/`);
  await expect(friend.locator("main")).toContainText(`${name}.boo`, { timeout: 30_000 });

  // The owner's wallet sees it arrive.
  // A reload locks it (the key lives in the tab's memory): the same passkey opens it again.
  await owner.reload();
  await owner.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(owner.getByText("The account of")).toBeVisible();
  for (const p of [owner, friend]) expect(await cspViolations(p)).toEqual([]);
  expect(errors).toEqual([]);
  expect(ownerErrors).toEqual([]);
});

test("the explorer links to the wallet, and the node's headers hold", async ({ page, request }) => {
  await page.goto("/");
  const open = page.locator("nav").getByRole("link", { name: "Open wallet" });
  await expect(open).toHaveAttribute("href", "/wallet/");
  await page.goto("/#/start/invite");
  await expect(page.locator("#app")).toContainText("Open the wallet");
  await page.goto("/#/start");
  await expect(page.locator("#app .invite").first()).toContainText(`${new URL(page.url()).origin}/wallet/`);
  const status = await (await request.get("/api/status")).json();
  expect(status.wallet).toBe("/wallet/");
  const res = await request.get("/wallet/account/anyone.boo/");
  expect(res.status()).toBe(200);
  const csp = res.headers()["content-security-policy"];
  expect(csp).toMatch(/script-src 'self'( 'sha256-[A-Za-z0-9+/]+=*')+;/);
  expect(csp).not.toMatch(/unsafe-inline|unsafe-eval/);
  expect(csp).toContain("connect-src 'self'");
  // Out of the export: an escaped slash or dot-dot is refused (a plain one never reaches the node).
  expect((await request.get("/wallet/tx/..%2f..%2fCargo.toml")).status()).toBe(400);
});
