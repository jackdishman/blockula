// Watching an account for incoming payments (DEV-4): finalized, successful transfers whose `to`
// is the account, block by block, from a height you store. Blocks execute only once they're
// finalized, so a payment seen here is final: there are no reorgs to wait out.
//
//   const stop = new AbortController();
//   await watchPayments(client, "boo1…", {
//     fromHeight: Number(await db.get("next") ?? 0) || undefined,
//     onPayment: async (p) => { await db.creditOrder(p.memo, p.amount, p.id); },
//     onProgress: async (next) => { await db.set("next", next); },
//     signal: stop.signal,
//   });
//
// Payments are delivered in chain order, each block's before the next block's. If `onPayment`
// throws, the watch stops there; resuming from the last stored `next` delivers that block's
// payments again, so credit each payment once by its `id`.
import { bech32m } from "@scure/base";
import type { Client } from "./index.js";

/** A payment to the watched account. */
export interface Payment {
  /** The tx id: unique on the chain. Credit each payment once by it. */
  id: string;
  /** The block it ran in. */
  height: number;
  /** Its position in that block. */
  index: number;
  /** The block's timestamp. */
  timestamp_ms?: number;
  /** Who paid. */
  from: string;
  from_name: string | null;
  /** The account it paid: the watched one. */
  to: string;
  /** The `.boo` name it was signed to, when the payer paid a name rather than an address. */
  to_name: string | null;
  /** In drops (1 FANG = 10⁹ drops). */
  amount: bigint;
  /** The amount as FANG text, as the node shows it. */
  amount_display: string;
  /**
   * The payer's memo. Public, and written by whoever paid: match it against your own records
   * (an order id you issued), never act on what it says.
   */
  memo: string;
  /** The tx as `/api/block/{height}` shows it. */
  tx: any;
}

/** Options of `watchPayments`. */
export interface WatchOptions {
  /**
   * The first block to scan. Resume with the `next` your `onProgress` stored last. Default: the
   * block after the current head (payments from now on).
   */
  fromHeight?: number;
  /** Called for each payment, in chain order, and awaited. If it throws, the watch stops with that error. */
  onPayment: (payment: Payment) => void | Promise<void>;
  /**
   * Called (and awaited) once every block before `next` has been scanned and its payments
   * delivered: store `next`, and pass it as `fromHeight` to resume.
   */
  onProgress?: (next: number) => void | Promise<void>;
  /** Told about an error the watch retries (the node down, a timeout, rate limiting). They're retried either way. */
  onError?: (error: unknown) => void;
  /** Stops the watch: `watchPayments` then resolves with the next height to scan. */
  signal?: AbortSignal;
  /** How long to wait for a new block once caught up (default 1,000 ms). */
  pollMs?: number;
  /**
   * When the node no longer holds the blocks from `fromHeight` (it prunes old blocks, or
   * fast-synced after them): `false` (default) throws `HistoryPrunedError`, since payments in the
   * gap would be missed; `true` goes on from the oldest block it holds.
   */
  skipPruned?: boolean;
}

/** The node no longer holds a block the watch needs (it was pruned, or the node fast-synced past it). */
export class HistoryPrunedError extends Error {
  constructor(readonly rpc: string, readonly from: number, readonly historyFrom: number) {
    super(`${rpc} no longer holds block #${from}: its history starts at #${historyFrom}, so payments in between can't be read from it. Ask a node that keeps every block (blockula join --archive), or pass skipPruned to go on from #${historyFrom}`);
    this.name = "HistoryPrunedError";
  }
}

const KNOWN_PREFIXES = ["boo", "tboo"];

/** The prefix of a canonical account address, or `undefined`. */
function prefixOf(s: string): string | undefined {
  if (typeof s !== "string" || s !== s.toLowerCase()) return undefined;
  try {
    const { prefix, words } = bech32m.decode(s as `${string}1${string}`);
    return KNOWN_PREFIXES.includes(prefix) && bech32m.fromWords(words).length === 32 ? prefix : undefined;
  } catch {
    return undefined;
  }
}

