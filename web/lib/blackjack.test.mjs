// lib/blackjack.ts, lib/blackjack-copy.ts and lib/blackjack-share.ts: the blackjack table page's
// own logic. The bet form is held to the program's checks (node/vm/guests/blackjack `open_hand`),
// the fairness check to the rules' vectors (services/blackjack/core/test/vectors.json, written by
// the Rust side), and every sentence the pages say to the copy rules. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { test } from "node:test";
import { ABORTS, DEFAULT_LIMITS, DROPS_PER_FANG, EMPTY_PLAYER, Outcome, Phase, POSTER_ALT, altText, bytesAddress, exposure, outcomeWords, payout, seedHash, snapshotOfGame, snapshotOfSettled, tableSvg } from "@blockula/blackjack";
import {
  ADULT_KEY,
  addHandTx,
  betProblem,
  bindBet,
  blocksWords,
  checkOpenHand,
  checkSettledHand,
  chooseTable,
  countdown,
  dailyLeft,
  gameIdFrom,
  grantProblem,
  handState,
  isTableAddress,
  logLine,
  maxBetNow,
  newSeed,
  noteBetTx,
  noteRevealTx,
  parseAmount,
  payoutWords,
  readStore,
  rememberSeed,
  seatOf,
  seedUsed,
  storeKey,
  storedHand,
  storedSeedFor,
  tableProblem,
} from "./blackjack.ts";
import { COPY, NOTE, rulesText } from "./blackjack-copy.ts";
import { PLAYER_HEIGHT, PLAYER_WIDTH, shareOrigin, shareTags, shareTarget, targetQuery } from "./blackjack-share.ts";

const F = DROPS_PER_FANG;
const vectors = JSON.parse(readFileSync(new URL("../../services/blackjack/core/test/vectors.json", import.meta.url), "utf8"));
const addr = (byte, prefix = "boo") => bytesAddress(new Uint8Array(32).fill(byte), prefix);
const TABLE = addr(0xb7);
const ME = addr(0x11);
const OTHER = addr(0x22);

const house = (o = {}) => ({ owner: addr(1), dealer: addr(2), bankroll: 2000n * F, reserved: 0n, stakes: 0n, table: 0n, nextCommit: 8n, nextGame: 3n, paused: false, open: 0, ...o });
const ctx = (o = {}) => ({ limits: DEFAULT_LIMITS, house: house(), player: EMPTY_PLAYER, height: 100_000, commitmentReady: true, transferable: 100n * F, ...o });

/** A storage like localStorage. */
function memory() {
  const m = new Map();
  return { getItem: (k) => (m.has(k) ? m.get(k) : null), setItem: (k, v) => void m.set(k, String(v)), raw: m };
}

/** An open hand's record as the program would hold it after `n` of a vector's links. */
function gameAt(h, n, o = {}) {
  const cards = h.cards.slice(0, n);
  // The deal is player, dealer, player; then the player's cards until they hold theirs; then the dealer's.
  const player = [];
  const dealer = [];
  cards.forEach((c, i) => (i === 1 || player.length >= h.player.length ? dealer : player).push(c));
  const hits = (h.moves.match(/H/g) ?? []).length;
  const phase = n < 3 ? Phase.Deal : n === h.links.length && h.outcome ? Phase.Done : player.length < 2 + hits ? (n === 3 + (player.length - 2) ? Phase.Player : Phase.Hit) : Phase.Dealer;
  return { id: BigInt(h.game_id), player: ME, stake: BigInt(h.stake), clientSeed: h.client_seed, seedHash: null, link: n === 0 ? h.commit : h.links[n - 1], deadline: 100_150n, postId: 0n, phase, table: false, dealt: n, playerCards: player, dealerCards: dealer, playerTotal: 0, dealerTotal: 0, ...o };
}
const lastOf = (h) => ({ game: BigInt(h.game_id), outcome: h.outcome, dealt: h.links.length, nPlayer: h.player.length, stake: BigInt(h.stake), payout: BigInt(h.payout), clientSeed: h.client_seed, seedHash: null, link: h.links.at(-1) });
/** A hand whose bet is in and whose seed isn't revealed: the chain holds the hash `ME`'s bet carried. */
const waitingFor = (h, o = {}) => gameAt(h, 0, { phase: Phase.Seed, clientSeed: null, seedHash: seedHash(ME, h.client_seed), deadline: 100_300n, ...o });

// ---------------------------------------------------------------------------------- which table

test("the table is the link's address, else the build's; a name or another chain's address is refused", () => {
  assert.deepEqual(chooseTable(null, undefined), { state: "none" });
  assert.deepEqual(chooseTable("", "  "), { state: "none" });
  assert.deepEqual(chooseTable(TABLE, undefined, "boo"), { state: "ok", address: TABLE, from: "link" });
  assert.deepEqual(chooseTable(null, TABLE, "boo"), { state: "ok", address: TABLE, from: "build" });
  assert.deepEqual(chooseTable(OTHER, TABLE), { state: "ok", address: OTHER, from: "link" }, "the link wins");
  const name = chooseTable("blackjack.boo", TABLE);
  assert.equal(name.state, "bad");
  assert.match(name.why, /never by a \.boo name/);
  const other = chooseTable(addr(0xb7, "tboo"), undefined, "boo");
  assert.equal(other.state, "bad");
  assert.match(other.why, /another chain/);
  // A checksum that doesn't hold isn't an address.
  assert.equal(isTableAddress(`${TABLE.slice(0, -1)}${TABLE.endsWith("q") ? "p" : "q"}`), false);
  assert.equal(isTableAddress("<script>"), false);
  assert.equal(chooseTable("x".repeat(500), undefined).given.length, 120);
});

test("a hand's id from a link is a u64 in decimal", () => {
  assert.equal(gameIdFrom("0"), 0n);
  assert.equal(gameIdFrom("18446744073709551615"), (1n << 64n) - 1n);
  for (const bad of [null, "", "-1", "1.5", "0x10", "18446744073709551616", "1e3", " 4"]) assert.equal(gameIdFrom(bad), null, String(bad));
});

