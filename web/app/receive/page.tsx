"use client";

import "@/app/creators.css";
import Link from "next/link";
import { useEffect, useState } from "react";
import { RequireWallet } from "@/components/Gate";
import { Apparition } from "@/components/Haunt";
import { QrCode } from "@/components/QrCode";
import { Copy, ExplorerLink, Spinner, Tip } from "@/components/ui";
import { DEFAULT_NETWORK, explorer, NETWORK, walletUrl } from "@/lib/config";
import { PUBLIC_MEMOS } from "@/lib/creators";
import { payLink, requestAmountError } from "@/lib/paylink";
import { useUnlocked } from "@/lib/wallet";

export default function ReceivePage() {
  return (
    <RequireWallet>
      <Receive />
    </RequireWallet>
  );
}

const utf8Len = (s: string) => new TextEncoder().encode(s).length;

function Receive() {
  const { wallet, account } = useUnlocked();
  const name = account?.names[0];
  const addr = wallet.account;
  return (
    <main className="narrow stack-lg">
      <Link href="/" className="back">← Home</Link>
      <Apparition kind="cauldron" />
      <h1>Receive FANG</h1>
      <PayRequest name={name} address={addr} />
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

/**
 * QR code + payment link that opens Send with the recipient (and optionally amount and memo) filled
 * in. It names the account by its name, unless it's for print: a name lapses if it isn't renewed,
 * and then whoever registers it next is paid, but a printed code can't be changed, so a code for
 * print carries the address (CRE-5).
 */
function PayRequest({ name, address }: { name?: string; address: string }) {
  const { client, status } = useUnlocked();
  const [amount, setAmount] = useState("");
  const [memo, setMemo] = useState("");
  const [print, setPrint] = useState(false);
  const to = name && !print ? name : address;
  const [canShare, setCanShare] = useState(false);
  useEffect(() => setCanShare(typeof navigator.share === "function"), []);

  // At most a u64 of drops, like the chain: a longer amount made the link too long for a QR code (main-11-2).
  const amountErr = requestAmountError(amount);
  const memoErr = utf8Len(memo) > 256 ? "Memos are limited to 256 bytes." : null;
  // The link says which chain it's for (a check for the payer's wallet, never a switch: main-11-1):
  // the pinned one, complete once the node's status has filled in what configuration didn't.
  const chain = client.pin.chainId && (client.pin.genesisHash || status) ? client.pin : null;
  // A bad amount or memo is left out of the link rather than passed on to the payer.
  const link = chain?.chainId ? payLink(walletUrl(), to, amountErr ? "" : amount, memoErr ? "" : memo, chain) : null;

  if (!link) return <section className="card"><Spinner>Making your payment link…</Spinner></section>;
  return (
    <section className="card stack">
      <span className="eyebrow">{print && name ? "Scan to pay, for print" : "Scan to pay"}</span>
      <div className="qr-frame">
        <QrCode text={link} label={`QR code: payment link to ${to}`} />
      </div>
      <label className="field">
        <span>Amount <span className="dim">(optional)</span></span>
        <div className="suffix-input">
          <input value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="Payer chooses" inputMode="decimal" />
          <span className="suffix">FANG</span>
        </div>
      </label>
      {amountErr && <span className="warn">{amountErr}</span>}
      <label className="field">
        <span>Memo <span className="dim">(optional, public)</span></span>
        <input value={memo} onChange={(e) => setMemo(e.target.value)} placeholder="for the pumpkins" />
      </label>
      {memoErr && <span className="warn">{memoErr}</span>}
      {name && (
        <label className="cre-print">
          <input type="checkbox" checked={print} onChange={(e) => setPrint(e.target.checked)} data-testid="for-print" />
          <span>
            For print (never changes hands)
            <span className="dim small block">
              {print
                ? "This link and code use your address, so they pay you even if your name lapses and someone else registers it."
                : `Uses ${name}. A name you stop renewing can be registered by someone else, and a printed code can't be changed: tick this before you print it.`}
            </span>
          </span>
        </label>
      )}
      <div className="mono address small" data-testid="paylink">{link}</div>
      <div className="row-gap">
        <Copy text={link} label="Copy link" />
        {canShare && (
          <button type="button" className="btn small ghost" onClick={() => navigator.share({ url: link }).catch(() => {})}>
            Share…
          </button>
        )}
      </div>
      {NETWORK.key !== DEFAULT_NETWORK.key && (
        <span className="warn small">
          You&apos;re on {NETWORK.label}. The link opens the wallet on whatever network the payer has picked, usually {DEFAULT_NETWORK.label}; it says it&apos;s for {NETWORK.label}, so a wallet on another network won&apos;t fill it in.
        </span>
      )}
      <span className="dim small">The link only fills in the form: the payer still sees who it goes to and signs it themselves.</span>
      <p className="cre-public" data-testid="public-memos">{PUBLIC_MEMOS}</p>
    </section>
  );
}
