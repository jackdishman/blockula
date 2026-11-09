"use client";

import { useEffect, useState } from "react";
import { QrCode } from "@/components/QrCode";
import { Copy } from "@/components/ui";
import { Unit } from "@/components/Unit";
import { walletUrl } from "@/lib/config";
import { MAX_MEMO_BYTES, payLink, requestAmountError } from "@/lib/paylink";
import { useWallet } from "@/lib/wallet";

const utf8Len = (s: string) => new TextEncoder().encode(s).length;

/**
 * "Request" on your own profile: an amount and a memo make a link anyone can open,
 * `<wallet>/send?to=<your name>&amount=…&memo=…` on this wallet's origin (walletUrl: the node's
 * `/wallet` when a node serves it, BASE_PATH included), with a QR code and the share sheet.
 *
 * No program is needed for this (programs are off on most chains, the demo's included): a request
 * is only a prefilled Send. The payer's wallet fills the form from the link (send's fromLink),
 * says a link filled it in, and the payer reviews and signs it themselves. Nothing is ever sent
 * on opening the link, and the link checks it's for this chain (lib/paylink).
 */
export function RequestPay({ to, onClose }: { to: string; onClose: () => void }) {
  const { client, status } = useWallet();
  const [amount, setAmount] = useState("");
  const [memo, setMemo] = useState("");
  const [canShare, setCanShare] = useState(false);
  useEffect(() => setCanShare(typeof navigator.share === "function"), []);

  const amountErr = requestAmountError(amount);
  const memoErr = utf8Len(memo) > MAX_MEMO_BYTES ? `Memos are limited to ${MAX_MEMO_BYTES} bytes.` : null;
  // The link names the chain it's for, once the pin is complete (as the receive page's).
  const chain = client.pin.chainId && (client.pin.genesisHash || status) ? client.pin : null;
  const link = chain?.chainId ? payLink(walletUrl(), to, amountErr ? "" : amount, memoErr ? "" : memo, chain) : null;

  return (
    <section className="card stack request-pay" data-testid="request-card">
      <div className="card-head">
        <h2>Request a payment</h2>
        <button type="button" className="btn small ghost" onClick={onClose}>
          Close
        </button>
      </div>
      <label className="field">
        <span>Amount <span className="dim">(optional)</span></span>
        <div className="suffix-input">
          <input value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="Payer chooses" inputMode="decimal" data-testid="request-amount" />
          <span className="suffix"><Unit /></span>
        </div>
      </label>
      {amountErr && <span className="warn">{amountErr}</span>}
      <label className="field">
        <span>Memo <span className="dim">(optional, public)</span></span>
        <input value={memo} onChange={(e) => setMemo(e.target.value)} placeholder="for the pumpkins" data-testid="request-memo" />
      </label>
      {memoErr && <span className="warn">{memoErr}</span>}
      {link ? (
        <>
          <div className="qr-frame">
            <QrCode text={link} label={`QR code: payment request to ${to}`} />
          </div>
          <div className="mono address small wrap-any" data-testid="request-link">{link}</div>
          <div className="row-gap">
            <Copy text={link} label="Copy link" />
            {canShare && (
              <button type="button" className="btn small ghost" onClick={() => navigator.share({ url: link, title: `Pay ${to}` }).catch(() => {})}>
                Share…
              </button>
            )}
          </div>
          <span className="dim small">The link only fills in the form: the payer sees who it goes to and signs it themselves.</span>
        </>
      ) : (
        <span className="dim small">Making your link…</span>
      )}
    </section>
  );
}
