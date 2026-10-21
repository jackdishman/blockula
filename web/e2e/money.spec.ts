// Money moves only after the wallet says plainly what happens (wallet-money): Send checks what can
// move now, not the balance, and Max fills exactly that less the fee and any account opening
// (USER-5, TR-11); the review names the payee on one line, folds the node-trust explanation away,
// warns about a name only when it lapses soon or changed hands, says payments are final right
// above Confirm, and offers no "Send anyway" when the refusal is certain (USER-6, CRE-5,
// legal-safety-10); Priority tips the block's proposer; moving stake shows a review first (USER-4).
import { formatFang, parseFang, type Tx } from "@blockula/sdk";
import type { Page, Route } from "@playwright/test";
import { RPC } from "./env";
import { addressOf, OTHER_VALIDATOR, VALIDATOR } from "./mock-node";
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";

const ALICE = addressOf("a1".repeat(32));
const BOBBY = addressOf("b2".repeat(32));
const CAROL = addressOf("c3".repeat(32));
const FRESH = addressOf("f5".repeat(32));
const YEAR = 31_557_600;
const cors = { "access-control-allow-origin": "*" };
const json = (route: Route, body: unknown) => route.fulfill({ status: 200, contentType: "application/json", headers: cors, body: JSON.stringify(body) });

/**
 * An address with no account yet, answered the same way every time (the mock node opens an
 * account on its first read, so a second read would say it exists).
 */
async function freshAccount(page: Page) {
  await page.route(`${RPC}/api/account/${FRESH}`, (route) =>
    json(route, {
      account: { address: FRESH, name: null, short: `${FRESH.slice(0, 10)}…${FRESH.slice(-6)}` },
      balance: "0", balance_display: "0 FANG", transferable: "0", locked: "0", locks: [], exists: false, creation_fee: "100000000",
      nonce: 0, next_nonce: 0, names: [], keys: [], recent_txs: [], github: null, familiars: [], hunts_created: [], hunts_won: [], contribution: null,
    }),
  );
}

const sendForm = (page: Page) => ({
  to: page.getByPlaceholder(/^alice\.boo or /),
  amount: page.getByPlaceholder("0.0"),
  review: page.getByRole("button", { name: "Review" }),
  max: page.getByRole("button", { name: "Max", exact: true }),
});

