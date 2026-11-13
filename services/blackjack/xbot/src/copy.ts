/**
 * Every word the bot can reply with. Replies are built here from numbers and cards only: never
 * from a post's text, a handle, or anything a program or a node wrote. Each fits a post on X
 * (280, counted as X counts), and a reply that starts or ends a hand carries the note.
 */
import { ABORTS, Outcome, cardLabel, fang, natural, outcomeWords, total } from "@blockula/blackjack";

/** The note every reply that starts or ends a hand carries. */
export const NOTE = "FANG has no value and can't be bought; no prizes.";

/** The most a post on X holds. */
export const X_LIMIT = 280;
const URL = /https?:\/\/[^\s]+/g;

/** A text's length as X counts it: code points, and 23 for every link. */
export function xLength(text: string): number {
  let n = 0;
  let last = 0;
  for (const m of text.matchAll(URL)) {
    const at = m.index ?? 0;
    n += [...text.slice(last, at)].length + 23;
    last = at + m[0].length;
  }
  return n + [...text.slice(last)].length;
}

/** The first of `variants` that fits a post: they go from the fullest to the shortest. */
function fit(variants: readonly string[]): string {
  for (const v of variants) if (xLength(v) <= X_LIMIT) return v;
  throw new Error("no variant of this reply fits a post");
}

const cards = (c: readonly number[]) => c.map(cardLabel).join(" ");
/** "16", "soft 17", "blackjack". */
function totalOf(c: readonly number[]): string {
  if (natural(c)) return "blackjack";
  const t = total(c);
  return t.soft ? `soft ${t.total}` : `${t.total}`;
}
const yours = (c: readonly number[], short: boolean) => (short ? `You: ${totalOf(c)} with ${c.length} cards.` : `You: ${cards(c)} (${totalOf(c)}).`);
function dealers(c: readonly number[], hidden: boolean, short: boolean): string {
  if (c.length === 0) return "Dealer: no cards yet.";
  if (hidden) return `Dealer: ${cards(short ? c.slice(0, 1) : c)} and one card face down.`;
  return short ? `Dealer: ${totalOf(c)} with ${c.length} cards.` : `Dealer: ${cards(c)} (${totalOf(c)}).`;
}
const lines = (...l: (string | false | null)[]) => l.filter((x): x is string => typeof x === "string" && x !== "").join("\n");

/** What a hand looks like, for a reply. */
export interface HandView {
  gameId: bigint;
  stake: bigint;
  playerCards: readonly number[];
  dealerCards: readonly number[];
}

/** The hand is the player's to move. `opened`: this reply answers the deal. */
export function turnText(h: HandView, opened: boolean): string {
  const head = opened ? `Hand ${h.gameId}. Bet ${fang(h.stake)} FANG.` : `Hand ${h.gameId}.`;
  return fit([false, true].map((short) => lines(head, yours(h.playerCards, short), dealers(h.dealerCards, true, short), "Reply hit or stand.", opened && NOTE)));
}

/** The dealer owes a card and hasn't shown it yet. */
export function waitingText(h: HandView, opened: boolean): string {
  const head = opened ? `Hand ${h.gameId}. Bet ${fang(h.stake)} FANG.` : `Hand ${h.gameId}.`;
  const wait = h.playerCards.length === 0 ? "The dealer hasn't dealt yet. I'll reply here when the cards are out." : "Waiting for the dealer's cards. I'll reply here when they're out.";
  return fit([false, true].map((short) => lines(head, h.playerCards.length > 0 && yours(h.playerCards, short), wait, opened && NOTE)));
}

export interface SettledView extends HandView {
  outcome: Outcome;
  payout: bigint;
  /** The table balance after it. */
  balance: bigint;
  /** The dealer never turned its second card. */
  holeHidden: boolean;
}

/** The hand is over: the outcome, the cards, what came back. */
export function settledText(s: SettledView): string {
  const head = `Hand ${s.gameId}: ${outcomeWords(s.outcome)}.`;
  const bet = `Bet ${fang(s.stake)} FANG.`;
  const back = s.payout > 0n ? `${bet} ${fang(s.payout)} FANG came back to your table balance, now ${fang(s.balance)} FANG.` : `${bet} Nothing came back. Table balance: ${fang(s.balance)} FANG.`;
  const backShort = `${bet} ${s.payout > 0n ? `${fang(s.payout)} FANG` : "Nothing"} came back. Table balance: ${fang(s.balance)} FANG.`;
  const dealt = s.playerCards.length > 0;
  const hand = (short: boolean) => [dealt && yours(s.playerCards, short), dealt && dealers(s.dealerCards, s.holeHidden, short)];
  // From the fullest to the shortest: every card, then counts, then the outcome and the FANG alone.
  return fit([lines(head, ...hand(false), back, NOTE), lines(head, ...hand(true), back, NOTE), lines(head, ...hand(true), backShort, NOTE), lines(head, backShort, NOTE)]);
}

/** A hand that ended and can't be shown card by card (its record didn't check out). */
export function endedText(gameId: bigint, balance: bigint): string {
  return lines(`Hand ${gameId} is over. Table balance: ${fang(balance)} FANG.`, NOTE);
}

