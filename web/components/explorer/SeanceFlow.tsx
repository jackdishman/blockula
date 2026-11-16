"use client";

import { useNode } from "@/lib/explorer/node";
import { flowMarks, seanceFlow, soulFacts } from "@/lib/explorer/seance-flow";
import type { ChainStatus, ContributionsView } from "@/lib/explorer/types";
import { Term } from "./kit";

/**
 * The Séance and Soul flow as a diagram: a numbered timeline from a merged pull request to a
 * vested reward (down the page on a phone, across it on a wide screen), marked with the open
 * Séance's phase and the next opening, and Soul in three facts under it. Every number is the
 * chain's (`lib/explorer/seance-flow.ts` reads `/api/contributions`' rules); it signs nothing.
 */
export function SeanceFlow({ pool }: { pool: ContributionsView }) {
  const status = useNode<ChainStatus>("/status", { every: 5000 });
  const blockMs = status.data?.block_time_ms || 1000;
  const steps = seanceFlow(pool.rules, blockMs);
  const marks = status.data?.height == null ? [] : flowMarks(pool, status.data.height, blockMs);
  return (
    <section className="card sflow-card" data-testid="seance-flow">
      <figure className="sflow-fig" aria-labelledby="seance-flow-caption">
        <figcaption id="seance-flow-caption" className="eyebrow">
          from a merged PR to a reward
        </figcaption>
        <ol className="sflow">
          {steps.map((s) => {
            const here = marks.filter((m) => m.step === s.key);
            return (
              <li key={s.key} className={here.some((m) => m.kind === "now") ? "now" : here.length ? "next" : undefined} data-step={s.key}>
                <span className="sflow-label">{s.label}</span>
                {s.value && (
                  <span className="sflow-value" data-testid={`flow-${s.key}`}>
                    {s.value}
                  </span>
                )}
                <span className="sflow-note">{s.note}</span>
                {here.map((m) => (
                  <span key={m.kind} className="sflow-mark" data-testid={`flow-${m.kind}`}>
                    {m.text}
                  </span>
                ))}
              </li>
            );
          })}
        </ol>
      </figure>
      <aside className="soul-panel" aria-labelledby="soul-panel-title" data-testid="soul-panel">
        <span id="soul-panel-title" className="eyebrow">
          <Term k="soul">Soul</Term>
        </span>
        <dl className="soul-facts">
          {soulFacts(pool.rules, blockMs).map((f) => (
            <div key={f.key} data-fact={f.key}>
              <dt>{f.label}</dt>
              <dd>{f.text}</dd>
            </div>
          ))}
        </dl>
      </aside>
    </section>
  );
}
