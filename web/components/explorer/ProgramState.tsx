"use client";

import Link from "next/link";
import { useState } from "react";
import { compactDrops, parseDrops } from "@/lib/explorer/ledger";
import { useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { ChainStatus, ClaimView, ContributionsView, HuntsView, NameDetail, SoulRow, ValidatorRow, ValidatorsDetail } from "@/lib/explorer/types";
import { isKeyId } from "@/lib/format";
import { Bars, Meter } from "./charts";
import { Fang, plural, Section, Stat, StatusPill, Term, useMe, Who } from "./kit";

/** Live state for one program, as its page shows it. */
export function ProgramState({ id }: { id: string }) {
  if (id === "hunts") return <HuntsState />;
  if (id === "pool") return <PoolState />;
  if (id === "validators") return <ValidatorsState />;
  if (id === "names") return <NamesState />;
  return <AccountsState />;
}

function HuntsState() {
  const me = useMe();
  const [status, setStatus] = useState<"open" | "awarded" | "cancelled" | "expired" | "">("open");
  const all = useNode<HuntsView>("/hunts", { every: 15_000 });
  const list = (all.data?.hunts ?? []).filter((h) => !status || h.status === status);
  const open = (all.data?.hunts ?? []).filter((h) => h.status === "open");
  const escrow = open.reduce((s, h) => s + parseDrops(h.amount), 0n);
  return (
    <>
      <div className="figures">
        <Stat v={<>{compactDrops(escrow)}<small>FANG</small></>} k={<Term k="escrow">in escrow</Term>} />
        <Stat v={open.length} k="open hunts" />
        <Stat v={(all.data?.hunts ?? []).filter((h) => h.status === "awarded").length} k="awarded" />
        <Stat v={new Set(open.flatMap((h) => h.funders.map((f) => f.account.address))).size} k="funders" />
      </div>
      <Section title="Hunts" aside={<span className="dim small">biggest first</span>}>
        <div className="filters" role="group" aria-label="Filter hunts">
          {(["open", "awarded", "cancelled", "expired", ""] as const).map((s) => (
            <button key={s || "all"} className="filter" aria-pressed={status === s} onClick={() => setStatus(s)}>
              {s || "all"}
            </button>
          ))}
        </div>
        {list.length ? (
          <Bars
            rows={list.map((h) => ({
              key: String(h.id),
              label: (
                <Link href={to.hunt(h.id)}>
                  {h.title} <span className="dim small">{h.repo}#{h.issue}</span>
                </Link>
              ),
              value: Number(parseDrops(h.amount) / 1_000_000n) / 1000,
              note: `${h.amount_display} from ${plural(h.funders.length, "funder")}`,
            }))}
            format={(v) => `${v.toLocaleString("en-US")} FANG`}
          />
        ) : (
          <p className="ledger-empty">No {status} hunts.</p>
        )}
        {me && open.some((h) => h.creator.address === me.address) && <p className="card-note">You can award or cancel the hunts you put up from each hunt&apos;s page.</p>}
      </Section>
    </>
  );
}

const phaseText = (s: { phase: string; commit_until: number; reveal_until: number }) =>
  s.phase === "sealing" ? `sealing votes until block #${s.commit_until}` : s.phase === "revealing" ? `revealing until block #${s.reveal_until}` : `closed at block #${s.reveal_until}`;

export function claimPill(c: ClaimView) {
  return <StatusPill status={["rejected", "spam"].includes(c.status) ? "failed" : c.status === "scored" ? "finalized" : "pending"}>{c.status.replace(/_/g, " ")}</StatusPill>;
}

function PoolState() {
  const me = useMe();
  const c = useNode<ContributionsView>("/contributions", { every: 10_000 });
  const claims = useNode<{ claims: ClaimView[] }>("/claims?limit=12", { every: 10_000 });
  const souls = useNode<{ souls: SoulRow[] }>("/souls?limit=8", { every: 30_000 });
  const d = c.data;
  if (!d) return <p className="dim">Reading the pool…</p>;
  const s = d.open_seance;
  return (
    <>
      <div className="figures">
        <Stat v={<Fang display={d.pool.balance_display} short />} k={<Term k="pool">in the pool</Term>} />
        <Stat v={<Fang display={d.pool.next_budget_display} />} k={<>next <Term k="seance">Séance</Term> pays up to</>} />
        <Stat v={d.repos.length} k="repos" />
        <Stat v={d.jurors.active} k="jurors" />
      </div>
      <Section title={s ? <>Séance #{s.id} · {phaseText(s)}</> : "No Séance open"} aside={s ? <Link className="small" href={to.seance(s.id)}>Open →</Link> : null}>
        {s ? (
          <>
            <p>
              {plural(s.claims.length, "claim")} · {plural(s.jurors.length, "juror")} · budget {s.budget_display}
            </p>
            <div className="row-gap">
              {s.jurors.map((j) => (
                <span key={j.juror.address} className={`chip ${j.revealed ? "" : "agent"}`}>
                  {j.juror.name ?? j.juror.short} · {j.revealed ? "revealed" : j.committed ? "sealed" : "waiting"}
                </span>
              ))}
            </div>
          </>
        ) : (
          <p className="dim">The next one can open from block #{d.next_seance_at} once at least 2 verified claims have matured and {String(d.rules.min_jury)} jurors are available.</p>
        )}
      </Section>
      <Section title={<Term k="claim">Recent claims</Term>}>
        {claims.data?.claims.length ? (
          <ul className="ledger">
            {claims.data.claims.map((cl) => (
              <li key={cl.id}>
                <Link className="ledger-row" href={to.claim(cl.id)} style={{ gridTemplateColumns: "minmax(0,1fr) auto" }}>
                  <span className="ledger-main">
                    <span className="ledger-summary">
                      {cl.repo}#{cl.pr}
                    </span>
                    <span className="ledger-meta">
                      by {cl.claimant.name ?? cl.claimant.short}
                      {cl.familiar ? ` via ${cl.familiar}` : ""}
                      {cl.score_display ? ` · score ${cl.score_display}` : ""}
                    </span>
                  </span>
                  {claimPill(cl)}
                </Link>
              </li>
            ))}
          </ul>
        ) : (
          <p className="ledger-empty">No claims yet. Be the first.</p>
        )}
      </Section>
      {!!souls.data?.souls.length && (
        <Section title={<Term k="soul">Souls</Term>}>
          <Bars rows={souls.data.souls.map((x) => ({ key: x.account.address, label: <Who who={x.account} me={me?.address} />, value: Number(x.soul.display) || x.soul.points / 1000 }))} />
        </Section>
      )}
      <Section title="How it pays">
        <ol className="steps">
          <li>Link GitHub and get a PR merged into a pool repo{d.repos.length ? ` (${d.repos.map((r) => r.repo).join(", ")})` : ""}.</li>
          <li>Claim it with a refundable bond ({String(d.rules.claim_bond_display)}). Validators check GitHub that it&apos;s merged and yours.</li>
          <li>A Séance of up to {String(d.rules.jury_size)} human jurors compares claims in pairs, sealing then revealing their votes.</li>
          <li>Each Séance pays up to {String(d.rules.pool_share_per_seance)} of the pool, square-rooted per person, vesting over {Number(d.rules.vesting_blocks).toLocaleString("en-US")} blocks.</li>
        </ol>
      </Section>
    </>
  );
}

function ValidatorsState() {
  const me = useMe();
  const v = useNode<ValidatorsDetail>("/validators", { every: 10_000 });
  const d = v.data;
  if (!d) return <p className="dim">Reading the validator set…</p>;
  const list = (k: keyof ValidatorsDetail) => (Array.isArray(d[k]) ? (d[k] as ValidatorRow[]) : []);
  const row = (x: ValidatorRow) => (
    <li key={x.validator.address} style={{ padding: "11px 0" }}>
      <div className="row-between">
        <Who who={x.validator} me={me?.address} />
        <StatusPill status={x.jailed ? "failed" : x.status === "active" ? "finalized" : "pending"}>{x.jailed ? "jailed" : x.status}</StatusPill>
      </div>
      <span className="dim small">
        {[
          x.genesis ? "genesis" : null,
          x.stake_display ? `stake ${x.stake_display}` : null,
          x.bond_display && x.bond !== "0" ? `bond ${x.bond_display}` : null,
          x.commission_display ? `commission ${x.commission_display}` : null,
          x.liveness?.recorded ? `missed ${x.liveness.missed} of ${x.liveness.recorded} turns` : null,
          x.p2p_addr ?? null,
        ]
          .filter(Boolean)
          .join(" · ")}
      </span>
    </li>
  );
  const groups: [string, ValidatorRow[], string][] = [
    ["Active this epoch", list("active"), ""],
    ["Waiting for approval", [...list("pending"), ...list("candidates")], "Nobody is waiting. Run `blockula validator init` to become one."],
    ["Joining at the next epoch", [...list("approved"), ...list("joining")], ""],
    ["Jailed", list("jailed"), ""],
    ["Removed", list("removed"), ""],
  ];
  const epochBlocks = d.epoch_blocks ?? 0;
  const left = d.blocks_until_next_epoch ?? 0;
  return (
    <>
      <div className="figures">
        <Stat v={d.epoch} k={<>current <Term k="epoch" /></>} />
        <Stat v={list("active").length} k="active" />
        <Stat v={left.toLocaleString("en-US")} k="blocks to the next epoch" />
        <Stat v={d.min_bond_display ? <Fang display={d.min_bond_display} /> : "—"} k={<>minimum <Term k="bond" /></>} />
      </div>
      {epochBlocks > 0 && (
        <div className="stack" style={{ gap: 6 }}>
          <Meter used={BigInt(epochBlocks - left)} limit={BigInt(epochBlocks)} label={`Epoch ${d.epoch} is ${Math.round(((epochBlocks - left) / epochBlocks) * 100)}% through`} />
          <span className="dim small">
            Epoch {d.epoch} is {Math.round(((epochBlocks - left) / epochBlocks) * 100)}% through its {epochBlocks.toLocaleString("en-US")} blocks. The set only changes at the boundary.
          </span>
        </div>
      )}
      {groups
        .filter(([, xs, empty]) => xs.length || empty)
        .map(([title, xs, empty]) => (
          <Section key={title} title={`${title} (${xs.length})`}>
            {xs.length ? <ul className="ledger">{xs.map(row)}</ul> : <p className="ledger-empty">{empty}</p>}
          </Section>
        ))}
      {!!d.admins?.length && (
        <p className="dim small">
          Season 0 admins: {d.admins.map((a, i) => (
            <span key={a.address}>
              {i ? ", " : ""}
              <Who who={a} me={me?.address} />
            </span>
          ))}
        </p>
      )}
    </>
  );
}

function NamesState() {
  const status = useNode<ChainStatus>("/status", { every: 10_000 });
  const [q, setQ] = useState("");
  const name = q.trim().toLowerCase().replace(/\.boo$/, "");
  const ok = /^[a-z0-9](?:[a-z0-9-]{1,30}[a-z0-9])$/.test(name);
  const look = useNode<NameDetail>(ok ? `/name/${encodeURIComponent(name)}` : null);
  return (
    <>
      <div className="figures">
        <Stat v={status.data?.names?.toLocaleString("en-US") ?? "—"} k="names registered" />
        <Stat v={status.data?.accounts?.toLocaleString("en-US") ?? "—"} k="accounts" />
      </div>
      <Section title="Look up a name">
        <label className="field">
          <span>Name</span>
          <div className="suffix-input">
            <input value={q} onChange={(e) => setQ(e.target.value)} placeholder="ghoul" autoCapitalize="none" spellCheck={false} />
            <span className="suffix">.boo</span>
          </div>
        </label>
        <div className="name-status" aria-live="polite" style={{ marginTop: 8 }}>
          {name && !ok && <span className="warn">3 to 32 of a–z, 0–9 and dashes.</span>}
          {ok && look.data && (look.data.available ? (
            <span className="ok">
              {look.data.name} is free for {look.data.price_display}. <Link href={to.name(name)}>Claim it →</Link>
            </span>
          ) : (
            <span>
              {look.data.name} belongs to <Who who={look.data.owner} />. <Link href={to.name(name)}>Details →</Link>
            </span>
          ))}
        </div>
        <p className="card-note">Three-letter names are scarcest and cost the most; five letters and up are cheapest. The price goes to the treasury.</p>
      </Section>
    </>
  );
}

function AccountsState() {
  const status = useNode<ChainStatus>("/status", { every: 10_000 });
  const [q, setQ] = useState("");
  const key = q.trim().toLowerCase();
  const look = useNode<{ key_id: string; accounts: { account: { address: string; name: string | null; short: string }; role: { type: string }; label: string }[] }>(isKeyId(key) ? `/key/${key}` : null);
  return (
    <>
      <div className="figures">
        <Stat v={status.data?.accounts?.toLocaleString("en-US") ?? "—"} k="accounts" />
        <Stat v={<Fang display={status.data?.supply_display} short />} k="total supply" />
        <Stat v={<Fang display={status.data?.burned_display} />} k={<Term k="burned" />} />
      </div>
      <Section title="Which accounts can this key sign for?">
        <label className="field">
          <span>Key id</span>
          <input className="mono" value={q} onChange={(e) => setQ(e.target.value)} placeholder="64 hex characters" autoCapitalize="none" spellCheck={false} />
        </label>
        {look.data && (
          <ul className="ledger" style={{ marginTop: 12 }}>
            {look.data.accounts.map((a) => (
              <li key={a.account.address} style={{ padding: "10px 0" }} className="row-between">
                <Who who={a.account} />
                <span className="chip">
                  {a.role.type === "session" ? "agent" : "owner"} · {a.label}
                </span>
              </li>
            ))}
          </ul>
        )}
        <p className="card-note">Accounts hold many keys: an owner key per passkey device, recovery keys, and spend-limited agent keys. Every transaction names the account it acts for; the signing key must be on it.</p>
      </Section>
    </>
  );
}
