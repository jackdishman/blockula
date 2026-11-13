// The blackjack table (app/blackjack), against the mock node with a table on it
// (e2e/blackjack-world.ts: the program's own storage layout, checks and events, and a dealer this
// spec drives with real hash chains): the 18+ gate, a bet through the wallet's review-then-sign
// flow, the deal, a hit and a stand, the settlement, "Check this hand" verifying every card in the
// browser, the dealer's missed deadline and the claim, the note on every page, the spectator view
// and who may frame it, and the share page's card. All under the production build's strict CSP.
import { POSTER_ALT, Outcome, call, cardLabel, outcomeWords, total } from "@blockula/blackjack";
import type { Tx } from "@blockula/sdk";
import type { Page } from "@playwright/test";
import { addTable, HIT_THEN_STAND, PLAYERS_TURN, TABLE } from "./blackjack-world";
import { PORT } from "./env";
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";
import { addressOf } from "./mock-node";

const F = 10n ** 9n;
const NOTE = "FANG has no value and can't be bought; no prizes.";
const OTHER = addressOf("d4".repeat(32));
const cards = (cs: readonly number[]) => cs.map(cardLabel).join(" ");

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

async function clean(page: Page, errors: string[]) {
  expect(await cspViolations(page)).toEqual([]);
  expect(errors).toEqual([]);
}

/** Reviews what's on screen and signs it; waits for it to be final. */
async function sign(page: Page, does: RegExp) {
  const review = page.getByTestId("bj-review");
  await expect(review.getByTestId("bj-review-does")).toHaveText(does);
  await review.getByTestId("bj-sign").click();
  await expect(page.getByTestId("bj-sent")).toHaveAttribute("data-state", "applied");
}

