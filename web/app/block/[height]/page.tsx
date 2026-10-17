"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import { Ribbon } from "@/components/explorer/charts";
import { ago, Hash, Loading, NotFound, plural, Section, Term, useMe, useNow, Who } from "@/components/explorer/kit";
import { Ledger } from "@/components/explorer/Ledger";
import { nodeExplorer } from "@/lib/config";
import { useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { BlockSummary, BlockView, ChainStatus } from "@/lib/explorer/types";

const SPAN = 30;

export default function BlockPage() {
  const { height } = useParams<{ height: string }>();
  const id = decodeURIComponent(height);
  const now = useNow(1000);
  const me = useMe();
  const block = useNode<BlockView>(`/block/${id}`);
  const status = useNode<ChainStatus>("/status", { every: 3000 });
  const b = block.data;
  // Its neighborhood: up to SPAN blocks either side (more appear as the chain grows).
  const near = useNode<BlockSummary[]>(b ? `/blocks?limit=${SPAN * 2 + 1}&before=${b.height + SPAN + 1}` : null, { every: 3000 });

  if (block.error && !b) return <NotFound what="block" error={block.error} />;
  if (!b) return <Loading>Finding block {id}…</Loading>;

  const head = status.data?.height;
  const neighbors = [...(near.data ?? [])].sort((x, y) => x.height - y.height);
  const failed = b.failed_count ?? b.txs.filter((t) => t.status === "failed").length;

  return (
    <main className="wide stack-lg">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.explore()}>Explore</Link>
        <span>/</span>
        <span>Block</span>
      </nav>
      <div className="page-head">
        <span className="eyebrow">
          <Term k="block" />
        </span>
        <h1 className="figure-title">#{b.height.toLocaleString("en-US")}</h1>
        <span className="sub">
          {plural(b.tx_count, "transaction")}
          {failed ? ` (${failed} failed)` : ""} · {new Date(b.timestamp_ms).toLocaleString()} · {ago(now - b.timestamp_ms)}
          {head != null && head > b.height ? ` · ${plural(head - b.height, "block")} deep` : ""}
        </span>
        <div className="tools">
          {b.height > 1 && (
            <Link className="btn small ghost" href={to.block(b.height - 1)}>
              ← #{b.height - 1}
            </Link>
          )}
          {head == null || b.height < head ? (
            <Link className="btn small ghost" href={to.block(b.height + 1)}>
              #{b.height + 1} →
            </Link>
          ) : (
            <span className="dim small">the newest block</span>
          )}
        </div>
      </div>

      {neighbors.length > 1 && (
        <section>
          <Ribbon blocks={neighbors} here={b.height} now={now} caption={() => "its neighborhood: click any block"} />
        </section>
      )}

      <div className="cols">
        <Section title={`Transactions (${b.tx_count})`}>
          <Ledger txs={b.txs} side={me?.address} me={me} empty="A quiet block. Nothing stirred." />
        </Section>
        <Section title="Seal">
          <dl className="details tight-130">
            <dt>
              <Term k="proposer" />
            </dt>
            <dd>
              <Who who={b.proposer} me={me?.address} />
            </dd>
            <dt>Sealed</dt>
            <dd>{new Date(b.timestamp_ms).toLocaleString()}</dd>
            <dt>
              <Term k="certificate">Finality</Term>
            </dt>
            <dd>{b.signature_bytes ? `ML-DSA-65 quorum certificate, ${b.signature_bytes.toLocaleString("en-US")} bytes` : "finalized with a later block's certificate"}</dd>
            <dt>
              <Term k="hash">Hash</Term>
            </dt>
            <dd>
              <Hash value={b.hash} />
            </dd>
            <dt>Previous</dt>
            <dd>
              <Hash value={b.prev_hash} href={b.height > 1 ? to.block(b.height - 1) : undefined} />
            </dd>
            <dt>
              <Term k="state_root">State root</Term>
            </dt>
            <dd>
              <Hash value={b.state_root} />
            </dd>
          </dl>
          <p className="card-note">
            <a href={nodeExplorer.block(b.height)} target="_blank" rel="noreferrer">
              This block on the node&apos;s explorer ↗
            </a>
          </p>
        </Section>
      </div>
    </main>
  );
}
