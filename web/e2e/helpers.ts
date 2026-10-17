import { test as base, expect, type Page } from "@playwright/test";
import { MockNode } from "./mock-node";

/**
 * A virtual platform authenticator (like Touch ID or Windows Hello) with resident keys, user
 * verification and the PRF extension, attached to one page through the Chrome DevTools Protocol.
 */
export async function addAuthenticator(page: Page) {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("WebAuthn.enable", { enableUI: false });
  const { authenticatorId } = await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: {
      protocol: "ctap2",
      ctap2Version: "ctap2_1",
      transport: "internal",
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      hasPrf: true,
      automaticPresenceSimulation: true,
    },
  });
  return {
    credentials: async () => (await cdp.send("WebAuthn.getCredentials", { authenticatorId })).credentials,
  };
}

/** Every browser test gets a fresh mock node of its project's chain and a passkey authenticator. */
export const test = base.extend<{ node: MockNode; passkeys: Awaited<ReturnType<typeof addAuthenticator>> }>({
  node: [
    async ({ context }, use, info) => {
      const node = new MockNode(info.project.name === "mainnet" ? "mainnet" : "testnet");
      await node.install(context);
      await use(node);
    },
    { auto: true },
  ],
  passkeys: [async ({ page }, use) => use(await addAuthenticator(page)), { auto: true }],
});

export { expect };

export interface StoredHint {
  kind: "passkey" | "local";
  keyId: string;
  credentialId?: string;
  account?: string;
  pendingKit?: { keyId: string; created: string };
}

/** Records CSP violations in the page from now on; `cspViolations` reads them. */
export const watchCsp = (page: Page) =>
  page.addInitScript(() => {
    const w = window as unknown as { __csp: string[] };
    w.__csp = [];
    document.addEventListener("securitypolicyviolation", (e) => w.__csp.push(`${e.violatedDirective} ${e.blockedURI}`));
  });
export const cspViolations = (page: Page) => page.evaluate(() => (window as unknown as { __csp?: string[] }).__csp ?? []);

/** The wallet's non-secret hints in localStorage. */
export const storedHint = (page: Page) => page.evaluate(() => JSON.parse(localStorage.getItem("blockula.wallet") ?? "null")) as Promise<StoredHint | null>;

/** Everything this origin keeps in localStorage, as one string (to check no secret ended up there). */
export const allStorage = (page: Page) => page.evaluate(() => JSON.stringify({ ...localStorage }));

/**
 * Onboard a new passkey wallet through /welcome. `kit` runs at the recovery-kit step and must
 * leave it (e.g. `makeKit` + "Next: claim a name"); without it the step is skipped.
 */
export async function createWallet(page: Page, opts: { kit?: (page: Page) => Promise<unknown> } = {}) {
  await page.goto("/welcome");
  await page.getByRole("button", { name: "Skip intro" }).click();
  await page.getByRole("button", { name: "Create wallet with passkey" }).click();
  // Faucet chains show what landed first; chains without one go straight to the kit.
  const next = page.getByRole("button", { name: "Next: your recovery kit" });
  const kitHeading = page.getByRole("heading", { name: "Make a recovery kit" });
  await expect(next.or(kitHeading)).toBeVisible();
  if (await next.isVisible()) await next.click();
  await expect(kitHeading).toBeVisible();
  if (opts.kit) await opts.kit(page);
  else await page.getByRole("button", { name: /^Skip for now/ }).click();
  await expect(page.getByRole("heading", { name: "Claim your .boo name" })).toBeVisible();
  await page.getByRole("button", { name: "Skip for now" }).click();
  await page.getByRole("button", { name: "Open my wallet" }).click();
  await expect(page.getByText("The account of")).toBeVisible();
  const hint = await storedHint(page);
  expect(hint?.account).toBeTruthy();
  return hint as StoredHint & { account: string };
}

/** Make a recovery kit from its intro, store nothing, answer the two confirmation words. */
export async function makeKit(page: Page) {
  await page.getByRole("button", { name: "Make my recovery kit" }).click();
  // The kit signs its grant (its consent to the account) first, so the words appear a moment later.
  await expect(page.getByTestId("kit-words")).toBeVisible();
  const words = (await page.getByTestId("kit-words").locator(".kit-w").allTextContents()).map((w) => w.trim());
  const keyId = ((await page.getByTestId("kit-key-id").textContent()) ?? "").trim();
  expect(words).toHaveLength(24);
  expect(keyId).toMatch(/^[0-9a-f]{64}$/);
  await page.getByRole("button", { name: "I've stored my kit" }).click();
  await answerKitWords(page, words);
  return { words, keyId };
}

/** Fill in the two words the confirmation step asks for. */
export async function answerKitWords(page: Page, words: string[]) {
  const confirm = page.getByTestId("kit-confirm");
  await expect(confirm).toBeVisible();
  for (const field of await confirm.locator("label").all()) {
    const n = Number(((await field.locator("span").first().textContent()) ?? "").match(/#(\d+)/)?.[1]);
    await field.locator("input").fill(words[n - 1]);
  }
  await confirm.getByRole("button", { name: "Confirm" }).click();
}
