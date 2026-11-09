// A fresh passkey check before every spend (lib/spend.ts): the wallet is unlocked, yet Send (and
// every other transaction that moves FANG out, or lets another key move it) asks the passkey again
// right before signing, for this wallet's credential only, with user verification required. A
// cancelled or failed check signs nothing, sends nothing and says so calmly.
import { parseFang, type Tx } from "@blockula/sdk";
import type { Page } from "@playwright/test";
import { addressOf } from "./mock-node";
import { createWallet, expect, test } from "./helpers";

const ALICE = addressOf("a1".repeat(32));

interface Get {
  uv?: string;
  allow: string[];
  challenge: string | null;
}

/** Records every navigator.credentials.get the page makes (what it asked for), from the next load on. */
const recordGets = (page: Page) =>
  page.addInitScript(() => {
    const w = window as unknown as { __gets: Get[] };
    w.__gets = [];
    const b64 = (b: BufferSource) => {
      const bytes = b instanceof ArrayBuffer ? new Uint8Array(b) : new Uint8Array(b.buffer, b.byteOffset, b.byteLength);
      return btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
    };
    const get = navigator.credentials.get.bind(navigator.credentials);
    navigator.credentials.get = (opts?: CredentialRequestOptions) => {
      const pk = opts?.publicKey;
      w.__gets.push({ uv: pk?.userVerification, allow: (pk?.allowCredentials ?? []).map((c) => b64(c.id)), challenge: pk ? b64(pk.challenge) : null });
      return get(opts);
    };
  });
const gets = (page: Page) => page.evaluate(() => (window as unknown as { __gets: Get[] }).__gets);

async function reviewPayment(page: Page) {
  await page.getByRole("link", { name: "Send", exact: true }).first().click();
  await page.getByPlaceholder(/alice\.boo or/).fill("alice.boo");
  await expect(page.getByText(/the chain pays its owner when this runs/)).toBeVisible();
  await page.getByPlaceholder("0.0").fill("1");
  await page.getByRole("button", { name: "Review" }).click();
  await expect(page.getByRole("heading", { name: "Review" })).toBeVisible();
}

test("Send asks the passkey again, with user verification, right before it signs", async ({ page, node }) => {
  await recordGets(page);
  const created = await createWallet(page);
  node.names.set("alice", ALICE);
  await reviewPayment(page);
  await expect(page.getByTestId("check-note")).toHaveText("Your passkey asks for Face ID, fingerprint or PIN before this is signed.");
  // The review is built, but nothing is signed or sent until Confirm.
  const before = (await gets(page)).length;
  expect(node.submitted).toHaveLength(0);

  await page.getByRole("button", { name: "Confirm & send" }).click();
  await expect(page.getByRole("heading", { name: "Sent" })).toBeVisible();
  const asked = (await gets(page)).slice(before);
  // One prompt: this wallet's passkey only, the user verified, a fresh challenge.
  expect(asked).toHaveLength(1);
  expect(asked[0]).toMatchObject({ uv: "required", allow: [created.credentialId] });
  expect((await gets(page)).filter((g) => g.challenge === asked[0].challenge)).toHaveLength(1);
  // Signed after the prompt, by the wallet's own key (the mock node checks the signature).
  expect(node.submitted).toHaveLength(1);
  const tx = node.submitted[0] as Tx;
  expect(tx.action).toEqual({ type: "transfer", to: "alice.boo", amount: parseFang("1").toString() });
  expect(tx.signature).not.toMatch(/^0+$/);
  expect(node.acct(ALICE).balance).toBe(parseFang("1"));
});

/** The next passkey prompt is dismissed, as the browser reports a cancelled one: NotAllowedError. */
const cancelNextPrompt = (page: Page) =>
  page.evaluate(() => {
    const get = navigator.credentials.get;
    navigator.credentials.get = () => {
      navigator.credentials.get = get;
      return Promise.reject(new DOMException("The operation either timed out or was not allowed.", "NotAllowedError"));
    };
  });

