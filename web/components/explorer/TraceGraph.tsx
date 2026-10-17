"use client";

import Link from "next/link";
import { useLayoutEffect, useRef, useState } from "react";
import { compactDrops } from "@/lib/explorer/ledger";
import { to } from "@/lib/explorer/routes";
import type { Edge, Trace, TraceNode } from "@/lib/explorer/trace";
import { whoText } from "./kit";

const NODE_W = 176;
const NODE_H = 56;
const COL_W = NODE_W + 108;
const ROW_H = 80;
const PAD = 26;
/** A wide graph shrinks to fit, down to this; past it, it scrolls. */
const MIN_SCALE = 0.72;
/** Past this many edges, only the biggest (and whatever you point at) carry their amount. */
const LABEL_ALL = 16;
const LABEL_TOP = 10;

export const edgeKey = (e: Pick<Edge, "from" | "to">) => `${e.from}>${e.to}`;

interface Geo {
  key: string;
  e: Edge;
  d: string;
  head: string;
  /** Where the amount sits; null when every spot near the line was taken (hover still shows it). */
  label: { x: number; y: number } | null;
  /** Where it may sit, best first. */
  spots: { x: number; y: number }[];
  /** The line's middle, for the hover tip. */
  mid: { x: number; y: number };
  w: number;
}

type Box = { x0: number; y0: number; x1: number; y1: number };
const hits = (a: Box, b: Box) => a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1;

/**
 * The money graph (lib/explorer/trace.ts): accounts as cards on a grid, money as curves whose
 * width is the amount, arrowheads for direction. Pointing at an account colors its money in
 * (blue) and out (red) and fades the rest; clicking one follows it a hop further.
 */
