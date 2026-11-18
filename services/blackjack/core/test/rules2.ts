// Rules v2 (the `blackjack2` program: docs/proposals/blackjack-dealer-outage.md) against the
// vectors the program's own rules wrote (node/vm/tests/blackjack2_rules.rs → test/vectors2.json),
// and the v2 bytes against layouts written out by hand from the program's doc comment. Offline.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  ABORTS,
  DEFAULT_LIMITS,
  DEFAULT_LIMITS_V2,
  Hand,
  OPENING_LINKS,
  Outcome,
  Phase,
  bytesAddress,
  card,
  chainFromTop,
  dealers,
  decodeEvent,
  decodeGame,
  decodeHouse,
  decodeLimits,
  decodePlayer,
  decodeRules,
  decodeStep,
  forfeitOf,
  input,
  isOutcome,
  outcomeWords,
  owedLinks,
  payout,
  phaseWords,
  seedHash,
  seeded,
  stampWords,
  storageKey,
  toHex,
  total,
  verifyHand,
  type Game,
  type Rules,
} from "../src/index.js";

interface VectorHand {
  top: string;
  client_seed: string;
  game_id: string;
  moves: string;
  commit: string;
  links: string[];
  cards: number[];
  player: number[];
  dealer: number[];
  outcome: number;
  stake: string;
  payout: string;
  /** What a missed deadline ends the hand as with `k` links revealed, bet from an account. */
  opened_stalls: number[];
  /** The same, for a relayed hand (dealt as v1). */
  relayed_stalls: number[];
}
const v = JSON.parse(readFileSync(new URL("./vectors2.json", import.meta.url), "utf8")) as {
  rules: number;
  opening_links: number;
  payouts: { stake: string; outcome: number; payout: string }[];
  forfeits: { phase: number; dealt: number; player: number[]; dealer: number[]; forfeit: number | null }[];
  hands: VectorHand[];
};

assert.equal(v.rules, 2);
assert.equal(v.opening_links, OPENING_LINKS);

// ---- the numbers ----
assert.equal(Outcome.Refund, 9);
assert.equal(Phase.Open, 6);
assert.ok(isOutcome(9) && !isOutcome(10) && !isOutcome(0));
assert.ok(dealers(Phase.Open) && !seeded(Phase.Open) && !seeded(Phase.Seed) && seeded(Phase.Deal) && seeded(Phase.Player));
assert.ok(!dealers(Phase.Seed) && !dealers(Phase.Player) && !dealers(Phase.Done));
assert.match(outcomeWords(Outcome.Refund), /stake is back/);
assert.equal(stampWords(Outcome.Refund), "Refunded");
assert.match(phaseWords({ phase: Phase.Open, outcome: null }), /dealer to open/);

// ---- payouts: every outcome, the refund included ----
assert.equal(v.payouts.length, 4 * 9);
for (const p of v.payouts) {
  assert.ok(isOutcome(p.outcome));
  assert.equal(payout(BigInt(p.stake), p.outcome), BigInt(p.payout), `outcome ${p.outcome} of ${p.stake}`);
}
for (const stake of [0n, 1n, 3n, 7_000_000_001n, 400_000_000_000_000_000n]) assert.equal(payout(stake, Outcome.Refund), stake, "a refund is the stake");

// ---- what a stall ends every kind of state as ----
assert.ok(v.forfeits.length > 300);
const kinds = new Set<string>();
for (const s of v.forfeits) {
  const state = { phase: s.phase as Phase, player: s.player };
  assert.equal(forfeitOf(state, 2), s.forfeit, `phase ${s.phase}, player ${s.player}, dealer ${s.dealer}`);
  kinds.add(`${s.phase}:${s.forfeit}`);
  // v1 never paid less, and paid 3:2 for any stall during the deal.
  const v1 = forfeitOf(state, 1);
  if (s.phase === Phase.Deal) assert.equal(v1, Outcome.ForfeitDeal);
  if (s.phase === Phase.Hit || s.phase === Phase.Dealer) assert.equal(v1, s.forfeit, "after the deal the two agree");
}
for (const kind of ["6:9", "0:null", "1:6", "1:7", "2:null", "3:6", "4:6", "4:7"]) assert.ok(kinds.has(kind), `no state of kind ${kind}`);

