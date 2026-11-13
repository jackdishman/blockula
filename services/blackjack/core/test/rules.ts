// The TypeScript rules against the vectors the program's own code wrote
// (node/vm/tests/blackjack_rules.rs → test/vectors.json). Offline.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { CHAIN_LEN, Hand, MAX_DEALER_CARDS, MAX_PLAYER_CARDS, Outcome, Phase, card, cardLabel, cardName, chainFromTop, exposure, isOutcome, linksFromNewest, payout, prevLink, replay, replayMoves, total } from "../src/rules.js";

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
  player_total: number;
  dealer_total: number;
  outcome: number;
  stake: string;
  exposure: string;
  payout: string;
}
const v = JSON.parse(readFileSync(new URL("./vectors.json", import.meta.url), "utf8")) as {
  chain_len: number;
  max_player_cards: number;
  max_dealer_cards: number;
  hands: VectorHand[];
  totals: { cards: number[]; total: number; soft: boolean }[];
  payouts: { stake: string; outcome: number; payout: string }[];
};

assert.equal(v.chain_len, CHAIN_LEN);
assert.equal(v.max_player_cards, MAX_PLAYER_CARDS);
assert.equal(v.max_dealer_cards, MAX_DEALER_CARDS);
assert.ok(v.hands.length >= 60);

const seen = new Set<number>();
for (const [i, h] of v.hands.entries()) {
  const id = BigInt(h.game_id);
  // The chain: the commitment and every link from the top.
  const chain = chainFromTop(h.top);
  assert.equal(chain.length, CHAIN_LEN + 1);
  assert.equal(chain[0], h.commit, `hand ${i}: commitment`);
  assert.deepEqual(chain.slice(1, h.links.length + 1), h.links, `hand ${i}: links`);
  assert.deepEqual(linksFromNewest(h.links[h.links.length - 1], h.links.length), h.links, `hand ${i}: links from the newest`);
  // Each card on its own.
  h.links.forEach((link, k) => assert.equal(card(link, h.client_seed, id, k + 1), h.cards[k], `hand ${i}: card ${k + 1}`));
  // The whole hand, replayed with the player's moves.
  const r = replayMoves(h.commit, h.client_seed, id, h.links, h.moves);
  assert.deepEqual(r.cards, h.cards, `hand ${i}`);
  assert.deepEqual(r.player, h.player, `hand ${i}`);
  assert.deepEqual(r.dealer, h.dealer, `hand ${i}`);
  assert.equal(r.playerTotal, h.player_total, `hand ${i}`);
  assert.equal(r.dealerTotal, h.dealer_total, `hand ${i}`);
  assert.equal(r.outcome, h.outcome, `hand ${i}`);
  assert.equal(r.phase, Phase.Done);
  assert.ok(isOutcome(h.outcome));
  seen.add(h.outcome);
  const stake = BigInt(h.stake);
  assert.equal(exposure(stake).toString(), h.exposure);
  assert.equal(payout(stake, h.outcome as Outcome).toString(), h.payout);

  // A link from somewhere else, a link out of order, or one too many is refused.
  const wrong = [...h.links];
  wrong[wrong.length - 1] = "00".repeat(32);
  assert.throws(() => replayMoves(h.commit, h.client_seed, id, wrong, h.moves), /doesn't hash/);
  if (h.links.length > 3) {
    const swapped = [...h.links];
    [swapped[1], swapped[2]] = [swapped[2], swapped[1]];
    assert.throws(() => replayMoves(h.commit, h.client_seed, id, swapped, h.moves), /doesn't hash/);
  }
  assert.throws(() => replayMoves(h.commit, h.client_seed, id, [...h.links, chain[h.links.length + 1]], h.moves), /owed no card/);
  // Another seed or game deals other cards (all of them the same by chance: 1 in 52^n).
  if (h.cards.length >= 4) {
    assert.notDeepEqual(h.links.map((l, k) => card(l, h.client_seed, id ^ 1n, k + 1)), h.cards);
    assert.notDeepEqual(h.links.map((l, k) => card(l, "11".repeat(32), id, k + 1)), h.cards);
  }
  // Without moves, the deal alone replays (three links), and stops at the player's turn.
  const dealt = replay(h.commit, h.client_seed, id, h.links.slice(0, 3));
  assert.deepEqual(dealt.cards, h.cards.slice(0, 3));
  if (h.moves.length > 0) {
    assert.equal(dealt.phase, Phase.Player);
    assert.throws(() => replay(h.commit, h.client_seed, id, h.links.slice(0, 4)), /replayMoves/);
  }
}
for (const o of [1, 2, 3, 4, 5]) assert.ok(seen.has(o), `no hand with outcome ${o}`);

for (const t of v.totals) assert.deepEqual(total(t.cards), { total: t.total, soft: t.soft }, JSON.stringify(t.cards));
for (const p of v.payouts) assert.equal(payout(BigInt(p.stake), p.outcome as Outcome).toString(), p.payout);

// The state machine refuses moves out of turn.
const hand = new Hand();
assert.equal(hand.hit(), false);
assert.equal(hand.stand(), false);
hand.deal(9);
hand.deal(0);
assert.deepEqual(hand.deal(8), { kind: "player" });
assert.equal(hand.deal(3), null, "no card on the player's turn");
assert.equal(hand.stand(), true);
assert.equal(hand.stand(), false);
assert.deepEqual(hand.deal(5), { kind: "settled", outcome: Outcome.Win }, "19 against a soft 17: the dealer stands");
assert.equal(hand.deal(5), null);

assert.equal(prevLink("00".repeat(32)), "2ada83c1819a5372dae1238fc1ded123c8104fdaa15862aaee69428a1820fcda");
assert.equal(cardLabel(0), "A♠");
assert.equal(cardLabel(51), "K♣");
assert.equal(cardLabel(22), "10♥");
assert.equal(cardName(25), "king of hearts");
assert.throws(() => cardLabel(52));
assert.throws(() => card("00".repeat(32), "00".repeat(32), 0n, 0));
assert.throws(() => card("00".repeat(32), "00".repeat(32), 1n << 64n, 1));
assert.throws(() => card("00".repeat(31), "00".repeat(32), 0n, 1));
assert.throws(() => card("AA".repeat(32), "00".repeat(32), 0n, 1), /lowercase hex/);
assert.deepEqual(linksFromNewest("00".repeat(32), 0), []);

console.log(`✓ rules: ${v.hands.length} hands, ${v.totals.length} totals and ${v.payouts.length} payouts match the program's vectors`);
