/**
 * The blackjack table page's own logic (app/blackjack): which table, what a bet may be, how long a
 * deadline has left, whose move it is, the seeds this browser made, and the fairness check's
 * steps. The rules, the program's encoding and the check itself are `@blockula/blackjack`'s; this
 * file only arranges them for the page.
 *
 * Pure (the shared library, and lib/format.ts), so `node --test` runs it (lib/blackjack.test.mjs).
 * Amounts are drops (bigint) or FANG as strings, never floats.
 */
import {
  DROPS_PER_FANG,
  Outcome,
  Phase,
  addressBytes,
  cardLabel,
  cardName,
  exposure,
  fang,
  outcomeWords,
  prefixOf,
  total,
  verifyHand,
  type Game,
  type House,
  type LastHand,
  type Limits,
  type PlayerRecord,
  type TableEvent,
} from "@blockula/blackjack";
import { blocksToHuman } from "./format.ts";

// ---------------------------------------------------------------------------------- which table

/** Where the page's table came from: the link (`?table=`) or the build (`NEXT_PUBLIC_BLACKJACK_TABLE`). */
export type TableChoice = { state: "none" } | { state: "bad"; given: string; from: "link" | "build"; why: string } | { state: "ok"; address: string; from: "link" | "build" };

/** Whether `s` is an account address (bech32m, 32 bytes), on the chain whose addresses start with `prefix` when that's known. */
export function isTableAddress(s: string, prefix?: string): boolean {
  if (!/^t?boo1[02-9ac-hj-np-z]{40,}$/.test(s)) return false;
  try {
    addressBytes(s);
    return prefix === undefined || prefixOf(s) === prefix;
  } catch {
    return false;
  }
}

/**
 * The table the page is for: the link's `?table=` when it has one, else the build's. A table is
 * named by its address only, never by a `.boo` name (a name moves with its owner).
 */
export function chooseTable(fromLink: string | null | undefined, fromBuild: string | undefined, prefix?: string): TableChoice {
  const pick = (given: string, from: "link" | "build"): TableChoice => {
    if (isTableAddress(given, prefix)) return { state: "ok", address: given, from };
    const why = isTableAddress(given) ? "That address belongs to another chain." : "A table is named by its address (boo1…), never by a .boo name.";
    return { state: "bad", given: given.slice(0, 120), from, why };
  };
  const link = fromLink?.trim();
  if (link) return pick(link, "link");
  const built = fromBuild?.trim();
  if (built) return pick(built, "build");
  return { state: "none" };
}

/** A hand's id from a link (`?game=`): a u64 in decimal, or null. */
export function gameIdFrom(raw: string | null | undefined): bigint | null {
  if (!raw || !/^\d{1,20}$/.test(raw)) return null;
  const id = BigInt(raw);
  return id < 1n << 64n ? id : null;
}

// ---------------------------------------------------------------------------------- amounts

/** An amount of FANG as typed ("1.5"): drops, or null when it isn't one. Never a float. */
export function parseAmount(raw: string): bigint | null {
  const v = raw.trim();
  if (!/^\d{1,11}(\.\d{1,9})?$/.test(v)) return null;
  const [whole, frac = ""] = v.split(".");
  return BigInt(whole) * DROPS_PER_FANG + BigInt(frac.padEnd(9, "0"));
}

/** "1.5 FANG". */
export const fangText = (drops: bigint): string => `${fang(drops)} FANG`;

// ---------------------------------------------------------------------------------- the bet form

/** What the bet form is checked against: all of it read from the chain. */
export interface BetContext {
  limits: Limits;
  house: House;
  player: PlayerRecord;
  /** The height the records were read at. */
  height: number;
  /** The dealer has a commitment waiting for the next hand. */
  commitmentReady: boolean;
  /** What the account can move now (drops), when known. */
  transferable?: bigint | null;
}

/** What the player may still wager today at this table (drops). A day is `dayBlocks` blocks, counted from block 0. */
export function dailyLeft(limits: Limits, player: PlayerRecord, height: number): bigint {
  const day = BigInt(height) / limits.dayBlocks;
  const wagered = player.day === day ? player.wagered : 0n;
  return wagered >= limits.dailyCap ? 0n : limits.dailyCap - wagered;
}

