"use client";

import { useEffect, useRef, useSyncExternalStore } from "react";
import { mountHaunt } from "@/lib/ascii/haunt";
import { mountProp } from "@/lib/ascii/prop";
import type { PropKind } from "@/lib/ascii/scenes";
import { motionOn, onMotionChange, setUserStill, systemStill } from "@/lib/ascii/settings";

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
  candle: [170, 214],
  bat: [340, 150],
  coin: [180, 166],
  coffin: [340, 180],
  grave: [220, 236],
  cauldron: [230, 224],
  cat: [200, 236],
};

/**
 * A 3D ASCII prop for a page's hero spot. It turns slowly, leans toward the cursor and is lit
 * from it. `busy` spins it (the key while you unlock, the coin when money arrives), `text` is
 * engraved on it (the gravestone), and `burst` sends that many bats flying out when it appears.
 */
export function Apparition({ kind, busy = false, text, burst }: { kind: PropKind; busy?: boolean; text?: string; burst?: number }) {
  const ref = useRef<HTMLCanvasElement>(null);
  const live = useRef({ busy, text });
  const handle = useRef<ReturnType<typeof mountProp> | null>(null);
  useEffect(() => {
    if (!ref.current) return;
    const h = mountProp(ref.current, kind, { busy: () => live.current.busy, text: () => live.current.text, burst });
    handle.current = h;
    return () => {
      h.dispose();
      handle.current = null;
    };
  }, [kind, burst]);
  useEffect(() => {
    live.current = { busy, text };
    handle.current?.redraw();
  }, [busy, text]);
  const [w, h] = SIZE[kind];
  return <canvas ref={ref} className={`apparition apparition-${kind}`} style={{ width: w, aspectRatio: `${w} / ${h}` }} aria-hidden />;
}

/** Footer switch for the animations, remembered in this browser. The OS setting wins. */
export function MotionToggle() {
  const on = useSyncExternalStore(onMotionChange, motionOn, () => true);
  const system = useSyncExternalStore(onMotionChange, systemStill, () => false);
  if (system) return <span className="motion-toggle dim">Animations off (your system asks for reduced motion)</span>;
  return (
    <button type="button" className="link motion-toggle" onClick={() => setUserStill(on)}>
      {on ? "Turn animations off" : "Turn animations on"}
    </button>
  );
}
