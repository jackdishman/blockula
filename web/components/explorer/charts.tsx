"use client";

import Link from "next/link";
import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { compactDrops, type Flow } from "@/lib/explorer/ledger";
import { to } from "@/lib/explorer/routes";
import type { BlockSummary } from "@/lib/explorer/types";
import { ago, whoText, Who } from "./kit";

/** Width of an element, tracked as it resizes (charts draw at real pixels, so text never scales). */
function useWidth<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [w, setW] = useState(0);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => setW(Math.round(e.contentRect.width)));
    ro.observe(el);
    setW(Math.round(el.getBoundingClientRect().width));
    return () => ro.disconnect();
  }, []);
  return [ref, w] as const;
}

function Tip({ x, y, children }: { x: number; y: number; children: ReactNode }) {
  return (
    <div className="tooltip" style={{ left: x, top: y }} role="status">
      {children}
    </div>
  );
}

/**
 * The pulse: one column per block, oldest left, newest right. Height is the transaction count
 * (square-root scaled so one busy block doesn't flatten the rest); empty blocks are stubs, so a
 * quiet chain still shows its heartbeat. `mine` marks blocks holding your transactions.
 */
export function Ribbon({ blocks: all, mine, here, born, now, caption }: { blocks: BlockSummary[]; mine?: ReadonlySet<number>; here?: number; born?: ReadonlySet<number>; now: number; caption?: (shown: BlockSummary[]) => ReactNode }) {
  const [hover, setHover] = useState<{ b: BlockSummary; x: number } | null>(null);
  const [box, w] = useWidth<HTMLDivElement>();
  // As many of the newest blocks as fit at 4px a column (a phone shows fewer); the block a page
  // is about stays in view.
  const fit = w ? Math.max(8, Math.floor((w + 2) / 6)) : all.length;
  let blocks = all.slice(-fit);
  if (here != null && !blocks.some((b) => b.height === here)) {
    const i = all.findIndex((b) => b.height === here);
    const start = Math.max(0, Math.min(all.length - fit, i - Math.floor(fit / 2)));
    blocks = all.slice(start, start + fit);
  }
  const first = blocks[0], last = blocks[blocks.length - 1];
  // A floor on the scale, so a block with one transaction isn't drawn as a full one.
  const max = Math.max(4, ...blocks.map((b) => b.tx_count));
  const show = (b: BlockSummary, el: HTMLElement) => {
    const r = el.getBoundingClientRect(), p = box.current?.getBoundingClientRect();
    if (p) setHover({ b, x: r.left - p.left + r.width / 2 });
  };
  return (
    <div className="chart" ref={box} onMouseLeave={() => setHover(null)}>
      <div className="ribbon">
        {blocks.map((b) => {
          const h = b.tx_count ? 18 + 82 * Math.sqrt(b.tx_count / max) : 7;
          const cls = ["cell", b.tx_count ? "" : "empty", mine?.has(b.height) ? "mine" : "", here === b.height ? "here" : "", born?.has(b.height) ? "born" : ""].join(" ");
          return (
            <Link
              key={b.height}
              href={to.block(b.height)}
              className={cls}
              aria-label={`Block #${b.height}, ${b.tx_count} transaction${b.tx_count === 1 ? "" : "s"}`}
              onMouseEnter={(e) => show(b, e.currentTarget)}
              onFocus={(e) => show(b, e.currentTarget)}
              onBlur={() => setHover(null)}
            >
              <span style={{ height: `${h}%` }} />
            </Link>
          );
        })}
      </div>
      {first && last && (
        <div className="ribbon-axis">
          <Link href={to.block(first.height)}>#{first.height.toLocaleString("en-US")}</Link>
          {caption && <span className="caption">{caption(blocks)}</span>}
          <Link href={to.block(last.height)}>#{last.height.toLocaleString("en-US")}</Link>
        </div>
      )}
      {hover && (
        <Tip x={hover.x} y={0}>
          <b>#{hover.b.height.toLocaleString("en-US")}</b> · {hover.b.tx_count ? `${hover.b.tx_count} tx${hover.b.tx_count === 1 ? "" : "s"}` : "empty"}
          <div className="tl">
            {whoText(hover.b.proposer)} · {ago(now - hover.b.timestamp_ms)}
          </div>
        </Tip>
      )}
    </div>
  );
}

/**
 * Seconds between consecutive blocks: consensus health at a glance. A hairline marks the target
 * block time; a crosshair finds the nearest block.
 */