test("a cancelled passkey check sends nothing, says so calmly, and can be tried again", async ({ page, node }) => {
  await createWallet(page);
  node.names.set("alice", ALICE);
  await reviewPayment(page);

  await cancelNextPrompt(page);
  await page.getByRole("button", { name: "Confirm & send" }).click();
  await expect(page.getByTestId("check-cancelled")).toHaveText("Cancelled. Nothing was signed or sent.");
  // Calm: a status line, not an error.
  await expect(page.locator("p.error")).toHaveCount(0);
  expect(node.submitted).toHaveLength(0);
  // Still on the review, still unlocked.
  await expect(page.getByRole("heading", { name: "Review" })).toBeVisible();

  // Verified this time: it goes through, once.
  await page.getByRole("button", { name: "Confirm & send" }).click();
  await expect(page.getByRole("heading", { name: "Sent" })).toBeVisible();
  expect(node.submitted).toHaveLength(1);
  expect(node.acct(ALICE).balance).toBe(parseFang("1"));
});

test("a failed Face ID (the authenticator doesn't verify the user) sends nothing", async ({ page, node, passkeys }) => {
  await createWallet(page);
  node.names.set("alice", ALICE);
  await reviewPayment(page);
  await passkeys.verifyUser(false);
  await page.getByRole("button", { name: "Confirm & send" }).click();
  await expect(page.getByTestId("check-cancelled")).toHaveText("Cancelled. Nothing was signed or sent.");
  expect(node.submitted).toHaveLength(0);
  expect(node.acct(ALICE).balance).toBe(0n);
  await expect(page.getByRole("heading", { name: "Review" })).toBeVisible();
});

test("a passkey that derives another key isn't accepted for this wallet's spend", async ({ page, node }) => {
  await createWallet(page);
  node.names.set("alice", ALICE);
  await reviewPayment(page);
  // The prompt answers, verified, but with a PRF output that isn't this wallet's.
  await page.evaluate(() => {
    const original = PublicKeyCredential.prototype.getClientExtensionResults;
    PublicKeyCredential.prototype.getClientExtensionResults = function () {
      return { ...original.call(this), prf: { enabled: true, results: { first: new Uint8Array(32).fill(7).buffer } } };
    };
  });
  await page.getByRole("button", { name: "Confirm & send" }).click();
  await expect(page.locator("p.error")).toHaveText("That passkey doesn't open this wallet, so nothing was signed.");
  expect(node.submitted).toHaveLength(0);
});

test("the explorer's actions sign only after the passkey check, and a cancelled one signs nothing", async ({ page, node }) => {
  await recordGets(page);
  await createWallet(page);
  node.names.set("alice", ALICE);
  await page.goto("/programs/accounts?do=transfer&to=alice.boo&amount=1");
  await page.getByRole("button", { name: "Unlock to sign" }).first().click();
  await page.getByRole("button", { name: "Preview" }).click();
  // The preview is the exact transaction, unsigned until "Sign & send".
  await expect(page.locator("details.raw pre")).toContainText("3309 bytes, made when you sign");
  await cancelNextPrompt(page);
  await page.getByRole("button", { name: "Sign & send" }).click();
  await expect(page.getByTestId("check-cancelled")).toHaveText("Cancelled. Nothing was signed or sent.");
  await expect(page.getByTestId("action-preview")).toBeVisible();
  expect(node.submitted).toHaveLength(0);
  const before = (await gets(page)).length;
  await page.getByRole("button", { name: "Sign & send" }).click();
  await expect(page.locator(".receipt .headline")).toBeVisible();
  expect((await gets(page)).slice(before)).toMatchObject([{ uv: "required" }]);
  expect(node.submitted).toHaveLength(1);
  expect((node.submitted[0] as Tx).action).toEqual({ type: "transfer", to: "alice.boo", amount: parseFang("1").toString() });
});

test("claiming a name asks the passkey first, and a cancelled check claims nothing", async ({ page, node }) => {
  await recordGets(page);
  await createWallet(page);
  await page.goto("/name");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await page.getByPlaceholder("vlad").fill("ghoul");
  await expect(page.getByText(/✓ ghoul\.boo is available/)).toBeVisible();
  await cancelNextPrompt(page);
  await page.getByRole("button", { name: "Claim ghoul.boo" }).click();
  await expect(page.getByTestId("check-cancelled")).toHaveText("Cancelled. Nothing was signed or sent.");
  expect(node.submitted).toHaveLength(0);
  const before = (await gets(page)).length;
  await page.getByRole("button", { name: "Claim ghoul.boo" }).click();
  await expect(page.getByText("The account of")).toBeVisible();
  expect(node.submitted.at(-1)?.action).toEqual({ type: "register_name", name: "ghoul" });
  expect((await gets(page)).slice(before)).toMatchObject([{ uv: "required" }]);
});
