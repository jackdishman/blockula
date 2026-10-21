"use client";

import "@/app/money.css";
import { ApiError, CONVICTION_PERIODS, CONVICTION_TENTHS, convictionWeight, paramsOf, parseFang, proposalSummary, stakeSummary, type ProposalSummary } from "@blockula/sdk";
import Link from "next/link";
import { cssVars } from "@/lib/cssvars";
import { useCallback, useEffect, useState } from "react";
import { RequireWallet } from "@/components/Gate";
import { ReviewStep } from "@/components/money/ReviewStep";
import { ErrorNote, ExplorerLink, Spinner, Tip } from "@/components/ui";
import { RPC } from "@/lib/config";
import { friendly } from "@/lib/errors";
import { aboutWhen, blocksToHuman, displayFang, shortAddr } from "@/lib/format";
import { voteLockEnd, voteLocks, type HistoryTx, type LockView } from "@/lib/money";
import { voteTerms } from "@/lib/money-copy";
import type { AccountView, TxView } from "@/lib/types";
import { useUnlocked } from "@/lib/wallet";

export default function GovernancePage() {
  return (
    <RequireWallet>
      <Governance />
    </RequireWallet>
  );
}

interface Loaded {
  proposals: ProposalSummary[];
  /** One conviction lock period, in blocks, if the node says. */
  lockPeriod?: number;
  /** What this account has staked and unbonding (it votes too), at value and at cost; null: not known. */
  staked: { value: bigint; cost: bigint } | null;
}

/** What `/api/account` adds for v1 nodes: its locks (vote locks and what's still vesting). */
type Account = AccountView & { locks?: (LockView & { amount: string })[] };

const exact = (d: bigint) => displayFang(d, { exact: true });

/**
 * What an account can vote with (node/src/econ.rs `voting_balance`): its balance, staked and
 * unbonding FANG, less what's still vesting (which counts its stake at cost). Vote locks don't
 * count against it: one stake votes on many proposals.
 */
function votable(a: Account | null, staked: Loaded["staked"]): bigint | null {
  if (!a || !staked) return null;
  const vesting = (a.locks ?? []).filter((l) => l.vesting).reduce((t, l) => t + BigInt(l.amount), 0n);
  const held = BigInt(a.balance) + (vesting > 0n ? staked.cost : staked.value);
  return held > vesting ? held - vesting : 0n;
}

