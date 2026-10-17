"use client";

import "@/app/explorer.css";
import { formatDrops } from "@/lib/explorer/ledger";
import type { Preflight } from "@/lib/explorer/preflight";
import { blocksToHuman } from "@/lib/format";
import { Meter } from "./charts";
import { Amount, Term, Who } from "./kit";

/**
 * What a transaction will do, before it's signed: each side's balance change, your balance and
 * agent budget after it, and whether the chain will refuse it. A forecast from the chain's
 * state now (lib/explorer/preflight.ts); the node's answer is final.
 */
export function PreflightView({ p, me, blockMs = 1000 }: { p: Preflight; me?: string | null; blockMs?: number }) {
  const fx = p.effects;
  // Refused, nothing changes: show what it asks for, not a balance it won't leave.
  const refused = p.refusals.length > 0;
  return (
    <div className="preflight">
      <span className="eyebrow">{refused ? "What it would change" : "What will change"}</span>
      <div className="changes">
        {fx.changes.map((c) => (
          <div key={c.who.address} style={{ display: "contents" }}>
            <span>
              <Who who={c.who} me={me} link={false} />
            </span>
            <span className="why">{c.why}</span>
            <Amount drops={c.delta} signed exact />
          </div>
        ))}
        {fx.burned > 0n && (
          <div style={{ display: "contents" }}>
            <span className="burn">
              <Term k="burned">Burned forever</Term>
            </span>
            <span className="why">the fee</span>
            <Amount drops={fx.burned} exact />
          </div>
        )}
      </div>
      {fx.unseen && <p className="card-note">{fx.unseen}</p>}

      {!refused && (
        <dl className="kv after">
          <dt>Balance after</dt>
          <dd>
            <Amount drops={p.balanceAfter} exact />
            {p.pending > 0n && <span className="dim small block">once your pending transactions ({formatDrops(p.pending)}) go through too</span>}
          </dd>
          {p.budget && (
            <>
              <dt>
                <Term k="session_key">Agent budget</Term>
              </dt>
              <dd>
                <Meter used={p.budget.spentAfter > p.budget.limit ? p.budget.limit : p.budget.spentAfter} limit={p.budget.limit} label={`${p.budget.label} will have used ${formatDrops(p.budget.spentAfter)} of ${formatDrops(p.budget.limit)}`} />
                <span className="small">
                  “{p.budget.label}” will have used <Amount drops={p.budget.spentAfter} /> of <Amount drops={p.budget.limit} /> per {blocksToHuman(p.budget.periodBlocks, blockMs)}
                </span>
              </dd>
            </>
          )}
        </dl>
      )}

      {p.notes.map((n) => (
        <p key={n} className="warn small" style={{ margin: 0 }}>
          {n}
        </p>
      ))}
      {p.refusals.length > 0 && (
        <div className="refusal" role="alert">
          <b>The chain will refuse this.</b> Nothing would be charged, but nothing would happen either.
          <ul>
            {p.refusals.map((r) => (
              <li key={r}>{r}</li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
