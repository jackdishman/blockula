// The reviews before a signature, on a 390 x 844 phone (components/explorer/Review.tsx): the line
// that names the action, the total and the primary button are all on screen at once, with nothing
// to scroll, and everything technical is under "Details", closed until it's opened. Checked for a
// blackjack bet (components/blackjack/TableCall.tsx) and for Send (app/send/page.tsx).
import { seedHash } from "@blockula/blackjack";
import type { Locator, Page } from "@playwright/test";
import { addTable, TABLE } from "./blackjack-world";
import { createWallet, expect, test } from "./helpers";
import { addressOf } from "./mock-node";

const PHONE = { width: 390, height: 844 };

/** The whole of `what` is inside the viewport as the page is scrolled now. */
async function onScreen(page: Page, what: Locator, name: string) {
  await expect(what, name).toBeVisible();
  const box = await what.boundingBox();
  const top = Math.round(box?.y ?? -1);
  const bottom = Math.round((box?.y ?? 0) + (box?.height ?? 0));
  expect(top, `${name} starts at ${top}px, above the screen`).toBeGreaterThanOrEqual(0);
  expect(bottom, `${name} ends at ${bottom}px, below a ${PHONE.height}px screen`).toBeLessThanOrEqual(PHONE.height);
  expect(Math.round((box?.x ?? 0) + (box?.width ?? 0)), `${name} is wider than the screen`).toBeLessThanOrEqual(PHONE.width);
}

test("a blackjack bet's review fits a 390 x 844 phone: the action, the total and Sign on screen, Details closed", async ({ page, node }) => {
  await page.setViewportSize(PHONE);
  const { account: me } = await createWallet(page);
  const table = addTable(node);
  await page.goto(`/blackjack?table=${TABLE}`);
  const form = page.getByTestId("bj-bet-form");
  await form.getByRole("button", { name: "Unlock" }).click();
  await page.getByTestId("bj-gate").getByRole("checkbox").check();
  await form.getByTestId("bj-bet-input").fill("25");
  await form.getByTestId("bj-bet-review").click();

  const review = page.getByTestId("bj-review");
  await expect(review.getByTestId("bj-review-does")).toHaveText("Bet 25 FANG on a new hand (step 1 of 2)");
  // The review scrolls itself into view: nothing here scrolls the page.
  await page.waitForTimeout(300);
  await onScreen(page, review.getByTestId("bj-review-does"), "the action line");
  await onScreen(page, review.getByTestId("bj-review-total"), "the total");
  await onScreen(page, review.getByTestId("bj-sign"), "Sign & send");
  await onScreen(page, review.getByRole("button", { name: "← Back" }), "Back");
  // The signer and what leaves the account stay in the summary.
  await expect(review).toContainText("(owner)");
  await expect(review.getByTestId("bj-review-value")).toContainText("25");
  await expect(review.getByTestId("bj-review-fee")).toBeVisible();

  // Everything technical is one disclosure, closed: the program's address, the input and the digest.
  const details = review.getByTestId("bj-review-details");
  await expect(details).not.toHaveAttribute("open", "");
  await expect(review.getByTestId("bj-review-program")).toBeHidden();
  await expect(review.getByTestId("bj-review-digest")).toBeHidden();
  await expect(review.locator("details[open]")).toHaveCount(0);
  // Opened, it's all there: the whole address, the input (the seed's hash, said once) and the version pin.
  await details.getByText("Details", { exact: true }).click();
  await expect(review.getByTestId("bj-review-program")).toHaveText(TABLE);
  await expect(review.getByTestId("bj-review-digest")).toHaveText(table.digest);
  await expect(review.getByTestId("bj-review-digest")).toBeVisible();
  await expect(review.getByTestId("bj-review-pinned")).toContainText("If it changed, the call is refused and nothing is charged.");
  const seed = await page.evaluate((k) => (JSON.parse(localStorage.getItem(k) ?? "null") as { pending: { seed: string }[] }).pending.at(-1)!.seed, `blockula.blackjack.blockula-testnet.${TABLE}`);
  await expect(review.getByTestId("bj-review-input")).toHaveText(seedHash(me, seed));
  await expect(review.getByTestId("bj-review-seed-hash")).toHaveCount(0);
  expect(node.submitted).toHaveLength(0);
});

test("Send's review fits a 390 x 844 phone: the action, the total and Confirm on screen, Details closed", async ({ page, node }) => {
  await page.setViewportSize(PHONE);
  await createWallet(page);
  const alice = addressOf("a1".repeat(32));
  node.credit(alice, "1");
  node.names.set("alice", alice);
  await page.getByRole("link", { name: "Send", exact: true }).first().click();
  await page.getByPlaceholder(/^alice\.boo or /).fill("alice.boo");
  await page.getByPlaceholder("0.0").fill("10");
  await page.getByRole("button", { name: "Review" }).click();

  const review = page.getByTestId("send-review");
  await expect(review.getByTestId("send-review-action")).toHaveText("You send 10 FANG to alice.boo");
  await page.waitForTimeout(300);
  await onScreen(page, review.getByTestId("send-review-action"), "the action line");
  await onScreen(page, review.getByTestId("send-review-total"), "the total");
  await onScreen(page, page.getByRole("button", { name: "Confirm & send" }), "Confirm & send");
  await onScreen(page, page.getByRole("button", { name: "← Edit" }), "Edit");
  // Who signs, who is paid and that payments are final stay in view.
  await expect(review.getByTestId("send-review-from")).toBeVisible();
  await expect(review.getByTestId("payee")).toContainText("Pays alice.boo");
  await expect(review.getByTestId("payments-final")).toBeVisible();

  const details = review.getByTestId("review-details");
  await expect(details).not.toHaveAttribute("open", "");
  await expect(review.locator("details[open]")).toHaveCount(0);
  await expect(review.getByTestId("by-chain")).toBeHidden();
  await details.getByText("Details", { exact: true }).click();
  await expect(review.getByTestId("by-chain")).toContainText("Signed as the name: the chain pays whoever owns alice.boo when this transaction runs");
  await expect(review.getByTestId("preflight-changes")).toBeVisible();
  expect(node.submitted).toHaveLength(0);
});