/** Proposals still taking votes (through `voting_ends_at`). */
const isOpen = (p: ProposalSummary, height: number) => p.status === "voting" && p.voting_ends_at >= height;
/** Only https links from the chain become hrefs. */
const safeUrl = (u: string) => (/^https:\/\/[^\s"'<>]+$/.test(u) ? u : null);
const explorerProposal = (id: number) => `${RPC}/#/proposal/${id}`;

function Governance() {
  const { client, wallet, account, status, refresh } = useUnlocked();
  const [data, setData] = useState<Loaded | "inactive" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<TxView | null>(null);

  const load = useCallback(async () => {
    try {
      const [raw, params, stake] = await Promise.all([client.proposals({ limit: 50 }), client.params().catch(() => null), client.staking(wallet.account).catch((e: unknown) => (e instanceof ApiError && e.status === 404 ? "none" : null))]);
      const list = (raw.proposals ?? []) as unknown[];
      const s = stake && stake !== "none" ? stakeSummary(stake) : null;
      const staked =
        stake === "none"
          ? { value: 0n, cost: 0n }
          : s
            ? { value: BigInt(s.bonded) + BigInt(s.unbonding_total), cost: s.delegations.reduce((t, d) => t + BigInt(d.principal), 0n) + BigInt(s.unbonding_total) }
            : null;
      setData({ proposals: list.map((p) => proposalSummary(p, raw.params)), lockPeriod: Number(paramsOf(params).lock_period_blocks) || undefined, staked });
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) setData("inactive");
      else setError(friendly(e));
    }
  }, [client, wallet]);

  useEffect(() => {
    load();
  }, [load]);

  /** Runs one governance tx, then reloads everything it changed. True when it went through. */
  const run = async (f: () => Promise<TxView>) => {
    setError(null);
    setDone(null);
    try {
      setDone(await f());
      await Promise.all([load(), refresh()]);
      return true;
    } catch (e) {
      setError(friendly(e));
      return false;
    }
  };

  if (data === null) return <main className="narrow">{error ? <ErrorNote error={error} /> : <Spinner>Loading proposals…</Spinner>}</main>;
  if (data === "inactive")
    return (
      <main className="narrow stack-lg">
        <Link href="/" className="back">← Home</Link>
        <h1>Governance</h1>
        <section className="card learn">
          <p>On-chain governance isn&apos;t active on this network yet. Once it is, proposals to change the chain&apos;s parameters, spend from the treasury or upgrade the protocol appear here, and you can vote on them.</p>
        </section>
      </main>
    );

  const height = status?.height ?? 0;
  const open = data.proposals.filter((p) => isOpen(p, height));
  const closed = data.proposals.filter((p) => !isOpen(p, height));
  // "Release expired locks" only once a vote lock has ended (USER-19): from this account's votes in
  // its recent history. When the wallet can't tell (older history), the chain answers.
  const acct = account as Account | null;
  const recent = (acct?.recent_txs ?? []) as unknown as HistoryTx[];
  const locks = voteLocks({
    recent,
    me: wallet.account,
    proposals: data.proposals,
    lockPeriodBlocks: data.lockPeriod,
    head: height,
    active: (acct?.locks ?? []).filter((l) => !l.vesting),
    historyFull: recent.filter((t) => t.height != null).length >= 25,
  });
  const canRelease = locks.ended.length > 0 || !locks.known;
  const lockHint = locks.ended.length
    ? `Your vote lock${locks.ended.length === 1 ? "" : "s"} on proposal ${locks.ended.map((id) => `#${id}`).join(", ")} ended. Release ${locks.ended.length === 1 ? "it" : "them"} to send that FANG again.`
    : !locks.known
      ? "Release vote locks that have ended:"
      : locks.nextEnd !== null
        ? `None has ended yet: your next vote lock ends ${aboutWhen(locks.nextEnd - height, status?.block_time_ms)}.`
        : "You have no vote locks to release.";
  return (
    <main className="narrow stack-lg">
      <Link href="/" className="back">← Home</Link>
      <div className="section-head">
        <span className="eyebrow">No admin keys</span>
        <h1>Governance</h1>
      </div>
      <p className="dim">
        Parameter changes, treasury spends, upgrades and community distributions are proposals that FANG holders vote on. Your vote locks the FANG you vote with; locking it longer counts more. Passed proposals wait out a timelock before they run.
      </p>
      {done && (
        <div className="banner" role="status">
          ✓ {done.summary} <ExplorerLink href={`${RPC}/#/tx/${done.id}`}>View ↗</ExplorerLink>
        </div>
      )}
      <ErrorNote error={error} />

      <section className="stack">
        <h2>Open for votes ({open.length})</h2>
        {open.length ? open.map((p) => <ProposalCard key={p.id} p={p} lockPeriod={data.lockPeriod} votable={votable(acct, data.staked)} run={run} canVote />) : <p className="dim">Nothing to vote on right now.</p>}
      </section>
      {closed.length > 0 && (
        <section className="stack">
          <h2>Earlier proposals</h2>
          {closed.map((p) => <ProposalCard key={p.id} p={p} lockPeriod={data.lockPeriod} votable={null} run={run} />)}
        </section>
      )}
      <section className="card stack">
        <h2>Vote locks</h2>
        <p className="dim small">
          Locked FANG stays in your balance but can&apos;t be sent until its lock ends. <span data-testid="locks-hint">{lockHint}</span>
        </p>
        <button className="btn" disabled={!canRelease} onClick={() => run(() => wallet.unlock())}>Release expired locks</button>
      </section>
      <Tip>Until the bootstrap period ends, a proposal can also pass if two-thirds of the active validators approve it, as long as token-weighted nays don&apos;t outweigh the ayes.</Tip>
    </main>
  );
}

function Tally({ p }: { p: ProposalSummary }) {
  const [aye, nay] = [BigInt(p.aye), BigInt(p.nay)];
  const total = aye + nay;
  const share = total > 0n ? Number((aye * 1000n) / total) / 10 : 0;
  return (
    <div>
      {total > 0n && (
        <div className="tally" role="img" aria-label={`${share}% aye`}>
          <span className="aye" ref={cssVars({ "--w": `${share}%` })} />
          <span className="nay" ref={cssVars({ "--w": `${100 - share}%` })} />
        </div>
      )}
      <div className="dim small money-wrap">
        {displayFang(aye)} aye · {displayFang(nay)} nay
        {` · ${displayFang(p.turnout)} voted`}
        {p.quorum !== undefined && ` of ${displayFang(p.quorum)} quorum${p.quorum_reached ? " ✓" : ""}`}
        {p.supermajority && (p.threshold_bps !== undefined ? ` · needs ${p.threshold_bps / 100}% aye` : " · needs a supermajority")}
        {(p.council.ayes > 0 || p.council.nays > 0) && ` · council: ${p.council.ayes} validator${p.council.ayes === 1 ? "" : "s"} approve${p.council.nays ? `, ${p.council.nays} reject` : ""}${p.council.of ? ` (of ${p.council.of})` : ""}`}
      </div>
    </div>
  );
}

type Run = (f: () => Promise<TxView>) => Promise<boolean>;

function ProposalCard({ p, lockPeriod, votable, run, canVote = false }: { p: ProposalSummary; lockPeriod?: number; votable: bigint | null; run: Run; canVote?: boolean }) {
  const { status } = useUnlocked();
  const height = status?.height ?? 0;
  const bt = status?.block_time_ms ?? 1000;
  const url = safeUrl(p.url);
  return (
    <article className="card stack" data-testid="proposal">
      <div className="row-between">
        <span className="eyebrow">Proposal #{p.id}</span>
        <span className="pill">{p.status.replace(/_/g, " ")}</span>
      </div>
      <h3 className="summary money-wrap">{p.title}</h3>
      {/* What it does, rendered here from the proposal's kind: the content a vote on it signs (with
          the title and link). The node's own summary isn't covered by the vote's digest, so a node
          could make it say anything; it isn't shown (audit gap-7-6). */}
      <p className="money-wrap" data-testid="proposal-does">{p.does}</p>
      {p.digest ? <div className="dim small mono" data-testid="proposal-digest" title={p.digest}>vote binds {p.digest.slice(0, 16)}…</div> : <div className="warn small">{p.digest_error ?? "This wallet can't read this proposal exactly"}.</div>}
      <Tally p={p} />
      <div className="dim small money-wrap">
        {[
          p.proposer && `by ${p.proposer_name ?? shortAddr(p.proposer)}`,
          p.voting_ends_at >= height ? `voting ends at block #${p.voting_ends_at} (in about ${blocksToHuman(p.voting_ends_at - height, bt)})` : `voting ended at block #${p.voting_ends_at}`,
          p.executes_at !== undefined && (p.status === "queued" ? `executes at block #${p.executes_at}` : `ran at block #${p.executes_at}`),
          p.outcome && p.status !== "voting" && p.outcome,
        ]
          .filter(Boolean)
          .join(" · ")}
      </div>
      <div className="row-gap small">
        {url && <a href={url} target="_blank" rel="noreferrer nofollow">Full text ↗</a>}
        <ExplorerLink href={explorerProposal(p.id)}>In the explorer ↗</ExplorerLink>
      </div>
      {canVote && (p.kind && p.digest ? <VoteForm p={p} lockPeriod={lockPeriod} votable={votable} run={run} /> : <p className="warn small" data-testid="cannot-vote">This wallet won&apos;t sign a vote on a proposal it can&apos;t show exactly. Vote with the blockula CLI, which reads it exactly.</p>)}
    </article>
  );
}

function VoteForm({ p, lockPeriod, votable, run }: { p: ProposalSummary; lockPeriod?: number; votable: bigint | null; run: Run }) {
  const { wallet, status } = useUnlocked();
  const [aye, setAye] = useState<boolean | null>(null);
  const [amount, setAmount] = useState("");
  // No lock beyond the vote itself unless asked for (USER-4): 1× locks for a week on mainnet.
  const [conviction, setConviction] = useState(0);
  const [reviewing, setReviewing] = useState(false);
  const [busy, setBusy] = useState(false);
  const height = status?.height ?? 0;
  const bt = status?.block_time_ms ?? 1000;
  let drops: bigint | null = null;
  let amountErr: string | null = null;
  try {
    if (amount.trim()) {
      drops = parseFang(amount);
      if (drops === 0n) amountErr = "Enter more than zero.";
      else if (votable !== null && drops > votable) amountErr = `You can vote with up to ${exact(votable)}: your balance, plus staked and unbonding FANG.`;
    }
  } catch {
    amountErr = "Use a number like 1 or 0.25 (up to 9 decimals).";
  }
  const lockFor = (c: number) => (CONVICTION_PERIODS[c] === 0 ? "locked until voting ends" : lockPeriod ? `locked ${blocksToHuman(CONVICTION_PERIODS[c] * lockPeriod, bt)} after voting ends` : `locked ${CONVICTION_PERIODS[c]} lock period${CONVICTION_PERIODS[c] === 1 ? "" : "s"} after voting ends`);
  // The lock holds through voting's last block, then the conviction's lock periods (node/src/governance.rs `vote`).
  const lockedUntil = (c: number) => {
    if (CONVICTION_PERIODS[c] > 0 && !lockPeriod) return `${CONVICTION_PERIODS[c]} lock period${CONVICTION_PERIODS[c] === 1 ? "" : "s"} after voting ends`;
    const end = voteLockEnd(p.voting_ends_at, c, lockPeriod ?? 0);
    return `${aboutWhen(end - height, bt)} (block #${end.toLocaleString("en-US")})`;
  };
  const submit = async () => {
    if (aye === null || drops === null) return;
    setBusy(true);
    // The vote binds the proposal shown here (its kind, which the card rendered, title and link):
    // a node that showed another proposal's content makes it fail instead of landing elsewhere
    // (audit gap-7-6).
    const kind = p.kind;
    await run(() => (kind ? wallet.vote(p.id, aye, drops as bigint, conviction, { proposal: { kind, title: p.title, url: p.url } }) : Promise.reject(new Error("This wallet can't show what this proposal does, so it won't sign a vote on it."))));
    setBusy(false);
    setReviewing(false);
  };
  if (reviewing && aye !== null && drops !== null)
    return (
      <div className="stack" data-testid="vote-form">
        <hr className="rule" />
        <ReviewStep
          title="Review: vote"
          summary={`Vote ${aye ? "aye" : "nay"} on proposal #${p.id} with ${exact(drops)}`}
          rows={[
            ["Proposal", `#${p.id}: ${p.title}`],
            // What the vote binds, rendered here from the proposal's kind (never the node's summary).
            ["What it does", p.does],
            ["Your vote", aye ? "Aye (for)" : "Nay (against)"],
            ["Amount", exact(drops)],
            ["Conviction", `${CONVICTION_TENTHS[conviction] / 10}×: counts as ${exact(convictionWeight(drops, conviction))}`],
            ["Locked until", <span key="l" data-testid="vote-locked-until">{lockedUntil(conviction)}</span>],
            ["Network fee", <span key="f" className="dim">a small fee, burned (the receipt shows it)</span>],
          ]}
          terms={voteTerms(exact(drops), lockedUntil(conviction), conviction)}
          confirm="Confirm vote"
          busy={busy}
          onBack={() => setReviewing(false)}
          onConfirm={submit}
        />
      </div>
    );
  return (
    <div className="stack" data-testid="vote-form">
      <hr className="rule" />
      <div className="vote-choice" role="group" aria-label="Your vote">
        <button className={`btn small ${aye === true ? "on" : ""}`} aria-pressed={aye === true} onClick={() => setAye(true)}>Aye</button>
        <button className={`btn small ${aye === false ? "on" : ""}`} aria-pressed={aye === false} onClick={() => setAye(false)}>Nay</button>
      </div>
      <label className="field">
        <span>Vote with</span>
        <div className="suffix-input">
          <input value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="0.0" inputMode="decimal" aria-label="Amount to vote with" />
          <span className="suffix">FANG</span>
        </div>
      </label>
      <div className="name-status">{amountErr ? <span className="warn">{amountErr}</span> : <span className="dim small">{votable !== null ? `You can vote with up to ${exact(votable)} (your balance, plus staked and unbonding FANG)` : "Staked FANG can vote too"}</span>}</div>
      <label className="field">
        <span>Conviction</span>
        <select className="field-select money-select" value={conviction} onChange={(e) => setConviction(Number(e.target.value))} aria-label="Conviction">
          {CONVICTION_TENTHS.map((t, c) => (
            <option key={c} value={c}>{`${t / 10}× · ${lockFor(c)}`}</option>
          ))}
        </select>
      </label>
      <button className="btn primary" disabled={aye === null || drops === null || !!amountErr || busy} onClick={() => setReviewing(true)}>
        {aye === null || drops === null ? "Vote" : `Vote ${aye ? "aye" : "nay"} with ${exact(drops)} (weight ${exact(convictionWeight(drops, conviction))})`}
      </button>
    </div>
  );
}
