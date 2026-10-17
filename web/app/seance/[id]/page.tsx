"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import { Loading, NotFound, plural, Section, StatusPill, Term, useMe, Who } from "@/components/explorer/kit";
import { claimPill } from "@/components/explorer/ProgramState";
import { useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { SeanceView } from "@/lib/explorer/types";

export default function SeancePage() {
  const { id } = useParams<{ id: string }>();
  const me = useMe();
  const seance = useNode<SeanceView>(`/seance/${encodeURIComponent(id)}`, { every: 5000 });
  if (seance.error && !seance.data) return <NotFound what="Séance" error={seance.error} />;
  const s = seance.data;
  if (!s) return <Loading>Summoning the Séance…</Loading>;
  const phase = s.phase === "sealing" ? `Sealing votes until block #${s.commit_until}` : s.phase === "revealing" ? `Revealing until block #${s.reveal_until}` : `Closed at block #${s.reveal_until}`;
  const r = s.results;
  return (
    <main className="wide stack-lg">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.programs()}>Programs</Link>
        <span>/</span>
        <Link href={to.program("pool")}>Contribution pool</Link>
        <span>/</span>
        <span>Séance #{s.id}</span>
      </nav>
      <section className="card plate">
        <span className="eyebrow">
          <Term k="seance">Séance #{s.id}</Term>
        </span>
        <p className="headline">{phase}</p>
        <span className="dim">
          opened at block #{s.opened_at} · budget {s.budget_display} · jury drawn from seed <span className="mono">{s.seed.slice(0, 16)}…</span>
        </span>
        {r && (
          <p style={{ marginTop: 12 }}>
            {r.accepted} accepted · {r.below_threshold} below the threshold · {r.spam} spam · {r.carried_over} carried over · {plural(r.no_shows, "no-show")}
            {s.results_display ? ` · paid ${s.results_display.paid}, ${s.results_display.returned} back to the pool` : ""}
          </p>
        )}
      </section>
      <div className="cols">
        <Section title={`Claims (${s.claims.length})`}>
          <ul className="ledger">
            {s.claims.map((c) => (
              <li key={c.id}>
                <Link className="ledger-row" href={to.claim(c.id)} style={{ gridTemplateColumns: "minmax(0,1fr) auto" }}>
                  <span className="ledger-main">
                    <span className="ledger-summary">
                      {c.repo}#{c.pr}
                    </span>
                    <span className="ledger-meta">
                      by {c.claimant.name ?? c.claimant.short}
                      {c.score_display ? ` · score ${c.score_display}` : ""}
                    </span>
                  </span>
                  {claimPill(c)}
                </Link>
              </li>
            ))}
          </ul>
        </Section>
        <Section title={`Jury (${s.jurors.length})`}>
          <ul className="ledger">
            {s.jurors.map((j) => (
              <li key={j.juror.address} className="row-between" style={{ padding: "10px 0" }}>
                <span>
                  <Who who={j.juror} me={me?.address} />
                  <span className="dim small block">
                    {j.pairs} pairs{j.agreement_display ? ` · agreed ${j.agreement_display}` : ""}
                    {j.reward_display !== "0 FANG" ? ` · earned ${j.reward_display}` : ""}
                  </span>
                </span>
                <StatusPill status={j.revealed ? "finalized" : "pending"}>{j.revealed ? "revealed" : j.committed ? "sealed" : s.phase === "closed" ? "no-show" : "waiting"}</StatusPill>
              </li>
            ))}
          </ul>
        </Section>
      </div>
    </main>
  );
}
