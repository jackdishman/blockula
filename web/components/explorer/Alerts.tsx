"use client";

import "@/app/explorer.css";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { useCallback, useEffect, useRef, useState } from "react";
import { compactDrops, deltaFor, fromMySide, parseDrops, type Me } from "@/lib/explorer/ledger";
import { nodeGet } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { AccountDetail, TxDetail } from "@/lib/explorer/types";
import { useWatchlist } from "@/lib/explorer/watch";
import { useWallet } from "@/lib/wallet";
import { Amount, useMe, whoText } from "./kit";
import { signerOf } from "./Ledger";

interface Notice {
  key: string;
  tx: string;
  title: string;
  body: string;
  /** Signed from the side the notice is about. */
  delta: bigint | null;
  tone: "in" | "out" | "agent" | "failed" | "";
  meta?: string;
}

const EVERY = 4000;
/** While the tab is hidden, only your own account is checked, this much less often. */
const HIDDEN_EVERY = 4;
const SHOW_MS = 9000;
const MAX_SHOWN = 4;

/**
 * Notices for money that moves while you're anywhere in the app: payments to you, spending by
 * your agents and other devices, your own transactions sealing, and activity on accounts you
 * watch. Read from the chain (works while locked), starting from what's already there: only
 * news is announced.
 */
export function Alerts() {
  const me = useMe();
  const { hint, wallet } = useWallet();
  const watch = useWatchlist();
  const path = usePathname();
  const [shown, setShown] = useState<Notice[]>([]);
  const seen = useRef(new Map<string, Map<string, string>>());
  const told = useRef(new Set<string>());
  const pathRef = useRef(path);
  pathRef.current = path;
  const unread = useRef(0);
  const title = useRef<string | null>(null);

  const myAddr = me?.address ?? null;
  const meRef = useRef(me);
  meRef.current = me;
  const myKey = wallet?.keyId ?? hint?.keyId;
  const followed = [...(myAddr ? [myAddr] : []), ...watch.map((w) => w.address).filter((a) => a !== myAddr)];
  const followKey = followed.join(",");

  const dismiss = useCallback((key: string) => setShown((s) => s.filter((n) => n.key !== key)), []);

  useEffect(() => {
    if (!followKey) return;
    const list = followKey.split(",");
    let tick = 0;
    let stop = false;

    const announce = (n: Notice) => {
      setShown((s) => [n, ...s.filter((x) => x.key !== n.key)].slice(0, MAX_SHOWN));
      if (document.visibilityState === "hidden") {
        unread.current++;
        title.current ??= document.title;
        document.title = `(${unread.current}) ${title.current}`;
      }
    };

    const read = (addr: string, a: AccountDetail) => {
      const before = seen.current.get(addr);
      const now = new Map(a.recent_txs.map((t) => [t.id, t.status] as const));
      seen.current.set(addr, now);
      // The first look is the baseline: what's already there isn't news.
      if (!before) return;
      for (const t of [...a.recent_txs].reverse()) {
        const was = before.get(t.id);
        if (t.status === "pending" || (was && was !== "pending") || told.current.has(t.id)) continue;
        const n = describe(t, addr, a, meRef.current, myKey, pathRef.current);
        // Even when it isn't worth a notice, a transaction is only ever considered once.
        told.current.add(t.id);
        if (n) announce(n);
      }
    };

    let busy = false;
    const poll = async () => {
      tick++;
      const hidden = document.visibilityState === "hidden";
      // A slow node: let the last round finish rather than stack another on it.
      if (busy || (hidden && tick % HIDDEN_EVERY)) return;
      busy = true;
      try {
        for (const addr of hidden ? list.slice(0, myAddr ? 1 : 0) : list) {
          if (stop) return;
          const a = await nodeGet<AccountDetail>(`/account/${addr}`).catch(() => null);
          if (a && !stop) read(addr, a);
        }
      } finally {
        busy = false;
      }
    };
    poll();
    const t = setInterval(poll, EVERY);
    const back = () => {
      if (document.visibilityState !== "visible" || title.current == null) return;
      document.title = title.current;
      title.current = null;
      unread.current = 0;
    };
    document.addEventListener("visibilitychange", back);
    return () => {
      stop = true;
      clearInterval(t);
      document.removeEventListener("visibilitychange", back);
    };
  }, [followKey, myAddr, myKey]);

  if (!shown.length) return <div className="notices" aria-live="polite" />;
  return (
    <div className="notices" aria-live="polite">
      {shown.map((n) => (
        <NoticeCard key={n.key} n={n} onClose={dismiss} />
      ))}
    </div>
  );
}

