/**
 * Money rules the wallet's Send, Stake and Governance pages share, read from protocol v1's own:
 * when a vote's lock ends (node/src/governance.rs `vote`), what a lock is in plain words, whether
 * "Release expired locks" has anything to release (node/src/econ.rs `release_expired`), and when
 * paying a .boo name by name could reach someone the payer didn't mean (CRE-5, USER-6). Pure, so
 * `node --test` runs it (lib/money.test.mjs).
 */
import { CONVICTION_PERIODS } from "@blockula/sdk";
import { aboutWhen } from "./format.ts";

/**
 * The block a vote's lock ends at: it holds through every block a vote can still land in
 * (`voting_end`), then the conviction's lock periods. So even conviction 0 holds the amount until
 * voting ends.
 */
export function voteLockEnd(votingEndsAt: number, conviction: number, lockPeriodBlocks: number): number {
  return votingEndsAt + 1 + (CONVICTION_PERIODS[conviction] ?? 0) * lockPeriodBlocks;
}

/** One lock as `/api/account` serves it (`locks[]`): it holds `amount` while the height is below `until`. */
export interface LockView {
  id: string;
  reason: string;
  until: number;
  vesting: boolean;
}

/** The proposal a vote lock is for: v1 nodes say `vote:<id>` (its id and reason). */
const voteOf = (l: Pick<LockView, "id" | "reason">) => /^vote:(\d+)$/.exec(l.id)?.[1] ?? /^vote:(\d+)$/.exec(l.reason)?.[1] ?? /^vote on proposal #(\d+)$/.exec(l.reason)?.[1];

/**
 * A lock in plain words, to follow "is locked": "by your vote on proposal #2 until about 21:15",
 * not "vote:2 until block #952". `head` is the chain's height now; the lock is gone from block
 * `until` on.
 */
export function lockWords(l: LockView, head: number, blockMs = 1000, now = Date.now()): string {
  const when = aboutWhen(l.until - head, blockMs, now);
  const vote = voteOf(l);
  if (vote !== undefined) return `by your vote on proposal #${vote} until ${when}`;
  // Strangers can't write these (genesis allocations and the chain's rewards), but they're text all the same.
  const why = l.reason && l.reason !== l.id ? ` (${l.reason})` : "";
  if (l.vesting) return `while it vests${why}, until ${when}`;
  return `by ${l.reason || l.id} until ${when}`;
}

/** Sealed transactions `/api/account` lists, newest first (node/src/api.rs `account`). */
export const ACCOUNT_HISTORY = 25;

/** Most locks an account holds at once (node/src/chain.rs `MAX_LOCKS`); `Unlock` clears ended ones. */
export const MAX_LOCKS = 64;

/** What `voteLocks` reads of a transaction (`/api/account` `recent_txs[]`). */
export interface HistoryTx {
  kind?: string;
  status?: string;
  height: number | null;
  from?: { address: string } | null;
  action?: { type?: string; proposal_id?: unknown; conviction?: unknown } | null;
}

/**
 * Which of this account's vote locks have ended and not been released yet, from its votes in its
 * recent history and the proposals' voting ends. `known` is false when the wallet can't be sure
 * (the account not read yet, history cut short, a proposal or the lock period it can't see): keep
 * "Release expired locks" usable then, and let the chain answer. `nextEnd` is the soonest lock
 * still holding.
 */
