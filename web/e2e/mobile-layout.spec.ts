// Every route fits a 390 x 844 phone with no sideways scroll (USER-7: /governance was 536 px wide,
// a proposal's "does" text carrying an unbreakable bech32 address). Checked against the mock node
// filled with a little of everything (e2e/explorer-world.ts), so long addresses, memos and
// amounts are on screen, plus the review steps before money moves.
import type { Page } from "@playwright/test";
import { parseFang } from "@blockula/sdk";
import { ADDR, seedWorld } from "./explorer-world";
import { VALIDATOR } from "./mock-node";
import { createWallet, expect, test } from "./helpers";

const PHONE = { width: 390, height: 844 };

/**
 * How far the page scrolls sideways, and what sticks out (to say what to fix): boxes past the
 * right edge, and boxes whose content (a long unbreakable word) spills out of them.
 */
const overflow = (page: Page) =>
  page.evaluate(() => {
    const w = document.documentElement.clientWidth;
    const name = (e: Element) => `${e.tagName.toLowerCase()}${e.getAttribute("data-testid") ? `[${e.getAttribute("data-testid")}]` : ""}${e.getAttribute("class") ? `.${String(e.getAttribute("class")).trim().replace(/\s+/g, ".")}` : ""}`;
    const all = [...document.querySelectorAll("body *")].map((e) => ({ e, r: e.getBoundingClientRect() })).filter(({ r }) => r.width > 0);
    const past = all.filter(({ r }) => r.right > w + 1).sort((a, b) => b.r.right - a.r.right).slice(0, 3).map(({ e, r }) => `${name(e)} → ${Math.round(r.right)}px`);
    const spills = all
      .filter(({ e, r }) => getComputedStyle(e).overflowX === "visible" && r.left + e.scrollWidth > w + 1 && e.scrollWidth > e.clientWidth + 1)
      .slice(-3)
      .map(({ e, r }) => `${name(e)} spills to ${Math.round(r.left + e.scrollWidth)}px`);
    return { by: document.documentElement.scrollWidth - window.innerWidth, out: [...past, ...spills] };
  });

/** Settle (data loaded, spinners gone), then the page must be no wider than the phone. */
async function fits(page: Page, what: string) {
  await expect(page.locator("main").first(), what).toBeVisible();
  await expect(page.locator("main .spinner, main .loading-note"), `${what} still loading`).toHaveCount(0, { timeout: 15_000 });
  await page.waitForTimeout(200);
  const o = await overflow(page);
  expect(o.by, `${what} scrolls sideways by ${o.by}px at 390 px: ${o.out.join(", ")}`).toBeLessThanOrEqual(1);
}

test("every route fits a 390 px phone", async ({ page, node }) => {
  test.setTimeout(240_000);
  await page.setViewportSize(PHONE);
  const { account } = await createWallet(page);
  const w = seedWorld(node, account);
  // USER-7's case: a treasury spend whose "does" names a whole bech32 address.
  node.proposals.push({ ...node.proposals[0], id: 1, title: "Pay the bat-house builders", kind: { type: "treasury_spend", to: ADDR.lucy, amount: parseFang("1000").toString() }, summary: "" });

  const open = async (path: string, wallet: boolean) => {
    await page.goto(path);
    if (wallet) await page.getByRole("button", { name: "Unlock with passkey" }).click();
    await fits(page, path);
  };

  // The wallet's own pages (behind the passkey).
  for (const path of ["/", "/send", "/receive", "/stake", "/governance", "/devices", "/name"]) await open(path, true);
  // Pages anyone can open.
  for (const path of ["/welcome", "/recover", "/link"]) await open(path, false);
  // The explorer.
  for (const path of [
    "/explore",
    `/tx/${w.ids.sent}`,
    `/tx/${w.ids.byName}`,
    `/tx/${w.ids.dropped}`,
    `/tx/${w.ids.multisigPropose}`,
    "/account/vlad.boo",
    "/account/lucy.boo",
    `/account/${ADDR.stranger}`,
    `/block/${node.height - 2}`,
    "/trace/vlad.boo",
    "/name/castle",
    "/name/tomb",
    "/programs",
    "/programs/accounts",
    "/programs/names",
    "/programs/hunts",
    "/programs/pool",
    "/programs/validators",
    "/hunt/0",
    "/hunt/2",
    "/claim/1",
    "/seance/2",
  ])
    await open(path, false);
});

test("the review steps fit a 390 px phone", async ({ page, node }) => {
  await page.setViewportSize(PHONE);
  const { account } = await createWallet(page);
  seedWorld(node, account);
  node.proposals.push({ ...node.proposals[0], id: 1, title: "Pay the bat-house builders", kind: { type: "treasury_spend", to: ADDR.lucy, amount: parseFang("1000").toString() }, summary: "" });

  // Send's review, to a name signed as the name.
  await page.getByRole("link", { name: "Send", exact: true }).first().click();
  await page.getByPlaceholder(/^alice\.boo or /).fill("mina.boo");
  await page.getByPlaceholder("0.0").fill("1");
  await page.getByRole("button", { name: "Review" }).click();
  await page.getByTestId("by-chain").getByText("Why is this safe?").click();
  await fits(page, "send review");

  // Staking's review.
  await page.goto("/stake");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await page.getByLabel("Validator").selectOption(VALIDATOR);
  await page.getByLabel("Amount to delegate").fill("5");
  await page.getByRole("button", { name: /^Delegate 5 FANG to/ }).click();
  await expect(page.getByTestId("review")).toBeVisible();
  await fits(page, "stake review");

  // A vote's review, on the proposal that used to overflow.
  await page.goto("/governance");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  const card = page.getByTestId("proposal").filter({ hasText: "Pay the bat-house builders" });
  await expect(card.getByTestId("proposal-does")).toContainText(ADDR.lucy);
  await fits(page, "governance");
  await card.getByRole("button", { name: "Aye" }).click();
  await card.getByLabel("Amount to vote with").fill("1");
  await card.getByRole("button", { name: /^Vote aye with 1 FANG/ }).click();
  await expect(card.getByTestId("review")).toBeVisible();
  await fits(page, "vote review");
});
