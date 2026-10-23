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

// Audit USER-2 / TOK-10: without a faucet, "Get some FANG" says someone has to send it, and to
// share the address until there's a name to share (it used to say "share your name" to people
// who had none).
test("without a faucet, the checklist says to share your address, then your name once you have one", async ({ page, node }) => {
  node.faucet = false;
  const { account } = await createWallet(page);
  const checklist = page.locator(".card.learn");
  await page.getByRole("button", { name: /Get some FANG/ }).click();
  await expect(checklist).toContainText("FANG is this chain's native unit (1 FANG = 1,000,000,000 drops).");
  await expect(checklist).toContainText("This chain has no faucet.");
  await expect(checklist).not.toContainText("Test chains have a faucet");
  const share = page.getByTestId("share-to-receive");
  await expect(share).toHaveText("Someone has to send you some: share your address from Receive.");
  await expect(share.getByRole("link", { name: "Receive" })).toHaveAttribute("href", "/receive");
  node.names.set("vlad", account);
  await expect(share).toHaveText("Someone has to send you some: share your name, vlad.boo, from Receive.");
  // Sending is signed post-quantum, not "quantum-safe", and a block is seconds away on any chain
  // (a second on the testnet, 5 seconds by default on a chain someone launches).
  await page.getByRole("button", { name: /Send FANG to a friend/ }).click();
  await expect(checklist).toContainText("Sending signs a transaction with your post-quantum ML-DSA-65 key. A validator seals it into a block seconds later, and then it's final.");
});