/** The largest stake whose exposure (1.5 ×, rounded down) is at most `room`. */
function stakeWithin(room: bigint): bigint {
  if (room <= 0n) return 0n;
  let s = (2n * room) / 3n;
  while (exposure(s + 1n) <= room) s += 1n;
  while (s > 0n && exposure(s) > room) s -= 1n;
  return s;
}

/**
 * The largest bet the table takes right now (drops): its maximum, the house's free bankroll
 * divided by the divisor, and what the house may still have at stake over all open hands.
 */
export function maxBetNow(limits: Limits, house: House): bigint {
  const share = house.bankroll / limits.bankrollDivisor;
  const room = limits.maxExposure > house.reserved ? limits.maxExposure - house.reserved : 0n;
  const covered = stakeWithin(house.bankroll < room ? house.bankroll : room);
  return [limits.maxBet, share, covered].reduce((a, b) => (b < a ? b : a));
}

/** Why no bet can be placed right now, whatever its size; null when one can. In the order the program checks. */
export function tableProblem(c: BetContext): string | null {
  if (c.house.paused) return "New bets are paused. Open hands play on.";
  if (maxBetNow(c.limits, c.house) < c.limits.minBet) return "The house's bankroll can't cover a bet right now.";
  if (BigInt(c.house.open) >= c.limits.maxOpenGames) return "The table is full. Try again when a hand finishes.";
  if (c.player.openGame !== null) return "Finish your open hand first.";
  if (dailyLeft(c.limits, c.player, c.height) < c.limits.minBet) return "You've reached today's limit at this table.";
  if (!c.commitmentReady) return "The dealer isn't ready for a new hand yet. Try again in a moment.";
  return null;
}

/** Why this bet can't be placed, or null when it can. `raw` is what's typed, in FANG. */
export function betProblem(raw: string, c: BetContext): string | null {
  const blocked = tableProblem(c);
  if (blocked) return blocked;
  if (!raw.trim()) return "Enter a bet in FANG.";
  const stake = parseAmount(raw);
  if (stake === null) return "A bet is an amount of FANG, like 1.5 (up to 9 decimals).";
  if (stake < c.limits.minBet) return `The smallest bet is ${fangText(c.limits.minBet)}.`;
  const most = maxBetNow(c.limits, c.house);
  if (stake > most) return `The largest bet right now is ${fangText(most)}.`;
  const left = dailyLeft(c.limits, c.player, c.height);
  if (stake > left) return `That passes today's limit. You have ${fangText(left)} left today.`;
  if (c.transferable != null && stake > c.transferable) return `Your account can move ${fangText(c.transferable)} right now.`;
  return null;
}

// ---------------------------------------------------------------------------------- deadlines

export interface Countdown {
  /** Blocks until the deadline's block. */
  left: number;
  /** A `timeout` sent now would land past the deadline. */
  passed: boolean;
  /** "42 blocks (about 42s)". */
  words: string;
}

/**
 * How long a hand's deadline has left at `height`. `timeout` works in any block after `deadline`,
 * and a transaction sent now lands in a later block than `height`, so the deadline has passed
 * once the chain is at it.
 */
export function countdown(deadline: bigint, height: number, blockMs = 1000): Countdown {
  const gap = deadline - BigInt(height);
  const left = gap > 0n ? Number(gap > 1_000_000_000n ? 1_000_000_000n : gap) : 0;
  const passed = left === 0;
  return { left, passed, words: passed ? "no blocks" : `${left.toLocaleString("en-US")} ${left === 1 ? "block" : "blocks"} (about ${blocksToHuman(left, blockMs)})` };
}

/** "150 blocks (about 3 min)". */
export const blocksWords = (blocks: bigint, blockMs = 1000): string => `${blocks.toLocaleString("en-US")} blocks (about ${blocksToHuman(Number(blocks), blockMs)})`;

// ---------------------------------------------------------------------------------- whose move

export interface HandState {
  /** The hand is the viewer's own. */
  mine: boolean;
  /** Who the hand waits for. */
  waitsFor: "dealer" | "player";
  /** The viewer may hit or stand now. */
  canMove: boolean;
  /** The dealer's deadline passed: anyone may claim the hand for the player. */
  canClaim: boolean;
  /** The player's deadline passed: the hand stands by itself, and anyone may trigger that. */
  idle: boolean;
  countdown: Countdown;
  /** One line saying where the hand stands. */
  words: string;
}

