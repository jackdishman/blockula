"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import { useState } from "react";
import { FlowChart, Meter } from "@/components/explorer/charts";
import { Amount, Ext, Fang, Hash, Loading, NotFound, plural, Section, Stat, StatusPill, Term, useMe, whoText } from "@/components/explorer/kit";
import { Ledger } from "@/components/explorer/Ledger";
import { WatchButton } from "@/components/explorer/Watch";
import { blocksToHuman, keyCode } from "@/lib/format";
import { nodeExplorer } from "@/lib/config";
import { activityCsv } from "@/lib/explorer/csv";
import { bySigner, deltaFor, flowsOf, formatDrops, parseDrops } from "@/lib/explorer/ledger";
import { useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { AccountDetail, ChainStatus, TxDetail } from "@/lib/explorer/types";
import type { KeyView } from "@/lib/types";
import { useWallet } from "@/lib/wallet";

type Filter = { by: "all" } | { by: "in" } | { by: "out" } | { by: "key"; key: string };

/** Save transactions as a CSV file, amounts signed from `side`. */
function downloadCsv(txs: TxDetail[], side: string, keys: AccountDetail["keys"], file: string) {
  const url = URL.createObjectURL(new Blob([activityCsv(txs, side, keys)], { type: "text/csv;charset=utf-8" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = `${file.replace(/[^\w.-]+/g, "_")}-activity.csv`;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export default function AccountPage() {
  const { id } = useParams<{ id: string }>();
  const ref = decodeURIComponent(id);
  const me = useMe();
  const { wallet } = useWallet();
  const acct = useNode<AccountDetail>(`/account/${encodeURIComponent(ref)}`, { every: 3000 });
  const status = useNode<ChainStatus>("/status", { every: 5000 });
  const [filter, setFilter] = useState<Filter>({ by: "all" });

  if (acct.error && !acct.data) {
    // A name nobody has registered isn't an account yet; it might be yours to claim.
    if (!/^t?boo1/i.test(ref) && /claimed|registered/.test(acct.error.message)) return <UnclaimedName name={ref} />;
    return <NotFound what="account" error={acct.error} />;
  }
  const a = acct.data;
  if (!a) return <Loading>Looking up {ref}…</Loading>;

  const addr = a.account.address;
  const yours = me?.address === addr;
  const name = a.names[0] ?? null;
  const system = !!a.account.name && /\s/.test(a.account.name);
  const head = status.data?.height ?? 0;
  const blockMs = status.data?.block_time_ms ?? 1000;
  const sealed = a.recent_txs.filter((t) => t.status !== "pending");
  const flows = flowsOf(a.recent_txs, addr);
  const signers = bySigner(a.recent_txs, addr);
  const owners = a.keys.filter((k) => k.role.type === "owner");
  const agents = a.keys.filter((k) => k.role.type === "session");
  const familiar = new Set(a.familiars.map((f) => f.key_id));

  const shown = a.recent_txs.filter((t) => {
    if (filter.by === "all") return true;
    if (filter.by === "key") return t.signer_key === filter.key && t.from?.address === addr;
    return deltaFor(t, addr).direction === filter.by;
  });
  const pc = a.contribution;
  const hasContribution = pc && (pc.soul.points > 0 || pc.claims.length || pc.juror || pc.vouched_by || pc.vouching_for.length || pc.rewards.total_display !== "0 FANG");

  return (
    <main className="wide stack-lg">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.explore()}>Explore</Link>
        <span>/</span>
        <span>Account</span>
      </nav>

      <div className="page-head">
        <span className="eyebrow">{yours ? "Your account, as the chain sees it" : system ? "System account" : "Account"}</span>
        <h1>
          {name ?? (system ? <i>{whoText(a.account)}</i> : "Unnamed account")}
          {yours && <span className="you big">you</span>}
        </h1>
        <span className="sub">
          <Term k="address" />: <Hash value={addr} />
        </span>
        <div className="tools">
          {a.github && (
            <Ext className="chip" href={`https://github.com/${encodeURIComponent(a.github.handle)}`}>
              github · @{a.github.handle}
            </Ext>
          )}
          {a.familiars.length > 0 && <span className="chip agent">{plural(a.familiars.length, "familiar")}</span>}
          {pc && pc.soul.points > 0 && <span className="chip">soul · {pc.soul.display}</span>}
          {pc?.sybil && <span className="pill failed" title={pc.sybil.reason}>flagged as a sybil</span>}
          {!yours && me && !system && (
            <Link className="btn small primary" href={`/send?to=${encodeURIComponent(name ?? addr)}`}>
              Send to {name ?? "this address"}
            </Link>
          )}
          {!yours && <WatchButton who={a.account} />}
          <Link className="btn small ghost" href={to.trace(name ?? addr)}>
            Follow the money
          </Link>
          {yours && (
            <>
              <Link className="btn small primary" href="/send">Send</Link>
              <Link className="btn small" href="/receive">Receive</Link>
              <Link className="btn small ghost" href="/devices">Devices &amp; agents</Link>
            </>
          )}
        </div>
      </div>

      <div className="figures">
        <Stat v={<Fang display={a.balance_display} short />} k="balance" />
        {a.locked != null && parseDrops(a.locked) > 0n && <Stat v={<Fang display={a.transferable_display} short />} k={<>can move · <Term k="locked">{a.locked_display} locked</Term></>} />}
        <Stat v={a.nonce.toLocaleString("en-US")} k={<Term k="nonce">transactions sent</Term>} />
        <Stat v={a.names.length} k={<Term k="name">names</Term>} />
        <Stat v={`${owners.length} · ${agents.length}`} k={<><Term k="owner_key">owners</Term> · <Term k="session_key">agents</Term></>} />
      </div>

      <div className="cols">
        <div className="stack-lg">
          <Section title="Money in &amp; out" aside={flows.counted ? <span className="dim small">last {plural(flows.counted, "transaction")}{flows.from != null ? ` · blocks #${flows.from}–#${flows.to}` : ""}</span> : null}>
            {flows.rows.length ? (
              <>
                <FlowChart rows={flows.rows} me={me?.address} />
                <div className="row-between small flow-sum">
                  <span>
                    <span className="keyline in" />
                    in <Amount drops={flows.totalIn} />
                  </span>
                  <span>
                    <span className="keyline out" />
                    out <Amount drops={flows.totalOut} />
                  </span>
                  <span className="dim">
                    <Term k="burned">burned</Term> in fees <Amount drops={flows.burned} />
                  </span>
                </div>
                {flows.unseen > 0 && (
                  <p className="card-note">
                    {plural(flows.unseen, "of these", "of these")} may also have moved FANG the transaction itself doesn&apos;t record (a name&apos;s price, a bounty, a bond, the fee for opening a recipient&apos;s account). Open one to see what&apos;s known.
                  </p>
                )}
                <Link className="small block mt-10" href={to.trace(name ?? addr)}>
                  Follow these flows further, hop by hop →
                </Link>
              </>
            ) : (
              <p className="ledger-empty">No FANG has moved in or out yet.</p>
            )}
          </Section>

          <Section
            title="Activity"
            aside={
              <span className="dim small">
                newest first · {sealed.length < a.recent_txs.length ? `${a.recent_txs.length - sealed.length} pending · ` : ""}last {a.recent_txs.length}
                {shown.length > 0 && (
                  <>
                    {" · "}
                    <button className="link small" onClick={() => downloadCsv(shown, addr, a.keys, name ?? addr)} title="Download these transactions as a spreadsheet, signed from this account's side; its exact and note columns say which rows moved more than the transaction records">
                      Export CSV
                    </button>
                  </>
                )}
              </span>
            }
          >
            <div className="filters" role="group" aria-label="Filter activity">
              {(["all", "in", "out"] as const).map((f) => (
                <button key={f} className="filter" aria-pressed={filter.by === f} onClick={() => setFilter({ by: f })}>
                  {f === "all" ? "All" : f === "in" ? "Money in" : "Money out"}
                </button>
              ))}
              {a.keys
                .filter((k) => signers.has(k.key_id))
                .map((k) => (
                  <button key={k.key_id} className="filter" aria-pressed={filter.by === "key" && filter.key === k.key_id} onClick={() => setFilter({ by: "key", key: k.key_id })} title={`Only what ${k.label} signed`}>
                    {k.role.type === "session" ? `agent · ${k.label}` : k.label}
                  </button>
                ))}
            </div>
            <Ledger txs={shown} side={addr} me={me} keys={a.keys} myKeyId={wallet?.keyId} head={head} blockMs={blockMs} empty={filter.by === "all" ? "No activity yet. Spooky quiet." : "Nothing matches that filter."} />
          </Section>
        </div>

        <div className="stack-lg">
          <Section title="Keys" aside={yours ? <Link className="small" href="/devices">Manage →</Link> : <span className="dim small">who can sign</span>}>
            <ul className="ledger">
              {[...owners, ...agents].map((k) => (
                <KeyRow key={k.key_id} k={k} head={head} blockMs={blockMs} signed={signers.get(k.key_id)} familiar={familiar.has(k.key_id)} mine={k.key_id === wallet?.keyId} onPick={() => setFilter({ by: "key", key: k.key_id })} />
              ))}
            </ul>
            <p className="card-note">
              An account is a list of keys. Owners can do anything; agents can only send, within a limit the chain enforces.
            </p>
          </Section>

          {!!a.locks?.length && (
            <Section title={<Term k="locked">Locked</Term>} aside={<span className="dim small">can be staked, not sent</span>}>
              <ul className="ledger" data-testid="locks">
                {a.locks.map((l) => (
                  <li key={l.id} className="row-between pad-row">
                    <span>
                      {l.reason}
                      <span className="dim small block">{l.vesting ? `vesting until block #${l.until.toLocaleString("en-US")}` : `until block #${l.until.toLocaleString("en-US")}`}</span>
                    </span>
                    <span className="amt">{l.amount_display}</span>
                  </li>
                ))}
              </ul>
            </Section>
          )}

          {a.names.length > 0 && (
            <Section title={<Term k="name">Names</Term>}>
              <div className="row-gap">
                {a.names.map((n) => (
                  <Link key={n} className="chip name-chip" href={to.name(n)}>
                    {n}
                  </Link>
                ))}
              </div>
            </Section>
          )}

          {(a.hunts_created.length > 0 || a.hunts_won.length > 0) && (
            <Section title={<Term k="hunt">Hunts</Term>}>
              <ul className="ledger">
                {a.hunts_won.map((h) => (
                  <li key={`w${h.id}`}>
                    <Link className="ledger-row two" href={to.hunt(h.id)}>
                      <span>
                        Won {h.repo}#{h.issue} for PR #{h.pr} {h.self_awarded && <span className="pill pending">self-awarded</span>}
                      </span>
                      <span className="amt">{h.amount_display}</span>
                    </Link>
                  </li>
                ))}
                {a.hunts_created.map((hid) => (
                  <li key={`c${hid}`}>
                    <Link className="ledger-row one" href={to.hunt(hid)}>
                      <span>Put up hunt #{hid}</span>
                    </Link>
                  </li>
                ))}
              </ul>
            </Section>
          )}

          {hasContribution && pc && (
            <Section title={<Term k="pool">Contribution</Term>} aside={<Link className="small" href={to.program("pool")}>The pool →</Link>}>
              <dl className="details tight-120">
                <dt><Term k="soul" /></dt>
                <dd>{pc.soul.display} <span className="dim small">({pc.soul.earned_display} earned)</span></dd>
                <dt><Term k="vesting">Rewards</Term></dt>
                <dd>{pc.rewards.withdrawable_display} withdrawable · {pc.rewards.unvested_display} vesting</dd>
                {pc.juror && (
                  <>
                    <dt><Term k="seance">Juror</Term></dt>
                    <dd>{pc.juror.active ? "in the pool" : "benched"} · bond {pc.juror.bond_display} · {plural(pc.juror.served, "Séance")}</dd>
                  </>
                )}
                {pc.claims.length > 0 && (
                  <>
                    <dt><Term k="claim">Claims</Term></dt>
                    <dd>
                      {pc.claims.map((c) => (
                        <span key={c.id} className="block">
                          <Link href={to.claim(c.id)}>
                            {c.repo}#{c.pr}
                          </Link>{" "}
                          <StatusPill status={c.status === "scored" ? "finalized" : ["rejected", "spam"].includes(c.status) ? "failed" : "pending"}>{c.status.replace(/_/g, " ")}</StatusPill>
                        </span>
                      ))}
                    </dd>
                  </>
                )}
              </dl>
            </Section>
          )}

          <p className="dim small">
            Second opinion: <a href={nodeExplorer.account(name ?? addr)} target="_blank" rel="noreferrer">this account on the node&apos;s explorer ↗</a>
          </p>
        </div>
      </div>
    </main>
  );
}

function KeyRow({ k, head, blockMs, signed, familiar, mine, onPick }: { k: KeyView; head: number; blockMs: number; signed?: { count: number; spent: bigint }; familiar: boolean; mine: boolean; onPick: () => void }) {
  const r = k.role;
  return (
    <li className="pad-row-12">
      <div className="row-between">
        <span>
          <span className="key-label big">{k.label}</span>{" "}
          {r.type === "session" ? <span className="chip agent">{familiar ? "familiar" : "agent"}</span> : <span className="chip">owner</span>} {mine && <span className="chip this">this device</span>}
        </span>
        {signed ? (
          <button className="link small" onClick={onPick} title="Show only what this key signed">
            {plural(signed.count, "tx", "txs")} →
          </button>
        ) : null}
      </div>
      <span className="mono small dim">{keyCode(k.key_id)} · {k.added_at ? `added at #${k.added_at}` : "since creation"}</span>
      {r.type === "session" && <Budget k={k} head={head} blockMs={blockMs} />}
    </li>
  );
}

/** A session key's spending this period against its limit: the chain's own numbers. */
function Budget({ k, head, blockMs }: { k: KeyView; head: number; blockMs: number }) {
  if (k.role.type !== "session") return null;
  const r = k.role;
  const over = head >= k.window_start + r.period_blocks;
  const spent = over ? 0n : parseDrops(k.spent_this_period);
  const limit = parseDrops(r.limit);
  const expired = head > r.expires_at;
  const resets = Math.max(0, k.window_start + r.period_blocks - head);
  return (
    <div className="stack gap-4 mt-8">
      <Meter used={spent} limit={limit} label={`${k.label} has spent ${formatDrops(spent)} of ${formatDrops(limit)} this period`} />
      <span className="small">
        <Amount drops={spent} /> of <Amount drops={limit} /> per {blocksToHuman(r.period_blocks, blockMs)}
        <span className="dim">
          {" · "}
          {expired ? <span className="warn">expired at #{r.expires_at}</span> : spent > 0n ? `resets in ${blocksToHuman(resets, blockMs)}` : `expires in ${blocksToHuman(r.expires_at - head, blockMs).replace(/^day$/, "1 day")}`}
        </span>
      </span>
    </div>
  );
}

function UnclaimedName({ name }: { name: string }) {
  const n = name.toLowerCase().replace(/\.boo$/, "");
  return (
    <main className="wide stack-lg">
      <section className="card">
        <span className="eyebrow">Nobody&apos;s here yet</span>
        <h1>{n}.boo isn&apos;t anyone&apos;s</h1>
        <p className="dim">No account has registered this name. Check the spelling, or claim it.</p>
        <Link className="btn primary self-start" href={to.name(n)}>
          See {n}.boo
        </Link>
      </section>
    </main>
  );
}
