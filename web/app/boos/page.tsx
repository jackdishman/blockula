"use client";

import Link from "next/link";
import { useState } from "react";
import { ageOf, Fang, NotFound, plural, Section, Term, useMe, whoText } from "@/components/explorer/kit";
import { nodeGet, useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { AccountsPage, ChainStatus, DirectoryAccount, NamesPage } from "@/lib/explorer/types";
import "./people.css";

/** The sorts `/api/accounts` takes, as tabs. */
const SORTS = [
  { key: "balance", label: "Richest" },
  { key: "recent", label: "Newest active" },
  { key: "names", label: "Names" },
] as const;
type Sort = (typeof SORTS)[number]["key"];

const PAGE = 50;

/** What a search box entry opens when no loaded row matches: a name (`alice` → `alice.boo`) or an address. */
const lookupRef = (q: string) => (/^t?boo1/i.test(q) || q.includes(".") ? q : `${q}.boo`);

/**
 * Boos: the chain's accounts as a directory, by name. Each row is a name (or, for an account
 * without one, its short address), what it holds, when it last did something, and a way to pay
 * it. Names are chain text, chosen by whoever registered them: React renders them as text, and
 * nothing here acts on what they say.
 */
export default function PeoplePage() {
  const me = useMe();
  const [sort, setSort] = useState<Sort>("balance");
  const [q, setQ] = useState("");
  const status = useNode<ChainStatus>("/status", { every: 5000 });
  const first = useNode<AccountsPage>(`/accounts?sort=${sort}&limit=${PAGE}`, { every: 10_000 });
  const recentNames = useNode<NamesPage>("/names?limit=12", { every: 10_000 });
  // Pages after the first, for this sort only.
  const [more, setMore] = useState<{ sort: Sort; rows: DirectoryAccount[]; next: string | null } | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);

  // A node that can't rank by activity (too many accounts) says so: fall back to the richest.
  const recentRefused = sort === "recent" && first.error && !first.data;
  if (first.error && !first.data && !recentRefused) return <NotFound what="directory" error={first.error} />;

  const page = first.data;
  const extra = more && more.sort === sort ? more : null;
  const rows = page ? [...page.accounts, ...(extra?.rows ?? [])] : [];
  const next = extra ? extra.next : page?.next ?? null;
  const head = status.data?.height;
  const blockMs = status.data?.block_time_ms ?? 1000;

  const needle = q.trim().toLowerCase().replace(/^@/, "");
  const shown = needle ? rows.filter((r) => r.names.some((n) => n.toLowerCase().includes(needle)) || r.account.address.toLowerCase().includes(needle)) : rows;

  async function loadMore() {
    if (!next || loadingMore) return;
    setLoadingMore(true);
    try {
      const p = await nodeGet<AccountsPage>(`/accounts?sort=${sort}&limit=${PAGE}&after=${encodeURIComponent(next)}`);
      setMore((m) => ({ sort, rows: [...(m && m.sort === sort ? m.rows : []), ...p.accounts], next: p.next }));
    } finally {
      setLoadingMore(false);
    }
  }

  return (
    <main className="wide stack-lg people">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.explore()}>Explore</Link>
        <span>/</span>
        <span>Boos</span>
      </nav>

      <div className="page-head">
        <span className="eyebrow">Who&apos;s on this chain</span>
        <h1>Boos</h1>
        <span className="sub">
          {page ? (
            <>
              {plural(page.accounts_total, "account")}, {plural(recentNames.data?.total ?? 0, "name")} in use. Find someone by their <Term k="name">.boo name</Term>, see what they hold, and pay them.
            </>
          ) : (
            "Everyone with an account, by name."
          )}
        </span>
        <div className="tools">
          <Link className="btn small ghost" href={to.guestbook()} data-testid="boos-guestbook">
            Sign the guestbook
          </Link>
        </div>
      </div>

      {recentNames.data && recentNames.data.names.length > 0 && (
        <Section title="Newest names" aside={<span className="dim small">{plural(recentNames.data.total, "name")} in all</span>}>
          <ul className="name-strip" data-testid="recent-names">
            {recentNames.data.names.map((n) => (
              <li key={n.name}>
                <Link className="chip name-chip" href={to.account(n.name)}>
                  {n.name}
                </Link>
                <span className="dim small">{ageOf(n.registered_at, head, blockMs)}</span>
              </li>
            ))}
          </ul>
        </Section>
      )}

      <section className="card">
        <div className="people-controls">
          <div className="filters" role="group" aria-label="Sort boos">
            {SORTS.map((s) => (
              <button key={s.key} type="button" className="filter" aria-pressed={sort === s.key} onClick={() => setSort(s.key)} data-testid={`sort-${s.key}`}>
                {s.label}
              </button>
            ))}
          </div>
          <label className="people-search">
            <input type="search" aria-label="Find someone" value={q} onChange={(e) => setQ(e.target.value)} placeholder="Find a name or address" autoComplete="off" spellCheck={false} data-testid="people-search" />
          </label>
        </div>

        {recentRefused ? (
          <p className="dim">This node has too many accounts to rank them all by activity. Try Richest or Names.</p>
        ) : !page ? (
          <p className="dim center">Gathering the living…</p>
        ) : (
          <>
            {sort === "names" && page.total === 0 && <p className="dim">Nobody has registered a name yet.</p>}
            <ol className="people-list" data-testid="people-list">
              {shown.map((r, i) => (
                <PersonRow key={r.account.address} row={r} rank={sort === "balance" && !needle ? i + 1 : null} me={me?.address ?? null} head={head} blockMs={blockMs} />
              ))}
            </ol>
            {needle && shown.length === 0 && (
              <p className="dim">
                No one loaded here matches. <Link href={to.account(lookupRef(q.trim()))}>Look up {lookupRef(q.trim())}</Link>
              </p>
            )}
            {next && !needle && (
              <button type="button" className="btn small ghost" onClick={loadMore} disabled={loadingMore} data-testid="people-more">
                {loadingMore ? "Loading…" : `Show more (${(page.total - rows.length).toLocaleString("en-US")} left)`}
              </button>
            )}
          </>
        )}
      </section>
    </main>
  );
}

