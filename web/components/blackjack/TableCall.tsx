"use client";

// One call of the blackjack table, signed the way every program call in this wallet is
// (components/explorer/ProgramCalls.tsx): the SDK builds the exact transaction without signing it,
// the review shows it (the program, the method, its input, the FANG sent, the storage limit, the
// full version digest it binds, its fuel and the most it can cost), and only then does an owner key
// sign it, right after a fresh passkey check. A transaction that was sent is never offered again:
// one whose outcome isn't known is checked by its id.
import { abortWords, eventsOf, type TableCall, type TableEvent } from "@blockula/blackjack";
import { checkReceipt, describeAction, loadFuel, outcomeWords, ProgramAnswerError, TxFailedError, TxPendingError, txId, type Action, type ProgramInfo, type ProgramReceipt, type Tx } from "@blockula/sdk";
import Link from "next/link";
import { Fragment, useCallback, useEffect, useState, type ReactNode } from "react";
import { useInlineUnlock } from "@/components/explorer/ActionForm";
import { Amount, Named, Term } from "@/components/explorer/kit";
import { useMultisig } from "@/components/shared";
import { ErrorNote, Spinner } from "@/components/ui";
import { COPY } from "@/lib/blackjack-copy";
import { inertText } from "@/lib/explorer/inert";
import { formatDrops } from "@/lib/explorer/ledger";
import { NodeError, submitTx } from "@/lib/explorer/node";
import { sameCall } from "@/lib/explorer/program-calls";
import { to } from "@/lib/explorer/routes";
import { chainUnit, shortAddr } from "@/lib/format";
import { PasskeyCheckError } from "@/lib/keys";
import { previewer, unsignedOf, useSpendCheck } from "@/lib/spend";
import { useWallet } from "@/lib/wallet";

const words = (e: unknown) => inertText(e instanceof Error ? e.message : String(e), 600);

/** Who's here, for a call: only an owner key of a personal account signs one. */
export type Caller = { kind: "signed-out"; hint: boolean } | { kind: "reading" } | { kind: "agent"; label: string } | { kind: "shared" } | { kind: "owner"; label: string };

/** What the page asks to have signed: the call, and how to say it. */
export interface CallRequest {
  call: TableCall;
  /** What it does, in the page's words: the review's headline. */
  does: string;
  /** Extra lines for the review (the bet's seed). */
  notes?: { k: string; v: ReactNode }[];
  /** The transaction's id, the moment it's signed (before the node answers). */
  onSigned?: (id: string) => void;
  /** Its receipt's table events, once it ran. */
  onApplied?: (id: string, events: TableEvent[]) => void;
}

interface Review {
  req: CallRequest;
  tx: Tx;
  fuel: { run: bigint; load: bigint };
  /** The SDK's own sentence for the action built. */
  action: string;
}

type Sent =
  | { state: "waiting"; id: string; does: string }
  | { state: "applied"; id: string; does: string; summary: string }
  | { state: "failed"; id: string; does: string; words: string; message: string | null }
  | { state: "refused"; id: string; does: string; words: string }
  | { state: "unsure"; id: string; does: string; words: string; validUntil: number };

export interface TableSigner {
  caller: Caller;
  /** The program's record, checked by the SDK: what every call binds. */
  info: ProgramInfo | null;
  /** Why the record can't be used (it doesn't check out, or the node can't be read). */
  infoError: string | null;
  /** A call is being built, reviewed, signed or waited on. */
  active: boolean;
  /** Builds `req` for review. Nothing is signed. */
  start: (req: CallRequest) => Promise<void>;
  /** The review, the outcome and any error: render it where the call was asked for. */
  panel: ReactNode;
  /** Unlock (or sign in), for the pages' own buttons. */
  unlock: () => Promise<void>;
  unlocking: boolean;
}

/** The action a table call is, as the transaction must carry it (only `fuel_limit` is the SDK's to fill). */
function actionOf(program: string, digest: string, c: TableCall): Action {
  return { type: "call_program", calls: [{ program, version_digest: digest, method: c.method, input: c.input, value: (c.value ?? 0n).toString() }], fuel_limit: 0, storage_limit: (c.storageLimit ?? 0n).toString(), bindings: [] };
}

