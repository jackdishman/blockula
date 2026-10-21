"use client";

import "@/app/money.css";
import { Fragment, type ReactNode } from "react";
import { Spinner } from "@/components/ui";

/**
 * A review step before staking, unstaking, moving stake or voting (USER-4): what gets signed, the
 * terms that come with it, and Confirm. Nothing is signed until Confirm; Edit goes back to the
 * form with what was typed still there.
 */
export function ReviewStep({
  title,
  summary,
  rows,
  terms,
  confirm,
  busy,
  onBack,
  onConfirm,
}: {
  title: string;
  summary: ReactNode;
  rows: [string, ReactNode][];
  terms: string[];
  confirm: string;
  busy: boolean;
  onBack: () => void;
  onConfirm: () => void;
}) {
  return (
    <section className="money-review grow" data-testid="review" aria-label={title}>
      <div className="section-head">
        <span className="eyebrow">Read it twice</span>
        <h3>{title}</h3>
      </div>
      <p className="summary money-wrap">{summary}</p>
      <dl className="kv">
        {/* Keyed by place: a review may repeat a heading, and its rows never reorder. */}
        {rows.map(([k, v], i) => (
          <Fragment key={i}>
            <dt>{k}</dt>
            <dd>{v}</dd>
          </Fragment>
        ))}
      </dl>
      {/* DRAFT copy for counsel (D4): lib/money-copy.ts. */}
      <ul className="money-terms" data-testid="review-terms">
        {terms.map((t) => (
          <li key={t}>{t}</li>
        ))}
      </ul>
      <div className="money-actions">
        <button type="button" className="btn" disabled={busy} onClick={onBack}>
          ← Edit
        </button>
        <button type="button" className="btn primary grow" disabled={busy} onClick={onConfirm}>
          {busy ? <Spinner>Sealing into a block…</Spinner> : confirm}
        </button>
      </div>
    </section>
  );
}
