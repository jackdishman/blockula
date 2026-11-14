// The blackjack table (app/blackjack), against the mock node with a table on it
// (e2e/blackjack-world.ts: the program's own storage layout, checks and events, and a dealer this
// spec drives with real hash chains): the 18+ gate, a bet in its two transactions through the
// wallet's review-then-sign flow (the bet with the seed's hash, then the seed's reveal, which the
// page presents by itself, again after a reload, and never when the seed isn't in this browser),
// a seed that isn't revealed in time, the deal, a hit and a stand, the settlement, "Check this hand" verifying every card in the
// browser, the dealer's missed deadline and the claim, the note on every page, the spectator view
// and who may frame it, and the share page's card. All under the production build's strict CSP.
import { POSTER_ALT, Outcome, Phase, call, cardLabel, outcomeWords, seedHash, total } from "@blockula/blackjack";
import type { Tx } from "@blockula/sdk";
import type { Page } from "@playwright/test";
import { addTable, HIT_THEN_STAND, PLAYERS_TURN, TABLE, type MockTable } from "./blackjack-world";
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

const STORE = `blockula.blackjack.blockula-testnet.${TABLE}`;
const VERIFIED = "Verified. Every card comes from the dealer's commitment and your seed. The seed is the one the bet committed to.";
const STEP_2 = /^Reveal your seed for hand 0 \(step 2 of 2\)$/;

/** What this browser keeps for the table: the seeds it made, by hand. */
const kept = (page: Page) => page.evaluate((k) => JSON.parse(localStorage.getItem(k) ?? "null") as { pending: { seed: string; seedHash?: string }[]; hands: Record<string, { seed: string; seedHash?: string; commit?: string; game?: string; txs: string[] }> } | null, STORE);

/**
 * Step 1 of 2: fills the bet, reviews it and signs it. The seed the page just made is read from
 * its storage and told to the mock dealer first, so a scripted hand can be dealt (the bet itself
 * carries only the hash). Resolves once the bet ran and the page has presented step 2 by itself.
 */
async function placeBet(page: Page, table: MockTable, amount: string): Promise<string> {
  await page.getByTestId("bj-bet-input").fill(amount);
  await page.getByTestId("bj-bet-review").click();
  await expect(page.getByTestId("bj-review-does")).toHaveText(new RegExp(`^Bet ${amount} FANG on a new hand \\(step 1 of 2\\)$`));
  const seed = (await kept(page))!.pending.at(-1)!.seed;
  table.knownSeeds.push(seed);
  await page.getByTestId("bj-sign").click();
  await expect(page.getByTestId("bj-review-does")).toHaveText(STEP_2);
  return seed;
}

