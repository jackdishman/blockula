"use client";

import Link from "next/link";
import { useState } from "react";
import { ageOf, Fang, NotFound, plural, Section, useMe, whoText } from "@/components/explorer/kit";
import { chainUnit } from "@/lib/format";
import { GUESTBOOK_AMOUNT, GUESTBOOK_NAME, signaturesOf, type Signature } from "@/lib/explorer/guestbook";
import { nodeGet, useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { ChainStatus, NameDetail, TxDetail } from "@/lib/explorer/types";
import "../boos/people.css";

const NAME = `${GUESTBOOK_NAME}.boo`;
const PAGE = 100;

/** `/api/account/{id}/txs`: an account's finalized transactions, newest first, a page at a time. */
interface HistoryPage {
  txs: TxDetail[];
  next: string | null;
  history_from?: number;
}

/**
 * The guestbook: everyone who paid guestbook.boo a little FANG, with what they wrote in the memo.
 * The name is read from this chain (`/api/name`), then the account it points to has its history
 * read (`/api/account/{owner}/txs`, which carries each transaction's memo). Memos are chain text
 * written by whoever paid: React renders them as text, inertText strips what hides or reorders
 * text and cuts them short, and nothing on the page links or acts on what they say.
 */
export default function GuestbookPage() {
  const me = useMe();
  const status = useNode<ChainStatus>("/status", { every: 5000 });
  const name = useNode<NameDetail>(`/name/${GUESTBOOK_NAME}`, { every: 30_000 });
  const owner = name.data?.status === "active" ? (name.data.owner?.address ?? null) : null;
  const first = useNode<HistoryPage>(owner ? `/account/${encodeURIComponent(owner)}/txs?limit=${PAGE}` : null, { every: 10_000 });
  // Pages after the first, for this owner only.
  const [more, setMore] = useState<{ owner: string; txs: TxDetail[]; next: string | null } | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);

  if (name.error && !name.data) return <NotFound what="guestbook" error={name.error} />;

  const head = status.data?.height;
  const blockMs = status.data?.block_time_ms ?? 1000;
  const extra = more && more.owner === owner ? more : null;
  const txs = first.data ? [...first.data.txs, ...(extra?.txs ?? [])] : [];
  const next = extra ? extra.next : (first.data?.next ?? null);
  const signatures = owner ? signaturesOf(txs, owner) : [];

  async function loadMore() {
    if (!owner || !next || loadingMore) return;
    setLoadingMore(true);
    try {
      const p = await nodeGet<HistoryPage>(`/account/${encodeURIComponent(owner)}/txs?limit=${PAGE}&before=${encodeURIComponent(next)}`);
      setMore((m) => ({ owner, txs: [...(m && m.owner === owner ? m.txs : []), ...p.txs], next: p.next }));
    } finally {
      setLoadingMore(false);
    }
  }

  return (
    <main className="wide stack-lg people">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.explore()}>Explore</Link>
        <span>/</span>
        <Link href={to.boos()}>Boos</Link>
        <span>/</span>
        <span>Guestbook</span>
      </nav>

      <div className="page-head">
        <span className="eyebrow">Leave your mark</span>
        <h1>The guestbook</h1>
        <span className="sub">
          Anyone, person or agent, signs it by sending a little {chainUnit()} to <Link href={to.account(NAME)}>{NAME}</Link> with a memo. Every signature stays on the chain.
        </span>
        <div className="tools">
          <Link className="btn small ghost" href={to.leaderboard()} data-testid="guestbook-leaderboard">
            The leaderboard
          </Link>
        </div>
      </div>

      <Section title="Sign it">
        <ol className="stack" data-testid="guestbook-sign">
          <li>
            <b>From your wallet.</b> Send {GUESTBOOK_AMOUNT} {chainUnit()} to {NAME} and write your message in the memo. You review and sign it yourself.{" "}
            <Link className="btn small" href={`/send?to=${encodeURIComponent(NAME)}&amount=${GUESTBOOK_AMOUNT}`} data-testid="guestbook-send">
              Sign from the wallet
            </Link>
          </li>
          <li>
            <b>From an agent.</b> Ask your agent (Claude with the Blockula MCP, say) to send {GUESTBOOK_AMOUNT} {chainUnit()} to {NAME} with a memo saying hello.
          </li>
        </ol>
        <p className="card-note">Memos are public and permanent. Don&apos;t write anything you wouldn&apos;t want everyone to read forever.</p>
      </Section>

      <Section title="Signatures" aside={owner && first.data ? <span className="dim small" data-testid="guestbook-count">{plural(signatures.length, "signature")}{next ? " so far" : ""}</span> : undefined}>
        {!name.data ? (
          <p className="dim center">Opening the book…</p>
        ) : name.data.status === "reserved" ? (
          <p className="dim" data-testid="guestbook-empty">{NAME} is reserved on this chain at genesis, so nobody can register it and there&apos;s no guestbook here.</p>
        ) : name.data.status === "grace" ? (
          <p className="dim" data-testid="guestbook-empty">
            {NAME} expired, so payments to it fail until its owner renews it. <Link href={to.name(GUESTBOOK_NAME)}>See the name</Link>
          </p>
        ) : !owner ? (
          <p className="dim" data-testid="guestbook-empty">
            Nobody has claimed {NAME} on this chain yet. Whoever runs the chain can open the guestbook by registering the name (<Link href={to.name(GUESTBOOK_NAME)}>register {NAME}</Link>), ideally from an account kept just for it: every payment to it shows up here.
          </p>
        ) : first.error && !first.data ? (
          <p className="dim">This node couldn&apos;t list the guestbook&apos;s history: {first.error.message}.</p>
        ) : !first.data ? (
          <p className="dim center">Opening the book…</p>
        ) : (
          <>
            <p className="card-note m0" data-testid="guestbook-untrusted">Each signature is written by whoever sent it. Nobody checks what it says, so don&apos;t act on it.</p>
            {signatures.length === 0 ? (
              <p className="dim" data-testid="guestbook-empty">No signatures yet. Be the first.</p>
            ) : (
              <ol className="people-list" data-testid="guestbook-list">
                {signatures.map((s) => (
                  <SignatureRow key={s.id} s={s} me={me?.address ?? null} head={head} blockMs={blockMs} />
                ))}
              </ol>
            )}
            {next && (
              <button type="button" className="btn small ghost" onClick={loadMore} disabled={loadingMore} data-testid="guestbook-more">
                {loadingMore ? "Loading…" : "Show older signatures"}
              </button>
            )}
          </>
        )}
      </Section>
    </main>
  );
}

