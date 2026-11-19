/**
 * The fairness check, from what the chain publishes about a hand: its id, the player's seed, the
 * newest link revealed and how many were. Every earlier link is the hash of the next, and the
 * last hash is the commitment the dealer posted before the bet, so these four values fix every
 * card. Nothing here trusts the node's reading of the cards: they are recomputed.
 */
import { seedHash } from "./codec.js";
import { OPENING_LINKS, Outcome, Phase, forfeitOf, hexBytes, linksFromNewest, payout, prevLink, replayCounted, type Replay, type Rules } from "./rules.js";

/** What a hand's record or its `settled` event gives. */
export interface HandProof {
  game: bigint;
  /** The player's seed; `null` for a hand whose seed was never revealed (it has no cards). */
  clientSeed: string | null;
  /**
   * What the bet committed to (its `bet` event), with `player`, the player's address: when both
   * are given and the seed is known, the seed is checked against the hash.
   */
  seedHash?: string | null;
  player?: string;
  /** The newest link revealed (the commitment itself while none is). */
  link: string;
  /** Links revealed: cards dealt (on a v2 table, also the opening links of a hand whose seed never came). */
  dealt: number;
  /** How many of the cards are the player's. */
  nPlayer: number;
  /** The commitment the bet took (its `bet` event), when the verifier has it: checked. */
  commit?: string;
  /** The outcome the chain recorded, once settled: checked against the cards. */
  outcome?: Outcome;
  stake?: bigint;
  payout?: bigint;
  /**
   * The rules the hand's table runs (`Table.rules()`); 1 when left out. They decide what a hand
   * the cards didn't finish may have ended as: see `forfeitOf`.
   */
  rules?: Rules;
  /**
   * Rules v2: whether the hand was relayed (its `bet` event's `table`). Only a relayed hand is
   * dealt card by card; one bet from an account gets its first three cards at once, from its
   * seed. When given, that is checked.
   */
  relayed?: boolean;
}

export interface VerifiedHand extends Replay {
  /** The commitment the links hash down to. */
  commit: string;
  /** Every revealed link, in order. */
  links: string[];
  /** The dealer missed a deadline: the cards didn't finish the hand, the timeout did. */
  forfeited: boolean;
  /** The player never revealed the bet's seed: the hand has no cards and the stake went to the house. */
  unrevealed: boolean;
  /** Rules v2: the dealer never opened the hand, and the stake went back. The hand has no cards. */
  refunded: boolean;
  /** The seed was checked against the hash the bet committed to (both were given). */
  seedChecked: boolean;
}

/** A hand without its seed: it has no cards, whatever was revealed of its chain. */
function seedless(p: HandProof, rules: Rules, commit: string, links: string[]): VerifiedHand {
  if (p.nPlayer !== 0) throw new Error("a hand without its seed has no cards");
  // v1 reveals nothing before the seed. v2's dealer opens the hand first: up to three links.
  const most = rules === 2 ? OPENING_LINKS : 0;
  if (p.dealt > most) throw new Error(rules === 2 ? "a hand without its seed holds its three opening links at most" : "a hand without its seed has no cards");
  if (rules === 2 && p.relayed === true) throw new Error("a relayed hand's seed is known from its bet");
  const opened = rules === 1 || p.dealt === OPENING_LINKS;
  if (p.outcome !== undefined) {
    // It ended without cards: the player never revealed the seed of a hand that waited for it,
    // or (v2) the dealer never finished opening it.
    const ended = opened ? Outcome.NoSeed : Outcome.Refund;
    if (p.outcome !== ended) {
      throw new Error(opened ? `a hand without its seed ends only as outcome ${Outcome.NoSeed}, not ${p.outcome}` : `a hand the dealer never opened ends only as a refund (outcome ${Outcome.Refund}), not ${p.outcome}`);
    }
    if (p.payout !== undefined) {
      if (ended === Outcome.NoSeed && p.payout !== 0n) throw new Error("a bet whose seed never came pays nothing");
      if (ended === Outcome.Refund && p.stake !== undefined && p.payout !== p.stake) throw new Error("a refund is the stake, no more and no less");
    }
  }
  const phase = p.outcome !== undefined ? Phase.Done : opened ? Phase.Seed : Phase.Open;
  const settled = p.outcome !== undefined;
  return { cards: [], player: [], dealer: [], playerTotal: 0, dealerTotal: 0, phase, outcome: p.outcome ?? null, moves: "", commit, links, forfeited: false, unrevealed: settled && opened, refunded: settled && !opened, seedChecked: false };
}

/**
 * Recomputes a hand and checks it against what the chain recorded. Throws, saying what's wrong,
 * if the links don't hash down to the commitment given, if the seed isn't the one the bet
 * committed to (when the hash is given), if the cards the rules deal from them don't give the
 * recorded outcome, or if the payout isn't the outcome's.
 */
export function verifyHand(p: HandProof): VerifiedHand {
  const rules = p.rules ?? 1;
  const links = linksFromNewest(p.link, p.dealt);
  const commit = links.length ? prevLink(links[0]) : p.link;
  if (p.commit !== undefined && p.commit !== commit) throw new Error("these links don't hash down to the commitment the bet took");
  if (p.clientSeed === null) return seedless(p, rules, commit, links);
  hexBytes(p.clientSeed, 32);
  if (p.outcome === Outcome.NoSeed) throw new Error("a hand that ended without its seed can't have one");
  if (p.outcome === Outcome.Refund) throw new Error("a hand that was never opened can't have its seed");
  // A v2 hand bet from an account has no card until its seed deals the first three at once.
  if (rules === 2 && p.relayed === false && p.dealt < OPENING_LINKS) throw new Error("a hand bet from an account has its three opening cards as soon as it has its seed");
  let seedChecked = false;
  if (p.seedHash != null && p.player !== undefined) {
    if (seedHash(p.player, p.clientSeed) !== p.seedHash) throw new Error("the seed isn't the one the bet committed to");
    seedChecked = true;
  }
  const r = replayCounted(commit, p.clientSeed, p.game, links, p.nPlayer);
  let forfeited = false;
  if (p.outcome !== undefined) {
    if (r.outcome !== null) {
      if (r.outcome !== p.outcome) throw new Error(`the cards give outcome ${r.outcome}, not the recorded ${p.outcome}`);
    } else {
      // The cards didn't finish the hand: only a missed deadline ends it. Under v1 it pays the
      // most any hand could still be paid; under v2, the most these cards still allow. The
      // player moved before any stall that wasn't the deal's, so the hand was the dealer's.
      const stalled = { phase: r.phase === Phase.Player ? Phase.Dealer : r.phase, player: r.player };
      const most = forfeitOf(stalled, rules);
      if (most === null || p.outcome !== most) throw new Error(`an unfinished hand ends only by the dealer's timeout (outcome ${most}), not ${p.outcome}`);
      forfeited = true;
    }
    if (p.stake !== undefined && p.payout !== undefined && payout(p.stake, p.outcome) !== p.payout) throw new Error("the payout isn't what the outcome pays");
  }
  return { ...r, commit, links, forfeited, unrevealed: false, refunded: false, seedChecked };
}
