"use client";

import Link from "next/link";
import { useState } from "react";
import { fromMySide } from "@/lib/explorer/ledger";
import { useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { AccountDetail, ChainStatus } from "@/lib/explorer/types";
import { toggleWatch, useWatchlist, WATCH_MAX, type Watched } from "@/lib/explorer/watch";
import type { Label } from "@/lib/types";
import { ageOf, Fang, Section, useMe, whoText } from "./kit";

/** Follow an account: it joins the pulse's Watching list, and its activity raises a notice. */
export function WatchButton({ who }: { who: Label }) {
  const list = useWatchlist();
  const on = list.some((w) => w.address === who.address);
  const [full, setFull] = useState(false);
  return (
    <>
      <button
        type="button"
        className={`btn small ${on ? "" : "ghost"}`}
        aria-pressed={on}
        title={on ? "Stop watching" : "Get a notice when this account moves money"}
        onClick={() => setFull(!toggleWatch(who.address, whoText(who)) && !on && list.length >= WATCH_MAX)}
      >
        {on ? "★ Watching" : "☆ Watch"}
      </button>
      {full && <span className="warn small">You can watch {WATCH_MAX} accounts. Unwatch one first.</span>}
    </>
  );
}

/** The accounts you watch, with balance and latest move, on the pulse. */
export function WatchingList() {
  const list = useWatchlist();
  const status = useNode<ChainStatus>("/status", { every: 10_000 });
  if (!list.length) return null;
  return (
    <Section title="Watching" aside={<span className="dim small">notices on, while the app is open</span>}>
      <ul className="ledger">
        {list.map((w) => (
          <WatchRow key={w.address} w={w} head={status.data?.height} blockMs={status.data?.block_time_ms} />
        ))}
      </ul>
    </Section>
  );
}

function WatchRow({ w, head, blockMs }: { w: Watched; head?: number; blockMs?: number }) {
  const me = useMe();
  const a = useNode<AccountDetail>(`/account/${w.address}`, { every: 10_000 });
  const last = a.data?.recent_txs.find((t) => t.status !== "pending");
  const name = a.data ? whoText(a.data.account) : w.label;
  return (
    <li>
      <div className="ledger-row watch-row">
        <span className="ledger-main">
          <Link className="ledger-summary" href={to.account(a.data?.names[0] ?? w.address)}>
            {name}
          </Link>
          <span className="ledger-meta">
            {last ? (
              <Link href={to.tx(last.id)} className="dim">
                {fromMySide(last.summary, me)} · {ageOf(last.height, head, blockMs)}
              </Link>
            ) : (
              <span>{a.error ? "can't load it from this node" : a.data ? "no activity yet" : "…"}</span>
            )}
          </span>
        </span>
        <span className="amt">{a.data ? <Fang display={a.data.balance_display} /> : null}</span>
        <button className="link small" onClick={() => toggleWatch(w.address, w.label)} aria-label={`Stop watching ${name}`} title="Stop watching">
          ×
        </button>
      </div>
    </li>
  );
}
