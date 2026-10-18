"use client";

import { describeAction } from "@blockula/sdk";
import Link from "next/link";
import { useParams } from "next/navigation";
import { useEffect, useState, type ReactNode } from "react";
import { Amount, ago, Hash, Loading, NotFound, Section, StatusPill, Term, useMe, useNow, Who } from "@/components/explorer/kit";
import { Ledger, signerOf } from "@/components/explorer/Ledger";
import { Copy } from "@/components/ui";
import { nodeExplorer } from "@/lib/config";
import { amountLeaves, deltaFor, effectsOf, formatDrops, fromMySide, parseDrops, plainDrops, showsAccount, type LedgerCtx } from "@/lib/explorer/ledger";
import { type NodeError, useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { AccountDetail, BlockView, ChainStatus, HuntView, TxDetail } from "@/lib/explorer/types";
import { useWallet } from "@/lib/wallet";

export default function TxPage() {
  const { id } = useParams<{ id: string }>();
  const txId = decodeURIComponent(id).toLowerCase();
  const now = useNow(1000);
  const me = useMe();
  const { wallet } = useWallet();
  // Poll until it settles: a transaction sealed in a block never changes again. One this node
  // dropped unrun hasn't settled: another node may still include it until its valid_until, and
  // then this node shows it in that block, so it keeps asking, less often.
  const [every, setEvery] = useState<number | undefined>(1000);
  const tx = useNode<TxDetail>(`/tx/${txId}`, { every });
  const t = tx.data;
  // A failure answered without the tx: `dropped: false` ran in a block and failed (its fee was
  // charged); a drop (or an older node that doesn't say) ran nowhere, and its height is where this
  // node dropped it, not a block it ran in (audit review-fix-clients-2, main13-render-3).
  const dropped = !!t && t.status === "failed" && (t.dropped ?? !t.from);
  const ranUnseen = !!t && t.status === "failed" && t.dropped === false && !t.from;
  const settled = !!t && t.status !== "pending" && !dropped;
  // Pending, then gone: the node forgot it (a restart empties the mempool). Say so and stop asking.
  const lost = !!t && t.status === "pending" && (tx.error as NodeError | null)?.status === 404;
  useEffect(() => {
    setEvery(settled || lost ? undefined : dropped ? 10_000 : 1000);
  }, [settled, lost, dropped]);
  const block = useNode<BlockView>(t?.height && !dropped ? `/block/${t.height}` : null);
  const sender = useNode<AccountDetail>(t?.from ? `/account/${t.from.address}` : null);
  // Paid to an address: did it have an account before this ran? Its own history says (an earlier
  // transaction that shows one, `showsAccount`), else the receipt can't tell whether this burned
  // the creation fee.
  const paysAddress = !!t?.to && t.status === "finalized" && (t.kind ?? t.action?.type) === "transfer" && t.action?.to === t.to.address && t.to.address !== t.from?.address;
  const recipient = useNode<AccountDetail>(paysAddress && t?.to ? `/account/${t.to.address}` : null);
  const status = useNode<ChainStatus>("/status", { every: settled || dropped ? 10_000 : 1000 });
  const huntId = t?.action && typeof t.action.hunt_id === "number" ? t.action.hunt_id : null;
  const hunt = useNode<HuntView>(huntId != null ? `/hunt/${huntId}` : null);

  if (tx.error && !t) return <NotFound what="transaction" error={tx.error} />;
  if (!t) return <Loading>Finding the transaction…</Loading>;

  const existed = (history: TxDetail[] | undefined, address: string | undefined) => !!address && !!history?.some((x) => x.height != null && t.height != null && x.height < t.height && showsAccount(x, address));
  const ctx: LedgerCtx = { recipientExisted: existed(recipient.data?.recent_txs, t.to?.address), senderExisted: existed(sender.data?.recent_txs, t.from?.address) };
  const fx = effectsOf(t, ctx);
  const mine = me ? deltaFor(t, me.address, ctx) : null;
  const involved = mine && mine.direction !== "none";
  const b = block.data;
  const keys = sender.data?.keys;
  const signer = signerOf(t, keys, wallet?.keyId);
  const signerKey = keys?.find((k) => k.key_id === t.signer_key);
  // Where this node dropped it, only as a whole block number (never echo anything else).
  const droppedAt = dropped && Number.isSafeInteger(t.height) ? ` around block #${(t.height as number).toLocaleString("en-US")}` : "";
  const head = status.data?.height;
  const fee = parseDrops(t.fee);
  const kind = t.kind ?? t.action?.type ?? "";
  const awarded = kind === "award_hunt" && hunt.data ? parseDrops(hunt.data.amount) : null;
  // The headline amount, signed only when it moved into or out of your balance: a bounty you won
  // is +, one you awarded only cost you the fee, and paying yourself moves nothing.
  const heroAmount = awarded ?? parseDrops(t.amount);
  // Only kinds whose amount moves the way the ledger reads it get a sign: a vote's amount is a
  // lock (nothing left), a claim's comes in (audit main13-protocol-9).
  const heroSign =
    kind === "award_hunt"
      ? me && hunt.data?.awarded_to?.address === me.address ? 1n : 0n
      : kind === "claim_distribution" || kind === "unshield"
        ? mine?.direction === "in" ? 1n : 0n
        : amountLeaves(kind) && mine?.direction === "out" ? -1n : amountLeaves(kind) && mine?.direction === "in" ? 1n : 0n;
  const heroNote = kind === "vote" ? "locked by the vote, not spent" : kind === "claim_distribution" ? "claimed" : null;

  // The lifecycle, step by step.
  const steps: { step: string; what: ReactNode; when?: string; state: "done" | "now" | "todo" | "bad" }[] = [
    {
      step: "Signed",
      state: "done",
      what: dropped ? (
        "by a key"
      ) : signer ? (
        <>
          by {signer.agent ? <Term k="session_key">agent</Term> : <Term k="owner_key">owner key</Term>} “{signer.label}”{signer.mine ? " (this device)" : ""}
        </>
      ) : (
        "with ML-DSA-65"
      ),
      when: t.nonce != null ? `nonce ${t.nonce}` : undefined,
    },
    {
      step: "Broadcast",
      state: dropped || lost ? "bad" : t.status === "pending" ? "now" : "done",
      what: dropped
        ? `dropped from this node's mempool without running${droppedAt} (${t.error ?? "it couldn't apply"}): nothing was charged, but another node may still include it until its valid_until, so don't send it again before then`
        : lost
          ? "this node no longer has it (it may have restarted); it was never sealed here"
          : t.status === "pending"
            ? "waiting in the mempool"
            : "accepted by the network",
      when: t.size_bytes ? `${(t.size_bytes / 1024).toFixed(1)} KB` : undefined,
    },
    {
      step: "Sealed",
      state: dropped || t.height == null ? "todo" : t.status === "failed" ? "bad" : "done",
      what: dropped ? (
        "not in any block this node holds: another node may still include it until its valid_until, so this page keeps checking"
      ) : ranUnseen ? (
        <>
          in block #{t.height?.toLocaleString("en-US")}, where it failed and its fee was charged: {t.error}. This node no longer holds that block.
        </>
      ) : t.height == null ? (
          "into the next block"
        ) : (
          <>
            in <Link href={to.block(t.height)}>block #{t.height.toLocaleString("en-US")}</Link>
            {b && (
              <>
                {" "}
                by <Who who={b.proposer} me={me?.address} />
              </>
            )}
            {t.status === "failed" && <span className="error block">but the action failed: {t.error}</span>}
          </>
        ),
      when: b && !dropped ? `${new Date(b.timestamp_ms).toLocaleString()} · ${ago(now - b.timestamp_ms)}` : undefined,
    },
    {
      step: "Final",
      state: t.height == null || dropped ? "todo" : "done",
      what: dropped ? (
        "not yet: it may still run until its valid_until passes"
      ) : t.height == null ? (
          "the moment it's sealed"
        ) : (
          <>
            {b?.signature_bytes ? <><Term k="certificate">quorum certificate</Term>, {b.signature_bytes.toLocaleString("en-US")} bytes</> : <>by a later block&apos;s <Term k="certificate">certificate</Term></>}
          </>
        ),
      when: t.height != null && head != null && !dropped ? `${(head - t.height).toLocaleString("en-US")} blocks deep` : undefined,
    },
  ];

  return (
    <main className="wide stack-lg">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.explore()}>Explore</Link>
        {t.height != null && !dropped && (
          <>
            <span>/</span>
            <Link href={to.block(t.height)}>Block #{t.height.toLocaleString("en-US")}</Link>
          </>
        )}
        <span>/</span>
        <span>Receipt</span>
      </nav>

      <section className="card plate receipt">
        <span className="eyebrow">{dropped ? "Dropped, not final" : lost ? "Lost track of it" : t.status === "pending" ? "On its way" : t.status === "failed" ? "Included, but failed" : "Receipt"}</span>
        <p className="headline">{fromMySide(t.summary, me)}</p>
        {/* What was signed, rendered here from the action itself: the node's summary isn't signed (audit gap-7-6). */}
        {t.action && <p className="dim small m0 wrap-any" data-testid="receipt-signs">Signed: {describeAction(t.action)}</p>}
        {/* The amount moved, signed from your side when it's yours; fees are itemized below. */}
        {!dropped && (t.amount || awarded != null) && (
          <div className="amount-hero" data-testid="amount-hero">
            <Amount drops={heroSign === 0n ? heroAmount : heroAmount * heroSign} signed={heroSign !== 0n} unit={false} />
            <span className="unit">FANG</span>
            {heroNote && <span className="dim small block">{heroNote}</span>}
          </div>
        )}
        <div className="row-gap">
          {/* A drop isn't a failure that ran: not final, and nothing was charged (audit review-r4-wallet-2). */}
          {dropped ? <StatusPill status="pending">dropped</StatusPill> : <StatusPill status={t.status} />}
          {t.height != null && !dropped && <span className="dim small">in block #{t.height.toLocaleString("en-US")}</span>}
          {t.memo && <span className="ledger-memo">“{t.memo}”</span>}
          <ReceiptTools summary={fromMySide(t.summary, me)} />
        </div>

        <hr className="tear" />
        <ol className="trace" aria-label="What happened">
          {steps.map((s) => (
            <li key={s.step} className={s.state === "done" ? "" : s.state}>
              <span className="step">{s.step}</span>
              <span className="what">{s.what}</span>
              {s.when && <span className="when">{s.when}</span>}
            </li>
          ))}
        </ol>

        {!dropped && (
          <>
            <hr className="tear" />
            <h2>
              {fx.pending ? "Will change" : "Balance changes"} <span className="dim small h2-aside">· what each side gained or lost</span>
            </h2>
            <div className="changes">
              {fx.changes.map((c) => (
                <div key={c.who.address} className="contents">
                  <span>
                    <Who who={c.who} me={me?.address} />
                  </span>
                  <span className="why">{c.why}</span>
                  <Amount drops={c.delta} signed exact />
                </div>
              ))}
              {awarded != null && hunt.data?.awarded_to && (
                <div className="contents">
                  <span>
                    <Who who={hunt.data.awarded_to} me={me?.address} />
                  </span>
                  <span className="why">bounty, paid from the hunt&apos;s escrow</span>
                  <Amount drops={awarded} signed exact />
                </div>
              )}
              {fx.burned > 0n && (
                <div className="contents">
                  <span className="burn">
                    <Term k="burned">Burned forever</Term>
                  </span>
                  <span className="why">{fx.burned === fee ? "the base fee" : "the base fee (the tip went to the proposer)"}</span>
                  <Amount drops={fx.burned} exact />
                </div>
              )}
            </div>
            {fx.unseen && !(awarded != null && kind === "award_hunt") && <p className="card-note">{fx.unseen}</p>}
            {fx.note && <p className="card-note">{fx.note}</p>}
          </>
        )}
        <p className="print-only small dim mt-18">
          {status.data?.chain_id ?? "Blockula"} · {typeof window === "undefined" ? "" : `${window.location.origin}${to.tx(t.id)}`} · printed {new Date(now).toLocaleString()}
        </p>
      </section>

      <div className="cols">
        <Section title="Details">
          <dl className="details">
            {t.from && (
              <>
                <dt>From</dt>
                <dd>
                  <Who who={t.from} me={me?.address} />
                </dd>
              </>
            )}
            {t.to && (
              <>
                <dt>To</dt>
                <dd>
                  <Who who={t.to} me={me?.address} />
                  <span className="mono small block dim">{t.to.address}</span>
                  {t.to_name && t.status !== "pending" && <span className="dim small block">signed to {t.to_name}: the account that owned it when this ran</span>}
                  {t.to_name && t.status === "pending" && <span className="dim small block">signed to {t.to_name}: whoever owns it when this runs (that&apos;s this account now)</span>}
                </dd>
              </>
            )}
            {t.amount && (
              <>
                <dt>Amount</dt>
                <dd>{formatDrops(parseDrops(t.amount))}</dd>
              </>
            )}
            {t.fee && (
              <>
                <dt>
                  <Term k="fee">Fee</Term>
                </dt>
                <dd>
                  {formatDrops(fee)}{" "}
                  <span className="dim small">
                    {parseDrops(t.tip) > 0n && t.max_fee != null ? `base fee burned + ${formatDrops(parseDrops(t.tip))} tip to the proposer` : "base fee, burned"}
                    {t.max_fee != null && parseDrops(t.max_fee) !== fee ? ` · it allowed up to ${formatDrops(parseDrops(t.max_fee))}` : ""}
                    {t.size_bytes ? ` · ${(Number(fee - parseDrops(t.tip)) / t.size_bytes).toFixed(1)} drops per byte` : ""}
                  </span>
                </dd>
              </>
            )}
            {t.signer_key && (
              <>
                <dt>Signed by</dt>
                <dd>
                  {signer ? (
                    <span className={`chip ${signer.agent ? "agent" : ""} ${signer.mine ? "this" : ""}`}>{signer.agent ? `agent · ${signer.label}` : signer.label}</span>
                  ) : null}{" "}
                  <Hash value={t.signer_key} />
                  {signerKey?.role.type === "session" && (
                    <span className="dim small block">
                      A <Term k="session_key">session key</Term>: up to {formatDrops(parseDrops(signerKey.role.limit))} per {signerKey.role.period_blocks.toLocaleString("en-US")} blocks, transfers only.
                    </span>
                  )}
                </dd>
              </>
            )}
            {t.nonce != null && (
              <>
                <dt>
                  <Term k="nonce" />
                </dt>
                <dd>{t.nonce}</dd>
              </>
            )}
            {t.size_bytes != null && (
              <>
                <dt>
                  <Term k="signature">Signature</Term>
                </dt>
                <dd>
                  {t.signature_scheme ?? "ML-DSA-65"} · {t.size_bytes.toLocaleString("en-US")} byte transaction
                </dd>
              </>
            )}
            <dt>
              <Term k="hash">Transaction id</Term>
            </dt>
            <dd>
              <Hash value={t.id} full />
            </dd>
          </dl>
          {t.action && (
            <details className="raw mt-16">
              <summary>What was signed, exactly</summary>
              <pre>{JSON.stringify(t.action, null, 2)}</pre>
            </details>
          )}
          <p className="card-note">
            Check it against another source: <a href={nodeExplorer.tx(t.id)} target="_blank" rel="noreferrer">the node&apos;s own explorer ↗</a>
          </p>
        </Section>

        <div className="stack-lg no-print">
          <TxActions t={t} />
          {sender.data && t.from && (
            <Section title={<>More from <Who who={t.from} me={me?.address} link={false} /></>} aside={<Link className="small" href={to.account(t.from.name && !/\s/.test(t.from.name) ? t.from.name : t.from.address)}>All →</Link>}>
              <Ledger txs={sender.data.recent_txs.filter((x) => x.id !== t.id).slice(0, 4)} side={t.from.address} me={me} keys={sender.data.keys} myKeyId={wallet?.keyId} head={head} empty="Nothing else yet." />
            </Section>
          )}
        </div>
      </div>
    </main>
  );
}

/** Keep a receipt: its link, the share sheet where there is one, or paper. */
function ReceiptTools({ summary }: { summary: string }) {
  const url = typeof window === "undefined" ? "" : window.location.href;
  const share = typeof navigator !== "undefined" && typeof navigator.share === "function";
  return (
    <span className="receipt-tools no-print">
      <Copy text={url} label="Copy link" />
      {share && (
        <button className="btn small ghost" onClick={() => navigator.share({ title: "Blockula receipt", text: summary, url }).catch(() => undefined)}>
          Share…
        </button>
      )}
      <button className="btn small ghost" onClick={() => window.print()}>
        Print
      </button>
    </span>
  );
}

/** What you can do from here, if this concerns you. */
function TxActions({ t }: { t: TxDetail }) {
  const me = useMe();
  // Locked is fine: these lead to wallet pages, which unlock first.
  if (!me || !t.from || t.status === "failed") return null;
  const name = (l: { name: string | null; address: string } | null | undefined) => (l?.name && !/\s/.test(l.name) ? l.name : l?.address ?? "");
  // Button labels use the short form of an address (the link carries the whole of it).
  const label = (l: { name: string | null; address: string; short?: string } | null | undefined) => (l?.name && !/\s/.test(l.name) ? l.name : l?.short ?? l?.address ?? "");
  const kind = t.kind ?? "";
  const out = t.from.address === me.address;
  const amount = t.amount ? plainDrops(parseDrops(t.amount)) : "";
  const links: { href: string; label: string; primary?: boolean }[] = [];
  // Again to what it was signed to: the name (whoever owns it now), or the address.
  if (kind === "transfer" && out && t.to) links.push({ href: `/send?to=${encodeURIComponent(t.to_name ?? name(t.to))}&amount=${amount}`, label: `Send ${t.to_name ?? label(t.to)} ${amount} FANG again`, primary: true });
  if (kind === "transfer" && !out && t.to?.address === me.address) links.push({ href: `/send?to=${encodeURIComponent(name(t.from))}`, label: `Pay ${label(t.from)} back`, primary: true });
  if (typeof t.action?.hunt_id === "number") links.push({ href: to.hunt(t.action.hunt_id), label: "Open the hunt" });
  if (kind === "add_key" || kind === "remove_key") links.push({ href: "/devices", label: "Devices & agents" });
  if (!links.length) return null;
  return (
    <Section title="From here">
      <div className="stack">
        {links.map((l) => (
          <Link key={l.href} className={`btn wrap ${l.primary ? "primary" : ""}`} href={l.href}>
            {l.label}
          </Link>
        ))}
      </div>
    </Section>
  );
}