test.describe("the blackjack table", () => {
  let errors: string[];
  test.beforeEach(async ({ page }) => {
    errors = watchErrors(page);
    await watchCsp(page);
  });

  test("a hand from the bet to the settlement: the gate, review-then-sign, the deal, a hit, a stand, and every card verified", async ({ page, node }) => {
    test.setTimeout(120_000);
    const { account: me } = await createWallet(page);
    const table = addTable(node);
    table.script = HIT_THEN_STAND;
    await page.goto(`/blackjack?table=${TABLE}`);

    // The note, the rules with the chain's limits, and which program the table is.
    await expect(page.getByTestId("bj-note")).toHaveText(NOTE);
    await expect(page.getByTestId("bj-table")).toContainText(TABLE);
    await expect(page.getByTestId("bj-table").getByRole("link", { name: "See the program's record" })).toHaveAttribute("href", `/program/${TABLE}`);
    const rules = page.getByTestId("bj-rules");
    await expect(rules).toContainText("This is a game played with FANG.");
    await expect(rules).toContainText("A bet is from 1 FANG to 50 FANG, and never more than 1/20 of the house's free bankroll.");
    await expect(rules).toContainText("You have 1,800 blocks (about 30 min) to move.");
    await expect(rules).toContainText("The dealer has 150 blocks (about 3 min) to reveal each card it owes.");
    await expect(page.getByTestId("bj-house")).toContainText("2000");

    // Locked: the table can be watched, nothing can be signed. Unlock with the passkey.
    const form = page.getByTestId("bj-bet-form");
    await expect(form.getByTestId("bj-bet-input")).toBeDisabled();
    await form.getByRole("button", { name: "Unlock" }).click();

    // The 18+ gate: without it the bet form is off.
    const reviewBet = form.getByTestId("bj-bet-review");
    await expect(reviewBet).toBeDisabled();
    await expect(form.getByTestId("bj-bet-input")).toBeDisabled();
    await expect(form.getByTestId("bj-gate-needed")).toHaveText("Confirm you are 18 or older to play.");
    await page.getByTestId("bj-gate").getByRole("checkbox").check();
    await expect(reviewBet).toBeEnabled();
    expect(await page.evaluate(() => localStorage.getItem("blockula.blackjack.adult"))).toBe("1");

    // The form is held to the table's limits, read from the chain.
    await form.getByTestId("bj-bet-input").fill("0.5");
    await reviewBet.click();
    await expect(form.getByTestId("bj-bet-problem")).toHaveText("The smallest bet is 1 FANG.");
    await form.getByTestId("bj-bet-input").fill("51");
    await expect(form.getByTestId("bj-bet-problem")).toHaveText("The largest bet right now is 50 FANG.");
    await expect(page.getByTestId("bj-review")).toHaveCount(0);
    expect(node.submitted).toHaveLength(0);

    // The bet, reviewed: the program, the method's input (the seed this browser just made), the FANG sent,
    // the storage limit, and the whole version digest the call binds.
    await form.getByTestId("bj-bet-input").fill("2");
    await reviewBet.click();
    const review = page.getByTestId("bj-review");
    await expect(review.getByTestId("bj-review-program")).toHaveText(TABLE);
    await expect(review.getByTestId("bj-review-digest")).toHaveText(table.digest);
    await expect(review.getByTestId("bj-review-value")).toContainText("2");
    await expect(review.getByTestId("bj-review-storage")).toContainText("0.25");
    await expect(review.getByTestId("bj-review-fee")).toContainText("fuel included");
    const seed = (await review.getByTestId("bj-review-seed").textContent()) ?? "";
    expect(seed).toMatch(/^[0-9a-f]{64}$/);
    await expect(review.getByTestId("bj-review-input")).toHaveText(seed);
    const before = node.acct(me).balance;
    await sign(page, /^Bet 2 FANG on a new hand$/);
    const bet = node.submitted.at(-1) as Tx;
    expect(bet.action).toMatchObject({ type: "call_program", storage_limit: (F / 4n).toString(), bindings: [], calls: [{ program: TABLE, version_digest: table.digest, method: "bet", input: seed, value: (2n * F).toString() }] });
    expect(node.acct(me).balance).toBe(before - 2n * F - BigInt(bet.fee));

    // The hand is open and waits for the dealer, with its deadline in blocks.
    const hand = page.getByTestId("bj-open-hand");
    await expect(hand.getByTestId("bj-status")).toContainText("Waiting for the dealer…");
    await expect(hand.getByTestId("bj-status")).toContainText(/It has \d+ blocks \(about \d+ min\) left to reveal the deal/);
    await expect(hand.getByTestId("bj-log")).toContainText("You bet 2 FANG. Hand 0.");
    await expect(hand.getByTestId("bj-hit")).toHaveCount(0);
    const game = table.openHand(me);
    expect(game.clientSeed).toBe(seed);

    // The dealer deals: two cards to the player, one up for the dealer, the other face down.
    expect(table.deal(game.id)).toBeNull();
    await expect(hand.getByTestId("bj-player-cards")).toContainText(cards(game.playerCards));
    await expect(hand.getByTestId("bj-dealer-cards")).toContainText(`${cards(game.dealerCards)}, and one card face down`);
    await expect(hand.getByTestId("bj-stake")).toHaveText("2 FANG");
    await expect(hand.getByTestId("bj-status")).toContainText("Your move: hit or stand.");
    // The picture is the table's SVG, inline, with every card in its label.
    const svg = hand.getByTestId("bj-svg");
    await expect(svg.locator("svg")).toBeVisible();
    await expect(svg).toHaveAttribute("aria-label", /A blackjack table on Blockula, hand 0\..*Bet: 2 FANG\. Your move: hit or stand\./);
    await expect(svg).toHaveAttribute("role", "img");

    // Mid-hand, the check already holds: the cards so far, down to the commitment the bet took.
    await hand.getByTestId("bj-check-open").click();
    const verify = hand.getByTestId("bj-verify");
    await expect(verify.getByTestId("bj-verdict")).toHaveText("Verified. Every card comes from the dealer's commitment and your seed.");
    await expect(verify.getByTestId("bj-verify-commit")).toHaveText(table.chains.get(game.id)![0]);
    await expect(verify.getByTestId("bj-verify-commit-check")).toHaveText("It is the commitment your bet took.");
    await expect(verify.getByTestId("bj-verify-seed")).toHaveText(seed);
    await expect(verify.getByTestId("bj-verify-steps").locator("li")).toHaveCount(3);
    await expect(verify.getByTestId("bj-verify-outcome")).toContainText("isn't settled yet");
    await hand.getByTestId("bj-check-open").click();

    // Hit: reviewed and signed like the bet, then the dealer's card.
    await hand.getByTestId("bj-hit").click();
    await expect(page.getByTestId("bj-review-digest")).toHaveText(table.digest);
    await expect(page.getByTestId("bj-review-value")).toContainText("0");
    await sign(page, /^Hit: take one more card \(hand 0\)$/);
    expect((node.submitted.at(-1) as Tx).action).toMatchObject({ type: "call_program", storage_limit: "0", calls: [{ method: "hit", input: call.hit(game.id).input, value: "0", version_digest: table.digest }] });
    await expect(hand.getByTestId("bj-status")).toContainText("left to reveal the next card");
    expect(table.deal(game.id)).toBeNull();
    expect(game.playerCards).toHaveLength(3);
    await expect(hand.getByTestId("bj-player-cards")).toContainText(`${cards(game.playerCards)} (${total(game.playerCards).soft ? "soft " : ""}${total(game.playerCards).total})`);
    await expect(hand.getByTestId("bj-log")).toContainText("You hit.");

    // Stand: the dealer plays its hand out and the hand is settled.
    await hand.getByTestId("bj-stand").click();
    await sign(page, /^Stand: take no more cards and let the dealer play \(hand 0\)$/);
    expect((node.submitted.at(-1) as Tx).action).toMatchObject({ calls: [{ method: "stand", input: call.stand(game.id).input }] });
    const outcome = table.deal(game.id) as Outcome;
    expect(outcome).not.toBeNull();
    const settled = table.seat(me).last!;
    const last = page.getByTestId("bj-last-hand");
    const came = settled.payout === 0n ? "Nothing came back from the 2 FANG bet." : settled.payout === 2n * F ? "The 2 FANG bet came back." : "4 FANG came back from a 2 FANG bet.";
    await expect(last.getByTestId("bj-last-outcome")).toHaveText(`${outcomeWords(outcome)}. ${came}`);
    await expect(page.getByTestId("bj-open-hand")).toHaveCount(0);
    await expect(page.getByTestId("bj-no-hand")).toBeVisible();
    await expect(last.getByTestId("bj-svg")).toHaveAttribute("aria-label", new RegExp(`${outcomeWords(outcome)}\\.`));

    // "Check this hand": every link, the card it gives, both hands, the outcome and what came back, with the verdict.
    await last.getByTestId("bj-check-last").click();
    const checked = last.getByTestId("bj-verify");
    await expect(checked).toHaveAttribute("data-ok", "yes");
    await expect(checked.getByTestId("bj-verdict")).toHaveText("Verified. Every card comes from the dealer's commitment and your seed.");
    await expect(checked.getByTestId("bj-verify-commit")).toHaveText(table.chains.get(game.id)![0]);
    const chain = table.chains.get(game.id)!;
    const steps = checked.getByTestId("bj-verify-steps").locator("li");
    await expect(steps).toHaveCount(settled.dealt);
    for (let j = 1; j <= settled.dealt; j++) await expect(steps.nth(j - 1)).toContainText(chain[j]);
    await expect(steps.nth(0)).toContainText("gives card 1");
    await expect(steps.nth(1)).toContainText("to the dealer");
    await expect(steps.nth(3)).toContainText("to you");
    await expect(checked.getByTestId("bj-verify-player")).toContainText(cards(game.playerCards));
    await expect(checked.getByTestId("bj-verify-dealer")).toContainText(cards(game.dealerCards));
    await expect(checked.getByTestId("bj-verify-outcome")).toContainText(`${outcomeWords(outcome)}. ${came}`);

    // The seed and the commitment are kept in this browser, per hand, for this chain's table.
    const kept = await page.evaluate((t) => JSON.parse(localStorage.getItem(`blockula.blackjack.blockula-testnet.${t}`) ?? "null"), TABLE);
    expect(kept.hands["0"]).toMatchObject({ seed, commit: chain[0], game: "0" });
    expect(kept.hands["0"].txs).toHaveLength(3);
    expect(kept.pending).toEqual([]);

    // A hand the node misreports doesn't verify: another last link than the chain's.
    table.seat(me).last = { ...settled, link: "00".repeat(32) };
    await expect(checked.getByTestId("bj-verdict")).toHaveText("Not verified: these links don't hash down to the commitment the bet took.");
    await expect(checked).toHaveAttribute("data-ok", "no");
    table.seat(me).last = settled;
    await expect(checked).toHaveAttribute("data-ok", "yes");

    // The bet form is back, and a new bet gets a new seed.
    await expect(page.getByTestId("bj-bet-form").getByTestId("bj-bet-range")).toContainText("498 FANG left today");
    await clean(page, errors);
  });

  test("a dealer that misses its deadline: the player claims the hand, and the timeout verifies", async ({ page, node }) => {
    test.setTimeout(90_000);
    const { account: me } = await createWallet(page);
    const table = addTable(node);
    table.script = PLAYERS_TURN;
    await page.goto(`/blackjack?table=${TABLE}`);
    await page.getByTestId("bj-bet-form").getByRole("button", { name: "Unlock" }).click();
    await page.getByTestId("bj-gate").getByRole("checkbox").check();
    await page.getByTestId("bj-bet-input").fill("2");
    await page.getByTestId("bj-bet-review").click();
    await sign(page, /^Bet 2 FANG/);
    const hand = page.getByTestId("bj-open-hand");
    await expect(hand.getByTestId("bj-status")).toContainText("Waiting for the dealer…");
    await expect(hand.getByTestId("bj-claim")).toHaveCount(0);

    // The dealer never deals, and its 150 blocks pass.
    node.height += 200;
    await expect(hand.getByTestId("bj-status")).toHaveText("The dealer ran out of time. The hand can be claimed for the player.");
    const before = node.acct(me).balance;
    await hand.getByTestId("bj-claim").click();
    await sign(page, /^Claim the hand: the dealer missed its deadline, so this settles the hand in the player's favor \(hand 0\)$/);
    const claim = node.submitted.at(-1) as Tx;
    expect(claim.action).toMatchObject({ calls: [{ method: "timeout", input: call.timeout(0n).input, value: "0" }] });
    // Two and a half times the bet: a natural was still possible.
    expect(node.acct(me).balance).toBe(before + 5n * F - BigInt(claim.fee));

    const last = page.getByTestId("bj-last-hand");
    await expect(last.getByTestId("bj-last-outcome")).toHaveText("You win: the dealer never dealt. 5 FANG came back from a 2 FANG bet.");
    await last.getByTestId("bj-check-last").click();
    await expect(last.getByTestId("bj-verdict")).toHaveText("Verified. Every card comes from the dealer's commitment and your seed.");
    await expect(last.getByTestId("bj-verify-outcome")).toContainText("the hand ended by timeout, not by the cards");
    await clean(page, errors);
  });

  test("a call the table refuses says why in the table's words, and was charged; a paused table takes no bets", async ({ page, node }) => {
    const { account: me } = await createWallet(page);
    const table = addTable(node);
    await page.goto(`/blackjack?table=${TABLE}`);
    await page.getByTestId("bj-bet-form").getByRole("button", { name: "Unlock" }).click();
    await page.getByTestId("bj-gate").getByRole("checkbox").check();
    await page.getByTestId("bj-bet-input").fill("2");
    await page.getByTestId("bj-bet-review").click();
    await expect(page.getByTestId("bj-review")).toBeVisible();
    // Between the review and the signature, the last commitment is taken by another player's bet.
    table.house.nextCommit = table.house.nextGame;
    const before = node.acct(me).balance;
    await page.getByTestId("bj-sign").click();
    const sent = page.getByTestId("bj-sent");
    await expect(sent).toHaveAttribute("data-state", "failed");
    await expect(sent.getByTestId("bj-sent-words")).toHaveText("The dealer isn't ready for a new hand yet. Try again in a moment.");
    await expect(sent).toContainText("Its fee was charged.");
    expect(node.acct(me).balance).toBe(before - BigInt((node.submitted.at(-1) as Tx).fee));
    await expect(page.getByTestId("bj-bet-blocked")).toHaveText("The dealer isn't ready for a new hand yet. Try again in a moment.");

    table.house.paused = true;
    await expect(page.getByTestId("bj-bet-blocked")).toHaveText("New bets are paused. Open hands play on.");
    await expect(page.getByTestId("bj-bet-review")).toHaveCount(0);
    await expect(page.getByTestId("bj-house")).toContainText("paused");
  });

  test("signed out, the table can be watched and nothing is kept; without a table the page says so", async ({ page, node }) => {
    addTable(node);
    await page.goto(`/blackjack?table=${TABLE}`);
    await expect(page.getByTestId("bj-note")).toHaveText(NOTE);
    await expect(page.getByTestId("bj-house")).toContainText("0 of 10");
    await expect(page.getByTestId("bj-sign-in")).toContainText("Sign in or create a wallet to play.");
    await expect(page.getByTestId("bj-bet-form")).toHaveCount(0);
    await expect(page.getByTestId("bj-rules")).toContainText("Why the cards are fair");
    expect(await page.evaluate(() => Object.keys(localStorage).filter((k) => k.startsWith("blockula.blackjack")))).toEqual([]);
    await clean(page, errors);

    // No table in the build and none in the link.
    await page.goto("/blackjack");
    await expect(page.getByTestId("bj-no-table")).toContainText("No table on this chain yet");
    await expect(page.getByTestId("bj-note")).toHaveText(NOTE);
    // A name, markup, or an address that isn't a table.
    await page.goto("/blackjack?table=blackjack.boo");
    await expect(page.getByTestId("bj-bad-table")).toContainText("never by a .boo name");
    await page.goto(`/blackjack?table=${encodeURIComponent('<img src=x onerror="window.__pwned=7">')}`);
    await expect(page.getByTestId("bj-bad-table")).toBeVisible();
    expect(await page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned)).toBeUndefined();
    expect(await cspViolations(page)).toEqual([]);
  });

  test("an address that is no table says so", async ({ page }) => {
    await page.goto(`/blackjack?table=${OTHER}`);
    await expect(page.getByTestId("bj-not-a-table")).toContainText("This program isn't a blackjack table that's set up.");
    await expect(page.getByTestId("bj-note")).toHaveText(NOTE);
  });
});

