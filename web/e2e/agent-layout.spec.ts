import { encodeKeyGrant, Wallet } from "@blockula/sdk";
import { keyCode } from "../lib/format";
import { CHAIN_ID, GENESIS } from "./env";
import { createWallet, expect, test } from "./helpers";

test("agent authorization stays within the viewport and copies the full instructions", async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
  const created = await createWallet(page);
  const agent = Wallet.generate();
  const code = encodeKeyGrant(await agent.keyGrant(created.account, { chainId: CHAIN_ID, genesisHash: GENESIS }));

  await page.goto("/devices");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  const card = page.locator("#agent");
  await card.getByPlaceholder("bkgrant1.…").fill(code);
  await card.getByLabel("Daily limit").fill("30");
  await card.getByRole("button", { name: "Allow 30 FANG/day" }).click();
  const snippet = card.locator("pre.snippet");
  await expect(snippet).toBeVisible();
  const instructions = await snippet.textContent();
  expect(instructions).toContain(`MCP:  call use_account with account "${created.account}"`);
  expect(instructions).toContain(`await wallet.actAs("${created.account}");`);

  // Long addresses must wrap visually, not push the page sideways or be clipped.
  for (const width of [320, 390, 430, 768, 1280]) {
    await page.setViewportSize({ width, height: 844 });
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth), `${width}px page overflow`).toBeLessThanOrEqual(1);
    const box = await snippet.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(width + 1);
    expect(await snippet.evaluate((el) => el.scrollWidth - el.clientWidth), `${width}px snippet overflow`).toBeLessThanOrEqual(1);
    if (width === 390) {
      const screenshot = testInfo.outputPath("agent-budget-mobile.png");
      await card.screenshot({ path: screenshot });
      await testInfo.attach("agent-budget-mobile", { path: screenshot, contentType: "image/png" });
    }
  }

  // CSS wrapping must not insert newlines into the address or change the copied text.
  await card.getByRole("button", { name: "Copy for your agent" }).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(instructions);
});

// Audit AGENT-5: the wallet shows the check code of a pasted grant to compare with the agent's, and
// that the agent is the user's responsibility (legal-safety-10). Labels go on-chain for good, so
// they say so (legal-safety-11).
test("a pasted grant code shows its check code, the agent's responsibility and the public-label hints", async ({ page, node }) => {
  const created = await createWallet(page);
  const agent = Wallet.generate();
  const code = encodeKeyGrant(await agent.keyGrant(created.account, { chainId: CHAIN_ID, genesisHash: GENESIS }));
  await page.goto("/devices");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  const card = page.locator("#agent");
  await card.getByPlaceholder("bkgrant1.…").fill(code);
  await expect(card.getByTestId("agent-check")).toHaveText(`Check code: ${keyCode(agent.keyId)} (the start and end of your agent's key_id)`);
  await expect(card.getByTestId("agent-warning")).toHaveText(
    "You're responsible for what your agent sends. Within this limit its payments are final, like yours. AI agents make mistakes and can be tricked by text they read (memos, issues, web pages). Start small, and revoke the key here when you're done.",
  );
  await expect(card.getByLabel(/^Label/)).toHaveAttribute("placeholder", "claude");
  await expect(card.locator(".onb-public")).toHaveText("(public and permanent)");
  // A 1-second chain: a day is a day.
  await expect(card.getByLabel("Daily limit")).toHaveValue("10");
  await expect(card.locator("p", { hasText: "You'll sign:" })).toContainText("every 86,400 blocks (about 1 day), transfers only, for the next 2,592,000 blocks (about 30 days)");
  await expect(card.locator(".tip")).toContainText("resets every 86,400 blocks (about 1 day)");
  // A new device's name is public too: nothing filled in that could name its owner.
  const device = page.locator("#device");
  const name = device.getByLabel(/^Name it/);
  await expect(name).toHaveValue("");
  await expect(name).toHaveAttribute("placeholder", "Laptop");
  await expect(device.locator(".onb-public")).toHaveText("(public and permanent: it's written on the chain)");

  await card.getByRole("button", { name: "Allow 10 FANG/day" }).click();
  await expect(card.locator("pre.snippet")).toBeVisible();
  expect(node.submitted.at(-1)?.action).toMatchObject({ type: "add_key", key_id: agent.keyId, label: "claude", role: { type: "session", period_blocks: 86_400 } });

  // A code for another account isn't taken either.
  const stranger = Wallet.generate();
  const other = encodeKeyGrant(await stranger.keyGrant(stranger.address, { chainId: CHAIN_ID, genesisHash: GENESIS }));
  await card.getByPlaceholder("bkgrant1.…").fill(other);
  await expect(card.getByText(/This code is for another account/)).toBeVisible();
  await expect(card.getByTestId("agent-check")).toHaveCount(0);
  await expect(card.getByRole("button", { name: "Allow 10 FANG/day" })).toBeDisabled();
});

