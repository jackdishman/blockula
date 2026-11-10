/**
 * An account's balance over time, from the history the wallet already reads (`Client.history`)
 * and the balance the node reports now. Amounts stay bigint drops (FANG) or raw token units.
 *
 * FANG is a chain fact: walk backward from the current balance by what each sealed transaction
 * moved (`deltaFor`). A newest-suffix is exact for the span it covers. A program token is not:
 * its line counts `transfer` events that program reported (BTS-1: 32-byte from, 32-byte to, u64
 * amount, little-endian), which the chain does not check. A later price series is a `TimeSeries`
 * built somewhere else; nothing here is special to balances except how the points are chosen.
 */
import type { TxDetail } from "./explorer/types.ts";
import { deltaFor, DROPS_PER_FANG, showsAccount } from "./explorer/ledger.ts";
import { describeSeries, formatMinor, timeAtHeight, type BlockClock, type TimeSeries } from "./series.ts";

export { DROPS_PER_FANG };

const ADDRESS = /^t?boo1[02-9ac-hj-np-z]{40,}$/;

/** A bech32 address rather than a `.boo` name, same test the ledger uses. */
const isAddress = (s: unknown): s is string => typeof s === "string" && ADDRESS.test(s);

export interface BalanceMove {
  /** When it was sealed, from the chain clock. */
  t: number;
  /** Oldest first, so two moves in one block stay in the order they ran. */
  seq: number;
  /** Signed change in minor units. */
  delta: bigint;
  /** False when the transaction moves more than its fields show. */
  exact: boolean;
}

const noteJoin = (a: string | null, b: string | null) => [a, b].filter(Boolean).join(" ");

/**
 * Vertices of a step line ending at `balance`. Walk newest to oldest so the present stays
 * anchored, then draw each move as the balance before it and the balance after it, at one time.
 */
export function seriesFromBalance(opts: {
  id: string;
  label: string;
  unit: string;
  scale: bigint;
  balance: bigint;
  moves: readonly BalanceMove[];
  now: number;
  approximate: boolean;
  note: string | null;
  emptySummary: string;
  /** When nothing moved, a flat line may still start here (a sealed transaction that changed nothing). */
  unchangedFrom?: number;
  detail?: string;
}): TimeSeries {
  const moves = opts.moves.filter((m) => m.delta !== 0n).slice().sort((a, b) => b.t - a.t || b.seq - a.seq);
  let cursor = opts.balance;
  let below = false;
  const steps: { t: number; before: bigint; after: bigint }[] = [];
  for (const m of moves) {
    const after = cursor;
    const before = cursor - m.delta;
    if (before < 0n || after < 0n) below = true;
    steps.push({ t: m.t, before, after });
    cursor = before;
  }
  steps.reverse();
  const points: TimeSeries["points"] = [];
  const push = (t: number, value: bigint) => {
    const last = points[points.length - 1];
    if (last && last.t === t && last.value === value) return;
    points.push({ t, value });
  };
  for (const s of steps) {
    push(s.t, s.before);
    push(s.t, s.after);
  }
  if (!points.length && opts.unchangedFrom != null) push(opts.unchangedFrom, opts.balance);
  const end = Math.max(opts.now, points.length ? points[points.length - 1].t : 0);
  if (points.length) {
    const last = points[points.length - 1];
    if (last.t < end || last.value !== opts.balance) push(end, opts.balance);
  }
  const note = below ? noteJoin(opts.note, "The line goes below zero, so a transaction was read short of what it moved.") : opts.note;
  const base = { id: opts.id, label: opts.label, unit: opts.unit, scale: opts.scale, note, detail: opts.detail };
  if (!points.length) return { ...base, points, approximate: false, summary: opts.emptySummary };
  const series: TimeSeries = { ...base, points, approximate: opts.approximate || below, summary: "" };
  series.summary = describeSeries(series, "all");
  return series;
}

