// Creators (product review, wallet-creators): a Pay button on public pages, GitHub links that say
// when they're unverified, multisig accounts shown as what they are (their pending actions from
// the actions themselves, never the node's words), memo hygiene, a print-safe payment link, name
// terms, and the contribution pages' notes, bond terms and timeline. The mock node doesn't serve
// /api/github: it's mocked here with page.route.
import { actionDigest, describeAction, type Action } from "@blockula/sdk";
import type { Page, Route } from "@playwright/test";
import { ADDR, DUST_MEMO, HOSTILE, PHISH_MEMO, seedWorld } from "./explorer-world";
import { RPC } from "./env";
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";

/** Node reads that answer 404 on purpose: most accounts aren't a multisig. */
const EXPECTED_404 = [/\/api\/multisig\//];

/** Errors the page logs, and node reads that failed unexpectedly, from now on. */
function watchErrors(page: Page) {
  const errors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error" && !/^Failed to load resource/.test(m.text())) errors.push(`console: ${m.text()}`);
  });
  page.on("pageerror", (e) => errors.push(`pageerror: ${e.message}`));
  page.on("response", (r) => {
    if (r.status() >= 400 && !EXPECTED_404.some((re) => re.test(r.url()))) errors.push(`${r.status()} ${r.url()}`);
  });
  return errors;
}

/** `/api/github/{handle}` as node/src/api.rs answers it, from `checks`; counts what was asked. */
async function mockGithub(page: Page, checks: Record<string, { account: string; verified: boolean; reason: string }>) {
  const asked: string[] = [];
  await page.route(`${RPC}/api/github/**`, (route: Route) => {
    const handle = decodeURIComponent(new URL(route.request().url()).pathname.split("/").pop() ?? "");
    asked.push(handle);
    const c = checks[handle.toLowerCase()];
    const cors = { "access-control-allow-origin": "*" };
    if (!c) return route.fulfill({ status: 404, contentType: "application/json", headers: cors, body: JSON.stringify({ error: "GitHub link not found" }) });
    const body = { handle, account: { address: c.account, name: null, short: c.account.slice(0, 10) }, gist_id: "abc", gist_url: "https://gist.github.com/abc", linked_at: 130, verified: c.verified, reason: c.reason, attested_by: "this node (off-chain check; validators re-check it for every contribution claim)" };
    return route.fulfill({ status: 200, contentType: "application/json", headers: cors, body: JSON.stringify(body) });
  });
  return asked;
}

