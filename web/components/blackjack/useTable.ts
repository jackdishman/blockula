"use client";

// The table's records, read from the node's storage route through the wallet's own API helper
// (lib/explorer/node.ts: the selected node, or the page's origin in the served wallet), and kept
// fresh: every few seconds while the tab is visible, and at once after the page's own transactions.
import { Table, type Game, type House, type Limits, type PlayerRecord, type Rules } from "@blockula/blackjack";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { nodeGet } from "@/lib/explorer/node";

/** How often the table is read. */
export const POLL_MS = 4000;

export interface TableState {
  house: House & { height: number };
  limits: Limits;
  /**
   * The rules the table runs: 1, or 2 (a `blackjack2` table: the dealer opens a hand before its
   * seed is owed, and a hand it never opens is refunded). A table's rules never change.
   */
  rules: Rules;
  /** The dealer has a commitment waiting for the next hand. */
  commitmentReady: boolean;
  /** The watched player's record (`null`: nobody is watched). */
  player: PlayerRecord | null;
  /** The hand on the table: the watched player's open one, or the one a link named. */
  game: Game | null;
}

export interface LiveTable {
  table: Table;
  state: TableState | null;
  /** Why the table can't be read (not a table, the node unreachable). */
  error: string | null;
  reload: () => Promise<void>;
}

/** Reads `address`'s table for `player` (their record and open hand) or for one hand (`gameId`). */
export function useTable(address: string, watch: { player?: string | null; gameId?: bigint | null }): LiveTable {
  const table = useMemo(() => new Table(address, (path) => nodeGet<unknown>(path)), [address]);
  const [state, setState] = useState<TableState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { player, gameId } = watch;
  const run = useRef(0);
  // Read once a table: its rules are fixed when it is created. A read that failed is tried again.
  const rulesOf = useMemo(() => {
    let known: Promise<Rules> | null = null;
    return () =>
      (known ??= table.rules().catch((e) => {
        known = null;
        throw e;
      }));
  }, [table]);

  const reload = useCallback(async () => {
    const mine = ++run.current;
    try {
      const [house, limits, rules] = await Promise.all([table.house(), table.limits(), rulesOf()]);
      const [commitment, record] = await Promise.all([house.nextGame < house.nextCommit ? table.commitment(house.nextGame) : null, player ? table.player(player) : null]);
      const id = gameId ?? record?.openGame ?? null;
      const game = id === null ? null : await table.game(id);
      if (mine !== run.current) return;
      setState({ house, limits, rules, commitmentReady: commitment !== null, player: record, game });
      setError(null);
    } catch (e) {
      if (mine !== run.current) return;
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [table, rulesOf, player, gameId]);

  useEffect(() => {
    setState(null);
    void reload();
    const t = setInterval(() => document.visibilityState === "visible" && void reload(), POLL_MS);
    return () => {
      run.current++;
      clearInterval(t);
    };
  }, [reload]);

  return { table, state, error, reload };
}
