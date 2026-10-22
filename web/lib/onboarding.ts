/**
 * Onboarding and Home rules, kept pure so lib/onboarding.test.mjs runs them under plain Node:
 *
 * - where a payment link opened without a wallet goes once onboarding is done (`safeNext`), and
 *   what /welcome may say about it (`payRequest`);
 * - what Home says a balance is made of: what can move now, what's staked, what's locked
 *   (`breakdown`);
 * - which names the account owns that need renewing (`nameAlert`);
 * - whether the name step can sell this wallet a name yet (`cheapestName`, `canAffordName`).
 *
 * The words these screens show are in lib/onboarding-copy.ts.
 */
import { fangDecimal, requestDrops } from "./paylink.ts";
import type { NameDetail } from "./explorer/types";

/** A base that can't be this site: a path that resolves to another origin against it isn't one. */
const BASE = "https://wallet.invalid";
/** Longer than any payment link the wallet makes (lib/paylink.ts keeps them QR-sized). */
const NEXT_MAX = 2048;

/**
 * The page a payment link asked for, if onboarding may go there afterwards: only this site's own
 * Send with a query (`/send?…`). Anything else (another path, `//host`, a backslash, an absolute
 * URL, control characters, something that resolves off this origin) is null, and onboarding
 * ends at Home as it always has.
 */
export function safeNext(raw: string | null | undefined): string | null {
  if (!raw || raw.length > NEXT_MAX || !raw.startsWith("/send?")) return null;
  if (raw.includes("//") || raw.includes("\\") || /[\u0000- \u007f]/.test(raw)) return null;
  let u: URL;
  try {
    u = new URL(raw, BASE);
  } catch {
    return null;
  }
  if (u.origin !== BASE || u.pathname !== "/send" || !u.search) return null;
  return `${u.pathname}${u.search}`;
}

/** Where RequireWallet sends a visitor with no wallet: /welcome, carrying a payment link along. */
export function welcomeHref(pathname: string, search: string): string {
  const next = safeNext(`${pathname}${search}`);
  return next ? `/welcome?${new URLSearchParams({ next })}` : "/welcome";
}

/** A name as the chain allows it (3-32 of a-z, 0-9 and dashes, no dash at either end), with or without `.boo`. */
const NAME = /^[a-z0-9][a-z0-9-]{1,30}[a-z0-9](\.boo)?$/;
/** A bech32m address: a short prefix, `1`, and the bech32 alphabet. */
const ADDRESS = /^[a-z]{1,10}1[02-9ac-hj-np-z]{38,90}$/;

/**
 * What /welcome says about a payment link: who it pays and how much. It's the sender's text, so
 * only a well-formed name or address and a valid amount are shown (null otherwise), never the memo.
 * Send checks the link again (its chain, the recipient) before anything is signed.
 */
export interface PayRequest {
  /** `vlad.boo` or a `boo1…` address. */
  to: string | null;
  /** FANG, as the shortest decimal ("1.5"). */
  amount: string | null;
}

export function payRequest(next: string | null): PayRequest | null {
  if (!next) return null;
  const q = new URL(next, BASE).searchParams;
  const to = (q.get("to") ?? "").trim().toLowerCase();
  const drops = requestDrops(q.get("amount") ?? "");
  return {
    to: NAME.test(to) ? (to.endsWith(".boo") ? to : `${to}.boo`) : ADDRESS.test(to) ? to : null,
    amount: drops === null ? null : fangDecimal(drops),
  };
}

/** `/api/account`'s balance and its protocol v1 parts (lib/explorer/types `AccountDetail`). */
export interface BalanceParts {
  balance: string;
  transferable?: string;
  locked?: string;
  locks?: { amount: string; until: number }[];
}

/** `/api/staking`'s totals, in drops. */
export interface StakeTotals {
  bonded?: string;
  unbonding?: string;
}

export interface Breakdown {
  /** What can leave the account now (`transferable`). */
  canSend: bigint;
  /** Delegated, earning or slashable, and the part on its way back. */
  staked: bigint;
  unstaking: bigint;
  /** Held by vesting or vote locks, and the height the last of them lifts. */
  locked: bigint;
  lockedUntil: number | null;
  /** Worth a line under the balance: anything staked or locked, or a balance that can't all move. */
  show: boolean;
}

export function breakdown(a: BalanceParts, s: StakeTotals | null): Breakdown {
  const balance = BigInt(a.balance);
  const canSend = a.transferable !== undefined ? BigInt(a.transferable) : balance;
  const staked = BigInt(s?.bonded ?? "0");
  const unstaking = BigInt(s?.unbonding ?? "0");
  const locked = a.locked !== undefined ? BigInt(a.locked) : 0n;
  const holding = (a.locks ?? []).filter((l) => BigInt(l.amount) > 0n);
  const lockedUntil = holding.length ? Math.max(...holding.map((l) => l.until)) : null;
  return { canSend, staked, unstaking, locked, lockedUntil, show: staked > 0n || unstaking > 0n || locked > 0n || canSend !== balance };
}

/** A name of this account's that needs renewing: in its grace period, or expiring within the window. */
export interface NameAlert {
  name: string;
  state: "grace" | "expiring";
  /** Blocks until it expires (`expiring`), or until anyone may register it (`grace`). */
  blocksLeft: number;
  /** A year's renewal, as the node prices it. */
  renewal: string | null;
}

/** Owned names are worth a banner this many days before they expire. */
export const NAME_WARN_DAYS = 60;

export function nameAlert(info: NameDetail, me: string, height: number, windowBlocks: number): NameAlert | null {
  if (info.owner?.address !== me || info.expires_at == null) return null;
  const renewal = info.renewal_price_display ?? null;
  // Older nodes don't say `status`: work it out from the heights, as node/src/api.rs does.
  const status = info.status ?? (height <= info.expires_at ? "active" : info.grace_until != null && height <= info.grace_until ? "grace" : "available");
  if (status === "grace") return { name: info.name, state: "grace", blocksLeft: Math.max(0, (info.grace_until ?? height) - height), renewal };
  if (status !== "active") return null;
  const left = info.expires_at - height;
  return left <= windowBlocks ? { name: info.name, state: "expiring", blocksLeft: Math.max(0, left), renewal } : null;
}

/**
 * The cheapest name's price, in drops, from `/api/params` (`name_price_3`, `_4`, `_5`: governance
 * parameters, never hard-coded here). Null when the node doesn't say.
 */
export function cheapestName(params: Record<string, unknown> | null | undefined): bigint | null {
  const prices = ["name_price_3", "name_price_4", "name_price_5"]
    .map((k) => params?.[k])
    .filter((v): v is string | number => (typeof v === "string" && /^\d+$/.test(v)) || (typeof v === "number" && Number.isSafeInteger(v) && v >= 0))
    .map((v) => BigInt(v));
  return prices.length ? prices.reduce((m, p) => (p < m ? p : m)) : null;
}

/** Can this balance buy any name? Without a price from the node, any FANG at all might. */
export const canAffordName = (balance: bigint, cheapest: bigint | null) => (cheapest === null ? balance > 0n : balance >= cheapest);