export interface BalanceView {
  balance: bigint;
  /** What the player can still bet today from X, and the most a hand; `null` without a live grant. */
  grant: { leftToday: bigint; maxBet: bigint; blocksLeft: bigint } | null;
  /** A hand is open. */
  open: boolean;
}

export function balanceText(b: BalanceView): string {
  return lines(
    `Table balance: ${fang(b.balance)} FANG.`,
    b.open && "You have a hand open.",
    b.grant ? `From X you can still bet ${fang(b.grant.leftToday)} FANG today, up to ${fang(b.grant.maxBet)} FANG a hand.` : "This bot has no grant from your account right now, so it can't play for you.",
    b.grant && `Your grant runs for ${b.grant.blocksLeft} more blocks.`,
  );
}

/** `stop`: what went back to the player's own account. */
export function stopText(sent: bigint | null): string {
  return lines(sent === null ? "Sent your table balance back to your own account." : `Sent your table balance of ${fang(sent)} FANG back to your own account.`, "I won't play for you until you post deal again.", NOTE);
}

/** Why the bot didn't do what a command asked. */
export type Refusal =
  | { reason: "no_link" }
  | { reason: "unverified" }
  | { reason: "conflict" }
  | { reason: "no_grant" }
  | { reason: "expired" }
  | { reason: "paused" }
  | { reason: "hand_open"; gameId: bigint; here: boolean }
  | { reason: "limits"; min: bigint; max: bigint }
  | { reason: "daily"; left: bigint }
  | { reason: "balance"; balance: bigint }
  | { reason: "no_hand" }
  | { reason: "thread" }
  | { reason: "waiting" }
  | { reason: "finish_first" }
  | { reason: "empty" }
  | { reason: "stop_no_grant" }
  | { reason: "not_sent" }
  | { reason: "table"; code: number | undefined };

/** The table's abort codes a player can meet, in the bot's words. Other codes get one plain sentence. */
const TABLE_WORDS: Readonly<Record<number, string>> = {
  46: ABORTS[46],
  47: ABORTS[47],
  48: ABORTS[48],
  49: ABORTS[49],
  50: ABORTS[50],
  51: ABORTS[51],
  52: ABORTS[52],
  53: ABORTS[53],
  54: "That hand is already over.",
  56: ABORTS[56],
  63: "Your table balance doesn't cover that.",
  64: "Your account hasn't granted this bot at the table.",
  65: "Your grant to this bot has expired. Renew it on the table page.",
  66: "That's past what your grant to this bot allows.",
  68: "That hand wasn't bet from your table balance, so I can't play it. Finish it on the table page.",
  69: ABORTS[69],
};

export function refusalText(r: Refusal): string {
  switch (r.reason) {
    case "no_link":
      return "I can't find a Blockula account that linked this X handle and granted this bot. Link X in your wallet, deposit at the table and grant the bot there, then post again.";
    case "unverified":
      return "An account names this X handle, but its proof post didn't check out. Post the proof from this handle and link again in your wallet.";
    case "conflict":
      return "Two accounts have linked this X handle, so I can't tell which one to play for. Unlink one of them in its wallet, then post again.";
    case "no_grant":
      return "Your account hasn't granted this bot at the table, or took the grant back. Grant it on the table page to play from X.";
    case "expired":
      return "Your grant to this bot has expired. Renew it on the table page to play from X.";
    case "paused":
      return "The table is paused for new hands. Hands already open can be finished.";
    case "hand_open":
      return r.here ? `You already have a hand open (hand ${r.gameId}). Reply hit or stand in its thread.` : `You already have a hand open (hand ${r.gameId}). Finish it on the table page first.`;
    case "limits":
      return `A bet from X is ${fang(r.min)} to ${fang(r.max)} FANG for you right now.`;
    case "daily":
      return r.left > 0n ? `That would pass today's limit. You can still bet ${fang(r.left)} FANG from X today.` : "You've reached today's limit for bets from X.";
    case "balance":
      return `Your table balance is ${fang(r.balance)} FANG, less than that bet. Deposit at the table or bet less.`;
    case "no_hand":
      return "You have no hand open here. Post deal and an amount to start one.";
    case "thread":
      return "Reply hit or stand in your hand's own thread, under the deal or my last reply.";
    case "waiting":
      return "The dealer hasn't shown the cards yet. I'll reply in the hand's thread when they're out.";
    case "finish_first":
      return "Finish your open hand first, then post stop.";
    case "empty":
      return lines("Your table balance is already empty, so there's nothing to send back.", "I won't play for you until you post deal again.");
    case "stop_no_grant":
      return lines("I can't move your table balance without a live grant. Withdraw it yourself on the table page.", "I won't play for you until you post deal again.");
    case "not_sent":
      return "That didn't go through, and nothing was played. Post it again.";
    case "table":
      return (r.code !== undefined && TABLE_WORDS[r.code]) || "The table refused that.";
  }
}

/** Words no reply, alt text or player-facing line may hold. */
export const BANNED = ["contract", "demo", "example", "mainnet", "casino", "jackpot", "win money", "cash"] as const;

/** The banned words in `text`, if any. */
export function bannedIn(text: string): string[] {
  const t = text.toLowerCase();
  return BANNED.filter((w) => t.includes(w));
}
