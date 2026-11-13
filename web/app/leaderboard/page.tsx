"use client";

import Link from "next/link";
import { useEffect, useState, type ReactNode } from "react";
import { plural, Section } from "@/components/explorer/kit";
import { chainUnit } from "@/lib/format";
import { GUESTBOOK_NAME, signaturesOf } from "@/lib/explorer/guestbook";
import { HISTORY_PAGE, HISTORY_PAGES, playersOf, refOf, topSigners, TRICKORTREAT_NAME } from "@/lib/explorer/leaderboard";
import { formatDrops } from "@/lib/explorer/ledger";
import { nodeGet, useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { Label, NameDetail, SoulRow, TxDetail } from "@/lib/explorer/types";
import "../boos/people.css";

interface HistoryPage {
  txs: TxDetail[];
  next: string | null;
}

/** Up to HISTORY_PAGES pages of an account's finalized history, newest first (`/api/account/{id}/txs`). Null until the first page has come. */
function useHistory(owner: string | null): { txs: TxDetail[] | null; error: Error | null } {
  const [state, setState] = useState<{ owner: string; txs: TxDetail[]; error: Error | null } | null>(null);
  useEffect(() => {
    if (!owner) return;
    let live = true;
    (async () => {
      const txs: TxDetail[] = [];
      let before: string | null = null;
      try {
        for (let i = 0; i < HISTORY_PAGES; i++) {
          const p: HistoryPage = await nodeGet<HistoryPage>(`/account/${encodeURIComponent(owner)}/txs?limit=${HISTORY_PAGE}${before ? `&before=${encodeURIComponent(before)}` : ""}`);
          txs.push(...p.txs);
          if (!live) return;
          setState({ owner, txs: [...txs], error: null });
          if (!p.next) break;
          before = p.next;
        }
      } catch (e) {
        if (live) setState({ owner, txs: [...txs], error: e instanceof Error ? e : new Error(String(e)) });
      }
    })();
    return () => {
      live = false;
    };
  }, [owner]);
  const mine = state && state.owner === owner ? state : null;
  return { txs: mine && (mine.txs.length || !mine.error) ? mine.txs : null, error: mine?.error ?? null };
}

/** The owner of a `.boo` name on this chain, and why there isn't one. */
function useOwner(name: string) {
  const n = useNode<NameDetail>(`/name/${name}`, { every: 30_000 });
  const owner = n.data?.status === "active" ? (n.data.owner?.address ?? null) : null;
  const missing = !n.data ? null : n.data.status === "reserved" ? "reserved" : n.data.status === "grace" ? "expired" : owner ? null : "unclaimed";
  return { owner, missing, loading: !n.data && !n.error, error: n.error };
}

/**
 * The leaderboards: three small boards, each computed here from what the node already serves
 * (nothing is stored or counted on the chain). Names are chain text: React renders them as text,
 * refOf strips what hides or reorders text, and memos are never shown.
 */
export default function LeaderboardPage() {
  return (
    <main className="wide stack-lg people">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.explore()}>Explore</Link>
        <span>/</span>
        <Link href={to.boos()}>Boos</Link>
        <span>/</span>
        <span>Leaderboard</span>
      </nav>
      <div className="page-head">
        <span className="eyebrow">Who&apos;s haunting</span>
        <h1>The leaderboard</h1>
        <span className="sub">
          Three boards worked out in your browser from the chain&apos;s public history. They count what the chain recorded, nothing more, and {chainUnit()} and Soul have no value.
        </span>
      </div>
      <SignersBoard />
      <PlayersBoard />
      <SoulBoard />
    </main>
  );
}

function Row({ who, rank, children }: { who: Label; rank: number; children: ReactNode }) {
  const { ref, text, named } = refOf(who);
  return (
    <li className="person" data-testid="board-row">
      <span className="person-rank">{rank}</span>
      <Link className="person-sigil" href={to.account(ref)} aria-hidden tabIndex={-1}>
        {named ? text[0].toUpperCase() : "·"}
      </Link>
      <div className="person-who">
        <Link className="person-name" href={named ? to.boo(ref) : to.account(ref)} data-testid="board-who">
          {named ? text : <span className="mono">{text}</span>}
        </Link>
      </div>
      <div className="person-balance">{children}</div>
    </li>
  );
}

const missingText = (name: string, why: string) =>
  why === "reserved" ? `${name}.boo is reserved on this chain at genesis, so there's nothing to rank here.` : why === "expired" ? `${name}.boo expired on this chain, so there's nothing to rank here.` : `Nobody has claimed ${name}.boo on this chain yet.`;

