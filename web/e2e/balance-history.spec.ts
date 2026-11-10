// The wallet's balance chart, against the mock node's own history (the faucet payment createWallet records).
import { createWallet, expect, test } from "./helpers";

test("the signed-in account charts its FANG balance, and a failed history can be retried", async ({ page }) => {
  const hint = await createWallet(page);
  const chart = page.getByTestId("balance-chart");
  await expect(chart.locator("svg")).toBeVisible();
  const summary = page.getByTestId("balance-summary");
  await expect(summary).toContainText("100 FANG");
  await expect(summary).toContainText("0 FANG");
  await expect(summary).toContainText("this history");

  await page.getByTestId("range-7d").click();
  await expect(page.getByTestId("range-7d")).toHaveAttribute("aria-pressed", "true");
  await expect(summary).toContainText("last 7 days");
  await page.getByTestId("range-30d").click();
  await expect(summary).toContainText("last 30 days");
  await page.getByTestId("range-all").click();
  await expect(summary).toContainText("this history");

  await page.route("**/api/account/*/txs*", (route) => route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ error: "history unavailable" }) }));
  await page.reload();
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(page.getByTestId("balance-error")).toBeVisible();
  await expect(page.getByTestId("balance-chart")).toHaveCount(0);

  await page.unroute("**/api/account/*/txs*");
  await page.getByTestId("balance-retry").click();
  await expect(chart.locator("svg")).toBeVisible();
  expect(hint.account).toBeTruthy();
});
