"use client";

import { useSyncExternalStore } from "react";
import { RPC } from "../config";

/**
 * Accounts you follow, kept in this browser per node (a name means different accounts on
 * different chains). The explorer lists them on the pulse, and the alerts tell you when they
 * move money.
 */
export interface Watched {
  address: string;
  /** How to show it until the chain answers (its name, or a short address). */
  label: string;
  at: number;
}

export const WATCH_MAX = 12;
const KEY = `blockula.watch:${RPC}`;
const EMPTY: Watched[] = [];
const subs = new Set<() => void>();
let cache: Watched[] | null = null;

function read(): Watched[] {
  if (cache) return cache;
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    cache = Array.isArray(v) ? v.filter((w) => typeof w?.address === "string" && typeof w?.label === "string").slice(0, WATCH_MAX) : [];
  } catch {
    cache = [];
  }
  return cache as Watched[];
}

function write(list: Watched[]) {
  cache = list;
  try {
    localStorage.setItem(KEY, JSON.stringify(list));
  } catch {
    /* storage full or off: it still works for this visit */
  }
  subs.forEach((f) => f());
}

function subscribe(f: () => void) {
  subs.add(f);
  // Another tab changed the list.
  const other = (e: StorageEvent) => {
    if (e.key !== KEY) return;
    cache = null;
    f();
  };
  window.addEventListener("storage", other);
  return () => {
    subs.delete(f);
    window.removeEventListener("storage", other);
  };
}

export function useWatchlist(): Watched[] {
  return useSyncExternalStore(subscribe, read, () => EMPTY);
}

export const isWatched = (address: string) => read().some((w) => w.address === address);

/** Follow or stop following. Returns whether it's watched now (false if the list is full). */
export function toggleWatch(address: string, label: string): boolean {
  const list = read();
  if (list.some((w) => w.address === address)) {
    write(list.filter((w) => w.address !== address));
    return false;
  }
  if (list.length >= WATCH_MAX) return false;
  write([...list, { address, label, at: Date.now() }]);
  return true;
}
