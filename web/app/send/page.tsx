"use client";

import { formatFang, isAddress, parseFang, type Tx } from "@blockula/sdk";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { Suspense, useEffect, useState } from "react";
import { PreflightView } from "@/components/explorer/PreflightView";
import { RequireWallet } from "@/components/Gate";
import { Apparition } from "@/components/Haunt";
import { ErrorNote, ExplorerLink, Spinner, Tip } from "@/components/ui";
import { explorer } from "@/lib/config";
import { friendly } from "@/lib/errors";
import { checkTx } from "@/lib/explorer/check";
import type { Preflight } from "@/lib/explorer/preflight";
import { shortAddr } from "@/lib/format";
import type { NameView, TxView } from "@/lib/types";
import { useUnlocked } from "@/lib/wallet";

export default function SendPage() {
  return (
    <RequireWallet>
      <Suspense>
        <Send />
      </Suspense>
    </RequireWallet>
  );
}

type Recipient =
  | { state: "empty" }
  | { state: "checking" }
  | { state: "ok"; address: string; display: string; detail: string }
  | { state: "bad"; msg: string };

const utf8Len = (s: string) => new TextEncoder().encode(s).length;

function useRecipient(input: string): Recipient {
  const { client, status } = useUnlocked();
  const [r, setR] = useState<Recipient>({ state: "empty" });
  useEffect(() => {
    const q = input.trim().toLowerCase();
    if (!q) return setR({ state: "empty" });
    if (q.startsWith("boo1")) {
      if (!isAddress(q)) return setR({ state: "bad", msg: "That address doesn't look right. Check for typos." });
      const label = status?.validator.address === q ? "the validator" : "an address";
      return setR({ state: "ok", address: q, display: shortAddr(q), detail: label });
    }
    setR({ state: "checking" });
    let live = true;
    const t = setTimeout(async () => {
      try {
        const v = (await client.name(q)) as NameView;
        if (!live) return;
        if (v.available || !v.owner) setR({ state: "bad", msg: `${v.name} isn't registered. Check the spelling.` });
        else setR({ state: "ok", address: v.owner.address, display: v.name, detail: v.owner.short });
      } catch (e) {
        if (live) setR({ state: "bad", msg: friendly(e) });
      }
    }, 250);
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [input, client, status]);
  return r;
}

function Send() {
  const { wallet, account, client, refresh } = useUnlocked();
  const params = useSearchParams();
  const [to, setTo] = useState(params.get("to") ?? "");
  const [amount, setAmount] = useState(params.get("amount") ?? "");
  const [memo, setMemo] = useState(params.get("memo") ?? "");
  const prefilled = params.has("to") || params.has("amount") || params.has("memo");
  const [review, setReview] = useState<{ tx: Tx; display: string; pf: Preflight | null; blockMs: number } | null>(null);
  const [sent, setSent] = useState<TxView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const recipient = useRecipient(to);

  let drops: bigint | null = null;
  let amountErr: string | null = null;
  try {
    if (amount.trim()) {
      drops = parseFang(amount);
      if (drops === 0n) amountErr = "Enter more than zero.";
      else if (account && drops > BigInt(account.balance)) amountErr = `You only have ${account.balance_display}.`;
    }
  } catch {
    amountErr = "Use a number like 1 or 0.25 (up to 9 decimals).";
  }
  const memoErr = utf8Len(memo) > 256 ? "Memos are limited to 256 bytes." : null;
  const self = recipient.state === "ok" && recipient.address === wallet.account;
  const canReview = recipient.state === "ok" && drops !== null && !amountErr && !memoErr && !self;

  const toReview = async () => {
    if (recipient.state !== "ok" || drops === null) return;
    setBusy(true);
    setError(null);
    try {
      // Build and sign now so the fee shown is exactly what will be paid.
      const tx = await wallet.build({ type: "transfer", to: recipient.address, amount: drops.toString() }, memo);
      // What it will do and whether the chain will take it; the review still works without it.
      const check = await checkTx(tx, wallet.keyId).catch(() => null);
      setReview({ tx, display: recipient.display, pf: check?.pf ?? null, blockMs: check?.blockMs ?? 1000 });
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  const confirm = async () => {
    if (!review) return;
    setBusy(true);
    setError(null);
    try {
      const done = (await client.submit(review.tx, true)) as TxView;
      setSent(done);
      refresh();
    } catch (e) {
      setError(friendly(e));
      // Nonce or balance may have moved; send them back to edit.
      setReview(null);
    } finally {
      setBusy(false);
    }
  };

  if (sent)
    return (
      <main className="narrow stack-lg center">
        <Apparition kind="bat" burst={9} />
        <h1>Sent</h1>
        <p className="summary">{sent.summary}</p>
        <p className="dim">Sealed in block #{sent.height}. That&apos;s final: no waiting for confirmations.</p>
        <div className="row-gap center-row">
          <ExplorerLink className="btn" href={explorer.tx(sent.id)}>View the receipt</ExplorerLink>
          <Link className="btn primary" href="/">Done</Link>
        </div>
      </main>
    );

  if (review) {
    const fee = BigInt(review.tx.fee);
    const amt = BigInt((review.tx.action as { amount: string }).amount);
    const me = account?.names[0] ?? "You";
    return (
      <main className="narrow stack-lg">
        <div className="section-head">
          <span className="eyebrow">Read it twice</span>
          <h1>Review</h1>
        </div>
        <section className="card stack">
          <p className="summary">
            {me} sends {formatFang(amt)} to {review.display}
          </p>
          <dl className="kv">
            <dt>To</dt>
            <dd>
              {review.display}
              {/* Always show the exact address being signed, not just the name. */}
              <div className="mono small break">{review.tx.action.type === "transfer" ? review.tx.action.to : ""}</div>
            </dd>
            <dt>Amount</dt>
            <dd>{formatFang(amt)}</dd>
            <dt>Network fee</dt>
            <dd>{formatFang(fee)} <span className="dim small">(burned)</span></dd>
            <dt className="total">Total</dt>
            <dd className="total"><b>{formatFang(amt + fee)}</b></dd>
            {review.tx.memo && (
              <>
                <dt>Memo</dt>
                <dd><i>“{review.tx.memo}”</i> <span className="dim small">(public)</span></dd>
              </>
            )}
          </dl>
          {review.pf && <PreflightView p={review.pf} me={wallet.account} blockMs={review.blockMs} />}
        </section>
        <Tip>
          The fee is priced by size: post-quantum signatures are about 3.3 KB, so every transaction pays a little for its bytes, and that fee is destroyed rather than paid to anyone.
        </Tip>
        <div className="row-gap">
          <button className="btn" disabled={busy} onClick={() => setReview(null)}>← Edit</button>
          <button className={`btn grow ${review.pf?.refusals.length ? "" : "primary"}`} disabled={busy} onClick={confirm}>
            {busy ? <Spinner>Sealing into a block…</Spinner> : review.pf?.refusals.length ? "Send anyway" : "Confirm & send"}
          </button>
        </div>
        <ErrorNote error={error} />
      </main>
    );
  }

  return (
    <main className="narrow stack-lg">
      <Link href="/" className="back">← Home</Link>
      <h1>Send FANG</h1>
      {prefilled && (
        <div className="banner">
          A link filled in this payment. Double-check who it goes to and how much before sending.
        </div>
      )}
      <section className="card stack">
        <label className="field">
          <span>To</span>
          <input value={to} onChange={(e) => setTo(e.target.value)} placeholder="alice.boo or boo1…" autoCapitalize="none" autoCorrect="off" spellCheck={false} />
        </label>
        <div className="name-status" aria-live="polite">
          {recipient.state === "checking" && <Spinner>Looking up…</Spinner>}
          {recipient.state === "ok" && !self && <span className="ok">✓ {recipient.display} · <span className="dim">{recipient.detail}</span></span>}
          {self && <span className="warn">That&apos;s you! Pick someone else.</span>}
          {recipient.state === "bad" && <span className="warn">{recipient.msg}</span>}
        </div>
        <label className="field">
          <span>Amount</span>
          <div className="suffix-input">
            <input value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="0.0" inputMode="decimal" />
            <span className="suffix">FANG</span>
          </div>
        </label>
        <div className="name-status">
          {amountErr ? <span className="warn">{amountErr}</span> : <span className="dim small">Balance: {account?.balance_display ?? "…"}</span>}
        </div>
        <label className="field">
          <span>Memo <span className="dim">(optional, public)</span></span>
          <input value={memo} onChange={(e) => setMemo(e.target.value)} placeholder="trick or treat" />
        </label>
        {memoErr && <span className="warn">{memoErr}</span>}
        <button className="btn primary" disabled={!canReview || busy} onClick={toReview}>
          {busy ? <Spinner>Preparing…</Spinner> : "Review"}
        </button>
        <ErrorNote error={error} />
      </section>
      <Tip>Names are checked live against the chain, so you&apos;ll see who you&apos;re paying before you sign anything.</Tip>
    </main>
  );
}