export function voteLocks(i: {
  /** The account's recent transactions (`/api/account` `recent_txs`); null when it hasn't been read. */
  recent: HistoryTx[] | null;
  me: string;
  proposals: { id: number; voting_ends_at: number }[];
  lockPeriodBlocks?: number;
  head: number;
  /** Vote locks the node says still hold (`/api/account` `locks`). */
  active?: LockView[];
  /** The node lists only its newest sealed transactions: true when the list may be cut short. */
  historyFull?: boolean;
}): { ended: number[]; nextEnd: number | null; known: boolean } {
  const sealed = (i.recent ?? []).filter((t) => t.height != null && t.status !== "failed" && t.from?.address === i.me);
  const typeOf = (t: HistoryTx) => t.action?.type ?? t.kind;
  const lastUnlock = Math.max(-1, ...sealed.filter((t) => typeOf(t) === "unlock").map((t) => t.height as number));
  let known = i.recent !== null && !i.historyFull && i.lockPeriodBlocks !== undefined;
  // A re-vote only ever extends its lock: the latest end per proposal.
  const ends = new Map<number, number>();
  for (const t of sealed.filter((x) => typeOf(x) === "vote")) {
    const id = Number(t.action?.proposal_id);
    const p = i.proposals.find((x) => x.id === id);
    const conviction = Number(t.action?.conviction);
    if (!p || i.lockPeriodBlocks === undefined || !Number.isInteger(conviction)) {
      known = false;
      continue;
    }
    ends.set(id, Math.max(ends.get(id) ?? 0, voteLockEnd(p.voting_ends_at, conviction, i.lockPeriodBlocks)));
  }
  const holding = new Set((i.active ?? []).map((l) => voteOf(l)).filter((x) => x !== undefined).map(Number));
  // An unlock at height h releases every lock that ends by h; the next one runs at head + 1.
  const ended = [...ends].filter(([id, end]) => end <= i.head + 1 && end > lastUnlock && !holding.has(id)).map(([id]) => id);
  const future = [...[...ends.values()].filter((e) => e > i.head + 1), ...(i.active ?? []).filter((l) => voteOf(l) !== undefined).map((l) => l.until)];
  return { ended, nextEnd: future.length ? Math.min(...future) : null, known };
}

/** What `/api/name` says about a name (node/src/api.rs `name`). */
export interface NameInfo {
  status?: string;
  registered_at?: number;
  expires_at?: number;
  grace_until?: number;
  /** A registration's (and a renewal year's) length, and the grace after it. */
  registration_blocks?: number;
  grace_blocks?: number;
}

/**
 * Whether a name may have had another holder before its current one, lately: it was registered or
 * accepted in the last `RECENT_NAME_DAYS`, and either accepting it re-stamped `registered_at` (so
 * its expiry is off the yearly grid that registering and renewing keep: node/src/econ.rs), or the
 * chain is old enough for a name to have lapsed and been registered again.
 */
export function changedHandsLately(n: NameInfo, head: number, blockMs = 1000): boolean {
  if (n.registered_at === undefined || n.registered_at <= 0) return false;
  if (head - n.registered_at >= Math.round((RECENT_NAME_DAYS * 86_400_000) / blockMs)) return false;
  const year = n.registration_blocks;
  if (!year || n.expires_at === undefined) return true;
  const accepted = (n.expires_at - n.registered_at) % year !== 0;
  const relapsed = n.registered_at > year + (n.grace_blocks ?? 0);
  return accepted || relapsed;
}

/** "Recently": a name registered or accepted within this many days may not be who the payer means. */
export const RECENT_NAME_DAYS = 30;

/**
 * Warnings for paying `name` (CRE-5, USER-6), shown only when they apply: it's expired or expires
 * before the payment stops being valid (when it's signed as the name, the chain pays whoever
 * holds the name when it runs), or it was registered or changed hands lately (an old link, QR
 * code or contact may mean someone else).
 */
export function nameRisks(n: NameInfo, o: { name: string; head: number; validUntil: number; signedAsName: boolean; blockMs?: number; now?: number }): string[] {
  const out: string[] = [];
  const blockMs = o.blockMs ?? 1000;
  if (o.signedAsName && n.status === "grace") out.push(`${o.name} has expired. Until its owner renews it, payments to it fail, and after its grace period anyone can register it. Pay the owner's address instead.`);
  else if (o.signedAsName && n.expires_at !== undefined && n.expires_at < o.validUntil) {
    // An expired name doesn't resolve (node/src/chain.rs `resolve`), so a payment that runs after
    // it fails: nodes drop it unrun, but one a block includes is charged its fee. Only once the
    // grace period is over can someone else register it and be paid.
    const when = aboutWhen(n.expires_at - o.head, blockMs, o.now);
    const after =
      n.grace_until !== undefined && n.grace_until < o.validUntil
        ? "it fails, or once the name's grace period ends it can pay whoever registers the name next"
        : "it fails unless the owner renews the name first, and its fee may still be charged";
    out.push(`${o.name} expires ${when}, before this payment stops being valid. If it runs after that, ${after}. Pay their address instead.`);
  }
  if (changedHandsLately(n, o.head, blockMs))
    out.push(`${o.name} was registered or changed hands in the last ${RECENT_NAME_DAYS} days. If you meant someone who had it before, ask them for their address.`);
  return out;
}