/** Where an open hand stands at `height`, for the account `me` (null: a spectator). */
export function handState(game: Game, me: string | null, height: number, blockMs = 1000): HandState {
  const mine = me !== null && game.player === me;
  const waitsFor = game.phase === Phase.Player ? "player" : "dealer";
  const cd = countdown(game.deadline, height, blockMs);
  const canClaim = waitsFor === "dealer" && cd.passed;
  const idle = waitsFor === "player" && cd.passed;
  const canMove = mine && waitsFor === "player";
  let words: string;
  if (waitsFor === "dealer") {
    if (canClaim) words = "The dealer ran out of time. The hand can be claimed for the player.";
    else words = `Waiting for the dealer… It has ${cd.words} left to reveal ${game.phase === Phase.Deal ? "the deal" : "the next card"}.`;
  } else if (idle) words = `${mine ? "Your" : "The player's"} time to move ran out. The hand stands by itself: anyone can trigger that, and the dealer then finishes it.${mine ? " Until someone does, you can still move." : ""}`;
  else words = `${mine ? "Your move: hit or stand. You have" : "The player's move. They have"} ${cd.words} left. After that the hand stands by itself.`;
  return { mine, waitsFor, canMove, canClaim, idle, countdown: cd, words };
}

/** What the table shows for a player, from their record and their open hand's. */
export type Seat =
  | { state: "open"; game: Game; last: LastHand | null }
  /** The record names an open hand the node no longer has: it settled between the two reads. Read again. */
  | { state: "settling"; last: LastHand | null }
  | { state: "idle"; last: LastHand | null };

/** A player's seat, from their record and the open hand read after it (null: no such record). */
export function seatOf(player: PlayerRecord, game: Game | null): Seat {
  if (player.openGame === null) return { state: "idle", last: player.last };
  if (!game || game.id !== player.openGame) return { state: "settling", last: player.last };
  return { state: "open", game, last: player.last };
}

// ---------------------------------------------------------------------------------- seeds

/** What this browser keeps about a hand it bet: the seed it made, and what the bet's receipt said. */
export interface StoredHand {
  /** The client seed: 32 random bytes, hex. */
  seed: string;
  /** The hand's id, once the bet's receipt gave it. */
  game?: string;
  /** The commitment the bet took (its `bet` event). */
  commit?: string;
  /** The bet's transaction. */
  betTx?: string;
  /** Every transaction of the hand this page sent, the bet first. */
  txs: string[];
  /** When the seed was made (ms). */
  at: number;
}

export interface HandStore {
  v: 1;
  /** Seeds made for a bet whose hand isn't known yet, newest last. */
  pending: StoredHand[];
  /** Hands by id (decimal). */
  hands: Record<string, StoredHand>;
}

/** What `localStorage` is to this file. */
export type KeyValue = Pick<Storage, "getItem" | "setItem">;

const MAX_PENDING = 20;
const MAX_HANDS = 100;
const isHash = (s: unknown): s is string => typeof s === "string" && /^[0-9a-f]{64}$/.test(s);

/** The storage key for a table's hands on a chain. */
export const storeKey = (chainId: string, table: string): string => `blockula.blackjack.${chainId}.${table}`;
/** The storage key of the 18+ answer. */
export const ADULT_KEY = "blockula.blackjack.adult";

function cleanHand(h: unknown): StoredHand | null {
  if (!h || typeof h !== "object") return null;
  const o = h as Record<string, unknown>;
  if (!isHash(o.seed)) return null;
  return {
    seed: o.seed,
    ...(typeof o.game === "string" && /^\d{1,20}$/.test(o.game) ? { game: o.game } : {}),
    ...(isHash(o.commit) ? { commit: o.commit } : {}),
    ...(isHash(o.betTx) ? { betTx: o.betTx } : {}),
    txs: Array.isArray(o.txs) ? o.txs.filter(isHash).slice(0, 64) : [],
    at: typeof o.at === "number" && Number.isFinite(o.at) ? o.at : 0,
  };
}

