"use client";

import "@/app/money.css";
import type { ReactNode } from "react";
import { ReviewRows } from "@/components/explorer/Review";
import { Spinner } from "@/components/ui";

/**
 * A review step before staking, unstaking, moving stake or voting (USER-4): one line naming the
 * action, a compact summary (the rows of every review: components/explorer/Review.tsx), the terms
 * that come with it, and Confirm. Nothing is signed until Confirm; Edit goes back to the
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
    <section className="money-review review grow" data-testid="review" aria-label={title}>
      <div className="section-head">
        <span className="eyebrow">Read it twice</span>
        <h3>{title}</h3>
      </div>
      <p className="summary money-wrap review-action">{summary}</p>
      <ReviewRows rows={rows.map(([k, v]) => ({ k, v }))} />
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
