"use client";

import "@/app/money.css";
import { isAddress, parseFang, type Tx } from "@blockula/sdk";
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
import { movable, type Preflight } from "@/lib/explorer/preflight";
import type { TxDetail } from "@/lib/explorer/types";
import { displayFang, fangInput, shortAddr } from "@/lib/format";
import { lockWords, nameRisks, type LockView, type NameInfo } from "@/lib/money";
import { PAYMENTS_FINAL } from "@/lib/money-copy";
import { checkLinkChain, linkChain } from "@/lib/paylink";
import type { AccountView, TxView } from "@/lib/types";
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
  | { state: "ok"; signed: string; address: string; display: string; detail: string; name?: string; proven?: boolean; byChain?: boolean }
  | { state: "bad"; msg: string };

const utf8Len = (s: string) => new TextEncoder().encode(s).length;
const exact = (d: bigint) => displayFang(d, { exact: true });

/** What `/api/account` adds for v1 nodes: what can move now, and why the rest can't. */
type Account = AccountView & { transferable?: string; locks?: (LockView & { amount: string })[] };

/** The biggest lock, in words that follow "is locked" ("by your vote on proposal #2 until about 21:15"). */
const lockedBy = (a: Account, head: number, blockMs?: number) => {
  const l = [...(a.locks ?? [])].sort((x, y) => (BigInt(y.amount) > BigInt(x.amount) ? 1 : BigInt(y.amount) < BigInt(x.amount) ? -1 : 0))[0];
  return l ? lockWords(l, head, blockMs) : "by a vote or vesting";
};

/** What the account can send now (transferable less pending), from what the node says. */
const sendableOf = (a: Account) => movable({ label: a.account, balance: a.balance, transferable: a.transferable, recent: a.recent_txs as unknown as TxDetail[] });

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
        setR({ state: "ok", signed: res.signed, address: res.address, display: `${res.name}.boo`, name: res.name ?? undefined, detail, proven: res.proven, byChain: res.byChain });
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

interface Review {
  tx: Tx;
  display: string;
  proven: boolean;
  byChain: boolean;
  nodeSays: string;
  pf: Preflight | null;
  blockMs: number;
  /** When paying this name by name could reach someone else (CRE-5): it lapses soon, or changed hands lately. */
  risks: string[];
}

