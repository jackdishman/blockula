/**
 * The leaderboards (app/leaderboard): three small boards computed in the browser from what the node
 * already serves. Who signed the guestbook most, how the trick or treat bot's players are doing,
 * and the Soul board. Pure and dependency-free (only ledger.ts and inert.ts, which import types
 * only), so `node --test` runs it directly. Amounts are BigInt drops, never floats.
 */
import { inertText } from "./inert.ts";
import { DROPS_PER_FANG } from "./ledger.ts";
import type { Signature } from "./guestbook.ts";
import type { Label, TxDetail } from "./types";

/** The name the trick or treat board reads, without `.boo`. */
export const TRICKORTREAT_NAME = "trickortreat";

/** The smallest payment that counts as a play. */
export const PLAY_MIN = DROPS_PER_FANG;

/** How the boards read a history: pages of this many transactions, at most this many pages. */
export const HISTORY_PAGE = 100;
export const HISTORY_PAGES = 5;

const drops = (s: string | null | undefined) => (s && /^\d+$/.test(s) ? BigInt(s) : 0n);

/** Whether a tx is a finalized transfer that ran (failed, pending and other kinds are not). */
const ran = (t: TxDetail) => (t.kind ?? t.action?.type) === "transfer" && t.status === "finalized" && !t.error && t.height != null;

export interface Ranked {
  who: Label;
}

export interface SignerRow extends Ranked {
  signatures: number;
}

/** Senders ranked by how many signatures they left (most first; ties keep the newest signer first). */
export function topSigners(sigs: Signature[]): SignerRow[] {
  const by = new Map<string, SignerRow>();
  for (const s of sigs) {
    const row = by.get(s.from.address);
    if (row) row.signatures++;
    else by.set(s.from.address, { who: s.from, signatures: 1 });
  }
  return [...by.values()].sort((a, b) => b.signatures - a.signatures);
}

export interface PlayerRow extends Ranked {
  plays: number;
  /** Drops paid in. */
  staked: bigint;
  /** Drops the bot paid out to them. */
  won: bigint;
  /** won - staked. */
  net: bigint;
}

/**
 * The players of a bot account (its `/api/account/{id}/txs`, newest first). A play is a finalized
 * transfer of at least PLAY_MIN into `bot` from someone else; winnings are finalized transfers
 * out of `bot` to someone else. The bot's own transfers to itself, failed and pending txs and
 * anything else are ignored, and a tx on two overlapping pages counts once. Ranked by net, then
 * plays, then first seen (newest). Memos are never read.
 */
export function playersOf(txs: TxDetail[], bot: string): PlayerRow[] {
  const seen = new Set<string>();
  const by = new Map<string, PlayerRow>();
  const row = (who: Label) => {
    let r = by.get(who.address);
    if (!r) by.set(who.address, (r = { who, plays: 0, staked: 0n, won: 0n, net: 0n }));
    return r;
  };
  for (const t of txs) {
    if (!ran(t) || !t.from || !t.to || seen.has(t.id)) continue;
    const amount = drops(t.amount);
    if (t.to.address === bot && t.from.address !== bot) {
      if (amount < PLAY_MIN) continue;
      seen.add(t.id);
      const r = row(t.from);
      r.plays++;
      r.staked += amount;
    } else if (t.from.address === bot && t.to.address !== bot) {
      seen.add(t.id);
      row(t.to).won += amount;
    }
  }
  const rows = [...by.values()];
  for (const r of rows) r.net = r.won - r.staked;
  return rows.sort((a, b) => (a.net === b.net ? b.plays - a.plays : a.net > b.net ? -1 : 1));
}

/** How a board row names an account: its `.boo` name (a name has no spaces; system labels do) or its short address, and where it links. */
export function refOf(who: Label): { ref: string; text: string; named: boolean } {
  const named = !!who.name && !/\s/.test(who.name);
  return { ref: named ? (who.name as string) : who.address, text: named ? inertText(who.name, 48) : who.short, named };
}