test.describe("a creator's public page", () => {
  let errors: string[];
  test.beforeEach(async ({ page }) => {
    errors = watchErrors(page);
    await watchCsp(page);
  });

  test("a visitor without a wallet gets a Pay button", async ({ page, node }) => {
    seedWorld(node);
    await page.goto("/account/mina.boo");
    const pay = page.getByTestId("pay");
    await expect(pay).toHaveText("Pay mina.boo");
    await expect(pay).toHaveAttribute("href", "/send?to=mina.boo");
    // An unnamed account is paid by its address; a system account isn't offered at all.
    await page.goto(`/account/${ADDR.stranger}`);
    await expect(page.getByTestId("pay")).toHaveAttribute("href", `/send?to=${ADDR.stranger}`);
    await page.goto(`/account/${ADDR.pool}`);
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    await expect(page.getByTestId("pay")).toHaveCount(0);
    expect(await cspViolations(page)).toEqual([]);
    expect(errors).toEqual([]);
  });

  test("a linked GitHub handle is 'unverified' unless the node verified it, and it's checked once a session", async ({ page, node }) => {
    const w = seedWorld(node);
    node.accountExtra.set(w.mina, { ...node.accountExtra.get(w.mina), github: { handle: "mina-draws", gist_id: "abc", linked_at: 130, verify_url: "/api/github/mina-draws" } });
    node.accountExtra.set(w.lucy, { ...node.accountExtra.get(w.lucy), github: { handle: "torvalds", gist_id: "def", linked_at: 131, verify_url: "/api/github/torvalds" } });
    const asked = await mockGithub(page, {
      "mina-draws": { account: w.mina, verified: true, reason: "gist owned by the handle contains the account's address" },
      torvalds: { account: w.lucy, verified: false, reason: "gist is owned by @lucy, not @torvalds" },
    });

    // Verified: the chip, and no pill.
    await page.goto("/account/mina.boo");
    const chip = page.getByTestId("github");
    await expect(chip).toContainText("github · @mina-draws");
    await expect.poll(() => asked.filter((h) => h === "mina-draws").length).toBe(1);
    await expect(chip.getByTestId("github-unverified")).toHaveCount(0);

    // Unverified: the pill, with the node's reason on hover.
    await page.goto("/account/lucy.boo");
    const pill = page.getByTestId("github").getByTestId("github-unverified");
    await expect(pill).toHaveText("unverified");
    await expect(pill).toHaveAttribute("title", /^Unverified: gist is owned by @lucy, not @torvalds\./);

    // A handle the chain couldn't have accepted isn't sent to the node at all.
    await page.goto("/account/vlad.boo");
    await expect(page.getByTestId("github").getByTestId("github-unverified")).toHaveAttribute("title", /not a GitHub handle the chain accepts/);

    // Back to mina.boo, after a reload too: the session's answer, not another request.
    await page.goto("/account/mina.boo");
    await expect(page.getByTestId("github")).toContainText("github · @mina-draws");
    await page.reload();
    await expect(page.getByTestId("github")).toContainText("github · @mina-draws");
    await expect(page.getByTestId("github").getByTestId("github-unverified")).toHaveCount(0);
    expect(asked.filter((h) => h === "mina-draws")).toHaveLength(1);
    expect(asked.filter((h) => h === "torvalds")).toHaveLength(1);
    expect(asked.some((h) => h.startsWith("vlad"))).toBe(false);
    expect(await cspViolations(page)).toEqual([]);
    expect(errors).toEqual([]);
  });

  test("a 2-of-2 multisig shows its threshold, and its pending action from the action, not the node's summary", async ({ page, node }) => {
    seedWorld(node);
    const action = { type: "transfer", to: ADDR.mina, amount: "2000000000" } as Action;
    await page.goto("/account/coven.boo");
    await expect(page.getByTestId("multisig-chip")).toHaveText("2-of-2 multisig");
    await expect(page.getByTestId("multisig-rule")).toHaveText("Both owners must approve every action.");
    await expect(page.getByText("Owners can do anything")).toHaveCount(0);

    const pending = page.getByTestId("ms-action");
    await expect(pending).toHaveCount(1);
    await expect(pending.getByTestId("ms-does")).toHaveText(describeAction(action));
    await expect(pending.getByTestId("ms-does")).toHaveText(`Send 2 FANG to ${ADDR.mina}`);
    await expect(pending).toContainText("1 of 2 approvals");
    await expect(pending).toContainText("approved by the other witch");
    await expect(pending.getByTestId("ms-digest")).toHaveText(`digest ${actionDigest(action)}`);
    await expect(pending).toContainText(`blockula multisig approve 0 --from ${ADDR.coven} --digest ${actionDigest(action)}`);
    // The node's own words (hostile ones here) never show.
    await expect(page.locator("main")).not.toContainText("would send");
    await expect(page.locator("main")).not.toContainText(HOSTILE);
    await expect(page.getByText("Passkey owners can't approve in the wallet yet")).toBeVisible();

    // An ordinary account is no multisig.
    await page.goto("/account/vlad.boo");
    await expect(page.getByRole("heading", { name: "vlad.boo" })).toBeVisible();
    await expect(page.getByText("Owners can do anything")).toBeVisible();
    await expect(page.getByTestId("multisig-chip")).toHaveCount(0);
    expect(await cspViolations(page)).toEqual([]);
    expect(errors).toEqual([]);
  });

  test("a dust payment's memo is folded until 'show'; contact-looking memos are flagged; no memo is a link", async ({ page, node }) => {
    const w = seedWorld(node);
    await page.goto("/account/mina.boo");
    const dust = page.locator(".ledger li", { has: page.locator(`a[href="/tx/${w.ids.dust}"]`) });
    await expect(dust.getByTestId("memo-folded")).toHaveText("memo hidden (tiny payment) · show");
    await expect(dust).not.toContainText("URGENT");
    await dust.getByRole("button", { name: "show" }).click();
    // Showing it doesn't open the transaction.
    await expect(page).toHaveURL(/\/account\/mina\.boo$/);
    await expect(dust.locator(".ledger-memo")).toHaveText(`“${DUST_MEMO}”`);
    await expect(dust.getByTestId("memo-warning")).toHaveText("Blockula never contacts you in a memo");

    // A real payment's memo isn't folded, but reads like a contact, so it's flagged.
    const phish = page.locator(".ledger li", { has: page.locator(`a[href="/tx/${w.ids.phish}"]`) });
    await expect(phish.locator(".ledger-memo")).toHaveText(`“${PHISH_MEMO}”`);
    await expect(phish.getByTestId("memo-warning")).toBeVisible();
    // An ordinary memo is just a memo.
    const tip = page.locator(".ledger li", { has: page.locator(`a[href="/tx/${w.ids.sent}"]`) });
    await expect(tip.locator(".ledger-memo")).toHaveText("“for the castle repairs”");
    await expect(tip.getByTestId("memo-warning")).toHaveCount(0);
    // Links in memos stay text: the only links are the rows' own.
    await expect(page.locator('a[href*="blockula-renew"], a[href*="blockula-help"]')).toHaveCount(0);
    await expect(page.locator(".ledger-memo a")).toHaveCount(0);

    // Keyboard works too.
    await page.reload();
    const show = dust.getByRole("button", { name: "show" });
    await show.focus();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/account\/mina\.boo$/);
    await expect(dust.locator(".ledger-memo")).toBeVisible();
    expect(await cspViolations(page)).toEqual([]);
    expect(errors).toEqual([]);
  });
});

