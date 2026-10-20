"use client";

import { useEffect, useRef, useState } from "react";
import { callOf, scanActivity } from "./activity";
import { nodeGet, useNode } from "./node";
import type { MempoolView, TxDetail } from "./types";

export function useProgramActivity(calls: readonly string[]) {
  const key = calls.join(",");
  const pending = useNode<MempoolView>("/mempool?limit=200", { every: 5000 });
  const [history, setHistory] = useState<{ txs: TxDetail[]; oldest?: number; newest?: number; exhausted: boolean }>({ txs: [], exhausted: false });
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<Error | null>(null);
  const older = useRef<() => void>(() => {});
  const retry = useRef<() => void>(() => {});

  useEffect(() => {
    let active = true;
    let running = false;
    let head: number | undefined;
    let next: number | undefined;
    const sealed = new Map<string, TxDetail>();
    const kinds = key.split(",");
    setHistory({ txs: [], exhausted: false });
    setLoading(true);
    setError(null);

    const load = async (back = false) => {
      if (running || !active) return;
      running = true;
      setBusy(true);
      try {
        const page = await scanActivity(nodeGet, kinds, {
          before: back ? next : undefined,
          after: back ? undefined : head,
          // On refresh, catch up all the way to the previous head, including after a hidden tab.
          pages: !back && head != null ? Infinity : 5,
          target: !back && head != null ? Infinity : 50,
          maxReads: !back && head != null ? Infinity : 40,
          active: () => active,
        });
        if (!active) return;
        for (const t of page.txs) sealed.set(t.id, t);
        // Stable height sort preserves newest-first execution order within each block.
        const txs = [...sealed.values()].sort((a, b) => (b.height ?? 0) - (a.height ?? 0));
        if (back || head == null) next = page.next;
        head = Math.max(head ?? 0, page.newest ?? 0) || undefined;
        setHistory((h) => ({
          txs,
          oldest: back || h.oldest == null ? page.oldest ?? h.oldest : h.oldest,
          newest: head,
          exhausted: back || h.oldest == null ? page.exhausted : h.exhausted,
        }));
        setError(null);
      } catch (e) {
        if (active) {
          setError(e as Error);
          retry.current = () => void load(back);
        }
      } finally {
        running = false;
        if (active) { setBusy(false); setLoading(false); }
      }
    };
    older.current = () => void load(true);
    void load();
    const timer = setInterval(() => { if (document.visibilityState === "visible") void load(); }, 5000);
    return () => { active = false; clearInterval(timer); };
  }, [key]);

  const sealedIds = new Set(history.txs.map((t) => t.id));
  const waiting = (pending.data?.txs ?? []).filter((t) => calls.includes(callOf(t)) && !sealedIds.has(t.id));
  return {
    ...history, txs: [...waiting, ...history.txs], loading, busy, error,
    pendingError: pending.error,
    loadOlder: () => older.current(), retry: () => retry.current(),
  };
}
