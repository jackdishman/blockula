"use client";

import Link from "next/link";
import { useParams, useSearchParams } from "next/navigation";
import { Suspense } from "react";
import { ActionForm } from "@/components/explorer/ActionForm";
import { Section, Term } from "@/components/explorer/kit";
import { ProgramState } from "@/components/explorer/ProgramState";
import { program, signature } from "@/lib/explorer/programs";
import { to } from "@/lib/explorer/routes";

export default function ProgramPage() {
  return (
    <Suspense>
      <Program />
    </Suspense>
  );
}

function Program() {
  const { id } = useParams<{ id: string }>();
  const params = useSearchParams();
  const p = program(id);
  if (!p)
    return (
      <main className="wide">
        <section className="card">
          <h1>No such program</h1>
          <Link href={to.programs()}>All programs</Link>
        </section>
      </main>
    );
  // `?do=fund_hunt&hunt_id=3` opens an action with fields filled in.
  const open = params.get("do");
  const initial = Object.fromEntries([...params.entries()].filter(([k]) => k !== "do"));
  return (
    <main className="wide stack-lg">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.programs()}>Programs</Link>
        <span>/</span>
        <span>{p.title}</span>
      </nav>
      <div className="page-head">
        <span className="eyebrow">
          <Term k={p.term}>program · {p.id}</Term>
        </span>
        <h1>{p.title}</h1>
        <span className="sub">{p.tagline}</span>
      </div>
      {open && Object.keys(initial).length > 0 && (
        <div className="banner">A link filled in this action. Check every field, and the preview, before you sign.</div>
      )}
      <div className="cols">
        <div className="stack-lg">
          <ProgramState id={p.id} />
          {p.actions.length > 0 && (
            <Section title="Actions" aside={<span className="dim small">preview, then sign</span>}>
              {p.actions.map((a) => (
                <details key={a.kind} className="action" open={open === a.kind || (p.actions.length === 1 && !open)}>
                  <summary>
                    <span className="title">{a.title}</span>
                    <span className="sig">{a.kind}</span>
                  </summary>
                  <ActionForm spec={a} initial={open === a.kind ? initial : {}} />
                </details>
              ))}
              {p.elsewhere && <p className="card-note">{p.elsewhere}</p>}
            </Section>
          )}
        </div>
        <Section title="Interface" aside={<span className="dim small">like a contract&apos;s</span>}>
          <span className="tag">reads</span>
          <ul className="iface" style={{ marginBottom: 14 }}>
            {p.reads.map((r) => (
              <li key={r.path}>
                <span className="fn">GET {r.path}</span>
                <span className="who-can block">{r.what}</span>
              </li>
            ))}
          </ul>
          <span className="tag">actions</span>
          <ul className="iface" style={{ marginBottom: 14 }}>
            {p.actions.map((a) => (
              <li key={a.kind}>
                <span className="fn">{a.kind}</span>
                {signature(a)
                  .slice(a.kind.length)
                  .split(/(: \w+)/)
                  .map((part, i) => (i % 2 ? <span key={i} className="ty">{part}</span> : part))}
                <span className="who-can block">{a.who}</span>
              </li>
            ))}
          </ul>
          <span className="tag">all calls</span>
          <ul className="iface">
            {p.calls.map((c) => (
              <li key={c}>{c}</li>
            ))}
          </ul>
          <p className="card-note">Every call is a signed transaction with a receipt. Calls without a form here need special keys or a secret between steps.</p>
        </Section>
      </div>
    </main>
  );
}
