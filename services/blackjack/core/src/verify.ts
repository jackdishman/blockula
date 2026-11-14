/**
 * The fairness check, from what the chain publishes about a hand: its id, the player's seed, the
 * newest link revealed and how many were. Every earlier link is the hash of the next, and the
 * last hash is the commitment the dealer posted before the bet, so these four values fix every
 * card. Nothing here trusts the node's reading of the cards: they are recomputed.
 */
import { seedHash } from "./codec.js";
import { Outcome, Phase, hexBytes, linksFromNewest, natural, payout, prevLink, replayCounted, type Replay } from "./rules.js";

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
  /** Links revealed: cards dealt. */
  dealt: number;
  /** How many of the cards are the player's. */
  nPlayer: number;
  /** The commitment the bet took (its `bet` event), when the verifier has it: checked. */
  commit?: string;
  /** The outcome the chain recorded, once settled: checked against the cards. */
  outcome?: Outcome;
  stake?: bigint;
  payout?: bigint;
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
  /** The seed was checked against the hash the bet committed to (both were given). */
  seedChecked: boolean;
}

/**
 * Recomputes a hand and checks it against what the chain recorded. Throws, saying what's wrong,
 * if the links don't hash down to the commitment given, if the seed isn't the one the bet
 * committed to (when the hash is given), if the cards the rules deal from them don't give the
 * recorded outcome, or if the payout isn't the outcome's.
 */
export function verifyHand(p: HandProof): VerifiedHand {
  const links = linksFromNewest(p.link, p.dealt);
  const commit = links.length ? prevLink(links[0]) : p.link;
  if (p.commit !== undefined && p.commit !== commit) throw new Error("these links don't hash down to the commitment the bet took");
  if (p.clientSeed === null) {
    // No seed, so no cards: the hand is waiting for its seed, or ended without one.
    if (p.dealt !== 0 || p.nPlayer !== 0) throw new Error("a hand without its seed has no cards");
    if (p.outcome !== undefined && p.outcome !== Outcome.NoSeed) throw new Error(`a hand without its seed ends only as outcome ${Outcome.NoSeed}, not ${p.outcome}`);
    if (p.outcome !== undefined && p.payout !== undefined && p.payout !== 0n) throw new Error("a bet whose seed never came pays nothing");
    return { cards: [], player: [], dealer: [], playerTotal: 0, dealerTotal: 0, phase: p.outcome === undefined ? Phase.Seed : Phase.Done, outcome: p.outcome ?? null, moves: "", commit, links, forfeited: false, unrevealed: p.outcome !== undefined, seedChecked: false };
  }
  hexBytes(p.clientSeed, 32);
  if (p.outcome === Outcome.NoSeed) throw new Error("a hand that ended without its seed can't have one");
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
      // The cards didn't finish the hand: only a missed deadline ends it, and it pays the most
      // the hand could still have paid.
      const most = r.phase === Phase.Deal || natural(r.player) ? Outcome.ForfeitDeal : Outcome.Forfeit;
      if (p.outcome !== most) throw new Error(`an unfinished hand ends only by the dealer's timeout (outcome ${most}), not ${p.outcome}`);
      forfeited = true;
    }
    if (p.stake !== undefined && p.payout !== undefined && payout(p.stake, p.outcome) !== p.payout) throw new Error("the payout isn't what the outcome pays");
  }
  return { ...r, commit, links, forfeited, unrevealed: false, seedChecked };
}
