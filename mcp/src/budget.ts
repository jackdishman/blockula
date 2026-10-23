// What the agent may spend, and the checks made before anything is signed.
//
// - Caps (AGENT-19, AGENT-15): BLOCKULA_MAX_SEND per call and BLOCKULA_MAX_DAILY per UTC day.
//   Unset, they're 100 and 101 FANG. A bad value is a configuration error every tool reports,
//   never a crash at startup.
// - Refusals before signing (AGENT-6): a session key signing what it can't, or more than its
//   allowance left; a payment past what the account can move; a key that isn't on the account.
//   They read what the node reports (the account, its keys, the fee), so a lying node can only
//   cause a refusal here, never a spend: whatever passes is still checked by the chain, and still
//   reserved against the caps (spend.ts).
// - What can be spent now (AGENT-7): the smallest of what the account can move, the session key's
//   allowance left this window, and the MCP's caps, with the one that binds.
import { checkTimeoutMs, formatFang, parseFang } from "@blockula/sdk";

/**
 * The default per-call cap; the daily cap is it plus DAILY_FEE_ALLOWANCE (review-fix-clients-3).
 * The caps guard against mistakes. A compromised agent can read its own key file, so only the
 * session key's on-chain limit protects the user's FANG from one.
 */
export const DEFAULT_MAX_SEND = "100";
/** Fees count toward the daily cap: by default it's the per-call cap plus this, so one full-size call fits. */
export const DAILY_FEE_ALLOWANCE = "1";

/** The spending caps in force. */
export interface Caps {
  maxSend: bigint;
  maxDaily: bigint;
  /** How a refusal names them. */
  sendText: string;
  dailyText: string;
}

const env = (e: Record<string, string | undefined>, name: string) => {
  const v = e[name]?.trim();
  return v === undefined || v === "" ? undefined : v;
};

const fangEnv = (name: string, v: string) => {
  try {
    return parseFang(v);
  } catch {
    throw new Error(`${name} must be an amount of FANG with up to 9 decimals, like "10" (got "${v.slice(0, 40)}")`);
  }
};

/** The caps from the environment. Throws on a bad value. */
export function capsFrom(e: Record<string, string | undefined>): Caps {
  const send = env(e, "BLOCKULA_MAX_SEND");
  const daily = env(e, "BLOCKULA_MAX_DAILY");
  const maxSend = fangEnv("BLOCKULA_MAX_SEND", send ?? DEFAULT_MAX_SEND);
  const maxDaily = daily !== undefined ? fangEnv("BLOCKULA_MAX_DAILY", daily) : maxSend + parseFang(DAILY_FEE_ALLOWANCE);
  const sendText = send !== undefined ? `BLOCKULA_MAX_SEND=${send} FANG` : `BLOCKULA_MAX_SEND, by default ${formatFang(maxSend)}`;
  const dailyText = daily !== undefined ? `BLOCKULA_MAX_DAILY=${daily} FANG` : `BLOCKULA_MAX_DAILY, by default BLOCKULA_MAX_SEND + ${DAILY_FEE_ALLOWANCE} FANG for fees = ${formatFang(maxDaily)}`;
  return { maxSend, maxDaily, sendText, dailyText };
}

/** A duration in milliseconds from the environment: whole and within a timer's range. */
export function msFrom(e: Record<string, string | undefined>, name: string, fallback: number): number {
  const v = env(e, name);
  if (v === undefined) return fallback;
  if (!/^\d+$/.test(v)) throw new Error(`${name} must be a positive number of milliseconds`);
  return checkTimeoutMs(Number(v), name);
}

/**
 * The configuration error in these variables, if any: every cap and duration is parsed once at
 * startup, inside a try, and the server reports the error through every tool (as it does a bad
 * network) instead of exiting.
 */
export function configError(e: Record<string, string | undefined>): Error | null {
  try {
    capsFrom(e);
    msFrom(e, "BLOCKULA_TIMEOUT_MS", 15_000);
    msFrom(e, "BLOCKULA_TX_WAIT_MS", 15_000);
    return null;
  } catch (err) {
    return err as Error;
  }
}

/** An amount the agent passes: FANG with up to 9 decimals; a trailing " FANG" is allowed (AGENT-14). */
export function amountArg(s: string, what = "amount"): bigint {
  const v = s.trim().replace(/\s*FANG$/i, "");
  try {
    return parseFang(v);
  } catch {
    throw new Error(`${what} must be a number in FANG with up to 9 decimals, like "1.5" (got "${s.slice(0, 40)}")`);
  }
}

