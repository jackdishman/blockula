// The home checklist remembers what's done per chain and account: the same key has the same
// address on a relaunched testnet, but what it did on the old one isn't done on the new one.
import { CHAIN_ID, GENESIS } from "./env";
import { createWallet, expect, test } from "./helpers";

const ALL = ["fang", "name", "send", "explorer", "device", "kit", "agent"];

test("the checklist ignores ticks from another chain or account, and keeps this chain's", async ({ page }) => {
  const created = await createWallet(page);
  const checklist = page.locator(".card.learn");
  // The faucet paid this wallet on the way in; nothing else is done.
  await expect(checklist).toContainText("1 of 7");

  const reopen = async () => {
    await page.reload();
    await page.getByRole("button", { name: "Unlock with passkey" }).click();
    await expect(page.getByText("The account of")).toBeVisible();
  };
  // Left by a wallet from before the checklist was kept per chain: it can't say which chain it was for.
  await page.evaluate((done) => localStorage.setItem("blockula.tutorial", JSON.stringify({ done, viewedExplorer: true })), ALL);
  await reopen();
  await expect(checklist).toContainText("1 of 7");
  // Everything ticked on an old testnet with this same address, and on this chain for another account.
  await page.evaluate(
    ({ done, scopes }) => localStorage.setItem("blockula.tutorial", JSON.stringify({ v: 2, scopes: Object.fromEntries(scopes.map((s) => [s, { done, viewedExplorer: true }])) })),
    { done: ALL, scopes: [`blockula-testnet-1/${"ab".repeat(32)}/${created.account}`, `${CHAIN_ID}/${GENESIS}/boo1someoneelse`] },
  );
  await reopen();
  await expect(checklist).toContainText("1 of 7");

  // What this account does here is remembered, under this chain and account.
  await page.getByRole("link", { name: /Flows, keys/ }).click();
  await page.getByRole("link", { name: "Wallet", exact: true }).click();
  await expect(checklist).toContainText("2 of 7");
  const mine = await page.evaluate((scope) => JSON.parse(localStorage.getItem("blockula.tutorial") ?? "{}").scopes?.[scope], `${CHAIN_ID}/${GENESIS}/${created.account}`);
  expect(mine).toMatchObject({ viewedExplorer: true, done: expect.arrayContaining(["fang", "explorer"]) });
});
