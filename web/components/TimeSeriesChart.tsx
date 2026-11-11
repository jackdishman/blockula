"use client";

import "@/app/explorer.css";
import { useLayoutEffect, useRef, useState, type PointerEvent } from "react";
import { cssVars } from "@/lib/cssvars";
import { formatMinor, linePath, plotPoints, timeLabel, valueTicks, valueToY, type TimeSeries } from "@/lib/series";

const H = 168;
const BOX = { top: 16, bottom: 22, left: 78, right: 10 };

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

/**
 * A step or sample line for any `TimeSeries`: a balance, or later a pool price. The series
 * arrives already cut to the range on screen. One point draws flat across the plot.
 */
export function TimeSeriesChart({ series, dim = false }: { series: TimeSeries; dim?: boolean }) {
  const [ref, w] = useWidth<HTMLElement>();
  const [at, setAt] = useState<number | null>(null);
  if (!series.points.length) return null;
  const pts = plotPoints(series.points, w, H, BOX);
  const ticks = valueTicks(series.points);
  const min = ticks[0] ?? 0n;
  const max = ticks[ticks.length - 1] ?? min;
  const plotH = H - BOX.top - BOX.bottom;
  const yOf = (v: bigint) => valueToY(v, min, max, BOX.top, plotH);
  const drawn = pts.length === 1 ? [{ x: BOX.left, y: pts[0].y }, { x: Math.max(BOX.left, w - BOX.right), y: pts[0].y }] : pts;
  const path = linePath(drawn);
  const span = series.points[series.points.length - 1].t - series.points[0].t;
  const onMove = (e: PointerEvent<SVGSVGElement>) => {
    if (!pts.length) return;
    const r = e.currentTarget.getBoundingClientRect();
    const x = e.clientX - r.left;
    let best = 0;
    let bestD = Infinity;
    pts.forEach((p, i) => {
      const d = Math.abs(p.x - x);
      if (d < bestD || (d === bestD && i > best)) {
        best = i;
        bestD = d;
      }
    });
    setAt(best);
  };
  const tip = at != null && pts[at] ? Math.min(Math.max(w - 88, 88), Math.max(88, Math.min(w - 88, pts[at].x))) : 0;
  return (
    <figure className={dim ? "chart refetching" : "chart"} ref={ref} data-testid="balance-chart">
      {w > 0 && pts.length > 0 && (
        <svg width={w} height={H} aria-hidden="true" onPointerDown={onMove} onPointerMove={onMove} onPointerLeave={() => setAt(null)} onPointerCancel={() => setAt(null)}>
          {ticks.map((t) => (
            <g key={t.toString()}>
              <line className="gridline" x1={BOX.left} x2={w - BOX.right} y1={yOf(t)} y2={yOf(t)} />
              <text className="axis" x={BOX.left - 8} y={yOf(t) + 4} textAnchor="end">{formatMinor(t, series.scale)}</text>
            </g>
          ))}
          {min < 0n && max > 0n && <line className="baseline" x1={BOX.left} x2={w - BOX.right} y1={yOf(0n)} y2={yOf(0n)} />}
          <path d={path} fill="none" stroke="var(--blood)" strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
          <text className="axis" x={BOX.left} y={H - 6}>{timeLabel(series.points[0].t, span)}</text>
          <text className="axis" x={w - BOX.right} y={H - 6} textAnchor="end">{timeLabel(series.points[series.points.length - 1].t, span)}</text>
          {at != null && pts[at] && (
            <g>
              <line x1={pts[at].x} x2={pts[at].x} y1={BOX.top} y2={H - BOX.bottom} stroke="var(--ink)" strokeWidth={1} />
              <circle cx={pts[at].x} cy={pts[at].y} r={4.5} fill="var(--blood)" stroke="var(--paper-hi)" strokeWidth={2} />
            </g>
          )}
        </svg>
      )}
      {at != null && pts[at] && (
        <div className="tooltip" ref={cssVars({ "--x": tip, "--y": pts[at].y })} role="status">
          <b>{formatMinor(series.points[at].value, series.scale, series.unit)}</b>
          <div className="tl">{timeLabel(series.points[at].t, span)}</div>
        </div>
      )}
      <figcaption className="chart-summary" data-testid="balance-summary">{series.summary}</figcaption>
    </figure>
  );
}
