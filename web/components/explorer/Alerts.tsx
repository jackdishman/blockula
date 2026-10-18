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
  /** The transaction it's about ("" when it's about several: see `href`). */
  tx: string;
  /** Where "open" goes when there's no one transaction (an account's activity). */
  href?: string;
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

/** Notices that outrank arrivals: they're what a stolen key or a runaway agent would do. */
const urgent = (n: Notice) => n.tone === "agent" || n.tone === "out" || n.tone === "failed";

/** What a poll saw of the account's own spending, to catch what scrolled out of `recent_txs`. */
interface Spend {
  nonce: number;
  keys: Map<string, { spent: bigint; window: number; label: string; agent: boolean }>;
}

/**
 * Notices for money that moves while you're anywhere in the app: payments to you, spending by
 * your agents and other devices, your own transactions sealing, and activity on accounts you
 * watch. Read from the chain (works while locked), starting from what's already there: only
 * news is announced.
 *
 * Only while the wallet has the node on its chain (`chain.state` ok, and each round re-checks the
 * pinned status): a node it refused never raises a notice (audit main13-signing-5). The node
 * lists an account's newest 25 transactions, so a burst of them (anyone can pay you dust) can push
 * a spend out of view between polls; the account's nonce and its keys' spending catch that, and
 * arrivals in one round are folded into one notice so they can't crowd out a spend (audit
 * main13-signing-6).
 */
export function Alerts() {
  const me = useMe();
  const { hint, wallet, chain, client } = useWallet();
  const watch = useWatchlist();
  const path = usePathname();
  const [shown, setShown] = useState<Notice[]>([]);
  const seen = useRef(new Map<string, Map<string, string>>());
  const told = useRef(new Set<string>());
  const spends = useRef(new Map<string, Spend>());
  const pathRef = useRef(path);
  pathRef.current = path;
  const unread = useRef(0);
  const title = useRef<string | null>(null);

  const myAddr = me?.address ?? null;
  const meRef = useRef(me);
  meRef.current = me;
  const myKey = wallet?.keyId ?? hint?.keyId;
  const onChain = chain.state === "ok";
  const followed = onChain ? [...(myAddr ? [myAddr] : []), ...watch.map((w) => w.address).filter((a) => a !== myAddr)] : [];
  const followKey = followed.join(",");

  const dismiss = useCallback((key: string) => setShown((s) => s.filter((n) => n.key !== key)), []);

  useEffect(() => {
    if (!followKey) return;
    const list = followKey.split(",");
    let tick = 0;
    let stop = false;

    // Newest first, urgent ones kept over arrivals when there are too many to show.
    const announce = (ns: Notice[]) => {
      if (!ns.length) return;
      setShown((s) => {
        const all = [...ns, ...s.filter((x) => !ns.some((n) => n.key === x.key))];
        return [...all.filter(urgent), ...all.filter((n) => !urgent(n))].slice(0, MAX_SHOWN).sort((a, b) => all.indexOf(a) - all.indexOf(b));
      });
      if (document.visibilityState === "hidden") {
        unread.current += ns.length;
        title.current ??= document.title;
        document.title = `(${unread.current}) ${title.current}`;
      }
    };

    const read = (addr: string, a: AccountDetail) => {
      const before = seen.current.get(addr);
      const now = new Map(a.recent_txs.map((t) => [t.id, t.status] as const));
      seen.current.set(addr, now);
      const own = addr === meRef.current?.address;
      const prevSpend = spends.current.get(addr);
      const spend: Spend = { nonce: a.nonce, keys: new Map(a.keys.map((k) => [k.key_id, { spent: parseDrops(k.spent_this_period), window: k.window_start, label: k.label, agent: k.role.type === "session" }])) };
      spends.current.set(addr, spend);
      // The first look is the baseline: what's already there isn't news.
      if (!before) return;
      const out: Notice[] = [];
      const arrivals: Notice[] = [];
      let sentSeen = 0;
      const toldBy = new Map<string, bigint>();
      for (const t of [...a.recent_txs].reverse()) {
        const was = before.get(t.id);
        if (t.status === "pending" || (was && was !== "pending")) continue;
        // Newly sealed in this account's list: counted against its nonce even when another
        // followed account's read announced it first (a payment to a watched account sealing
        // between the two reads is no missed spend: audit review-main13-4).
        if (t.from?.address === addr) {
          sentSeen++;
          if (t.signer_key) toldBy.set(t.signer_key, (toldBy.get(t.signer_key) ?? 0n) - deltaFor(t, addr).delta);
        }
        if (told.current.has(t.id)) continue;
        const n = describe(t, addr, a, meRef.current, myKey, pathRef.current);
        // Even when it isn't worth a notice, a transaction is only ever considered once.
        told.current.add(t.id);
        if (n) (n.tone === "in" ? arrivals : out).push(n);
      }
      // Sent but never seen: the nonce moved further than the transactions that showed up.
      const missed = own && prevSpend ? a.nonce - prevSpend.nonce - sentSeen : 0;
      if (missed > 0 && prevSpend) {
        let named = false;
        for (const [id, k] of spend.keys) {
          const p = prevSpend.keys.get(id);
          const moved = !p || p.window !== k.window ? k.spent : k.spent - p.spent;
          const extra = moved - (toldBy.get(id) ?? 0n);
          if (k.agent && extra > 0n && id !== myKey) {
            named = true;
            out.push({ key: `${addr}:spent:${id}:${a.nonce}`, tx: "", title: `Your agent “${k.label}” spent`, body: `${missed} transaction${missed === 1 ? "" : "s"} from your account were pushed out of view by newer activity before this page saw ${missed === 1 ? "it" : "them"}. Check your activity.`, delta: -extra, tone: "agent", meta: `${compactDrops(k.spent)} FANG used this period`, href: to.account(addr) });
          }
        }
        if (!named) out.push({ key: `${addr}:missed:${a.nonce}`, tx: "", title: "Signed on your account", body: `${missed} transaction${missed === 1 ? "" : "s"} from your account were pushed out of view by newer activity before this page saw ${missed === 1 ? "it" : "them"}. Check your activity.`, delta: null, tone: "out", href: to.account(addr) });
      }
      // Arrivals in one round fold into one notice.
      const folded: Notice[] =
        arrivals.length > 2
          ? [{ key: `${addr}:in:${arrivals[0].tx}`, tx: "", title: "You received", body: `${arrivals.length} payments`, delta: arrivals.reduce((t, n) => t + (n.delta ?? 0n), 0n), tone: "in", href: to.account(addr) }]
          : arrivals;
      // Announced last, so they sit on top.
      announce([...out.reverse(), ...folded]);
    };

    let busy = false;
    const poll = async () => {
      tick++;
      const hidden = document.visibilityState === "hidden";
      // A slow node: let the last round finish rather than stack another on it.
      if (busy || (hidden && tick % HIDDEN_EVERY)) return;
      busy = true;
      try {
        // The pinned client's status: a node on another chain stops the round (ChainMismatchError).
        if (!(await client.status().then(() => true, () => false))) return;
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
  }, [followKey, myAddr, myKey, client]);

  if (!onChain || !shown.length) return <div className="notices" aria-live="polite" />;
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
        <Link href={n.tx ? to.tx(n.tx) : (n.href ?? to.explore())} onClick={() => onClose(n.key)}>
          {n.tx ? "Receipt →" : "Activity →"}
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
