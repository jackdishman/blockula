import type { BlockSummary, BlockView, TxDetail, TxStatus } from "./types";

export const callOf = (tx: TxDetail) => tx.kind ?? tx.action?.type ?? "";
export const callTitle = (kind: string) => kind.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());

export type ActivitySort = "newest" | "oldest" | "fee-high" | "fee-low";

/** Filter the loaded history only; amounts and fees stay exact, even above Number's range. */
export function filterActivity(txs: TxDetail[], options: { call: string; status: TxStatus | "all"; search: string; sort: ActivitySort }) {
  const q = options.search.trim().toLowerCase();
  const shown = txs.filter((t) =>
    (options.call === "all" || callOf(t) === options.call) &&
    (options.status === "all" || t.status === options.status) &&
    (!q || [t.summary, t.memo, t.id, callOf(t), t.from?.name, t.to?.name, t.to_name, t.action?.name].some((v) => typeof v === "string" && v.toLowerCase().includes(q))),
  );
  // The input is newest first, including order within a block. Stable sorts retain that order.
  if (options.sort === "oldest") return shown.reverse();
  if (options.sort === "fee-high" || options.sort === "fee-low") {
    const dir = options.sort === "fee-high" ? -1 : 1;
    shown.sort((a, b) => {
      // Pending fees are caps, not charged fees: keep them after executed receipts.
      if (a.status === "pending" || b.status === "pending") return Number(a.status === "pending") - Number(b.status === "pending");
      const x = BigInt(a.fee ?? "0"), y = BigInt(b.fee ?? "0");
      return x === y ? 0 : x < y ? -dir : dir;
    });
  }
  return shown;
}

/**
 * Scan headers, reading only nonempty blocks, and select program calls BEFORE limiting results.
 * Blocks are consumed completely so a continuation never skips other calls in the last block.
 * A failed read rejects the scan: callers keep their cursor and can retry without a history gap.
 */
export async function scanActivity(
  get: <T>(path: string) => Promise<T>,
  calls: readonly string[],
  options: { before?: number; after?: number; pages?: number; target?: number; maxReads?: number; active?: () => boolean } = {},
) {
  const txs: TxDetail[] = [];
  let before = options.before;
  let newest: number | undefined;
  let oldest: number | undefined;
  let exhausted = false;
  let reads = 0;
  for (let page = 0; page < (options.pages ?? 5) && txs.length < (options.target ?? 50); page++) {
    if (options.active && !options.active()) break;
    const headers = await get<BlockSummary[]>(`/blocks?limit=100${before == null ? "" : `&before=${before}`}`);
    if (!headers.length) { exhausted = true; break; }
    const blocks = headers.filter((b) => b.height > (options.after ?? 0));
    if (!blocks.length) break;
    newest ??= blocks[0].height;
    oldest = blocks[blocks.length - 1].height;
    const busy = blocks.filter((b) => b.tx_count > 0);
    // Bound concurrency on a busy chain rather than burst hundreds of requests at the RPC.
    for (let i = 0; i < busy.length; i += 4) {
      if (options.active && !options.active()) return { txs, newest, oldest, next: before, exhausted };
      const full = await Promise.all(busy.slice(i, i + 4).map((b) => get<BlockView>(`/block/${b.height}`)));
      for (const b of full) txs.push(...[...b.txs].reverse().filter((t) => calls.includes(callOf(t))));
      reads += full.length;
      if (reads >= (options.maxReads ?? 40) && i + 4 < busy.length) {
        // Continue at the first unread header next time; unrelated busy blocks must also count
        // toward the read budget, otherwise a quiet program can flood the RPC with requests.
        const last = full[full.length - 1].height;
        return { txs, newest, oldest: last, next: last, exhausted: false };
      }
    }
    before = oldest;
    if (oldest <= 1) { exhausted = true; break; }
    if (headers[headers.length - 1].height <= (options.after ?? 0)) break;
    if (reads >= (options.maxReads ?? 40)) break;
  }
  return { txs, newest, oldest, next: before, exhausted };
}
