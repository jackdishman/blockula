import { parseFang } from "@blockula/sdk";
import { bech32m } from "@scure/base";
import { CHAIN_ID, GENESIS } from "./env";
import { createWallet, expect, storedHint, test } from "./helpers";
import { MockNode } from "./mock-node";

test("shows the network and chain id on every screen", async ({ page }) => {
  for (const path of ["/welcome", "/recover", "/link"]) {
    await page.goto(path);
    const badge = page.getByTestId("network-badge");
    await expect(badge).toContainText("Testnet");
    await expect(badge).toContainText(CHAIN_ID);
    // Anything but a pinned mainnet is stamped.
    await expect(badge).toHaveClass(/\btest\b/);
  }
  await createWallet(page);
  for (const path of ["/", "/devices", "/receive", "/send"]) {
    await page.goto(path);
    await page.getByRole("button", { name: "Unlock with passkey" }).click();
    await expect(page.getByTestId("network-badge")).toContainText(CHAIN_ID);
  }
});

test("refuses a node that reports another chain id", async ({ page, node }) => {
  node.chainId = "blockula-1";
  await page.goto("/welcome");
  const block = page.getByTestId("wrong-chain");
  await expect(block).toBeVisible();
  await expect(block).toContainText("blockula-1");
  await expect(block).toContainText(CHAIN_ID);
  await expect(page.getByTestId("network-badge")).toContainText("wrong chain");
  await expect(page.getByRole("button", { name: /Create wallet/ })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Skip intro" })).toHaveCount(0);
});

test("refuses a node with the right chain id but another genesis", async ({ page, node }) => {
  node.genesisHash = "00".repeat(32);
  await page.goto("/welcome");
  await expect(page.getByTestId("wrong-chain")).toContainText(`genesis ${GENESIS}`);
});

test("refuses a node whose addresses use another prefix", async ({ page, node }) => {
  node.statusExtra = { address_prefix: "tboo" };
  await page.goto("/welcome");
  await expect(page.getByTestId("wrong-chain")).toContainText("tboo1");
});

test("an account saved on another chain is looked up again, and the faucet pays this chain's", async ({ page, node }) => {
  // No faucet while onboarding, so the checklist's faucet is the one that pays below.
  node.faucet = false;
  const created = await createWallet(page);
  node.faucet = true;
  // The hint isn't kept per chain: a wallet from a testnet relaunched with another prefix still
  // names that chain's address (the same key's, tboo1… on this boo1… chain), which the node
  // refuses, down to the faucet (a 400).
  const elsewhere = bech32m.encode("tboo", bech32m.decode(created.account as `${string}1${string}`).words);
  await page.evaluate((h) => localStorage.setItem("blockula.wallet", h), JSON.stringify({ ...created, account: elsewhere }));
  await page.reload();
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(page.getByText("The account of")).toBeVisible();
  expect((await storedHint(page))?.account).toBe(created.account);
  await page.getByRole("button", { name: /Get some FANG/ }).click();
  await page.getByRole("button", { name: "Get 100 test FANG" }).click();
  await expect.poll(() => node.acct(created.account).balance).toBe(parseFang("100"));
  await expect(page.locator(".card.learn").getByRole("alert")).toHaveCount(0);
});

test("stops, and forgets the key, when the node switches chains mid-session", async ({ page, node }) => {
  await createWallet(page);
  const before = node.submitted.length;
  node.chainId = "blockula-evil";
  await expect(page.getByTestId("wrong-chain")).toBeVisible();
  await expect(page.getByText("The account of")).toHaveCount(0);
  // Even with the node back on the right chain, nothing resumes without a reload.
  node.chainId = CHAIN_ID;
  await page.waitForTimeout(3500);
  await expect(page.getByTestId("wrong-chain")).toBeVisible();
  expect(node.submitted.length).toBe(before);
});

test("lets the user leave a wrong-chain node for another network", async ({ page, context, node }) => {
  // Never reach a real local node that happens to be running on this machine.
  await context.route("http://localhost:7070/**", (route) => route.abort());
  node.chainId = "blockula-evil";
  await page.goto("/welcome");
  await page.getByRole("button", { name: "Switch to Local node" }).click();
  await expect(page.getByTestId("network-badge")).toContainText("Local node");
  expect(await page.evaluate(() => localStorage.getItem("blockula.network"))).toBe("local");
  // Nothing answers there, which the wallet says without blocking.
  await expect(page.getByText(/Can.t reach the node/)).toBeVisible();
});

test("a custom node: let through the CSP, trusted on first use, refused when its chain changes", async ({ page, context }) => {
  const url = "https://my-node.blockula.test";
  const custom = new MockNode("testnet", url);
  await custom.install(context);
  page.on("dialog", (d) => d.accept(d.type() === "prompt" ? url : undefined));
  await page.goto("/welcome");
  await page.getByRole("combobox", { name: "Network" }).selectOption("custom");
  // The page reloads with the node's origin in its CSP, and the node's chain on the badge.
  await expect(page.getByTestId("network-badge")).toContainText("Custom node");
  await expect(page.getByTestId("network-badge")).toContainText(CHAIN_ID);
  await expect(page.getByTestId("network-badge")).toHaveClass(/\bcustom\b/);
  expect((await page.request.get("/welcome")).headers()["content-security-policy"]).toContain(url);
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem("blockula.customChain") ?? "{}"))).toEqual({
    [url]: { chainId: CHAIN_ID, genesisHash: GENESIS },
  });

  // The node comes back on another chain: refused until the user says they reset it.
  custom.chainId = "my-dev-chain";
  custom.genesisHash = "11".repeat(32);
  await page.reload();
  await expect(page.getByTestId("wrong-chain")).toContainText("my-dev-chain");
  await page.getByRole("button", { name: /trust its new chain/ }).click();
  await expect(page.getByTestId("network-badge")).toContainText("my-dev-chain");
});