test("receive: memos are public, and a payment link for print carries the address, not the name", async ({ page, node }) => {
  await watchCsp(page);
  const { account } = await createWallet(page);
  node.names.set("poet", account);
  await page.getByRole("link", { name: "Receive" }).first().click();
  const link = page.getByTestId("paylink");
  await expect(link).toContainText("/send?to=poet.boo", { timeout: 10_000 });
  await expect(page.getByRole("img", { name: "QR code: payment link to poet.boo" })).toBeVisible();
  await expect(page.getByTestId("public-memos")).toHaveText("Payments and memos are public and permanent; anyone can send you one.");
  // The toggle is a small box with its words beside it, inside its row (inputs are 100% wide by
  // default, and a full-width checkbox pushed the words out of the card).
  const [box, words, row] = await Promise.all([page.getByTestId("for-print").boundingBox(), page.locator(".cre-print > span").boundingBox(), page.locator(".cre-print").boundingBox()]);
  expect(box!.width).toBeLessThan(40);
  expect(words!.x + words!.width).toBeLessThanOrEqual(row!.x + row!.width + 1);

  await page.getByTestId("for-print").check();
  await expect(link).toContainText(`/send?to=${account}&chain=`);
  await expect(link).not.toContainText("poet");
  await expect(page.getByRole("img", { name: `QR code: payment link to ${account}` })).toBeVisible();
  await expect(page.getByText("they pay you even if your name lapses")).toBeVisible();
  // With an amount, still the address.
  await page.getByPlaceholder("Payer chooses").fill("3");
  await expect(link).toContainText(`/send?to=${account}&amount=3&chain=`);
  await page.getByTestId("for-print").uncheck();
  await expect(link).toContainText("/send?to=poet.boo&amount=3");
  expect(await cspViolations(page)).toEqual([]);
});

