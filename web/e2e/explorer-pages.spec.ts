// Every explorer page and flow, against the mock node filled with protocol v1's shapes
// (e2e/explorer-world.ts), on the production build and its strict CSP (lib/csp.ts: style-src
// 'self' 'nonce-…', so a style attribute in server-rendered HTML is a violation). Each page must
// render, raise no CSP violation and log no error, show strangers' text as text (no markup runs,
// no `javascript:` link), and read v1's rules right: dropped vs failed receipts, tips, names
// signed as names, expired and reserved names, hunts' cancel rule, staking's groups.
import type { Tx } from "@blockula/sdk";
import { HOSTILE, PHONE, seedWorld, type World } from "./explorer-world";
import { RPC } from "./env";
import { addressOf, VALIDATOR } from "./mock-node";
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";
import type { Locator, Page } from "@playwright/test";

/** Node reads that answer 404 on purpose (not a multisig, a page about something that isn't there). */
const EXPECTED_404 = [/\/api\/multisig\//, /\/api\/tx\/0{64}$/, /\/api\/account\/nosuch\.boo$/, /\/api\/hunt\/99$/];

/** Errors the page logs, uncaught exceptions, and node reads that failed unexpectedly, from now on. */
function watchErrors(page: Page) {
  const errors: string[] = [];
  page.on("console", (m) => {
    // A failed fetch is logged as a resource error: those are checked by status below.
    if (m.type() === "error" && !/^Failed to load resource/.test(m.text())) errors.push(`console: ${m.text()}`);
  });
  page.on("pageerror", (e) => errors.push(`pageerror: ${e.message}`));
  page.on("response", (r) => {
    if (r.status() >= 400 && !EXPECTED_404.some((re) => re.test(r.url()))) errors.push(`${r.status()} ${r.url()}`);
  });
  return errors;
}

/** Open `path`, wait for `ready`, then hold the page to the rules every explorer page keeps. */
async function visit(page: Page, path: string, ready: (p: Page) => Locator, errors: string[]) {
  await page.goto(path);
  await expect(ready(page), path).toBeVisible();
  // A beat for polls and effects to run (and fail, if they would).
  await page.waitForTimeout(150);
  expect(await cspViolations(page), `CSP violations on ${path}`).toEqual([]);
  expect(errors, `errors on ${path}`).toEqual([]);
  // Strangers' text renders as text: no markup ran, and no link from chain data is `javascript:`.
  expect(await page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned), path).toBeUndefined();
  await expect(page.locator('a[href^="javascript:" i]'), path).toHaveCount(0);
  await expect(page.locator("main img[src='x']"), path).toHaveCount(0);
}

const receipt = (p: Page) => p.locator(".receipt .headline");

test("no explorer page server-renders a style attribute (the CSP would block it)", async ({ request, node }) => {
  test.skip(!!process.env.E2E_DEV, "next dev relaxes the CSP");
  const w = seedWorld(node);
  const paths = ["/explore", `/tx/${w.ids.sent}`, `/tx/${w.ids.dropped}`, "/account/vlad.boo", "/block/101", "/trace/vlad.boo", "/name/castle", "/name/tomb", "/programs", "/programs/hunts", "/programs/validators", "/programs/pool", "/programs/names", "/programs/accounts", "/hunt/0", "/claim/1", "/seance/2", "/governance"];
  for (const path of paths) {
    const res = await request.get(path);
    expect(res.status(), path).toBe(200);
    const html = await res.text();
    expect([...html.matchAll(/<[a-z][^>]*\sstyle=/gi)].map((m) => m[0].slice(0, 80)), path).toEqual([]);
  }
});

