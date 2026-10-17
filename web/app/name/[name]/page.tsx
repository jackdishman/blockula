"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import { ActionForm } from "@/components/explorer/ActionForm";
import { Loading, NotFound, Section, Term, useMe, Who } from "@/components/explorer/kit";
import { blocksToHuman } from "@/lib/format";
import { useNode } from "@/lib/explorer/node";
import { actionId, program } from "@/lib/explorer/programs";
import { to } from "@/lib/explorer/routes";
import type { ChainStatus, NameDetail } from "@/lib/explorer/types";

const names = program("names");
const spec = (id: string) => names?.actions.find((a) => actionId(a) === id);

/**
 * A name as protocol v1 keeps it (DEC-11): registered a year at a time; once expired it points to
 * nobody, and only its owner can renew it until its grace period ends; offered names stay their
 * owner's until the recipient accepts; reserved names can't be registered.
 */
export default function NamePage() {
  const { name } = useParams<{ name: string }>();
  const n = decodeURIComponent(name).toLowerCase().replace(/\.boo$/, "");
  const me = useMe();
  const look = useNode<NameDetail>(`/name/${encodeURIComponent(n)}`, { every: 5000 });
  const status = useNode<ChainStatus>("/status", { every: 10_000 });
  if (look.error && !look.data) return <NotFound what="name" error={look.error} />;
  const r = look.data;
  if (!r) return <Loading>Looking up {n}.boo…</Loading>;
  const head = status.data?.height;
  const blockMs = status.data?.block_time_ms;
  const st = r.status ?? (r.available ? "available" : "active");
  const mine = !!me && !!r.owner && me.address === r.owner.address;
  const offeredToMe = !!me && !!r.offered_to && me.address === r.offered_to.address;
  const lapsed = st === "available" && !!r.owner;
  const eyebrow = st === "grace" ? "Expired name" : st === "reserved" ? "Reserved name" : st === "available" ? "Unclaimed name" : "Name";
  const form = (id: string, title: string, open = false) => {
    const a = spec(id);
    if (!a) return null;
    return (
      <details className="action" open={open}>
        <summary>
          <span className="title">{title}</span>
          <span className="sig">{a.kind}</span>
        </summary>
        <ActionForm spec={a} initial={{ name: n }} fixed={["name"]} compact />
      </details>
    );
  };
  return (
    <main className="wide stack-lg">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.programs()}>Programs</Link>
        <span>/</span>
        <Link href={to.program("names")}>Names</Link>
        <span>/</span>
        <span>{r.name}</span>
      </nav>
      <div className="page-head">
        <span className="eyebrow">
          <Term k="name">{eyebrow}</Term>
        </span>
        <h1 className="figure-title">{r.name}</h1>
        {st === "active" && r.owner && (
          <span className="sub" data-testid="name-status">
            Points to <Who who={r.owner} me={me?.address} />
            {r.registered_at != null && (
              <>
                {" "}
                since <Link href={to.block(r.registered_at)}>block #{r.registered_at}</Link>
              </>
            )}
            {r.expires_at != null && (
              <>
                {" "}
                · <Term k="expiry">expires</Term> at block #{r.expires_at.toLocaleString("en-US")}
                {head != null && r.expires_at > head ? ` (in about ${blocksToHuman(r.expires_at - head, blockMs)})` : ""}
              </>
            )}
          </span>
        )}
        {st === "grace" && (
          <span className="sub warn" data-testid="name-status">
            Expired at block #{r.expires_at?.toLocaleString("en-US")}: it points to nobody now, so payments to it fail. Its owner {r.owner && <Who who={r.owner} me={me?.address} />} can renew it until block #{r.grace_until?.toLocaleString("en-US")}
            {head != null && r.grace_until != null && r.grace_until > head ? ` (about ${blocksToHuman(r.grace_until - head, blockMs)} from now)` : ""}; after that anyone can register it.
          </span>
        )}
        {st === "reserved" && (
          <span className="sub" data-testid="name-status">
            Reserved at genesis: nobody can register it.
          </span>
        )}
        {lapsed && (
          <span className="sub" data-testid="name-status">
            Free again: its last registration lapsed (its grace period ended at block #{r.grace_until?.toLocaleString("en-US")}).
          </span>
        )}
        {r.offered_to && st !== "available" && (
          <span className="sub" data-testid="name-offer">
            <Term k="offer">Offered</Term> to <Who who={r.offered_to} me={me?.address} />: it stays {r.owner ? <Who who={r.owner} me={me?.address} link={false} /> : "its owner"}&apos;s until they accept it.
          </span>
        )}
      </div>

      {st === "available" ? (
        <div className="cols">
          <Section title={`Yours for ${r.price_display ?? "…"} a year`}>
            {spec("register_name") && <ActionForm spec={spec("register_name")!} initial={{ name: n }} compact />}
          </Section>
          <Section title="About names">
            <p>A .boo name points to an account, like a domain name points to a server, so people can pay {r.name} instead of an address.</p>
            <p className="dim">
              Names are registered for a year at a time{r.renewal_price_display ? ` (renewing costs ${r.renewal_price_display} a year)` : ""}. Shorter names cost more because they&apos;re scarcer. The price goes to the treasury.
            </p>
          </Section>
        </div>
      ) : (
        <>
          <div className="row-gap">
            {/* An expired name resolves to nobody: open its owner by address, and don't offer to pay it. */}
            {st === "active" && r.owner && (
              <Link className="btn primary" href={to.account(r.name)}>
                Open {r.name}
              </Link>
            )}
            {st === "grace" && r.owner && (
              <Link className="btn" href={to.account(r.owner.address)}>
                Open its owner&apos;s account
              </Link>
            )}
            {st === "active" && me && r.owner && !mine && (
              <Link className="btn" href={`/send?to=${encodeURIComponent(r.name)}`}>
                Send to {r.name}
              </Link>
            )}
          </div>
          {(offeredToMe || (mine && st !== "reserved")) && (
            <Section title={offeredToMe ? "It's offered to you" : "You own this name"}>
              {offeredToMe && form("accept_name", `Accept ${r.name}`, true)}
              {mine && form("renew_name", st === "grace" ? `Renew ${r.name} before it's lost` : `Renew ${r.name}`, st === "grace")}
              {mine && st === "active" && form("transfer_name", r.offered_to ? "Offer it to someone else" : "Offer it to another account")}
              {mine && r.offered_to && form("withdraw_name_offer", "Withdraw the offer")}
              {mine && form("release_name", "Release it")}
              {mine && r.renewal_price_display && <p className="card-note">Renewing costs {r.renewal_price_display} a year, paid to the treasury.</p>}
            </Section>
          )}
        </>
      )}
    </main>
  );
}