// ---- hands: opened before the seed, and relayed ----
const seenOutcomes = new Set<number>();
assert.ok(v.hands.length >= 30);
for (const [i, h] of v.hands.entries()) {
  const id = BigInt(h.game_id);
  const stake = BigInt(h.stake);
  const chain = chainFromTop(h.top);
  assert.equal(chain[0], h.commit, `hand ${i}`);
  assert.deepEqual(chain.slice(1, h.links.length + 1), h.links);
  const cards = h.links.map((l, j) => card(l, h.client_seed, id, j + 1));
  assert.deepEqual(cards, h.cards);
  seenOutcomes.add(h.outcome);

  // Plays the hand from `hand`, checking the stall at every link from `k` on; returns its outcome.
  const finish = (hand: Hand, k: number, stalls: number[], settled: Outcome | null): Outcome => {
    let m = 0;
    let outcome = settled;
    while (outcome === null) {
      if (hand.phase === Phase.Player) {
        assert.equal(forfeitOf(hand, 2), null, "the player's turn");
        if (h.moves[m++] === "H") hand.hit();
        else hand.stand();
      }
      assert.equal(forfeitOf(hand, 2), stalls[k], `hand ${i}: a stall after ${k} links`);
      const step = hand.deal(cards[k++]);
      if (step?.kind === "settled") outcome = step.outcome;
    }
    assert.equal(m, h.moves.length);
    assert.equal(k, cards.length);
    return outcome;
  };

  // Bet from an account: three links that deal nothing, then the seed deals three cards.
  const opened = Hand.unopened();
  assert.equal(opened.dealOpening([0, 1, 2]), null, "no seed before the hand is opened");
  assert.equal(opened.deal(0), null, "an opening link deals no card");
  assert.ok(!opened.hit() && !opened.stand());
  for (let k = 0; k < OPENING_LINKS; k++) {
    assert.equal(h.opened_stalls[k], Outcome.Refund);
    assert.equal(forfeitOf(opened, 2), Outcome.Refund);
    assert.equal(opened.openLink(), k === OPENING_LINKS - 1);
    assert.deepEqual([opened.player, opened.dealer], [[], []]);
  }
  assert.equal(opened.phase, Phase.Seed);
  assert.equal(forfeitOf(opened, 2), null, "the player owes the seed");
  assert.equal(opened.openLink(), null);
  const step = opened.dealOpening([cards[0], cards[1], cards[2]]);
  assert.ok(step);
  assert.deepEqual([opened.player, opened.dealer, opened.dealt], [[cards[0], cards[2]], [cards[1]], 3]);
  assert.equal(opened.dealOpening([cards[0], cards[1], cards[2]]), null, "once");
  assert.equal(finish(opened, 3, h.opened_stalls, step.kind === "settled" ? step.outcome : null), h.outcome);
  assert.deepEqual([opened.player, opened.dealer], [h.player, h.dealer]);

  // Relayed: dealt as v1, card by card.
  assert.equal(finish(new Hand(), 0, h.relayed_stalls, null), h.outcome);
  assert.deepEqual(h.opened_stalls.slice(3), h.relayed_stalls.slice(3), "from the seed on both forfeit alike");

  // The fairness check, on the settled hand and on every point it could have stalled at.
  const newest = (k: number) => (k === 0 ? h.commit : h.links[k - 1]);
  for (const relayed of [false, true]) {
    const ok = verifyHand({ game: id, clientSeed: h.client_seed, link: newest(h.links.length), dealt: h.links.length, nPlayer: h.player.length, commit: h.commit, outcome: h.outcome as Outcome, stake, payout: BigInt(h.payout), rules: 2, relayed });
    assert.deepEqual([ok.outcome, ok.forfeited, ok.refunded, ok.unrevealed, ok.player, ok.dealer], [h.outcome, false, false, false, h.player, h.dealer]);
  }
  // How many of the first `k` cards are the player's, replaying the moves.
  const playerCardsAfter = (k: number): number => {
    const hand = new Hand();
    let m = 0;
    for (let j = 0; j < k; j++) {
      if (hand.phase === Phase.Player) {
        if (h.moves[m++] === "H") hand.hit();
        else hand.stand();
      }
      hand.deal(cards[j]);
    }
    return hand.player.length;
  };
  for (let k = 0; k < h.links.length; k++) {
    const nPlayer = playerCardsAfter(k);
    // A relayed hand that stalled with `k` links out.
    const want = h.relayed_stalls[k] as Outcome;
    const proof = { game: id, clientSeed: h.client_seed, link: newest(k), dealt: k, nPlayer, commit: h.commit, stake, rules: 2 as Rules, relayed: true };
    const stalled = verifyHand({ ...proof, outcome: want, payout: payout(stake, want) });
    assert.ok(stalled.forfeited && !stalled.refunded);
    const other = want === Outcome.Forfeit ? Outcome.ForfeitDeal : Outcome.Forfeit;
    assert.throws(() => verifyHand({ ...proof, outcome: other, payout: payout(stake, other) }), /only by the dealer's timeout/, `hand ${i}: ${k} links can't end as ${other}`);
    assert.throws(() => verifyHand({ ...proof, outcome: want, payout: payout(stake, want) + 1n }), /payout/);
    assert.throws(() => verifyHand({ ...proof, outcome: Outcome.Refund, payout: stake }), /never opened/);
    // Under v1 the same stall paid v1's way: 3:2 for any stall during the deal.
    const v1 = k < 3 ? Outcome.ForfeitDeal : want;
    assert.ok(verifyHand({ ...proof, rules: 1, outcome: v1, payout: payout(stake, v1) }).forfeited);
    if (k >= 3) {
      // From an account the same stall, with its seed out.
      assert.ok(verifyHand({ ...proof, relayed: false, outcome: want, payout: payout(stake, want) }).forfeited);
    } else {
      // From an account there is no seed and no card before the opening is done.
      assert.throws(() => verifyHand({ ...proof, relayed: false, outcome: want, payout: payout(stake, want) }), /three opening cards/);
      const hash = "ab".repeat(32);
      const unopened = { game: id, clientSeed: null, seedHash: hash, link: newest(k), dealt: k, nPlayer: 0, commit: h.commit, stake, rules: 2 as Rules };
      const refund = verifyHand({ ...unopened, outcome: Outcome.Refund, payout: stake });
      assert.deepEqual([refund.refunded, refund.unrevealed, refund.forfeited, refund.cards, refund.phase, refund.links.length], [true, false, false, [], Phase.Done, k]);
      assert.equal(verifyHand(unopened).phase, Phase.Open, "still waiting to be opened");
      assert.throws(() => verifyHand({ ...unopened, outcome: Outcome.Refund, payout: stake + 1n }), /refund is the stake/);
      assert.throws(() => verifyHand({ ...unopened, outcome: Outcome.Refund, payout: 0n }), /refund is the stake/);
      for (const wrong of [Outcome.NoSeed, Outcome.ForfeitDeal, Outcome.Forfeit, Outcome.Win]) assert.throws(() => verifyHand({ ...unopened, outcome: wrong, payout: payout(stake, wrong) }), /only as a refund/);
      assert.throws(() => verifyHand({ ...unopened, commit: "00".repeat(32), outcome: Outcome.Refund, payout: stake }), /commitment/);
      assert.throws(() => verifyHand({ ...unopened, relayed: true }), /relayed/);
      // A v1 table never has a link before its seed.
      if (k > 0) assert.throws(() => verifyHand({ ...unopened, rules: 1 }), /no cards/);
    }
  }
  // Opened, and its seed never came: three links, no cards, the stake is the house's.
  const waiting = { game: id, clientSeed: null, link: newest(3), dealt: 3, nPlayer: 0, commit: h.commit, stake, rules: 2 as Rules };
  assert.equal(verifyHand(waiting).phase, Phase.Seed);
  const lost = verifyHand({ ...waiting, outcome: Outcome.NoSeed, payout: 0n });
  assert.deepEqual([lost.unrevealed, lost.refunded, lost.links], [true, false, h.links.slice(0, 3)]);
  assert.throws(() => verifyHand({ ...waiting, outcome: Outcome.Refund, payout: stake }), /only as outcome 8/);
  assert.throws(() => verifyHand({ ...waiting, outcome: Outcome.NoSeed, payout: 1n }), /pays nothing/);
  assert.throws(() => verifyHand({ ...waiting, dealt: 4, link: chain[4] }), /three opening links/);
  assert.throws(() => verifyHand({ ...waiting, nPlayer: 1 }), /no cards/);
  assert.throws(() => verifyHand({ game: id, clientSeed: h.client_seed, link: newest(3), dealt: 3, nPlayer: 2, outcome: Outcome.Refund, rules: 2 }), /never opened/);

  // The dealer's side: what it owes, and never a link too many.
  const game = (over: Partial<Game>): Game => ({ id, player: "boo1x", stake, clientSeed: null, seedHash: "ab".repeat(32), link: h.commit, deadline: 0n, postId: 0n, phase: Phase.Open, table: false, dealt: 0, playerCards: [], dealerCards: [], playerTotal: 0, dealerTotal: 0, ...over });
  assert.deepEqual(owedLinks(game({}), chain), chain.slice(1, 4), "the three opening links, without the seed");
  assert.deepEqual(owedLinks(game({ dealt: 1, link: chain[1] }), chain), chain.slice(2, 4));
  assert.deepEqual(owedLinks(game({ dealt: 2, link: chain[2] }), chain), chain.slice(3, 4));
  assert.deepEqual(owedLinks(game({ phase: Phase.Seed, dealt: 3, link: chain[3] }), chain), [], "nothing until the seed is on the chain");
  assert.throws(() => owedLinks(game({ dealt: 3 }), chain), /fewer than three/);
  assert.throws(() => owedLinks(game({ clientSeed: h.client_seed }), chain), /no seed yet/);
  assert.throws(() => owedLinks(game({ playerCards: [1] }), chain), /no cards/);
  if (h.links.length > 3) {
    // Once the seed has dealt the three cards, the rest is v1's: everything the turn owes.
    const after = Hand.unopened();
    for (let k = 0; k < 3; k++) after.openLink();
    after.dealOpening([cards[0], cards[1], cards[2]]);
    if (after.phase === Phase.Player) {
      if (h.moves[0] === "H") after.hit();
      else after.stand();
    }
    const owed = owedLinks(game({ clientSeed: h.client_seed, seedHash: null, phase: after.phase, dealt: 3, link: chain[3], playerCards: after.player, dealerCards: after.dealer }), chain);
    assert.ok(owed.length >= 1);
    assert.deepEqual(owed, chain.slice(4, 4 + owed.length));
    assert.ok(total(after.player).total <= 21);
  }
}
for (let o = 1; o <= 5; o++) assert.ok(seenOutcomes.has(o), `no hand with outcome ${o}`);

// ---- the bytes ----
const le = (n: bigint | number, bytes = 8) => {
  let x = BigInt(n);
  let out = "";
  for (let i = 0; i < bytes; i++, x >>= 8n) out += (x & 0xffn).toString(16).padStart(2, "0");
  return out;
};
const A = (n: number) => new Uint8Array(32).fill(n);
const [alice, house] = [bytesAddress(A(1), "boo"), bytesAddress(A(3), "boo")];
const [a, hh] = [toHex(A(1)), toHex(A(3))];
const s1 = "aa".repeat(32);
const l1 = "c1".repeat(32);
const l2 = "c2".repeat(32);

// The version record: absent on a v1 table.
assert.equal(storageKey.version, "56");
assert.equal(decodeRules(null), 1);
assert.equal(decodeRules("02"), 2);
assert.throws(() => decodeRules("03"), /doesn't know/);
assert.throws(() => decodeRules("01"));

// The limits: fourteen, and each table refuses the other's length.
assert.equal(DEFAULT_LIMITS_V2.openBlocks, 150n);
const v1Limits = input.setLimits(DEFAULT_LIMITS);
const v2Limits = input.setLimits(DEFAULT_LIMITS_V2);
assert.equal(v1Limits.length, 2 * 104);
assert.equal(v2Limits, v1Limits + le(150));
assert.deepEqual(decodeLimits(v2Limits), { ...DEFAULT_LIMITS_V2 });
assert.deepEqual(decodeLimits(v1Limits), { ...DEFAULT_LIMITS });
assert.equal(decodeLimits(v1Limits).openBlocks, undefined);
assert.throws(() => decodeLimits(v2Limits + le(1)), /104 or 112/);
assert.deepEqual(decodeEvent("limits", v2Limits, "boo"), { name: "limits", limits: { ...DEFAULT_LIMITS_V2 } });

// The house: the pause is 0, 1 (the owner's) or 2 (tripped).
const houseHex = (paused: number) => hh + a + le(1) + le(2) + le(3) + le(4) + le(5) + le(6) + le(paused, 1) + le(7, 1);
assert.deepEqual([decodeHouse(houseHex(0), "boo").paused, decodeHouse(houseHex(0), "boo").tripped], [false, false]);
assert.deepEqual([decodeHouse(houseHex(1), "boo").paused, decodeHouse(houseHex(1), "boo").tripped], [true, false]);
assert.deepEqual([decodeHouse(houseHex(2), "boo").paused, decodeHouse(houseHex(2), "boo").tripped], [true, true]);
assert.equal(decodeHouse(houseHex(2), "boo").owner, house);
assert.throws(() => decodeHouse(houseHex(3), "boo"), /pause/);

// A hand being opened: the seed's hash, links and no cards.
const gameHex = (phase: number, dealt: number) => a + le(5) + s1 + l2 + le(900) + le(0) + le(phase, 1) + le(0, 1) + le(dealt, 1) + le(0, 1) + le(0, 1) + "00".repeat(32);
const opening = decodeGame(7n, gameHex(Phase.Open, 2), "boo");
assert.deepEqual([opening.phase, opening.clientSeed, opening.seedHash, opening.link, opening.dealt, opening.playerCards, opening.deadline], [Phase.Open, null, s1, l2, 2, [], 900n]);
const waits = decodeGame(7n, gameHex(Phase.Seed, 3), "boo");
assert.deepEqual([waits.phase, waits.clientSeed, waits.seedHash, waits.dealt], [Phase.Seed, null, s1, 3]);
assert.throws(() => decodeGame(7n, gameHex(7, 0), "boo"), /phase/);

// Events.
assert.deepEqual(decodeEvent("opened", le(7) + a + le(2, 1) + le(2, 1) + l1 + l2 + le(Phase.Seed, 1) + le(1310), "boo"), { name: "opened", game: 7n, player: alice, first: 2, links: [l1, l2], phase: Phase.Seed, deadline: 1310n });
assert.throws(() => decodeEvent("opened", le(7) + a + le(1, 1) + le(2, 1) + l1 + le(Phase.Open, 1) + le(1), "boo"));
assert.deepEqual(decodeEvent("tripped", le(7) + le(6, 1), "boo"), { name: "tripped", game: 7n, outcome: Outcome.Forfeit });
assert.deepEqual(decodeEvent("tripped", le(7) + le(7, 1), "boo"), { name: "tripped", game: 7n, outcome: Outcome.ForfeitDeal });
assert.throws(() => decodeEvent("tripped", le(7) + le(9, 1), "boo"), /trip/);
// A refund's `settled`: the seed field is the hash that was never opened.
const refunded = decodeEvent("settled", le(7) + a + le(9, 1) + le(5) + le(5) + le(0, 1) + le(2, 1) + le(0, 1) + s1 + l2 + le(0, 1) + le(0, 1), "boo");
assert.deepEqual(refunded, { name: "settled", game: 7n, player: alice, outcome: Outcome.Refund, stake: 5n, payout: 5n, toTable: false, dealt: 2, nPlayer: 0, clientSeed: null, seedHash: s1, link: l2, playerTotal: 0, dealerTotal: 0 });
// And the player's record of it.
const playerHex = le(0) + le(0) + le(0) + le(0) + "00".repeat(32) + le(0) + le(0) + le(0) + le(0) + le(0) + le(7) + le(9, 1) + le(2, 1) + le(0, 1) + le(5) + le(5) + s1 + l2;
assert.deepEqual(decodePlayer(playerHex, "boo").last, { game: 7n, outcome: Outcome.Refund, dealt: 2, nPlayer: 0, stake: 5n, payout: 5n, clientSeed: null, seedHash: s1, link: l2 });

// Returns: `reveal` on a hand being opened, a `timeout` that refunds, a seed that settles.
assert.deepEqual(decodeStep("0600"), { phase: Phase.Open, outcome: null });
assert.deepEqual(decodeStep("0000"), { phase: Phase.Seed, outcome: null });
assert.deepEqual(decodeStep("0509"), { phase: Phase.Done, outcome: Outcome.Refund });
assert.deepEqual(decodeStep("0501"), { phase: Phase.Done, outcome: Outcome.Blackjack });
assert.throws(() => decodeStep("050a"));
assert.match(ABORTS[73], /open/);
assert.equal(seedHash(alice, s1).length, 64);

console.log(`✓ rules v2: ${v.payouts.length} payouts, ${v.forfeits.length} stalled states, ${v.hands.length} hands opened and relayed, the v2 bytes`);