test.describe("every explorer page", () => {
  let w: World;
  let errors: string[];
  test.beforeEach(async ({ page, node }) => {
    w = seedWorld(node);
    errors = watchErrors(page);
    await watchCsp(page);
  });

  test("the pulse, blocks, accounts and the money trail", async ({ page, node }) => {
    await visit(page, "/explore", (p) => p.getByRole("heading", { name: "blockula-testnet, live" }), errors);
    await expect(page.getByText("The network is healthy")).toBeVisible();
    await expect(page.getByText("Waiting for a block")).toBeVisible();
    await expect(page.locator(".ledger-memo", { hasText: "on its way" })).toBeVisible();
    await expect(page.locator(".ledger-summary").first()).toBeVisible();

    const sealed = node.height;
    await visit(page, `/block/${sealed - 2}`, (p) => p.getByRole("heading", { name: `#${(sealed - 2).toLocaleString("en-US")}` }), errors);
    await visit(page, "/block/1", (p) => p.getByRole("heading", { name: "#1" }), errors);

    await visit(page, "/account/vlad.boo", (p) => p.getByRole("heading", { name: "vlad.boo" }), errors);
    await expect(page.locator(".key-label", { hasText: "Vlad's phone" })).toBeVisible();
    await expect(page.locator(".key-label", { hasText: `claude ${HOSTILE}` })).toBeVisible();
    await expect(page.locator(".chip", { hasText: `github · @vlad${HOSTILE}` })).toBeVisible();
    await visit(page, "/account/lucy.boo", (p) => p.getByTestId("locks"), errors);
    await expect(page.getByTestId("locks")).toContainText("vote on proposal #0");
    await expect(page.getByTestId("locks")).toContainText("vesting until block");
    await visit(page, "/account/coven.boo", (p) => p.getByRole("heading", { name: "coven.boo" }), errors);
    await visit(page, `/account/${w.stranger}`, (p) => p.getByRole("heading", { name: "Unnamed account" }), errors);
    await visit(page, "/account/nosuch.boo", (p) => p.getByRole("heading", { name: "nosuch.boo isn't anyone's" }), errors);

    await visit(page, "/trace/vlad.boo", (p) => p.getByRole("heading", { name: "Around vlad.boo" }), errors);
  });

  // A receipt for every kind, a quarter of them per test: each page load is a server render, so
  // all of them in one test wouldn't fit its timeout on a busy machine.
  const PARTS = 4;
  for (let part = 0; part < PARTS; part++) {
    test(`receipts for every kind of transaction, as v1 reports them (${part + 1} of ${PARTS})`, async ({ page }) => {
      const eyebrow = (p: Page) => p.locator(".receipt .eyebrow");
      for (const [name, id] of Object.entries(w.ids).filter((_, i) => i % PARTS === part)) {
        await visit(page, `/tx/${id}`, receipt, errors);
        if (["failed", "ranUnseen"].includes(name)) await expect(eyebrow(page), name).toHaveText("Included, but failed");
        else if (name === "dropped") {
          // Not a failure that ran: not final, nothing charged, no red pill (review-r4-wallet-2).
          await expect(eyebrow(page), name).toHaveText("Dropped, not final");
          await expect(page.locator(".receipt .pill"), name).toHaveText(["dropped"]);
        }
        else if (name === "waiting") await expect(eyebrow(page), name).toHaveText("On its way");
        else await expect(eyebrow(page), name).toHaveText("Receipt");
        // What was signed is rendered here from the action, never taken from the node's summary.
        if (!["dropped", "ranUnseen"].includes(name)) await expect(page.getByTestId("receipt-signs"), name).toBeVisible();
      }
    });
  }

  test("receipts read v1's dropped, failed, tipped, by-name and locking transactions right", async ({ page }) => {
    const { ids } = w;
    // Dropped unrun: in no block, nothing charged, and it may still be included elsewhere.
    await visit(page, `/tx/${ids.dropped}`, receipt, errors);
    await expect(page.locator(".crumbs")).not.toContainText("Block #");
    await expect(page.locator("ol.trace")).toContainText("nothing was charged, but another node may still include it until its valid_until");
    await expect(page.locator("ol.trace")).toContainText("not in any block this node holds");
    await expect(page.locator(".receipt .pill.failed")).toHaveCount(0);
    await expect(page.locator("ol.trace")).not.toContainText("quorum certificate");
    await expect(page.getByText("Balance changes")).toHaveCount(0);
    // Ran in a block this node no longer holds: it was charged.
    await visit(page, `/tx/${ids.ranUnseen}`, receipt, errors);
    await expect(page.locator("ol.trace")).toContainText("where it failed and its fee was charged");
    // Failed in a block: only the fee.
    await visit(page, `/tx/${ids.failed}`, receipt, errors);
    await expect(page.locator(".changes")).toContainText("fee (the action failed)");

    // A tip: the proposer gets it, the rest burns.
    await visit(page, `/tx/${ids.tipped}`, receipt, errors);
    await expect(page.locator(".changes")).toContainText("the base fee (the tip went to the proposer)");
    await expect(page.locator(".changes")).toContainText("+0.0001");
    await expect(page.getByText("the node doesn't say how much")).toHaveCount(0);

    // Signed to a name: paid to whoever owned it then.
    await visit(page, `/tx/${ids.byName}`, receipt, errors);
    await expect(page.getByText("signed to lucy.boo: the account that owned it when this ran")).toBeVisible();

    // A vote locks, a claim comes in, a kind newer than this explorer: never an outflow by default.
    await visit(page, `/tx/${ids.voted}`, receipt, errors);
    await expect(page.getByTestId("amount-hero")).toContainText("locked by the vote, not spent");
    await expect(page.getByTestId("amount-hero")).not.toContainText("−");
    await visit(page, `/tx/${ids.newer}`, receipt, errors);
    await expect(page.getByTestId("amount-hero")).not.toContainText("−");
    await expect(page.locator(".card-note", { hasText: "doesn't know this kind" })).toBeVisible();

    // A memo from a stranger is text.
    await visit(page, `/tx/${ids.received}`, receipt, errors);
    await expect(page.locator(".ledger-memo").first()).toHaveText(`“${HOSTILE}”`);

    // Not there at all.
    await visit(page, `/tx/${"0".repeat(64)}`, (p) => p.getByRole("heading", { name: "No such transaction" }), errors);
  });

  test("printing a receipt prints the receipt", async ({ page }) => {
    await visit(page, `/tx/${w.ids.sent}`, receipt, errors);
    await page.emulateMedia({ media: "print" });
    await expect(receipt(page)).toBeVisible();
    await expect(page.locator(".receipt .print-only")).toBeVisible();
    await expect(page.locator("header.top")).toBeHidden();
  });

  test("names: active, offered, expired, reserved and free", async ({ page }) => {
    await visit(page, "/name/vlad", (p) => p.getByTestId("name-status"), errors);
    await expect(page.getByTestId("name-status")).toContainText("Points to vlad.boo");
    await visit(page, "/name/castle", (p) => p.getByTestId("name-offer"), errors);
    await expect(page.getByTestId("name-offer")).toContainText("Offered to mina.boo: it stays vlad.boo's until they accept it");
    await visit(page, "/name/tomb", (p) => p.getByTestId("name-status"), errors);
    await expect(page.getByTestId("name-status")).toContainText("it points to nobody now, so payments to it fail");
    await expect(page.getByRole("link", { name: "Open its owner's account" })).toBeVisible();
    await expect(page.getByRole("link", { name: /^Send to/ })).toHaveCount(0);
    await expect(page.getByRole("link", { name: "Open tomb.boo" })).toHaveCount(0);
    await visit(page, "/name/crypt", (p) => p.getByTestId("name-status"), errors);
    await expect(page.getByTestId("name-status")).toHaveText("Reserved at genesis: nobody can register it.");
    await visit(page, "/name/ghoul", (p) => p.getByRole("heading", { name: "Yours for 5 FANG a year" }), errors);
  });

  test("programs, hunts, claims and Séances", async ({ page }) => {
    await visit(page, "/programs", (p) => p.getByRole("heading", { name: "What the chain can do" }), errors);
    await visit(page, "/programs/accounts", (p) => p.getByRole("heading", { name: "Accounts & keys", level: 1 }), errors);
    await visit(page, "/programs/names", (p) => p.getByRole("heading", { name: "Names", exact: true, level: 1 }), errors);
    const lookup = page.locator("section", { has: page.getByRole("heading", { name: "Look up a name" }) }).getByPlaceholder("ghoul");
    for (const [q, says] of [["tomb", "expired at block"], ["crypt", "reserved at genesis"], ["castle", "offered to"]]) {
      await lookup.fill(q);
      await expect(page.locator(".name-status")).toContainText(says);
    }
    await visit(page, "/programs/hunts", (p) => p.getByRole("heading", { name: "Hunts", exact: true, level: 1 }), errors);
    await expect(page.locator(".bars").first()).toContainText(HOSTILE);
    await visit(page, "/programs/pool", (p) => p.getByText("Séance #2 · sealing votes until block"), errors);
    await visit(page, "/programs/validators", (p) => p.getByRole("heading", { name: "Validators", exact: true, level: 1 }), errors);
    for (const group of ["Active this epoch (2)", "Joining at the next epoch (1)", "Leaving at the next epoch (1)", "Candidates, by stake (2)", "Jailed (1)", "Tombstoned (1)"]) await expect(page.getByRole("heading", { name: group })).toBeVisible();
    await expect(page.getByRole("heading", { name: /Waiting for approval/ })).toHaveCount(0);
    await expect(page.getByTestId("admission")).toContainText("it joins only once 1 of them admit it this epoch");
    // v1's staking actions, and no Unbond (v1 refuses it).
    for (const title of ["Stake with a validator", "Unstake", "Withdraw unbonded stake", "Unjail a validator", "Set commission"]) await expect(page.locator("summary .title", { hasText: title })).toBeVisible();
    await expect(page.locator("summary .sig", { hasText: /^unbond$/ })).toHaveCount(0);
    await visit(page, "/programs/nope", (p) => p.getByRole("heading", { name: "No such program" }), errors);
    // A link that fills an action says it can't tell which network it was made for.
    await visit(page, "/programs/hunts?do=fund_hunt&hunt_id=0&amount=2", (p) => p.locator(".banner", { hasText: "A link filled in this action" }), errors);

    await visit(page, "/hunt/0", receipt, errors);
    await expect(receipt(page)).toHaveText(`“${HOSTILE}”`);
    await expect(page.getByText("Expiring refunds nothing by itself")).toBeVisible();
    await visit(page, "/hunt/1", receipt, errors);
    await expect(page.getByText(/its escrow still held until the creator awards or cancels it/)).toBeVisible();
    await visit(page, "/hunt/2", (p) => p.locator(".headline.won"), errors);
    // Links come from the repo and number, never from the node's *_url fields.
    await expect(page.locator('.headline.won a[target="_blank"]')).toHaveAttribute("href", "https://github.com/jackdishman/blockula/pull/7");
    await visit(page, "/hunt/3", receipt, errors);
    await expect(page.getByText("Cancelled at block #128")).toBeVisible();
    await expect(page.locator('a[href*="evil"]')).toHaveCount(0);
    await visit(page, "/hunt/99", (p) => p.getByRole("heading", { name: "No such hunt" }), errors);

    for (const id of [0, 1, 2, 3, 4, 5]) await visit(page, `/claim/${id}`, (p) => p.getByText(`Claim #${id}`, { exact: true }).first(), errors);
    await expect(page.getByText(HOSTILE)).toBeVisible();
    await visit(page, "/seance/1", (p) => p.getByText("Closed at block #210"), errors);
    await expect(page.getByText("1 accepted · 0 below the threshold · 1 spam")).toBeVisible();
    await visit(page, "/seance/2", (p) => p.getByText(/^Sealing votes until block/), errors);
  });

  test("the command palette finds names, accounts and transactions", async ({ page }) => {
    await visit(page, "/explore", (p) => p.getByRole("heading", { name: "blockula-testnet, live" }), errors);
    await page.keyboard.press("Control+k");
    const search = page.getByRole("dialog", { name: "Search and commands" });
    await expect(search).toBeVisible();
    await search.getByRole("textbox", { name: "Search" }).fill("castle");
    await expect(search.getByText("castle.boo").first()).toBeVisible();
    await search.getByRole("textbox", { name: "Search" }).fill(w.ids.sent);
    await expect(search.getByText(/Transaction|transaction/).first()).toBeVisible();
    expect(await cspViolations(page)).toEqual([]);
    expect(errors).toEqual([]);
  });
});

