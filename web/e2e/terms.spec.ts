// The Terms of Use and Privacy Policy: nobody makes, restores or unlocks a wallet before agreeing
// (components/Terms.tsx), a change of terms asks again, and both pages are there to read.
import type { Browser } from "@playwright/test";
import { TERMS_KEY, TERMS_VERSION } from "../lib/legal";
import type { MockNode } from "./mock-node";
import { addAuthenticator, cspViolations, expect, test, watchCsp } from "./helpers";

/** A page in its own profile that hasn't agreed to anything. */
async function stranger(browser: Browser, node: MockNode) {
  const context = await browser.newContext();
  await node.install(context, { terms: false });
  const page = await context.newPage();
  await addAuthenticator(page);
  return { context, page };
}

test("onboarding starts with the agreement, and it's remembered", async ({ browser, node }) => {
  const { context, page } = await stranger(browser, node);
  await watchCsp(page);
  await page.goto("/welcome");
  const agreement = page.getByTestId("terms-agreement");
  await expect(agreement).toContainText("For education and research only.");
  await expect(agreement).toContainText("The coins have no value.");
  await expect(page.getByRole("button", { name: "Create wallet with passkey" })).toHaveCount(0);
  const agree = page.getByRole("button", { name: "I agree" });
  await expect(agree).toBeDisabled();
  await page.getByTestId("terms-checkbox").check();
  await agree.click();
  await expect(agreement).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Skip intro" })).toBeVisible();
  const stored = await page.evaluate((k) => JSON.parse(localStorage.getItem(k) ?? "null"), TERMS_KEY);
  expect(stored).toMatchObject({ v: 1, version: TERMS_VERSION });
  await page.reload();
  await expect(page.getByRole("button", { name: "Skip intro" })).toBeVisible();
  await expect(page.getByTestId("terms-agreement")).toHaveCount(0);
  expect(await cspViolations(page)).toEqual([]);
  await context.close();
});

test("restoring and linking ask too; older terms ask again before unlocking", async ({ browser, node }) => {
  const { context, page } = await stranger(browser, node);
  for (const path of ["/recover", "/link"]) {
    await page.goto(path);
    await expect(page.getByTestId("terms-agreement")).toBeVisible();
  }
  // A wallet saved before the terms changed: the unlock screen waits for the new agreement.
  await page.evaluate((k) => {
    localStorage.setItem(k, JSON.stringify({ v: 1, version: "2000-01-01", at: new Date(0).toISOString() }));
    localStorage.setItem("blockula.wallet", JSON.stringify({ v: 1, kind: "passkey", credentialId: "AA", keyId: "11".repeat(32), account: "tboo1x" }));
  }, TERMS_KEY);
  await page.goto("/");
  await expect(page.getByTestId("terms-agreement")).toBeVisible();
  await expect(page.getByRole("button", { name: "Unlock with passkey" })).toHaveCount(0);
  await page.getByTestId("terms-checkbox").check();
  await page.getByRole("button", { name: "I agree" }).click();
  await expect(page.getByRole("button", { name: "Unlock with passkey" })).toBeVisible();
  await context.close();
});

test("the Terms of Use and Privacy Policy, linked from every page", async ({ page }) => {
  await watchCsp(page);
  await page.goto("/explore");
  await expect(page.getByTestId("imprint-terms")).toContainText("For education and research only.");
  await page.getByTestId("imprint-terms").getByRole("link", { name: "Terms" }).click();
  await expect(page.getByRole("heading", { name: "Terms of Use", level: 1 })).toBeVisible();
  await expect(page.getByTestId("terms-page")).toContainText(`Last updated ${TERMS_VERSION}`);
  await expect(page.getByTestId("terms-page")).toContainText("We have never sold chain units and never will");
  await page.goto("/privacy");
  await expect(page.getByRole("heading", { name: "Privacy Policy", level: 1 })).toBeVisible();
  await expect(page.getByTestId("privacy-page")).toContainText("your wallet's key never leaves your device");
  expect(await cspViolations(page)).toEqual([]);
});
