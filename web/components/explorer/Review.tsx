"use client";

import "@/app/explorer.css";
import type { Tx } from "@blockula/sdk";
import { Fragment, useEffect, useRef, type ReactNode } from "react";
import { shortAddr } from "@/lib/format";

/**
 * The one layout every review before a signature uses: a line that names the action and its amount,
 * a short summary (who signs, who it goes to, the amount, the most it costs, the total), then
 * everything technical behind one closed "Details" disclosure, the exact transaction behind its
 * own, and the buttons. Nothing a signer needs is dropped: it is one tap away, not in the way.
 */
export interface ReviewRow {
  k: ReactNode;
  v: ReactNode;
  /** One short line under the value. */
  hint?: ReactNode;
  testid?: string;
  /** The total: ruled off and last in the summary. */
  total?: boolean;
}

export function ReviewRows({ rows, className = "" }: { rows: (ReviewRow | null | false | undefined)[]; className?: string }) {
  return (
    <dl className={`kv review-kv ${className}`}>
      {/* Keyed by place: a review may repeat a heading, and its rows never reorder. */}
      {rows.map((r, i) =>
        r ? (
          <Fragment key={i}>
            <dt className={r.total ? "total" : undefined}>{r.k}</dt>
            <dd className={r.total ? "total" : undefined} data-testid={r.testid}>
              {r.v}
              {r.hint && <span className="dim small block">{r.hint}</span>}
            </dd>
          </Fragment>
        ) : null,
      )}
    </dl>
  );
}

/** A name when there is one, else the short address; the full address is always on hover and in Details. */
export function Party({ name, address, label }: { name?: string | null; address: string; label?: string }) {
  if (name) return <span title={address}>{name}</span>;
  return (
    <span title={address}>
      {label ? `${label} · ` : ""}
      <span className="mono nowrap">{shortAddr(address)}</span>
    </span>
  );
}

/** Everything technical, in one place, closed until asked for. */
export function ReviewDetails({ rows = [], children, testid = "review-details" }: { rows?: (ReviewRow | null | false | undefined)[]; children?: ReactNode; testid?: string }) {
  return (
    <details className="raw review-details" data-testid={testid}>
      <summary>Details</summary>
      <div className="review-details-body">
        {rows.some(Boolean) && <ReviewRows rows={rows} />}
        {children}
      </div>
    </details>
  );
}

/** The exact transaction, as it will be signed (its signature is made on signing). */
export function RawTx({ tx, label = "The exact transaction you'll sign" }: { tx: Tx; label?: string }) {
  return (
    <details className="raw">
      <summary>{label}</summary>
      <pre>{JSON.stringify({ ...tx, public_key: `${tx.public_key.slice(0, 24)}…`, signature: `${tx.signature.length / 2} bytes, made when you sign` }, null, 2)}</pre>
    </details>
  );
}

/** A ref for a review: it scrolls into view when it appears, so its action line, total and buttons are on screen. */
export function useReveal<T extends HTMLElement>(on = true) {
  const ref = useRef<T>(null);
  useEffect(() => {
    if (on) ref.current?.scrollIntoView({ block: "nearest" });
  }, [on]);
  return ref;
}

export function ReviewCard({
  testid,
  className = "preview",
  eyebrow = "Read it twice",
  aside,
  action,
  actionTestid,
  rows,
  children,
  details,
  raw,
  actions,
  reveal = true,
}: {
  testid?: string;
  className?: string;
  eyebrow?: ReactNode;
  /** Beside the eyebrow (a status pill). */
  aside?: ReactNode;
  /** One plain line: the action and its amount. */
  action: ReactNode;
  actionTestid?: string;
  /** The summary: From, To, Amount, Max fee, Total. */
  rows: (ReviewRow | null | false | undefined)[];
  /** Warnings and blockers: always in view, between the summary and the disclosures. */
  children?: ReactNode;
  /** The Details disclosure (`<ReviewDetails>`). */
  details?: ReactNode;
  /** The exact-transaction disclosure (`<RawTx>`). */
  raw?: ReactNode;
  /** Back and the primary button. */
  actions?: ReactNode;
  /** Scroll the review into view when it appears (it opens under the form that asked for it). */
  reveal?: boolean;
}) {
  const ref = useReveal<HTMLDivElement>(reveal);
  return (
    <div className={`review ${className}`} aria-live="polite" data-testid={testid} ref={ref}>
      {aside ? (
        <div className="row-between">
          <span className="eyebrow">{eyebrow}</span>
          {aside}
        </div>
      ) : (
        <span className="eyebrow">{eyebrow}</span>
      )}
      <p className="headline review-action" data-testid={actionTestid}>
        {action}
      </p>
      <ReviewRows rows={rows} />
      {children}
      {details}
      {raw}
      {actions && <div className="row-gap review-actions">{actions}</div>}
    </div>
  );
}
