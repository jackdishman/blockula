/**
 * The fairness check, from what the chain publishes about a hand: its id, the player's seed, the
 * newest link revealed and how many were. Every earlier link is the hash of the next, and the
 * last hash is the commitment the dealer posted before the bet, so these four values fix every
 * card. Nothing here trusts the node's reading of the cards: they are recomputed.
 */
import { Outcome, Phase, linksFromNewest, natural, payout, prevLink, replayCounted, type Replay } from "./rules.js";

/** What a hand's record or its `settled` event gives. */
export interface HandProof {
  game: bigint;
  clientSeed: string;
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
}

/**
 * Recomputes a hand and checks it against what the chain recorded. Throws, saying what's wrong,
 * if the links don't hash down to the commitment given, if the cards the rules deal from them
 * don't give the recorded outcome, or if the payout isn't the outcome's.
 */
export function verifyHand(p: HandProof): VerifiedHand {
  const links = linksFromNewest(p.link, p.dealt);
  const commit = links.length ? prevLink(links[0]) : p.link;
  if (p.commit !== undefined && p.commit !== commit) throw new Error("these links don't hash down to the commitment the bet took");
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
  return { ...r, commit, links, forfeited };
}