/** Sealed transactions only, oldest first, with what each one did to `me`. */
export function fangMoves(txs: readonly TxDetail[], me: string, clock: BlockClock, creationFee: bigint | null): { moves: BalanceMove[]; inexact: number; sealed: number } {
  const sealed = txs.filter((tx) => tx.status !== "pending" && tx.dropped !== true && tx.height != null);
  const rows = sealed.map((tx, index) => ({ tx, index }));
  // History arrives newest first, so a higher index is older. Same-height moves ran in that order.
  rows.sort((a, b) => a.tx.height! - b.tx.height! || b.index - a.index);
  const moves: BalanceMove[] = [];
  let inexact = 0;
  // Accounts an earlier transaction in this order already showed. Height alone can't: two payments
  // in one block, and only the first of them can open the recipient.
  const seen = new Set<string>();
  rows.forEach(({ tx }, seq) => {
    const kind = tx.kind ?? tx.action?.type ?? "";
    const recipientExisted = !!tx.to && seen.has(tx.to.address);
    const senderExisted = !!tx.from && seen.has(tx.from.address);
    // The creation fee burns from the sender only when this payment opened the recipient's account.
    const opening = creationFee != null && !recipientExisted && kind === "transfer" && isAddress(tx.action?.to);
    const { delta, exact } = deltaFor(tx, me, { recipientExisted, senderExisted, ...(opening ? { creationFee } : {}) });
    for (const w of [tx.from, tx.to]) if (w && showsAccount(tx, w.address)) seen.add(w.address);
    if (!exact) inexact++;
    if (delta === 0n) return;
    moves.push({ t: timeAtHeight(tx.height!, clock), seq, delta, exact });
  });
  return { moves, inexact, sealed: sealed.length };
}

const historyNote = (truncated: boolean, historyFrom?: number) => {
  const parts: string[] = [];
  if (truncated) parts.push("This line uses the newest transactions the node returned, not the whole history.");
  if (historyFrom != null && historyFrom > 1) parts.push(`This node holds history from block ${historyFrom.toLocaleString("en-US")} on.`);
  return parts.length ? parts.join(" ") : null;
};

/** The FANG series for an account. `balance` is its current balance, in drops. */
export function fangSeries(opts: {
  me: string;
  balance: bigint;
  unit: string;
  txs: readonly TxDetail[];
  clock: BlockClock;
  creationFee: bigint | null;
  truncated: boolean;
  historyFrom?: number;
}): TimeSeries {
  // A truncated history can't show that a recipient was new: charging the creation fee would invent it.
  const fee = !opts.truncated && (opts.historyFrom == null || opts.historyFrom <= 1) ? opts.creationFee : null;
  const { moves, inexact, sealed } = fangMoves(opts.txs, opts.me, opts.clock, fee);
  const span = historyNote(opts.truncated, opts.historyFrom);
  const sketch = inexact > 0 ? "Some transactions don't record every drop they move, so the line before those may not match what the account held." : null;
  const amount = formatMinor(opts.balance, DROPS_PER_FANG, opts.unit);
  let emptySummary = "No transactions yet. The line appears once a payment is sealed into a block.";
  let unchangedFrom: number | undefined;
  if (sealed === 0 && opts.balance > 0n) emptySummary = `The balance is ${amount}, and this history has no transaction to draw a line from.`;
  else if (sealed === 0) emptySummary = noteJoin(emptySummary, span) ?? emptySummary;
  else if (!moves.length) {
    const times = opts.txs.filter((tx) => tx.height != null && tx.status !== "pending" && tx.dropped !== true).map((tx) => timeAtHeight(tx.height!, opts.clock));
    unchangedFrom = Math.min(...times);
  }
  return seriesFromBalance({
    id: "native",
    label: opts.unit,
    unit: opts.unit,
    scale: DROPS_PER_FANG,
    balance: opts.balance,
    moves,
    now: opts.clock.now,
    approximate: inexact > 0,
    note: noteJoin(sketch, span),
    emptySummary: noteJoin(emptySummary, sealed === 0 && opts.balance > 0n ? span : null) ?? emptySummary,
    unchangedFrom,
  });
}

export interface TokenMove {
  program: string;
  txId: string;
  /** Index in that transaction's event list. */
  index: number;
  height: number;
  /**
   * Position in the newest-first list this move was read from. A move seen only on the account
   * (the program's own history didn't reach it) sorts older than one the program history listed.
   */
  order: number;
  t: number;
  delta: bigint;
  omitted: boolean;
}

const u64le = (hex: string): bigint | null => {
  if (!/^[0-9a-f]{16}$/.test(hex)) return null;
  let n = 0n;
  for (let i = 0; i < 8; i++) n += BigInt(parseInt(hex.slice(i * 2, i * 2 + 2), 16)) << (8n * BigInt(i));
  return n;
};