/** The review-then-sign flow for one table: at most one call at a time. */
export function useTableSigner(program: string, onDone: () => void): TableSigner {
  const { client, wallet, account, hint, refresh } = useWallet();
  const inlineUnlock = useInlineUnlock();
  const check = useSpendCheck();
  const [read, setRead] = useState<{ info: ProgramInfo } | { error: string } | null>(null);
  const [review, setReview] = useState<Review | null>(null);
  const [sent, setSent] = useState<Sent | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const readInfo = useCallback(
    () =>
      client.program(program).then(
        (info) => (setRead({ info }), info),
        (e) => {
          setRead({ error: e instanceof ProgramAnswerError ? `Your node's record of this program doesn't check out: ${words(e)}` : `Couldn't read the program's record: ${words(e)}` });
          return null;
        },
      ),
    [client, program],
  );
  useEffect(() => {
    setRead(null);
    void readInfo();
  }, [readInfo]);

  const signedIn = !!wallet && !!hint?.account;
  const { ms } = useMultisig(signedIn ? wallet.account : null);
  const myKey = account?.keys.find((k) => k.key_id === wallet?.keyId);
  const caller: Caller = !signedIn ? { kind: "signed-out", hint: !!hint?.account } : !account ? { kind: "reading" } : myKey?.role.type === "session" ? { kind: "agent", label: myKey.label } : ms ? { kind: "shared" } : { kind: "owner", label: myKey?.label ?? "this device" };
  const info = read && "info" in read ? read.info : null;

  const start = async (req: CallRequest) => {
    if (!wallet || caller.kind !== "owner") return;
    setBusy("review");
    setError(null);
    setSent(null);
    setReview(null);
    try {
      // The record as it is now: the review shows the digest the call will bind, whole.
      const now = await readInfo();
      if (!now) throw new Error("The program's record couldn't be read, so there's nothing to review. Nothing was signed.");
      const built: { tx?: Tx } = {};
      const stop = new Error("built for review");
      try {
        await previewer(wallet).call(program, req.call.method, req.call.input, {
          value: req.call.value ?? 0n,
          versionDigest: now.version_digest,
          storageLimit: req.call.storageLimit ?? 0n,
          waitMs: 15_000,
          onSigned: (tx) => {
            built.tx = tx;
            throw stop;
          },
        });
      } catch (e) {
        if (e !== stop) throw e;
      }
      const tx = built.tx;
      if (!tx) throw new Error("The call wasn't built, so there's nothing to review. Nothing was signed.");
      if (tx.action.type !== "call_program" || !sameCall(tx.action, actionOf(program, now.version_digest, req.call))) throw new Error("The transaction built for this call isn't the call this page describes, so it isn't offered for signing. Nothing was signed.");
      setReview({ req, tx, fuel: { run: BigInt(tx.action.fuel_limit), load: loadFuel(now.code.size ?? 0) }, action: describeAction(tx.action) });
    } catch (e) {
      setError(words(e));
    } finally {
      setBusy(null);
    }
  };

  const send = async () => {
    if (!review) return;
    const { req } = review;
    setError(null);
    let signed: Tx;
    try {
      setBusy("check");
      const signer = await check();
      signed = signer.sign(unsignedOf(review.tx));
    } catch (e) {
      setError(e instanceof PasskeyCheckError ? e.message : words(e));
      setBusy(null);
      return;
    }
    let id = txId(signed);
    req.onSigned?.(id);
    setBusy(null);
    setReview(null);
    setSent({ state: "waiting", id, does: req.does });
    try {
      id = await submitTx(signed);
    } catch (e) {
      // A 4xx is the node refusing it at admission: it never ran and nothing was charged. Anything else, it may have arrived.
      if (e instanceof NodeError && e.status >= 400 && e.status < 500) setSent({ state: "refused", id, does: req.does, words: words(e) });
      else setSent({ state: "unsure", id, does: req.does, words: `It may have reached your node: ${words(e)}.`, validUntil: signed.valid_until });
      return;
    }
    refresh();
    try {
      const done = await client.waitForTx(id, 30_000, signed.valid_until);
      let receipt: ProgramReceipt | null = null;
      try {
        receipt = checkReceipt(client.rpc, done);
      } catch {
        // A receipt that doesn't check out is no receipt: the table's records still say what happened.
      }
      let events: TableEvent[] = [];
      try {
        if (receipt) events = eventsOf(receipt, program);
      } catch {
        // Events that aren't the table's layout are left out.
      }
      req.onApplied?.(id, events);
      setSent({ state: "applied", id, does: req.does, summary: String(done.summary ?? "") });
    } catch (e) {
      if (e instanceof TxFailedError && e.dropped) setSent({ state: "unsure", id, does: req.does, words: `Your node dropped it without running it, so nothing was charged (${inertText(e.reason, 300)}). Another node may still include it.`, validUntil: signed.valid_until });
      else if (e instanceof TxFailedError) {
        let receipt: ProgramReceipt | null = null;
        try {
          receipt = checkReceipt(client.rpc, e.tx);
        } catch {
          // Its receipt doesn't check out: the node's own reason stands.
        }
        const failed = receipt && !receipt.outcome.ok ? receipt.outcome : null;
        // An abort is the table saying no: its code, in the table's words. Anything else, the chain's.
        const said = failed?.code === "aborted" ? abortWords(failed.abort_code) : failed ? `${outcomeWords(failed, req.call.storageLimit ?? 0n)}.` : `${inertText(e.reason, 600)}.`;
        setSent({ state: "failed", id, does: req.does, words: said, message: failed?.untrusted_message ?? null });
      } else if (e instanceof TxPendingError) setSent({ state: "unsure", id, does: req.does, words: "Submitted, and not in a block yet.", validUntil: signed.valid_until });
      else setSent({ state: "unsure", id, does: req.does, words: `It may have reached your node: ${words(e)}.`, validUntil: signed.valid_until });
    } finally {
      refresh();
      onDone();
    }
  };

  const unlock = async () => {
    setBusy("unlock");
    setError(null);
    try {
      await inlineUnlock();
    } catch (e) {
      setError(words(e));
    } finally {
      setBusy(null);
    }
  };

  const who = account?.names[0] ?? shortAddr(wallet?.account ?? "");
  const panel = (
    <>
      {busy === "review" && <Spinner>Preparing…</Spinner>}
      {review && info && (
        <div className="preview" aria-live="polite" data-testid="bj-review">
          <span className="eyebrow">{COPY.review.eyebrow}</span>
          <p className="headline" data-testid="bj-review-does">{review.req.does}</p>
          <dl className="kv">
            <dt>From</dt>
            <dd>
              {who} <span className="dim small">signed by “{inertText(caller.kind === "owner" ? caller.label : "", 40)}” (owner)</span>
            </dd>
            <dt>Program</dt>
            <dd>
              <span className="mono break" data-testid="bj-review-program">{program}</span>
              <span className="dim small block">{review.action}</span>
            </dd>
            <dt>Method</dt>
            <dd>
              <Named text={review.req.call.method} /> <span className="dim small">named by the program&apos;s deployer</span>
            </dd>
            <dt>Input</dt>
            <dd>
              <span className="mono break" data-testid="bj-review-input">{review.req.call.input || "(empty)"}</span>
            </dd>
            {(review.req.notes ?? []).map((n) => (
              <Fragment key={n.k}>
                <dt>{n.k}</dt>
                <dd>{n.v}</dd>
              </Fragment>
            ))}
            <dt>FANG sent</dt>
            <dd data-testid="bj-review-value">
              <Amount drops={review.req.call.value ?? 0n} exact />
            </dd>
            <dt>Storage limit</dt>
            <dd data-testid="bj-review-storage">
              <Amount drops={review.req.call.storageLimit ?? 0n} exact /> <span className="dim small">{(review.req.call.storageLimit ?? 0n) === 0n ? "this call stores nothing new" : "at most, bonded as a deposit for what it stores"}</span>
            </dd>
            <dt>
              <Term k="digest">Version digest</Term>
            </dt>
            <dd>
              <span className="mono break" data-testid="bj-review-digest">{info.version_digest}</span>
              <span className="dim small block">
                Version {info.version}. {COPY.review.digest}
              </span>
            </dd>
            <dt>
              <Term k="fuel">Fuel</Term>
            </dt>
            <dd>
              {(review.fuel.run + review.fuel.load).toLocaleString("en-US")} reserved <span className="dim small">paid for whether it&apos;s used or not</span>
            </dd>
            <dt>
              <Term k="fee">Max fee</Term>
            </dt>
            <dd data-testid="bj-review-fee">
              {formatDrops(BigInt(review.tx.fee), chainUnit())} <span className="dim small">{COPY.review.fee}</span>
            </dd>
            <dt className="total">Total</dt>
            <dd className="total" data-testid="bj-review-total">
              <b>
                <Amount drops={BigInt(review.tx.fee) + (review.req.call.value ?? 0n) + (review.req.call.storageLimit ?? 0n)} exact />
              </b>{" "}
              <span className="dim small">{COPY.review.total}</span>
            </dd>
          </dl>
          <details className="raw">
            <summary>{COPY.review.exact}</summary>
            <pre>{JSON.stringify({ ...review.tx, public_key: `${review.tx.public_key.slice(0, 24)}…`, signature: `${review.tx.signature.length / 2} bytes, made when you sign` }, null, 2)}</pre>
          </details>
          <div className="row-gap">
            <button className="btn" disabled={!!busy} onClick={() => setReview(null)}>
              {COPY.review.back}
            </button>
            <button className="btn grow primary" disabled={!!busy} onClick={send} data-testid="bj-sign">
              {busy === "check" ? <Spinner>Waiting for your passkey…</Spinner> : COPY.review.sign}
            </button>
          </div>
        </div>
      )}
      {sent && (
        <div className="preview" aria-live="polite" data-testid="bj-sent" data-state={sent.state}>
          <span className="eyebrow">{sent.does}</span>
          {sent.state === "waiting" && <Spinner>{COPY.sent.waiting}</Spinner>}
          {sent.state === "applied" && (
            <p className="m0">
              {COPY.sent.applied} <span className="dim small">{inertText(sent.summary, 200)}</span>
            </p>
          )}
          {sent.state === "refused" && (
            <>
              <p className="m0">{COPY.sent.refused}</p>
              <p className="m0 warn" data-testid="bj-sent-words">{sent.words}</p>
            </>
          )}
          {sent.state === "failed" && (
            <>
              <p className="m0">{COPY.sent.failed}</p>
              <p className="m0 warn" data-testid="bj-sent-words">{sent.words}</p>
              {sent.message && (
                <p className="m0 dim small">
                  The program&apos;s own words: <Named text={sent.message} max={200} />
                </p>
              )}
            </>
          )}
          {sent.state === "unsure" && (
            <p className="m0 warn" data-testid="bj-sent-words">
              {sent.words} It can run until block #{sent.validUntil.toLocaleString("en-US")}. {COPY.sent.unsure}
            </p>
          )}
          <div className="row-gap">
            <Link href={to.tx(sent.id)} data-testid="bj-sent-tx">
              {COPY.sent.see}
            </Link>
            {sent.state !== "waiting" && sent.state !== "unsure" && (
              <button className="btn small" onClick={() => setSent(null)}>
                {COPY.sent.ok}
              </button>
            )}
          </div>
        </div>
      )}
      <ErrorNote error={error} />
    </>
  );

  return {
    caller,
    info,
    infoError: read && "error" in read ? read.error : null,
    active: !!busy || !!review || sent?.state === "waiting",
    start,
    panel,
    unlock,
    unlocking: busy === "unlock",
  };
}

/** What stands in for the play buttons when this visitor can't sign a call: why, and what to do. */
export function CallerNote({ signer }: { signer: TableSigner }) {
  const c = signer.caller;
  if (c.kind === "owner") return null;
  if (c.kind === "reading") return <Spinner>Reading your account…</Spinner>;
  if (c.kind === "agent") return <p className="warn small m0" data-testid="bj-blocked">{COPY.signIn.agent}</p>;
  if (c.kind === "shared") return <p className="warn small m0" data-testid="bj-blocked">{COPY.signIn.shared}</p>;
  if (c.hint)
    return (
      <div className="stack" data-testid="bj-sign-in">
        <span className="small">{COPY.signIn.locked}</span>
        <button className="btn primary self-start" disabled={signer.unlocking} onClick={signer.unlock}>
          {signer.unlocking ? <Spinner>Waiting for your passkey…</Spinner> : "Unlock"}
        </button>
      </div>
    );
  return (
    <p className="small m0" data-testid="bj-sign-in">
      <Link href="/welcome">{COPY.signIn.none}</Link>
    </p>
  );
}
