"use client";

import { useEffect, useRef, useState } from "react";
import { TimeSeriesChart } from "@/components/TimeSeriesChart";
import { Spinner } from "@/components/ui";
import { addressBytes, hexOf } from "@/lib/address";
import {
  accountOnlyMoves,
  creationFeeOf,
  decodeBalance,
  fangSeries,
  mergeTokenMoves,
  programsTouched,
  readHistory,
  tokenMoves,
  tokenSeries,
  type HistoryRead,
} from "@/lib/balance-series";
import { friendly } from "@/lib/errors";
import type { TxDetail } from "@/lib/explorer/types";
import { chainUnit, shortAddr } from "@/lib/format";
import { RANGES, sliceSeries, type BlockClock, type RangeId, type TimeSeries } from "@/lib/series";
import { useUnlocked } from "@/lib/wallet";

const dropsOf = (s: string | null | undefined) => (s && /^\d+$/.test(s) ? BigInt(s) : 0n);

/** `/api/status` reports how old the head is; the wallet's status type doesn't always carry it. */
const headAge = (st: object) => {
  const v = "head_age_ms" in st ? st.head_age_ms : 0;
  return typeof v === "number" && Number.isFinite(v) ? Math.max(0, v) : 0;
};

/**
 * The signed-in account's balance over time: FANG from its transactions, then each program token
 * those transactions name. The chart itself only sees a `TimeSeries`.
 */
export function BalanceHistory() {
  const { account, status, client } = useUnlocked();
  const address = account?.account.address ?? "";
  const balance = account?.balance ?? "0";
  const nonce = account?.next_nonce ?? 0;
  const statusRef = useRef(status);
  statusRef.current = status;
  const [attempt, setAttempt] = useState(0);
  const [range, setRange] = useState<RangeId>("all");
  const [picked, setPicked] = useState("native");
  const [state, setState] = useState<
    | { address: string; phase: "loading" }
    | { address: string; phase: "error"; message: string }
    | { address: string; phase: "ready"; series: TimeSeries[]; now: number; busy: boolean }
    | null
  >(null);
  const gen = useRef(0);
  const clockReady = status != null;

  useEffect(() => {
    const st = statusRef.current;
    if (!address || !st) return;
    const n = ++gen.current;
    const live = () => gen.current === n;
    const clock: BlockClock = {
      head: st.height,
      blockTimeMs: st.block_time_ms || 1000,
      now: Date.now(),
      headAgeMs: headAge(st),
    };
    setState((s) => (s && s.address === address && s.phase === "ready" ? { ...s, busy: true } : { address, phase: "loading" }));
    const run = async () => {
      const [hist, fees] = await Promise.all([
        readHistory((page) => client.history(address, page)),
        client.fees().catch(() => null),
      ]);
      if (!live()) return;
      const fang = fangSeries({
        me: address,
        balance: dropsOf(balance),
        unit: chainUnit(),
        txs: hist.txs,
        clock,
        creationFee: creationFeeOf(fees),
        truncated: hist.truncated,
        historyFrom: hist.historyFrom,
      });
      setState({ address, phase: "ready", series: [fang], now: clock.now, busy: false });
      const tokens = await loadTokens(client, address, hist.txs, clock);
      if (!live()) return;
      setState({ address, phase: "ready", series: [fang, ...tokens], now: clock.now, busy: false });
    };
    run().catch((e: unknown) => {
      if (live()) setState({ address, phase: "error", message: friendly(e) });
    });
    return () => {
      gen.current += 1;
    };
    // The clock is read from the latest status, but a new block alone doesn't reload history.
  }, [address, balance, nonce, attempt, client, clockReady]);

  const mine = state && state.address === address ? state : null;
  const ready = mine && mine.phase === "ready" ? mine : null;
  const shown = ready?.series.find((s) => s.id === picked) ?? ready?.series[0] ?? null;
  const visible = shown && ready ? sliceSeries(shown, range, ready.now) : null;

  return (
    <section className="card balance-history" data-testid="balance-history" aria-busy={!ready || ready.busy}>
      <div className="row-between">
        <h2>Over time</h2>
        {ready && (
          <div className="filters" role="group" aria-label="Time range">
            {RANGES.map((r) => (
              <button key={r.id} type="button" className="filter" aria-pressed={range === r.id} data-testid={`range-${r.id}`} onClick={() => setRange(r.id)}>
                {r.label}
              </button>
            ))}
          </div>
        )}
      </div>
      {mine?.phase === "error" && (
        <p className="error" role="alert" data-testid="balance-error">
          Couldn&apos;t read balance history. {mine.message}{" "}
          <button type="button" className="link" data-testid="balance-retry" onClick={() => setAttempt((a) => a + 1)}>Retry</button>
        </p>
      )}
      {!mine || mine.phase === "loading" ? <Spinner>Reading balance history…</Spinner> : null}
      {ready && shown && (
        <>
          {ready.series.length > 1 && (
            <div className="filters" role="group" aria-label="Which balance">
              {ready.series.map((s) => (
                <button
                  key={s.id}
                  type="button"
                  className="filter"
                  aria-pressed={s.id === shown.id}
                  aria-label={s.id === "native" ? s.label : `Token ${s.detail ?? s.label}`}
                  data-testid={s.id === "native" ? "series-native" : "series-token"}
                  onClick={() => setPicked(s.id)}
                >
                  {s.label}
                </button>
              ))}
            </div>
          )}
          {visible && visible.points.length > 0 ? <TimeSeriesChart series={visible} dim={ready.busy} /> : <p className="dim" data-testid="balance-summary">{visible?.summary}</p>}
          {shown.detail && <p className="mono small dim" data-testid="series-detail">{shown.detail}</p>}
        </>
      )}
    </section>
  );
}