test.describe("the spectator view and the share card", () => {
  test("the embed shows a hand with no wallet around it, and only X may frame it", async ({ page, node, request }) => {
    const errors = watchErrors(page);
    await watchCsp(page);
    const table = addTable(node);
    table.script = PLAYERS_TURN;
    const id = table.place(OTHER, 3n * F);
    table.deal(id);
    const game = table.openHand(OTHER);
    const path = `/blackjack/embed?table=${TABLE}&player=${OTHER}`;
    const res = await page.goto(path);
    const embed = page.getByTestId("bj-embed");
    await expect(embed.getByTestId("bj-svg")).toHaveAttribute("aria-label", new RegExp(`A blackjack table on Blockula, hand ${id}\\..*Bet: 3 FANG\\.`));
    await expect(embed.getByTestId("bj-svg").locator("svg")).toBeVisible();
    await expect(embed.getByTestId("bj-svg")).toHaveAttribute("aria-label", /hit or stand/);
    await expect(embed.getByTestId("bj-note")).toHaveText(NOTE);
    // No wallet around it: no header, no footer, nothing to sign, nothing kept.
    await expect(page.locator("header.top")).toBeHidden();
    await expect(page.locator(".imprint")).toBeHidden();
    await expect(page.getByRole("button")).toHaveCount(0);
    expect(await page.evaluate(() => Object.keys(localStorage).filter((k) => k.startsWith("blockula.blackjack")))).toEqual([]);
    const out = embed.getByTestId("bj-embed-open");
    await expect(out).toHaveAttribute("href", `/blackjack?table=${TABLE}`);
    await expect(out).toHaveAttribute("target", "_blank");
    // By its hand, too; and the hand moves as the table does.
    await page.goto(`/blackjack/embed?table=${TABLE}&game=${id}`);
    await expect(page.getByTestId("bj-svg")).toHaveAttribute("aria-label", new RegExp(`hand ${id}\\.`));
    expect(game.playerCards).toHaveLength(2);
    expect(await cspViolations(page)).toEqual([]);
    expect(errors).toEqual([]);

    // Its headers: X (and this origin) may frame it, and nothing else about its policy is looser.
    test.skip(!!process.env.E2E_DEV, "next dev relaxes the CSP");
    const csp = res!.headers()["content-security-policy"];
    expect(csp).toContain("frame-ancestors 'self' https://x.com https://twitter.com;");
    expect(csp).toContain("frame-src 'none'");
    expect(csp).toMatch(/script-src 'self' 'nonce-[^']+' 'strict-dynamic';/);
    expect(res!.headers()["x-frame-options"]).toBeUndefined();
    expect(res!.headers()["x-content-type-options"]).toBe("nosniff");
    // Every other route is never framed, the table page and the share page included.
    for (const other of [`/blackjack?table=${TABLE}`, "/blackjack/share", "/blackjack/embedded", "/welcome", "/explore", "/"]) {
      const r = await request.get(other);
      expect(r.headers()["x-frame-options"], other).toBe("DENY");
      expect(r.headers()["content-security-policy"], other).toContain("frame-ancestors 'none'");
    }
    for (const same of ["/blackjack/embed", "/blackjack/embed/"]) {
      const r = await request.get(same, { maxRedirects: 0 });
      if (r.status() === 200) expect(r.headers()["x-frame-options"], same).toBeUndefined();
    }
  });

  test("in a frame: x.com shows the embed, another site doesn't, and the table page is never framed", async ({ page, node }) => {
    test.skip(!!process.env.E2E_DEV, "next dev relaxes the CSP");
    const table = addTable(node);
    table.script = PLAYERS_TURN;
    table.deal(table.place(OTHER, 3n * F));
    const origin = `http://localhost:${PORT}`;
    const framing = (src: string) => `<!doctype html><title>a post</title><iframe id="card" width="480" height="330" src="${origin}${src}"></iframe>`;
    const embed = `/blackjack/embed?table=${TABLE}&player=${OTHER}`;
    // The posts' pages are made up here (nothing is fetched from X), and Chrome asks before a public page reaches
    // this machine's own server: allowed for all three, so only the embed's policy decides who frames it.
    for (const host of ["x.com", "twitter.com", "elsewhere.test"]) await page.context().grantPermissions(["local-network-access"], { origin: `https://${host}` });
    await page.route(/^https:\/\/(x\.com|twitter\.com|elsewhere\.test)\//, (route) => route.fulfill({ contentType: "text/html", body: framing(new URL(route.request().url()).searchParams.get("src") ?? "") }));

    for (const host of ["x.com", "twitter.com"]) {
      await page.goto(`https://${host}/post?src=${encodeURIComponent(embed)}`);
      const card = page.frameLocator("#card");
      await expect(card.getByTestId("bj-svg"), host).toHaveAttribute("aria-label", /A blackjack table on Blockula, hand 0\./);
      await expect(card.getByTestId("bj-note")).toHaveText(NOTE);
      await expect(card.locator("header.top")).toBeHidden();
    }
    // Another site's frame is refused by the embed's own policy.
    await page.goto(`https://elsewhere.test/post?src=${encodeURIComponent(embed)}`);
    await page.waitForTimeout(1500);
    await expect(page.frameLocator("#card").getByTestId("bj-embed")).toHaveCount(0);
    // And X can't frame the table page, where a wallet signs.
    await page.goto(`https://x.com/post?src=${encodeURIComponent(`/blackjack?table=${TABLE}`)}`);
    await page.waitForTimeout(1500);
    await expect(page.frameLocator("#card").getByTestId("bj-note")).toHaveCount(0);
  });

  test("the share page carries X's player card and the matching Open Graph tags", async ({ page, node, request }) => {
    addTable(node);
    const res = await request.get(`/blackjack/share?table=${TABLE}&player=${OTHER}`);
    expect(res.status()).toBe(200);
    const html = await res.text();
    const meta = (attr: "name" | "property", key: string) => new RegExp(`<meta ${attr}="${key}" content="([^"]*)"`).exec(html)?.[1]?.replace(/&amp;/g, "&").replace(/&#x27;/g, "'");
    // The e2e build's passkey host is the default one: the card's URLs are absolute https ones on it.
    const at = "https://testnet.blockula.app";
    expect(meta("name", "twitter:card")).toBe("player");
    expect(meta("name", "twitter:title")).toBe("Blockula Blackjack");
    expect(meta("name", "twitter:description")).toContain("played with FANG");
    expect(meta("name", "twitter:description")).toContain(NOTE);
    expect(meta("name", "twitter:player")).toBe(`${at}/blackjack/embed?table=${TABLE}&player=${OTHER}`);
    expect(meta("name", "twitter:player:width")).toBe("480");
    expect(meta("name", "twitter:player:height")).toBe("330");
    expect(meta("name", "twitter:image")).toBe(`${at}/blackjack/poster.png`);
    expect(meta("name", "twitter:image:alt")).toBe(POSTER_ALT);
    expect(meta("property", "og:title")).toBe("Blockula Blackjack");
    expect(meta("property", "og:description")).toContain(NOTE);
    expect(meta("property", "og:type")).toBe("website");
    expect(meta("property", "og:url")).toBe(`${at}/blackjack/share?table=${TABLE}&player=${OTHER}`);
    expect(meta("property", "og:image")).toBe(`${at}/blackjack/poster.png`);
    expect(meta("property", "og:image:alt")).toBe(POSTER_ALT);
    // Only addresses come from the link: markup in it never reaches a tag.
    const hostile = await (await request.get(`/blackjack/share?table=${encodeURIComponent('"><script>alert(1)</script>')}`)).text();
    expect(hostile).not.toContain("<script>alert(1)</script>");
    expect(new RegExp('<meta name="twitter:player" content="([^"]*)"').exec(hostile)?.[1]).toBe(`${at}/blackjack/embed`);

    // The page itself: the poster with its alt text, the note, and the way to the table.
    await watchCsp(page);
    await page.goto(`/blackjack/share?table=${TABLE}`);
    const share = page.getByTestId("bj-share");
    await expect(share.getByRole("img", { name: POSTER_ALT })).toBeVisible();
    expect(await share.locator("img.bj-poster").evaluate((img: HTMLImageElement) => img.naturalWidth)).toBeGreaterThan(0);
    await expect(share.getByTestId("bj-note")).toHaveText(NOTE);
    await expect(share.getByTestId("bj-share-open")).toHaveAttribute("href", `/blackjack?table=${TABLE}`);
    expect(await cspViolations(page)).toEqual([]);
    await share.getByTestId("bj-share-open").click();
    await expect(page.getByTestId("bj-table")).toContainText(TABLE);
  });
});
