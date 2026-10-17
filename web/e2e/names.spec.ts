// Payments to a .boo name (audit gap-7-1): without witnesses the wallet signs the name itself, so
// the chain pays whoever owns it when the transfer runs, whatever the node said the owner was.
import { parseFang, type Tx } from "@blockula/sdk";
import { addressOf } from "./mock-node";
import { createWallet, expect, test } from "./helpers";

test("a payment to a name signs the name, so a lying node can't redirect it", async ({ page, node }) => {
  const alice = addressOf("a1".repeat(32));
  const mallory = addressOf("66".repeat(32));
  node.names.set("alice", alice);
  // The node answers /api/name/alice with Mallory's address (the round-2 reviewer's bypass: the
  // wallet used to sign that address).
  node.nameLies.set("alice", mallory);
  await createWallet(page);
  await page.getByRole("link", { name: "Send", exact: true }).first().click();
  await page.getByPlaceholder(/alice\.boo or/).fill("alice.boo");
  await expect(page.getByText(/the chain pays its owner when this runs · your node says/)).toBeVisible();
  await page.getByPlaceholder("0.0").fill("1");
  await page.getByRole("button", { name: "Review" }).click();
  await expect(page.getByTestId("signed-to")).toHaveText("alice.boo");
  await expect(page.getByTestId("by-chain")).toContainText("Signed as the name: the chain pays whoever owns alice.boo when this transaction runs");
  await expect(page.getByTestId("by-chain")).toContainText(mallory);
  await page.getByRole("button", { name: "Confirm & send" }).click();
  await expect(page.getByRole("heading", { name: "Sent" })).toBeVisible();
  const tx = node.submitted.at(-1) as Tx;
  expect(tx.action).toEqual({ type: "transfer", to: "alice.boo", amount: parseFang("1").toString() });
  expect(node.accounts.get(alice)?.balance).toBe(parseFang("1"));
  expect(node.accounts.get(mallory)?.balance ?? 0n).toBe(0n);
});

test("a payment to an address signs that address", async ({ page, node }) => {
  const bob = addressOf("b0".repeat(32));
  await createWallet(page);
  await page.getByRole("link", { name: "Send", exact: true }).first().click();
  await page.getByPlaceholder(/alice\.boo or/).fill(bob);
  await page.getByPlaceholder("0.0").fill("2");
  await page.getByRole("button", { name: "Review" }).click();
  await expect(page.getByTestId("signed-to")).toHaveText(bob);
  await expect(page.getByTestId("by-chain")).toHaveCount(0);
  await page.getByRole("button", { name: "Confirm & send" }).click();
  await expect(page.getByRole("heading", { name: "Sent" })).toBeVisible();
  expect((node.submitted.at(-1) as Tx).action).toEqual({ type: "transfer", to: bob, amount: parseFang("2").toString() });
});