/** The stored hands, read defensively: anything that isn't the shape is dropped. */
export function readStore(storage: KeyValue | null, key: string): HandStore {
  const empty: HandStore = { v: 1, pending: [], hands: {} };
  if (!storage) return empty;
  try {
    const raw = JSON.parse(storage.getItem(key) ?? "null") as { pending?: unknown; hands?: unknown } | null;
    if (!raw || typeof raw !== "object") return empty;
    const pending = (Array.isArray(raw.pending) ? raw.pending : []).map(cleanHand).filter((h): h is StoredHand => h !== null);
    const hands: Record<string, StoredHand> = {};
    for (const [id, h] of Object.entries(raw.hands && typeof raw.hands === "object" ? (raw.hands as Record<string, unknown>) : {})) {
      const c = cleanHand(h);
      if (c && /^\d{1,20}$/.test(id)) hands[id] = { ...c, game: id };
    }
    return { v: 1, pending, hands };
  } catch {
    return empty;
  }
}

function writeStore(storage: KeyValue | null, key: string, s: HandStore): void {
  if (!storage) return;
  const ids = Object.keys(s.hands).sort((a, b) => (BigInt(a) < BigInt(b) ? -1 : 1));
  const hands = Object.fromEntries(ids.slice(-MAX_HANDS).map((id) => [id, s.hands[id]]));
  try {
    storage.setItem(key, JSON.stringify({ v: 1, pending: s.pending.slice(-MAX_PENDING), hands }));
  } catch {
    /* storage full or unavailable: the hand still plays, the commitment just isn't kept */
  }
}

/** 32 fresh random bytes as hex: a bet's client seed. `random` is `crypto.getRandomValues`. */
export function newSeed(random: (bytes: Uint8Array) => Uint8Array = (b) => crypto.getRandomValues(b)): string {
  const bytes = random(new Uint8Array(32));
  if (bytes.length !== 32) throw new Error("a seed is 32 bytes");
  return Array.from(bytes, (x) => x.toString(16).padStart(2, "0")).join("");
}

/** Whether this browser already made `seed` for this table: a seed is never used twice. */
export function seedUsed(storage: KeyValue | null, key: string, seed: string): boolean {
  const s = readStore(storage, key);
  return s.pending.some((h) => h.seed === seed) || Object.values(s.hands).some((h) => h.seed === seed);
}

/** Keeps a seed made for a bet that's about to be reviewed. Throws if this browser used it before. */
export function rememberSeed(storage: KeyValue | null, key: string, seed: string, now: number): void {
  if (!isHash(seed)) throw new Error("a seed is 32 bytes of hex");
  if (seedUsed(storage, key, seed)) throw new Error("This seed was used before. A new one is made for every bet.");
  const s = readStore(storage, key);
  s.pending.push({ seed, txs: [], at: now });
  writeStore(storage, key, s);
}

/** Notes the bet's transaction beside its seed, as soon as it's signed (before the node answers). */
export function noteBetTx(storage: KeyValue | null, key: string, seed: string, txId: string): void {
  const s = readStore(storage, key);
  const p = s.pending.find((h) => h.seed === seed);
  if (!p || !isHash(txId)) return;
  p.betTx = txId;
  p.txs = [txId];
  writeStore(storage, key, s);
}

/** Files a seed under its hand, with the commitment the bet took, once the bet's receipt is read. */
export function bindBet(storage: KeyValue | null, key: string, seed: string, bet: { game: bigint; commit?: string; txId?: string }): void {
  const s = readStore(storage, key);
  const i = s.pending.findIndex((h) => h.seed === seed);
  const id = bet.game.toString();
  const from = i >= 0 ? s.pending.splice(i, 1)[0] : (s.hands[id]?.seed === seed ? s.hands[id] : { seed, txs: [], at: 0 });
  const betTx = bet.txId ?? from.betTx;
  s.hands[id] = { ...from, seed, game: id, ...(isHash(bet.commit) ? { commit: bet.commit } : {}), ...(betTx ? { betTx } : {}), txs: betTx ? [betTx, ...from.txs.filter((t) => t !== betTx && t !== from.betTx)] : from.txs };
  writeStore(storage, key, s);
}

/** Adds a transaction this page sent (a hit, a stand, a claim) to its hand's list. */
export function addHandTx(storage: KeyValue | null, key: string, game: bigint, txId: string): void {
  const s = readStore(storage, key);
  const h = s.hands[game.toString()];
  if (!h || !isHash(txId) || h.txs.includes(txId)) return;
  h.txs.push(txId);
  writeStore(storage, key, s);
}

/**
 * What this browser kept for hand `game`, only if its seed is the one the chain recorded for that
 * hand (`clientSeed`): a hand with the same id on another chain, or one bet elsewhere, isn't it.
 * A seed still waiting for its hand counts too (the bet's receipt wasn't read: `commit` is unset).
 */