/** A receipt that left events out, on a call or event that names this program. */
function omittedTouching(txs: readonly TxDetail[], program: string): boolean {
  return txs.some((tx) => {
    if (tx.program?.outcome?.ok !== true || (tx.program.claims?.omitted?.events ?? 0) <= 0) return false;
    if ((tx.program.claims.events ?? []).some((e) => e.program === program)) return true;
    const calls = tx.action?.type === "call_program" && Array.isArray(tx.action.calls) ? tx.action.calls : [];
    return calls.some((c) => !!c && typeof c === "object" && (c as { program?: unknown }).program === program);
  });
}

/** Token lines for programs this account's transactions name. One failure skips that program. */
async function loadTokens(
  client: { history: (id: string, page: { before?: string; limit?: number }) => Promise<{ txs?: unknown; next?: unknown; paged?: unknown; history_from?: unknown }>; view: (address: string, method: string, input: Uint8Array) => Promise<{ return_data: string | null }> },
  me: string,
  accountTxs: TxDetail[],
  clock: BlockClock,
): Promise<TimeSeries[]> {
  const bytes = addressBytes(me);
  if (!bytes) return [];
  const meKey = hexOf(bytes);
  const programs = programsTouched(accountTxs);
  const own = tokenMoves(accountTxs, meKey, clock);
  const found = new Map<string, TimeSeries>();
  await Promise.all(programs.map(async (program) => {
    let hist: HistoryRead | null = null;
    try {
      hist = await readHistory((page) => client.history(program, page), { maxPages: 4 });
    } catch {
      hist = null;
    }
    const fromProgram = hist ? tokenMoves(hist.txs, meKey, clock) : { moves: [], omitted: false };
    const moves = mergeTokenMoves([fromProgram.moves, accountOnlyMoves(own.moves.filter((m) => m.program === program), fromProgram.moves)]);
    // A program the account called isn't a token until a transfer event says so. Asking every one
    // for balance_of would 404, and the chain has nothing to chart.
    if (!moves.length) return;
    let balance: bigint | null = null;
    try {
      balance = decodeBalance((await client.view(program, "balance_of", bytes)).return_data);
    } catch {
      balance = null;
    }
    const line = tokenSeries({
      program,
      label: shortAddr(program),
      moves,
      balance,
      now: clock.now,
      complete: hist != null && !hist.truncated && (hist.historyFrom == null || hist.historyFrom <= 1),
      truncated: hist == null || hist.truncated,
      omitted: fromProgram.omitted || omittedTouching(accountTxs, program),
    });
    if (line) found.set(program, line);
  }));
  return programs.flatMap((p) => {
    const line = found.get(p);
    return line ? [line] : [];
  });
}
