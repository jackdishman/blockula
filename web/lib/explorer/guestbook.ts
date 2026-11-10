/**
 * The guestbook (app/guestbook): a `.boo` name that anyone signs by paying it a little FANG with a
 * memo. A signature is a payment that ran and credited the account the name points to; its memo is
 * whatever the payer wrote. Pure and dependency-free (only type imports and inert.ts), so
 * `node --test` runs it directly.
 */
import { inertText } from "./inert.ts";
import type { Label, TxDetail } from "./types";

/** The name the guestbook page reads, without `.boo`. */
export const GUESTBOOK_NAME = "guestbook";

/** What the page suggests a signature costs. Any amount signs it. */
export const GUESTBOOK_AMOUNT = "0.1";

/** The most characters of a memo the wall shows (the chain allows 256 bytes). */
export const MEMO_MAX = 200;

export interface Signature {
  id: string;
  height: number;
  from: Label;
  /** The memo, made fit to show (inertText): empty when the payer wrote none. */
  memo: string;
  amount: string | null;
  amount_display: string | null;
}

/**
 * The signatures among an account's transactions (`/api/account/{id}/txs`, newest first): finalized
 * transfers that paid `owner` (the tx's `to`, the account the chain credited when it ran, never
 * the name it was signed to) from someone else. Failed, pending and outgoing ones aren't
 * signatures. Order is kept, and a tx that appears twice (two pages overlapping) is shown once.
 */
export function signaturesOf(txs: TxDetail[], owner: string): Signature[] {
  const seen = new Set<string>();
  const out: Signature[] = [];
  for (const t of txs) {
    if ((t.kind ?? t.action?.type) !== "transfer" || t.status !== "finalized" || t.error) continue;
    if (!t.to || t.to.address !== owner || !t.from || t.from.address === owner || t.height == null) continue;
    if (seen.has(t.id)) continue;
    seen.add(t.id);
    out.push({ id: t.id, height: t.height, from: t.from, memo: inertText(t.memo ?? "", MEMO_MAX), amount: t.amount ?? null, amount_display: t.amount_display ?? null });
  }
  return out;
}
