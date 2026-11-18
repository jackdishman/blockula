/**
 * The rules of Blockula blackjack, as the program runs them (`node/vm/guests/bj-core`): how a
 * card comes out of the dealer's hash chain, what a hand totals, when the dealer draws, who wins
 * and what a stake pays. `test/vectors.json` (written by the Rust side) holds this file to the
 * program's own code.
 *
 * Rules v1: one hand against the dealer, an infinite deck, the dealer stands on every 17, a
 * natural pays 3:2, no split, double or insurance.
 *
 * Rules v2 (`node/vm/guests/bj2-core`, the `blackjack2` program, a separate table:
 * docs/proposals/blackjack-dealer-outage.md) deal the same cards and change what a dealer that
 * stops costs: `Phase.Open`, `Outcome.Refund`, `Hand.unopened`, `Hand.openLink`,
 * `Hand.dealOpening` and `forfeitOf`. `test/vectors2.json` holds those to the v2 program's code.
 * A v1 table never shows the v2 numbers.
 */
import { blake3 } from "@noble/hashes/blake3.js";

/** Links a chain holds after its commitment. */
export const CHAIN_LEN = 32;
export const MAX_PLAYER_CARDS = 20;
export const MAX_DEALER_CARDS = 12;
export const DEALER_STANDS_ON = 17;

const hex = (b: Uint8Array): string => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

/** Bytes from lowercase hex of exactly `bytes` bytes. */
export function hexBytes(s: string, bytes?: number): Uint8Array {
  if (typeof s !== "string" || !/^([0-9a-f]{2})*$/.test(s)) throw new Error("expected lowercase hex");
  if (bytes !== undefined && s.length !== 2 * bytes) throw new Error(`expected ${bytes} bytes of hex, got ${s.length / 2}`);
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.slice(2 * i, 2 * i + 2), 16);
  return out;
}

export const toHex = hex;

/** The link before `link` in a chain: `s_{j-1} = BLAKE3(s_j)` (hex in, hex out). */
export function prevLink(link: string): string {
  return hex(blake3(hexBytes(link, 32)));
}

/**
 * A dealer's chain from its top link `s_N`: `links[i]` is `s_i`, so `links[0]` is the commitment
 * and `links[j]` deals card `j`.
 */
export function chainFromTop(top: string): string[] {
  const links = [top];
  for (let i = 0; i < CHAIN_LEN; i++) links.push(prevLink(links[links.length - 1]));
  return links.reverse();
}

/**
 * Card `j` (from 1) of game `gameId`: the first 8 bytes (little-endian) of
 * BLAKE3(link ‖ client seed ‖ game id (8, LE) ‖ j), modulo 52. The bias is 16 parts in 2^64.
 */
export function card(link: string, clientSeed: string, gameId: bigint, j: number): number {
  if (!Number.isInteger(j) || j < 1 || j > CHAIN_LEN) throw new Error(`card index ${j} isn't 1 to ${CHAIN_LEN}`);
  if (gameId < 0n || gameId >= 1n << 64n) throw new Error("a game id is a u64");
  const pre = new Uint8Array(73);
  pre.set(hexBytes(link, 32), 0);
  pre.set(hexBytes(clientSeed, 32), 32);
  new DataView(pre.buffer).setBigUint64(64, gameId, true);
  pre[72] = j;
  const h = blake3(pre);
  return Number(new DataView(h.buffer, h.byteOffset).getBigUint64(0, true) % 52n);
}

/** A card's rank: 0 ace, 1–9 the cards 2–10, 10 jack, 11 queen, 12 king. */
export const rank = (c: number): number => c % 13;
/** A card's suit: 0 spades, 1 hearts, 2 diamonds, 3 clubs. */
export const suit = (c: number): number => Math.floor(c / 13);
/** What a card counts for: an ace 1, a face card 10. */
export const points = (c: number): number => (rank(c) <= 8 ? rank(c) + 1 : 10);

export const RANKS = ["A", "2", "3", "4", "5", "6", "7", "8", "9", "10", "J", "Q", "K"] as const;
export const SUITS = ["♠", "♥", "♦", "♣"] as const;
export const SUIT_NAMES = ["spades", "hearts", "diamonds", "clubs"] as const;
const RANK_NAMES = ["ace", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "jack", "queen", "king"] as const;

