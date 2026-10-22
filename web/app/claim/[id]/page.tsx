"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import { ClaimTimeline, ContributionNote } from "@/components/creators";
import { Ext, github, Loading, NotFound, Section, Term, useMe, Who } from "@/components/explorer/kit";
import { claimPill } from "@/components/explorer/ProgramState";
import { BOND_TERMS, type PocRules } from "@/lib/creators";
import { useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { ChainStatus, ClaimView, ContributionsView } from "@/lib/explorer/types";

/** What became of a settled bond, as far as the claim's status tells (a refuted claim's reason says which way it went). */
const settled = (status: string) => (status === "scored" ? "returned" : status === "spam" ? "went to the pool: the jury marked it spam" : status === "rejected" ? "settled (the reason above says how)" : "settled");

export default function ClaimPage() {
  const { id } = useParams<{ id: string }>();
  const me = useMe();
  const claim = useNode<ClaimView>(`/claim/${encodeURIComponent(id)}`, { every: 10_000 });
  // The pool's timings and the head, for the claim's timeline (CONTRIB-16).
  const pool = useNode<ContributionsView>("/contributions", { every: 60_000 });
  const status = useNode<ChainStatus>("/status", { every: 10_000 });
  if (claim.error && !claim.data) return <NotFound what="claim" error={claim.error} />;
  const c = claim.data;
  if (!c) return <Loading>Finding the claim…</Loading>;
  return (
    <main className="wide stack-lg">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.programs()}>Programs</Link>
        <span>/</span>
        <Link href={to.program("pool")}>Contribution pool</Link>
        <span>/</span>
        <span>Claim #{c.id}</span>
      </nav>
      <section className="card plate">
        <span className="eyebrow">
          <Term k="claim">Claim #{c.id}</Term>
        </span>
        <p className="headline">
          <Ext href={github(c.repo, "pull", c.pr)}>
            {c.repo}#{c.pr} ↗
          </Ext>{" "}
          by <Who who={c.claimant} me={me?.address} /> <span className="dim">(@{c.handle})</span>
        </p>
        <div className="row-gap">
          {claimPill(c)}
          {c.reason && <span className="dim">{c.reason}</span>}
        </div>
      </section>
      <ContributionNote />
      <div className="cols">
        <Section title={`Validator checks (${c.attestations?.length ?? 0})`}>
          {c.attestations?.length ? (
            <ul className="ledger">
              {c.attestations.map((a) => (
                <li key={`${a.validator.address}-${a.at}`} className="row-between pad-row">
                  <span>
                    <Who who={a.validator} me={me?.address} />{" "}
                    {a.ok ? (
                      <>
                        confirmed the merge as <span className="mono">{a.merge_sha.slice(0, 12)}</span>
                      </>
                    ) : (
                      <span className="error">couldn&apos;t confirm it: {a.note || "GitHub doesn't confirm it"}</span>
                    )}
                  </span>
                  <Link className="dim small" href={to.block(a.at)}>
                    #{a.at}
                  </Link>
                </li>
              ))}
            </ul>
          ) : (
            <p className="ledger-empty">Waiting for validators to check GitHub…</p>
          )}
        </Section>
        <Section title="Record">
          <dl className="details tight-150">
            <dt>Submitted</dt>
            <dd>
              <Link href={to.block(c.submitted_at)}>block #{c.submitted_at}</Link>
              {c.familiar ? ` by their Familiar ${c.familiar}` : ""}
            </dd>
            <dt>
              <Term k="bond" />
            </dt>
            <dd data-testid="claim-bond">{c.bond !== "0" ? `${c.bond_display}, held: ${BOND_TERMS}` : settled(c.status)}</dd>
            {c.verified_at != null && (
              <>
                <dt>Verified</dt>
                <dd>
                  block #{c.verified_at} · merge <span className="mono">{(c.merge_sha ?? "").slice(0, 12)}</span>
                </dd>
              </>
            )}
            {c.seance != null && (
              <>
                <dt>
                  <Term k="seance">Séance</Term>
                </dt>
                <dd>
                  <Link href={to.seance(c.seance)}>#{c.seance}</Link>
                </dd>
              </>
            )}
            {c.score_display && (
              <>
                <dt>Score</dt>
                <dd>
                  {c.score_display} of comparisons won · earned {c.payout_display} (<Term k="vesting">vesting</Term>)
                </dd>
              </>
            )}
          </dl>
        </Section>
      </div>
      <ClaimTimeline claim={c} rules={pool.data?.rules as PocRules | undefined} head={status.data?.height} blockMs={status.data?.block_time_ms} />
    </main>
  );
}
