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

// Audit AGENT-5: an agent can hand the user a link (`/devices#grant=<code>`) instead of a 7,000-
// character code to copy. The fragment fills in the grant, never reaching a server; the wallet
// shows the check code to compare with the agent's, and that the agent is the user's
// responsibility (legal-safety-10). Labels go on-chain for good, so they say so (legal-safety-11).
test("a #grant= link fills in the agent's grant code and shows its check code", async ({ page, node }) => {
  const created = await createWallet(page);
  const agent = Wallet.generate();
  const code = encodeKeyGrant(await agent.keyGrant(created.account, { chainId: CHAIN_ID, genesisHash: GENESIS }));
  await page.goto(`/devices#grant=${encodeURIComponent(code)}`);
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  const card = page.locator("#agent");
  await expect(card.getByPlaceholder("bkgrant1.…")).toHaveValue(code);
  await expect(card.getByTestId("agent-check")).toHaveText(`Check code: ${keyCode(agent.keyId)} (your agent shows the same)`);
  await expect(card.getByTestId("agent-from-link")).toContainText("Allow it only if the check code matches the one your agent shows you.");
  await expect(card.getByTestId("agent-warning")).toHaveText(
    "You're responsible for what your agent sends. Within this limit its payments are final, like yours. AI agents make mistakes and can be tricked by text they read (memos, issues, web pages). Start small, and revoke the key here when you're done.",
  );
  await expect(card.getByLabel(/^Label/)).toHaveAttribute("placeholder", "claude");
  await expect(card.locator(".onb-public")).toHaveText("(public and permanent)");
  // A new device's name is public too: nothing filled in that could name its owner.
  const device = page.locator("#device");
  const name = device.getByLabel(/^Name it/);
  await expect(name).toHaveValue("");
  await expect(name).toHaveAttribute("placeholder", "Laptop");
  await expect(device.locator(".onb-public")).toHaveText("(public and permanent: it's written on the chain)");

  await card.getByRole("button", { name: "Allow 10 FANG/day" }).click();
  await expect(card.locator("pre.snippet")).toBeVisible();
  expect(node.submitted.at(-1)?.action).toMatchObject({ type: "add_key", key_id: agent.keyId, label: "claude", role: { type: "session" } });

  // A code for another account isn't taken from a link either.
  const stranger = Wallet.generate();
  const other = encodeKeyGrant(await stranger.keyGrant(stranger.address, { chainId: CHAIN_ID, genesisHash: GENESIS }));
  // (A full load: a goto that only changes the fragment wouldn't be one.)
  await page.goto("/");
  await page.goto(`/devices#grant=${encodeURIComponent(other)}`);
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(card.getByText(/This code is for another account/)).toBeVisible();
  await expect(card.getByTestId("agent-check")).toHaveCount(0);
  await expect(card.getByRole("button", { name: "Allow 10 FANG/day" })).toBeDisabled();
});