/** About how long `blocks` blocks take at the chain's block time, e.g. "about 23 h". */
export function aboutBlocks(blocks: number, blockTimeMs = 1000): string {
  const s = Math.max(0, blocks) * (blockTimeMs / 1000);
  if (s < 90) return `about ${Math.round(s)} s`;
  if (s < 5400) return `about ${Math.round(s / 60)} min`;
  if (s < 172_800) return `about ${Math.round(s / 3600)} h`;
  return `about ${Math.round(s / 86_400)} days`;
}

/** A drops amount the node reports (a digit string, or a safe whole number), or undefined. */
export const dropsOf = (v: unknown) => (typeof v === "string" && /^\d+$/.test(v) ? BigInt(v) : typeof v === "number" && Number.isSafeInteger(v) && v >= 0 ? BigInt(v) : undefined);
const digits = dropsOf;
const whole = (v: unknown) => (Number.isSafeInteger(v) && (v as number) >= 0 ? (v as number) : undefined);

/** A key on an account, as `/api/account` lists it. */
export interface KeyEntry {
  key_id?: unknown;
  label?: unknown;
  role?: { type?: unknown; limit?: unknown; period_blocks?: unknown; expires_at?: unknown };
  spent_this_period?: unknown;
  window_start?: unknown;
}

/** A session key's allowance as of the block a tx can run in first. */
export interface Allowance {
  limit: bigint;
  /** Spent in the window that block falls in (0 once the window restarts). */
  spent: bigint;
  left: bigint;
  periodBlocks: number;
  /** When the window restarts, if anything's spent in it. */
  resetsAt?: number;
  expiresAt: number;
  expired: boolean;
}

/** The node's session-key rule (node/src/chain.rs `admit`): spent + amount + charge ≤ limit, the window restarting `period_blocks` after it began. */
export function allowanceOf(key: KeyEntry | undefined, height: number): Allowance | null {
  if (key?.role?.type !== "session") return null;
  const limit = digits(key.role.limit);
  const periodBlocks = whole(key.role.period_blocks);
  const expiresAt = whole(key.role.expires_at);
  if (limit === undefined || periodBlocks === undefined || expiresAt === undefined) return null;
  const windowStart = whole(key.window_start) ?? 0;
  const restarts = height >= windowStart + periodBlocks;
  const spent = restarts ? 0n : (digits(key.spent_this_period) ?? 0n);
  return { limit, spent, left: limit > spent ? limit - spent : 0n, periodBlocks, ...(spent > 0n ? { resetsAt: windowStart + periodBlocks } : {}), expiresAt, expired: height > expiresAt };
}

/** The key with this id on an account (`/api/account`'s `keys`), or undefined when it isn't there; null when the node doesn't list keys. */
export function keyOn(account: { keys?: unknown } | null | undefined, keyId: string): KeyEntry | undefined | null {
  if (!account || !Array.isArray(account.keys)) return null;
  return (account.keys as KeyEntry[]).find((k) => k?.key_id === keyId);
}

/** Everything the checks before signing need: reads the MCP makes anyway, and the tx's cost. */
export interface Preflight {
  /** `/api/account` for the account the tx acts for (null: the node didn't answer). */
  account: any;
  keyId: string;
  actingFor: string;
  action: string;
  /**
   * FANG the action takes out of what the account can move: sent, a name's price, escrowed, a
   * bond. Not a stake (locked FANG can be staked) or a vote (it locks, it doesn't spend).
   */
  outflow: bigint;
  /** What opening the recipient's account burns on top. */
  opening: bigint;
  /** The tx's fee (what it can be charged at most). */
  fee: bigint;
  /** The first block the tx can run in (the node's height + 1). */
  height: number;
  blockTimeMs?: number;
}

const REFUSED = "Refused before signing: nothing was signed or sent, and nothing counts toward today's limit.";

/**
 * Throws when the chain would refuse the tx for something the node's reads already show (AGENT-6).
 * A field the node doesn't report isn't checked: the chain still is.
 */
