// Calling a deployed program from its page (components/explorer/ProgramCalls.tsx), against the mock node
// with programs on (e2e/explorer-world.ts `addCallable`): views run signed out and show what they return
// as text, never markup; a call is reviewed (its program, method, input, value, storage limit, the full
// version digest it binds, its fuel and the most it can cost), then signed after the passkey check, and its
// summary, outcome and return value come back; a call that traps for want of a storage limit says so and
// was charged; one the chain refuses because the program changed costs nothing; an agent key and a shared
// account are told why they can't call. All under the production build's strict CSP.
import { encodeProgramInput, versionDigest, type Tx } from "@blockula/sdk";
import type { Locator, Page } from "@playwright/test";
import { addCallable, CALLABLE, HOSTILE_GREETING } from "./explorer-world";
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";
import { addressOf } from "./mock-node";

const CREATOR = addressOf("c1".repeat(32));
const F = 10n ** 9n;

/** Errors the page logs, and node reads that failed (none are expected here), from now on. */
function watchErrors(page: Page) {
  const errors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error" && !/^Failed to load resource/.test(m.text())) errors.push(`console: ${m.text()}`);
  });
  page.on("pageerror", (e) => errors.push(`pageerror: ${e.message}`));
  page.on("response", (r) => {
    // Multisig reads answer 404 for an account that isn't shared: that's how the page learns it.
    if (r.status() >= 400 && !/\/api\/multisig\//.test(r.url())) errors.push(`${r.status()} ${r.url()}`);
  });
  return errors;
}

/** Nothing a stranger wrote ran, and the page broke none of the CSP's rules. */
async function clean(page: Page, errors: string[]) {
  expect(await cspViolations(page)).toEqual([]);
  expect(errors).toEqual([]);
  expect(await page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned)).toBeUndefined();
  await expect(page.locator("main img[src='x']")).toHaveCount(0);
  await expect(page.locator('a[href^="javascript:" i]')).toHaveCount(0);
}

/** A method's form on the page, opened. */
async function method(page: Page, kind: "view" | "call", name: string): Promise<Locator> {
  const f = page.getByTestId(`program-${kind}`).filter({ has: page.locator("summary code.named", { hasText: new RegExp(`^${name}$`) }) });
  await expect(f).toHaveCount(1);
  if ((await f.getAttribute("open")) == null) await f.locator("summary").click();
  return f;
}