function checkCard(c: number): void {
  if (!Number.isInteger(c) || c < 0 || c > 51) throw new Error(`${c} isn't a card (0 to 51)`);
}

/** "A♠", "10♥". */
export function cardLabel(c: number): string {
  checkCard(c);
  return `${RANKS[rank(c)]}${SUITS[suit(c)]}`;
}

/** "ace of spades", for alt text. */
export function cardName(c: number): string {
  checkCard(c);
  return `${RANK_NAMES[rank(c)]} of ${SUIT_NAMES[suit(c)]}`;
}

/** A hand's best total and whether it's soft (an ace counted as 11). */
export function total(cards: readonly number[]): { total: number; soft: boolean } {
  let hard = 0;
  let ace = false;
  for (const c of cards) {
    checkCard(c);
    hard += points(c);
    ace ||= rank(c) === 0;
  }
  return ace && hard + 10 <= 21 ? { total: hard + 10, soft: true } : { total: hard, soft: false };
}

/** 21 in the first two cards. */
export const natural = (cards: readonly number[]): boolean => cards.length === 2 && total(cards).total === 21;
/** Whether a dealer showing `upcard` could still hold a natural. */
export const mayBeNatural = (upcard: number): boolean => points(upcard) === 1 || points(upcard) === 10;

/** The rules a table runs: 1 (`blackjack`) or 2 (`blackjack2`). */
export type Rules = 1 | 2;

/** Links a v2 dealer reveals to open a hand, before the player's seed. */
export const OPENING_LINKS = 3;

/**
 * How a hand ended: the program's numbers. `Refund` is rules v2's: the dealer never opened the
 * hand, and the stake went back.
 */
export const Outcome = { Blackjack: 1, Win: 2, Push: 3, Lose: 4, Bust: 5, Forfeit: 6, ForfeitDeal: 7, NoSeed: 8, Refund: 9 } as const;
export type Outcome = (typeof Outcome)[keyof typeof Outcome];

export function isOutcome(n: number): n is Outcome {
  return Number.isInteger(n) && n >= 1 && n <= 9;
}

/**
 * A hand's phase: the program's numbers. `Open` is rules v2's: the bet is placed and the dealer
 * owes the opening links, before it can know a card. Under v2 `Seed` means the hand is opened
 * and the player owes the seed; under v1, that the bet is placed and the player owes it.
 */
export const Phase = { Seed: 0, Deal: 1, Player: 2, Hit: 3, Dealer: 4, Done: 5, Open: 6 } as const;
export type Phase = (typeof Phase)[keyof typeof Phase];

/** Whether the next move is the dealer's: a link to reveal. */
export const dealers = (p: Phase): boolean => p === Phase.Open || p === Phase.Deal || p === Phase.Hit || p === Phase.Dealer;

/** Whether the player's seed is public, so that the dealer can know the hand's cards. */
export const seeded = (p: Phase): boolean => p !== Phase.Open && p !== Phase.Seed;

/** The house's exposure to a stake (drops): a natural's 3:2, half a drop rounded down. */
export const exposure = (stake: bigint): bigint => stake + stake / 2n;

/** What the player gets back from `stake` drops, the stake included. */
export function payout(stake: bigint, outcome: Outcome): bigint {
  switch (outcome) {
    case Outcome.Blackjack:
    case Outcome.ForfeitDeal:
      return stake + exposure(stake);
    case Outcome.Win:
    case Outcome.Forfeit:
      return 2n * stake;
    case Outcome.Push:
    case Outcome.Refund:
      return stake;
    case Outcome.Lose:
    case Outcome.Bust:
    case Outcome.NoSeed:
      return 0n;
  }
}

/**
 * What a bet commits to: BLAKE3("blockula/blackjack/seed" ‖ player (32 bytes) ‖ client seed (32
 * bytes)), as hex. The player's account is part of it, so a hash copied into another account's
 * bet can't be opened there. `player` is the account's 32 bytes as hex.
 */
