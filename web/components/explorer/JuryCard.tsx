"use client";

import Link from "next/link";
import { chainUnit } from "@/lib/format";
import { bondInput, bondToPost, displayToDrops, JOIN_JURY as JOIN, joinBlockers, JUROR_STATUS_TEXT, jurorStatus, juryRules, LEAVE_JURY as LEAVE, leaveBlockers, seanceTiming, type Blocker } from "@/lib/explorer/jury";
import { parseDrops } from "@/lib/explorer/ledger";
import { useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { AccountDetail, ChainStatus, ContributionsView } from "@/lib/explorer/types";
import { useWallet } from "@/lib/wallet";
import { ActionForm } from "./ActionForm";
import { plural, Term, useMe } from "./kit";

function Blockers({ list, testid }: { list: Blocker[]; testid: string }) {
  return (
    <ul className="jury-blockers" data-testid={testid}>
      {list.map((b) => (
        <li key={b.code} className="warn small" data-blocker={b.code}>
          {b.text}
        </li>
      ))}
    </ul>
  );
}

/**
 * Enrolling in the Séance juror pool, for this wallet's account: its Soul against the chain's
 * minimum, the bond, where it stands, when the next Séance can open, then joining or leaving.
 * Every number is the chain's (`/api/contributions` rules, the account's own record), and both
 * actions are the pool program's specs signed by the one action form (preview, then a passkey
 * check), so only an owner key signs them.
 */
export function JuryCard({ pool }: { pool: ContributionsView }) {
  const { wallet, account } = useWallet();
  const me = useMe();
  const status = useNode<ChainStatus>("/status", { every: 5000 });
  const detail = useNode<AccountDetail>(me ? `/account/${me.address}` : null, { every: 10_000 });
  const rules = juryRules(pool.rules);
  const head = status.data?.height;
  const timing = head == null ? null : seanceTiming(pool, head, status.data?.block_time_ms || 1000);

  // Only the node's answer for this very account is this account's.
  const a = me && detail.data?.account.address === me.address ? detail.data : null;
  const pc = a?.contribution ?? null;
  const juror = pc?.juror ?? null;
  const standing = jurorStatus(juror);
  const soul = pc?.soul ?? { points: 0, display: "0" };
  const myKey = account?.keys.find((k) => k.key_id === wallet?.keyId);
  const agentKeyLabel = myKey?.role.type === "session" ? myKey.label : null;
  const post = bondToPost(rules, juror ? displayToDrops(juror.bond_display) : null);
  const seat = me ? pool.open_seance?.jurors.find((j) => j.juror.address === me.address) : undefined;
  const seated = seat && pool.open_seance ? { id: pool.open_seance.id, reveal_until: pool.open_seance.reveal_until } : null;

  const cantJoin = a ? joinBlockers({ rules, status: standing, soul, post, transferable: a.transferable == null ? parseDrops(a.balance) : parseDrops(a.transferable), agentKeyLabel, sybil: !!pc?.sybil, unit: chainUnit() }) : [];
  const cantLeave = a ? leaveBlockers({ status: standing, agentKeyLabel, seated }) : [];
  const enough = rules.minSoulPoints == null ? null : soul.points >= rules.minSoulPoints;

  return (
    <section className="card plate jury-card" data-testid="jury-card">
      <span className="eyebrow">
        <Term k="seance">the Séance jury</Term>
      </span>
      <h2 className="m0">Join the jury</h2>
      <p className="dim m0">
        Jurors compare merged pull requests in pairs (“which mattered more?”), sealing their votes and then revealing them. Agreeing with the outcome earns <Term k="soul">Soul</Term> and a share of the payout; not showing up costs Soul and part of the bond. Jurors are people: only an owner key can join.
      </p>
      <dl className="kv">
        <dt>
          <Term k="soul">Your Soul</Term>
        </dt>
        <dd data-testid="jury-soul">
          {!me ? "—" : !a ? "Reading…" : soul.display}{" "}
          <span className={enough === false && a ? "warn small" : "dim small"}>
            {rules.minSoul == null ? "the node didn't say how much a juror needs" : `jurors need at least ${rules.minSoul}`}
            {a && enough ? " · enough" : a && enough === false ? " · not enough yet" : ""}
          </span>
        </dd>
        <dt>Bond</dt>
        <dd data-testid="jury-bond">
          {rules.bondDisplay ?? "—"} <span className="dim small">held while you&apos;re in the pool, and yours again when you leave. A no-show loses part of it.</span>
        </dd>
        <dt>You</dt>
        <dd data-testid="jury-status">
          {!me ? "No wallet open" : !a ? "Reading…" : JUROR_STATUS_TEXT[standing]}
          {juror && (
            <span className="dim small">
              {" "}
              · bond {juror.bond_display} · served {plural(juror.served, "Séance")}
              {standing === "benched" ? ". A juror who misses a Séance is benched until they join again." : ""}
            </span>
          )}
        </dd>
        <dt>
          <Term k="seance">Next Séance</Term>
        </dt>
        <dd data-testid="jury-cutoff">
          {!timing ? (
            "Reading…"
          ) : (
            <>
              {timing.open && (
                <span className="block" data-testid="jury-open-seance">
                  {timing.open.text} <Link href={to.seance(timing.open.id)}>{seated ? "Judge it →" : "See it →"}</Link>
                </span>
              )}
              {timing.open ? "The one after can open from " : "Can open from "}
              {timing.next}
              <span className="dim small block">
                {rules.minJury != null ? `It opens once claims by at least 2 people have matured and ${rules.minJury} jurors are available. ` : ""}
                Its jury is drawn from jurors who joined at least an epoch before it opens, so joining close to an opening seats you from the one after. {pool.jurors.active.toLocaleString("en-US")} in the pool now.
              </span>
            </>
          )}
        </dd>
      </dl>

      {standing !== "enrolled" && (
        <div className="jury-act" data-testid="jury-join">
          {cantJoin.length > 0 ? (
            <>
              <Blockers list={cantJoin} testid="jury-join-blocked" />
              <button className="btn primary self-start" disabled>
                Join the jury
              </button>
            </>
          ) : !me || a ? (
            // Mounted once the rules (and a benched juror's bond) are read, so the bond field starts from the chain's.
            <ActionForm key={bondInput(post)} spec={JOIN} initial={{ bond: bondInput(post) }} compact cta={standing === "benched" ? "Rejoin the jury" : "Join the jury"} />
          ) : null}
        </div>
      )}

      {standing !== "none" && (
        <div className="jury-act" data-testid="jury-leave">
          <span className="eyebrow">leave the jury</span>
          <p className="dim m0">Leaving takes you out of the draw and returns your bond ({juror?.bond_display}) to this account. You can join again later.</p>
          {cantLeave.length > 0 ? <Blockers list={cantLeave} testid="jury-leave-blocked" /> : <ActionForm spec={LEAVE} compact cta="Leave the jury" />}
        </div>
      )}
      {standing === "none" && a && <span className="dim small">Not enrolled, so there&apos;s nothing to leave: a way out, with your bond, shows here once you&apos;re in the pool.</span>}
    </section>
  );
}