function Send() {
  const { wallet, account, client, status, refresh } = useUnlocked();
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
  const [priority, setPriority] = useState(false);
  useEffect(() => {
    if (!refused) return;
    setTo("");
    setAmount("");
    setMemo("");
  }, [refused]);
  const [review, setReview] = useState<Review | null>(null);
  const [sent, setSent] = useState<TxView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const recipient = useRecipient(to);

  // What can move now: v1 nodes report `transferable` (vote and vesting locks hold the rest), and
  // pending payments take theirs first (USER-5, TR-11).
  const can = account ? sendableOf(account as Account) : null;
  let drops: bigint | null = null;
  let amountErr: string | null = null;
  try {
    if (amount.trim()) {
      drops = parseFang(amount);
      if (drops === 0n) amountErr = "Enter more than zero.";
      else if (can && drops > can.available)
        amountErr = can.locked > 0n
          ? `You can send up to ${exact(can.available)} now: ${exact(can.locked)} of your balance is locked ${lockedBy(account as Account, status?.height ?? 0, status?.block_time_ms)}.`
          : can.pending > 0n
            ? `You can send up to ${exact(can.available)} now, after your pending transactions.`
            : `You only have ${exact(can.available)}.`;
    }
  } catch {
    amountErr = "Use a number like 1 or 0.25 (up to 9 decimals).";
  }
  const memoErr = utf8Len(memo) > 256 ? "Memos are limited to 256 bytes." : null;
  const self = recipient.state === "ok" && recipient.address === wallet.account;
  const canReview = recipient.state === "ok" && drops !== null && !amountErr && !memoErr && !self && !refused && linkCheck?.state !== "pending";

  /**
   * Builds the transfer. With Priority on, it tips the block's proposer one base fee on top (the
   * same again), well inside the chain's cap (`tip_cap_multiple` × the base fee, 20× by default).
   */
  const build = async (to: string, amount: bigint) => {
    const action = { type: "transfer" as const, to, amount: amount.toString() };
    const plain = await wallet.build(action, memo);
    return priority ? wallet.build(action, memo, { tip: BigInt(plain.fee) }) : plain;
  };

  /** Everything that can move now, less the fee and what opening their account burns (TR-11). */
  const fillMax = async () => {
    if (recipient.state !== "ok") return;
    setBusy(true);
    setError(null);
    try {
      const [probe, mine, theirs] = await Promise.all([build(recipient.signed, 1n), client.account(wallet.account) as Promise<Account>, client.account(recipient.signed).catch(() => null)]);
      const opening = BigInt((theirs as { creation_fee?: string } | null)?.creation_fee ?? "0");
      const free = sendableOf(mine).available;
      const max = free - BigInt(probe.fee) - opening;
      if (max > 0n) setAmount(fangInput(max));
      else setError(`Nothing left to send: the fee${opening ? " and opening their account" : ""} would take all ${exact(free)} you can send now.`);
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  const toReview = async () => {
    if (recipient.state !== "ok" || drops === null) return;
    setBusy(true);
    setError(null);
    try {
      // Build and sign now so the fee shown is exactly what will be paid.
      const tx = await build(recipient.signed, drops);
      // What it will do and whether the chain will take it, and whether the name is risky to pay
      // by name; the review still works without either.
      const [check, info] = await Promise.all([checkTx(tx, wallet.keyId).catch(() => null), recipient.name ? (client.name(recipient.name) as Promise<NameInfo>).catch(() => null) : null]);
      const blockMs = check?.blockMs ?? status?.block_time_ms ?? 1000;
      const head = status?.height ?? tx.valid_until - 600;
      setReview({
        tx,
        display: recipient.display,
        proven: recipient.proven === true,
        byChain: recipient.byChain === true,
        nodeSays: recipient.address,
        pf: check?.pf ?? null,
        blockMs,
        risks: info ? nameRisks(info, { name: recipient.display, head, validUntil: tx.valid_until, signedAsName: recipient.byChain === true, blockMs }) : [],
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
    const tip = BigInt(review.tx.tip ?? "0");
    const amt = BigInt((review.tx.action as { amount: string }).amount);
    // A first payment opens the recipient's account, which burns the creation fee from this one (v1).
    const opening = review.pf && review.pf.cost > amt + fee ? review.pf.cost - amt - fee : 0n;
    const me = account?.names[0];
    const signed = review.tx.action.type === "transfer" ? review.tx.action.to : "";
    const refusals = review.pf?.refusals.length ?? 0;
    // A refusal the chain's state settles (balance, locks): no "Send anyway", only Edit.
    const sure = review.pf?.certain === true;
    return (
      <main className="narrow stack-lg">
        <div className="section-head">
          <span className="eyebrow">Read it twice</span>
          <h1>Review</h1>
        </div>
        <section className="card stack">
          <p className="summary">
            {me ? `${me} sends` : "You send"} {exact(amt)} to {review.display}
          </p>
          <dl className="kv">
            <dt>To</dt>
            <dd>
              {/* One line: who it pays, then exactly what's signed (the address, or the name the chain resolves). */}
              <div className="money-payee" data-testid="payee">
                Pays{" "}
                {review.byChain ? (
                  <>
                    <b data-testid="signed-to">{signed}</b> · <span className="mono">{shortAddr(review.nodeSays)}</span> <span className="dim small">(your node&apos;s answer)</span>
                  </>
                ) : signed === review.nodeSays && review.display !== shortAddr(signed) ? (
                  <>
                    <b>{review.display}</b> · <span className="mono break" data-testid="signed-to">{signed}</span>
                  </>
                ) : (
                  <span className="mono break" data-testid="signed-to">{signed}</span>
                )}
              </div>
              {review.byChain && (
                <details className="money-why" data-testid="by-chain">
                  <summary>Why is this safe?</summary>
                  <p className="small">
                    Signed as the name: the chain pays whoever owns {review.display} when this transaction runs, so your node can&apos;t point it at another account now. Your node says that&apos;s <span className="mono break">{review.nodeSays}</span> (not proven). A dishonest node could still hold the transaction and release it later (its validity window counts from the height that node reports), after the name changed hands.
                  </p>
                </details>
              )}
              {review.proven && !review.byChain && review.display !== shortAddr(signed) && client.witnesses.length > 0 && (
                <div className="ok small">Proven by {client.witnesses.length} other node{client.witnesses.length === 1 ? "" : "s"}.</div>
              )}
              {!review.proven && !review.byChain && <div className="warn small">Looked up on your node only, not proven. For a large payment, check this address with the person you&apos;re paying.</div>}
              {review.risks.map((r) => (
                <p key={r} className="warn small money-note" data-testid="name-risk">
                  {r}
                </p>
              ))}
            </dd>
            <dt>Amount</dt>
            <dd>{exact(amt)}</dd>
            <dt>Network fee</dt>
            <dd>
              {exact(fee)} <span className="dim small">at most ({tip > 0n ? <>burned, but for a {exact(tip)} tip to the block&apos;s proposer</> : "burned"})</span>
            </dd>
            {opening > 0n && (
              <>
                <dt>Opening their account</dt>
                <dd data-testid="opening">{exact(opening)} <span className="dim small">(burned: this is the first payment to it)</span></dd>
              </>
            )}
            <dt className="total">Total</dt>
            <dd className="total"><b>{exact(amt + fee + opening)}</b></dd>
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
        {/* DRAFT copy for counsel (D4): lib/money-copy.ts. */}
        <p className="money-final" data-testid="payments-final">{PAYMENTS_FINAL}</p>
        <div className="row-gap">
          <button className="btn" disabled={busy} onClick={() => setReview(null)}>← Edit</button>
          {!sure && (
            <button className={`btn grow ${refusals ? "" : "primary"}`} disabled={busy} onClick={confirm}>
              {busy ? <Spinner>Sealing into a block…</Spinner> : refusals ? "Send anyway" : "Confirm & send"}
            </button>
          )}
        </div>
        {sure && <p className="dim small" data-testid="cannot-send">The chain would refuse this as it is, so there&apos;s nothing to confirm. Change the amount, or wait for FANG to arrive or unlock.</p>}
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
        <div className="name-status money-amount-row">
          {amountErr ? (
            <span className="warn">{amountErr}</span>
          ) : (
            <span className="dim small" data-testid="can-send">
              Can send now: {can ? exact(can.available) : "…"}
              {can && can.locked > 0n ? ` · ${displayFang(can.locked)} locked` : ""}
            </span>
          )}
          <button type="button" className="btn small ghost" disabled={recipient.state !== "ok" || self || busy || !can} onClick={fillMax} title={recipient.state === "ok" ? "Everything you can send now, less the fee" : "Choose who to pay first"}>
            Max
          </button>
        </div>
        <label className="field">
          <span>Memo <span className="dim">(optional, public)</span></span>
          <input value={memo} onChange={(e) => setMemo(e.target.value)} placeholder="trick or treat" />
        </label>
        {memoErr && <span className="warn">{memoErr}</span>}
        <label className="money-check">
          <input type="checkbox" checked={priority} onChange={(e) => setPriority(e.target.checked)} />
          <span>
            Priority <span className="dim small">(adds a tip for the block&apos;s proposer, the same again as the fee, so busy blocks take it first)</span>
          </span>
        </label>
        <button className="btn primary" disabled={!canReview || busy} onClick={toReview}>
          {busy ? <Spinner>Preparing…</Spinner> : "Review"}
        </button>
        <ErrorNote error={error} />
      </section>
      <Tip>
        {client.witnesses.length
          ? <>Names are proven against {client.witnesses.length + 1} independent nodes before you sign, and the review shows the exact address that gets paid.</>
          : <>A payment to a name is signed as the name: the chain pays whoever owns it when the payment runs. The review shows exactly what gets signed.</>}
      </Tip>
    </main>
  );
}