// Review WOK-1: approving a grant adds a spending key to the account, and anyone can make a grant
// for an address they know. So a link never fills one in: the user pastes the code their own agent
// gave them. (A `#grant=` receiver would turn that into one click on whatever a link carried.)
test("a #grant= link fills in nothing: a grant is pasted, never carried by a link", async ({ page, node }) => {
  const created = await createWallet(page);
  const agent = Wallet.generate();
  const code = encodeKeyGrant(await agent.keyGrant(created.account, { chainId: CHAIN_ID, genesisHash: GENESIS }));
  const submitted = node.submitted.length;
  await page.goto(`/devices#grant=${encodeURIComponent(code)}`);
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  const card = page.locator("#agent");
  await expect(card.getByRole("heading", { name: "Give an agent a budget" })).toBeVisible();
  // Let a receiver's effect run (it would on mount) before saying nothing happened.
  await page.waitForTimeout(500);
  await expect(card.getByPlaceholder("bkgrant1.…")).toHaveValue("");
  await expect(card.getByTestId("agent-check")).toHaveCount(0);
  await expect(card.getByTestId("agent-from-link")).toHaveCount(0);
  await expect(card.getByRole("button", { name: "Allow 10 FANG/day" })).toBeDisabled();
  expect(node.submitted.length).toBe(submitted);
});

// Review WOK-5: the budget is signed in blocks (86,400 a period), a day only at 1-second blocks. On
// a chain `blockula summon` made with its default 5-second blocks the card says what the blocks come to.
test("the agent card says what its period is on a 5-second chain, and signs the same blocks", async ({ page, node }) => {
  const created = await createWallet(page);
  node.statusExtra = { block_time_ms: 5000 };
  const agent = Wallet.generate();
  const code = encodeKeyGrant(await agent.keyGrant(created.account, { chainId: CHAIN_ID, genesisHash: GENESIS }));
  await page.goto("/devices");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  const card = page.locator("#agent");
  await expect(card.getByText("a limit per 5 days and an expiry date")).toBeVisible();
  await expect(card.getByLabel("Limit per 5 days")).toHaveValue("10");
  await expect(card.getByLabel("Daily limit")).toHaveCount(0);
  await expect(card.locator(".suffix", { hasText: "× 5 days" })).toBeVisible();
  await card.getByPlaceholder("bkgrant1.…").fill(code);
  await expect(card.locator("p", { hasText: "You'll sign:" })).toContainText("every 86,400 blocks (about 5 days), transfers only, for the next 2,592,000 blocks (about 150 days)");
  await expect(card.locator(".tip")).toContainText("resets every 86,400 blocks (about 5 days)");
  await expect(card).not.toContainText(/FANG\/day|Daily|a day|per day/);
  await card.getByRole("button", { name: "Allow 10 FANG per 5 days" }).click();
  const snippet = card.locator("pre.snippet");
  await expect(snippet).toContainText("up to 10 FANG per 5 days, sends only, for 150 days");
  // What's signed is the same blocks as on any chain.
  const action = node.submitted.at(-1)?.action as { role: { period_blocks: number; expires_at: number } };
  expect(action).toMatchObject({ type: "add_key", key_id: agent.keyId, role: { type: "session", period_blocks: 86_400 } });
  expect(action.role.expires_at - node.height).toBeGreaterThan(30 * 86_400 - 100);
  expect(action.role.expires_at - node.height).toBeLessThan(30 * 86_400 + 100);
});