export function seedHashOf(playerHex: string, clientSeed: string): string {
  const pre = new Uint8Array(87);
  pre.set(new TextEncoder().encode("blockula/blackjack/seed"), 0);
  pre.set(hexBytes(playerHex, 32), 23);
  pre.set(hexBytes(clientSeed, 32), 55);
  return hex(blake3(pre));
}

/** Final hands compared (the dealer's as far as it was played). */
export function showdown(player: readonly number[], dealer: readonly number[]): Outcome {
  const p = total(player).total;
  const d = total(dealer).total;
  if (p > 21) return Outcome.Bust;
  if (natural(player)) return natural(dealer) ? Outcome.Push : Outcome.Blackjack;
  if (natural(dealer)) return Outcome.Lose;
  if (d > 21 || p > d) return Outcome.Win;
  return p === d ? Outcome.Push : Outcome.Lose;
}

export type Step = { kind: "more" } | { kind: "player" } | { kind: "settled"; outcome: Outcome };

/** One hand: the cards on the table and whose move it is, as `bj_core::Hand`. */
export class Hand {
  /** A new hand's seed is known and the dealer owes the deal; `Hand.committed()` is one whose seed isn't revealed yet. */
  phase: Phase = Phase.Deal;
  /** Cards dealt so far: the chain index of the last. */
  dealt = 0;
  player: number[] = [];
  dealer: number[] = [];

  /** A hand whose bet is placed with the hash of a seed the player hasn't revealed yet. */
  static committed(): Hand {
    const hand = new Hand();
    hand.phase = Phase.Seed;
    return hand;
  }

  /** The player revealed their seed: the dealer owes the deal. False (and nothing changes) unless the hand waited for it. */
  seeded(): boolean {
    if (this.phase !== Phase.Seed) return false;
    this.phase = Phase.Deal;
    return true;
  }

  /** Rules v2: a hand whose bet is placed with the hash of a seed. The dealer owes the opening links. */
  static unopened(): Hand {
    const hand = new Hand();
    hand.phase = Phase.Open;
    return hand;
  }

  /**
   * Rules v2: the dealer revealed the next opening link, which deals no card (the seed isn't
   * known). True once all three are out and the player owes the seed, false while more are owed,
   * null (and nothing changes) unless the hand is being opened.
   */
  openLink(): boolean | null {
    if (this.phase !== Phase.Open) return null;
    this.dealt++;
    if (this.dealt < OPENING_LINKS) return false;
    this.phase = Phase.Seed;
    return true;
  }

  /**
   * Rules v2: the player revealed their seed, and `cards` are the three the opening links deal
   * under it, in chain order. They are on the table at once, by v1's deal. Null (and nothing
   * changes) unless the hand was opened and waited for its seed.
   */
  dealOpening(cards: readonly [number, number, number]): Step | null {
    if (this.phase !== Phase.Seed || this.dealt !== OPENING_LINKS) return null;
    cards.forEach(checkCard);
    this.phase = Phase.Deal;
    this.dealt = 0;
    let step: Step | null = null;
    for (const c of cards) step = this.deal(c);
    return step;
  }

  /** The player hits; false (and nothing changes) unless it's the player's turn. */
  hit(): boolean {
    if (this.phase !== Phase.Player) return false;
    this.phase = Phase.Hit;
    return true;
  }

  /** The player stands; false (and nothing changes) unless it's the player's turn. */
  stand(): boolean {
    if (this.phase !== Phase.Player) return false;
    this.phase = Phase.Dealer;
    return true;
  }

  /** The next card off the chain; null (and nothing changes) when the dealer owes none. */
  deal(c: number): Step | null {
    checkCard(c);
    switch (this.phase) {
      case Phase.Deal:
        if (this.dealt === 1) this.dealer.push(c);
        else this.player.push(c);
        this.dealt++;
        if (this.dealt < 3) return { kind: "more" };
        if (!natural(this.player)) {
          this.phase = Phase.Player;
          return { kind: "player" };
        }
        if (mayBeNatural(this.dealer[0])) {
          this.phase = Phase.Dealer;
          return { kind: "more" };
        }
        return this.settle();
      case Phase.Hit: {
        this.player.push(c);
        this.dealt++;
        const t = total(this.player).total;
        if (t > 21) return this.settle();
        if (t === 21) {
          this.phase = Phase.Dealer;
          return { kind: "more" };
        }
        this.phase = Phase.Player;
        return { kind: "player" };
      }
      case Phase.Dealer:
        this.dealer.push(c);
        this.dealt++;
        return natural(this.player) || total(this.dealer).total >= DEALER_STANDS_ON ? this.settle() : { kind: "more" };
      default:
        return null;
    }
  }

