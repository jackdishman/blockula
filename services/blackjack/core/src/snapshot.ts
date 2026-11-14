/**
 * A hand as a picture or a sentence shows it: the cards on the table, what's still face down,
 * the totals, the bet. The card renderer draws it; `altText` says it in words (every image of a
 * hand carries one); the table page and the X bot build it from a hand's record or its events.
 */
import { fang, type Game, type LastHand } from "./codec.js";
import { Outcome, Phase, cardName, natural, outcomeWords, total } from "./rules.js";
import type { VerifiedHand } from "./verify.js";

export interface TableSnapshot {
  gameId: bigint;
  playerCards: number[];
  /** The dealer's cards that are face up. */
  dealerCards: number[];
  /** The dealer's hole card is still face down (the hand isn't the dealer's to play yet). */
  holeHidden: boolean;
  /** Stake, in drops. */
  stake: bigint;
  phase: Phase;
  /** Set once the hand is settled. */
  outcome: Outcome | null;
  /** What the player got back (drops), once settled. */
  payout: bigint | null;
  /** The transaction that made this state, if known: shown shortened. */
  txId?: string;
  /** The player's `.boo` name or X handle, if known. Shown as text, never acted on. */
  player?: string;
}

/** An open hand's snapshot, from its record. */
export function snapshotOfGame(g: Game, extra: Pick<TableSnapshot, "txId" | "player"> = {}): TableSnapshot {
  return {
    gameId: g.id,
    playerCards: [...g.playerCards],
    dealerCards: [...g.dealerCards],
    // Until the dealer plays, its second card isn't dealt: it's shown as a card face down.
    holeHidden: g.dealerCards.length === 1,
    stake: g.stake,
    phase: g.phase,
    outcome: null,
    payout: null,
    ...extra,
  };
}

/** A settled hand's snapshot, from its record (or `settled` event) and its recomputed cards. */
export function snapshotOfSettled(last: Pick<LastHand, "game" | "outcome" | "stake" | "payout">, cards: Pick<VerifiedHand, "player" | "dealer">, extra: Pick<TableSnapshot, "txId" | "player"> = {}): TableSnapshot {
  return {
    gameId: last.game,
    playerCards: [...cards.player],
    dealerCards: [...cards.dealer],
    // A hand that ended before the dealer played (a bust, a natural the upcard couldn't match, a timeout) never turned the hole card.
    holeHidden: cards.dealer.length < 2,
    stake: last.stake,
    phase: Phase.Done,
    outcome: last.outcome,
    payout: last.payout,
    ...extra,
  };
}

const list = (cards: readonly number[]): string => {
  const names = cards.map(cardName);
  if (names.length <= 1) return names.join("");
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
};

/** "17", "soft 17", "blackjack". */
export function totalWords(cards: readonly number[]): string {
  if (cards.length === 0) return "no cards";
  if (natural(cards)) return "blackjack";
  const t = total(cards);
  return t.soft ? `soft ${t.total}` : `${t.total}`;
}

/** What comes next, for a caption: "Your move: hit or stand." */
export function phaseWords(s: Pick<TableSnapshot, "phase" | "outcome">): string {
  if (s.outcome !== null) return `${outcomeWords(s.outcome)}.`;
  switch (s.phase) {
    case Phase.Seed:
      return "The bet is placed. Waiting for the player's seed.";
    case Phase.Deal:
      return "Waiting for the dealer to deal.";
    case Phase.Player:
      return "Your move: hit or stand.";
    case Phase.Hit:
      return "Waiting for the dealer's card.";
    case Phase.Dealer:
      return "The dealer is playing.";
    case Phase.Done:
      return "The hand is over.";
  }
}

/** Alt text for a picture of the hand: every card, both totals, the bet and where the hand stands. */
export function altText(s: TableSnapshot): string {
  const who = s.player ? `${s.player}'s` : "The player's";
  const parts = [`A blackjack table on Blockula, hand ${s.gameId}.`];
  parts.push(s.playerCards.length ? `${who} cards: ${list(s.playerCards)}, ${totalWords(s.playerCards)}.` : `${who} cards aren't dealt yet.`);
  if (s.dealerCards.length === 0) parts.push("The dealer's cards aren't dealt yet.");
  else if (s.holeHidden) parts.push(`The dealer shows the ${list(s.dealerCards)} and one card face down, its back a black bat with red wings.`);
  else parts.push(`The dealer's cards: ${list(s.dealerCards)}, ${totalWords(s.dealerCards)}.`);
  parts.push(`Bet: ${fang(s.stake)} FANG.`);
  parts.push(phaseWords(s));
  if (s.outcome !== null && s.payout !== null) parts.push(s.payout > 0n ? `${fang(s.payout)} FANG came back.` : "Nothing came back.");
  return parts.join(" ");
}