/** A program's `balance_of` answer: exactly 8 bytes, little-endian. Anything else isn't this layout. */
export function decodeBalance(hex: string | null | undefined): bigint | null {
  if (!hex) return null;
  return u64le(hex.trim().toLowerCase().replace(/^0x/, ""));
}

/**
 * `transfer` events that name `meKey` (the account's 32 bytes, lowercase hex). Failed calls are
 * skipped: their events were rolled back. `order` is the index in `txs`, which must be newest first.
 */
export function tokenMoves(txs: readonly TxDetail[], meKey: string, clock: BlockClock, orderFrom = 0): { moves: TokenMove[]; omitted: boolean } {
  const moves: TokenMove[] = [];
  let omitted = false;
  txs.forEach((tx, i) => {
    if (tx.status !== "finalized" || tx.height == null || tx.program?.outcome?.ok !== true) return;
    if ((tx.program.claims?.omitted?.events ?? 0) > 0) omitted = true;
    const events = tx.program.claims?.events ?? [];
    events.forEach((e, index) => {
      if (e.untrusted_name !== "transfer" || !isAddress(e.program)) return;
      const data = e.data.trim().toLowerCase().replace(/^0x/, "");
      if (!/^[0-9a-f]{144}$/.test(data)) return;
      const amount = u64le(data.slice(128));
      if (amount == null || amount === 0n) return;
      const from = data.slice(0, 64);
      const to = data.slice(64, 128);
      let delta = 0n;
      if (to === meKey && from !== meKey) delta = amount;
      else if (from === meKey && to !== meKey) delta = -amount;
      else return;
      moves.push({ program: e.program, txId: tx.id, index, height: tx.height!, order: orderFrom + i, t: timeAtHeight(tx.height!, clock), delta, omitted: (tx.program?.claims?.omitted?.events ?? 0) > 0 });
    });
  });
  return { moves, omitted };
}

/** One move per transaction event. Earlier lists win, so pass the program's own history first. */
export function mergeTokenMoves(groups: readonly (readonly TokenMove[])[]): TokenMove[] {
  const seen = new Set<string>();
  const out: TokenMove[] = [];
  for (const g of groups) {
    for (const m of g) {
      const k = `${m.txId}:${m.index}:${m.program}`;
      if (seen.has(k)) continue;
      seen.add(k);
      out.push(m);
    }
  }
  return out;
}

const ACCOUNT_ONLY = 1_000_000;

/** Moves the account's own history has that the program history didn't, sorted as older. */
export function accountOnlyMoves(accountMoves: readonly TokenMove[], programMoves: readonly TokenMove[]): TokenMove[] {
  const seen = new Set(programMoves.map((m) => `${m.txId}:${m.index}:${m.program}`));
  return accountMoves.filter((m) => !seen.has(`${m.txId}:${m.index}:${m.program}`)).map((m) => ({ ...m, order: ACCOUNT_ONLY + m.order }));
}

function asMoves(moves: readonly TokenMove[]): BalanceMove[] {
  const sorted = moves.slice().sort((a, b) => a.height - b.height || b.order - a.order);
  return sorted.map((m, seq) => ({ t: m.t, seq, delta: m.delta, exact: !m.omitted }));
}

const TOKEN_NOTE = "Counted from transfer events the program reported. The chain doesn't check that count.";

/**
 * One token's line, or null when there's nothing to show (no balance and no transfers).
 * With a `balance` anchor the newest moves walk back from it. Without one, a line is drawn from
 * zero only when the history is complete and the running total never goes negative.
 */
