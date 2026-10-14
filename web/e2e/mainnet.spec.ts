// The mainnet build (NEXT_PUBLIC_NETWORK=mainnet, see playwright.config.ts): pinned chain and
// genesis, a plain badge, no browser-only keys, and a build that refuses a borrowed domain.
import { spawnSync } from "node:child_process";
import path from "node:path";
import { MAINNET_GENESIS, MAINNET_RPC } from "./env";
import { createWallet, expect, test } from "./helpers";

test("defaults to mainnet, pinned to blockula-1, with a plain badge", async ({ page, request }) => {
  await page.goto("/welcome");
  const badge = page.getByTestId("network-badge");
  await expect(badge).toContainText("Mainnet");
  await expect(badge).toContainText("blockula-1");
  await expect(badge).toHaveClass(/\bmain\b/);
  // Mainnet and a local node; the testnet isn't offered unless configured.
  await expect(page.getByRole("combobox", { name: "Network" }).locator("option")).toHaveText(["Mainnet", "Local node", "Custom node…"]);
  const csp = (await request.get("/welcome")).headers()["content-security-policy"];
  expect(csp).toContain(MAINNET_RPC);
  expect(csp).not.toContain("blockula-testnet.fly.dev");
});

test("creates passkey wallets on mainnet", async ({ page, node }) => {
  node.faucet = false;
  const hint = await createWallet(page);
  expect(hint.kind).toBe("passkey");
  expect(hint.account).toMatch(/^boo1/);
});

test("refuses a node claiming blockula-1 from another genesis", async ({ page, node }) => {
  node.genesisHash = "00".repeat(32);
  await page.goto("/welcome");
  await expect(page.getByTestId("wrong-chain")).toContainText(MAINNET_GENESIS);
});

test("never offers a browser-only key", async ({ page }) => {
  await page.addInitScript(() => {
    PublicKeyCredential.prototype.getClientExtensionResults = () => ({});
  });
  await page.goto("/welcome");
  await page.getByRole("button", { name: "Skip intro" }).click();
  await page.getByRole("button", { name: "Create wallet with passkey" }).click();
  await expect(page.getByRole("heading", { name: /can.t unlock a wallet/ })).toBeVisible();
  await expect(page.getByRole("button", { name: /browser-only key/ })).toHaveCount(0);
  await page.goto("/link");
  await page.getByRole("button", { name: "Create a passkey for this device" }).click();
  await expect(page.getByText(/doesn.t support/)).toBeVisible();
  await expect(page.getByRole("button", { name: /browser-only key/ })).toHaveCount(0);
});

test("won't open a browser-only key left in storage", async ({ page }) => {
  await page.goto("/welcome");
  await page.evaluate(() =>
    localStorage.setItem("blockula.wallet", JSON.stringify({ v: 1, kind: "local", keyId: "11".repeat(32), seed: "22".repeat(32), account: "boo1x" })),
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Unlock browser-only key" }).click();
  await expect(page.getByText("Browser-only keys are disabled on mainnet.", { exact: false })).toBeVisible();
  await expect(page.getByText("The account of")).toHaveCount(0);
});

test("a mainnet build refuses an rpId it doesn't own, and missing pins", () => {
  const load = (env: Record<string, string>) =>
    spawnSync("npx", ["tsx", "-e", 'import("./next.config.ts").then(() => console.log("ok"), (e) => { console.error(e.message); process.exit(1); })'], {
      cwd: path.join(__dirname, ".."),
      env: { ...process.env, NEXT_PUBLIC_RP_ID: "", NEXT_PUBLIC_MAINNET_RPC: "", NEXT_PUBLIC_MAINNET_GENESIS_HASH: "", NEXT_PUBLIC_NETWORK: "mainnet", ...env },
      encoding: "utf8",
    });
  const good = { NEXT_PUBLIC_RP_ID: "wallet.blockula.example", NEXT_PUBLIC_MAINNET_RPC: MAINNET_RPC, NEXT_PUBLIC_MAINNET_GENESIS_HASH: MAINNET_GENESIS };
  expect(load(good).status).toBe(0);
  const unset = load({ ...good, NEXT_PUBLIC_RP_ID: "" });
  expect(unset.status).toBe(1);
  expect(unset.stderr).toContain("NEXT_PUBLIC_RP_ID is unset");
  for (const host of ["blockula-wallet.vercel.app", "wallet.pages.dev"]) {
    const borrowed = load({ ...good, NEXT_PUBLIC_RP_ID: host });
    expect(borrowed.status).toBe(1);
    expect(borrowed.stderr).toContain("shared hosting domain");
  }
  expect(load({ ...good, NEXT_PUBLIC_MAINNET_GENESIS_HASH: "" }).stderr).toContain("NEXT_PUBLIC_MAINNET_GENESIS_HASH");
  expect(load({ ...good, NEXT_PUBLIC_MAINNET_RPC: "" }).stderr).toContain("NEXT_PUBLIC_MAINNET_RPC is unset");
  // Test chains build without any of it.
  expect(load({ NEXT_PUBLIC_NETWORK: "" }).status).toBe(0);
});
