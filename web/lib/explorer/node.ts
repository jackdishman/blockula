"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { RPC } from "../config";

/** An error the node answered with (404 for "no such block", 400 for bad input…). */
export class NodeError extends Error {
  constructor(readonly status: number, message: string, readonly body?: Record<string, unknown>) {
    super(message);
    this.name = "NodeError";
  }
}

// Responses by path, so going back to a page paints at once and then revalidates. Bounded:
// oldest entries go first.
const cache = new Map<string, unknown>();
const CACHE_MAX = 400;
const remember = (path: string, value: unknown) => {
  cache.delete(path);
  cache.set(path, value);
  if (cache.size > CACHE_MAX) cache.delete(cache.keys().next().value as string);
};
const inflight = new Map<string, Promise<unknown>>();

/** GET `/api{path}` from the selected node. Concurrent calls for one path share a request. */
export function nodeGet<T>(path: string): Promise<T> {
  const running = inflight.get(path);
  if (running) return running as Promise<T>;
  const p = (async () => {
    try {
      const res = await fetch(`${RPC}/api${path}`);
      const text = await res.text();
      let json: { error?: string } & Record<string, unknown>;
      try {
        json = JSON.parse(text);
      } catch {
        throw new NodeError(res.status, res.ok ? "the node returned something that isn't JSON" : `node error ${res.status}`);
      }
      if (!res.ok) throw new NodeError(res.status, json.error ?? res.statusText, json);
      remember(path, json);
      return json as T;
    } finally {
      inflight.delete(path);
    }
  })();
  inflight.set(path, p);
  return p;
}

/**
 * Submit a signed transaction and return its id, straight from the node's answer. (Reading the
 * transaction back is the receipt page's job: if that read failed here, a retry would send twice.)
 */
export async function submitTx(tx: unknown): Promise<string> {
  const res = await fetch(`${RPC}/api/tx`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(tx) });
  const json = (await res.json().catch(() => ({}))) as { id?: string; error?: string };
  if (!res.ok || !json.id) throw new NodeError(res.status, json.error ?? `node error ${res.status}`, json);
  return json.id;
}

export const cached = <T,>(path: string) => cache.get(path) as T | undefined;

export interface Live<T> {
  data: T | undefined;
  error: Error | null;
  /** True until the first answer (or error) for this path. */
  loading: boolean;
  reload: () => void;
}

/**
 * Read a node path and keep it fresh. `every` polls (only while the tab is visible), and a
 * refetch keeps showing the last answer, so charts hold their frame instead of flashing.
 * `path = null` waits (e.g. for a dependency).
 */
export function useNode<T>(path: string | null, opts: { every?: number } = {}): Live<T> {
  const [state, setState] = useState<{ path: string | null; data: T | undefined; error: Error | null }>(() => ({
    path,
    data: path ? cached<T>(path) : undefined,
    error: null,
  }));
  const [tick, setTick] = useState(0);
  const live = useRef(path);
  live.current = path;

  // A new path starts from its own cache entry, never the previous path's data.
  if (state.path !== path) setState({ path, data: path ? cached<T>(path) : undefined, error: null });

  useEffect(() => {
    if (!path) return;
    let stop = false;
    const load = () =>
      nodeGet<T>(path).then(
        (data) => !stop && live.current === path && setState({ path, data, error: null }),
        (error: Error) => !stop && live.current === path && setState((s) => ({ ...s, path, error })),
      );
    load();
    if (!opts.every) return () => void (stop = true);
    const t = setInterval(() => document.visibilityState === "visible" && load(), opts.every);
    return () => {
      stop = true;
      clearInterval(t);
    };
  }, [path, opts.every, tick]);

  const reload = useCallback(() => setTick((t) => t + 1), []);
  const current = state.path === path ? state : { data: undefined, error: null };
  return { data: current.data, error: current.error, loading: !!path && current.data === undefined && !current.error, reload };
}
