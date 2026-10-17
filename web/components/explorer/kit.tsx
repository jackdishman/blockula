"use client";

import "@/app/explorer.css";
import Link from "next/link";
import { useEffect, useState, type ReactNode } from "react";
import { Copy } from "@/components/ui";
import { GLOSSARY, type TermKey } from "@/lib/explorer/glossary";
import { compactDrops, formatDrops, parseDrops, type Holding, type Me } from "@/lib/explorer/ledger";
import { to } from "@/lib/explorer/routes";
import type { KeyView, Label } from "@/lib/types";
import { useWallet } from "@/lib/wallet";

/**
 * Who "you" are, for writing pages from your side. Known from the unlocked account, or from the
 * non-secret hint while the wallet is locked (pages read the same, nothing can be signed).
 */
export function useMe(): (Me & { keys: KeyView[]; unlocked: boolean }) | null {
  const { wallet, account, hint } = useWallet();
  if (account && wallet) return { address: account.account.address, names: account.names, short: account.account.short, keys: account.keys, unlocked: true };
  // The node's short form ("boo1dcggj8…lcec4k"), which summaries use for unnamed senders.
  if (hint?.account) return { address: hint.account, names: hint.name ? [hint.name] : [], short: `${hint.account.slice(0, 10)}…${hint.account.slice(-6)}`, keys: [], unlocked: false };
  return null;
}

/** Re-render every `ms`, for relative times. */
export function useNow(ms = 1000) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), ms);
    return () => clearInterval(t);
  }, [ms]);
  return now;
}

