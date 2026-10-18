"use client";

import { formatFang, isAddress, parseFang, type Tx } from "@blockula/sdk";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { Suspense, useEffect, useState } from "react";
import { PreflightView } from "@/components/explorer/PreflightView";
import { RequireWallet } from "@/components/Gate";
import { Apparition } from "@/components/Haunt";
import { PayLinkNotice } from "@/components/PayLinkNotice";
import { ErrorNote, ExplorerLink, Spinner, Tip } from "@/components/ui";
import { explorer, MAINNET_PREFIX, NETWORK } from "@/lib/config";
import { friendly } from "@/lib/errors";
import { checkTx } from "@/lib/explorer/check";
import type { Preflight } from "@/lib/explorer/preflight";
import { shortAddr } from "@/lib/format";
import { checkLinkChain, linkChain } from "@/lib/paylink";
import type { TxView } from "@/lib/types";
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
  /** `signed`: what goes in the transfer; `byChain`: it's the name, and `address` only the node's current answer. */
  | { state: "ok"; signed: string; address: string; display: string; detail: string; proven?: boolean; byChain?: boolean }
  | { state: "bad"; msg: string };

const utf8Len = (s: string) => new TextEncoder().encode(s).length;

function useRecipient(input: string): Recipient {
  const { client, status } = useUnlocked();
  const [r, setR] = useState<Recipient>({ state: "empty" });
  useEffect(() => {
    const q = input.trim().toLowerCase();
    if (!q) return setR({ state: "empty" });
    // This chain's address prefix (DEC-7): another network's address is refused, never "converted".
    const hrp = client.pin.prefix ?? MAINNET_PREFIX;
    const prefix = q.match(/^([a-z]+)1[a-z0-9]{40,}$/)?.[1];
    if (prefix && prefix !== hrp) return setR({ state: "bad", msg: `That's an address on another network (${prefix}1…). ${NETWORK.label} addresses start with ${hrp}1.` });
    if (q.startsWith(`${hrp}1`)) {
      if (!isAddress(q)) return setR({ state: "bad", msg: "That address doesn't look right. Check for typos." });
      const label = status?.validator.address === q ? "the validator" : "an address";
      return setR({ state: "ok", signed: q, address: q, display: shortAddr(q), detail: label, proven: true });
    }
    setR({ state: "checking" });
    let live = true;
    const t = setTimeout(async () => {
      try {
        // With the network's witnesses the name's proven owner is signed. Without them the name
        // itself is signed, and the chain pays whoever owns it when the transfer runs, so the node
        // can't point it at another account now (it could hold it and release it later in its
        // validity window); what it says the owner is, is shown as just that (audit gap-7-1,
        // review-r3-clients-2).
        const res = await client.recipient(q);
        if (!live) return;
        const detail = res.byChain ? `the chain pays its owner when this runs · your node says ${shortAddr(res.address)} (not proven)` : `${shortAddr(res.address)} · proven by ${client.witnesses.length} other node${client.witnesses.length === 1 ? "" : "s"}`;
        setR({ state: "ok", signed: res.signed, address: res.address, display: `${res.name}.boo`, detail, proven: res.proven, byChain: res.byChain });
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
  const prefilled = params.has("to") || params.has("amount") || params.has("memo");
  // A payment link says which chain it was made for: a check, never a switch. A .boo name can be
  // someone else's on another network, so a link from another chain fills nothing and can't be
  // reviewed; until the wallet knows its own chain, it can't be reviewed either (audit main-11-1).
  const link = prefilled ? linkChain(params) : null;
  const linkCheck = prefilled ? checkLinkChain(link, client.pin) : null;
  const refused = linkCheck?.state === "mismatch";
  const fromLink = (key: string) => (refused ? "" : (params.get(key) ?? ""));
  const [to, setTo] = useState(() => fromLink("to"));
  const [amount, setAmount] = useState(() => fromLink("amount"));
  const [memo, setMemo] = useState(() => fromLink("memo"));
  useEffect(() => {
    if (!refused) return;
    setTo("");
    setAmount("");
    setMemo("");
  }, [refused]);
  const [review, setReview] = useState<{ tx: Tx; display: string; proven: boolean; byChain: boolean; nodeSays: string; pf: Preflight | null; blockMs: number } | null>(null);
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
  const canReview = recipient.state === "ok" && drops !== null && !amountErr && !memoErr && !self && !refused && linkCheck?.state !== "pending";

  const toReview = async () => {
    if (recipient.state !== "ok" || drops === null) return;
    setBusy(true);
    setError(null);
    try {
      // Build and sign now so the fee shown is exactly what will be paid.
      const tx = await wallet.build({ type: "transfer", to: recipient.signed, amount: drops.toString() }, memo);
      // What it will do and whether the chain will take it; the review still works without it.
      const check = await checkTx(tx, wallet.keyId).catch(() => null);
      setReview({
        tx,
        display: recipient.display,
        proven: recipient.proven === true,
        byChain: recipient.byChain === true,
        nodeSays: recipient.address,
        pf: check?.pf ?? null,
        blockMs: check?.blockMs ?? 1000,
      });
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
    // A first payment opens the recipient's account, which burns the creation fee from this one (v1).
    const opening = review.pf && review.pf.cost > amt + fee ? review.pf.cost - amt - fee : 0n;
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
              {/* Always show exactly what's signed: the address, or the name the chain resolves. */}
              <div className="mono small break" data-testid="signed-to">{review.tx.action.type === "transfer" ? review.tx.action.to : ""}</div>
              {review.byChain && (
                <div className="small" data-testid="by-chain">
                  Signed as the name: the chain pays whoever owns {review.display} when this transaction runs, so your node can&apos;t point it at another account now. Your node says that&apos;s <span className="mono break">{review.nodeSays}</span> (not proven). A dishonest node could still hold the transaction and release it later (its validity window counts from the height that node reports), after the name changed hands: for a name about to expire, pay the address instead.
                </div>
              )}
              {!review.proven && !review.byChain && <div className="warn small">Looked up on your node only, not proven. For a large payment, check this address with the person you&apos;re paying.</div>}
            </dd>
            <dt>Amount</dt>
            <dd>{formatFang(amt)}</dd>
            <dt>Network fee</dt>
            <dd>{formatFang(fee)} <span className="dim small">at most (burned)</span></dd>
            {opening > 0n && (
              <>
                <dt>Opening their account</dt>
                <dd data-testid="opening">{formatFang(opening)} <span className="dim small">(burned: this is the first payment to it)</span></dd>
              </>
            )}
            <dt className="total">Total</dt>
            <dd className="total"><b>{formatFang(amt + fee + opening)}</b></dd>
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
      {linkCheck && <PayLinkNotice check={linkCheck} link={link} wallet={client.pin} />}
      <section className="card stack">
        <label className="field">
          <span>To</span>
          <input value={to} onChange={(e) => setTo(e.target.value)} placeholder={`alice.boo or ${client.pin.prefix ?? MAINNET_PREFIX}1…`} autoCapitalize="none" autoCorrect="off" spellCheck={false} />
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
      <Tip>
        {client.witnesses.length
          ? <>Names are proven against {client.witnesses.length + 1} independent nodes before you sign, and the review shows the exact address that gets paid.</>
          : <>A name is signed as the name: the chain pays whoever owns it when the transaction runs, so the node you&apos;re connected to can&apos;t point it at another account now (a dishonest one could only hold the transaction and release it later, by when the name may have changed hands). The review shows exactly what gets signed.</>}
      </Tip>
    </main>
  );
}
