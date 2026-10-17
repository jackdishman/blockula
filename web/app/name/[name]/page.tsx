"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import { ActionForm } from "@/components/explorer/ActionForm";
import { Loading, NotFound, Section, Term, useMe, Who } from "@/components/explorer/kit";
import { useNode } from "@/lib/explorer/node";
import { program } from "@/lib/explorer/programs";
import { to } from "@/lib/explorer/routes";
import type { NameDetail } from "@/lib/explorer/types";

export default function NamePage() {
  const { name } = useParams<{ name: string }>();
  const n = decodeURIComponent(name).toLowerCase().replace(/\.boo$/, "");
  const me = useMe();
  const look = useNode<NameDetail>(`/name/${encodeURIComponent(n)}`, { every: 5000 });
  if (look.error && !look.data) return <NotFound what="name" error={look.error} />;
  const r = look.data;
  if (!r) return <Loading>Looking up {n}.boo…</Loading>;
  const register = program("names")?.actions.find((a) => a.kind === "register_name");
  return (
    <main className="wide stack-lg">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.programs()}>Programs</Link>
        <span>/</span>
        <Link href={to.program("names")}>Names</Link>
        <span>/</span>
        <span>{r.name}</span>
      </nav>
      <div className="page-head">
        <span className="eyebrow">
          <Term k="name">{r.available ? "Unclaimed name" : "Name"}</Term>
        </span>
        <h1 className="figure-title">{r.name}</h1>
        {!r.available && r.owner && (
          <span className="sub">
            Points to <Who who={r.owner} me={me?.address} />
            {r.registered_at != null && (
              <>
                {" "}
                since <Link href={to.block(r.registered_at)}>block #{r.registered_at}</Link>
              </>
            )}
            {r.expires_at != null && <> · expires at block #{r.expires_at}</>}
          </span>
        )}
      </div>
      {r.available ? (
        <div className="cols">
          <Section title={`Yours for ${r.price_display ?? "…"}`}>
            {register && <ActionForm spec={register} initial={{ name: n }} compact />}
          </Section>
          <Section title="About names">
            <p>A .boo name points to an account, like a domain name points to a server, so people can pay {r.name} instead of a boo1… address.</p>
            <p className="dim">Shorter names cost more because they&apos;re scarcer. The price goes to the treasury.</p>
          </Section>
        </div>
      ) : (
        <div className="row-gap">
          {r.owner && (
            <Link className="btn primary" href={to.account(r.name)}>
              Open {r.name}
            </Link>
          )}
          {me && r.owner && me.address !== r.owner.address && (
            <Link className="btn" href={`/send?to=${encodeURIComponent(r.name)}`}>
              Send to {r.name}
            </Link>
          )}
        </div>
      )}
    </main>
  );
}