export function storedHand(storage: KeyValue | null, key: string, game: bigint, clientSeed: string): StoredHand | null {
  const s = readStore(storage, key);
  const h = s.hands[game.toString()];
  if (h && h.seed === clientSeed) return h;
  const p = s.pending.find((x) => x.seed === clientSeed);
  return p ? { ...p, game: game.toString() } : null;
}

// ---------------------------------------------------------------------------------- the check

/** One card of a checked hand: the link that deals it, and whose it is. */
export interface CheckStep {
  /** The card's index in the chain, from 1. */
  index: number;
  link: string;
  card: number;
  /** "A♠". */
  label: string;
  /** "ace of spades". */
  name: string;
  to: "player" | "dealer";
}

export interface HandCheck {
  ok: boolean;
  /** What doesn't fit, when something doesn't. */
  problem: string | null;
  game: bigint;
  clientSeed: string;
  /** Whether the seed is the one this browser made for the hand. */
  seed: "yours" | "unknown";
  /** The commitment the links hash down to. */
  commit: string | null;
  /** Whether that's the commitment the bet took: `unknown` when this browser doesn't hold it. */
  commitCheck: "matches" | "unknown";
  steps: CheckStep[];
  player: number[];
  dealer: number[];
  playerTotal: number;
  dealerTotal: number;
  /** Once the hand is settled. */
  outcome: Outcome | null;
  stake: bigint;
  payout: bigint | null;
  /** The dealer missed a deadline: the timeout ended the hand, not the cards. */
  forfeited: boolean;
  /** One line: verified, or not and why. */
  verdict: string;
}

const same = (a: readonly number[], b: readonly number[]) => a.length === b.length && a.every((x, i) => x === b[i]);

/** Whose each card is, in chain order: the deal is player, dealer, player; then the player's hits; then the dealer's. */
function stepsOf(links: readonly string[], cards: readonly number[], nPlayer: number): CheckStep[] {
  let p = 0;
  return cards.map((card, i) => {
    const to: CheckStep["to"] = i === 1 ? "dealer" : p < nPlayer ? "player" : "dealer";
    if (to === "player") p++;
    return { index: i + 1, link: links[i], card, label: cardLabel(card), name: cardName(card), to };
  });
}

interface CheckInput {
  game: bigint;
  clientSeed: string;
  link: string;
  dealt: number;
  nPlayer: number;
  stake: bigint;
  outcome?: Outcome;
  payout?: bigint;
  /** The cards the record shows, for an open hand: they must be the ones the links give. */
  shown?: { player: readonly number[]; dealer: readonly number[] };
}

function check(input: CheckInput, stored: StoredHand | null | undefined): HandCheck {
  const kept = stored && stored.seed === input.clientSeed ? stored : null;
  const base = { game: input.game, clientSeed: input.clientSeed, seed: kept ? ("yours" as const) : ("unknown" as const), stake: input.stake, outcome: input.outcome ?? null, payout: input.payout ?? null };
  try {
    const v = verifyHand({ game: input.game, clientSeed: input.clientSeed, link: input.link, dealt: input.dealt, nPlayer: input.nPlayer, commit: kept?.commit, outcome: input.outcome, stake: input.outcome !== undefined ? input.stake : undefined, payout: input.payout });
    if (input.shown && !(same(v.player, input.shown.player) && same(v.dealer, input.shown.dealer))) throw new Error("the cards on the table aren't the ones the links give");
    const commitCheck = kept?.commit ? "matches" : "unknown";
    const verdict =
      commitCheck === "matches"
        ? `Verified. Every card comes from the dealer's commitment and ${kept ? "your" : "the player's"} seed.`
        : "The cards fit the links and the seed. This browser doesn't hold the commitment from the bet, so that last step isn't checked here.";
    return { ...base, ok: true, problem: null, commit: v.commit, commitCheck, steps: stepsOf(v.links, v.cards, v.player.length), player: v.player, dealer: v.dealer, playerTotal: v.playerTotal, dealerTotal: v.dealerTotal, forfeited: v.forfeited, verdict };
  } catch (e) {
    const problem = e instanceof Error ? e.message : String(e);
    return { ...base, ok: false, problem, commit: null, commitCheck: "unknown", steps: [], player: [], dealer: [], playerTotal: 0, dealerTotal: 0, forfeited: false, verdict: `Not verified: ${problem}.` };
  }
}

