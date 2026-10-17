"use client";

import Link from "next/link";
import { useParams, useRouter, useSearchParams } from "next/navigation";
import { Suspense, useEffect, useMemo, useState } from "react";
import { Amount, Loading, NotFound, plural, Section, Stat, useMe, Who, whoText } from "@/components/explorer/kit";
import { Ledger } from "@/components/explorer/Ledger";
import { edgeKey, TraceGraph } from "@/components/explorer/TraceGraph";
import { Copy } from "@/components/ui";
import { parseDrops } from "@/lib/explorer/ledger";
import { nodeGet, useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import { buildTrace, FOLLOW_MAX, followParam, parseFollow, type TraceNode } from "@/lib/explorer/trace";
import type { AccountDetail, TxDetail } from "@/lib/explorer/types";

export default function TracePage() {
  return (
    <Suspense fallback={<Loading>Following the money…</Loading>}>
      <TraceView />
    </Suspense>
  );
}

/** Followed accounts, fetched as they're added; `round` refetches them all. */
function useAccounts(list: string[], round: number) {
  const [got, setGot] = useState<{ map: ReadonlyMap<string, AccountDetail>; failed: ReadonlySet<string> }>({ map: new Map(), failed: new Set() });
  const key = list.join(",");
  useEffect(() => {
    let stop = false;
    for (const a of key ? key.split(",") : []) {
      nodeGet<AccountDetail>(`/account/${encodeURIComponent(a)}`).then(
        (d) => !stop && setGot((g) => ({ map: new Map(g.map).set(a, d), failed: new Set([...g.failed].filter((x) => x !== a)) })),
        () => !stop && setGot((g) => ({ ...g, failed: new Set(g.failed).add(a) })),
      );
    }
    return () => void (stop = true);
  }, [key, round]);
  return got;
}

function TraceView() {
  const { id } = useParams<{ id: string }>();
  const ref = decodeURIComponent(id);
  const search = useSearchParams();
  const router = useRouter();
  const me = useMe();
  const [round, setRound] = useState(0);
  const root = useNode<AccountDetail>(`/account/${encodeURIComponent(ref)}`);
  const follow = useMemo(() => parseFollow(search.get("follow")), [search]);
  const followed = useAccounts(follow, round);
  const [selected, setSelected] = useState<string | null>(null);
  const [full, setFull] = useState(false);

  const rootAddr = root.data?.account.address ?? null;
  const history = useMemo(() => {
    const h = new Map<string, TxDetail[]>();
    if (!root.data || !rootAddr) return h;
    h.set(rootAddr, root.data.recent_txs);
    for (const a of follow) {
      const d = followed.map.get(a);
      if (d && a !== rootAddr) h.set(a, d.recent_txs);
    }
    return h;
  }, [root.data, rootAddr, follow, followed.map]);
  const trace = useMemo(() => (rootAddr ? buildTrace(rootAddr, history) : null), [rootAddr, history]);
  const byId = useMemo(() => {
    const m = new Map<string, TxDetail>();
    for (const list of history.values()) for (const t of list) m.set(t.id, t);
    return m;
  }, [history]);
  const balances = useMemo(() => {
    const b = new Map<string, bigint>();
    if (root.data) b.set(root.data.account.address, parseDrops(root.data.balance));
    for (const d of followed.map.values()) b.set(d.account.address, parseDrops(d.balance));
    return b;
  }, [root.data, followed.map]);

  if (root.error && !root.data) return <NotFound what="account" error={root.error} />;
  if (!root.data || !trace || !rootAddr) return <Loading>Following the money around {ref}…</Loading>;

  const name = whoText(root.data.account);
  const loading = new Set(follow.filter((a) => !followed.map.has(a) && !followed.failed.has(a)));
  const setFollow = (next: string[]) => {
    setSelected(null);
    setFull(false);
    router.replace(to.trace(ref, followParam(next, rootAddr)), { scroll: false });
  };
  const toggle = (n: TraceNode) => {
    if (n.holding || n.id === rootAddr) return;
    if (!follow.includes(n.id)) return follow.length >= FOLLOW_MAX ? setFull(true) : setFollow([...follow, n.id]);
    // Stop following it, and whatever was only reachable through it.
    const rest = new Map([...history].filter(([a]) => a !== n.id));
    const reach = new Set(buildTrace(rootAddr, rest).nodes.map((x) => x.id));
    setFollow(follow.filter((a) => a !== n.id && reach.has(a)));
  };
  const moved = trace.edges.reduce((s, e) => s + e.amount, 0n);
  const sel = trace.edges.find((e) => edgeKey(e) === selected) ?? null;
  const at = new Map(trace.nodes.map((n) => [n.id, n.who]));

  return (
    <main className="wide stack-lg">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.explore()}>Explore</Link>
        <span>/</span>
        <Link href={to.account(root.data.names[0] ?? rootAddr)}>{name}</Link>
        <span>/</span>
        <span>Trace</span>
      </nav>

      <div className="page-head">
        <span className="eyebrow">Follow the money</span>
        <h1>Around {name}</h1>
        <span className="sub">Who paid {rootAddr === me?.address ? "you" : name} sits to the left; who {rootAddr === me?.address ? "you" : "it"} paid, to the right. Click an account to follow its money another hop. A line&apos;s width is how much moved.</span>
        <div className="tools">
          <button
            className="btn small"
            onClick={() => {
              root.reload();
              setRound((r) => r + 1);
            }}
          >
            Refresh
          </button>
          {follow.length > 0 && (
            <button className="btn small ghost" onClick={() => setFollow([])}>
              Back to one hop
            </button>
          )}
          <Copy text={typeof window === "undefined" ? "" : window.location.href} label="Copy link to this trace" />
          {full && (
            <span className="warn small" role="alert">
              A trace follows at most {FOLLOW_MAX} accounts. Stop following one (click it) to follow another.
            </span>
          )}
        </div>
      </div>

      <div className="figures">
        <Stat v={trace.nodes.length} k="accounts & holdings" />
        <Stat v={trace.edges.length} k="flows" />
        <Stat v={<Amount drops={moved} />} k="moved between them" />
        <Stat v={trace.counted} k={trace.since != null ? `transactions, since #${trace.since.toLocaleString("en-US")}` : "transactions read"} />
      </div>

      <Section
        title="The graph"
        aside={
          <span className="legend">
            <span><span className="sw" style={{ background: "var(--in)" }} />into {rootAddr === me?.address ? "you" : name}</span>
            <span><span className="sw" style={{ background: "var(--out)" }} />out of {rootAddr === me?.address ? "you" : name}</span>
            <span className="dim">point at any account for its own</span>
          </span>
        }
      >
        {trace.edges.length ? (
          <TraceGraph trace={trace} root={rootAddr} me={me?.address} loading={loading} failed={followed.failed} balances={balances} selected={selected} onToggle={toggle} onSelect={setSelected} />
        ) : (
          <p className="ledger-empty">No FANG has moved in or out of {name} in its latest transactions.</p>
        )}
        <p className="card-note">
          Drawn from what each followed account&apos;s latest transactions show ({plural(trace.counted, "transaction")}), so older money isn&apos;t here. Amounts are what moved, fees aside; open a flow for its receipts.
        </p>
      </Section>

      <Section title="Flows" aside={<span className="dim small">biggest first · select one for its transactions</span>}>
        <ul className="ledger flows-list">
          {trace.edges.map((e) => {
            const k = edgeKey(e);
            return (
              <li key={k}>
                <button className="ledger-row flow-row" aria-pressed={selected === k} onClick={() => setSelected(selected === k ? null : k)}>
                  <span className="ledger-main">
                    <span className="ledger-summary">
                      <Who who={at.get(e.from)} me={me?.address} link={false} /> <span className="dim">→</span> <Who who={at.get(e.to)} me={me?.address} link={false} />
                    </span>
                  </span>
                  <Amount drops={e.amount} />
                  <span className="ledger-when">{plural(e.txs.length, "tx", "txs")}</span>
                </button>
                {sel && selected === k && (
                  <div className="flow-txs">
                    <Ledger txs={e.txs.map((t) => byId.get(t)).filter((t): t is TxDetail => !!t)} side={e.from} me={me} />
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      </Section>
    </main>
  );
}
