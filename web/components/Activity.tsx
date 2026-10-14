"use client";

import { explorer } from "@/lib/config";
import type { TxView } from "@/lib/types";
import { ExplorerLink } from "./ui";

/** A ledger column: what happened, and which way the money went. */
const kind = (t: TxView, me: string): [string, string] => {
  const k: string = t.kind;
  if (k === "register_name") return ["Name", ""];
  if (k === "add_key") return ["Key added", ""];
  if (k === "remove_key") return ["Revoked", ""];
  if (k !== "transfer") return [k.replace(/_/g, " "), ""];
  return t.from.address === me ? ["Sent", "out"] : ["Received", "in"];
};

export function Activity({ txs, me }: { txs: TxView[]; me: string }) {
  if (!txs.length) return <p className="dim">Nothing yet. Your transactions will appear here once they&apos;re sealed into a block.</p>;
  return (
    <ul className="activity">
      {txs.map((t) => {
        const [label, dir] = kind(t, me);
        return (
          <li key={t.id}>
            <ExplorerLink href={explorer.tx(t.id)} className="activity-row">
              <span className={`activity-kind ${dir}`}>{label}</span>
              <span className="activity-main">
                <span>{t.summary}</span>
                {t.memo && <span className="activity-memo">“{t.memo}”</span>}
              </span>
              <span className="activity-height nowrap">{t.height ? `#${t.height}` : "pending"}</span>
            </ExplorerLink>
          </li>
        );
      })}
    </ul>
  );
}
