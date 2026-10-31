"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useState, type ReactNode } from "react";
import { Fang, Named, plural, Term } from "@/components/explorer/kit";
import { inertText, shortAddress, typeText } from "@/lib/explorer/inert";
import { compactDrops, parseDrops } from "@/lib/explorer/ledger";
import { useNode } from "@/lib/explorer/node";
import { deployedProgram, PROGRAMS, signature } from "@/lib/explorer/programs";
import { to } from "@/lib/explorer/routes";
import type { ChainStatus, ContributionsView, DeployedProgramView, HuntsView, ValidatorsDetail } from "@/lib/explorer/types";
import { useProgramActivity } from "@/lib/explorer/useProgramActivity";

export default function ProgramsPage() {
  const status = useNode<ChainStatus>("/status", { every: 10_000 });
  const hunts = useNode<HuntsView>("/hunts?status=open", { every: 30_000 });
  const pool = useNode<ContributionsView>("/contributions", { every: 30_000 });
  const vals = useNode<ValidatorsDetail>("/validators", { every: 30_000 });
  const escrow = (hunts.data?.hunts ?? []).reduce((s, h) => s + parseDrops(h.amount), 0n);
  const figure: Record<string, ReactNode> = {
    accounts: <>{status.data?.accounts?.toLocaleString("en-US") ?? "…"} accounts</>,
    names: <>{status.data?.names?.toLocaleString("en-US") ?? "…"} names</>,
    hunts: <>{compactDrops(escrow)} FANG in {plural(hunts.data?.hunts.length ?? 0, "open hunt")}</>,
    pool: <><Fang display={pool.data?.pool.balance_display} /> in the pool</>,
    validators: <>{vals.data ? `${vals.data.active.length} active, epoch ${vals.data.epoch}` : "…"}</>,
  };
  return (
    <main className="wide stack-lg">
      <div className="page-head">
        <span className="eyebrow">
          <Term k="program">Programs</Term>
        </span>
        <h1>What the chain can do</h1>
        <span className="sub">
          Blockula&apos;s built-in modules, each laid out the way third-party programs will be: the state it keeps, the calls it accepts, and actions you can sign right here with a preview of exactly what you&apos;re signing.
        </span>
      </div>
      <div className="programs">
        {PROGRAMS.map((p) => (
          <Link key={p.id} href={to.program(p.id)} className="card program-card">
            <span className="tag">program · {p.id}</span>
            <h2>{p.title}</h2>
            <span className="dim">{p.tagline}</span>
            <span className="figure">{figure[p.id]}</span>
            <ul className="iface">
              {p.actions.slice(0, 3).map((a) => (
                <li key={a.id ?? a.kind}>
                  <span className="fn">{a.kind}</span>
                  {signature(a).slice(a.kind.length)}
                </li>
              ))}
              {p.actions.length > 3 && <li className="who-can">and {p.actions.length - 3} more</li>}
            </ul>
          </Link>
        ))}
        <div className="card program-card coming">
          <span className="tag">deploy</span>
          <h2>Your program here</h2>
          <span className="dim">
            On a chain that has switched programs on, anyone can upload WebAssembly and create a program from it (`blockula program`, or the SDK). Each one gets a page like these: its record and what it holds, its methods as its deployer named them, and its transactions. In this release a deployed program is read here and called with the CLI or the SDK.
          </span>
        </div>
      </div>
      <Deployed />
    </main>
  );
}

/**
 * The addresses of the programs recent blocks created, newest first: from the receipts of their `create_program` transactions.
 * That's a window of recent blocks, not every program: no route lists them yet (docs/PROGRAMS-V0.md §8.2), and the page says so.
 */
const CREATES = ["create_program"] as const;

/** Programs anyone deployed, beside the built-in ones (docs/PROGRAMS-V0.md §8.6): read only, every name in them the deployer's. */
function Deployed() {
  const router = useRouter();
  const [q, setQ] = useState("");
  const creates = useProgramActivity(CREATES);
  const addresses = [...new Set(creates.txs.flatMap((t) => (typeof t.program?.facts?.created === "string" && t.status === "finalized" ? [t.program.facts.created] : [])))].slice(0, 12);
  const open = (e: React.FormEvent) => {
    e.preventDefault();
    const v = q.trim().toLowerCase();
    if (v) router.push(to.deployed(v));
  };
  return (
    <section className="stack-lg" aria-labelledby="deployed-heading">
      <div className="page-head">
        <span className="eyebrow">
          <Term k="program">deployed</Term>
        </span>
        <h2 id="deployed-heading">Programs anyone deployed</h2>
        <span className="sub">
          WebAssembly on this chain, newest first. This lists the programs created in a window of recent blocks (the node has no list of every program yet), so an older program isn&apos;t here: open it by its address below. What they hold and store is on the chain; every name they use is their deployer&apos;s claim.
        </span>
      </div>
      <form className="row-gap" onSubmit={open} role="search" aria-label="Open a program or code">
        <label>
          Open a program by its address, or a code by its hash
          <input type="text" className="mono" value={q} onChange={(e) => setQ(e.target.value)} placeholder="boo1… or 64 hex characters" spellCheck={false} autoComplete="off" />
        </label>
        <button className="btn small" type="submit" disabled={!q.trim()}>
          Open
        </button>
      </form>
      {addresses.length === 0 ? (
        <p className="card-note" data-testid="no-deployed">
          {creates.loading ? "Reading recent blocks…" : "No program was created in the recent blocks (or this chain has programs off)."}
        </p>
      ) : (
        <div className="programs" data-testid="deployed-programs">
          {addresses.map((a) => (
            <DeployedCard key={a} address={a} />
          ))}
        </div>
      )}
    </section>
  );
}

function DeployedCard({ address }: { address: string }) {
  const read = useNode<DeployedProgramView>(`/program/${address}`, { every: 30_000 });
  if (!read.data) {
    return (
      <div className="card program-card">
        <span className="tag">program · deployed</span>
        <h2 className="mono">{shortAddress(address)}</h2>
        <span className="dim">{read.error ? `Couldn't read it: ${inertText(read.error.message, 120)}` : "Reading it…"}</span>
      </div>
    );
  }
  const p = deployedProgram(read.data);
  const methods = Array.isArray(read.data.methods) ? read.data.methods : [];
  return (
    <Link href={to.deployed(address)} className="card program-card" data-testid="deployed-card">
      <span className="tag">program · deployed</span>
      <h2 className="mono">{shortAddress(address)}</h2>
      <span className="dim">{p.tagline}</span>
      <span className="figure">
        <Fang display={read.data.balance_display} /> held
      </span>
      <ul className="iface">
        {methods.slice(0, 3).map((m, i) => (
          <li key={i}>
            <Named text={m.untrusted_name} />({(Array.isArray(m.args) ? m.args : []).map((a) => `${inertText(a.untrusted_name, 24)}: ${typeText(a.type)}`).join(", ")}) <span className="who-can">{inertText(m.kind, 8)}</span>
          </li>
        ))}
        {methods.length > 3 && <li className="who-can">and {methods.length - 3} more</li>}
      </ul>
    </Link>
  );
}
