"use client";

import Link from "next/link";
import { useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { ChainStatus } from "@/lib/explorer/types";
import { useNow } from "./kit";

/** The chain's pulse in one line, linking to the explorer: "● Block #1,242 · sealed 0.4s ago". */
export function Heartbeat() {
  const status = useNode<ChainStatus>("/status", { every: 2000 });
  const now = useNow(500);
  const s = status.data;
  if (!s) return null;
  // head_age_ms was measured when the status was fetched; keep counting from then.
  const fetched = fetchedAt(s);
  const age = s.head_age_ms != null ? s.head_age_ms + Math.max(0, now - fetched) : null;
  return (
    <Link className="heartbeat" href={to.explore()} title="The pulse: live blocks">
      <span className="live-dot" aria-hidden />
      Block #{s.height.toLocaleString("en-US")}
      {age != null && ` · sealed ${(age / 1000).toFixed(1)}s ago`} · the pulse →
    </Link>
  );
}

const seen = new WeakMap<object, number>();
function fetchedAt(s: object) {
  if (!seen.has(s)) seen.set(s, Date.now());
  return seen.get(s) as number;
}