export function preflight(p: Preflight): void {
  const short = (a: string) => (a.length > 20 ? `${a.slice(0, 12)}…${a.slice(-6)}` : a);
  const key = keyOn(p.account, p.keyId);
  if (key === undefined) {
    throw new Error(`${REFUSED} This agent's key (${checkCodeOf(p.keyId)}) isn't on ${p.actingFor}: it was removed, or never added. Call use_account with the account to get a new grant code for the user, or use_account "self" to act for the key's own address.`);
  }
  const allowance = allowanceOf(key ?? undefined, p.height);
  if (key && allowance) {
    if (allowance.expired) throw new Error(`${REFUSED} This agent's session key on ${short(p.actingFor)} expired at block ${allowance.expiresAt}. Ask the user for a new budget: call use_account with the account for a new grant code.`);
    const familiar = Array.isArray(p.account?.familiars) && p.account.familiars.some((f: { key_id?: unknown }) => f?.key_id === p.keyId);
    const allowed = p.action === "transfer" || (p.action === "submit_claim" && familiar);
    if (!allowed) {
      const what =
        p.action === "submit_claim"
          ? "Only a session key the user registered as a Familiar can claim contributions: ask them to register it in their wallet (Programs → Accounts → Register a Familiar)."
          : p.action === "link_github"
            ? "Linking GitHub needs an owner key: ask the user to link GitHub in their wallet."
            : "Ask the user to do it in their wallet, with an owner key.";
      throw new Error(`${REFUSED} This agent acts for ${short(p.actingFor)} through a session key, and session keys can only send transfers (and claims, for registered Familiars). ${what}`);
    }
    // A transfer counts what it sends and what opening the recipient burns; a claim its bond; both the fee.
    const need = p.outflow + p.opening + p.fee;
    if (need > allowance.left) {
      const resets = allowance.resetsAt !== undefined ? ` It resets at block ${allowance.resetsAt} (${aboutBlocks(allowance.resetsAt - p.height, p.blockTimeMs)}).` : "";
      throw new Error(`${REFUSED} This needs ${formatFang(need)} with the fee${p.opening > 0n ? ` and the account-opening burn (${formatFang(p.opening)})` : ""}, but the session key has ${formatFang(allowance.left)} of its ${formatFang(allowance.limit)} allowance left this window.${resets} Send less, or ask the user to raise the limit.`);
    }
  }
  const transferable = digits(p.account?.transferable);
  if (p.outflow > 0n && transferable !== undefined && p.outflow + p.opening + p.fee > transferable) {
    const locked = digits(p.account?.locked) ?? 0n;
    throw new Error(`${REFUSED} This needs ${formatFang(p.outflow + p.opening + p.fee)} with the fee${p.opening > 0n ? ` and the account-opening burn (${formatFang(p.opening)})` : ""}, but ${short(p.actingFor)} can move ${formatFang(transferable)} now${locked > 0n ? ` (${formatFang(locked)} more is locked: wallet_info shows until when)` : ""}.`);
  }
}

const checkCodeOf = (keyId: string) => (typeof keyId === "string" && keyId.length >= 12 ? `${keyId.slice(0, 4).toUpperCase()}-${keyId.slice(4, 8).toUpperCase()}-${keyId.slice(-4).toUpperCase()}` : keyId);

/** One limit on what can be spent now. */
interface Limit {
  amount: bigint;
  why: string;
}

/**
 * What can be spent now, before fees, and what limits it: the account's transferable FANG, the
 * session key's allowance left this window, the MCP's daily cap left and its per-call cap.
 */
export function spendableNow(o: { transferable?: bigint; allowance?: Allowance | null; height: number; blockTimeMs?: number; dailyLeft: bigint; caps: Caps }): { amount: bigint; limitedBy: string; limits: string[] } {
  const limits: Limit[] = [];
  if (o.transferable !== undefined) limits.push({ amount: o.transferable, why: `the account can move ${formatFang(o.transferable)} now` });
  if (o.allowance) {
    const resets = o.allowance.resetsAt !== undefined ? ` until block ${o.allowance.resetsAt} (${aboutBlocks(o.allowance.resetsAt - o.height, o.blockTimeMs)})` : "";
    limits.push({ amount: o.allowance.expired ? 0n : o.allowance.left, why: o.allowance.expired ? `the session key expired at block ${o.allowance.expiresAt}` : `the session key has ${formatFang(o.allowance.left)} of ${formatFang(o.allowance.limit)} left${resets}` });
  }
  limits.push({ amount: o.dailyLeft, why: `this MCP's daily cap has ${formatFang(o.dailyLeft)} of ${formatFang(o.caps.maxDaily)} left today (UTC)` });
  limits.push({ amount: o.caps.maxSend, why: `this MCP's per-call cap is ${formatFang(o.caps.maxSend)}` });
  const binding = limits.reduce((a, b) => (b.amount < a.amount ? b : a));
  return { amount: binding.amount, limitedBy: binding.why, limits: limits.map((l) => l.why) };
}