// ---------------------------------------------------------------------------------- amounts and the bet form

test("amounts are FANG as text, to 9 decimals, never floats", () => {
  assert.equal(parseAmount("1"), F);
  assert.equal(parseAmount("1.5"), 1_500_000_000n);
  assert.equal(parseAmount(" 0.000000001 "), 1n);
  assert.equal(parseAmount("0.1") + parseAmount("0.2"), parseAmount("0.3"), "exact where a float isn't");
  for (const bad of ["", ".5", "1.", "1e3", "-1", "1,5", "1.0000000001", "abc", "1 2", "Infinity"]) assert.equal(parseAmount(bad), null, bad);
});

test("the largest bet is the max bet, the bankroll's share, and what the house may still have at stake", () => {
  assert.equal(maxBetNow(DEFAULT_LIMITS, house()), 50n * F, "the max bet");
  assert.equal(maxBetNow(DEFAULT_LIMITS, house({ bankroll: 300n * F })), 15n * F, "a twentieth of the bankroll");
  // 750 FANG of exposure at most: with 720 reserved, 30 are left, which covers a stake of 20.
  assert.equal(maxBetNow(DEFAULT_LIMITS, house({ reserved: 720n * F })), 20n * F);
  assert.equal(exposure(maxBetNow(DEFAULT_LIMITS, house({ reserved: 720n * F }))), 30n * F);
  assert.equal(maxBetNow(DEFAULT_LIMITS, house({ reserved: 750n * F })), 0n);
  // With a divisor of 1 the bankroll must still cover the exposure (1.5 ×).
  const l = { ...DEFAULT_LIMITS, bankrollDivisor: 1n, maxBet: 1000n * F };
  const most = maxBetNow(l, house({ bankroll: 10n * F + 1n }));
  assert.ok(exposure(most) <= 10n * F + 1n && exposure(most + 1n) > 10n * F + 1n);
});

test("the daily limit counts the day the record names, and starts over on the next", () => {
  const day = 100_000n / DEFAULT_LIMITS.dayBlocks;
  assert.equal(dailyLeft(DEFAULT_LIMITS, EMPTY_PLAYER, 100_000), 500n * F);
  assert.equal(dailyLeft(DEFAULT_LIMITS, { ...EMPTY_PLAYER, day, wagered: 480n * F }, 100_000), 20n * F);
  assert.equal(dailyLeft(DEFAULT_LIMITS, { ...EMPTY_PLAYER, day: day - 1n, wagered: 500n * F }, 100_000), 500n * F, "yesterday's wagers don't count");
  assert.equal(dailyLeft(DEFAULT_LIMITS, { ...EMPTY_PLAYER, day, wagered: 600n * F }, 100_000), 0n);
});

