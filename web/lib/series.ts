import { compactDrops } from "./explorer/ledger.ts";

/**
 * A line a chart can draw without knowing what it measures.
 *
 * Balance history builds one series per coin the account holds: FANG, from the account
 * balance and its transactions, and each program token whose transfers show up in that
 * history. A later change can build the same shape from an AMM pool's swap prices (a pinned
 * pool such as BLOOD/FANG) and pass it to `TimeSeriesChart`. `value` stays an integer in
 * `scale` minor units (drops for FANG). The chart turns that into pixels only while drawing.
 */

/** One vertex. `value` is an integer in the series' minor units. Never a float. */
export interface SeriesPoint {
  /** Unix milliseconds. */
  t: number;
  value: bigint;
}

/** A labeled line: a balance, or later a pool price. Oldest vertex first. */
export interface TimeSeries {
  id: string;
  /** What to call it. "FANG", or the short address of a program. */
  label: string;
  /** Word after an amount. Empty when the series has no unit of its own. */
  unit: string;
  /** Minor units in one whole unit. 1_000_000_000n for FANG drops; 1n for a raw integer. */
  scale: bigint;
  points: SeriesPoint[];
  /** A step was inexact, or the line is only a sketch of what the history shows. */
  approximate: boolean;
  /** Shown under the chart, and the chart's accessible name. */
  summary: string;
  /** Appended to a drawn line's summary. Null when there's nothing to add. */
  note: string | null;
  /** A second line under the summary, such as a program's full address. */
  detail?: string;
}

export const RANGES = [
  { id: "7d", label: "7d", ms: 7 * 86_400_000 },
  { id: "30d", label: "30d", ms: 30 * 86_400_000 },
  { id: "all", label: "All", ms: null },
] as const;

export type RangeId = (typeof RANGES)[number]["id"];

const DAY = 86_400_000;

/** The chain's clock, so a block height becomes a time without asking for every block. */
export interface BlockClock {
  /** The node's height when `now` was read. */
  head: number;
  blockTimeMs: number;
  now: number;
  /** How old that head was, in milliseconds, at `now`. */
  headAgeMs?: number;
}

/** When a block at `height` was sealed, on this clock. Never later than `now`. */
export function timeAtHeight(height: number, clock: BlockClock): number {
  const blockMs = Number.isFinite(clock.blockTimeMs) && clock.blockTimeMs > 0 ? clock.blockTimeMs : 1000;
  const headAt = clock.now - (Number.isFinite(clock.headAgeMs) ? Math.max(0, clock.headAgeMs ?? 0) : 0);
  const t = headAt - (clock.head - height) * blockMs;
  return Math.min(clock.now, t);
}

const rangeOf = (id: RangeId) => RANGES.find((r) => r.id === id) ?? RANGES[2];

/**
 * The part of `series` inside the range, ending at `now`. The balance in effect at the
 * window's start is carried in, so a quiet week still draws the balance it held.
 */
export function sliceSeries(series: TimeSeries, range: RangeId, now: number): TimeSeries {
  if (range === "all" || !series.points.length) return series;
  const ms = rangeOf(range).ms;
  if (ms == null) return series;
  const start = now - ms;
  const pts = series.points;
  let i = 0;
  while (i < pts.length && pts[i].t < start) i++;
  const points: SeriesPoint[] = [];
  if (i > 0) points.push({ t: start, value: pts[i - 1].value });
  for (const p of pts.slice(i)) if (p.t <= now) points.push(p);
  if (points.length === 1 && points[0].t < now) points.push({ t: now, value: points[0].value });
  const next: TimeSeries = { ...series, points };
  next.summary = points.length ? describeSeries(next, range) : series.summary;
  return next;
}