/** The check of an open hand, from its record: every card so far recomputed and held to the cards the record shows. */
export function checkOpenHand(game: Game, stored?: StoredHand | null): HandCheck {
  return check({ game: game.id, clientSeed: game.clientSeed, link: game.link, dealt: game.dealt, nPlayer: game.playerCards.length, stake: game.stake, shown: { player: game.playerCards, dealer: game.dealerCards } }, stored);
}

/** The check of a settled hand, from the player's record of it: the cards, the outcome they give and what it pays. */
export function checkSettledHand(last: LastHand, stored?: StoredHand | null): HandCheck {
  return check({ game: last.game, clientSeed: last.clientSeed, link: last.link, dealt: last.dealt, nPlayer: last.nPlayer, stake: last.stake, outcome: last.outcome, payout: last.payout }, stored);
}

/** "A♠ 10♥ (blackjack)" style: a hand's cards as text. */
export function cardsText(cards: readonly number[]): string {
  if (!cards.length) return "no cards yet";
  return cards.map(cardLabel).join(" ");
}

/** A hand's total as a number with "soft" when an ace counts 11. */
export function totalText(cards: readonly number[]): string {
  if (!cards.length) return "–";
  const t = total(cards);
  return t.soft ? `soft ${t.total}` : `${t.total}`;
}

/** What a settled hand returned, in words. */
export function payoutWords(stake: bigint, paid: bigint): string {
  if (paid === 0n) return `Nothing came back from the ${fangText(stake)} bet.`;
  if (paid === stake) return `The ${fangText(stake)} bet came back.`;
  return `${fangText(paid)} came back from a ${fangText(stake)} bet.`;
}

// ---------------------------------------------------------------------------------- the log

/** One of the table's events as a line of the hand's log, from the side of `me`; null for events that aren't a hand's. */
export function logLine(e: TableEvent, me: string | null): string | null {
  const who = (player: string) => (player === me ? "You" : "The player");
  switch (e.name) {
    case "bet":
      return `${who(e.player)} bet ${fangText(e.stake)}. Hand ${e.game}.`;
    case "hit":
      return `${who(e.player)} hit.`;
    case "stand":
      return e.idle ? `Time ran out, so the hand stood.` : `${who(e.player)} stood.`;
    case "cards":
      return `The dealer revealed ${e.cards.length === 1 ? "a card" : `${e.cards.length} cards`}: ${e.cards.map((c) => cardLabel(c.card)).join(" ")}.`;
    case "settled":
      return `${outcomeWords(e.outcome)}. ${payoutWords(e.stake, e.payout)}`;
    case "deposit":
      return `${who(e.player)} added ${fangText(e.amount)} to the table balance. It holds ${fangText(e.balance)}.`;
    case "withdraw":
      return `${fangText(e.amount)} went back from the table balance. It holds ${fangText(e.balance)}.`;
    case "grant":
      return `${who(e.player)} let a relayer play the table balance until block ${e.expiresAt}.`;
    case "revoke":
      return `${who(e.player)} ended the relayer's grant.`;
    default:
      return null;
  }
}

// ---------------------------------------------------------------------------------- a relayer's grant

/** Why a grant can't be made, or null: the program's own rule (`set_relayer`), from the table's limits. */
export function grantProblem(g: { relayer: string; maxBet: string; dailyCap: string; blocks: string }, limits: Limits, prefix?: string): string | null {
  if (!isTableAddress(g.relayer.trim(), prefix)) return "The relayer is an address (boo1…), never a .boo name.";
  const maxBet = parseAmount(g.maxBet);
  if (maxBet === null || maxBet < 1n) return "The most per bet is an amount of FANG, like 5.";
  if (maxBet > limits.relayMaxBet) return `The most a relayed bet may be at this table is ${fangText(limits.relayMaxBet)}.`;
  const cap = parseAmount(g.dailyCap);
  if (cap === null) return "The most per day is an amount of FANG, like 20.";
  if (cap < maxBet) return "The most per day can't be less than the most per bet.";
  if (!/^\d{1,9}$/.test(g.blocks.trim()) || BigInt(g.blocks.trim()) < 1n) return "How long is a number of blocks.";
  if (BigInt(g.blocks.trim()) > limits.grantMaxBlocks) return `A grant lasts at most ${limits.grantMaxBlocks.toLocaleString("en-US")} blocks at this table.`;
  return null;
}
