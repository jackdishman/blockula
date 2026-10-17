/**
 * An account's activity as CSV, for spreadsheets and bookkeeping: one row per transaction,
 * amounts exact and signed from the account's side. Pure, so `node --test` runs it directly.
 */
import type { KeyView } from "../types";
import { counterparty, deltaFor, effectsOf, plainDrops } from "./ledger.ts";
import type { TxDetail } from "./types";

export const CSV_COLUMNS = ["block", "status", "kind", "direction", "counterparty", "counterparty_address", "amount_fang", "fee_fang", "signed_by", "memo", "summary", "tx_id"] as const;

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
  const rows = txs.map((t) => {
    const d = deltaFor(t, side);
    const other = counterparty(t, side);
    const paidFee = t.from?.address === side && t.status !== "pending" ? effectsOf(t).burned : 0n;
    const key = t.from?.address === side && t.signer_key ? keys.find((k) => k.key_id === t.signer_key) : undefined;
    const signer = key ? `${key.label}${key.role.type === "session" ? " (agent)" : ""}` : t.from?.address === side && t.signer_key ? t.signer_key : "";
    const otherName = other ? ("holding" in other ? other.name : plain(other.name) || other.short) : "";
    const otherAddr = other && !("holding" in other) ? other.address : "";
    return [
      cell(t.height, false),
      cell(t.status),
      cell(t.kind ?? t.action?.type ?? ""),
      cell(d.direction === "none" ? "" : d.direction),
      cell(otherName),
      cell(otherAddr),
      cell(plainDrops(d.delta), false),
      cell(paidFee ? plainDrops(paidFee) : "", false),
      cell(signer),
      cell(t.memo ?? ""),
      cell(t.summary),
      cell(t.id),
    ].join(",");
  });
  return [CSV_COLUMNS.join(","), ...rows].join("\r\n") + "\r\n";
}