export function tokenSeries(opts: {
  program: string;
  label: string;
  moves: readonly TokenMove[];
  balance: bigint | null;
  now: number;
  complete: boolean;
  truncated: boolean;
  omitted: boolean;
}): TimeSeries | null {
  const extra = [opts.omitted ? "A receipt left some events out, so a transfer may be missing." : null, historyNote(opts.truncated)].filter(Boolean).join(" ");
  const note = noteJoin(TOKEN_NOTE, extra || null);
  const moves = asMoves(opts.moves);
  const nonzero = moves.some((m) => m.delta !== 0n);
  if (opts.balance == null && !nonzero) return null;
  if ((opts.balance == null || opts.balance === 0n) && !nonzero) return null;
  const empty = (summary: string): TimeSeries => ({
    id: `token:${opts.program}`,
    label: opts.label,
    unit: "",
    scale: 1n,
    points: [],
    approximate: false,
    summary,
    note,
    detail: opts.program,
  });
  let balance = opts.balance;
  if (balance == null) {
    if (!opts.complete) return empty(`These transfers don't include the balance now, so there's no line. ${note}`);
    let bal = 0n;
    for (const m of moves) {
      bal += m.delta;
      if (bal < 0n) return empty(`These transfers don't start from zero in the history on hand, so there's no line. ${note}`);
    }
    balance = bal;
  }
  if (!nonzero) return empty(`The program reports ${formatMinor(balance, 1n)}. This history has no transfer of it, so there's no line. ${note}`);
  const oldest = moves.reduce((t, m) => Math.min(t, m.t), opts.now);
  return seriesFromBalance({
    id: `token:${opts.program}`,
    label: opts.label,
    unit: "",
    scale: 1n,
    balance,
    moves,
    now: opts.now,
    approximate: opts.omitted || moves.some((m) => !m.exact),
    note,
    emptySummary: empty("").summary,
    unchangedFrom: oldest,
    detail: opts.program,
  });
}

/** Program addresses this account's own transactions name, callers and events first, at most `cap`. */
export function programsTouched(txs: readonly TxDetail[], cap = 6): string[] {
  const out: string[] = [];
  const add = (s: unknown) => {
    if (typeof s !== "string" || !isAddress(s) || out.includes(s) || out.length >= cap) return;
    out.push(s);
  };
  for (const tx of txs) {
    const calls = tx.action?.type === "call_program" && Array.isArray(tx.action.calls) ? tx.action.calls : [];
    for (const c of calls) if (c && typeof c === "object") add((c as { program?: unknown }).program);
    for (const e of tx.program?.claims?.events ?? []) add(e.program);
    add(tx.program?.facts?.created);
  }
  return out;
}

/** What `/api/fees` calls the account-creation fee, or null when the node didn't say. */
export function creationFeeOf(fees: unknown): bigint | null {
  if (!fees || typeof fees !== "object") return null;
  const v = (fees as { account_creation_fee?: unknown }).account_creation_fee;
  return typeof v === "string" && /^\d+$/.test(v) ? BigInt(v) : null;
}

export interface HistoryRead {
  txs: TxDetail[];
  truncated: boolean;
  historyFrom?: number;
}

const asTx = (t: unknown): TxDetail | null => {
  if (!t || typeof t !== "object") return null;
  const o = t as TxDetail;
  return typeof o.id === "string" && typeof o.status === "string" ? o : null;
};

/**
 * Pages of `Client.history` (or the same shape), newest first, up to `maxPages`.
 * A node without paged history returns the newest 25 (`paged: false`); that's a truncated line.
 */
export async function readHistory(
  read: (page: { before?: string; limit: number }) => Promise<{ txs?: unknown; next?: unknown; paged?: unknown; history_from?: unknown }>,
  opts: { limit?: number; maxPages?: number } = {},
): Promise<HistoryRead> {
  const limit = opts.limit ?? 100;
  const maxPages = opts.maxPages ?? 20;
  const txs: TxDetail[] = [];
  let before: string | undefined;
  let historyFrom: number | undefined;
  const seen = new Set<string>();
  for (let i = 0; i < maxPages; i++) {
    const page = await read(before ? { before, limit } : { limit });
    if (typeof page.history_from === "number") historyFrom = page.history_from;
    const batch = Array.isArray(page.txs) ? page.txs.flatMap((t) => { const tx = asTx(t); return tx ? [tx] : []; }) : [];
    txs.push(...batch);
    const paged = page.paged !== false;
    const next = typeof page.next === "string" && page.next ? page.next : null;
    if (!paged) {
      const sealed = txs.filter((t) => t.height != null && t.status !== "pending").length;
      return { txs, truncated: sealed >= 25, historyFrom };
    }
    if (!next) return { txs, truncated: false, historyFrom };
    if (seen.has(next) || i === maxPages - 1) return { txs, truncated: true, historyFrom };
    seen.add(next);
    before = next;
  }
  return { txs, truncated: true, historyFrom };
}
