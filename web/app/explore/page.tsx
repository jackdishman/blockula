"use client";

import Link from "next/link";
import { useEffect, useRef } from "react";
import { Bars, Cadence, Ribbon } from "@/components/explorer/charts";
import { Fang, Section, Stat, Term, useMe, useNow, Who, whoText, plural } from "@/components/explorer/kit";
import { Ledger } from "@/components/explorer/Ledger";
import { WatchingList } from "@/components/explorer/Watch";
import { type NodeError, useNode } from "@/lib/explorer/node";
import { usePulse, useRecentTxs } from "@/lib/explorer/pulse";
import { to } from "@/lib/explorer/routes";
import type { AccountDetail, ChainStatus, HuntsView, MempoolView, ValidatorsDetail } from "@/lib/explorer/types";

const WINDOW = 120;

export default function ExplorePage() {
  const now = useNow(1000);
  const me = useMe();
  const status = useNode<ChainStatus>("/status", { every: 2000 });
  const health = useNode<{ ok: boolean; reason?: string }>("/health", { every: 5000 });
  const validators = useNode<ValidatorsDetail>("/validators", { every: 15_000 });
  const mempool = useNode<MempoolView>("/mempool?limit=25", { every: 2000 });
  const hunts = useNode<HuntsView>("/hunts?status=open", { every: 30_000 });
  const { blocks, born, error } = usePulse(WINDOW);
  const recent = useRecentTxs(blocks);
  // A quiet window: look back once on our own, so the list isn't empty on a sleepy chain.
  const lookedBack = useRef(false);
  const quiet = blocks.length > 0 && !blocks.some((b) => b.tx_count > 0);
  useEffect(() => {
    if (!quiet || lookedBack.current) return;
    lookedBack.current = true;
    recent.further(1000);
  }, [quiet, recent]);
  const s = status.data;
  // /api/health answers 503 with a reason when the head is stale or the node is syncing.
  const healthErr = health.error as NodeError | null;
  const healthy = healthErr ? false : health.data?.ok;
  const unhealthyBecause = String(healthErr?.body?.reason ?? health.data?.reason ?? healthErr?.message ?? "unknown");
  const head = blocks[blocks.length - 1];
  const blockMs = s?.block_time_ms ?? 1000;

  // Your transactions light up their blocks in the pulse (read from the chain, so it works locked too).
  const mineAcct = useNode<AccountDetail>(me ? `/account/${me.address}` : null, { every: 5000 });
  const mine = new Set((mineAcct.data?.recent_txs ?? []).map((t) => t.height).filter((h): h is number => h != null));
  const avg = blocks.length > 1 ? (blocks[blocks.length - 1].timestamp_ms - blocks[0].timestamp_ms) / (blocks.length - 1) / 1000 : null;
  const age = head ? Math.max(0, now - head.timestamp_ms) : null;

  // Who proposed the blocks in the window: round-robin health.
  const proposers = new Map<string, { who: (typeof blocks)[number]["proposer"]; n: number }>();
  for (const b of blocks) {
    const p = proposers.get(b.proposer.address) ?? { who: b.proposer, n: 0 };
    p.n++;
    proposers.set(b.proposer.address, p);
  }

  if (!s && (status.error || error)) {
    return (
      <main className="wide stack-lg">
        <section className="card">
          <span className="eyebrow">The node is silent</span>
          <h1>Can&apos;t reach the node</h1>
          <p className="dim">{(status.error ?? error)?.message}. Pick another network at the top, or start a local node with <code>blockula dev</code>.</p>
        </section>
      </main>
    );
  }

  return (
    <main className="wide stack-lg">
      <div className="page-head">
        <span className="eyebrow">The pulse</span>
        <h1>{s ? s.chain_id : "…"}, live</h1>
        <span className="sub">
          A new block about every {(blockMs / 1000).toLocaleString("en-US")}s, final the moment it&apos;s sealed. Click any block, name or transaction to dig in, or press <span className="kbd">⌘K</span> to search.
        </span>
      </div>

      <section>
        <div className="figures">
          <Stat
            v={
              <>
                <span className="live-dot" aria-hidden />#{(head?.height ?? s?.height ?? 0).toLocaleString("en-US")}
              </>
            }
            k={
              <>
                <Term k="height">latest block</Term>
                {s?.syncing ? ` · syncing ${Math.floor((s.sync_progress ?? 0) * 100)}%` : ""}
              </>
            }
          />
          <Stat v={age == null ? "—" : `${(age / 1000).toFixed(1)}s`} k="since the last block" />
          <Stat v={avg == null ? "—" : `${avg.toFixed(2)}s`} k={`average over ${blocks.length} blocks`} />
          <Stat v={s?.active_validators ?? "—"} k={<><Link href={to.program("validators")}>validators</Link> in <Term k="epoch" /> {s?.epoch ?? "…"}</>} />
        </div>
        <div className="figures">
          <Stat v={<Fang display={s?.supply_display} short />} k="total supply" />
          <Stat v={<Fang display={s?.burned_display} />} k={<Term k="burned" />} />
          <Stat v={s?.accounts?.toLocaleString("en-US") ?? "—"} k="accounts" />
          <Stat v={s?.names?.toLocaleString("en-US") ?? "—"} k={<Term k="name">.boo names</Term>} />
          <Stat v={mempool.data?.count ?? s?.mempool ?? 0} k={<Term k="mempool">waiting</Term>} />
        </div>
        <div className="row-between small dim" style={{ padding: "8px 0" }}>
          <span className={healthy ? "ok" : healthy === false ? "error" : "dim"} style={{ margin: 0 }}>
            ● {healthy ? "The network is healthy" : healthy === false ? `Unhealthy: ${unhealthyBecause}` : "Checking health…"}
          </span>
          {validators.data?.blocks_until_next_epoch != null && <span>{plural(validators.data.blocks_until_next_epoch, "block")} until epoch {validators.data.epoch + 1}</span>}
        </div>
      </section>

      <Section
        title="The latest blocks"
        aside={
          <span className="legend">
            <span><span className="sw" style={{ background: "var(--ink-soft)" }} />has transactions</span>
            <span><span className="sw" style={{ background: "rgba(23,18,15,.16)" }} />empty</span>
            {me && <span><span className="sw" style={{ background: "var(--blood)" }} />yours</span>}
          </span>
        }
      >
        {blocks.length ? (
          <>
            <Ribbon
              blocks={blocks}
              mine={mine}
              born={born}
              now={now}
              caption={(shown) => {
                const n = shown.reduce((sum, b) => sum + b.tx_count, 0);
                return n ? `${plural(n, "transaction")} in ${plural(shown.length, "block")}` : `a quiet stretch: ${plural(shown.length, "empty block")}`;
              }}
            />
          </>
        ) : (
          <p className="dim">Waiting for blocks…</p>
        )}
      </Section>

      <div className="cols">
        <div className="stack-lg">
          {!!mempool.data?.txs.length && (
            <Section title={<Term k="mempool">Waiting for a block</Term>} aside={<span className="dim small">{mempool.data.count} pending</span>}>
              <Ledger txs={mempool.data.txs} side={me?.address} me={me} />
            </Section>
          )}
          <Section title="Latest transactions">
            <Ledger txs={recent.txs} side={me?.address} me={me} head={head?.height} blockMs={blockMs} empty={`No transactions in the last ${plural(blocks.length + recent.searchedBack, "block")}.`} />
            {recent.txs.length < 12 && !recent.exhausted && blocks.length > 0 && (
              <button className="link small" disabled={recent.scanning} onClick={() => recent.further()} style={{ marginTop: 8 }}>
                {recent.scanning ? "Searching further back…" : recent.searchedBack ? "Search further back" : "Search earlier blocks"}
              </button>
            )}
          </Section>
        </div>
        <div className="stack-lg">
          <WatchingList />
          <Section title="Block cadence" aside={<span className="dim small">seconds between blocks</span>}>
            <Cadence blocks={blocks} targetMs={blockMs} />
            <p className="card-note">The target is {(blockMs / 1000).toLocaleString("en-US")}s. Spikes mean a round needed a new leader or a validator was slow to vote.</p>
          </Section>
          <Section title={<Term k="proposer">Who proposed</Term>} aside={<span className="dim small">last {blocks.length}</span>}>
            <Bars rows={[...proposers.values()].sort((a, b) => b.n - a.n).map((p) => ({ key: p.who.address, label: <Who who={p.who} me={me?.address} />, value: p.n, note: whoText(p.who) }))} />
            <p className="card-note">Validators take turns. An even split means every one of them is online and leading its rounds.</p>
          </Section>
          {!!hunts.data?.hunts.length && (
            <Section title={<Term k="hunt">Open hunts</Term>} aside={<Link className="small" href={to.program("hunts")}>All →</Link>}>
              <ul className="ledger">
                {hunts.data.hunts.slice(0, 4).map((h) => (
                  <li key={h.id}>
                    <Link href={to.hunt(h.id)} className="ledger-row" style={{ gridTemplateColumns: "minmax(0,1fr) auto" }}>
                      <span className="ledger-main">
                        <span className="ledger-summary">{h.title}</span>
                        <span className="ledger-meta">{h.repo}#{h.issue} · {plural(h.funders.length, "funder")}</span>
                      </span>
                      <span className="amt">
                        <Fang display={h.amount_display} />
                      </span>
                    </Link>
                  </li>
                ))}
              </ul>
            </Section>
          )}
        </div>
      </div>
    </main>
  );
}