export function Cadence({ blocks, targetMs }: { blocks: BlockSummary[]; targetMs: number }) {
  const [ref, w] = useWidth<HTMLDivElement>();
  const [at, setAt] = useState<number | null>(null);
  const pts = blocks.slice(1).map((b, i) => ({ b, s: Math.max(0, (b.timestamp_ms - blocks[i].timestamp_ms) / 1000) }));
  const H = 150, top = 12, bottom = 24, left = 34, right = 8;
  const target = targetMs / 1000;
  const ymax = Math.max(target * 2, Math.ceil(Math.max(...pts.map((p) => p.s), 0) * 1.15));
  const x = (i: number) => left + (pts.length <= 1 ? 0 : (i / (pts.length - 1)) * (w - left - right));
  const y = (s: number) => top + (1 - s / ymax) * (H - top - bottom);
  const path = pts.map((p, i) => `${i ? "L" : "M"}${x(i).toFixed(1)},${y(p.s).toFixed(1)}`).join("");
  const ticks = [0, target, ymax].filter((v, i, a) => a.indexOf(v) === i);
  const avg = pts.length ? pts.reduce((s, p) => s + p.s, 0) / pts.length : 0;
  const onMove = (e: React.PointerEvent) => {
    const r = (e.currentTarget as SVGElement).getBoundingClientRect();
    const i = Math.round(((e.clientX - r.left - left) / Math.max(1, w - left - right)) * (pts.length - 1));
    setAt(Math.max(0, Math.min(pts.length - 1, i)));
  };
  return (
    <div className="chart" ref={ref}>
      {w > 0 && pts.length > 1 && (
        <svg width={w} height={H} onPointerMove={onMove} onPointerLeave={() => setAt(null)} role="img" aria-label={`Block intervals over the last ${pts.length} blocks, averaging ${avg.toFixed(2)} seconds`}>
          {ticks.map((t) => (
            <g key={t}>
              <line className="gridline" x1={left} x2={w - right} y1={y(t)} y2={y(t)} />
              <text className="axis" x={left - 6} y={y(t) + 4} textAnchor="end">
                {t}s
              </text>
            </g>
          ))}
          <path d={path} fill="none" stroke="var(--ink)" strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
          <text className="axis" x={left} y={H - 6}>
            #{pts[0].b.height}
          </text>
          <text className="axis" x={w - right} y={H - 6} textAnchor="end">
            #{pts[pts.length - 1].b.height}
          </text>
          {at != null && (
            <g>
              <line x1={x(at)} x2={x(at)} y1={top} y2={H - bottom} stroke="var(--ink)" strokeWidth={1} />
              <circle cx={x(at)} cy={y(pts[at].s)} r={4.5} fill="var(--blood)" stroke="var(--paper-hi)" strokeWidth={2} />
            </g>
          )}
        </svg>
      )}
      {at != null && (
        <Tip x={x(at)} y={y(pts[at].s)}>
          <b>{pts[at].s.toFixed(2)}s</b> <span className="tl">before #{pts[at].b.height}</span>
        </Tip>
      )}
    </div>
  );
}

/** One series of horizontal bars, value at the tip. */
export function Bars({ rows, format = (v: number) => v.toLocaleString("en-US") }: { rows: { key: string; label: ReactNode; value: number; note?: string }[]; format?: (v: number) => string }) {
  const max = Math.max(1, ...rows.map((r) => r.value));
  return (
    <div className="bars-wrap">
    <div className="bars">
      {rows.map((r) => (
        <div key={r.key} style={{ display: "contents" }} title={r.note}>
          <span className="bar-label">{r.label}</span>
          <span className="bar-track">
            <span className="bar-fill" style={{ display: "block", width: `${(r.value / max) * 100}%` }} />
          </span>
          <span className="bar-value">{format(r.value)}</span>
        </div>
      ))}
    </div>
    </div>
  );
}

const ratio = (a: bigint, max: bigint) => (max === 0n ? 0 : Number((a * 10_000n) / max) / 100);

/**
 * Money in and out by counterparty: what went out grows left from each name (blood), what came
 * in grows right (Prussian blue). Position and the values written at each bar's tip carry the
 * direction, so it reads without color.
 */
export function FlowChart({ rows, me, limit = 8 }: { rows: Flow[]; me?: string | null; limit?: number }) {
  const shown = rows.slice(0, limit);
  const rest = rows.slice(limit);
  const other = rest.length ? { in: rest.reduce((s, r) => s + r.in, 0n), out: rest.reduce((s, r) => s + r.out, 0n), n: rest.length } : null;
  const max = [...shown.map((r) => (r.in > r.out ? r.in : r.out)), other ? (other.in > other.out ? other.in : other.out) : 0n].reduce((a, b) => (a > b ? a : b), 0n);
  const side = (v: bigint, dir: "in" | "out") => (
    <span className={`flow-side ${dir}`}>
      {dir === "out" && v > 0n && <span className="flow-val">−{compactDrops(v)}</span>}
      {v > 0n && <span className="flow-bar" style={{ width: `${Math.max(1.5, ratio(v, max) * 0.78)}%` }} />}
      {dir === "in" && v > 0n && <span className="flow-val">+{compactDrops(v)}</span>}
    </span>
  );
  return (
    <div className="flow" role="table" aria-label="Money in and out by counterparty">
      <span className="flow-head out" role="columnheader">
        Sent to
      </span>
      <span className="flow-head" style={{ textAlign: "center" }} role="columnheader">
        Counterparty
      </span>
      <span className="flow-head" role="columnheader">
        Received from
      </span>
      {shown.map((r) => (
        <div key={r.who.address} role="row" style={{ display: "contents" }}>
          {side(r.out, "out")}
          <span className="flow-who" role="cell" title={`${r.count} transaction${r.count === 1 ? "" : "s"}`}>
            <Who who={r.who} me={me} />
          </span>
          {side(r.in, "in")}
        </div>
      ))}
      {other && (
        <div role="row" style={{ display: "contents" }}>
          {side(other.out, "out")}
          <span className="flow-who dim" role="cell">
            {other.n} others
          </span>
          {side(other.in, "in")}
        </div>
      )}
    </div>
  );
}

/** A budget used against its limit. */
export function Meter({ used, limit, label }: { used: bigint; limit: bigint; label: string }) {
  const pct = limit > 0n ? Math.min(100, ratio(used, limit)) : 0;
  return (
    <div className={`meter ${pct < 50 ? "low" : ""}`} role="meter" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct)} aria-label={label}>
      <span style={{ width: `${pct}%` }} />
    </div>
  );
}