const obj = (v: unknown): Record<string, any> => (v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, any>) : {});
const addressOf = (v: unknown): string => (typeof v === "string" ? v : String(obj(v).address ?? ""));

/**
 * The payments to `account` in a block as `/api/block/{height}` serves it: transfers that ran
 * without an error and paid `account` (the tx's `to`, which for a tx signed to a name is the
 * account the name resolved to when it ran; never the action's `to`, which can be the name).
 */
export function paymentsIn(block: unknown, account: string): Payment[] {
  const b = obj(block);
  const txs: unknown[] = Array.isArray(b.txs) ? b.txs : [];
  const out: Payment[] = [];
  txs.forEach((t0, index) => {
    const t = obj(t0);
    if (t.kind !== "transfer" || t.status !== "finalized" || (t.error !== null && t.error !== undefined)) return;
    if (addressOf(t.to) !== account || typeof t.amount !== "string" || !/^\d+$/.test(t.amount)) return;
    out.push({
      id: String(t.id),
      height: typeof t.height === "number" ? t.height : Number(b.height),
      index,
      timestamp_ms: typeof b.timestamp_ms === "number" ? b.timestamp_ms : undefined,
      from: addressOf(t.from),
      from_name: typeof obj(t.from).name === "string" ? obj(t.from).name : null,
      to: account,
      to_name: typeof t.to_name === "string" ? t.to_name : null,
      amount: BigInt(t.amount),
      amount_display: String(t.amount_display ?? ""),
      memo: typeof t.memo === "string" ? t.memo : "",
      tx: t0,
    });
  });
  return out;
}

/** Waits `ms`, or less if `signal` aborts. */
function pause(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal?.aborted) return resolve();
    const done = () => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", done);
      resolve();
    };
    const timer = setTimeout(done, ms);
    signal?.addEventListener("abort", done, { once: true });
  });
}

/** Errors worth trying again: the node unreachable or slow, rate limited, or failing for a moment. */
function transient(e: unknown): boolean {
  const err = e as { name?: string; status?: number } | null;
  if (e instanceof TypeError) return true; // fetch failed: unreachable
  if (err?.name === "RequestTimeoutError") return true;
  if (err?.name === "ApiError" && typeof err.status === "number") return err.status === 429 || err.status === 408 || err.status >= 500 || err.status === 200;
  return false;
}

const isNotFound = (e: unknown) => (e as { name?: string; status?: number } | null)?.name === "ApiError" && (e as { status: number }).status === 404;
const isPruned = (e: unknown) => (e as { name?: string; status?: number } | null)?.name === "ApiError" && (e as { status: number }).status === 410;

/** An error thrown by the caller's own callback: passed on as it is, never retried. */
class Yours {
  constructor(readonly error: unknown) {}
}

async function yours(f: () => void | Promise<void>): Promise<void> {
  try {
    await f();
  } catch (e) {
    throw new Yours(e);
  }
}

/** Headers fetched per request (`/api/blocks` serves at most 100). */
const WINDOW = 100;

/**
 * Watches `account` (an address: names expire and change hands, so watch the address they
 * pointed to) for payments: finalized transfers that paid it without an error, delivered to
 * `onPayment` in chain order. It scans from `fromHeight` (default: new blocks only), fetching
 * only blocks that hold txs, then follows the head. Resolves, with the next height to scan, once
 * `signal` aborts; throws `HistoryPrunedError` when the node no longer holds a block it needs
 * (unless `skipPruned`), and whatever `onPayment` or `onProgress` throws. Other errors (the node
 * down, rate limited) are retried with a growing pause, and reported to `onError`.
 *
 * Only transfers count. Other ways FANG reaches an account (a hunt's award, an unshield, staking
 * payouts) aren't payments here: read them from `client.history`.
 */