export function ago(msAgo: number) {
  const s = Math.max(0, Math.round(msAgo / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 172_800) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86_400)}d ago`;
}

/** A block's age from its height, at the chain's block time ("~3m ago"). Exact times come from the block. */
export function ageOf(height: number | null | undefined, head: number | undefined, blockMs = 1000) {
  if (height == null || head == null) return "";
  return `~${ago(Math.max(0, head - height) * blockMs)}`;
}

export function Term({ k, children }: { k: TermKey; children?: ReactNode }) {
  return (
    <span className="term" tabIndex={0} data-tip={GLOSSARY[k]}>
      {children ?? k.replace(/_/g, " ")}
    </span>
  );
}

// The node labels unnamed system accounts ("🧛 validator", "🏛️ treasury"); those read as italic labels.
const isSystem = (name: string | null) => !!name && /\s/.test(name);
const plain = (s: string) => s.replace(/^[\p{Extended_Pictographic}️‍]+\s*/u, "");

/** Display text for an account label: its name, a system account's label, or its short address. */
export function whoText(who: Label | Holding | null | undefined) {
  if (!who) return "—";
  if ("holding" in who) return who.name;
  if (who.name) return isSystem(who.name) ? plain(who.name) : who.name;
  return who.short;
}

/** An account, by name, linking to its page. Program holdings (escrow, bonds) aren't accounts. */
export function Who({ who, me, link = true }: { who: Label | Holding | null | undefined; me?: string | null; link?: boolean }) {
  if (!who) return <span className="dim">—</span>;
  if ("holding" in who) return <span className="who-link holding">{who.name}</span>;
  const named = !!who.name && !isSystem(who.name);
  const inner = named ? (
    <>{who.name}</>
  ) : who.name ? (
    <>
      <span className="system">{plain(who.name)}</span> <span className="mono small nowrap">{who.short}</span>
    </>
  ) : (
    <span className="mono">{who.short}</span>
  );
  const you = me && who.address === me ? <span className="you">you</span> : null;
  if (!link)
    return (
      <span>
        {inner}
        {you}
      </span>
    );
  return (
    <span>
      <Link className="who-link" href={to.account(named ? (who.name as string) : who.address)}>
        {inner}
      </Link>
      {you}
    </span>
  );
}

/** FANG, compact for reading (or `exact`), exact on hover. `signed` adds +/− (and is the only place a sign appears). */
export function Amount({ drops, signed = false, unit = true, exact = false, className = "" }: { drops: bigint | string | null | undefined; signed?: boolean; unit?: boolean; exact?: boolean; className?: string }) {
  const d = typeof drops === "bigint" ? drops : parseDrops(drops ?? "0");
  const sign = signed ? (d > 0n ? "+" : d < 0n ? "−" : "") : d < 0n ? "−" : "";
  const abs = d < 0n ? -d : d;
  return (
    <span className={`amt ${signed ? "signed" : ""} ${className}`} title={formatDrops(d)}>
      {sign}
      {exact ? formatDrops(abs, "") : compactDrops(abs)}
      {unit && <small>FANG</small>}
    </span>
  );
}

/** A long hex value: shortened unless `full`, always copyable. */
export function Hash({ value, full = false, href }: { value: string; full?: boolean; href?: string }) {
  const text = full || value.length <= 20 ? value : `${value.slice(0, 10)}…${value.slice(-8)}`;
  return (
    <span className="hash">
      {href ? (
        <Link href={href} className={`mono ${text === value ? "" : "nowrap"}`}>
          {text}
        </Link>
      ) : (
        <span className={`mono ${text === value ? "" : "nowrap"}`} title={value}>
          {text}
        </span>
      )}
      <Copy text={value} label="Copy" />
    </span>
  );
}

export function Stat({ v, k }: { v: ReactNode; k: ReactNode }) {
  return (
    <div className="stat">
      <div className="v">{v}</div>
      <div className="k">{k}</div>
    </div>
  );
}

/**
 * A node `*_display` amount for a figure: "12.5 FANG", "48,210.25 FANG", or "1.10B FANG" once it
 * would crowd a column (`short`). The exact amount is in the tooltip.
 */
export function Fang({ display, short = false }: { display: string | undefined; short?: boolean }) {
  if (!display) return <>—</>;
  const [n, unit = ""] = display.split(" ");
  const [w, f = ""] = n.split(".");
  if (!/^\d+$/.test(w)) return <>{display}</>;
  const big = short && w.length > 6 ? `${(Number(w) / (w.length > 9 ? 1e9 : 1e6)).toFixed(2)}${w.length > 9 ? "B" : "M"}` : null;
  const frac = (w.length > 6 ? "" : w.length > 3 ? f.slice(0, 2) : f.slice(0, 4)).replace(/0+$/, "");
  return (
    <span title={display}>
      {big ?? Number(w).toLocaleString("en-US")}
      {!big && frac ? `.${frac}` : ""}
      <small className="unit-s">{unit}</small>
    </span>
  );
}

const STATUS_CLASS: Record<string, string> = { finalized: "ok", pending: "pending", failed: "failed", open: "ok", awarded: "dim", cancelled: "dim", expired: "dim" };

export function StatusPill({ status, children }: { status: string; children?: ReactNode }) {
  return <span className={`pill ${STATUS_CLASS[status] ?? "pending"}`}>{children ?? status.replace(/_/g, " ")}</span>;
}

export function Section({ title, aside, children, className = "" }: { title: ReactNode; aside?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <section className={`card ${className}`}>
      <div className="card-head">
        <h2>{title}</h2>
        {aside}
      </div>
      {children}
    </section>
  );
}

/** A page the node couldn't find, or a node we can't reach, said plainly. */
export function NotFound({ what, error }: { what: string; error: Error }) {
  const unreachable = /Failed to fetch|NetworkError|Load failed/i.test(error.message);
  return (
    <main className="wide stack-lg">
      <section className="card">
        <span className="eyebrow">{unreachable ? "The node is silent" : "Nothing here"}</span>
        <h1>{unreachable ? "Can't reach the node" : `No such ${what}`}</h1>
        <p className="dim">
          {unreachable ? "The network picked at the top isn't answering. Try another, or start a local node with `blockula dev`." : `${error.message}. It may be on another network: check the one picked at the top.`}
        </p>
        <Link href={to.explore()}>Back to the explorer</Link>
      </section>
    </main>
  );
}

export function Loading({ children = "Rising from the grave…" }: { children?: ReactNode }) {
  return (
    <main className="wide">
      <p className="dim center" style={{ padding: "60px 0", fontStyle: "italic" }}>
        {children}
      </p>
    </main>
  );
}

/**
 * An outside link from chain data (an issue or PR URL), only if it's plain https: a node can
 * send anything, and `javascript:` must never become an href.
 */
export function Ext({ href, children, className }: { href: string | null | undefined; children: ReactNode; className?: string }) {
  if (!href || !/^https:\/\/[^\s"'<>]+$/.test(href)) return <span className={className}>{children}</span>;
  return (
    <a href={href} target="_blank" rel="noreferrer nofollow" className={className}>
      {children}
    </a>
  );
}

export const plural = (n: number, one: string, many = `${one}s`) => `${n.toLocaleString("en-US")} ${n === 1 ? one : many}`;
