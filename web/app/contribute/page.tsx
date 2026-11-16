"use client";

import Link from "next/link";
import { ContributionNote } from "@/components/creators";
import { JuryCard } from "@/components/explorer/JuryCard";
import { Term } from "@/components/explorer/kit";
import { ProgramState } from "@/components/explorer/ProgramState";
import { NodeError, useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { ContributionsView } from "@/lib/explorer/types";

/**
 * The contribution pool from the wallet's side (the node explorer's `#/contribute`, plus what a
 * wallet can do there): the juror enrollment card first, then the pool, its open Séance, claims,
 * Souls and rules. Every other pool action stays on the program's page.
 */
export default function ContributePage() {
  const pool = useNode<ContributionsView>("/contributions", { every: 10_000 });
  const none = !pool.data && pool.error instanceof NodeError && pool.error.status === 404;
  return (
    <main className="wide stack-lg">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.programs()}>Programs</Link>
        <span>/</span>
        <Link href={to.program("pool")}>Contribution pool</Link>
        <span>/</span>
        <span>Contribute</span>
      </nav>
      <div className="page-head">
        <span className="eyebrow">
          <Term k="pool">contribution pool</Term>
        </span>
        <h1>Contribute</h1>
        <span className="sub">Useful work gets paid here, and people with Soul judge it. Sit on the jury, or claim a merged pull request.</span>
      </div>
      {none ? (
        <section className="card" data-testid="no-pool">
          <p className="m0">This chain has no contribution pool, so there&apos;s no jury to join.</p>
        </section>
      ) : !pool.data ? (
        pool.error ? (
          <section className="card">
            <p className="error m0" role="alert">Couldn&apos;t read the pool: {pool.error.message}</p>
          </section>
        ) : (
          <p className="dim">Reading the pool…</p>
        )
      ) : (
        <>
          {!pool.data.active && <p className="card-note m0">The pool pays nothing yet: it&apos;s switched on once governance adds a repository to it.</p>}
          <JuryCard pool={pool.data} />
          <ContributionNote />
          <ProgramState id="pool" />
          <p className="card-note">
            Claiming a merged PR, linking GitHub, vouching, funding the pool and withdrawing vested rewards are on <Link href={to.program("pool")}>the pool&apos;s program page</Link>. Jurors seal and reveal their votes on each Séance&apos;s page.
          </p>
        </>
      )}
    </main>
  );
}
