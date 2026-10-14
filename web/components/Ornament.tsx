import type { ReactNode } from "react";

/** Woodcut-style ink silhouettes, used where the app used to put a big emoji. */
export type OrnamentKind = "bat" | "key" | "moon" | "candle";

const WING =
  "M60 21 L62.5 20.5 L65 11 L67.5 20.5 C72 21 76 19.5 80 16.5 C90 9.5 103 6 118 5.5 C113.5 10.5 110.5 16 109.5 24 C104.5 21 99.5 22 96 28.5 C91 25.5 85 27 81.5 34 C76.5 32 71.5 35 68.5 42.5 C65.5 41.5 62.5 43.5 60 48 Z";

const ART: Record<OrnamentKind, { viewBox: string; body: ReactNode }> = {
  bat: {
    viewBox: "0 0 120 52",
    body: (
      <>
        <path d={WING} />
        <path d={WING} transform="translate(120 0) scale(-1 1)" />
      </>
    ),
  },
  key: {
    viewBox: "0 0 120 40",
    body: (
      <>
        <path fillRule="evenodd" d="M20 5a15 15 0 1 1 0 30a15 15 0 1 1 0-30zm0 8a7 7 0 1 0 0 14a7 7 0 1 0 0-14z" />
        <rect x="33" y="17" width="80" height="6" />
        <rect x="36" y="13" width="4" height="14" />
        <rect x="91" y="23" width="6" height="9" />
        <rect x="101" y="23" width="6" height="13" />
        <rect x="109" y="23" width="4" height="7" />
      </>
    ),
  },
  moon: {
    viewBox: "0 0 60 60",
    body: (
      <>
        <path d="M38 6 A25 25 0 1 0 54 42 A20 20 0 0 1 38 6 Z" />
        <path d="M48 12 l1.2 3.3 l3.3 1.2 l-3.3 1.2 l-1.2 3.3 l-1.2 -3.3 l-3.3 -1.2 l3.3 -1.2 z" />
      </>
    ),
  },
  candle: {
    viewBox: "0 0 40 80",
    body: (
      <>
        <path
          fillRule="evenodd"
          d="M20 4 C25 13 26.5 19 23 23.5 C21.5 25.5 18.5 25.5 17 23.5 C13.5 19 15 13 20 4 Z M20 13 C22 17 22.5 20 20.8 22 C20.3 22.6 19.7 22.6 19.2 22 C17.5 20 18 17 20 13 Z"
        />
        <rect x="19.3" y="24" width="1.4" height="6" />
        <path d="M12 30 C14 29 16 31 18 30 C20 29 22 31 24 30 C26 29 27 30 28 30 V72 H12 Z" />
        <path d="M28 30 V42 C28 44.5 25.5 44.5 25.5 42 V31 Z" />
        <path d="M4 72 H36 C36 76 32 78 20 78 C8 78 4 76 4 72 Z" />
      </>
    ),
  },
};

export function Ornament({ kind, className = "" }: { kind: OrnamentKind; className?: string }) {
  const { viewBox, body } = ART[kind];
  return (
    <svg className={`ornament ornament-${kind} ${className}`} viewBox={viewBox} fill="currentColor" aria-hidden focusable="false">
      {body}
    </svg>
  );
}
