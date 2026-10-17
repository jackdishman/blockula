"use client";

import { useEffect, useRef, useState } from "react";
import { nodeGet } from "./node";
import type { BlockSummary, BlockView, TxDetail } from "./types";

/**
 * The last `size` blocks, kept live: fetched once, then topped up from the head every `every` ms.
 * `born` holds the heights that arrived in the latest top-up, for the ribbon to animate.
 */
export function usePulse(size = 120, every = 1500) {
  const [blocks, setBlocks] = useState<BlockSummary[]>([]);
  const [born, setBorn] = useState<ReadonlySet<number>>(new Set());
  const [error, setError] = useState<Error | null>(null);
  const known = useRef(new Map<number, BlockSummary>());

  useEffect(() => {
    let stop = false;
    const merge = (list: BlockSummary[], animate: boolean) => {
      const fresh: number[] = [];
      for (const b of list) {
        if (!known.current.has(b.height)) fresh.push(b.height);
        known.current.set(b.height, b);
      }
      const all = [...known.current.values()].sort((a, b) => a.height - b.height);
      for (const b of all.slice(0, Math.max(0, all.length - size))) known.current.delete(b.height);
      if (stop) return;
      setBlocks(all.slice(-size));
      if (animate && fresh.length) setBorn(new Set(fresh));
    };
    (async () => {
      try {
        const first = await nodeGet<BlockSummary[]>(`/blocks?limit=${Math.min(100, size)}`);
        merge(first, false);
        const oldest = first[first.length - 1]?.height;
        if (size > 100 && oldest && oldest > 1) merge(await nodeGet<BlockSummary[]>(`/blocks?limit=${size - 100}&before=${oldest}`), false);
      } catch (e) {
        if (!stop) setError(e as Error);
      }
    })();
    const t = setInterval(async () => {
      if (document.visibilityState !== "visible") return;
      try {
        const top = await nodeGet<BlockSummary[]>("/blocks?limit=12");
        // Back from a hidden tab (or a slow poll): fill the gap so the window stays contiguous.
        const newest = Math.max(0, ...known.current.keys());
        let lowest = top[top.length - 1]?.height ?? 0;
        let pages = Math.ceil(size / 100);
        while (newest && lowest > newest + 1 && pages-- > 0) {
          const gap = await nodeGet<BlockSummary[]>(`/blocks?limit=${Math.min(100, lowest - newest - 1)}&before=${lowest}`);
          if (!gap.length) break;
          top.push(...gap);
          lowest = gap[gap.length - 1].height;
        }
        if (newest && lowest > newest + 1) known.current.clear();
        merge(top, true);
        if (!stop) setError(null);
      } catch (e) {
        if (!stop) setError(e as Error);
      }
    }, every);
    return () => {
      stop = true;
      clearInterval(t);
    };
  }, [size, every]);

  return { blocks, born, error };
}

/**
 * The newest transactions in `blocks` (fetching only blocks that have any), plus `further()` to
 * scan back past the window on a quiet chain. Scans stop at `limit` transactions.
 */
export function useRecentTxs(blocks: BlockSummary[], limit = 12) {
  const [txs, setTxs] = useState<TxDetail[]>([]);
  const [scan, setScan] = useState<{ below: number | null; busy: boolean; searched: number }>({ below: null, busy: false, searched: 0 });
  const loaded = useRef(new Map<number, TxDetail[]>());
  const busyKey = blocks
    .filter((b) => b.tx_count > 0)
    .map((b) => b.height)
    .join(",");

  useEffect(() => {
    let stop = false;
    const heights = busyKey ? busyKey.split(",").map(Number).reverse() : [];
    (async () => {
      for (const h of heights.slice(0, limit)) {
        if (loaded.current.has(h)) continue;
        try {
          const b = await nodeGet<BlockView>(`/block/${h}`);
          loaded.current.set(h, b.txs);
        } catch {
          /* skip a block that vanished (history pruned) */
        }
      }
      if (!stop) setTxs(collect(loaded.current, limit));
    })();
    return () => void (stop = true);
  }, [busyKey, limit]);

  /** Look back up to `span` more blocks below the window (or below the last scan). */
  const further = async (span = 600) => {
    const oldest = scan.below ?? blocks[0]?.height;
    if (!oldest || oldest <= 1 || scan.busy) return;
    setScan((s) => ({ ...s, busy: true }));
    let before = oldest;
    let found = 0;
    let searched = 0;
    while (searched < span && before > 1 && found < limit) {
      const page = await nodeGet<BlockSummary[]>(`/blocks?limit=100&before=${before}`).catch(() => [] as BlockSummary[]);
      if (!page.length) break;
      searched += page.length;
      before = page[page.length - 1].height;
      for (const b of page.filter((b) => b.tx_count > 0)) {
        if (found >= limit) break;
        const full = await nodeGet<BlockView>(`/block/${b.height}`).catch(() => null);
        if (full) {
          loaded.current.set(b.height, full.txs);
          found += full.txs.length;
        }
      }
    }
    setTxs(collect(loaded.current, limit));
    setScan((s) => ({ below: before, busy: false, searched: s.searched + searched }));
  };

  const bottom = scan.below ?? blocks[0]?.height ?? null;
  return { txs, further, scanning: scan.busy, searchedBack: scan.searched, exhausted: bottom !== null && bottom <= 1 };
}

function collect(by: Map<number, TxDetail[]>, limit: number) {
  // Keep only the blocks that can still make the list.
  const keep = [...by.keys()].sort((a, b) => b - a).slice(0, limit * 2);
  for (const h of [...by.keys()]) if (!keep.includes(h)) by.delete(h);
  return [...by.entries()]
    .sort((a, b) => b[0] - a[0])
    .flatMap(([, txs]) => [...txs].reverse())
    .slice(0, limit);
}