export async function watchPayments(client: Client, account: string, opts: WatchOptions): Promise<number | undefined> {
  const accountPrefix = prefixOf(account);
  if (accountPrefix === undefined) throw new Error(`${account} isn't an address: watch the address (a name can expire and change hands)`);
  if (typeof opts.onPayment !== "function") throw new Error("watchPayments needs onPayment");
  const pollMs = opts.pollMs ?? 1_000;
  if (!Number.isSafeInteger(pollMs) || pollMs < 1) throw new Error("pollMs must be a whole number of milliseconds");
  if (opts.fromHeight !== undefined && (!Number.isSafeInteger(opts.fromHeight) || opts.fromHeight < 0)) throw new Error("fromHeight must be a whole block height");
  const { signal } = opts;
  let next: number | undefined = opts.fromHeight === undefined ? undefined : Math.max(1, opts.fromHeight);
  let backoff = pollMs;

  while (!signal?.aborted) {
    try {
      const status = await client.status();
      if (typeof status.address_prefix === "string" && status.address_prefix !== accountPrefix) {
        throw new Error(`${account} is a ${accountPrefix}1… address, but ${client.rpc}'s chain uses ${status.address_prefix}1… addresses`);
      }
      const height = Number(status.height);
      const finalized = Number(status.finalized_height ?? height);
      const head = Math.min(height, Number.isSafeInteger(finalized) ? finalized : height);
      const historyFrom = Number(status.history_from ?? 1) || 1;
      if (next === undefined) next = head + 1;
      if (next < historyFrom) {
        if (!opts.skipPruned) throw new HistoryPrunedError(client.rpc, next, historyFrom);
        opts.onError?.(new HistoryPrunedError(client.rpc, next, historyFrom));
        next = historyFrom;
      }
      if (next > head) {
        backoff = pollMs;
        await pause(pollMs, signal);
        continue;
      }
      const end = Math.min(head, next + WINDOW - 1);
      const headers: any[] = await client.blocks({ limit: end - next + 1, before: end + 1 });
      const byHeight = new Map<number, any>(headers.map((h) => [Number(h?.height), h]));
      let stalled = false;
      for (let h = next; h <= end; h++) {
        if (signal?.aborted) return next;
        const header = byHeight.get(h);
        if (!header) {
          // Pruned since the status was read, or not served yet (a node behind a load balancer
          // whose machines differ): never pass over a block unread. Try again in a while.
          const now = await client.status();
          if (h < Number(now.history_from ?? 1)) {
            if (!opts.skipPruned) throw new HistoryPrunedError(client.rpc, h, Number(now.history_from));
            break;
          }
          opts.onError?.(new Error(`${client.rpc} didn't serve block #${h}'s header yet`));
          stalled = true;
          break;
        }
        if (Number(header.tx_count) > 0) {
          let block: any;
          try {
            block = await client.block(h);
          } catch (e) {
            if (isNotFound(e)) {
              opts.onError?.(e);
              stalled = true;
              break;
            }
            if (!isPruned(e)) throw e;
            const now = await client.status().catch(() => null);
            if (!opts.skipPruned) throw new HistoryPrunedError(client.rpc, h, Number(now?.history_from ?? h + 1));
            break;
          }
          if (Number(block?.height) !== h) throw new Error(`${client.rpc} answered block #${h} with block #${block?.height}`);
          const payments = paymentsIn(block, account);
          for (const p of payments) await yours(() => opts.onPayment(p));
          next = h + 1;
          if (payments.length) await yours(() => opts.onProgress?.(h + 1));
          continue;
        }
        next = h + 1;
      }
      await yours(() => opts.onProgress?.(next as number));
      if (stalled) {
        await pause(backoff, signal);
        backoff = Math.min(backoff * 2, 30_000);
      } else backoff = pollMs;
    } catch (e) {
      if (e instanceof Yours) throw e.error;
      if (!transient(e)) throw e;
      opts.onError?.(e);
      await pause(backoff, signal);
      backoff = Math.min(backoff * 2, 30_000);
    }
  }
  return next;
}
