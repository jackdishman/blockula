/**
 * The dealer's side of a hand: which links it owes now. A dealer that holds a hand's chain knows
 * every card it will ever deal, so it reveals everything a turn owes in one call: the three cards
 * of the deal (and the hole card against a natural), the one card of a hit (and its own hand
 * after a hit to 21), its whole hand after a stand.
 */
import type { Game } from "./codec.js";
import { CHAIN_LEN, Hand, card, dealers } from "./rules.js";

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
  while (dealers(hand.phase)) {
    const j = hand.dealt + 1;
    if (j > CHAIN_LEN) throw new Error("a hand never takes more than 32 cards");
    out.push(chain[j]);
    hand.deal(card(chain[j], game.clientSeed, game.id, j));
  }
  return out;
}
