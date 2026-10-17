"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import { ActionForm } from "@/components/explorer/ActionForm";
import { Bars } from "@/components/explorer/charts";
import { Amount, Ext, Loading, NotFound, plural, Section, StatusPill, Term, useMe, Who } from "@/components/explorer/kit";
import { blocksToHuman } from "@/lib/format";
import { parseDrops } from "@/lib/explorer/ledger";
import { useNode } from "@/lib/explorer/node";
import { program } from "@/lib/explorer/programs";
import { to } from "@/lib/explorer/routes";
import type { ChainStatus, HuntView } from "@/lib/explorer/types";

const action = (kind: string) => program("hunts")?.actions.find((a) => a.kind === kind);

export default function HuntPage() {
  const { id } = useParams<{ id: string }>();
  const me = useMe();
  const hunt = useNode<HuntView>(`/hunt/${encodeURIComponent(id)}`, { every: 5000 });
  const status = useNode<ChainStatus>("/status", { every: 10_000 });
  if (hunt.error && !hunt.data) return <NotFound what="hunt" error={hunt.error} />;
  const h = hunt.data;
  if (!h) return <Loading>Finding the hunt…</Loading>;
  const head = status.data?.height ?? h.created_at;
  const left = h.expires_at - head;
  const creator = me?.address === h.creator.address;
  const fund = action("fund_hunt"), award = action("award_hunt"), cancel = action("cancel_hunt");
  return (
    <main className="wide stack-lg">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.programs()}>Programs</Link>
        <span>/</span>
        <Link href={to.program("hunts")}>Hunts</Link>
        <span>/</span>
        <span>#{h.id}</span>
      </nav>
      <section className="card plate receipt">
        <span className="eyebrow">
          <Term k="hunt">Hunt #{h.id}</Term>
        </span>
        <p className="headline">“{h.title}”</p>
        <div className="amount-hero">
          <Amount drops={h.amount} unit={false} />
          <span className="unit">FANG</span>
        </div>
        <div className="row-gap">
          <StatusPill status={h.status} />
          <Ext href={h.issue_url}>
            {h.repo}#{h.issue} ↗
          </Ext>
          <span className="dim small">
            {plural(h.funders.length, "funder")} · put up by <Who who={h.creator} me={me?.address} /> at <Link href={to.block(h.created_at)}>#{h.created_at}</Link>
            {h.status === "open" && (left > 0 ? ` · expires in ${blocksToHuman(left, status.data?.block_time_ms)}` : " · past its expiry")}
          </span>
        </div>
        {h.awarded_to && (
          <>
            <hr className="tear" />
            <p className="headline" style={{ fontSize: 22 }}>
              Won by <Who who={h.awarded_to} me={me?.address} />
              {h.familiar ? ` (via their Familiar ${h.familiar})` : ""} for{" "}
              <Ext href={h.pr_url}>PR #{h.pr} ↗</Ext>
              {h.self_awarded && <span className="pill pending" style={{ marginLeft: 8 }}>self-awarded</span>}
            </p>
            {h.awarded_by && h.awarded_at != null && (
              <span className="dim small">
                awarded by <Who who={h.awarded_by} me={me?.address} /> at <Link href={to.block(h.awarded_at)}>block #{h.awarded_at}</Link>
              </span>
            )}
          </>
        )}
        {h.cancelled_at != null && <p className="dim">Cancelled at block #{h.cancelled_at}; every funder got back what they put in.</p>}
      </section>

      <div className="cols">
        <Section title={`Funders (${h.funders.length})`} aside={<span className="dim small">who put up the bounty</span>}>
          <Bars
            rows={[...h.funders]
              .sort((a, b) => (parseDrops(b.amount) > parseDrops(a.amount) ? 1 : -1))
              .map((f) => ({ key: f.account.address, label: <Who who={f.account} me={me?.address} />, value: Number(parseDrops(f.amount) / 1_000_000n) / 1000 }))}
            format={(v) => `${v.toLocaleString("en-US")} FANG`}
          />
          <p className="card-note">
            The bounty sits in <Term k="escrow">escrow</Term>. If the hunt is cancelled or expires, each funder gets back exactly what they put in.
          </p>
        </Section>
        {h.status === "open" && (
          <div className="stack-lg">
            {fund && (
              <Section title="Add to the bounty">
                <ActionForm spec={fund} initial={{ hunt_id: String(h.id) }} fixed={["hunt_id"]} compact />
              </Section>
            )}
            {creator && award && cancel && (
              <Section title="You put this up">
                <details className="action">
                  <summary>
                    <span className="title">Award it</span>
                    <span className="sig">award_hunt</span>
                  </summary>
                  <ActionForm spec={award} initial={{ hunt_id: String(h.id) }} fixed={["hunt_id"]} compact />
                </details>
                <details className="action">
                  <summary>
                    <span className="title">Cancel and refund</span>
                    <span className="sig">cancel_hunt</span>
                  </summary>
                  <ActionForm spec={cancel} initial={{ hunt_id: String(h.id) }} fixed={["hunt_id"]} compact />
                </details>
              </Section>
            )}
          </div>
        )}
      </div>
    </main>
  );
}
