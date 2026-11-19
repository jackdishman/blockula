/**
 * The dealer's side of a hand: which links it owes now. A dealer that holds a hand's chain knows
 * every card it will ever deal, so it reveals everything a turn owes in one call: the three cards
 * of the deal (and the hole card against a natural), the one card of a hit (and its own hand
 * after a hit to 21), its whole hand after a stand. On a v2 table a hand bet from an account is
 * opened first: its chain's first three links, before the seed, and nothing more until the seed
 * has dealt them.
 */
import type { Game } from "./codec.js";
import { CHAIN_LEN, Hand, OPENING_LINKS, Phase, card, dealers } from "./rules.js";

/** The rules' state of an open hand, from its record. */
export function handOf(game: Pick<Game, "phase" | "dealt" | "playerCards" | "dealerCards">): Hand {
  const hand = new Hand();
  hand.phase = game.phase;
  hand.dealt = game.dealt;
  hand.player = [...game.playerCards];
  hand.dealer = [...game.dealerCards];
  return hand;
}

/**
 * The links the dealer owes on `game` now, in order: empty when it's the player's turn. `chain`
 * is the hand's chain from its commitment (`chainFromTop`): `chain[j]` deals card `j`.
 */
export function owedLinks(game: Pick<Game, "id" | "clientSeed" | "phase" | "dealt" | "playerCards" | "dealerCards">, chain: readonly string[]): string[] {
  if (chain.length !== CHAIN_LEN + 1) throw new Error("a chain is its commitment and 32 links");
  const hand = handOf(game);
  const out: string[] = [];
  const seed = game.clientSeed;
  if (hand.phase === Phase.Open) {
    // Rules v2: the dealer opens the hand with the chain's first three links before the seed is
    // revealed. They deal no card yet, and never a fourth: that would be a card the player
    // hasn't asked for, public before their seed is.
    if (seed !== null) throw new Error("a hand being opened has no seed yet");
    if (hand.dealt >= OPENING_LINKS || game.playerCards.length || game.dealerCards.length) throw new Error("a hand being opened holds fewer than three links and no cards");
    for (let j = hand.dealt + 1; j <= OPENING_LINKS; j++) out.push(chain[j]);
    return out;
  }
  // Before the player's seed is revealed the dealer owes nothing more, and no card can be known.
  if (seed === null) {
    if (dealers(hand.phase)) throw new Error("a hand the dealer owes a card has its seed");
    return out;
  }
  while (dealers(hand.phase)) {
    const j = hand.dealt + 1;
    if (j > CHAIN_LEN) throw new Error("a hand never takes more than 32 cards");
    out.push(chain[j]);
    hand.deal(card(chain[j], seed, game.id, j));
  }
  return out;
}