test("the bet form says what the program would refuse, in the program's order", () => {
  assert.equal(betProblem("2", ctx()), null);
  assert.equal(betProblem("1", ctx()), null, "the minimum");
  assert.equal(betProblem("50", ctx()), null, "the maximum");
  assert.match(betProblem("", ctx()), /Enter a bet/);
  assert.match(betProblem("two", ctx()), /amount of FANG/);
  assert.match(betProblem("0.999999999", ctx()), /smallest bet is 1 FANG/);
  assert.match(betProblem("50.000000001", ctx()), /largest bet right now is 50 FANG/);
  assert.match(betProblem("16", ctx({ house: house({ bankroll: 300n * F }) })), /largest bet right now is 15 FANG/);
  const day = 100_000n / DEFAULT_LIMITS.dayBlocks;
  assert.match(betProblem("21", ctx({ player: { ...EMPTY_PLAYER, day, wagered: 480n * F } })), /You have 20 FANG left today/);
  assert.match(betProblem("30", ctx({ transferable: 25n * F })), /can move 25 FANG/);
  assert.equal(betProblem("30", ctx({ transferable: null })), null, "an unknown balance isn't a refusal");
  // Whatever the size: paused, no bankroll, a full table, an open hand, the day used up, no commitment.
  assert.match(tableProblem(ctx({ house: house({ paused: true }) })), /paused/);
  assert.match(tableProblem(ctx({ house: house({ bankroll: 19n * F }) })), /bankroll can't cover/);
  assert.match(tableProblem(ctx({ house: house({ open: 10 }) })), /table is full/);
  assert.match(tableProblem(ctx({ player: { ...EMPTY_PLAYER, openGame: 2n } })), /Finish your open hand/);
  assert.match(tableProblem(ctx({ player: { ...EMPTY_PLAYER, day, wagered: 499_500_000_000n } })), /today's limit/);
  assert.match(tableProblem(ctx({ commitmentReady: false })), /dealer isn't ready/);
  assert.match(betProblem("2", ctx({ house: house({ paused: true }) })), /paused/, "a blocked table blocks every bet");
  assert.equal(tableProblem(ctx()), null);
});

// ---------------------------------------------------------------------------------- deadlines and whose move

test("a deadline counts down in blocks, and has passed once the chain is at it", () => {
  assert.deepEqual(countdown(100_150n, 100_000), { left: 150, passed: false, words: "150 blocks (about 3 min)" });
  assert.equal(countdown(100_150n, 100_149).words, "1 block (about 1s)");
  // `timeout` works in a block after the deadline, and a transaction sent at the deadline's height lands in one.
  assert.deepEqual(countdown(100_150n, 100_150), { left: 0, passed: true, words: "no blocks" });
  assert.equal(countdown(100_150n, 200_000).passed, true);
  assert.equal(countdown(101_800n, 100_000, 2000).words, "1,800 blocks (about 60 min)");
  assert.equal(blocksWords(1800n), "1,800 blocks (about 30 min)");
  assert.equal(blocksWords(150n, 400), "150 blocks (about 60s)");
});

test("an open hand waits for the dealer or the player, and says what a passed deadline means", () => {
  const h = vectors.hands.find((x) => x.moves === "HS");
  const deal = handState(gameAt(h, 0), ME, 100_000);
  assert.deepEqual([deal.mine, deal.waitsFor, deal.canMove, deal.canClaim], [true, "dealer", false, false]);
  assert.match(deal.words, /Waiting for the dealer… It has 150 blocks \(about 3 min\) left to reveal the deal/);
  const late = handState(gameAt(h, 0), ME, 100_150);
  assert.deepEqual([late.canClaim, late.canMove], [true, false]);
  assert.match(late.words, /dealer ran out of time/);
  const turn = handState(gameAt(h, 3), ME, 100_000);
  assert.deepEqual([turn.waitsFor, turn.canMove, turn.canClaim, turn.idle], ["player", true, false, false]);
  assert.match(turn.words, /^Your move: hit or stand\. You have 150 blocks/);
  const idle = handState(gameAt(h, 3), ME, 100_200);
  assert.deepEqual([idle.idle, idle.canMove, idle.canClaim], [true, true, false]);
  assert.match(idle.words, /stands by itself: anyone can trigger that/);
  // A spectator, and another account's hand: nothing to sign.
  const watch = handState(gameAt(h, 3), null, 100_000);
  assert.deepEqual([watch.mine, watch.canMove], [false, false]);
  assert.match(watch.words, /^The player's move/);
  assert.equal(handState(gameAt(h, 3), OTHER, 100_000).canMove, false);
  assert.match(handState(gameAt(h, 4, { phase: Phase.Hit }), ME, 100_000).words, /to reveal the next card/);
});

test("a bet waits for its seed: step 2 of 2 while there is time, the house's once there isn't", () => {
  const h = vectors.hands.find((x) => x.moves === "HS");
  const g = waitingFor(h);
  const mine = handState(g, ME, 100_000);
  assert.deepEqual([mine.mine, mine.waitsFor, mine.seedLate, mine.canMove, mine.canClaim, mine.idle], [true, "seed", false, false, false, false]);
  assert.equal(mine.words, "Bet placed. Now reveal your seed to be dealt (step 2 of 2). You have 300 blocks (about 5 min) left. If the seed isn't revealed in time, the bet goes to the house.");
  // A reveal sent at the deadline's own block would land after it.
  assert.equal(handState(g, ME, 100_299).seedLate, false);
  const late = handState(g, ME, 100_300);
  assert.deepEqual([late.seedLate, late.canClaim, late.canMove], [true, false, false]);
  assert.equal(late.words, "Your seed wasn't revealed in time, so the bet goes to the house. Anyone can end the hand.");
  // The dealer's missed deadline pays the player; a seed's missed deadline never offers a claim.
  assert.equal(handState(g, ME, 200_000).canClaim, false);
  const watch = handState(g, null, 100_000);
  assert.deepEqual([watch.mine, watch.waitsFor], [false, "seed"]);
  assert.match(watch.words, /^The bet is placed\. The player has 300 blocks/);
  assert.match(handState(g, OTHER, 100_300).words, /^The seed wasn't revealed in time/);
  // Once the seed is in, the dealer owes the deal. A relayed hand never had this step.
  assert.match(handState(gameAt(h, 0), ME, 100_000).words, /^Seed revealed\. Waiting for the dealer…/);
  assert.match(handState(gameAt(h, 0, { table: true }), ME, 100_000).words, /^Waiting for the dealer…/);
});

test("the seed of a bet is found again by the hash the bet carried, and only by the browser that holds it", () => {
  const store = memory();
  const key = storeKey("blockula-testnet", TABLE);
  const seed = "a1".repeat(32);
  const hash = seedHash(ME, seed);
  const tx = "c3".repeat(32);
  rememberSeed(store, key, seed, 1000, hash);
  rememberSeed(store, key, "b2".repeat(32), 1001, seedHash(ME, "b2".repeat(32)));
  // Still pending (the page closed before the bet's receipt was read): found by the hash, for the account that bet.
  assert.deepEqual(storedSeedFor(store, key, ME, hash), { seed, seedHash: hash, txs: [], at: 1000 });
  assert.equal(storedSeedFor(store, key, OTHER, hash), null, "the hash binds the account");
  assert.equal(storedSeedFor(store, key, ME, seedHash(OTHER, seed)), null);
  assert.equal(storedSeedFor(store, key, ME, "zz"), null);
  assert.equal(storedSeedFor(memory(), key, ME, hash), null, "another browser, or cleared storage, has no seed");
  assert.equal(storedSeedFor(null, key, ME, hash), null);
  // A stored hash that isn't the seed's doesn't make a seed open another bet: the hash is recomputed.
  const forged = memory();
  forged.setItem(key, JSON.stringify({ v: 1, pending: [{ seed, seedHash: seedHash(ME, "ff".repeat(32)), txs: [], at: 1 }], hands: {} }));
  assert.equal(storedSeedFor(forged, key, ME, seedHash(ME, "ff".repeat(32))), null);
  // The open hand's record has only the hash: the hand is found, with its id.
  const h = vectors.hands[0];
  const waiting = waitingFor(h, { seedHash: hash });
  assert.deepEqual(storedHand(store, key, waiting.id, null, { account: ME, seedHash: hash }), { seed, seedHash: hash, txs: [], at: 1000, game: waiting.id.toString() });
  assert.equal(storedHand(store, key, waiting.id, null, { account: OTHER, seedHash: hash }), null);
  assert.equal(storedHand(store, key, waiting.id, null), null);
  // Filed under its hand (the bet's receipt, or the chain showing the hand), the reveal's transaction is kept with it.
  noteBetTx(store, key, seed, tx);
  bindBet(store, key, seed, { game: 7n, commit: "d4".repeat(32), txId: tx, seedHash: hash });
  addHandTx(store, key, 7n, "e5".repeat(32));
  assert.deepEqual(storedSeedFor(store, key, ME, hash), { seed, seedHash: hash, game: "7", commit: "d4".repeat(32), betTx: tx, txs: [tx, "e5".repeat(32)], at: 1000 });
  // Filing again (the page found the hand on a reload) keeps what was known.
  bindBet(store, key, seed, { game: 7n, seedHash: hash });
  assert.equal(storedHand(store, key, 7n, seed).commit, "d4".repeat(32));
  // The seed's reveal is noted as the hand's newest reveal (the page warns while it hasn't landed), a second one replaces it,
  // and both stay in the hand's list. Nothing is noted for a hand that isn't kept, or an id that isn't one.
  noteRevealTx(store, key, 7n, "f6".repeat(32));
  noteRevealTx(store, key, 7n, "a7".repeat(32));
  noteRevealTx(store, key, 7n, "not an id");
  noteRevealTx(store, key, 8n, "b8".repeat(32));
  assert.equal(storedHand(store, key, 7n, seed).revealTx, "a7".repeat(32));
  assert.deepEqual(storedHand(store, key, 7n, seed).txs, [tx, "e5".repeat(32), "f6".repeat(32), "a7".repeat(32)]);
  assert.equal(readStore(store, key).hands["8"], undefined);
  assert.match(COPY.seed.unlanded, /isn't in a block .* the bet goes to the house/);
  assert.match(COPY.seed.sentMany(2), /Your node and 2 other nodes .* its fee is paid once/);
  assert.match(COPY.seed.sentMany(1), /1 other node of/);
  assert.match(COPY.seed.unlandedMany(1), /1 other node\./);
  assert.equal(readStore(store, key).pending.length, 1, "the other seed still waits for its bet");
});

test("the check knows a hand without its seed, and holds a revealed seed to the hash its bet carried", () => {
  const h = vectors.hands.find((x) => x.moves === "HS");
  const hash = seedHash(ME, h.client_seed);
  const kept = { seed: h.client_seed, seedHash: hash, commit: h.commit, txs: [], at: 0 };
  // Waiting for the seed: nothing to check yet, and the seed isn't shown as the chain's.
  const waiting = checkOpenHand(waitingFor(h), kept);
  assert.deepEqual([waiting.ok, waiting.unrevealed, waiting.clientSeed, waiting.seedHash, waiting.seed, waiting.steps.length, waiting.commit], [true, true, null, hash, "yours", 0, h.commit]);
  assert.match(waiting.verdict, /^Nothing to check yet\. No card is dealt until the seed is revealed/);
  assert.equal(checkOpenHand(waitingFor(h)).seed, "unknown");
  assert.equal(checkOpenHand(waitingFor(h), { ...kept, seed: "5a".repeat(32) }).seed, "unknown", "a seed that doesn't open the hash isn't this hand's");
  // Revealed and dealt: the seed is the one the bet committed to.
  const open = checkOpenHand(gameAt(h, 3), kept);
  assert.deepEqual([open.ok, open.seedCheck, open.commitCheck, open.unrevealed, open.seedHash], [true, "matches", "matches", false, hash]);
  assert.equal(open.verdict, "Verified. Every card comes from the dealer's commitment and your seed. The seed is the one the bet committed to.");
  const settled = checkSettledHand(lastOf(h), kept, ME);
  assert.deepEqual([settled.ok, settled.seedCheck], [true, "matches"]);
  // Without the account, or without the stored hash, the seed isn't compared (and nothing claims it was).
  assert.equal(checkSettledHand(lastOf(h), kept).seedCheck, "unknown");
  assert.equal(checkSettledHand(lastOf(h), { ...kept, seedHash: undefined }, ME).seedCheck, "unknown");
  // A seed that isn't the one the bet carried a hash of doesn't verify.
  const wrong = checkSettledHand(lastOf(h), { ...kept, seedHash: seedHash(OTHER, h.client_seed) }, ME);
  assert.deepEqual([wrong.ok, wrong.verdict], [false, "Not verified: the seed isn't the one the bet committed to."]);
  // A hand that ended without its seed: no cards, nothing paid, and that is what's verified.
  const stake = 2n * F;
  const lost = { game: BigInt(h.game_id), outcome: Outcome.NoSeed, dealt: 0, nPlayer: 0, stake, payout: 0n, clientSeed: null, seedHash: hash, link: h.commit };
  const none = checkSettledHand(lost, kept, ME);
  assert.deepEqual([none.ok, none.unrevealed, none.outcome, none.steps.length, none.player, none.dealer, none.seed, none.commitCheck], [true, true, Outcome.NoSeed, 0, [], [], "yours", "matches"]);
  assert.equal(none.verdict, "Verified. No cards were dealt: the seed was never revealed, so the bet went to the house.");
  assert.equal(checkSettledHand({ ...lost, payout: 1n }).ok, false);
  assert.equal(checkSettledHand({ ...lost, outcome: Outcome.ForfeitDeal, payout: payout(stake, Outcome.ForfeitDeal) }).ok, false);
  assert.equal(checkSettledHand({ ...lost, link: "00".repeat(32) }, kept, ME).ok, false, "not the commitment the bet took");
  assert.equal(`${outcomeWords(Outcome.NoSeed)}. ${payoutWords(stake, 0n)}`, "The bet is the house's: its seed was never revealed. Nothing came back from the 2 FANG bet.");
  assert.equal(logLine({ name: "seed", game: 7n, player: ME, clientSeed: h.client_seed, deadline: 0n }, ME), "You revealed your seed.");
  assert.equal(logLine({ name: "seed", game: 7n, player: OTHER, clientSeed: h.client_seed, deadline: 0n }, ME), "The player revealed their seed.");
});

test("a seat is the open hand, a hand that settled between two reads, or none, each with the last hand", () => {
  const h = vectors.hands[0];
  const last = lastOf(h);
  assert.deepEqual(seatOf(EMPTY_PLAYER, null), { state: "idle", last: null });
  assert.deepEqual(seatOf({ ...EMPTY_PLAYER, last }, null), { state: "idle", last });
  const game = gameAt(h, 3);
  assert.deepEqual(seatOf({ ...EMPTY_PLAYER, openGame: game.id, last }, game), { state: "open", game, last });
  // The record still names the hand, but its record is gone: it settled between the two reads.
  assert.deepEqual(seatOf({ ...EMPTY_PLAYER, openGame: game.id }, null), { state: "settling", last: null });
  assert.equal(seatOf({ ...EMPTY_PLAYER, openGame: 99n }, game).state, "settling", "another hand's record isn't it");
});

// ---------------------------------------------------------------------------------- seeds and commitments

test("a seed is 32 fresh random bytes, kept before the bet is signed, and never used twice", () => {
  const key = storeKey("blockula-x", TABLE);
  assert.equal(key, `blockula.blackjack.blockula-x.${TABLE}`);
  const store = memory();
  const a = newSeed();
  const b = newSeed();
  assert.match(a, /^[0-9a-f]{64}$/);
  assert.notEqual(a, b);
  // The page's source of randomness is the one given: `crypto.getRandomValues`.
  assert.equal(newSeed((bytes) => bytes.fill(0xab)), "ab".repeat(32));
  assert.throws(() => newSeed(() => new Uint8Array(16)), /32 bytes/);
  rememberSeed(store, key, a, 1000);
  assert.equal(seedUsed(store, key, a), true);
  assert.equal(seedUsed(store, key, b), false);
  assert.throws(() => rememberSeed(store, key, a, 2000), /used before/);
  assert.throws(() => rememberSeed(store, key, "zz", 2000), /32 bytes of hex/);
  assert.deepEqual(readStore(store, key), { v: 1, pending: [{ seed: a, txs: [], at: 1000 }], hands: {} });
  // Another table's hands are another key.
  assert.equal(seedUsed(store, storeKey("blockula-x", OTHER), a), false);
});

test("a bet's seed is filed under its hand with the commitment its receipt gave, and found only by that hand's seed", () => {
  const key = storeKey("blockula-x", TABLE);
  const store = memory();
  const [seed, commit, tx, hit] = ["11".repeat(32), "22".repeat(32), "33".repeat(32), "44".repeat(32)];
  rememberSeed(store, key, seed, 1000);
  noteBetTx(store, key, seed, tx);
  // Before the receipt is read, the hand is found by the seed the chain recorded for it: no commitment yet.
  assert.deepEqual(storedHand(store, key, 7n, seed), { seed, betTx: tx, txs: [tx], at: 1000, game: "7" });
  bindBet(store, key, seed, { game: 7n, commit, txId: tx });
  addHandTx(store, key, 7n, hit);
  addHandTx(store, key, 7n, hit);
  assert.deepEqual(storedHand(store, key, 7n, seed), { seed, game: "7", commit, betTx: tx, txs: [tx, hit], at: 1000 });
  assert.deepEqual(readStore(store, key).pending, []);
  // The same id with another seed (another chain's hand 7, or a bet made elsewhere) isn't this browser's hand.
  assert.equal(storedHand(store, key, 7n, "99".repeat(32)), null);
  assert.equal(storedHand(store, key, 8n, seed), null);
  assert.equal(seedUsed(store, key, seed), true, "a seed that was bet is still a used seed");
  // A bet whose seed wasn't kept (storage cleared) still binds, from its receipt.
  bindBet(store, key, "55".repeat(32), { game: 9n, commit, txId: tx });
  assert.equal(storedHand(store, key, 9n, "55".repeat(32)).commit, commit);
});

test("stored hands are read defensively and bounded", () => {
  const key = storeKey("c", TABLE);
  const store = memory();
  assert.deepEqual(readStore(null, key), { v: 1, pending: [], hands: {} });
  for (const junk of ["not json", "null", "[]", '{"pending":"x","hands":7}', '{"hands":{"1":{"seed":"<img>"}},"pending":[{"seed":5}]}']) {
    store.setItem(key, junk);
    assert.deepEqual(readStore(store, key), { v: 1, pending: [], hands: {} }, junk);
  }
  store.setItem(key, JSON.stringify({ hands: { 4: { seed: "ab".repeat(32), commit: "javascript:alert(1)", betTx: "x", txs: ["cd".repeat(32), "<b>"], extra: 1 } } }));
  assert.deepEqual(readStore(store, key).hands["4"], { seed: "ab".repeat(32), game: "4", txs: ["cd".repeat(32)], at: 0 });
  store.setItem(key, "{}");
  for (let i = 0; i < 30; i++) rememberSeed(store, key, i.toString(16).padStart(64, "0"), i);
  assert.equal(readStore(store, key).pending.length, 20, "old seeds that never became hands are dropped");
  for (let i = 0; i < 120; i++) bindBet(store, key, (1000 + i).toString(16).padStart(64, "0"), { game: BigInt(i) });
  const ids = Object.keys(readStore(store, key).hands);
  assert.equal(ids.length, 100);
  assert.ok(ids.includes("119") && !ids.includes("0"), "the newest hands are kept");
  // A storage that refuses writes doesn't break the page.
  rememberSeed({ getItem: () => null, setItem: () => { throw new Error("full"); } }, key, "ee".repeat(32), 1);
  assert.equal(ADULT_KEY, "blockula.blackjack.adult");
});

// ---------------------------------------------------------------------------------- the check

test("every vector hand verifies from its last link and seed alone: each card, both hands, the outcome and the payout", () => {
  let settled = 0;
  for (const h of vectors.hands) {
    if (!h.outcome) continue;
    settled++;
    const kept = { seed: h.client_seed, commit: h.commit, txs: [], at: 0 };
    const c = checkSettledHand(lastOf(h), kept);
    assert.equal(c.ok, true, `hand ${h.game_id}: ${c.problem}`);
    assert.equal(c.commit, h.commit);
    assert.equal(c.commitCheck, "matches");
    assert.equal(c.seed, "yours");
    assert.deepEqual(c.steps.map((s) => s.link), h.links);
    assert.deepEqual(c.steps.map((s) => s.card), h.cards);
    assert.deepEqual(c.steps.filter((s) => s.to === "player").map((s) => s.card), h.player);
    assert.deepEqual(c.steps.filter((s) => s.to === "dealer").map((s) => s.card), h.dealer);
    assert.deepEqual([c.player, c.dealer, c.playerTotal, c.dealerTotal], [h.player, h.dealer, h.player_total, h.dealer_total]);
    assert.deepEqual([c.outcome, c.payout, c.forfeited], [h.outcome, BigInt(h.payout), false]);
    assert.match(c.verdict, /^Verified\. Every card comes from the dealer's commitment and your seed\.$/);
    // Without the commitment from the bet, the cards still check, and the verdict says what wasn't compared.
    const bare = checkSettledHand(lastOf(h));
    assert.deepEqual([bare.ok, bare.commitCheck, bare.seed, bare.commit], [true, "unknown", "unknown", h.commit]);
    assert.match(bare.verdict, /doesn't hold the commitment/);
  }
  assert.ok(settled >= 32, `${settled} settled vector hands`);
});

test("the check refuses a hand whose links, commitment, cards, outcome or payout don't fit", () => {
  const h = vectors.hands.find((x) => x.outcome === Outcome.Win && x.moves === "HS");
  const kept = { seed: h.client_seed, commit: h.commit, txs: [], at: 0 };
  const last = lastOf(h);
  const bad = (l, k = kept) => checkSettledHand(l, k);
  // A link that isn't the chain's: it hashes down to another commitment than the bet took.
  let c = bad({ ...last, link: "00".repeat(32) });
  assert.equal(c.ok, false);
  assert.match(c.verdict, /^Not verified: these links don't hash down to the commitment the bet took\.$/);
  assert.deepEqual([c.steps, c.commit], [[], null]);
  // Another commitment than the one this browser kept from the bet.
  c = bad(last, { ...kept, commit: "ab".repeat(32) });
  assert.match(c.problem, /commitment the bet took/);
  // The recorded outcome, the payout and the player's card count are each held to the cards.
  assert.match(bad({ ...last, outcome: Outcome.Lose, payout: 0n }).problem, /cards give outcome 2, not the recorded 4/);
  assert.match(bad({ ...last, payout: last.payout + 1n }).problem, /payout isn't what the outcome pays/);
  assert.equal(bad({ ...last, nPlayer: last.nPlayer + 1 }).ok, false);
  assert.equal(bad({ ...last, dealt: last.dealt - 1 }).ok, false, "a link short");
  // Another seed deals other cards: with the commitment unknown the links still chain, but the outcome doesn't fit.
  const other = checkSettledHand({ ...last, clientSeed: "5a".repeat(32) });
  assert.equal(other.seed, "unknown");
  // A stored hand for another seed isn't used: its commitment doesn't vouch for this hand.
  assert.equal(checkSettledHand(last, { ...kept, seed: "5a".repeat(32) }).commitCheck, "unknown");
});

test("an open hand checks at every step, against the cards its record shows", () => {
  const h = vectors.hands.find((x) => x.moves === "HS" && x.outcome);
  const kept = { seed: h.client_seed, commit: h.commit, txs: [], at: 0 };
  for (let n = 0; n < h.links.length; n++) {
    const g = gameAt(h, n);
    const c = checkOpenHand(g, kept);
    assert.equal(c.ok, true, `${n} links: ${c.problem}`);
    assert.equal(c.commit, h.commit);
    assert.equal(c.steps.length, n);
    assert.deepEqual([c.player, c.dealer], [g.playerCards, g.dealerCards]);
    assert.equal(c.outcome, null);
    assert.equal(c.payout, null);
  }
  // Before any card, the record's link is the commitment itself.
  assert.equal(checkOpenHand(gameAt(h, 0)).commit, h.commit);
  // A node that shows other cards than the links give is caught.
  const g = gameAt(h, 3);
  const lied = checkOpenHand({ ...g, playerCards: [g.playerCards[0], (g.playerCards[1] + 1) % 52] }, kept);
  assert.equal(lied.ok, false);
  assert.match(lied.verdict, /cards on the table aren't the ones the links give/);
});

test("a hand the dealer forfeited verifies as a timeout, paid the most it could still have paid", () => {
  const h = vectors.hands.find((x) => x.links.length >= 3);
  const stake = 2n * F;
  // The dealer never dealt: no link revealed, the commitment is the record's link, the player is paid 3:2.
  const never = checkSettledHand({ game: BigInt(h.game_id), outcome: Outcome.ForfeitDeal, dealt: 0, nPlayer: 0, stake, payout: payout(stake, Outcome.ForfeitDeal), clientSeed: h.client_seed, link: h.commit }, { seed: h.client_seed, commit: h.commit, txs: [], at: 0 });
  assert.deepEqual([never.ok, never.forfeited, never.steps.length, never.payout], [true, true, 0, 5n * F]);
  assert.equal(never.commitCheck, "matches");
  // The same record claiming a plain win isn't what an undealt hand can end as.
  assert.equal(checkSettledHand({ game: BigInt(h.game_id), outcome: Outcome.Win, dealt: 0, nPlayer: 0, stake, payout: 2n * stake, clientSeed: h.client_seed, link: h.commit }).ok, false);
});

test("the pictures and their words come from the checked cards", () => {
  const h = vectors.hands.find((x) => x.outcome === Outcome.Win);
  const c = checkSettledHand(lastOf(h));
  const snap = snapshotOfSettled(lastOf(h), { player: c.player, dealer: c.dealer }, { player: "mina.boo" });
  assert.match(altText(snap), /mina\.boo's cards: /);
  assert.match(altText(snap), /You win\./);
  const svg = tableSvg(snap, { fonts: { serif: '"EB Garamond", serif' } });
  assert.match(svg, /^<svg /);
  assert.doesNotMatch(svg, /<script|<style|\sstyle=|\son[a-z]+=|javascript:/i, "nothing the CSP would refuse, nothing that runs");
  const open = snapshotOfGame(gameAt(h, 3));
  assert.equal(open.holeHidden, true);
  assert.match(altText(open), /one card face down/);
});

test("the log says a hand's events from the player's side", () => {
  assert.equal(logLine({ name: "bet", game: 7n, player: ME, stake: 2n * F, clientSeed: "", commit: "", deadline: 0n, table: false, postId: 0n }, ME), "You bet 2 FANG. Hand 7.");
  assert.equal(logLine({ name: "hit", game: 7n, player: ME, deadline: 0n }, ME), "You hit.");
  assert.equal(logLine({ name: "hit", game: 7n, player: OTHER, deadline: 0n }, ME), "The player hit.");
  assert.equal(logLine({ name: "stand", game: 7n, player: ME, deadline: 0n, idle: false }, ME), "You stood.");
  assert.equal(logLine({ name: "stand", game: 7n, player: ME, deadline: 0n, idle: true }, ME), "Time ran out, so the hand stood.");
  assert.equal(logLine({ name: "cards", game: 7n, player: ME, first: 1, cards: [{ link: "", card: 0 }, { link: "", card: 22 }], phase: 2, playerTotal: 0, dealerTotal: 0, deadline: 0n }, ME), "The dealer revealed 2 cards: A♠ 10♥.");
  assert.equal(logLine({ name: "settled", game: 7n, player: ME, outcome: Outcome.Blackjack, stake: 2n * F, payout: 5n * F, toTable: false, dealt: 3, nPlayer: 2, clientSeed: "", link: "", playerTotal: 21, dealerTotal: 10 }, ME), "Blackjack. 5 FANG came back from a 2 FANG bet.");
  assert.equal(logLine({ name: "paused", on: true }, ME), null);
  assert.equal(payoutWords(2n * F, 0n), "Nothing came back from the 2 FANG bet.");
  assert.equal(payoutWords(2n * F, 2n * F), "The 2 FANG bet came back.");
});

test("a relayer's grant is held to the table's limits before it's reviewed", () => {
  const g = { relayer: OTHER, maxBet: "5", dailyCap: "20", blocks: "3600" };
  assert.equal(grantProblem(g, DEFAULT_LIMITS, "boo"), null);
  assert.match(grantProblem({ ...g, relayer: "bot.boo" }, DEFAULT_LIMITS), /never a \.boo name/);
  assert.match(grantProblem({ ...g, maxBet: "26" }, DEFAULT_LIMITS), /most a relayed bet may be at this table is 25 FANG/);
  assert.match(grantProblem({ ...g, maxBet: "0" }, DEFAULT_LIMITS), /most per bet/);
  assert.match(grantProblem({ ...g, dailyCap: "4" }, DEFAULT_LIMITS), /can't be less than the most per bet/);
  assert.match(grantProblem({ ...g, blocks: "43201" }, DEFAULT_LIMITS), /at most 43,200 blocks/);
  assert.match(grantProblem({ ...g, blocks: "soon" }, DEFAULT_LIMITS), /number of blocks/);
  assert.match(grantProblem({ ...g, blocks: "0" }, DEFAULT_LIMITS), /number of blocks/);
});

// ---------------------------------------------------------------------------------- the share card

test("the share card's URLs are absolute https ones under the wallet's public origin", () => {
  assert.equal(shareOrigin(undefined, "wallet.blockula.test", false), "https://wallet.blockula.test");
  assert.equal(shareOrigin("https://play.blockula.test/", "wallet.blockula.test", false), "https://play.blockula.test");
  assert.equal(shareOrigin("http://play.blockula.test", "wallet.blockula.test", false), null, "never plain http");
  assert.equal(shareOrigin(undefined, "", true), null, "the served wallet can't know its host when it's built");
  assert.equal(shareOrigin("https://node.blockula.test", "", true), "https://node.blockula.test");
  assert.equal(shareOrigin(undefined, "localhost", false), null);
  const target = shareTarget({ table: TABLE, player: ME, game: "12" });
  assert.deepEqual(target, { table: TABLE, player: ME, game: "12" });
  assert.equal(targetQuery(target), `?table=${TABLE}&player=${ME}&game=12`);
  // Only addresses and a hand's number are taken from a link.
  assert.deepEqual(shareTarget({ table: '"><script>', player: "mina.boo", game: "1e9" }, TABLE), { table: TABLE });
  assert.equal(targetQuery({}), "");
  const t = shareTags("https://wallet.blockula.test", "", target);
  assert.deepEqual(t.twitter, {
    "twitter:card": "player",
    "twitter:title": "Blockula Blackjack",
    "twitter:description": COPY.share.description,
    "twitter:player": `https://wallet.blockula.test/blackjack/embed?table=${TABLE}&player=${ME}&game=12`,
    "twitter:player:width": String(PLAYER_WIDTH),
    "twitter:player:height": String(PLAYER_HEIGHT),
    "twitter:image": "https://wallet.blockula.test/blackjack/poster.png",
    "twitter:image:alt": POSTER_ALT,
  });
  assert.equal(t.og["og:image"], t.twitter["twitter:image"]);
  assert.equal(t.og["og:image:alt"], POSTER_ALT);
  assert.equal(t.og["og:url"], `https://wallet.blockula.test/blackjack/share?table=${TABLE}&player=${ME}&game=12`);
  // The served wallet lives under /wallet.
  assert.equal(shareTags("https://node.blockula.test", "/wallet", {}).twitter["twitter:player"], "https://node.blockula.test/wallet/blackjack/embed");
  // Without an origin there is no card: a title and a description only.
  const none = shareTags(null, "", target);
  assert.deepEqual(none.twitter, {});
  assert.deepEqual(Object.keys(none.og).sort(), ["og:description", "og:site_name", "og:title", "og:type"]);
});

// ---------------------------------------------------------------------------------- copy

/** Every string in a value, however nested. */
function strings(v, out = []) {
  if (typeof v === "string") out.push(v);
  else if (Array.isArray(v)) v.forEach((x) => strings(x, out));
  else if (v && typeof v === "object") Object.values(v).forEach((x) => strings(x, out));
  return out;
}

/** Words the pages never say. `programs`, never the other word; nothing that says FANG is worth anything or can be had for money. */
const BANNED = /\b(contracts?|casinos?|jackpots?|cash|demo|example|mainnet|win(ning)? money|real money)\b/i;

test("the note is on every page's copy, word for word", () => {
  assert.equal(NOTE, "FANG has no value and can't be bought; no prizes.");
  assert.equal(COPY.note, NOTE);
  assert.ok(COPY.share.description.endsWith(NOTE));
  assert.ok(rulesText(DEFAULT_LIMITS)[0].body[0].includes(NOTE));
  // Each page renders it from the one constant.
  for (const file of ["../app/blackjack/view.tsx", "../app/blackjack/embed/view.tsx", "../app/blackjack/share/page.tsx"]) {
    const src = readFileSync(new URL(file, import.meta.url), "utf8");
    assert.match(src, /\{NOTE\}/, file);
    assert.match(src, /data-testid="bj-note"/, file);
  }
});

test("the rules state the table's limits as the chain has them, and the game as one played with FANG", () => {
  const text = strings(rulesText(DEFAULT_LIMITS)).join("\n");
  assert.match(text, /a game played with FANG/);
  assert.match(text, /from 1 FANG to 50 FANG, and never more than 1\/20 of the house's free bankroll/);
  assert.match(text, /up to 500 FANG a day here\. A day is 43,200 blocks/);
  assert.match(text, /Up to 10 hands can be open at once/);
  assert.match(text, /You have 1,800 blocks \(about 30 min\) to move\. After that your hand stands by itself\. Anyone can trigger that/);
  assert.match(text, /The dealer has 150 blocks \(about 3 min\) to reveal each card it owes\. If it doesn't, you can claim the hand/);
  // The bet's two steps, the seed's deadline and why a seed that never comes costs the bet.
  assert.match(text, /you have 300 blocks \(about 5 min\) to reveal your seed\. This page asks for it right away\. If the seed isn't revealed in time, the bet goes to the house/);
  assert.match(text, /Until you reveal your seed nobody can know a single card, you included/);
  assert.match(text, /A bet is two steps\. First the bet goes in with only a hash of your seed/);
  assert.match(text, /The house can't get ahead of your bet, because your seed stays hidden until your bet already holds its commitment/);
  assert.match(strings(rulesText({ ...DEFAULT_LIMITS, seedBlocks: 60n }, 2000)).join("\n"), /you have 60 blocks \(about 2 min\) to reveal your seed/);
  assert.match(COPY.seed.missing, /can only be revealed from the browser that placed the bet/);
  assert.match(COPY.bet.seed, /The bet sends only their hash/);
  assert.match(text, /stands on every 17\. There is no split, no double and no insurance/);
  assert.match(text, /two and a half times your bet/);
  // Other limits, other words: nothing is a constant of the page.
  const other = strings(rulesText({ ...DEFAULT_LIMITS, minBet: F / 2n, maxBet: 7n * F, bankrollDivisor: 40n, idleBlocks: 600n }, 2000)).join("\n");
  assert.match(other, /from 0\.5 FANG to 7 FANG, and never more than 1\/40/);
  assert.match(other, /600 blocks \(about 20 min\)/);
});

test("nothing the pages say uses a word the copy rules ban", () => {
  const said = [
    ...strings(COPY),
    ...strings(rulesText(DEFAULT_LIMITS)),
    ...Object.values(ABORTS),
    ...[1, 2, 3, 4, 5, 6, 7, 8].map(outcomeWords),
    handState(waitingFor(vectors.hands[0]), ME, 100_000).words,
    handState(waitingFor(vectors.hands[0]), null, 100_000).words,
    handState(waitingFor(vectors.hands[0]), ME, 200_000).words,
    handState(waitingFor(vectors.hands[0]), null, 200_000).words,
    checkOpenHand(waitingFor(vectors.hands[0])).verdict,
    checkSettledHand({ game: 1n, outcome: Outcome.NoSeed, dealt: 0, nPlayer: 0, stake: F, payout: 0n, clientSeed: null, seedHash: "ab".repeat(32), link: vectors.hands[0].commit }).verdict,
    logLine({ name: "seed", game: 7n, player: ME, clientSeed: "", deadline: 0n }, ME),
    logLine({ name: "seed", game: 7n, player: OTHER, clientSeed: "", deadline: 0n }, ME),
    POSTER_ALT,
    ...strings(shareTags("https://wallet.blockula.test", "", {})),
    betProblem("0.5", ctx()),
    betProblem("99", ctx()),
    ...[{ house: house({ paused: true }) }, { house: house({ open: 10 }) }, { commitmentReady: false }, { house: house({ bankroll: 0n }) }].map((o) => tableProblem(ctx(o))),
    handState(gameAt(vectors.hands[0], 0), ME, 100_000).words,
    handState(gameAt(vectors.hands[0], 0), ME, 200_000).words,
    handState(gameAt(vectors.hands[0], 3, { phase: Phase.Player }), ME, 200_000).words,
    checkSettledHand(lastOf(vectors.hands.find((h) => h.outcome))).verdict,
  ];
  assert.ok(said.length > 120, `${said.length} strings checked`);
  for (const s of said) assert.doesNotMatch(s, BANNED, s);
  // The pages' own source too: what's written between the tags and in their strings (imports and comments aside).
  const dirs = ["../app/blackjack/", "../app/blackjack/embed/", "../app/blackjack/share/", "../components/blackjack/"];
  const files = dirs.flatMap((d) => readdirSync(new URL(d, import.meta.url)).filter((f) => /\.tsx?$/.test(f)).map((f) => d + f));
  assert.ok(files.length >= 8, `${files.length} source files checked`);
  for (const file of [...files, "./blackjack.ts", "./blackjack-copy.ts", "./blackjack-share.ts"]) {
    const src = readFileSync(new URL(file, import.meta.url), "utf8")
      .split("\n")
      .filter((line) => !/^\s*(import\b|\/\/|\/?\*)/.test(line))
      .join("\n");
    const hit = src.match(BANNED);
    assert.equal(hit, null, `${file}: “${hit?.[0]}”`);
  }
});

test("the table is always called a program, and FANG is never priced", () => {
  const said = [...strings(COPY), ...strings(rulesText(DEFAULT_LIMITS))].join("\n");
  assert.match(said, /program/);
  assert.doesNotMatch(said, /[$€£]|\bUSD\b|\bworth\b/i);
  assert.match(COPY.balance.grant, /It can only ever send your balance back to you/);
  assert.match(COPY.gate.label, /18 or older/);
});
