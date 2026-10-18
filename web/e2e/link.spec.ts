// Linking a second device: the old device's QR link opens /link on the new one with the account
// filled in; the new device's grant code goes back, and once it's added the new device opens.
import { addAuthenticator, createWallet, expect, storedHint, test } from "./helpers";

test("links a new device from the other device's link, then opens the wallet there", async ({ page, node, browser }) => {
  await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
  const created = await createWallet(page);
  await page.goto("/devices");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  const card = page.locator("#device");
  await expect(card.getByRole("img", { name: "QR code: link a new device to this account" })).toBeVisible();
  await expect(card.getByTestId("device-account")).toHaveText(created.account);
  await card.getByRole("button", { name: "Copy link" }).click();
  const link = new URL(await page.evaluate(() => navigator.clipboard.readText()));
  expect(link.pathname).toBe("/link");
  expect(link.searchParams.get("account")).toBe(created.account);

  // The new device: another browser profile with its own authenticator, on the same chain.
  const context = await browser.newContext();
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await node.install(context);
  const phone = await context.newPage();
  await addAuthenticator(phone);
  await phone.goto(`${link.pathname}${link.search}`);
  await phone.getByRole("button", { name: "Create a passkey for this device" }).click();
  const join = phone.getByTestId("link-account");
  await expect(join).toContainText(created.account);
  await join.getByRole("button", { name: "Make this device's grant code" }).click();
  await expect(phone.getByRole("heading", { name: "Almost there" })).toBeVisible();
  await phone.getByRole("button", { name: "Copy grant code" }).click();
  const code = await phone.evaluate(() => navigator.clipboard.readText());
  const check = ((await phone.locator(".code").textContent()) ?? "").trim();

  await card.getByPlaceholder("bkgrant1.…").fill(code);
  await expect(card.getByText("Check code:")).toContainText(check);
  await card.getByRole("button", { name: "Add as owner device" }).click();
  await expect(card.getByText(/^✓/)).toBeVisible();

  // The new device finds its key on the account and opens it by itself.
  await expect(phone.getByText("The account of")).toBeVisible();
  expect((await storedHint(phone))?.account).toBe(created.account);
  await context.close();
});

test("a device link made on another chain fills nothing in", async ({ page }) => {
  await page.goto(`/link?account=boo1yuuvyc7ngus0l2vytdt6yr0zqhj2qyz9j4zyfxk97vjtpsw9rk4qzmxxe9&chain=blockula-1`);
  await expect(page.getByTestId("device-link-refused")).toContainText("Mainnet");
  await page.getByRole("button", { name: "Create a passkey for this device" }).click();
  await expect(page.getByRole("heading", { name: "Which account?" })).toBeVisible();
  await expect(page.getByTestId("link-account")).toHaveCount(0);
});

test("a new device finds linking from the first welcome screen", async ({ page }) => {
  await page.goto("/welcome");
  await page.getByRole("button", { name: "I already have a wallet" }).click();
  await page.getByRole("link", { name: /^Link this device/ }).click();
  await expect(page.getByRole("heading", { name: "Link this device" })).toBeVisible();
});
