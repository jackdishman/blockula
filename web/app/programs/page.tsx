"use client";

import Link from "next/link";
import type { ReactNode } from "react";
import { Fang, plural, Term } from "@/components/explorer/kit";
import { compactDrops, parseDrops } from "@/lib/explorer/ledger";
import { useNode } from "@/lib/explorer/node";
import { PROGRAMS, signature } from "@/lib/explorer/programs";
import { to } from "@/lib/explorer/routes";
import type { ChainStatus, ContributionsView, HuntsView, ValidatorsDetail } from "@/lib/explorer/types";

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
          <span className="tag">coming</span>
          <h2>Your program here</h2>
          <span className="dim">
            When anyone can deploy a program (planned for protocol v2), each one gets a page like these: its state, its calls, and a form generated from its interface, with the same preview, signature and receipt.
          </span>
        </div>
      </div>
    </main>
  );
}
