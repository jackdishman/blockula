"use client";

import Link from "next/link";
import { RequireWallet } from "@/components/Gate";
import { Apparition } from "@/components/Haunt";
import { Copy, ExplorerLink, Tip } from "@/components/ui";
import { explorer } from "@/lib/config";
import { useUnlocked } from "@/lib/wallet";

export default function ReceivePage() {
  return (
    <RequireWallet>
      <Receive />
    </RequireWallet>
  );
}

function Receive() {
  const { wallet, account } = useUnlocked();
  const name = account?.names[0];
  const addr = wallet.account;
  return (
    <main className="narrow stack-lg">
      <Link href="/" className="back">← Home</Link>
      <Apparition kind="cauldron" />
      <h1>Receive FANG</h1>
      {name ? (
        <section className="card stack">
          <span className="eyebrow">Share your name</span>
          <div className="big-name">{name}</div>
          <Copy text={name} label="Copy name" />
        </section>
      ) : (
        <section className="card">
          <p>You don&apos;t have a name yet. Friends can still pay your address below, but <Link href="/name">a .boo name</Link> is much easier to share.</p>
        </section>
      )}
      <section className="card stack">
        <span className="eyebrow">Or your full address</span>
        <div className="mono address">{addr}</div>
        <div className="row-gap">
          <Copy text={addr} label="Copy address" />
          <ExplorerLink className="btn small ghost" href={explorer.account(name ?? addr)}>View in explorer ↗</ExplorerLink>
        </div>
      </section>
      <Tip>
        Your address is a fingerprint (BLAKE3 hash) of your public key, encoded so typos get caught. It&apos;s safe to share; only your passkey can spend from it.
      </Tip>
    </main>
  );
}
