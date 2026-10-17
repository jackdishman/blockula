/**
 * An account's activity as CSV, for spreadsheets and bookkeeping: one row per transaction, signed
 * from the account's side. `exact` says whether the row's amount is everything the transaction
 * moved for this account; when it isn't (a name's price, a bounty paid from escrow, the fee for
 * opening a recipient's account), `note` says what the transaction doesn't record. Pure, so
 * `node --test` runs it directly.
 */
import type { KeyView } from "../types";
import { contextsOf, counterparty, deltaFor, effectsOf, parseDrops, plainDrops, uncertainFor } from "./ledger.ts";
import type { TxDetail } from "./types";

export const CSV_COLUMNS = ["block", "status", "kind", "direction", "counterparty", "counterparty_address", "amount_fang", "fee_fang", "exact", "note", "signed_by", "memo", "summary", "tx_id"] as const;

/**
 * One cell. Quoted when it holds a comma, quote or line break; and text that a spreadsheet
 * would run as a formula (starting with = + - @ or a tab) gets a leading apostrophe, since memos
 * and labels are written by strangers.
 */
export function cell(v: string | number | null | undefined, text = true): string {
  let s = v == null ? "" : String(v);
  if (text && /^[=+\-@\t\r]/.test(s)) s = `'${s}`;
  return /[",\r\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

const plain = (s: string | null) => (s ?? "").replace(/^[\p{Extended_Pictographic}️‍]+\s*/u, "");

export function activityCsv(txs: TxDetail[], side: string, keys: KeyView[] = []): string {
  const ctxOf = contextsOf(txs);
  const rows = txs.map((t) => {
    const ctx = ctxOf(t);
    const fx = effectsOf(t, ctx);
    const d = deltaFor(t, side, ctx);
    const other = counterparty(t, side);
    // What the sender paid in fees: the charge (base fee burned + tip), as its balance saw it.
    const paidFee = t.from?.address === side && t.status !== "pending" ? parseDrops(t.fee) : 0n;
    const key = t.from?.address === side && t.signer_key ? keys.find((k) => k.key_id === t.signer_key) : undefined;
    const signer = key ? `${key.label}${key.role.type === "session" ? " (agent)" : ""}` : t.from?.address === side && t.signer_key ? t.signer_key : "";
    const otherName = other ? ("holding" in other ? other.name : plain(other.name) || other.short) : "";
    const otherAddr = other && !("holding" in other) ? other.address : "";
    const exact = !uncertainFor(fx, t, side);
    return [
      cell(t.height, false),
      cell(t.status),
      cell(t.kind ?? t.action?.type ?? ""),
      cell(d.direction === "none" ? "" : d.direction),
      cell(otherName),
      cell(otherAddr),
      cell(plainDrops(d.delta), false),
      cell(paidFee ? plainDrops(paidFee) : "", false),
      cell(exact ? "yes" : "no"),
      cell(exact ? "" : (fx.unseen ?? "")),
      cell(signer),
      cell(t.memo ?? ""),
      cell(t.summary),
      cell(t.id),
    ].join(",");
  });
  return [CSV_COLUMNS.join(","), ...rows].join("\r\n") + "\r\n";
}
