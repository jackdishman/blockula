/**
 * Following the money: who paid whom, drawn outward from one account a hop at a time.
 * Pure, so `node --test` runs it directly.
 *
 * Every edge is a transaction's own visible effects (`effectsOf`), so a trace shows what the
 * receipts show and nothing more. The node serves each account's latest transactions, so the
 * graph covers each followed account's recent window, not its whole history. Pages say so.
 *
 * Layout reads left to right, the way money moves: whoever mostly paid an account sits a column
 * to its left, whoever it mostly paid sits a column to its right. Each new node takes the free
 * row nearest the node it was found through, biggest flows first, so neighbors fan out around
 * their source.
 */
import type { Label } from "../types";
import { effectsOf, type Holding } from "./ledger.ts";
import type { TxDetail } from "./types";

export type Who = Label | Holding;

/** FANG one transaction moved from one party to another (fees aside). */
export interface Move {
  tx: string;
  from: string;
  to: string;
  amount: bigint;
  height: number | null;
}

export interface Edge {
  from: string;
  to: string;
  amount: bigint;
  /** Transaction ids, newest first. */
  txs: string[];
}

export interface TraceNode {
  id: string;
  who: Who;
  /** Columns left of the root (negative) paid toward it; right of it (positive) were paid. */
  col: number;
  row: number;
  /** Hops from the root. */
  depth: number;
  holding: boolean;
  /** Following it: its transactions are part of the graph. */
  followed: boolean;
}

export interface Trace {
  nodes: TraceNode[];
  edges: Edge[];
  /** Inclusive column range and row count, for sizing the drawing. */
  minCol: number;
  maxCol: number;
  rows: number;
  /** Transactions the graph is built from, and the lowest height among them. */
  counted: number;
  since: number | null;
}

const isHolding = (w: Who): w is Holding => "holding" in w;

/** What each sealed transaction moved, sender to each recipient. Failed ones only burned a fee. */
export function movesOf(txs: Iterable<TxDetail>): { moves: Move[]; who: Map<string, Who> } {
  const moves: Move[] = [];
  const who = new Map<string, Who>();
  const know = (w: Who | null | undefined) => {
    if (!w) return;
    const had = who.get(w.address);
    // Keep the best label: a name beats a bare address.
    if (!had || (!had.name && w.name)) who.set(w.address, w);
  };
  for (const tx of txs) {
    if (tx.status !== "finalized" || !tx.from) continue;
    know(tx.from);
    know(tx.to);
    for (const c of effectsOf(tx).changes) {
      know(c.who);
      if (c.who.address === tx.from.address) continue;
      // Whoever gained was paid by the sender; a holding that lost (a distribution's claim) paid it.
      if (c.delta > 0n) moves.push({ tx: tx.id, from: tx.from.address, to: c.who.address, amount: c.delta, height: tx.height });
      else if (c.delta < 0n) moves.push({ tx: tx.id, from: c.who.address, to: tx.from.address, amount: -c.delta, height: tx.height });
    }
  }
  return { moves, who };
}

/** Moves summed per direction (A→B and B→A are separate edges), biggest first. */
export function edgesOf(moves: Move[]): Edge[] {
  const by = new Map<string, Edge & { last: number }>();
  for (const m of moves) {
    const k = `${m.from}>${m.to}`;
    const e = by.get(k) ?? { from: m.from, to: m.to, amount: 0n, txs: [], last: -1 };
    e.amount += m.amount;
    e.txs.push(m.tx);
    e.last = Math.max(e.last, m.height ?? -1);
    by.set(k, e);
  }
  return [...by.values()].sort((a, b) => (b.amount > a.amount ? 1 : b.amount < a.amount ? -1 : b.last - a.last)).map(({ last: _last, ...e }) => e);
}

/**
 * The trace around `root`, built from the transactions of every followed account (`history`:
 * address → its recent transactions; the root is always followed). Accounts only reachable
 * through an account that's no longer followed drop out.
 */
