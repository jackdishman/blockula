"use client";

import Link from "next/link";
import { deltaFor, formatDrops, fromMySide, knownKind, type Me } from "@/lib/explorer/ledger";
import { to } from "@/lib/explorer/routes";
import type { TxDetail } from "@/lib/explorer/types";
import type { KeyView } from "@/lib/types";
import { Amount, ageOf, StatusPill } from "./kit";

const KIND: Record<string, string> = {
  register_name: "Name",
  add_key: "Key added",
  remove_key: "Revoked",
  create_hunt: "Hunt",
  fund_hunt: "Funded",
  award_hunt: "Awarded",
  cancel_hunt: "Cancelled",
  fund_pool: "Donated",
  register_familiar: "Familiar",
  link_github: "GitHub",
  unlink_github: "GitHub",
  submit_claim: "Claim",
  attest_claim: "Attested",
  join_jury: "Jury",
  leave_jury: "Jury",
  commit_votes: "Sealed",
  reveal_votes: "Revealed",
  withdraw_vested: "Withdrew",
  vouch: "Vouched",
  register_validator: "Validator",
  approve_validator: "Approved",
  remove_validator: "Removed",
};

/** The ledger column: what happened, read from `side` (the account the list is about). */
export function kindLabel(tx: TxDetail, side: string | null) {
  const kind = tx.kind ?? tx.action?.type ?? "";
  if (tx.status === "failed") return { label: "Failed", dir: "failed" };
  const d = side ? deltaFor(tx, side).direction : "none";
  if (kind === "transfer") {
    if (d === "in") return { label: "Received", dir: "in" };
    if (d === "out") return { label: "Sent", dir: "out" };
    if (d === "self") return { label: "To self", dir: "" };
    return { label: "Transfer", dir: "" };
  }
  return { label: KIND[kind] ?? kind.replace(/_/g, " "), dir: d === "out" ? "out" : d === "in" ? "in" : "" };
}

/** Which key signed it, when we know that account's keys: "via claude" for an agent. */
export function signerOf(tx: TxDetail, keys: KeyView[] | undefined, myKeyId?: string) {
  if (!tx.signer_key || !keys?.length) return null;
  const k = keys.find((x) => x.key_id === tx.signer_key);
  if (!k) return { label: "a revoked key", agent: false, mine: false };
  return { label: k.label, agent: k.role.type === "session", mine: k.key_id === myKeyId };
}

export function Ledger({
  txs,
  side,
  me,
  keys,
  myKeyId,
  head,
  blockMs,
  fresh,
  empty = "Nothing yet.",
}: {
  txs: TxDetail[];
  /** Amounts are signed from this account's side (the page's account), if set. */
  side?: string | null;
  /** The viewer, for "you" in summaries. */
  me?: Me | null;
  /** The side account's keys, to name the signer of each transaction it sent. */
  keys?: KeyView[];
  myKeyId?: string;
  head?: number;
  blockMs?: number;
  fresh?: ReadonlySet<string>;
  empty?: string;
}) {
  if (!txs.length) return <p className="ledger-empty">{empty}</p>;
  return (
    <ul className="ledger">
      {txs.map((t) => {
        const { label, dir } = kindLabel(t, side ?? null);
        // A kind this explorer can't read shows its amount as the node reports it, unsigned.
        const signed = side && knownKind(t.kind ?? t.action?.type ?? "") ? deltaFor(t, side) : null;
        const signer = side && t.from?.address === side ? signerOf(t, keys, myKeyId) : null;
        return (
          <li key={t.id} className={fresh?.has(t.id) ? "fresh" : undefined}>
            <Link href={to.tx(t.id)} className="ledger-row">
              <span className={`ledger-kind ${dir}`}>{label}</span>
              <span className="ledger-main">
                <span className="ledger-summary">{fromMySide(t.summary, me ?? null)}</span>
                {(t.memo || (signer && !signer.mine) || t.status !== "finalized") && (
                  <span className="ledger-meta">
                    {t.status !== "finalized" && <StatusPill status={t.status} />}
                    {/* Your own device is the default; only other keys (an agent, another device) are called out. */}
                    {signer && !signer.mine && <span className={`chip ${signer.agent ? "agent" : ""}`}>{signer.agent ? `agent · ${signer.label}` : signer.label}</span>}
                    {t.memo && <span className="ledger-memo">“{t.memo}”</span>}
                    {t.status === "failed" && t.error && <span className="error">{t.error}</span>}
                  </span>
                )}
              </span>
              {signed && signed.direction !== "none" && t.amount ? (
                <Amount drops={signed.delta} signed />
              ) : t.amount ? (
                <Amount drops={t.amount} />
              ) : signed && signed.direction === "out" ? (
                // Only the fee moved: say so quietly.
                <span className="amt dim small" title={`fee ${formatDrops(-signed.delta)}`}>
                  fee
                </span>
              ) : (
                <span />
              )}
              <span className="ledger-when" title={t.height ? `block #${t.height}` : "waiting for a block"}>
                {t.height ? `#${t.height}` : "pending"}
                {t.height != null && head != null && (
                  <span className="block" style={{ fontSize: 12 }}>
                    {ageOf(t.height, head, blockMs)}
                  </span>
                )}
              </span>
            </Link>
          </li>
        );
      })}
    </ul>
  );
}
