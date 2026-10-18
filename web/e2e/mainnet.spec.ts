// The mainnet build (NEXT_PUBLIC_NETWORK=mainnet, see playwright.config.ts): pinned chain and
// genesis, a plain badge, no browser-only keys, and a build that refuses a borrowed domain.
import { spawnSync } from "node:child_process";
import path from "node:path";
import { MAINNET_GENESIS, MAINNET_RPC } from "./env";
import { Wallet, encodeKeyGrant, parseFang } from "@blockula/sdk";
import { addressOf } from "./mock-node";
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

test("the footer doesn't call mainnet a test chain", async ({ page }) => {
  await page.goto("/welcome");
  await expect(page.getByTestId("imprint-note")).toHaveText(/Self-custodial/);
  await expect(page.locator("footer")).not.toContainText("test chains");
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

// Audit gap-7-1: linking a device signs its consent to one address. On mainnet without witnesses
// a name is the node's word, so the address waits for the user to check it on their other device.
test("linking by name on mainnet waits for the address to be checked", async ({ page, node }) => {
  const account = addressOf("a1".repeat(32));
  node.names.set("alice", account);
  await page.goto("/link");
  await page.getByRole("button", { name: "Create a passkey for this device" }).click();
  await page.getByPlaceholder("yourname.boo or boo1…").fill("alice.boo");
  await page.getByRole("button", { name: "Make this device's grant code" }).click();
  const check = page.getByTestId("check-address");
  await expect(check).toContainText(account);
  const go = check.getByRole("button", { name: "It matches: make the grant code" });
  await expect(go).toBeDisabled();
  await expect(page.getByText("This device's grant code")).toHaveCount(0);
  await check.getByRole("checkbox").check();
  await go.click();
  await expect(page.getByText("This device's grant code")).toBeVisible();
  await expect(page.getByRole("heading", { name: "Almost there" })).toBeVisible();
});

// Audit main13-signing-2: the explorer's own "Unlock to sign" keeps the wallet's rule.
test("the explorer's inline unlock won't open a browser-only key either", async ({ page }) => {
  await page.goto("/welcome");
  await page.evaluate(() =>
    localStorage.setItem("blockula.wallet", JSON.stringify({ v: 1, kind: "local", keyId: "11".repeat(32), seed: "22".repeat(32), account: "boo1x" })),
  );
  await page.goto("/programs/names?do=register_name&name=ghoul");
  await page.getByRole("button", { name: "Unlock to sign" }).click();
  await expect(page.getByText("Browser-only keys are disabled on mainnet.", { exact: false })).toBeVisible();
  await expect(page.getByRole("button", { name: "Preview" })).toHaveCount(0);
});

// Audit main13-signing-1: a field that needs an address (staking's validator) never signs a name
// only one node vouched for on mainnet; fields the chain resolves itself sign the name.
test("explorer forms on mainnet: an unproven name is refused where an address is needed, and signed as the name elsewhere", async ({ page, node }) => {
  node.faucet = false;
  const hint = await createWallet(page);
  node.credit(hint.account, "100");
  const validator = addressOf("7a".repeat(32));
  node.names.set("fangs", validator);
  await page.goto("/programs/validators?do=delegate&validator=fangs.boo&amount=2");
  await page.getByRole("button", { name: "Unlock to sign" }).click();
  await page.getByRole("button", { name: "Preview" }).click();
  await expect(page.getByText(/fangs\.boo resolves to .* only by .*'s word \(unproven/)).toBeVisible();
  await expect(page.getByTestId("action-preview")).toHaveCount(0);
  const before = node.submitted.length;
  // The address itself signs.
  const stake = page.locator("details.action[open]");
  await stake.getByLabel("Validator", { exact: true }).fill(validator);
  await stake.getByRole("button", { name: "Preview" }).click();
  await expect(stake.getByTestId("action-preview")).toBeVisible();
  // A payment to a name is signed as the name.
  const alice = addressOf("a1".repeat(32));
  node.names.set("alice", alice);
  node.nameLies.set("alice", addressOf("66".repeat(32)));
  await page.goto("/programs/accounts?do=transfer&to=alice.boo&amount=1");
  await page.getByRole("button", { name: "Unlock to sign" }).click();
  await page.getByRole("button", { name: "Preview" }).click();
  await expect(page.getByTestId("signed-name")).toHaveText("alice.boo");
  await page.getByRole("button", { name: "Sign & send" }).click();
  await expect(page.locator(".receipt .headline")).toBeVisible();
  expect(node.submitted.slice(before).map((t) => t.action)).toEqual([{ type: "transfer", to: "alice.boo", amount: "1000000000" }]);
  expect(node.accounts.get(alice)?.balance).toBe(1_000_000_000n);
});

// Audit review-r3-clients-4: the agent is told the account's address (the wallet's own, not a
// node's answer), with the name only as a label: on mainnet an agent won't act for a name one
// node vouched for (the SDK's actAs throws, the MCP's use_account asks to confirm the address).
test("an agent's budget is set up by the account's address, its name only a label", async ({ page, node }) => {
  node.faucet = false;
  const hint = await createWallet(page);
  node.names.set("alice", hint.account);
  node.accounts.set(hint.account, { balance: parseFang("10"), nonce: 0 });
  await page.goto("/devices#agent");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  const agentSection = page.locator("#agent");
  await expect(page.getByTestId("agent-account")).toHaveText(hint.account);
  await expect(agentSection).toContainText("(alice.boo)");
  const agent = Wallet.fromSeed("ab".repeat(32));
  const grant = await agent.keyGrant(hint.account, { chainId: "blockula-1", genesisHash: MAINNET_GENESIS });
  await agentSection.getByPlaceholder("bkgrant1.…").fill(encodeKeyGrant(grant));
  await agentSection.getByRole("button", { name: "Allow 10 FANG/day" }).click();
  const snippet = agentSection.locator("pre.snippet");
  await expect(snippet).toContainText(`Your Blockula key may now spend from alice.boo (${hint.account})`);
  await expect(snippet).toContainText(`call use_account with account "${hint.account}"`);
  await expect(snippet).toContainText(`await wallet.actAs("${hint.account}");`);
  await expect(snippet).not.toContainText('"alice.boo"');
});