export function buildTrace(root: string, history: ReadonlyMap<string, TxDetail[]>): Trace {
  const seen = new Set<string>();
  const txs: TxDetail[] = [];
  for (const list of history.values())
    for (const t of list)
      if (!seen.has(t.id)) {
        seen.add(t.id);
        txs.push(t);
      }
  const { moves, who } = movesOf(txs);
  const edges = edgesOf(moves);

  // Undirected neighbors with the volume each way, biggest first.
  const flow = new Map<string, bigint>();
  for (const e of edges) flow.set(`${e.from}>${e.to}`, e.amount);
  const near = new Map<string, string[]>();
  const link = (a: string, b: string) => {
    const l = near.get(a) ?? [];
    if (!l.includes(b)) l.push(b);
    near.set(a, l);
  };
  for (const e of edges) {
    link(e.from, e.to);
    link(e.to, e.from);
  }
  const volume = (a: string, b: string) => (flow.get(`${a}>${b}`) ?? 0n) + (flow.get(`${b}>${a}`) ?? 0n);

  // Breadth-first from the root. Holdings (escrow) are leaves: they aren't accounts to follow.
  const rootWho: Who = who.get(root) ?? { address: root, name: null, short: `${root.slice(0, 10)}…${root.slice(-6)}` };
  const nodes = new Map<string, TraceNode>([[root, { id: root, who: rootWho, col: 0, row: 0, depth: 0, holding: false, followed: true }]]);
  const taken = new Map<number, Set<number>>([[0, new Set([0])]]);
  let frontier = [root];
  while (frontier.length) {
    const next: string[] = [];
    // Place this hop's discoveries in the order of their parents, then by volume.
    for (const parent of frontier) {
      const p = nodes.get(parent)!;
      if (p.holding || !history.has(parent)) continue;
      const kids = (near.get(parent) ?? []).filter((n) => !nodes.has(n)).sort((a, b) => cmp(volume(parent, b), volume(parent, a)));
      for (const id of kids) {
        const w = who.get(id) ?? { address: id, name: null, short: id };
        // Mostly paid the parent: upstream (left). Mostly paid by it: downstream (right).
        const col = p.col + ((flow.get(`${id}>${parent}`) ?? 0n) > (flow.get(`${parent}>${id}`) ?? 0n) ? -1 : 1);
        const row = freeRow(taken, col, p.row);
        nodes.set(id, { id, who: w, col, row, depth: p.depth + 1, holding: isHolding(w), followed: history.has(id) });
        next.push(id);
      }
    }
    frontier = next;
  }

  const placed = [...nodes.values()];
  const minRow = Math.min(...placed.map((n) => n.row));
  for (const n of placed) n.row -= minRow;
  const cols = placed.map((n) => n.col);
  const shown = new Set(nodes.keys());
  const kept = edges.filter((e) => shown.has(e.from) && shown.has(e.to));
  const heights = txs.filter((t) => t.status === "finalized" && t.height != null).map((t) => t.height as number);
  return {
    nodes: placed,
    edges: kept,
    minCol: Math.min(...cols),
    maxCol: Math.max(...cols),
    rows: Math.max(...placed.map((n) => n.row)) + 1,
    counted: txs.length,
    since: heights.length ? Math.min(...heights) : null,
  };
}

/** The free row in `col` nearest `want`: want, want+1, want−1, want+2… */
function freeRow(taken: Map<number, Set<number>>, col: number, want: number): number {
  const used = taken.get(col) ?? new Set<number>();
  taken.set(col, used);
  for (let d = 0; ; d++) {
    for (const r of d ? [want + d, want - d] : [want]) {
      if (!used.has(r)) {
        used.add(r);
        return r;
      }
    }
  }
}

const cmp = (a: bigint, b: bigint) => (a > b ? 1 : a < b ? -1 : 0);

/** How many accounts a trace follows besides its root. */
export const FOLLOW_MAX = 12;

/** Followed accounts in a shareable form (`?follow=a,b`), the root left out. */
export const followParam = (followed: string[], root: string) => followed.filter((a) => a !== root).join(",");
export const parseFollow = (s: string | null) => (s ? s.split(",").map((x) => x.trim()).filter(Boolean).slice(0, FOLLOW_MAX) : []);
