"use client";

import { useEffect, useRef } from "react";
import { mountHaunt } from "@/lib/ascii/haunt";
import { mountProp } from "@/lib/ascii/prop";
import type { PropKind } from "@/lib/ascii/scenes";

/** The ASCII backdrop (moon, fog, jack-o'-lantern, bats) behind every page. */
export function Haunt() {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => (ref.current ? mountHaunt(ref.current) : undefined), []);
  return <canvas ref={ref} className="haunt" aria-hidden />;
}

/** Display size of each prop, CSS px. */
const SIZE: Record<PropKind, [number, number]> = {
  key: [340, 136],
  moon: [200, 190],
  pumpkin: [240, 214],
  candle: [170, 210],
  bat: [340, 150],
};

/**
 * A 3D ASCII prop for a page's hero spot, in place of a flat Ornament. It turns slowly, leans
 * toward the cursor and is lit from it; `busy` spins it (the key turns while you unlock).
 */
export function Apparition({ kind, busy = false }: { kind: PropKind; busy?: boolean }) {
  const ref = useRef<HTMLCanvasElement>(null);
  const busyRef = useRef(busy);
  busyRef.current = busy;
  useEffect(() => (ref.current ? mountProp(ref.current, kind, () => busyRef.current) : undefined), [kind]);
  const [w, h] = SIZE[kind];
  return <canvas ref={ref} className={`apparition apparition-${kind}`} style={{ width: w, aspectRatio: `${w} / ${h}` }} aria-hidden />;
}