/** One person: name big, short address under it, balance, last activity, and Pay (or your own actions). */
function PersonRow({ row, rank, me, head, blockMs }: { row: DirectoryAccount; rank: number | null; me: string | null; head: number | undefined; blockMs: number }) {
  const name = row.names[0] ?? null;
  const yours = me === row.account.address;
  const ref = name ?? row.account.address;
  const system = !!row.system;
  const title = name ?? (system ? whoText(row.account) : null);
  return (
    <li className={`person${rank == null ? " no-rank" : ""}${yours ? " is-you" : ""}`} data-testid="person">
      {rank != null && <span className="person-rank">{rank}</span>}
      <Link className="person-sigil" href={to.account(ref)} aria-hidden tabIndex={-1}>
        {name ? name[0].toUpperCase() : "·"}
      </Link>
      <div className="person-who">
        <Link className="person-name" href={name ? to.boo(name) : to.account(ref)}>
          {title ? system && !name ? <i>{title}</i> : title : <span className="mono">{row.account.short}</span>}
        </Link>
        {yours && <span className="you">you</span>}
        <div className="person-meta">
          {title && <span className="mono">{row.account.short}</span>}
          {row.names.length > 1 && <span>+{plural(row.names.length - 1, "name")}</span>}
          {row.validator && <span className="chip">validator</span>}
          {row.program && <span className="chip">program</span>}
          <span>{row.last_active != null ? `active ${ageOf(row.last_active, head, blockMs)}` : "no activity held"}</span>
        </div>
      </div>
      <div className="person-balance">
        <Fang display={row.balance_display} short />
      </div>
      <div className="person-actions">
        {yours ? (
          <Link className="btn small" href="/receive">
            Receive
          </Link>
        ) : (
          !system &&
          !row.program && (
            <Link className="btn small" href={`/send?to=${encodeURIComponent(ref)}`} data-testid="person-send">
              Send
            </Link>
          )
        )}
      </div>
    </li>
  );
}