/** Both transactions of a bet: the hand then waits for the dealer. */
async function bet(page: Page, table: MockTable, amount: string): Promise<string> {
  const seed = await placeBet(page, table, amount);
  await sign(page, STEP_2);
  return seed;
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
    await expect(rules).toContainText("you have 300 blocks (about 5 min) to reveal your seed.");
    await expect(rules).toContainText("The house can't get ahead of your bet, because your seed stays hidden until your bet already holds its commitment.");
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

    // Step 1 of 2, the bet, reviewed: the program, the method's input (the hash of the seed this
    // browser just made, never the seed), the FANG sent, the storage limit, and the whole version
    // digest the call binds.
    await expect(form.getByTestId("bj-bet-two")).toHaveText("A bet is two transactions: the bet, then your seed. Each one is signed with your passkey.");
    await form.getByTestId("bj-bet-input").fill("2");
    await reviewBet.click();
    const review = page.getByTestId("bj-review");
    await expect(review.getByTestId("bj-review-program")).toHaveText(TABLE);
    await expect(review.getByTestId("bj-review-digest")).toHaveText(table.digest);
    await expect(review.getByTestId("bj-review-value")).toContainText("2");
    await expect(review.getByTestId("bj-review-storage")).toContainText("0.25");
    await expect(review.getByTestId("bj-review-fee")).toContainText("fuel included");
    // The seed is kept in this browser before anything is signed, and it is nowhere in the review or the transaction.
    const seed = (await kept(page))!.pending.at(-1)!.seed;
    expect(seed).toMatch(/^[0-9a-f]{64}$/);
    const hash = seedHash(me, seed);
    await expect(review.getByTestId("bj-review-seed-hash")).toHaveText(hash);
    await expect(review.getByTestId("bj-review-input")).toHaveText(hash);
    await expect(review).not.toContainText(seed);
    await expect(review).toContainText("The bet sends only their hash.");
    table.knownSeeds.push(seed);
    const before = node.acct(me).balance;
    await expect(review.getByTestId("bj-review-does")).toHaveText(/^Bet 2 FANG on a new hand \(step 1 of 2\)$/);
    await review.getByTestId("bj-sign").click();

    // Step 2 of 2 is presented by itself once the bet is in a block: the seed's reveal, for review.
    await expect(review.getByTestId("bj-review-does")).toHaveText(STEP_2);
    const betTx = node.submitted.at(-1) as Tx;
    expect(betTx.action).toMatchObject({ type: "call_program", storage_limit: (F / 4n).toString(), bindings: [], calls: [{ program: TABLE, version_digest: table.digest, method: "bet", input: hash, value: (2n * F).toString() }] });
    expect(JSON.stringify(betTx)).not.toContain(seed);
    expect(node.acct(me).balance).toBe(before - 2n * F - BigInt(betTx.fee));
    const hand = page.getByTestId("bj-open-hand");
    await expect(hand.getByTestId("bj-status")).toContainText("Bet placed. Now reveal your seed to be dealt (step 2 of 2).");
    await expect(hand.getByTestId("bj-status")).toContainText(/You have \d+ blocks \(about \d+ min\) left\. If the seed isn't revealed in time, the bet goes to the house\./);
    await expect(hand.getByTestId("bj-seed-warn")).toHaveText("If the seed isn't revealed in time, the bet goes to the house.");
    expect(table.openHand(me)).toMatchObject({ phase: Phase.Seed, clientSeed: null, seedHash: hash });
    await expect(review.getByTestId("bj-review-seed")).toHaveText(seed);
    await expect(review.getByTestId("bj-review-input")).toHaveText(call.revealSeed(0n, seed).input);
    // The reveal says where it will be sent. This build knows one node only, and says what that means.
    await expect(review.getByTestId("bj-review-nodes")).toHaveText("Your node only. This wallet knows no other node of this chain, so this one node decides whether your seed reaches the chain in time.");
    await expect(review.getByTestId("bj-review-value")).toContainText("0");
    await expect(review.getByTestId("bj-review-storage")).toContainText("this call stores nothing new");
    await expect(review.getByTestId("bj-review-digest")).toHaveText(table.digest);
    await sign(page, STEP_2);
    expect((node.submitted.at(-1) as Tx).action).toMatchObject({ type: "call_program", storage_limit: "0", calls: [{ method: "reveal_seed", input: call.revealSeed(0n, seed).input, value: "0", version_digest: table.digest }] });
    expect(node.submitted).toHaveLength(2);

    // The seed is in: the hand waits for the dealer, with its deadline in blocks.
    await expect(hand.getByTestId("bj-status")).toContainText("Seed revealed. Waiting for the dealer…");
    await expect(hand.getByTestId("bj-seed-step")).toHaveCount(0);
    await expect(hand.getByTestId("bj-log")).toContainText("You revealed your seed.");
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
    await expect(verify.getByTestId("bj-verdict")).toHaveText(VERIFIED);
    await expect(verify.getByTestId("bj-verify-seed-hash")).toHaveText(hash);
    await expect(verify.getByTestId("bj-verify-seed-check")).toHaveText("The bet carried this hash, and the seed above is the one that opens it.");
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
    await expect(checked.getByTestId("bj-verdict")).toHaveText(VERIFIED);
    await expect(checked.getByTestId("bj-verify-seed-check")).toHaveText("The bet carried this hash, and the seed above is the one that opens it.");
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
    const store = (await kept(page))!;
    expect(store.hands["0"]).toMatchObject({ seed, seedHash: hash, commit: chain[0], game: "0" });
    expect(store.hands["0"].txs).toHaveLength(4);
    expect(store.pending).toEqual([]);

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
    await bet(page, table, "2");
    const hand = page.getByTestId("bj-open-hand");
    await expect(hand.getByTestId("bj-status")).toContainText("Seed revealed. Waiting for the dealer…");
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
    await expect(last.getByTestId("bj-verdict")).toHaveText(VERIFIED);
    await expect(last.getByTestId("bj-verify-outcome")).toContainText("the hand ended by timeout, not by the cards");
    await clean(page, errors);
  });

  test("the page is reloaded between a bet's two transactions: the reveal is offered again from the stored seed, and works", async ({ page, node }) => {
    test.setTimeout(90_000);
    const { account: me } = await createWallet(page);
    const table = addTable(node);
    table.script = PLAYERS_TURN;
    await page.goto(`/blackjack?table=${TABLE}`);
    await page.getByTestId("bj-bet-form").getByRole("button", { name: "Unlock" }).click();
    await page.getByTestId("bj-gate").getByRole("checkbox").check();
    const seed = await placeBet(page, table, "3");
    expect(node.submitted).toHaveLength(1);

    // The page goes away with the bet in a block and the seed not revealed.
    await page.reload();
    const hand = page.getByTestId("bj-open-hand");
    await expect(hand.getByTestId("bj-status")).toContainText("Bet placed. Now reveal your seed to be dealt (step 2 of 2).");
    await expect(hand.getByTestId("bj-seed-missing")).toHaveCount(0);
    // The wallet is locked after a reload. The seed is still here; unlocking is all it takes.
    const step = hand.getByTestId("bj-seed-step");
    await expect(step.getByTestId("bj-seed-warn")).toBeVisible();
    await expect(page.getByTestId("bj-review")).toHaveCount(0);
    await step.getByRole("button", { name: "Unlock" }).click();
    // Step 2 is presented again by itself, with the same seed, and it opens the bet.
    await expect(page.getByTestId("bj-review-does")).toHaveText(STEP_2);
    await expect(page.getByTestId("bj-review-seed")).toHaveText(seed);
    // Dismissed, the hand's own button brings it back.
    await page.getByTestId("bj-review").getByRole("button", { name: "← Back" }).click();
    await expect(page.getByTestId("bj-review")).toHaveCount(0);
    await hand.getByTestId("bj-reveal").click();
    await sign(page, STEP_2);
    expect(node.submitted).toHaveLength(2);
    expect((node.submitted.at(-1) as Tx).action).toMatchObject({ calls: [{ method: "reveal_seed", input: call.revealSeed(0n, seed).input }] });
    await expect(hand.getByTestId("bj-status")).toContainText("Seed revealed. Waiting for the dealer…");
    const game = table.openHand(me);
    expect(game).toMatchObject({ phase: Phase.Deal, clientSeed: seed });
    // The hand plays on, and checks down to the commitment and the hash the bet carried.
    expect(table.deal(game.id)).toBeNull();
    await expect(hand.getByTestId("bj-status")).toContainText("Your move: hit or stand.");
    await hand.getByTestId("bj-check-open").click();
    await expect(hand.getByTestId("bj-verdict")).toHaveText(VERIFIED);
    expect((await kept(page))!.hands["0"]).toMatchObject({ seed, seedHash: seedHash(me, seed), commit: table.chains.get(0n)![0] });
    await clean(page, errors);
  });

  test("a seed's reveal that doesn't land: the page warns while the hand still waits for its seed, the same transaction is sent again, and the warning goes once it's in a block", async ({ page, node }) => {
    test.setTimeout(120_000);
    const { account: me } = await createWallet(page);
    const table = addTable(node);
    table.script = PLAYERS_TURN;
    await page.goto(`/blackjack?table=${TABLE}`);
    await page.getByTestId("bj-bet-form").getByRole("button", { name: "Unlock" }).click();
    await page.getByTestId("bj-gate").getByRole("checkbox").check();
    const seed = await placeBet(page, table, "2");
    const hand = page.getByTestId("bj-open-hand");
    // Nothing was signed to reveal the seed yet: no warning about a reveal.
    await expect(hand.getByTestId("bj-seed-unlanded")).toHaveCount(0);

    // The node takes the reveal and keeps it out of its blocks.
    const isReveal = (tx: Tx) => tx.action.type === "call_program" && tx.action.calls[0].method === "reveal_seed";
    node.holdTx = isReveal;
    await page.getByTestId("bj-sign").click();
    const sent = page.getByTestId("bj-sent");
    await expect(sent).toHaveAttribute("data-state", "waiting");
    // While the page still waits for it, it doesn't warn yet.
    await expect(hand.getByTestId("bj-seed-unlanded")).toHaveCount(0);
    await expect(sent).toHaveAttribute("data-state", "unsure", { timeout: 45_000 });
    await expect(sent.getByTestId("bj-sent-words")).toContainText("Submitted, and not in a block yet.");

    // The hand still waits for its seed, and the page says the reveal hasn't landed and what that costs.
    expect(table.openHand(me)).toMatchObject({ phase: Phase.Seed, clientSeed: null });
    const warning = hand.getByTestId("bj-seed-unlanded");
    await expect(warning).toContainText("Your seed's reveal was signed and sent, but it isn't in a block and the hand still waits for its seed. If it isn't in a block before the deadline, the bet goes to the house.");
    await expect(warning).toContainText("It went through one node only, the one this wallet uses.");
    // The same signed transaction went out a second time when the wait ended: one id, so it can run only once.
    await expect.poll(() => node.held.length).toBe(2);
    expect(node.held[1]).toEqual(node.held[0]);
    expect(isReveal(node.held[0])).toBe(true);
    const stored = (await kept(page))!.hands["0"] as { revealTx?: string; txs: string[] };
    expect(stored.revealTx).toMatch(/^[0-9a-f]{64}$/);
    expect(stored.txs.at(-1)).toBe(stored.revealTx);
    await expect(warning.getByTestId("bj-seed-unlanded-tx")).toHaveAttribute("href", new RegExp(`${stored.revealTx}`));
    // The reveal can still be made again from here.
    await expect(hand.getByTestId("bj-reveal")).toBeEnabled();
    // The warning is the hand's, not the moment's: it is there after a reload too.
    await page.reload();
    await expect(page.getByTestId("bj-open-hand").getByTestId("bj-seed-unlanded")).toBeVisible();

    // It reaches a block at last, once: the hand has its seed and the warning is gone.
    const before = node.submitted.length;
    expect(node.release()).toEqual([stored.revealTx]);
    expect(node.submitted).toHaveLength(before + 1);
    expect(table.openHand(me)).toMatchObject({ phase: Phase.Deal, clientSeed: seed });
    await expect(page.getByTestId("bj-open-hand").getByTestId("bj-status")).toContainText("Seed revealed. Waiting for the dealer…");
    await expect(page.getByTestId("bj-open-hand").getByTestId("bj-seed-unlanded")).toHaveCount(0);
    await clean(page, errors);
  });

  test("the seed isn't in this browser: the page says so, offers no reveal, and the bet goes to the house at the deadline", async ({ page, node }) => {
    test.setTimeout(90_000);
    const { account: me } = await createWallet(page);
    const table = addTable(node);
    await page.goto(`/blackjack?table=${TABLE}`);
    await page.getByTestId("bj-bet-form").getByRole("button", { name: "Unlock" }).click();
    await page.getByTestId("bj-gate").getByRole("checkbox").check();
    await placeBet(page, table, "2");
    // The browser's storage for the table is cleared (or the bet was placed in another browser).
    await page.evaluate((k) => localStorage.removeItem(k), STORE);
    await page.reload();
    const hand = page.getByTestId("bj-open-hand");
    await expect(hand.getByTestId("bj-seed-missing")).toHaveText(
      "This browser doesn't hold this bet's seed. A seed can only be revealed from the browser that placed the bet, while its storage is intact. If you placed it in another browser, open the table there now. When the time runs out, the bet goes to the house.",
    );
    await expect(hand.getByTestId("bj-status")).toContainText("Bet placed. Now reveal your seed to be dealt (step 2 of 2).");
    // Nothing that would fail is offered: no reveal button, no review, nothing signed.
    await expect(hand.getByTestId("bj-reveal")).toHaveCount(0);
    await expect(hand.getByTestId("bj-seed-step")).toHaveCount(0);
    await page.waitForTimeout(5000);
    await expect(page.getByTestId("bj-review")).toHaveCount(0);
    expect(node.submitted).toHaveLength(1);
    await hand.getByTestId("bj-check-open").click();
    await expect(hand.getByTestId("bj-verdict")).toContainText("Nothing to check yet. No card is dealt until the seed is revealed.");
    await expect(hand.getByTestId("bj-verify-seed")).toHaveText("Not revealed.");

    // The seed's 300 blocks pass: the bet is the house's, and anyone can end the hand.
    node.height += 400;
    await expect(hand.getByTestId("bj-status")).toHaveText("Your seed wasn't revealed in time, so the bet goes to the house. Anyone can end the hand.");
    await expect(hand.getByTestId("bj-seed-missing")).toHaveCount(0);
    const late = hand.getByTestId("bj-seed-late");
    await expect(late).toContainText("The time to reveal this bet's seed has passed. No card was dealt, and the bet goes to the house.");
    await late.getByRole("button", { name: "Unlock" }).click();
    const before = node.acct(me).balance;
    await late.getByTestId("bj-end").click();
    await sign(page, /^End the hand: the seed wasn't revealed in time, so this ends the hand and the bet goes to the house \(hand 0\)$/);
    const end = node.submitted.at(-1) as Tx;
    expect(end.action).toMatchObject({ calls: [{ method: "timeout", input: call.timeout(0n).input, value: "0" }] });
    expect(node.acct(me).balance).toBe(before - BigInt(end.fee));
    expect(table.seat(me).last).toMatchObject({ outcome: Outcome.NoSeed, payout: 0n, dealt: 0, clientSeed: null });
    expect(table.house.bankroll).toBe(2002n * F);
    const last = page.getByTestId("bj-last-hand");
    await expect(last.getByTestId("bj-last-outcome")).toHaveText("The bet is the house's: its seed was never revealed. Nothing came back from the 2 FANG bet.");
    await expect(page.getByTestId("bj-no-hand")).toBeVisible();
    await last.getByTestId("bj-check-last").click();
    await expect(last.getByTestId("bj-verdict")).toHaveText("Verified. No cards were dealt: the seed was never revealed, so the bet went to the house.");
    await expect(last.getByTestId("bj-verify-seed-check")).toHaveText("What the bet committed to. The seed that opens it was not revealed.");
    await clean(page, errors);
  });

  test("a seed that isn't revealed in time: a late reveal is refused in the table's words, the hand ends as the house's, and that verifies", async ({ page, node }) => {
    test.setTimeout(90_000);
    const { account: me } = await createWallet(page);
    const table = addTable(node);
    await page.goto(`/blackjack?table=${TABLE}`);
    await page.getByTestId("bj-bet-form").getByRole("button", { name: "Unlock" }).click();
    await page.getByTestId("bj-gate").getByRole("checkbox").check();
    const seed = await placeBet(page, table, "2");
    const hand = page.getByTestId("bj-open-hand");
    const deadline = table.openHand(me).deadline;
    await expect(hand.getByTestId("bj-deadline")).toContainText(`Deadline: block ${Number(deadline).toLocaleString("en-US")}.`);

    // The review of step 2 is left open while the seed's deadline passes; signing it then is refused, and charged.
    node.height = Number(deadline) + 5;
    await expect(hand.getByTestId("bj-status")).toHaveText("Your seed wasn't revealed in time, so the bet goes to the house. Anyone can end the hand.");
    await expect(hand.getByTestId("bj-reveal")).toHaveCount(0);
    await page.getByTestId("bj-sign").click();
    const sent = page.getByTestId("bj-sent");
    await expect(sent).toHaveAttribute("data-state", "failed");
    await expect(sent.getByTestId("bj-sent-words")).toHaveText("The time to reveal that bet's seed has passed.");
    expect(table.openHand(me).phase).toBe(Phase.Seed);
    // It isn't presented again: a reveal that would fail is never offered.
    await page.waitForTimeout(5000);
    await expect(page.getByTestId("bj-review")).toHaveCount(0);

    // The hand ends: the stake is the house's, no card was dealt, and the check says exactly that.
    const before = node.acct(me).balance;
    await hand.getByTestId("bj-seed-late").getByTestId("bj-end").click();
    await sign(page, /^End the hand: the seed wasn't revealed in time/);
    expect(node.acct(me).balance).toBe(before - BigInt((node.submitted.at(-1) as Tx).fee));
    expect(table.house).toMatchObject({ bankroll: 2002n * F, reserved: 0n, stakes: 0n, open: 0 });
    const last = page.getByTestId("bj-last-hand");
    await expect(last.getByTestId("bj-last-outcome")).toHaveText("The bet is the house's: its seed was never revealed. Nothing came back from the 2 FANG bet.");
    await expect(last.getByTestId("bj-svg")).toHaveAttribute("aria-label", /The bet is the house's: its seed was never revealed\. Nothing came back\./);
    await last.getByTestId("bj-check-last").click();
    const checked = last.getByTestId("bj-verify");
    await expect(checked).toHaveAttribute("data-ok", "yes");
    await expect(checked.getByTestId("bj-verdict")).toHaveText("Verified. No cards were dealt: the seed was never revealed, so the bet went to the house.");
    await expect(checked.getByTestId("bj-verify-seed")).toHaveText("Not revealed.");
    await expect(checked.getByTestId("bj-verify-seed-hash")).toHaveText(seedHash(me, seed));
    await expect(checked.getByTestId("bj-verify-commit")).toHaveText(table.chains.get(0n)![0]);
    await expect(checked.getByTestId("bj-verify-commit-check")).toHaveText("It is the commitment your bet took.");
    // The seat is free: a new bet can be placed, with a new seed.
    await expect(page.getByTestId("bj-bet-form").getByTestId("bj-bet-range")).toContainText("498 FANG left today");
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
