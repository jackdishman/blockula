"use client";

import { useMemo } from "react";
import { encodeQr, qrPath } from "@/lib/qr";

/** A QR code as inline SVG, black on white with a 4-module quiet zone so any camera can read it. */
export function QrCode({ text, label }: { text: string; label: string }) {
  const { size, d } = useMemo(() => {
    const m = encodeQr(text);
    return { size: m.length + 8, d: qrPath(m, 4) };
  }, [text]);
  return (
    <svg className="qr" viewBox={`0 0 ${size} ${size}`} role="img" aria-label={label} shapeRendering="crispEdges">
      <rect width={size} height={size} fill="#fff" />
      <path d={d} fill="#000" />
    </svg>
  );
}
