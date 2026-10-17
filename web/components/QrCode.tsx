"use client";

import { useMemo } from "react";
import { qrSvg } from "@/lib/qr";

/**
 * A QR code as inline SVG, black on white with a 4-module quiet zone so any camera can read it.
 * Text too long for a QR code shows a note instead (the page keeps its copyable link).
 */
export function QrCode({ text, label }: { text: string; label: string }) {
  const qr = useMemo(() => qrSvg(text, 4), [text]);
  if (!qr) {
    return (
      <div className="qr qr-too-long" role="img" aria-label={`${label}: too long for a QR code`} data-testid="qr-too-long">
        Too long for a QR code. Copy the link instead.
      </div>
    );
  }
  return (
    <svg className="qr" viewBox={`0 0 ${qr.size} ${qr.size}`} role="img" aria-label={label} shapeRendering="crispEdges">
      <rect width={qr.size} height={qr.size} fill="#fff" />
      <path d={qr.d} fill="#000" />
    </svg>
  );
}
