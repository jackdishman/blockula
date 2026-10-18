import { readFileSync } from "node:fs";
import path from "node:path";
import { addressOf } from "./mock-node";
import { createWallet, expect, storedHint, test } from "./helpers";

/** The SDK's derivation vectors (sdk-ts/test/passkey-vectors.json), shared with its own test. */
const VECTORS = JSON.parse(readFileSync(path.join(__dirname, "../../sdk-ts/test/passkey-vectors.json"), "utf8")) as {
  vectors: { prf: string; key_id: string; address: string }[];
};

test("creates a passkey wallet whose address is stable across reloads and unlocks", async ({ page, node, passkeys }) => {
  const created = await createWallet(page);
  expect(created.kind).toBe("passkey");
  expect(created.credentialId).toBeTruthy();
  // A fresh passkey wallet is its own account: the address is the key's.
  expect(created.account).toBe(addressOf(created.keyId));
  expect(node.acct(created.account).balance).toBeGreaterThan(0n);
  // One resident credential, verified by the user, in the authenticator.
  const creds = await passkeys.credentials();
  expect(creds).toHaveLength(1);
  expect(creds[0].isResidentCredential).toBe(true);

  // Reload: the key is gone from memory and the passkey re-derives the same one.
  await page.reload();
  await expect(page.getByRole("heading", { name: "Welcome back" })).toBeVisible();
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(page.getByText("The account of")).toBeVisible();
  expect(await storedHint(page)).toMatchObject({ keyId: created.keyId, account: created.account });

  // Lock and unlock again.
  await page.getByRole("button", { name: "Lock" }).click();
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(page.getByText("The account of")).toBeVisible();
  expect((await storedHint(page))?.keyId).toBe(created.keyId);
});

test("opens the same wallet from the passkey alone when this browser has forgotten it", async ({ page }) => {
  const created = await createWallet(page);
  await page.evaluate(() => localStorage.clear());
  await page.goto("/welcome");
  await page.getByRole("button", { name: "I already have a wallet" }).click();
  await page.getByRole("button", { name: /^Open it with my passkey/ }).click();
  await expect(page.getByText("The account of")).toBeVisible();
  expect(await storedHint(page)).toMatchObject({ keyId: created.keyId, account: created.account });
});

test("derives the SDK's vector wallets from fixed PRF outputs in the browser", async ({ page }) => {
  const v = VECTORS.vectors[1];
  // Stand-in for an authenticator whose PRF output is known: rewrite what the browser reports.
  await page.addInitScript((prfHex) => {
    const prf = new Uint8Array(prfHex.match(/../g)!.map((h) => parseInt(h, 16)));
    const original = PublicKeyCredential.prototype.getClientExtensionResults;
    PublicKeyCredential.prototype.getClientExtensionResults = function () {
      return { ...original.call(this), prf: { enabled: true, results: { first: prf.slice().buffer } } };
    };
  }, v.prf);
  const created = await createWallet(page);
  expect(created.keyId).toBe(v.key_id);
  expect(created.account).toBe(v.address);
  await expect(page.getByText(`${v.address.slice(0, 10)}…${v.address.slice(-6)}`)).toBeVisible();
});

test("explains a passkey provider without PRF", async ({ page }) => {
  await page.addInitScript(() => {
    PublicKeyCredential.prototype.getClientExtensionResults = () => ({});
  });
  await page.goto("/welcome");
  await page.getByRole("button", { name: "Skip intro" }).click();
  await page.getByRole("button", { name: "Create wallet with passkey" }).click();
  await expect(page.getByRole("heading", { name: /can.t unlock a wallet/ })).toBeVisible();
  // Testnet: the browser-only fallback is offered (it never is on mainnet).
  await expect(page.getByRole("button", { name: /browser-only key/ })).toBeVisible();
});
