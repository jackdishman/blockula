"use client";

import Link from "next/link";
import { RequireWallet } from "@/components/Gate";
import { Activity } from "@/components/Activity";
import { Tutorial, useTutorial } from "@/components/Tutorial";
import { Copy, ExplorerLink, Spinner } from "@/components/ui";
import { explorer } from "@/lib/config";
import { fangParts, shortAddr } from "@/lib/format";
import { useUnlocked } from "@/lib/wallet";

export default function HomePage() {
  return (
    <RequireWallet>
      <Home />
    </RequireWallet>
  );
}

function Home() {
  const { account, hint } = useUnlocked();
  const [tutorial, updateTutorial] = useTutorial();
  if (!account) return <main className="narrow"><Spinner>Loading your account…</Spinner></main>;
  const { whole, frac } = fangParts(account.balance);
  const name = account.names[0];
  const addr = account.account.address;
  return (
    <main className="narrow stack-lg">
      <section className="card plate balance-card">
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
      </section>

      <Tutorial state={tutorial} update={updateTutorial} />

      <section className="card">
        <div className="row-between">
          <h2>Activity</h2>
          <ExplorerLink className="small" href={explorer.account(name ?? addr)}>Open in the explorer ↗</ExplorerLink>
        </div>
        <Activity txs={account.recent_txs} me={addr} />
      </section>

      {tutorial.dismissed && (
        <button className="link small center-self" onClick={() => updateTutorial({ dismissed: false })}>Show the tutorial again</button>
      )}
      {hint.kind === "passkey" && <p className="dim small center-self">Unlocked with your passkey. The key lives only in this tab&apos;s memory.</p>}
    </main>
  );
}