export function TraceGraph({
  trace,
  root,
  me,
  loading,
  failed,
  balances,
  selected,
  onToggle,
  onSelect,
}: {
  trace: Trace;
  root: string;
  me?: string | null;
  loading: ReadonlySet<string>;
  failed: ReadonlySet<string>;
  balances: ReadonlyMap<string, bigint>;
  selected: string | null;
  onToggle: (n: TraceNode) => void;
  onSelect: (key: string | null) => void;
}) {
  const [hoverNode, setHoverNode] = useState<string | null>(null);
  const [hoverEdge, setHoverEdge] = useState<string | null>(null);
  const box = useRef<HTMLDivElement>(null);
  const prevMin = useRef<number | null>(null);
  const [room, setRoom] = useState(0);

  const width = (trace.maxCol - trace.minCol) * COL_W + NODE_W + 2 * PAD;
  const height = (trace.rows - 1) * ROW_H + NODE_H + 2 * PAD;
  const scale = room ? Math.max(MIN_SCALE, Math.min(1, room / width)) : 1;
  const at = new Map(trace.nodes.map((n) => [n.id, { n, x: PAD + (n.col - trace.minCol) * COL_W, y: PAD + n.row * ROW_H }]));

  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setRoom(el.clientWidth));
    ro.observe(el);
    setRoom(el.clientWidth);
    return () => ro.disconnect();
  }, []);

  // Start with the root in view; when a column opens on the left, keep what you were looking at still.
  useLayoutEffect(() => {
    const el = box.current;
    if (!el || !room) return;
    if (prevMin.current == null) el.scrollLeft = (PAD + -trace.minCol * COL_W + NODE_W / 2) * scale - el.clientWidth / 2;
    else if (prevMin.current !== trace.minCol) el.scrollLeft += (prevMin.current - trace.minCol) * COL_W * scale;
    prevMin.current = trace.minCol;
  }, [trace.minCol, room, scale]);

  const max = trace.edges.reduce((m, e) => (e.amount > m ? e.amount : m), 1n);
  const keys = new Set(trace.edges.map(edgeKey));
  const degree = new Map<string, number>();
  for (const e of trace.edges) for (const id of [e.from, e.to]) degree.set(id, (degree.get(id) ?? 0) + 1);

  // Which side of each card an edge leaves and enters: toward the other card's column, or the
  // right side for two cards in one column (the curve bows out to the right).
  const sides = (e: Edge) => {
    const a = at.get(e.from)!, b = at.get(e.to)!;
    return b.n.col > a.n.col ? (["R", "L"] as const) : b.n.col < a.n.col ? (["L", "R"] as const) : (["R", "R"] as const);
  };
  // Ports: edges on one side of a card spread down it, ordered by where they go, so they don't
  // cross at the card, and money both ways between two cards runs in two lanes.
  const ports = new Map<string, { key: string; end: "from" | "to"; toward: number }[]>();
  for (const e of trace.edges) {
    if (!at.has(e.from) || !at.has(e.to)) continue;
    const [sf, st] = sides(e);
    const k = edgeKey(e);
    const push = (id: string, side: string, end: "from" | "to", other: string) => {
      const l = ports.get(`${id}:${side}`) ?? [];
      l.push({ key: k, end, toward: at.get(other)!.y });
      ports.set(`${id}:${side}`, l);
    };
    push(e.from, sf, "from", e.to);
    push(e.to, st, "to", e.from);
  }
  const portY = new Map<string, number>();
  for (const [where, list] of ports) {
    const y0 = at.get(where.slice(0, where.lastIndexOf(":")))!.y;
    list.sort((a, b) => a.toward - b.toward || (a.key < b.key ? -1 : 1));
    list.forEach((p, i) => portY.set(`${p.key}:${p.end}`, y0 + (NODE_H * (i + 1)) / (list.length + 1)));
  }

  const geos: Geo[] = trace.edges.flatMap((e) => {
    const a = at.get(e.from), b = at.get(e.to);
    if (!a || !b) return [];
    const k = edgeKey(e);
    const w = 1.5 + 7 * Math.sqrt(Number((e.amount * 10_000n) / max) / 10_000);
    const ay = portY.get(`${k}:from`)!, by = portY.get(`${k}:to`)!;
    const L = 7 + w * 1.1, half = 3.5 + w * 0.7;
    let sx: number, ex: number, dir: number, bulge = 0;
    if (b.n.col > a.n.col) [sx, ex, dir] = [a.x + NODE_W, b.x, 1];
    else if (b.n.col < a.n.col) [sx, ex, dir] = [a.x, b.x + NODE_W, -1];
    else [sx, ex, dir, bulge] = [a.x + NODE_W, b.x + NODE_W, -1, 64];
    const endX = ex - dir * L;
    const reach = bulge || Math.max(30, Math.abs(endX - sx) * 0.5);
    const p0 = { x: sx, y: ay };
    const c1 = { x: bulge ? sx + bulge : sx + dir * reach, y: ay };
    const c2 = { x: bulge ? endX + bulge : endX - dir * reach, y: by };
    const p3 = { x: endX, y: by };
    const d = `M${p0.x},${p0.y} C${c1.x},${c1.y} ${c2.x},${c2.y} ${p3.x},${p3.y}`;
    const head = `${ex},${by} ${endX},${by - half} ${endX},${by + half}`;
    const at_ = (t: number) => {
      const u = 1 - t;
      const pt = (q: "x" | "y") => u * u * u * p0[q] + 3 * u * u * t * c1[q] + 3 * u * t * t * c2[q] + t * t * t * p3[q];
      return { x: pt("x"), y: pt("y") };
    };
    // The amount prefers the quieter end, where edges have room, above the line (below, for the
    // lower lane of a two-way pair); then other spots along it.
    const t0 = (degree.get(e.from) ?? 0) > (degree.get(e.to) ?? 0) ? 0.84 : (degree.get(e.from) ?? 0) < (degree.get(e.to) ?? 0) ? 0.16 : 0.5;
    const lower = keys.has(`${e.to}>${e.from}`) && k > `${e.to}>${e.from}`;
    const spots = [t0, 0.5, 1 - t0, 0.3, 0.7].flatMap((t) => {
      const p = at_(t);
      const above = { x: p.x, y: p.y - w / 2 - 5 }, below = { x: p.x, y: p.y + w / 2 + 14 };
      return lower ? [below, above] : [above, below];
    });
    return [{ key: k, e, d, head, label: null, spots, mid: at_(0.5), w }];
  });

  // Place amounts biggest first, each in its first spot clear of cards and earlier amounts.
  const taken: Box[] = trace.nodes.map((n) => {
    const p = at.get(n.id)!;
    return { x0: p.x - 2, y0: p.y - 2, x1: p.x + NODE_W + 2, y1: p.y + NODE_H + 2 };
  });
  for (const g of geos) {
    const half = (compactDrops(g.e.amount).length * 7.9 + 6) / 2;
    for (const s of g.spots) {
      const b = { x0: s.x - half, y0: s.y - 13, x1: s.x + half, y1: s.y + 3 };
      if (taken.some((t) => hits(t, b))) continue;
      taken.push(b);
      g.label = s;
      break;
    }
  }
  const top = new Set(geos.slice(0, LABEL_TOP).map((g) => g.key));

  // Colors read from one account's side: the one you point at, else the root.
  const tone = (g: Geo) => {
    if (hoverNode) return g.e.to === hoverNode ? "in" : g.e.from === hoverNode ? "out" : "faded";
    if (hoverEdge) return g.key === hoverEdge ? "on" : "faded";
    if (selected) return g.key === selected ? "on" : "faded";
    return g.e.to === root ? "in rest" : g.e.from === root ? "out rest" : "";
  };
  const hovered = geos.find((g) => g.key === hoverEdge);

  return (
    <div className="trace-canvas" ref={box}>
      <div className="trace-fit" style={{ width: width * scale, height: height * scale }}>
        <div className="trace-plane" style={{ width, height, transform: scale < 1 ? `scale(${scale})` : undefined }} onPointerLeave={() => setHoverEdge(null)}>
          <svg width={width} height={height} className="trace-svg" role="img" aria-label={`Money flows between ${trace.nodes.length} accounts; the list below has the same flows as text.`}>
            {geos.map((g) => {
              const t = tone(g);
              const showLabel = !!g.label && t !== "faded" && (geos.length <= LABEL_ALL || top.has(g.key) || !t.endsWith("rest"));
              return (
                <g key={g.key} className={`tedge ${t}`}>
                  <path d={g.d} className="tline" strokeWidth={g.w} />
                  <polygon points={g.head} className="thead" />
                  {showLabel && (
                    <text x={g.label!.x} y={g.label!.y} className="tlabel" textAnchor="middle">
                      {compactDrops(g.e.amount)}
                    </text>
                  )}
                  <path d={g.d} className="thit" onPointerEnter={() => setHoverEdge(g.key)} onClick={() => onSelect(selected === g.key ? null : g.key)} />
                </g>
              );
            })}
          </svg>
          {trace.nodes.map((n) => {
            const p = at.get(n.id)!;
            const name = whoText(n.who);
            const isRoot = n.id === root;
            const bal = balances.get(n.id);
            const meta = n.holding
              ? "held by a program"
              : isRoot
                ? bal != null ? `start · ${compactDrops(bal)} FANG` : "start here"
                : loading.has(n.id)
                  ? "following…"
                  : failed.has(n.id)
                    ? "couldn't load it"
                    : n.followed
                      ? bal != null ? `${compactDrops(bal)} FANG` : "following"
                      : "follow its money →";
            const cls = ["tnode", isRoot ? "root" : "", n.followed ? "followed" : "", n.holding ? "holding" : "", hoverNode === n.id ? "hover" : "", n.id === me ? "mine" : ""].join(" ");
            const body = (
              <>
                <span className="tname">
                  {name}
                  {n.id === me && <span className="you">you</span>}
                </span>
                <span className="tmeta">{meta}</span>
              </>
            );
            return (
              <div key={n.id} className={cls} style={{ left: p.x, top: p.y, width: NODE_W, height: NODE_H }} onPointerEnter={() => setHoverNode(n.id)} onPointerLeave={() => setHoverNode(null)}>
                {n.holding || isRoot ? (
                  <div className="tbody">{body}</div>
                ) : (
                  <button
                    type="button"
                    className="tbody"
                    aria-pressed={n.followed}
                    aria-label={`${name}: ${n.followed ? "following its money. Press to stop." : "press to follow its money a hop further."}`}
                    onClick={() => onToggle(n)}
                    onFocus={() => setHoverNode(n.id)}
                    onBlur={() => setHoverNode(null)}
                  >
                    {body}
                  </button>
                )}
                {!n.holding && (
                  <Link className="topen" href={to.account(n.who.name && !/\s/.test(n.who.name) ? n.who.name : n.id)} title={`Open ${name}'s page`} aria-label={`Open ${name}'s page`}>
                    ↗
                  </Link>
                )}
              </div>
            );
          })}
          {hovered && (
            <div className="tooltip" style={{ left: hovered.mid.x, top: hovered.mid.y - hovered.w / 2 - 8 }}>
              <b>{compactDrops(hovered.e.amount)} FANG</b>{" "}
              <span className="tl">
                {whoText(at.get(hovered.e.from)?.n.who)} → {whoText(at.get(hovered.e.to)?.n.who)} · {hovered.e.txs.length} tx{hovered.e.txs.length === 1 ? "" : "s"}
              </span>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
