import { readFileSync } from "node:fs";
import { keyId as keyIdOf, Wallet } from "@blockula/sdk";
import { hexToBytes } from "@noble/hashes/utils.js";
import type { Page } from "@playwright/test";
import { KIT_LABEL, wordsToSeed } from "../lib/recovery";
import { addAuthenticator, allStorage, answerKitWords, createWallet, expect, makeKit, storedHint, test } from "./helpers";
import type { MockNode } from "./mock-node";

/** Onboard with a recovery kit; returns the wallet's hint and the kit. */
async function walletWithKit(page: Page) {
  let kit = { words: [] as string[], keyId: "" };
  const hint = await createWallet(page, {
    kit: async (p) => {
      kit = await makeKit(p);
      await expect(p.getByTestId("kit-done")).toBeVisible();
      await p.getByRole("button", { name: "Next: claim a name" }).click();
    },
  });
  return { hint, kit };
}

/** No secret left the page: not in any request, not in storage, not on screen. */
async function expectNoLeak(page: Page, node: MockNode, words: string[]) {
  const phrase = words.join(" ");
  const seedHex = Buffer.from(wordsToSeed(phrase)).toString("hex");
  for (const body of node.bodies) {
    expect(body).not.toContain(seedHex);
    expect(body).not.toContain(words.slice(0, 3).join(" "));
  }
  const stored = await allStorage(page);
  expect(stored).not.toContain(seedHex);
  expect(stored).not.toContain(words.slice(0, 3).join(" "));
  await expect(page.getByTestId("kit-words")).toHaveCount(0);
  expect(await page.locator("body").innerText()).not.toContain(words.slice(0, 4).join(" "));
}

test("makes a recovery kit during onboarding: print, download, confirm, activate", async ({ page, node }) => {
  await page.addInitScript(() => {
    window.print = () => {
      (window as unknown as { __printed: number }).__printed = ((window as unknown as { __printed?: number }).__printed ?? 0) + 1;
    };
  });
  await createWallet(page, {
    kit: async (p) => {
      await p.getByRole("button", { name: "Make my recovery kit" }).click();
      await expect(p.getByTestId("kit-words")).toBeVisible();
      const words = (await p.getByTestId("kit-words").locator(".kit-w").allTextContents()).map((w) => w.trim());
      const keyId = ((await p.getByTestId("kit-key-id").textContent()) ?? "").trim();
      // The 24 words are the kit key's seed.
      expect(Wallet.fromSeed(wordsToSeed(words.join(" "))).keyId).toBe(keyId);

      await p.getByRole("button", { name: "Print" }).click();
      expect(await p.evaluate(() => (window as unknown as { __printed?: number }).__printed)).toBe(1);

      const [download] = await Promise.all([p.waitForEvent("download"), p.getByRole("button", { name: "Download (.txt)" }).click()]);
      expect(download.suggestedFilename()).toMatch(/^blockula-recovery-kit-.+\.txt$/);
      const text = readFileSync((await download.path()) as string, "utf8");
      expect(text).toContain("BLOCKULA RECOVERY KIT");
      expect(text).toContain(keyId);
      expect(text).toContain("chain blockula-testnet");
      for (const [i, w] of words.entries()) expect(text).toMatch(new RegExp(`\\b${i + 1}\\. ${w}\\b`));

      // A wrong word is caught; the right ones activate the kit.
      await p.getByRole("button", { name: "I've stored my kit" }).click();
      const confirm = p.getByTestId("kit-confirm");
      for (const input of await confirm.locator("input").all()) await input.fill("zoo");
      if (!words.includes("zoo")) {
        await confirm.getByRole("button", { name: "Confirm" }).click();
        await expect(confirm.getByRole("alert")).toContainText("isn't word #");
      }
      await answerKitWords(p, words);
      await expect(p.getByTestId("kit-done")).toBeVisible();
      await expectNoLeak(p, node, words);

      const hint = await storedHint(p);
      const add = node.submitted.at(-1);
      expect(add?.action).toMatchObject({ type: "add_key", key_id: keyId, label: KIT_LABEL, role: { type: "owner" } });
      // The kit key's consent to this account rode along (the mock node checks it, as nodes do).
      expect(add?.action.type === "add_key" && keyIdOf(hexToBytes(add.action.public_key))).toBe(keyId);
      expect(add?.from).toBe(hint?.account);
      expect(hint?.pendingKit).toBeUndefined();
      await p.getByRole("button", { name: "Next: claim a name" }).click();
    },
  });
  // The kit shows on Devices & agents, separate from devices.
  await page.goto("/devices");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(page.locator("#recovery")).toContainText(KIT_LABEL);
});