test.describe("calling a deployed program", () => {
  let errors: string[];
  test.beforeEach(async ({ page }) => {
    errors = watchErrors(page);
    await watchCsp(page);
  });

  test("signed out, its views run and show what they return as text; its calls say to sign in", async ({ page, node }) => {
    addCallable(node, CREATOR);
    await page.goto(`/program/${CALLABLE}`);
    await expect(page.getByTestId("program-calls")).toBeVisible();

    const count = await method(page, "view", "count");
    await count.getByRole("button", { name: "Run" }).click();
    await expect(count.getByTestId("view-value")).toHaveText("1");
    await expect(count.getByTestId("view-raw")).toHaveText("0100000000000000");

    // A stranger's text: markup stays text, and what hides or reorders text is taken out, and said.
    const greeting = await method(page, "view", "greeting");
    await greeting.getByRole("button", { name: "Run" }).click();
    const value = greeting.getByTestId("view-value");
    await expect(value.locator("code.named")).toContainText('Welcome! <img src=x onerror="window.__pwned=7">');
    await expect(value).toContainText("hidden characters taken out");
    expect(await value.locator("code.named").textContent()).not.toMatch(/[‮‬\u0007]/);
    await expect(greeting.getByTestId("view-result")).toContainText("the program's claim");

    // Typed arguments, encoded by the interface's types; an option comes back as the value or none.
    const lookup = await method(page, "view", "lookup");
    await lookup.getByLabel("label", { exact: true }).fill("darkoh");
    await lookup.getByRole("button", { name: "Run" }).click();
    await expect(lookup.getByTestId("view-value")).toHaveText(CREATOR);
    await lookup.getByLabel("label", { exact: true }).fill("nobody");
    await lookup.getByRole("button", { name: "Run" }).click();
    await expect(lookup.getByTestId("view-value")).toHaveText("none");
    // Text is bounded in bytes: 33 bytes don't fit a text(32), and nothing is sent.
    await lookup.getByLabel("label", { exact: true }).fill("é".repeat(17));
    await lookup.getByRole("button", { name: "Run" }).click();
    await expect(lookup.getByTestId("arg-arg0")).toContainText("34 of 32 bytes");
    await expect(lookup.getByTestId("arg-arg0").locator(".warn")).toBeVisible();

    const register = await method(page, "call", "register");
    await expect(register.getByTestId("call-sign-in")).toContainText("Sign in or create a wallet");
    await expect(register.getByRole("button", { name: "Review" })).toHaveCount(0);
    await expect(register.getByTestId("storage-note")).toContainText("At 0 it may write nothing new");
    await clean(page, errors);
    expect(node.submitted).toEqual([]);
  });

  test("an owner reviews a call, with the full digest it binds, signs it, and sees its summary, outcome and return value", async ({ page, node }) => {
    const { account: me } = await createWallet(page);
    const { digest } = addCallable(node, CREATOR);
    await page.goto(`/program/${CALLABLE}`);
    const register = await method(page, "call", "register");
    await register.getByRole("button", { name: "Unlock to call it" }).click();
    await register.getByLabel("label", { exact: true }).fill("Mina");
    await register.getByLabel("handle", { exact: true }).fill("mina_x");
    // A list, an item at a time; an address field takes an address, never a name, and offers your own in one click.
    await register.getByRole("button", { name: /^Add an item/ }).click();
    await register.getByLabel("friends, item 1", { exact: true }).fill("lucy.boo");
    await register.getByRole("button", { name: "Review" }).click();
    await expect(register.getByTestId("arg-arg2")).toContainText("never a .boo name");
    await expect(register.getByTestId("call-review")).toHaveCount(0);
    await register.getByRole("button", { name: "Use my address" }).click();
    await expect(register.getByLabel("friends, item 1", { exact: true })).toHaveValue(me);

    // Storage limit left at 0: the review says what that means, and the call traps in its block, charged.
    await register.getByRole("button", { name: "Review" }).click();
    const review = register.getByTestId("call-review");
    await expect(review).toBeVisible();
    const input = encodeProgramInput([{ type: { text: 32 } }, { type: { option: { text: 32 } } }, { type: { vec: ["address", 3] } }], ["Mina", { some: "mina_x" }, [me]]);
    await expect(review.getByTestId("review-program")).toHaveText(CALLABLE);
    await expect(review.getByTestId("review-input")).toHaveText(input);
    await expect(review.getByTestId("review-args")).toContainText("Mina");
    await expect(review.getByTestId("review-args")).toContainText(me);
    await expect(review.getByTestId("review-digest")).toHaveText(digest);
    await expect(review.getByTestId("review-storage")).toContainText("no new storage");
    await expect(review.getByTestId("review-fuel")).toContainText("10,058,000 reserved");
    await expect(review.getByTestId("review-fee")).toContainText("fuel included");
    await expect(review.getByTestId("review-value")).toHaveCount(0);
    const balance = node.acct(me).balance;
    await review.getByRole("button", { name: "Sign & send" }).click();
    const outcome = register.getByTestId("call-outcome");
    await expect(outcome).toContainText("It ran and failed");
    await expect(outcome.getByTestId("call-words")).toContainText("trapped: storage limit");
    await expect(outcome.getByTestId("call-words")).toContainText("Its fee was charged");
    const trapped = node.submitted.at(-1) as Tx;
    expect(node.acct(me).balance).toBe(balance - BigInt(trapped.fee));

    // Back to the form (never "send it again"): a storage limit this time.
    await outcome.getByRole("button", { name: "Back to the form" }).click();
    await register.getByLabel("Storage limit", { exact: true }).fill("0.01");
    await register.getByRole("button", { name: "Review" }).click();
    await expect(review.getByTestId("review-storage")).toContainText("bonded as deposit");
    await expect(review.getByTestId("review-total")).toContainText(/0\.01\d*/);
    await review.getByRole("button", { name: "Sign & send" }).click();
    await expect(outcome).toContainText("It ran");
    await expect(outcome.getByTestId("call-summary")).toContainText("called program");
    await expect(outcome.getByTestId("call-words")).toHaveText("it applied");
    await expect(outcome.getByTestId("call-returned")).toContainText("2");
    await expect(outcome.getByTestId("call-returned")).toContainText("the program's claim");
    const signed = node.submitted.at(-1) as Tx;
    expect(signed.action).toMatchObject({ type: "call_program", storage_limit: "10000000", bindings: [], calls: [{ program: CALLABLE, version_digest: digest, method: "register", input, value: "0" }] });
    await clean(page, errors);

    // A payable method takes a value; the review counts it in the total.
    const tip = await method(page, "call", "tip");
    await tip.getByLabel("note", { exact: true }).fill("for the castle");
    await tip.getByLabel("Value", { exact: true }).fill("1.5");
    await tip.getByRole("button", { name: "Review" }).click();
    const tipReview = tip.getByTestId("call-review");
    await expect(tipReview.getByTestId("review-value")).toContainText("1.5");
    await expect(tipReview.getByTestId("review-total")).toContainText("1.5");
    await tipReview.getByRole("button", { name: "Sign & send" }).click();
    await expect(tip.getByTestId("call-outcome").getByTestId("call-words")).toHaveText("it applied");
    expect(node.programs.get(CALLABLE)?.balance).toBe((15n * F / 10n).toString());

    // The receipt is a page of its own.
    await tip.getByTestId("call-tx").click();
    await expect(page.locator(".receipt .headline")).toBeVisible();
    await clean(page, errors);
  });

  test("a call to a program that changed since its review is refused by the chain, and costs nothing", async ({ page, node }) => {
    const { account: me } = await createWallet(page);
    const { wasm_hash, interface_hash } = addCallable(node, CREATOR);
    await page.goto(`/program/${CALLABLE}`);
    const tip = await method(page, "call", "tip");
    await tip.getByRole("button", { name: "Unlock to call it" }).click();
    await tip.getByLabel("note", { exact: true }).fill("hi");
    await tip.getByRole("button", { name: "Review" }).click();
    await expect(tip.getByTestId("call-review")).toBeVisible();
    // An upgrade lands between the review and the signature: the call binds the digest that was shown.
    const p = node.programs.get(CALLABLE) as Record<string, unknown>;
    node.programs.set(CALLABLE, { ...p, version: 2, version_digest: versionDigest(CALLABLE, wasm_hash, interface_hash, 2) });
    const balance = node.acct(me).balance;
    await tip.getByRole("button", { name: "Sign & send" }).click();
    const outcome = tip.getByTestId("call-outcome");
    await expect(outcome).toContainText("Refused");
    await expect(outcome).toContainText("nothing was charged");
    await expect(outcome.getByTestId("call-words")).toContainText("binds version digest");
    expect(node.acct(me).balance).toBe(balance);
    expect(node.submitted).toHaveLength(0);
  });

  test("an agent key and a shared account are told why they can't call, and views still run", async ({ page, node }) => {
    const { account: me, keyId } = await createWallet(page);
    addCallable(node, CREATOR);
    const owner = "0e".repeat(32);
    node.acct(me).keys = new Map([
      [owner, { label: "Vlad's laptop", role: { type: "owner" as const }, added_at: 1 }],
      [keyId, { label: "claude", role: { type: "session" as const, limit: (10n * F).toString(), period_blocks: 86_400, expires_at: 2_000_000 }, added_at: 2 }],
    ]);
    await page.goto(`/program/${CALLABLE}`);
    const register = await method(page, "call", "register");
    await register.getByRole("button", { name: "Unlock to call it" }).click();
    await expect(register.getByTestId("call-blocked")).toContainText("agent key (“claude”)");
    await expect(register.getByTestId("call-blocked")).toContainText("a program call needs an owner key");
    await expect(register.getByRole("button", { name: "Review" })).toHaveCount(0);
    const count = await method(page, "view", "count");
    await count.getByRole("button", { name: "Run" }).click();
    await expect(count.getByTestId("view-value")).toHaveText("1");

    // The same account made shared: its owner keys only propose and approve.
    node.acct(me).keys = new Map([
      [keyId, { label: "this browser", role: { type: "owner" as const }, added_at: 1 }],
      [owner, { label: "Vlad's laptop", role: { type: "owner" as const }, added_at: 1 }],
    ]);
    node.multisigs.set(me, { account: node.label(me), threshold: 2, owners: [{ key_id: keyId, label: "this browser" }, { key_id: owner, label: "Vlad's laptop" }], created_at: 1, pending: [] });
    await page.reload();
    const again = await method(page, "call", "register");
    await again.getByRole("button", { name: "Unlock to call it" }).click();
    await expect(again.getByTestId("call-blocked")).toContainText("shared account (2 of 2 owners approve its actions)");
    await expect(again.getByRole("button", { name: "Review" })).toHaveCount(0);
    expect(node.submitted).toHaveLength(0);
    await clean(page, errors);
  });

  test("on a chain without programs, the review says so in the SDK's words and signs nothing", async ({ page, node }) => {
    await createWallet(page);
    addCallable(node, CREATOR);
    node.fuel = null;
    await page.goto(`/program/${CALLABLE}`);
    const tip = await method(page, "call", "tip");
    await tip.getByRole("button", { name: "Unlock to call it" }).click();
    await tip.getByLabel("note", { exact: true }).fill("hi");
    await tip.getByRole("button", { name: "Review" }).click();
    await expect(tip.getByRole("alert")).toContainText("programs aren't on");
    await expect(tip.getByRole("alert")).toContainText("Nothing was signed");
    await expect(tip.getByTestId("call-review")).toHaveCount(0);
    expect(node.submitted).toHaveLength(0);
  });
});

test("a value typed as text keeps the program's text out of the page's own words", async ({ page, node }) => {
  // The hostile greeting also comes back as an abort message: it's shown as the program's words, in a box.
  const { account: me } = await createWallet(page);
  addCallable(node, CREATOR);
  await page.goto(`/program/${CALLABLE}`);
  const register = await method(page, "call", "register");
  await register.getByRole("button", { name: "Unlock to call it" }).click();
  await register.getByLabel("label", { exact: true }).fill("darkoh");
  await register.getByLabel("Storage limit", { exact: true }).fill("0.01");
  await register.getByRole("button", { name: "Review" }).click();
  await register.getByTestId("call-review").getByRole("button", { name: "Sign & send" }).click();
  const outcome = register.getByTestId("call-outcome");
  await expect(outcome.getByTestId("call-words")).toContainText("aborted with code 1");
  await expect(outcome.getByTestId("call-words")).not.toContainText("Welcome");
  await expect(outcome.locator("code.named")).toContainText("taken Welcome!");
  expect(await page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned)).toBeUndefined();
  await expect(page.locator("main img[src='x']")).toHaveCount(0);
  expect(node.acct(me).balance).toBeLessThan(100n * F);
});