/** One sentence for the line on screen. Amounts stay integers until they're formatted. */
export function describeSeries(series: TimeSeries, range: RangeId = "all"): string {
  const pts = series.points;
  if (!pts.length) return series.summary;
  const span = range === "7d" ? "the last 7 days" : range === "30d" ? "the last 30 days" : "this history";
  let high = pts[0].value;
  let low = pts[0].value;
  let changed = false;
  for (let i = 0; i < pts.length; i++) {
    const v = pts[i].value;
    if (v > high) high = v;
    if (v < low) low = v;
    if (i > 0 && v !== pts[i - 1].value) changed = true;
  }
  const amount = (v: bigint) => formatMinor(v, series.scale, series.unit);
  const who = series.unit ? "The balance" : series.label;
  const line = changed
    ? `${who} went from ${amount(pts[0].value)} to ${amount(pts[pts.length - 1].value)} over ${span}. Highest ${amount(high)}, lowest ${amount(low)}.`
    : `${who} stayed at ${amount(pts[pts.length - 1].value)} over ${span}.`;
  return series.note ? `${line} ${series.note}` : line;
}

/**
 * An integer amount for reading. `scale` minor units make one whole unit. FANG (scale 10^9)
 * uses the ledger's drop formatting; anything else is grouped whole units plus at most four
 * decimal places, cut off rather than rounded with a float.
 */
export function formatMinor(value: bigint, scale: bigint, unit = ""): string {
  const s = scale > 0n ? scale : 1n;
  if (s === 1_000_000_000n) {
    const body = compactDrops(value);
    return unit ? `${body} ${unit}` : body;
  }
  const neg = value < 0n;
  const v = neg ? -value : value;
  const whole = v / s;
  let text = `${neg && v !== 0n ? "-" : ""}${whole.toLocaleString("en-US")}`;
  const frac = v % s;
  if (frac !== 0n && s > 1n) {
    const digits = s.toString().length - 1;
    const raw = frac.toString().padStart(digits, "0").slice(0, 4).replace(/0+$/, "");
    if (raw) text += `.${raw}`;
    else if (v !== 0n) text = `${neg ? "-" : ""}< 0.0001`;
  }
  return unit ? `${text} ${unit}` : text;
}

export interface PlotBox {
  top: number;
  bottom: number;
  left: number;
  right: number;
}

/** Pixel y of `value` inside the plot. Higher value sits higher. Bigint until the pixel. */
export function valueToY(value: bigint, min: bigint, max: bigint, plotTop: number, plotH: number): number {
  if (max === min) return plotTop + plotH / 2;
  const span = max - min;
  const milli = BigInt(Math.max(0, Math.round(plotH * 1000)));
  const fromBottom = Number(((value - min) * milli) / span) / 1000;
  return plotTop + plotH - fromBottom;
}

/** The vertices in pixel space. An empty width or an empty series draws nothing. */
export function plotPoints(points: readonly SeriesPoint[], width: number, height: number, box: PlotBox): { x: number; y: number }[] {
  if (width <= 0 || points.length === 0) return [];
  const t0 = points[0].t;
  const t1 = points[points.length - 1].t;
  const spanT = t1 - t0;
  let min = points[0].value;
  let max = points[0].value;
  for (const p of points) {
    if (p.value < min) min = p.value;
    if (p.value > max) max = p.value;
  }
  const plotH = Math.max(0, height - box.top - box.bottom);
  const plotW = Math.max(0, width - box.left - box.right);
  return points.map((p) => ({
    x: box.left + (spanT === 0 ? plotW / 2 : ((p.t - t0) / spanT) * plotW),
    y: valueToY(p.value, min, max, box.top, plotH),
  }));
}

/** Min, midpoint and max, for axis labels. One tick when the line is flat. */
export function valueTicks(points: readonly SeriesPoint[]): bigint[] {
  if (!points.length) return [];
  let min = points[0].value;
  let max = points[0].value;
  for (const p of points) {
    if (p.value < min) min = p.value;
    if (p.value > max) max = p.value;
  }
  if (min === max) return [min];
  const mid = min + (max - min) / 2n;
  return mid === min || mid === max ? [min, max] : [min, mid, max];
}

/** An SVG path through pixel vertices. */
export function linePath(pts: readonly { x: number; y: number }[]): string {
  return pts.map((p, i) => `${i ? "L" : "M"}${p.x.toFixed(1)},${p.y.toFixed(1)}`).join("");
}

/** A short time or date, depending on how wide the line's span is. */
export function timeLabel(t: number, spanMs: number): string {
  const d = new Date(t);
  if (spanMs < 36 * 3_600_000) return d.toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" });
  if (spanMs < 400 * DAY) return d.toLocaleDateString("en-US", { month: "short", day: "numeric" });
  return d.toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
}