function SignersBoard() {
  const { owner, missing, loading, error } = useOwner(GUESTBOOK_NAME);
  const { txs, error: historyError } = useHistory(owner);
  const sigs = owner && txs ? signaturesOf(txs, owner) : [];
  const rows = topSigners(sigs);
  return (
    <Section title="Most guestbook signatures" aside={<Link href={to.guestbook()}>The guestbook</Link>}>
      {missing ? (
        <p className="dim" data-testid="signers-empty">{missingText(GUESTBOOK_NAME, missing)}</p>
      ) : error && !loading ? (
        <p className="dim" data-testid="signers-empty">This node couldn&apos;t look up {GUESTBOOK_NAME}.boo: {error.message}.</p>
      ) : historyError && !txs ? (
        <p className="dim" data-testid="signers-empty">This node couldn&apos;t list the guestbook&apos;s history: {historyError.message}.</p>
      ) : !txs ? (
        <p className="dim center">Counting…</p>
      ) : rows.length === 0 ? (
        <p className="dim" data-testid="signers-empty">No signatures yet.</p>
      ) : (
        <>
          <p className="card-note m0" data-testid="signers-scope">
            From the latest {plural(sigs.length, "signature")} ({plural(rows.length, "signer")}).
          </p>
          <ol className="people-list" data-testid="signers-list">
            {rows.map((r, i) => (
              <Row key={r.who.address} who={r.who} rank={i + 1}>
                <span data-testid="signers-count">{plural(r.signatures, "signature")}</span>
              </Row>
            ))}
          </ol>
        </>
      )}
    </Section>
  );
}

function PlayersBoard() {
  const { owner, missing, loading, error } = useOwner(TRICKORTREAT_NAME);
  const { txs, error: historyError } = useHistory(owner);
  const rows = owner && txs ? playersOf(txs, owner) : [];
  return (
    <Section title="Trick or treat" aside={<Link href={to.account(`${TRICKORTREAT_NAME}.boo`)}>{TRICKORTREAT_NAME}.boo</Link>}>
      {missing ? (
        <p className="dim" data-testid="players-empty">{missingText(TRICKORTREAT_NAME, missing)}</p>
      ) : error && !loading ? (
        <p className="dim" data-testid="players-empty">This node couldn&apos;t look up {TRICKORTREAT_NAME}.boo: {error.message}.</p>
      ) : historyError && !txs ? (
        <p className="dim" data-testid="players-empty">This node couldn&apos;t list the bot&apos;s history: {historyError.message}.</p>
      ) : !txs ? (
        <p className="dim center">Counting…</p>
      ) : rows.length === 0 ? (
        <p className="dim" data-testid="players-empty">Nobody has played yet.</p>
      ) : (
        <>
          <p className="card-note m0" data-testid="players-scope">
            Ranked by net: what the bot paid a player, less what they paid it, in {chainUnit()}, from the latest {HISTORY_PAGES * HISTORY_PAGE} transactions of {TRICKORTREAT_NAME}.boo. A play is a payment of at least 1 {chainUnit()}.
          </p>
          <ol className="people-list" data-testid="players-list">
            {rows.map((r, i) => (
              <Row key={r.who.address} who={r.who} rank={i + 1}>
                <span data-testid="players-net">
                  {r.net > 0n ? "+" : ""}
                  {formatDrops(r.net, "")}
                </span>
                <span className="dim small block" data-testid="players-detail">
                  {plural(r.plays, "play")}, won {formatDrops(r.won, "")}
                </span>
              </Row>
            ))}
          </ol>
        </>
      )}
    </Section>
  );
}

/** A Soul amount as the node displays it ("12", "3.5"), or a dash if it isn't a plain number. */
const plainNumber = (s: string) => (/^\d+(\.\d+)?$/.test(s) ? s : "—");

function SoulBoard() {
  const souls = useNode<{ souls: SoulRow[] }>("/souls?limit=20", { every: 30_000 });
  const rows = souls.data?.souls ?? [];
  return (
    <Section title="Soul">
      {souls.error && !souls.data ? (
        <p className="dim" data-testid="souls-empty">This node couldn&apos;t list Soul: {souls.error.message}.</p>
      ) : !souls.data ? (
        <p className="dim center">Counting…</p>
      ) : rows.length === 0 ? (
        <p className="dim" data-testid="souls-empty">Nobody has earned Soul on this chain yet.</p>
      ) : (
        <ol className="people-list" data-testid="souls-list">
          {rows.map((r, i) => (
            <Row key={r.account.address} who={r.account} rank={i + 1}>
              <span data-testid="souls-points">{plainNumber(r.soul.display)}</span>
            </Row>
          ))}
        </ol>
      )}
    </Section>
  );
}