test.describe("the explorer from an account holder's side", () => {
  let w: World;
  let errors: string[];
  let me: string;
  test.beforeEach(async ({ page, node }) => {
    errors = watchErrors(page);
    await watchCsp(page);
    me = (await createWallet(page)).account;
    w = seedWorld(node, me);
  });

  test("their own receipts, account and names", async ({ page }) => {
    // Their own names stay names in summaries (audit main13-render-6): on the wallet's activity,
    // and on the receipt it links to.
    const offered = page.locator(".ledger-summary", { hasText: "You offered castle.boo to mina.boo" });
    await expect(offered).toBeVisible({ timeout: 9000 });
    await offered.click();
    await expect(receipt(page)).toHaveText("You offered castle.boo to mina.boo");
    expect(await cspViolations(page)).toEqual([]);
    await visit(page, `/account/${me}`, (p) => p.getByText("Your account, as the chain sees it"), errors);
    await expect(page.getByRole("button", { name: "Export CSV" })).toBeVisible();
    await visit(page, "/name/castle", (p) => p.getByRole("heading", { name: "You own this name" }), errors);
    await expect(page.locator("summary .title", { hasText: "Withdraw the offer" })).toBeVisible();
    await visit(page, "/name/tomb", (p) => p.getByRole("heading", { name: "You own this name" }), errors);
    await expect(page.locator("details[open] summary .title", { hasText: "Renew tomb.boo before it's lost" })).toBeVisible();
    await visit(page, "/hunt/0", (p) => p.getByRole("heading", { name: "You put this up" }), errors);
    await expect(page.getByText(/You can cancel it and refund its funders once it expires/)).toBeVisible();
    await expect(page.locator("summary .title", { hasText: "Cancel and refund" })).toHaveCount(0);
    await visit(page, "/hunt/1", (p) => p.getByRole("heading", { name: "You put this up" }), errors);
    await expect(page.locator("summary .title", { hasText: "Cancel and refund" })).toBeVisible();
    await expect(page.getByRole("heading", { name: "Add to the bounty" })).toHaveCount(0);
  });

  test("on a phone, a receipt's buttons fit the screen", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await visit(page, `/tx/${w.ids.opened}`, receipt, errors);
    const again = page.getByRole("link", { name: /^Send .* 1 FANG again$/ });
    await expect(again).toBeVisible();
    await expect(again).toHaveText(`Send ${w.stranger.slice(0, 10)}…${w.stranger.slice(-6)} 1 FANG again`);
    const width = () => page.evaluate(() => document.documentElement.scrollWidth);
    expect(await width()).toBeLessThanOrEqual(390);
    // Nor does anything else long a stranger can write (labels, handles, addresses in what was signed).
    for (const path of [`/tx/${w.ids.sent}`, `/tx/${w.ids.agent}`, `/account/${me}`, "/explore", "/hunt/0", "/programs/validators"]) {
      await visit(page, path, (p) => p.locator("main h1, main .headline").first(), errors);
      expect(await width(), path).toBeLessThanOrEqual(390);
    }
  });

  test("offering to one's own signed name previews acceptance, not an offer (audit polish P2)", async ({ page, node }) => {
    node.names.set("castle", w.lucy);
    node.nameInfo.set("castle", { offered_to: me });
    await page.goto("/programs/names?do=transfer_name&name=castle&to=vlad.boo");
    const f = page.locator("details.action", { has: page.locator("summary .title", { hasText: "Offer a name" }) });
    const unlock = f.getByRole("button", { name: "Unlock to sign" });
    await expect(unlock.or(f.getByRole("button", { name: "Preview" }))).toBeVisible();
    if (await unlock.isVisible()) await unlock.click();
    await f.getByRole("button", { name: "Preview" }).click();
    await expect(f.getByTestId("action-preview").locator(".headline")).toHaveText("Accept castle.boo: it becomes this account's name");
    await expect(f.getByTestId("preflight")).not.toContainText("isn't this account's to offer");
    // The resolution informs only the forecast: without witnesses the signed recipient stays a name.
    await expect(f.getByTestId("signs")).toContainText("vlad.boo");
    expect(node.submitted).toEqual([]);
    expect(errors).toEqual([]);
  });

  test("previews of every program action render, with v1's refusals", async ({ page, node }) => {
    await visit(page, "/programs/accounts", (p) => p.getByRole("heading", { name: "Accounts & keys", level: 1 }), errors);
    const form = (title: string) => page.locator("details.action", { has: page.locator("summary .title", { hasText: title }) });
    const preview = async (title: string, fill: Record<string, string>) => {
      const f = form(title);
      if ((await f.getAttribute("open")) == null) await f.locator("summary").click();
      // The first form on a page unlocks the wallet for all of them.
      const unlock = f.getByRole("button", { name: "Unlock to sign" });
      await expect(unlock.or(f.getByRole("button", { name: "Preview" }))).toBeVisible();
      if (await unlock.isVisible()) await unlock.click();
      for (const [label, value] of Object.entries(fill)) await f.getByLabel(label, { exact: true }).fill(value);
      await f.getByRole("button", { name: "Preview" }).click();
      await expect(f.getByTestId("action-preview")).toBeVisible();
      await expect(f.getByTestId("preflight")).toBeVisible();
      return f;
    };
    // A payment that opens an account: the creation fee is counted and said.
    const pay = await preview("Send FANG", { To: w.fresh, Amount: "1" });
    await expect(pay.getByTestId("preflight")).toContainText("Opening its account burns 0.1 FANG");
    await expect(pay.getByTestId("signs")).toContainText("Send 1 FANG");
    await preview("Register a Familiar", { "Agent key id": PHONE });

    await page.goto("/programs/names");
    await preview("Register a name", { Name: "ghoul" });
    await preview("Renew a name", { Name: "vlad" });
    await preview("Offer a name", { Name: "castle", To: "lucy.boo" });
    // review-main13-2: castle.boo is this account's (offered to mina.boo), so sending it to oneself
    // withdraws the offer: the headline says what the chain will do, and the "accept" form refuses.
    const accept = await preview("Accept a name", { Name: "castle" });
    await expect(accept.getByTestId("action-preview").locator(".headline")).toHaveText("Withdraw your offer of castle.boo");
    await expect(accept.getByTestId("preflight")).toContainText("owns castle.boo, so this would withdraw its offer, not accept one");
    const withdraw = await preview("Withdraw an offer", { Name: "castle" });
    await expect(withdraw.getByTestId("action-preview").locator(".headline")).toHaveText("Withdraw your offer of castle.boo");
    await preview("Release a name", { Name: "castle" });

    await page.goto("/programs/hunts");
    await preview("Put up a hunt", { Repository: "jackdishman/blockula", "Issue number": "20", Title: "A title", Bounty: "10" });
    await preview("Add to a bounty", { Hunt: "0", Amount: "1" });
    const cancel = await preview("Cancel a hunt", { Hunt: "0" });
    await expect(cancel.getByTestId("preflight")).toContainText("can be cancelled by its creator after block");
    await expect(cancel.getByRole("button", { name: "Sign & send anyway" })).toBeVisible();
    await preview("Award a hunt", { Hunt: "0", Winner: "lucy.boo", "Pull request number": "4" });

    await page.goto("/programs/pool");
    await preview("Fund the pool", { Amount: "1" });
    await preview("Vouch for a newcomer", { Newcomer: "renfield.boo" });
    await preview("Withdraw vested rewards", {});

    await page.goto("/programs/validators");
    await preview("Stake with a validator", { Validator: VALIDATOR, Amount: "1" });
    await preview("Unstake", { Validator: VALIDATOR, Amount: "1" });
    await preview("Withdraw unbonded stake", {});
    await preview("Set commission", { "Commission (basis points)": "500" });
    expect(await cspViolations(page)).toEqual([]);
    expect(errors).toEqual([]);
    expect(node.submitted.length).toBe(0);
  });

  test("a payment and an award to a name sign the name, so a lying node can't redirect them (audit main13-signing-1)", async ({ page, node }) => {
    const alice = addressOf("a1".repeat(32));
    const bob = addressOf("b0".repeat(32));
    const mallory = addressOf("66".repeat(32));
    node.names.set("alice", alice);
    node.names.set("bob", bob);
    node.nameLies.set("alice", mallory);
    node.nameLies.set("bob", mallory);
    node.credit(me, "100");

    await visit(page, "/programs/accounts?do=transfer&to=alice.boo&amount=1", (p) => p.locator(".banner"), errors);
    await page.getByRole("button", { name: "Unlock to sign" }).first().click();
    await page.getByRole("button", { name: "Preview" }).click();
    await expect(page.getByTestId("signed-name")).toHaveText("alice.boo");
    await expect(page.getByTestId("by-chain")).toContainText("Signed as the name: the chain acts on whoever owns alice.boo when this transaction runs");
    await expect(page.getByTestId("by-chain")).toContainText(mallory);
    await page.getByRole("button", { name: "Sign & send" }).click();
    await expect(receipt(page)).toBeVisible();
    expect((node.submitted.at(-1) as Tx).action).toEqual({ type: "transfer", to: "alice.boo", amount: "1000000000" });
    expect(node.accounts.get(alice)?.balance).toBe(1_000_000_000n);
    expect(node.accounts.get(mallory)?.balance ?? 0n).toBe(0n);
    await expect(page.getByText("signed to alice.boo: the account that owned it when this ran")).toBeVisible();

    await visit(page, "/hunt/0", (p) => p.getByRole("heading", { name: "You put this up" }), errors);
    await page.getByRole("button", { name: "Unlock to sign" }).first().click();
    await page.locator("summary", { hasText: "Award it" }).click();
    const award = page.locator("details.action[open]");
    await award.getByLabel("Winner", { exact: true }).fill("bob.boo");
    await award.getByLabel("Pull request number", { exact: true }).fill("9");
    await award.getByRole("button", { name: "Preview" }).click();
    await expect(award.getByTestId("signed-name")).toHaveText("bob.boo");
    await award.getByRole("button", { name: "Sign & send" }).click();
    await expect(receipt(page)).toBeVisible();
    expect((node.submitted.at(-1) as Tx).action).toMatchObject({ type: "award_hunt", hunt_id: 0, to: "bob.boo", pr: 9 });
    expect(node.accounts.get(bob)?.balance).toBe(25_000_000_000n);
    expect(node.accounts.get(mallory)?.balance ?? 0n).toBe(0n);
  });

  test("a validator field signs an address, and says when only this node vouched for it", async ({ page, node }) => {
    node.names.set("fangs", VALIDATOR);
    node.credit(me, "100");
    await visit(page, "/programs/validators?do=delegate&validator=fangs.boo&amount=2", (p) => p.locator(".banner"), errors);
    await page.getByRole("button", { name: "Unlock to sign" }).first().click();
    await page.getByRole("button", { name: "Preview" }).click();
    await expect(page.getByTestId("signed-name")).toHaveText(VALIDATOR);
    await expect(page.getByText("Looked up on your node only, not proven. Check this address before you sign.")).toBeVisible();
    await page.getByRole("button", { name: "Sign & send" }).click();
    await expect(receipt(page)).toBeVisible();
    expect((node.submitted.at(-1) as Tx).action).toEqual({ type: "delegate", validator: VALIDATOR, amount: "2000000000" });
  });

  test("a hunt's expiry is days of the chain's blocks from the height it's signed against (audit main13-signing-4)", async ({ page, node }) => {
    // A node that says blocks come ten times faster than they do can't stretch the escrow tenfold.
    node.statusExtra = { block_time_ms: 100 };
    node.credit(me, "100");
    await visit(page, "/programs/hunts?do=create_hunt&repo=jackdishman/blockula&issue=21&title=Ghosts&amount=10&days=30", (p) => p.locator(".banner"), errors);
    await page.getByRole("button", { name: "Unlock to sign" }).first().click();
    await page.getByRole("button", { name: "Preview" }).click();
    await expect(page.getByTestId("expires")).toContainText("30 days of blocks after #");
    await page.getByRole("button", { name: "Sign & send" }).click();
    await expect(receipt(page)).toBeVisible();
    const tx = node.submitted.at(-1) as Tx;
    expect(tx.action.type).toBe("create_hunt");
    // valid_until is counted from the same height, 600 blocks on.
    expect((tx.action as { expires_at: number }).expires_at - (tx.valid_until - 600)).toBe(30 * 86_400);
  });

  test("notices: a payment arrives; an agent's spend is caught even when dust pushes it out of view (audit main13-signing-6)", async ({ page, node }) => {
    await visit(page, "/explore", (p) => p.getByRole("heading", { name: "blockula-testnet, live" }), errors);
    await page.waitForTimeout(4500); // the first look is the baseline
    node.add({ kind: "transfer", from: w.mina, to: me, amount: 3_000_000_000n, fee: 64_920n, memo: "", summary: "mina.boo sent 3 FANG to vlad.boo", action: { type: "transfer", to: me, amount: "3000000000" } });
    await expect(page.locator(".notice", { hasText: "You received" })).toBeVisible({ timeout: 9000 });

    // An agent spends, then a stranger's 26 dust payments push it out of the node's 25-entry list.
    node.add({ kind: "transfer", from: me, to: w.renfield, amount: 4_000_000_000n, fee: 64_920n, memo: "", summary: "vlad.boo sent 4 FANG to renfield.boo", signer: "a9".repeat(32), action: { type: "transfer", to: w.renfield, amount: "4000000000" } });
    node.acct(me).nonce += 1;
    node.spent.set("a9".repeat(32), (node.spent.get("a9".repeat(32)) ?? 0n) + 4_000_064_920n);
    for (let i = 0; i < 26; i++) node.add({ kind: "transfer", from: w.lucy, to: me, amount: 1n, fee: 64_920n, memo: "", summary: "lucy.boo sent 0.000000001 FANG to vlad.boo", action: { type: "transfer", to: me, amount: "1" } });
    await expect(page.locator(".notice.agent", { hasText: "Your agent “claude" })).toBeVisible({ timeout: 9000 });
    expect(await cspViolations(page)).toEqual([]);
    expect(errors).toEqual([]);
  });

  test("a payment to a watched account that seals between the two reads is no missed spend (audit review-main13-4)", async ({ page, node }) => {
    await page.addInitScript(([key, address]) => localStorage.setItem(key, JSON.stringify([{ address, label: "mina.boo", at: 1 }])), [`blockula.watch:${RPC}`, w.mina]);
    // No WatchingList on this page: only Alerts can consume the watched-account hook (polish P4).
    await visit(page, "/programs", (p) => p.getByRole("heading", { name: "What the chain can do" }), errors);
    await page.waitForTimeout(4500); // the first look is the baseline
    // A round reads this account, then the watched one; a payment from this account to the watched
    // one seals in between (just before the watched account's read, which only these notices
    // make): that read announces it first, and this account's next read sees its nonce moved.
    node.beforeAccountRead = (id) => {
      if (id !== w.mina) return;
      node.beforeAccountRead = null;
      node.add({ kind: "transfer", from: me, to: w.mina, amount: 2_000_000_000n, fee: 64_920n, memo: "", summary: "vlad.boo sent 2 FANG to mina.boo", action: { type: "transfer", to: w.mina, amount: "2000000000" } });
      node.acct(me).nonce += 1;
    };
    await expect(page.locator(".notice", { hasText: "Watching mina.boo" })).toBeVisible({ timeout: 9000 });
    // Through this account's next read: no notice says a spend was pushed out of view. (Sampled,
    // not retried until it passes: a notice that showed and was dismissed still counts.)
    const shown = new Set<string>();
    for (let i = 0; i < 24; i++) {
      for (const text of await page.locator(".notice").allInnerTexts()) shown.add(text);
      await page.waitForTimeout(250);
    }
    expect([...shown].filter((text) => text.includes("pushed out of view"))).toEqual([]);
    expect(errors).toEqual([]);
  });

  test("notices stop when the node turns out to be on another chain (audit main13-signing-5)", async ({ page, node }) => {
    await visit(page, "/explore", (p) => p.getByRole("heading", { name: "blockula-testnet, live" }), errors);
    await page.waitForTimeout(4500);
    node.chainId = "blockula-evil";
    await expect(page.getByTestId("wrong-chain")).toBeVisible({ timeout: 9000 });
    node.add({ kind: "transfer", from: w.mina, to: me, amount: 1000_000_000_000n, fee: 64_920n, memo: "", summary: "mina.boo sent 1,000 FANG to vlad.boo", action: { type: "transfer", to: me, amount: "1000000000000" } });
    await page.waitForTimeout(9000);
    await expect(page.locator(".notice")).toHaveCount(0);
  });
});

