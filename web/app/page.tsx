"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { RequireWallet } from "@/components/Gate";
import { Heartbeat } from "@/components/explorer/Heartbeat";
import { useMe } from "@/components/explorer/kit";
import { Ledger } from "@/components/explorer/Ledger";
import { Apparition } from "@/components/Haunt";
import { Tutorial, useTutorial } from "@/components/Tutorial";
import { Copy, ExplorerLink, Spinner } from "@/components/ui";
import { explorer } from "@/lib/config";
import { fangParts, shortAddr } from "@/lib/format";
import type { TxView } from "@/lib/types";
import { useUnlocked } from "@/lib/wallet";

/**
 * Ids of payments into `me` that showed up since the last poll (none on first load), held for a
 * few seconds so the coin can spin and the rows can light up.
 */
function useArrivals(txs: TxView[] | undefined, me: string | undefined) {
  const seen = useRef<Set<string> | null>(null);
  // Not tied to the effect: the account re-polls every few seconds, which mustn't cancel it.
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const [fresh, setFresh] = useState<ReadonlySet<string>>(new Set());
  useEffect(() => () => clearTimeout(timer.current), []);
  useEffect(() => {
    if (!txs || !me) return;
    const known = seen.current;
    seen.current = new Set(txs.map((t) => t.id));
    if (!known) return;
    const arrived = txs.filter((t) => !known.has(t.id) && t.kind === "transfer" && t.from.address !== me).map((t) => t.id);
    if (!arrived.length) return;
    setFresh(new Set(arrived));
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setFresh(new Set()), 4000);
  }, [txs, me]);
  return fresh;
}

export default function HomePage() {
  return (
    <RequireWallet>
      <Home />
    </RequireWallet>
  );
}

function Home() {
  const { account, hint, wallet, status } = useUnlocked();
  const me = useMe();
  const [tutorial, updateTutorial] = useTutorial();
  const fresh = useArrivals(account?.recent_txs, account?.account.address);
  if (!account) return <main className="narrow"><Spinner>Loading your account…</Spinner></main>;
  const { whole, frac } = fangParts(account.balance);
  const name = account.names[0];
  const addr = account.account.address;
  return (
    <main className="narrow stack-lg">
      <section className="card plate balance-card">
        <Apparition kind="coin" busy={fresh.size > 0} />
        <span className="eyebrow">The account of</span>
        <div className="who">
          {name ? <span className="who-name">{name}</span> : <Link href="/name" className="claim-pill">Claim your .boo name</Link>}
          <span className="addr-line"><span className="mono">{shortAddr(addr)}</span> <Copy text={addr} label="Copy" /></span>
        </div>
        <div className="balance" aria-label={`${account.balance_display}`}>
          {whole}
          {frac && <span className="frac">.{frac}</span>}
          <span className="unit">FANG</span>
        </div>
        <div className="actions">
          <Link className="btn primary" href="/send">Send</Link>
          <Link className="btn" href="/receive">Receive</Link>
        </div>
        <Heartbeat />
      </section>

      {hint.pendingKit && !account.keys.some((k) => k.key_id === hint.pendingKit?.keyId) && (
        <div className="banner">
          <b>Your recovery kit isn&apos;t active yet.</b> <Link href="/devices#recovery">Activate it</Link> once you have a little FANG for the fee.
        </div>
      )}

      <Tutorial state={tutorial} update={updateTutorial} />

      <section className="card">
        <div className="row-between">
          <h2>Activity</h2>
          <ExplorerLink className="small" href={explorer.account(name ?? addr)}>Flows, keys &amp; full history →</ExplorerLink>
        </div>
        <Ledger txs={account.recent_txs} side={addr} me={me} keys={account.keys} myKeyId={wallet.keyId} head={status?.height} blockMs={status?.block_time_ms} fresh={fresh} empty="Nothing yet. Your transactions will appear here once they're sealed into a block." />
      </section>

      {tutorial?.dismissed && (
        <button className="link small center-self" onClick={() => updateTutorial({ dismissed: false })}>Show the tutorial again</button>
      )}
      {hint.kind === "passkey" && <p className="dim small center-self">Unlocked with your passkey. The key lives only in this tab&apos;s memory.</p>}
    </main>
  );
}
