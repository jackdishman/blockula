"use client";

import { booProfileUrl } from "@blockula/sdk";
import Link from "next/link";
import { GithubChip } from "@/components/creators";
import { Hash, Loading, NotFound, plural, Section, Stat, StatusPill, Term, useMe } from "@/components/explorer/kit";
import { WatchButton } from "@/components/explorer/Watch";
import { XChip, XLinkManage, useXLink } from "@/components/explorer/XLink";
import { Copy } from "@/components/ui";
import { useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { AccountDetail } from "@/lib/explorer/types";
import { useRouteParam } from "@/lib/route-param";
import "./profile.css";

/** The label a profile is for (`vlad` from `vlad` or `Vlad.boo`), or null when the URL's isn't a name. */
function labelOf(raw: string): string | null {
  let s = raw;
  try {
    s = decodeURIComponent(raw);
  } catch {
    return null;
  }
  const label = s.toLowerCase().replace(/\.boo$/, "");
  return /^[a-z0-9][a-z0-9-]{1,30}[a-z0-9]$/.test(label) ? label : null;
}

/**
 * A boo's profile, at `/boo/<name>`: the public face of the account that holds a `.boo` name.
 * The name, the handles it links (each a claim, shown with how far it's been checked), its Soul,
 * the hunts it won and the contributions it claimed. Balances, keys and activity stay on the
 * account's page. Everything here is chain text chosen by the account: React renders it as text,
 * and nothing acts on what it says.
 */
export default function ProfilePage() {
  const label = labelOf(useRouteParam("name"));
  const name = label ? `${label}.boo` : null;
  const me = useMe();
  const acct = useNode<AccountDetail>(name ? `/account/${encodeURIComponent(name)}` : null, { every: 10_000 });
  // Only the account that holds this name now is its profile.
  const a = acct.data && name && acct.data.names.includes(name) ? acct.data : null;
  const xLink = useXLink(a);

  if (!label || !name) return <NoSuchBoo title="That isn't a .boo name" body="A name is 3 to 32 letters, digits or hyphens." />;
  if (acct.error && !acct.data) {
    if (/claimed|registered/.test(acct.error.message)) return <NoSuchBoo title={`${name} isn't anyone's`} body="No account holds this name. Check the spelling, or claim it." name={label} />;
    return <NotFound what="profile" error={acct.error} />;
  }
  if (!acct.data) return <Loading>Looking up {name}…</Loading>;
  if (!a) return <NoSuchBoo title={`${name} isn't anyone's`} body="No account holds this name now." name={label} />;

  const addr = a.account.address;
  const yours = me?.address === addr;
  const pc = a.contribution;
  const soul = pc && pc.soul.points > 0 ? pc.soul : null;
  const claims = pc?.claims ?? [];
  const others = a.names.filter((n) => n !== name);
  const share = typeof window === "undefined" ? null : booProfileUrl(window.location.origin, name);
  const empty = !a.github && !xLink && !soul && !a.hunts_won.length && !claims.length;

  return (
    <main className="wide stack-lg" data-testid="profile">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.boos()}>Boos</Link>
        <span>/</span>
        <span>{name}</span>
      </nav>

      <div className="profile-head">
        <span className="profile-sigil" aria-hidden>
          {label[0].toUpperCase()}
        </span>
        <div className="page-head">
          <span className="eyebrow">{yours ? "Your profile, as anyone sees it" : a.program ? "A program" : "A boo on this chain"}</span>
          <h1>
            {name}
            {yours && <span className="you big">you</span>}
          </h1>
          <span className="sub">
            <Term k="address" />: <Hash value={addr} />
          </span>
          <div className="profile-ids" data-testid="profile-ids">
            {a.github && <GithubChip link={a.github} account={addr} />}
            {xLink && <XChip link={xLink} />}
            {a.familiars.length > 0 && <span className="chip agent">{plural(a.familiars.length, "familiar")}</span>}
            {pc?.juror && <span className="chip">juror</span>}
            {pc?.sybil && <span className="pill failed" title={pc.sybil.reason}>flagged as a sybil</span>}
          </div>
          <div className="tools">
            {yours ? (
              <>
                {xLink !== undefined && <XLinkManage linked={!!xLink} />}
                {share && <Copy text={share} label="Copy your profile link" />}
              </>
            ) : (
              <>
                {!a.program && (
                  <Link className="btn small primary" href={`/send?to=${encodeURIComponent(name)}`} data-testid="pay">
                    Pay {name}
                  </Link>
                )}
                <WatchButton who={a.account} />
              </>
            )}
          </div>
        </div>
      </div>

      <div className="figures">
        <Stat v={soul ? soul.display : "0"} k={<Term k="soul" />} />
        <Stat v={a.hunts_won.length} k={<Term k="hunt">hunts won</Term>} />
        <Stat v={claims.length} k={<Term k="claim">contributions claimed</Term>} />
        <Stat v={a.names.length} k={<Term k="name">names</Term>} />
      </div>

      {empty && (
        <section className="card">
          <p className="dim m0">
            {yours ? "Nothing here yet. Link your X account, or win a hunt, and it shows up on this page." : `${name} hasn't linked a handle or won a hunt yet.`}
          </p>
        </section>
      )}

      {a.hunts_won.length > 0 && (
        <Section title={<Term k="hunt">Hunts won</Term>}>
          <ul className="ledger" data-testid="profile-hunts">
            {a.hunts_won.map((h) => (
              <li key={h.id}>
                <Link className="ledger-row two" href={to.hunt(h.id)}>
                  <span>
                    {h.repo}#{h.issue} for PR #{h.pr} {h.self_awarded && <span className="pill pending">self-awarded</span>}
                  </span>
                  <span className="amt">{h.amount_display}</span>
                </Link>
              </li>
            ))}
          </ul>
        </Section>
      )}

      {claims.length > 0 && (
        <Section title={<Term k="pool">Contributions</Term>} aside={<Link className="small" href={to.program("pool")}>The pool →</Link>}>
          <ul className="ledger" data-testid="profile-claims">
            {claims.map((c) => (
              <li key={c.id}>
                <Link className="ledger-row two" href={to.claim(c.id)}>
                  <span>
                    {c.repo}#{c.pr}
                  </span>
                  <StatusPill status={c.status === "scored" ? "finalized" : ["rejected", "spam"].includes(c.status) ? "failed" : "pending"}>{c.status.replace(/_/g, " ")}</StatusPill>
                </Link>
              </li>
            ))}
          </ul>
        </Section>
      )}

      {others.length > 0 && (
        <Section title={<Term k="name">Also known as</Term>}>
          <div className="row-gap">
            {others.map((n) => (
              <Link key={n} className="chip name-chip" href={to.boo(n)}>
                {n}
              </Link>
            ))}
          </div>
        </Section>
      )}

      <p className="profile-more small">
        <Link href={to.account(name)} data-testid="profile-account">
          Balance, keys and activity: the full account →
        </Link>
        <Link href={to.name(name)}>About the name {name} →</Link>
      </p>
    </main>
  );
}

function NoSuchBoo({ title, body, name }: { title: string; body: string; name?: string }) {
  return (
    <main className="wide stack-lg">
      <section className="card">
        <span className="eyebrow">Nobody&apos;s here yet</span>
        <h1>{title}</h1>
        <p className="dim">{body}</p>
        <Link className="btn primary self-start" href={name ? to.name(name) : to.boos()}>
          {name ? `See ${name}.boo` : "See who's here"}
        </Link>
      </section>
    </main>
  );
}
