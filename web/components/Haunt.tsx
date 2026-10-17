"use client";

import { usePathname } from "next/navigation";
import { useEffect, useRef, useSyncExternalStore } from "react";
import { mountHaunt } from "@/lib/ascii/haunt";
import { isExplorerPath } from "@/lib/explorer/routes";
import { mountProp } from "@/lib/ascii/prop";
import type { PropKind } from "@/lib/ascii/scenes";
import { motionOn, onMotionChange, setUserStill, systemStill } from "@/lib/ascii/settings";

/** The ASCII backdrop (moon, fog, jack-o'-lantern, bats) behind every page. */
export function Haunt() {
  const ref = useRef<HTMLCanvasElement>(null);
  // Explorer pages use a wider column (app/explorer.css .wide); keep the props in the margins beside it.
  const wide = isExplorerPath(usePathname());
  useEffect(() => (ref.current ? mountHaunt(ref.current, { column: wide ? 1080 : 580 }) : undefined), [wide]);
  return <canvas ref={ref} className="haunt" aria-hidden />;
}

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
  // Each kind's display size is in globals.css (.apparition-<kind>): the CSP refuses inline style attributes.
  return <canvas ref={ref} className={`apparition apparition-${kind}`} aria-hidden />;
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