test("restores on a new device with the kit, then revokes the lost device", async ({ page, node, browser }) => {
  const { hint: lost, kit } = await walletWithKit(page);

  // A new device: another browser profile with its own authenticator, on the same chain.
  const context = await browser.newContext();
  await node.install(context);
  const fresh = await context.newPage();
  await addAuthenticator(fresh);
  await fresh.goto("/welcome");
  await fresh.getByRole("button", { name: "Skip intro" }).click();
  await fresh.getByRole("link", { name: "Restore with a recovery kit" }).click();

  // People write kits down numbered, one per line, and may shorten words to four letters.
  const typed = kit.words.map((w, i) => `${i + 1}. ${i % 5 === 0 && w.length > 4 ? w.slice(0, 4) : w}`).join("\n");
  await fresh.getByTestId("kit-input").fill(typed);
  await fresh.getByRole("button", { name: "Find my account" }).click();
  await expect(fresh.getByRole("heading", { name: /^Found / })).toBeVisible();
  await expect(fresh.getByTestId("kit-input")).toHaveCount(0);
  await fresh.getByRole("button", { name: "Create a passkey for this device" }).click();
  await expect(fresh.getByTestId("restore-done")).toBeVisible();

  // The kit's key added the new passkey as an owner of the same account.
  const restored = await storedHint(fresh);
  expect(restored?.account).toBe(lost.account);
  expect(restored?.keyId).not.toBe(lost.keyId);
  const add = node.submitted.at(-1);
  expect(add?.action).toMatchObject({ type: "add_key", key_id: restored?.keyId, role: { type: "owner" } });
  expect(keyIdOf(hexToBytes(add?.public_key ?? ""))).toBe(kit.keyId);
  expect(node.keysOf(lost.account).get(restored?.keyId as string)?.role.type).toBe("owner");

  // Revoke the lost device, signed by the new passkey.
  const lostRow = fresh.locator(".key-row", { hasText: "original key" });
  await lostRow.getByRole("button", { name: "Revoke" }).click();
  await expect(lostRow).toHaveCount(0);
  expect(node.submitted.at(-1)?.action).toEqual({ type: "remove_key", key_id: lost.keyId });
  expect(node.keysOf(lost.account).has(lost.keyId)).toBe(false);
  await expectNoLeak(fresh, node, kit.words);

  await fresh.getByRole("button", { name: "Open my wallet" }).click();
  await expect(fresh.getByText("The account of")).toBeVisible();

  // The new device's passkey opens the account on its own after a reload.
  await fresh.reload();
  await fresh.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(fresh.getByText("The account of")).toBeVisible();
  expect((await storedHint(fresh))?.account).toBe(lost.account);
  await context.close();
});

test("rejects mistyped kits before touching the network", async ({ page, node }) => {
  await page.goto("/recover");
  const input = page.getByTestId("kit-input");
  const find = page.getByRole("button", { name: "Find my account" });
  const words = Array(23).fill("abandon");
  await input.fill(words.join(" "));
  await expect(page.getByText("23 of 24 words")).toBeVisible();
  await expect(find).toBeDisabled();
  await input.fill([...words, "abandon"].join(" "));
  await expect(page.getByText(/don.t add up/)).toBeVisible();
  await expect(find).toBeDisabled();
  await input.fill([...words, "vampire"].join(" "));
  await expect(page.getByText("Not on the word list: vampire")).toBeVisible();
  // A valid kit that isn't on any account says so.
  await input.fill([...words, "art"].join(" "));
  await find.click();
  await expect(page.getByText(/This kit isn't an owner of any account on Testnet/)).toBeVisible();
  expect(node.submitted).toHaveLength(0);
});

test("keeps a kit pending on a chain without a faucet, and activates it later", async ({ page, node }) => {
  node.faucet = false;
  let kit = { words: [] as string[], keyId: "" };
  const hint = await createWallet(page, {
    kit: async (p) => {
      kit = await makeKit(p);
      await expect(p.getByTestId("kit-pending")).toBeVisible();
      await p.getByRole("button", { name: "Next: claim a name" }).click();
    },
  });
  expect((await storedHint(page))?.pendingKit?.keyId).toBe(kit.keyId);
  await expect(page.getByText("Your recovery kit isn't active yet.")).toBeVisible();

  node.credit(hint.account, "1");
  await page.goto("/devices#recovery");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await page.getByRole("button", { name: "Activate my kit" }).click();
  await expect(page.locator("#recovery .key-row", { hasText: KIT_LABEL })).toBeVisible();
  expect(node.submitted.at(-1)?.action).toMatchObject({ type: "add_key", key_id: kit.keyId, label: KIT_LABEL });
  expect((await storedHint(page))?.pendingKit).toBeUndefined();
});