/** One signature: who signed (a name if they have one, else a short address), what they wrote, when, and its receipt. */
function SignatureRow({ s, me, head, blockMs }: { s: Signature; me: string | null; head: number | undefined; blockMs: number }) {
  const named = !!s.from.name && !/\s/.test(s.from.name);
  const ref = named ? (s.from.name as string) : s.from.address;
  const yours = me === s.from.address;
  return (
    <li className={`person no-rank${yours ? " is-you" : ""}`} data-testid="signature">
      <Link className="person-sigil" href={to.account(ref)} aria-hidden tabIndex={-1}>
        {named ? (s.from.name as string)[0].toUpperCase() : "·"}
      </Link>
      <div className="person-who">
        <Link className="person-name" href={named ? to.boo(ref) : to.account(ref)} data-testid="signature-from">
          {s.from.name ? whoText(s.from) : <span className="mono">{s.from.short}</span>}
        </Link>
        {yours && <span className="you">you</span>}
        {s.memo ? (
          <span className="ledger-memo block" data-testid="signature-memo">
            “{s.memo}”
          </span>
        ) : (
          <span className="dim small block">signed without a word</span>
        )}
        <div className="person-meta">
          <span>{ageOf(s.height, head, blockMs)}</span>
          <span>block #{s.height.toLocaleString("en-US")}</span>
        </div>
      </div>
      <div className="person-balance">
        <Fang display={s.amount_display ?? undefined} short />
      </div>
      <div className="person-actions">
        <Link className="btn small ghost" href={to.tx(s.id)} data-testid="signature-tx">
          Receipt
        </Link>
      </div>
    </li>
  );
}