/** `onClose` must be stable: the wallet re-renders every few seconds, which mustn't restart the timer. */
function NoticeCard({ n, onClose }: { n: Notice; onClose: (key: string) => void }) {
  const [held, setHeld] = useState(false);
  useEffect(() => {
    if (held) return;
    const t = setTimeout(() => onClose(n.key), SHOW_MS);
    return () => clearTimeout(t);
  }, [held, onClose, n.key]);
  return (
    <div className={`notice ${n.tone}`} onMouseEnter={() => setHeld(true)} onMouseLeave={() => setHeld(false)} onFocus={() => setHeld(true)} onBlur={() => setHeld(false)}>
      <div className="notice-head">
        <span className="notice-title">{n.title}</span>
        <button className="notice-x" onClick={() => onClose(n.key)} aria-label="Dismiss">
          ×
        </button>
      </div>
      <div className="notice-body">
        <span>{n.body}</span>
        {n.delta != null && n.delta !== 0n && <Amount drops={n.delta} signed />}
      </div>
      <div className="notice-foot">
        {n.meta && <span className="dim">{n.meta}</span>}
        <Link href={to.tx(n.tx)} onClick={() => onClose(n.key)}>
          Receipt →
        </Link>
      </div>
    </div>
  );
}

/** The notice a newly sealed transaction deserves, from `addr`'s side, or null. */
function describe(t: TxDetail, addr: string, a: AccountDetail, viewer: Me | null, myKey: string | undefined, path: string): Notice | null {
  const key = `${addr}:${t.id}`;
  const me = viewer?.address ?? null;
  const d = deltaFor(t, addr);
  const delta = d.direction === "none" ? null : d.delta;
  const summary = fromMySide(t.summary, viewer);
  // Whoever is looking at this transaction's receipt already knows.
  if (path === to.tx(t.id)) return null;

  if (addr !== me) {
    const name = whoText(a.account);
    return { key, tx: t.id, title: t.status === "failed" ? `${name}: a transaction failed` : `Watching ${name}`, body: summary, delta, tone: t.status === "failed" ? "failed" : d.direction === "in" ? "in" : d.direction === "out" ? "out" : "" };
  }

  const fromMe = t.from?.address === me;
  if (t.status === "failed") return fromMe ? { key, tx: t.id, title: "A transaction failed", body: t.error ? `${summary}: ${t.error}` : summary, delta, tone: "failed" } : null;
  if (!fromMe) {
    // Home lights up arrivals itself.
    if (d.direction !== "in" || path === "/") return null;
    return { key, tx: t.id, title: "You received", body: summary, delta, tone: "in", meta: t.memo ? `“${t.memo}”` : undefined };
  }
  const signer = signerOf(t, a.keys, myKey);
  if (signer?.mine || !signer) {
    // Your own: worth a word only once you've moved on from where you sent it.
    if (path === "/send") return null;
    return { key, tx: t.id, title: `Sealed in block #${t.height?.toLocaleString("en-US")}`, body: summary, delta, tone: "" };
  }
  if (signer.agent) {
    const k = a.keys.find((x) => x.key_id === t.signer_key);
    const meta = k?.role.type === "session" ? `${compactDrops(parseDrops(k.spent_this_period))} of ${compactDrops(parseDrops(k.role.limit))} FANG used this period` : undefined;
    return { key, tx: t.id, title: `Your agent “${signer.label}” ${t.kind === "transfer" ? "spent" : "acted"}`, body: summary, delta, tone: "agent", meta };
  }
  return { key, tx: t.id, title: `Signed on “${signer.label}”`, body: summary, delta, tone: "out", meta: "another of your devices" };
}