test("a name says its price, its renewal and roughly when it expires, and its owner can renew it from there", async ({ page, node }) => {
  await watchCsp(page);
  const { account } = await createWallet(page);
  node.names.set("poet", account);
  seedWorld(node);
  // Someone else's: the terms, no renew.
  await page.goto("/name/mina");
  const terms = page.getByTestId("name-terms");
  await expect(terms).toContainText(/^5 FANG a year to register · renews at 1 FANG a year · expires around [A-Z][a-z]{2} \d{1,2}, \d{4}$/);
  await expect(terms.getByRole("button", { name: "Renew it" })).toHaveCount(0);
  // One's own: "Renew it" opens the renew form.
  await page.goto("/name/poet");
  await expect(page.getByRole("heading", { name: "You own this name" })).toBeVisible();
  const renew = page.locator("details#do-renew_name");
  await expect(renew).not.toHaveAttribute("open");
  await page.getByTestId("name-terms").getByRole("button", { name: "Renew it" }).click();
  await expect(renew).toHaveAttribute("open");
  await expect(renew.locator("summary .title")).toHaveText("Renew poet.boo");
  expect(await cspViolations(page)).toEqual([]);
});

test.describe("contribution pages", () => {
  let errors: string[];
  test.beforeEach(async ({ page, node }) => {
    seedWorld(node);
    errors = watchErrors(page);
    await watchCsp(page);
  });

  test("carry the testnet note, the bond's terms and a claim's timeline", async ({ page }) => {
    const note = "Testnet FANG, Hunts and Soul have no monetary value and come with no promise of any mainnet allocation.";
    for (const path of ["/hunt/0", "/claim/0", "/seance/1"]) {
      await page.goto(path);
      await expect(page.getByTestId("contribution-note"), path).toHaveText(note);
    }

    // Pending: the bond is held on terms, and the timeline waits on validators.
    await page.goto("/claim/0");
    await expect(page.getByTestId("claim-bond")).toHaveText("1 FANG, held: returned unless validators refute the claim or the jury marks it spam");
    const timeline = page.getByTestId("claim-timeline");
    await expect(timeline.locator("li")).toHaveCount(5);
    await expect(timeline.locator("li.now")).toContainText("Merge checked by validators");
    await expect(timeline).toContainText("if they don't settle it by then, the bond comes back");
    await expect(timeline).not.toContainText("refundable");

    // Scored and paid in Séance #1, closed at #135: it vests for the pool's 600 blocks.
    await page.goto("/claim/3");
    await expect(page.getByTestId("claim-bond")).toHaveText("returned");
    const steps = page.getByTestId("claim-timeline").locator("li");
    await expect(steps.nth(3)).toContainText("Séance #1: judged, closed at block #135");
    await expect(steps.nth(4)).toContainText("Reward fully vested");
    await expect(steps.nth(4)).toContainText("block #735");
    await page.getByTestId("claim-timeline").getByRole("link", { name: "Séance #1" }).click();
    await expect(page.getByText("Closed at block #210")).toBeVisible();

    // Spam: the bond went to the pool, and there's no reward.
    await page.goto("/claim/4");
    await expect(page.getByTestId("claim-bond")).toHaveText("went to the pool: the jury marked it spam");
    await expect(page.getByTestId("claim-timeline").locator("li", { hasText: "Reward fully vested" })).toHaveClass(/skipped/);
    expect(await cspViolations(page)).toEqual([]);
    expect(errors).toEqual([]);
  });

  test("program pages say the same, and that passkey owners can't approve a multisig yet", async ({ page }) => {
    await page.goto("/programs/pool");
    await page.locator("summary", { hasText: "Claim a merged PR" }).click();
    await expect(page.locator("details.action[open]")).toContainText("A bond is held until it's judged: it's returned unless validators refute the claim or the jury marks it spam.");
    await expect(page.locator(".card-note", { hasText: "On the testnet: Testnet FANG, Hunts and Soul have no monetary value" })).toBeVisible();
    await expect(page.locator("main")).not.toContainText("refundable bond is held");
    await page.goto("/programs/hunts");
    await expect(page.locator(".card-note", { hasText: "On mainnet: Blockula runs Hunts and the contribution pool on the testnet for now." })).toBeVisible();
    await page.goto("/programs/accounts");
    await expect(page.locator(".card-note", { hasText: "Passkey owners can't approve in the wallet yet" })).toBeVisible();
    await expect(page.locator("main")).not.toContainText(/contract/i);
    expect(await cspViolations(page)).toEqual([]);
    expect(errors).toEqual([]);
  });
});