  private settle(): Step {
    this.phase = Phase.Done;
    return { kind: "settled", outcome: showdown(this.player, this.dealer) };
  }
}

/**
 * How a hand ends if the dealer misses its deadline now, under `rules`; null when the dealer
 * owes nothing (the player's seed or move is awaited, or the hand is over).
 *
 * v1 pays the most any hand could: 3:2 until the deal is done or against a natural, else 1:1.
 * v2 refunds a hand that was never opened (the dealer had seen no card), and otherwise pays
 * exactly the most the cards on the table still allow: 3:2 only for a natural in the player's
 * hand, or during a relayed hand's deal while the player's first card is unseen or is an ace or
 * a ten-value card.
 */
export function forfeitOf(hand: Pick<Hand, "phase" | "player">, rules: Rules): Outcome | null {
  switch (hand.phase) {
    case Phase.Open:
      return Outcome.Refund;
    case Phase.Deal:
      if (rules === 1) return Outcome.ForfeitDeal;
      return hand.player.length === 0 || mayBeNatural(hand.player[0]) ? Outcome.ForfeitDeal : Outcome.Forfeit;
    case Phase.Hit:
    case Phase.Dealer:
      return natural(hand.player) ? Outcome.ForfeitDeal : Outcome.Forfeit;
    default:
      return null;
  }
}

/** A hand recomputed from what the chain published. */
export interface Replay {
  /** Every card, in chain order. */
  cards: number[];
  player: number[];
  dealer: number[];
  playerTotal: number;
  dealerTotal: number;
  phase: Phase;
  /** Set once the cards settle the hand. */
  outcome: Outcome | null;
  /** The player's moves the cards imply: "H" before each hit card, "S" before the dealer's play. */
  moves: string;
}

/**
 * Recomputes a hand from its commitment, the player's seed and the links revealed so far, in
 * order: checks every link against the one before it (the first against the commitment), derives
 * every card and replays the rules. Throws if a link isn't the chain's next, or if there are more
 * links than the hand could have used. This is the whole fairness check: with it, the cards are
 * the only ones the dealer's commitment and the player's seed allow.
 */
export function replay(commit: string, clientSeed: string, gameId: bigint, links: readonly string[]): Replay {
  const hand = new Hand();
  const cards: number[] = [];
  let last = commit;
  let outcome: Outcome | null = null;
  let moves = "";
  links.forEach((link, i) => {
    const j = i + 1;
    if (prevLink(link) !== last) throw new Error(`link ${j} doesn't hash to the one before it: these aren't the committed chain's links`);
    last = link;
    if (hand.phase === Phase.Done) throw new Error(`link ${j} comes after the hand was settled`);
    if (hand.phase === Phase.Player) {
      // A card after the player's turn: they hit if it's theirs, stood if it's the dealer's. The
      // links alone can't say which, so a caller that knows the moves checks them (`replayMoves`).
      throw new Error(`link ${j} needs the player's move before it: use replayMoves`);
    }
    const c = card(link, clientSeed, gameId, j);
    cards.push(c);
    const step = hand.deal(c);
    if (step?.kind === "settled") outcome = step.outcome;
  });
  return { cards, player: [...hand.player], dealer: [...hand.dealer], playerTotal: total(hand.player).total, dealerTotal: total(hand.dealer).total, phase: hand.phase, outcome, moves };
}

/**
 * `replay` for a hand whose player moved: `moves` is the player's moves in order ("H" hit, "S"
 * stand), as the chain's events record them. Throws if the links, the moves and the rules don't
 * fit together (a move out of turn, a link nobody owed, moves left over).
 */
