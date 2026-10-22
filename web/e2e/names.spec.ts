// Payments to a .boo name (audit gap-7-1): without witnesses the wallet signs the name itself, so
// the chain pays whoever owns it when the transfer runs, whatever the node said the owner was.
import { parseFang, type Tx } from "@blockula/sdk";
import { RPC } from "./env";
import { addressOf } from "./mock-node";
import { createWallet, expect, test } from "./helpers";

test("a payment to a name signs the name, so a lying node can't point it at another account", async ({ page, node }) => {
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
  // bob has no account yet: opening it burns the creation fee, and the total says so (audit main13-protocol-2).
  await expect(page.getByTestId("opening")).toHaveText(/^0\.1 FANG/);
  await expect(page.locator("dd.total")).toHaveText(/^2\.1000/);
  await page.getByRole("button", { name: "Confirm & send" }).click();
  await expect(page.getByRole("heading", { name: "Sent" })).toBeVisible();
  expect((node.submitted.at(-1) as Tx).action).toEqual({ type: "transfer", to: bob, amount: parseFang("2").toString() });
});

// Audit review-r3-clients-2: a tx's validity window counts from the height the node reports, so a
// node reporting one the chain can't have reached could hold a signed payment and release it later,
// after the name it pays has changed hands. The wallet checks that height against the chain's
// genesis clock (read once and remembered with the genesis hash) before anything is signed.
test("a node reporting a height the chain can't have reached gets nothing signed", async ({ page, node }) => {
  const alice = addressOf("a1".repeat(32));
  node.names.set("alice", alice);
  node.genesis = { chain_id: node.chainId, created_at_ms: Date.now() - 86_400_000, block_time_ms: 1000, validators: [] };
  await createWallet(page);
  const review = async () => {
    await page.getByPlaceholder(/alice\.boo or/).fill("alice.boo");
    await page.getByPlaceholder("0.0").fill("1");
    await page.getByRole("button", { name: "Review" }).click();
  };
  const pay = async () => {
    await page.getByRole("link", { name: "Send", exact: true }).first().click();
    await review();
  };
  // A height the chain can have reached (a day of 1 s blocks): signed.
  await pay();
  await page.getByRole("button", { name: "Confirm & send" }).click();
  await expect(page.getByRole("heading", { name: "Sent" })).toBeVisible();
  const sent = node.submitted.length;
  await page.getByRole("link", { name: "Done" }).click();
  // A year of blocks ahead: refused before anything is signed.
  node.height += 31_536_000;
  await pay();
  await expect(page.getByText("Your node reports a block height the network can't have reached yet, so nothing was signed")).toBeVisible();
  await expect(page.getByRole("button", { name: "Confirm & send" })).toHaveCount(0);
  // The clock is remembered: a genesis served later that would lift the bound changes nothing.
  node.genesis = { chain_id: node.chainId, created_at_ms: 0, block_time_ms: 1, validators: [] };
  await page.reload();
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(page.getByRole("heading", { name: "Send FANG" })).toBeVisible();
  await review();
  await expect(page.getByText("Your node reports a block height the network can't have reached yet")).toBeVisible();
  expect(node.submitted.length).toBe(sent);
});

// Audit USER-12 / legal-safety-10: name prices are governance parameters, so the claim screen
// shows the node's price and renewal price for the name typed (never a table of its own), says a
// registration lasts a year, and what a name is and isn't.
test("a name's price, renewal price and term come from the node", async ({ page, node }) => {
  await createWallet(page);
  // The node prices names its own way (as after a governance change): 7.5 FANG, then 1.25 a year, 14 days' grace.
  await page.route(`${RPC}/api/name/*`, (route) => {
    const name = decodeURIComponent(new URL(route.request().url()).pathname.split("/").pop() ?? "").replace(/\.boo$/, "");
    return route.fulfill({
      headers: { "access-control-allow-origin": "*" },
      json: { name: `${name}.boo`, status: "available", available: true, price: "7500000000", price_display: "7.5 FANG", renewal_price: "1250000000", renewal_price_display: "1.25 FANG", registration_blocks: 31_536_000, grace_blocks: 1_209_600 },
    });
  });
  await page.goto("/name");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(page.getByTestId("name-price")).toHaveText("Shorter names cost more. Type one to see its price.");
  await page.getByPlaceholder("vlad").fill("ghoul");
  await expect(page.getByText("✓ ghoul.boo is available · 7.5 FANG")).toBeVisible();
  await expect(page.getByTestId("name-price")).toHaveText("7.5 FANG for one year, then 1.25 FANG a year. The price goes to the community treasury and isn't refunded.");
  const terms = page.getByTestId("name-terms");
  await expect(terms).toContainText("Names are public and go to whoever asks first.");
  await expect(terms).toContainText("They aren't internet (ICANN) domains, give you no trademark or other rights, and mustn't impersonate anyone.");
  await expect(terms).toContainText("14 days after it expires, anyone can register it.");
  await expect(page.locator("main")).not.toContainText("3 letters 100 FANG");
  await page.getByRole("button", { name: "Claim ghoul.boo" }).click();
  await expect(page.getByText("The account of")).toBeVisible();
  expect(node.submitted.at(-1)?.action).toEqual({ type: "register_name", name: "ghoul" });
});

test("short of FANG for a name: the faucet is mentioned only on a chain that has one", async ({ page, node }) => {
  node.faucet = false;
  const { account } = await createWallet(page);
  node.credit(account, "1");
  await page.goto("/name");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await page.getByPlaceholder("vlad").fill("ghoul");
  await expect(page.getByText("You need 5 FANG plus a small fee.")).toBeVisible();
  await expect(page.locator("main")).not.toContainText(/faucet/i);
  await expect(page.getByRole("button", { name: "Claim ghoul.boo" })).toBeDisabled();
  // A faucet chain: the shortfall points at it.
  node.faucet = true;
  await page.reload();
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await page.getByPlaceholder("vlad").fill("ghoul");
  await expect(page.getByText("You need 5 FANG plus a small fee. Grab test FANG from the faucet first.")).toBeVisible();
});