test("Send checks what can move now, Max fills it less the fee, and payments are final above Confirm", async ({ page, node }) => {
  await watchCsp(page);
  const { account } = await createWallet(page);
  node.names.set("alice", ALICE);
  node.credit(ALICE, "1");
  // Half the faucet's 100 FANG is held by a vote (the node's id and reason are both "vote:2").
  node.locks.set(account, [{ id: "vote:2", reason: "vote:2", amount: parseFang("50"), until: node.height + 900, vesting: false }]);
  await page.getByRole("link", { name: "Send", exact: true }).first().click();
  const f = sendForm(page);
  await expect(page.getByTestId("can-send")).toHaveText("Can send now: 50 FANG · 50 FANG locked");
  await f.to.fill("alice.boo");
  await expect(page.getByText(/the chain pays its owner when this runs/)).toBeVisible();

  // More than can move: refused before anything is built.
  await f.amount.fill("90");
  // The lock in words: whose vote, and until about when (USER-5), not "vote:2 until block #…".
  await expect(page.getByText(/^You can send up to 50 FANG now: 50 FANG of your balance is locked by your vote on proposal #2 until about \d{2}:\d{2}\.$/)).toBeVisible();
  await expect(f.review).toBeDisabled();

  // Max: everything that can move, less the fee.
  await f.max.click();
  await expect(f.amount).toHaveValue(/^49\.9999\d*$/);
  await f.review.click();
  await expect(page.locator("dd.total")).toHaveText("50 FANG");
  // No name on this account: "You send", not "You sends".
  await expect(page.locator("p.summary")).toHaveText(/^You send 49\.9999\d* FANG to alice\.boo$/);

  // The payee on one line; the node-trust explanation behind "Why is this safe?".
  await expect(page.getByTestId("payee")).toHaveText(`Pays alice.boo · ${ALICE.slice(0, 10)}…${ALICE.slice(-6)} (your node's answer)`);
  const why = page.getByTestId("by-chain");
  await expect(why.getByText(/^Signed as the name/)).toBeHidden();
  await why.getByText("Why is this safe?").click();
  await expect(why.getByText(/^Signed as the name/)).toBeVisible();
  // A name that's settled (registered long before, far from expiring) gets no warning.
  await expect(page.getByTestId("name-risk")).toHaveCount(0);

  // "Payments are final" right above Confirm.
  const final = page.getByTestId("payments-final");
  await expect(final).toHaveText(/^Payments are final: nobody can reverse them, Blockula included\./);
  const confirm = page.getByRole("button", { name: "Confirm & send" });
  const [fb, cb] = [await final.boundingBox(), await confirm.boundingBox()];
  expect(fb && cb && fb.y + fb.height <= cb.y).toBe(true);
  await confirm.click();
  await expect(page.getByRole("heading", { name: "Sent" })).toBeVisible();
  const tx = node.submitted.at(-1) as Tx;
  expect(tx.action.type === "transfer" && BigInt(tx.action.amount) + BigInt(tx.fee)).toBe(parseFang("50"));
  expect(await cspViolations(page)).toEqual([]);
});

test("Max leaves room for opening a new account", async ({ page }) => {
  await freshAccount(page);
  await createWallet(page);
  await page.getByRole("link", { name: "Send", exact: true }).first().click();
  const f = sendForm(page);
  await expect(page.getByTestId("can-send")).toHaveText("Can send now: 100 FANG");
  await expect(f.max).toBeDisabled();
  await f.to.fill(FRESH);
  await f.max.click();
  await expect(f.amount).toHaveValue(/^99\.8999\d*$/);
  await f.review.click();
  await expect(page.getByTestId("opening")).toHaveText(/^0\.1 FANG/);
  await expect(page.locator("dd.total")).toHaveText("100 FANG");
  await expect(page.getByRole("button", { name: "Confirm & send" })).toBeVisible();
});

test("a refusal the chain's state settles offers no Send anyway; one it might not, still does", async ({ page }) => {
  await freshAccount(page);
  await createWallet(page);
  await page.getByRole("link", { name: "Send", exact: true }).first().click();
  const f = sendForm(page);
  await expect(page.getByTestId("can-send")).toHaveText("Can send now: 100 FANG");
  // All 100 FANG, with nothing left for the fee: certain.
  await f.to.fill(FRESH);
  await f.amount.fill("100");
  await f.review.click();
  await expect(page.getByTestId("preflight")).toContainText("The chain will refuse this.");
  await expect(page.getByTestId("preflight")).toContainText("Not enough FANG");
  await expect(page.getByRole("button", { name: "Send anyway" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Confirm & send" })).toHaveCount(0);
  await expect(page.getByTestId("cannot-send")).toBeVisible();
  await page.getByRole("button", { name: "← Edit" }).click();

  // Less than a new account's minimum first payment: refused by a rule this wallet reads from its
  // node, so it still lets you try.
  await f.amount.fill("0.001");
  await f.review.click();
  await expect(page.getByTestId("preflight")).toContainText("a first payment must be at least 0.01 FANG");
  await expect(page.getByRole("button", { name: "Send anyway" })).toBeVisible();
  await expect(page.getByTestId("cannot-send")).toHaveCount(0);
});

test("a name gets a warning only when it lapses within the payment's validity, or changed hands lately", async ({ page, node }) => {
  await createWallet(page);
  // Registered a year before it expires, 100 blocks from now: inside a payment's 600-block window.
  node.names.set("bobby", BOBBY);
  node.credit(BOBBY, "1");
  node.nameInfo.set("bobby", { registered_at: node.height + 100 - YEAR, expires_at: node.height + 100 });
  // Accepted 5 blocks ago (accepting re-stamps registered_at, not the expiry).
  node.names.set("carol", CAROL);
  node.credit(CAROL, "1");
  node.nameInfo.set("carol", { registered_at: node.height - 5, expires_at: YEAR + 1 });
  const pay = async (name: string) => {
    await page.goto("/send");
    await page.getByRole("button", { name: "Unlock with passkey" }).click();
    const f = sendForm(page);
    await f.to.fill(name);
    await expect(page.getByText(/the chain pays its owner when this runs/)).toBeVisible();
    await f.amount.fill("1");
    await f.review.click();
    await expect(page.getByRole("heading", { name: "Review" })).toBeVisible();
  };
  await pay("bobby.boo");
  await expect(page.getByTestId("name-risk")).toHaveCount(1);
  await expect(page.getByTestId("name-risk")).toContainText("bobby.boo expires about");
  await expect(page.getByTestId("name-risk")).toContainText("before this payment stops being valid");
  await pay("carol.boo");
  await expect(page.getByTestId("name-risk")).toHaveCount(1);
  await expect(page.getByTestId("name-risk")).toContainText("carol.boo was registered or changed hands in the last 30 days");
});

test("Priority tips the block's proposer one base fee", async ({ page, node }) => {
  await createWallet(page);
  node.names.set("alice", ALICE);
  node.credit(ALICE, "1");
  await page.getByRole("link", { name: "Send", exact: true }).first().click();
  const f = sendForm(page);
  await f.to.fill("alice.boo");
  await f.amount.fill("1");
  await page.getByLabel(/^Priority/).check();
  await f.review.click();
  await expect(page.getByText(/tip to the block's proposer/)).toBeVisible();
  await page.getByRole("button", { name: "Confirm & send" }).click();
  await expect(page.getByRole("heading", { name: "Sent" })).toBeVisible();
  const tx = node.submitted.at(-1) as Tx;
  expect(BigInt(tx.tip ?? "0")).toBeGreaterThan(0n);
  expect(BigInt(tx.fee)).toBe(2n * BigInt(tx.tip ?? "0"));
});

test("moving stake to another validator: a review first, then the pool shares are signed", async ({ page, node }) => {
  await watchCsp(page);
  // The mock node doesn't run redelegations; this route does, as node/src/staking.rs does: the
  // shares leave one pool at its price and join the other at its own.
  await page.route(`${RPC}/api/tx`, async (route) => {
    const req = route.request();
    const tx = req.method() === "POST" ? (JSON.parse(req.postData() ?? "{}") as Tx) : null;
    if (!tx || tx.action.type !== "redelegate") return route.fallback();
    const act = tx.action;
    const [from, to] = [node.pools.get(act.from_validator), node.pools.get(act.to_validator)];
    const mine = node.shares.get(tx.from);
    if (!from || !to || !mine) return route.fulfill({ status: 400, contentType: "application/json", headers: cors, body: JSON.stringify({ error: "no such pool" }) });
    const shares = BigInt(act.shares);
    const value = (shares * from.stake) / from.shares;
    const got = (value * to.shares) / to.stake;
    [from.stake, from.shares, to.stake, to.shares] = [from.stake - value, from.shares - shares, to.stake + value, to.shares + got];
    mine.set(act.from_validator, (mine.get(act.from_validator) ?? 0n) - shares);
    mine.set(act.to_validator, (mine.get(act.to_validator) ?? 0n) + got);
    const a = node.acct(tx.from);
    a.balance -= BigInt(tx.fee);
    a.nonce += 1;
    node.submitted.push(tx);
    const id = node.add({ kind: "redelegate", from: tx.from, to: null, amount: null, fee: BigInt(tx.fee), memo: "", summary: `moved ${formatFang(value)} of stake to another validator`, action: act as unknown as Record<string, unknown>, nonce: tx.nonce });
    return json(route, { id });
  });
  await createWallet(page);
  await page.getByRole("link", { name: "Stake", exact: true }).click();
  await page.getByLabel("Validator").selectOption(VALIDATOR);
  await page.getByLabel("Amount to delegate").fill("25");
  await page.getByRole("button", { name: /^Delegate 25 FANG to/ }).click();
  await page.getByRole("button", { name: "Confirm & stake" }).click();
  await expect(page.getByTestId("pool")).toContainText("25 FANG");

  const pool = page.getByTestId("pool");
  await pool.getByRole("button", { name: "Move", exact: true }).click();
  await page.getByLabel("Move to validator").selectOption(OTHER_VALIDATOR);
  await page.getByLabel("Amount to move").fill("10");
  await page.getByRole("button", { name: /^Move 10 FANG to/ }).click();
  const review = page.getByTestId("review");
  await expect(review).toContainText("Move about 10 FANG from");
  await expect(review).toContainText(VALIDATOR);
  await expect(review).toContainText(OTHER_VALIDATOR);
  await expect(review).toContainText("10% of what the pool earns");
  await expect(review.getByTestId("review-terms")).toContainText("It moves now, without waiting out the unbonding period.");
  const before = node.submitted.length;
  await review.getByRole("button", { name: "Confirm & move" }).click();
  await expect(page.getByRole("status")).toContainText("moved 10 FANG of stake");
  expect(node.submitted.length).toBe(before + 1);
  expect(node.submitted.at(-1)?.action).toEqual({ type: "redelegate", from_validator: VALIDATOR, to_validator: OTHER_VALIDATOR, shares: parseFang("10").toString() });
  await expect(page.getByTestId("pool")).toHaveCount(2);
  await expect(page.getByTestId("staked")).toHaveText("25 FANG");
  expect(await cspViolations(page)).toEqual([]);
});