export function replayMoves(commit: string, clientSeed: string, gameId: bigint, links: readonly string[], moves: string): Replay {
  if (!/^[HS]*$/.test(moves)) throw new Error('moves are "H" and "S"');
  const hand = new Hand();
  const cards: number[] = [];
  let last = commit;
  let outcome: Outcome | null = null;
  let m = 0;
  const move = () => {
    while (hand.phase === Phase.Player && m < moves.length) {
      if (moves[m] === "H") hand.hit();
      else hand.stand();
      m++;
    }
  };
  links.forEach((link, i) => {
    const j = i + 1;
    if (prevLink(link) !== last) throw new Error(`link ${j} doesn't hash to the one before it: these aren't the committed chain's links`);
    last = link;
    move();
    if (!dealers(hand.phase)) throw new Error(`link ${j} was revealed when the dealer owed no card`);
    const c = card(link, clientSeed, gameId, j);
    cards.push(c);
    const step = hand.deal(c);
    if (step?.kind === "settled") outcome = step.outcome;
  });
  move();
  if (m !== moves.length) throw new Error("more moves than the hand had turns");
  return { cards, player: [...hand.player], dealer: [...hand.dealer], playerTotal: total(hand.player).total, dealerTotal: total(hand.dealer).total, phase: hand.phase, outcome, moves };
}

/**
 * `replay` for a hand whose player's card count is known (the program's records keep it): the
 * player hit until they held `nPlayer` cards, then stood if the hand went on. Throws if the
 * links, that count and the rules don't fit together.
 */
export function replayCounted(commit: string, clientSeed: string, gameId: bigint, links: readonly string[], nPlayer: number): Replay {
  const hand = new Hand();
  const cards: number[] = [];
  let last = commit;
  let outcome: Outcome | null = null;
  let moves = "";
  links.forEach((link, i) => {
    const j = i + 1;
    if (prevLink(link) !== last) throw new Error(`link ${j} doesn't hash to the one before it: these aren't the committed chain's links`);
    last = link;
    if (hand.phase === Phase.Player) {
      if (hand.player.length < nPlayer) {
        hand.hit();
        moves += "H";
      } else {
        hand.stand();
        moves += "S";
      }
    }
    if (!dealers(hand.phase)) throw new Error(`link ${j} was revealed when the dealer owed no card`);
    const c = card(link, clientSeed, gameId, j);
    cards.push(c);
    const step = hand.deal(c);
    if (step?.kind === "settled") outcome = step.outcome;
  });
  if (hand.player.length !== nPlayer && !(hand.phase === Phase.Deal && nPlayer <= 2)) throw new Error(`the links deal the player ${hand.player.length} cards, not ${nPlayer}`);
  return { cards, player: [...hand.player], dealer: [...hand.dealer], playerTotal: total(hand.player).total, dealerTotal: total(hand.dealer).total, phase: hand.phase, outcome, moves };
}

/**
 * Every link of a chain from its newest revealed one: `links[0]` is the commitment's successor
 * `s_1`, the last is `newest` (`s_count`). The program keeps only the newest link of an open
 * hand and publishes only the last one of a settled hand; the rest are its hashes.
 */
export function linksFromNewest(newest: string, count: number): string[] {
  if (!Number.isInteger(count) || count < 0 || count > CHAIN_LEN) throw new Error(`a hand reveals 0 to ${CHAIN_LEN} links`);
  if (count === 0) return [];
  const links = [newest];
  for (let i = 1; i < count; i++) links.push(prevLink(links[links.length - 1]));
  return links.reverse();
}

/** In words: "Blackjack", "You win", … */
export function outcomeWords(o: Outcome): string {
  switch (o) {
    case Outcome.Blackjack:
      return "Blackjack";
    case Outcome.Win:
      return "You win";
    case Outcome.Push:
      return "Push";
    case Outcome.Lose:
      return "Dealer wins";
    case Outcome.Bust:
      return "Bust";
    case Outcome.Forfeit:
      return "You win: the dealer ran out of time";
    case Outcome.ForfeitDeal:
      return "You win: the dealer never dealt";
    case Outcome.NoSeed:
      return "The bet is the house's: its seed was never revealed";
    case Outcome.Refund:
      return "Your stake is back: the dealer never opened the hand";
  }
}
