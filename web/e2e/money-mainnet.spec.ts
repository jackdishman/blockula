// The mainnet build's staking review (wallet-money, USER-4): on blockula-1 it also says who runs
// most validators today. Copy: web/lib/money-copy.ts (a draft for counsel, D3/D4).
import { VALIDATOR } from "./mock-node";
import { createWallet, expect, test } from "./helpers";

test("staking's review on mainnet says most validators are the founder's", async ({ page, node }) => {
  node.faucet = false;
  const { account } = await createWallet(page);
  node.credit(account, "100");
  await page.goto("/stake");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(page.getByText("Available: 100 FANG")).toBeVisible();
  await page.getByLabel("Validator").selectOption(VALIDATOR);
  await page.getByLabel("Amount to delegate").fill("5");
  await page.getByRole("button", { name: /^Delegate 5 FANG to/ }).click();
  const terms = page.getByTestId("review").getByTestId("review-terms");
  await expect(terms).toContainText("You can unstake any time, but it takes 21 days to come back.");
  await expect(terms).toContainText("Most validators are run by Blockula's founder today.");
  expect(node.submitted).toHaveLength(0);
});
