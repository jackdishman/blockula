// The landing page: a first visit to / (no wallet on this device) gets Blockula 1.0's page, with the
// testnet wallet a click away; a device that has a wallet still opens it at /.
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";

test("a first visit to / gets the landing page, and its button opens the wallet's welcome", async ({ page }) => {
  await watchCsp(page);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Launch your own quantum-safe ledger" })).toBeVisible();
  await expect(page.locator(".landing-cmd code").first()).toHaveText("curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh");
  await expect(page.locator(".landing-cmd code").nth(1)).toHaveText("blockula summon mychain --host <your address>");
  await expect(page.getByRole("link", { name: "Read the launch guide" })).toHaveAttribute("href", "https://github.com/jackdishman/blockula/blob/main/docs/LAUNCH.md");
  // What FANG is, said as the wallet says it, and no mainnet.
  await expect(page.locator("main")).toContainText("has no value by design");
  await expect(page.locator("main")).not.toContainText(/mainnet/i);
  await expect(page).toHaveURL(/\/$/);
  await page.getByRole("link", { name: "Open the testnet wallet" }).click();
  await expect(page).toHaveURL(/\/welcome$/);
  await expect(page.locator(".chapter-body")).toBeVisible();
  expect(await cspViolations(page)).toEqual([]);
});

test("a device with a wallet opens it at /, not the landing page", async ({ page }) => {
  await createWallet(page);
  // A new page load locks the wallet: / asks for the passkey.
  await page.goto("/");
  await expect(page.getByRole("button", { name: "Unlock with passkey" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Launch your own quantum-safe ledger" })).toHaveCount(0);
});
