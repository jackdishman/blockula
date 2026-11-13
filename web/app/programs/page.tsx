"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Fang, Named, plural, Term } from "@/components/explorer/kit";
import { COPY, NOTE } from "@/lib/blackjack-copy";
import { BLACKJACK_TABLE } from "@/lib/config";
import { inertText, shortAddress, typeText } from "@/lib/explorer/inert";
import { compactDrops, parseDrops } from "@/lib/explorer/ledger";
import { NodeError, useNode } from "@/lib/explorer/node";
import { createdProgramAddresses, featuredPrograms, listDeployed, type FeaturedProgram } from "@/lib/explorer/featured";
import { deployedProgram, PROGRAMS, signature } from "@/lib/explorer/programs";
import { to } from "@/lib/explorer/routes";
import type { ChainStatus, ContributionsView, DeployedProgramView, HuntsView, ProgramsList, ValidatorsDetail } from "@/lib/explorer/types";
import { useProgramActivity } from "@/lib/explorer/useProgramActivity";

export default function ProgramsPage() {
  const status = useNode<ChainStatus>("/status", { every: 10_000 });
  const hunts = useNode<HuntsView>("/hunts?status=open", { every: 30_000 });
  const pool = useNode<ContributionsView>("/contributions", { every: 30_000 });
  const vals = useNode<ValidatorsDetail>("/validators", { every: 30_000 });
  const escrow = (hunts.data?.hunts ?? []).reduce((s, h) => s + parseDrops(h.amount), 0n);
  const figure: Record<string, ReactNode> = {
    accounts: <>{status.data?.accounts?.toLocaleString("en-US") ?? "…"} accounts</>,
    names: <>{status.data?.names?.toLocaleString("en-US") ?? "…"} names</>,
    hunts: <>{compactDrops(escrow)} FANG in {plural(hunts.data?.hunts.length ?? 0, "open hunt")}</>,
    pool: <><Fang display={pool.data?.pool.balance_display} /> in the pool</>,
    validators: <>{vals.data ? `${vals.data.active.length} active, epoch ${vals.data.epoch}` : "…"}</>,
  };
  return (
    <main className="wide stack-lg">
      <div className="page-head">
        <span className="eyebrow">
          <Term k="program">Programs</Term>
        </span>
        <h1>What the chain can do</h1>
        <span className="sub">
          Blockula&apos;s built-in modules, each laid out the way third-party programs will be: the state it keeps, the calls it accepts, and actions you can sign right here with a preview of exactly what you&apos;re signing.
        </span>
      </div>
      <div className="programs">
        {PROGRAMS.map((p) => (
          <Link key={p.id} href={to.program(p.id)} className="card program-card">
            <span className="tag">program · {p.id}</span>
            <h2>{p.title}</h2>
            <span className="dim">{p.tagline}</span>
            <span className="figure">{figure[p.id]}</span>
            <ul className="iface">
              {p.actions.slice(0, 3).map((a) => (
                <li key={a.id ?? a.kind}>
                  <span className="fn">{a.kind}</span>
                  {signature(a).slice(a.kind.length)}
                </li>
              ))}
              {p.actions.length > 3 && <li className="who-can">and {p.actions.length - 3} more</li>}
            </ul>
          </Link>
        ))}
        {/* The blackjack table, when this wallet was built with one (lib/config.ts BLACKJACK_TABLE). */}
        {BLACKJACK_TABLE && (
          <Link href={to.blackjack()} className="card program-card" data-testid="blackjack-card">
            <span className="tag">{COPY.nav.tag}</span>
            <h2>{COPY.nav.title}</h2>
            <span className="dim">{COPY.nav.blurb}</span>
            <span className="figure">{NOTE}</span>
          </Link>
        )}
        <div className="card program-card coming">
          <span className="tag">deploy</span>
          <h2>Your program here</h2>
          <span className="dim">
            On a chain that has switched programs on, anyone can upload WebAssembly and create a program from it (`blockula program`, or the SDK). Each one gets a page like these: its record and what it holds, its methods as its deployer named them, and its transactions. In this release a deployed program is read here and called with the CLI or the SDK.
          </span>
        </div>
      </div>
      <Deployed chainId={status.data?.chain_id} genesisHash={status.data?.genesis_hash} />
    </main>
  );
}

/**
 * The addresses of the programs recent blocks created, newest first: from the receipts of their `create_program` transactions.
 * Only for a node that predates `/api/programs` (docs/PROGRAMS-V0.md §8.2): that's a window of recent blocks, not every
 * program, and the page says so. Pins in `FEATURED_PROGRAMS` stay listed for their chain either way.
 */
const CREATES = ["create_program"] as const;

/** How many programs one read of `/api/programs` asks for; "Show more" asks for that many more, up to the node's page cap. */
const LIST_PAGE = 24;
const LIST_MAX = 100;
/** What every program route answers on a chain whose programs were never switched on (node/src/programs/api.rs `dormant`). */
const DORMANT = "no programs exist on this chain";

/** Programs anyone deployed, beside the built-in ones (docs/PROGRAMS-V0.md §8.6): read only, every name in them the deployer's. */
function Deployed({ chainId, genesisHash }: { chainId?: string; genesisHash?: string }) {
  const router = useRouter();
  const [q, setQ] = useState("");
  const [limit, setLimit] = useState(LIST_PAGE);
  const all = useNode<ProgramsList>(`/programs?limit=${limit}`, { every: 30_000 });
  const pins = featuredPrograms(chainId, genesisHash);
  const dormant = !all.data && all.error instanceof NodeError && all.error.status === 404 && all.error.message.startsWith(DORMANT);
  // A node that predates the route: anything but a list or the dormant 404.
  const scan = !all.data && all.error != null && !dormant;
  const open = (e: React.FormEvent) => {
    e.preventDefault();
    const v = q.trim().toLowerCase();
    if (v) router.push(to.deployed(v));
  };
  const rows = Array.isArray(all.data?.programs) ? all.data.programs : [];
  const total = typeof all.data?.total === "number" ? all.data.total : rows.length;
  return (
    <section className="stack-lg" aria-labelledby="deployed-heading">
      <div className="page-head">
        <span className="eyebrow">
          <Term k="program">deployed</Term>
        </span>
        <h2 id="deployed-heading">Programs anyone deployed</h2>
        <span className="sub">
          {scan ? (
            <>WebAssembly on this chain. Featured programs are pinned by this wallet for this chain, so they stay listed; a featured title, and the line under it, are this wallet&apos;s description. The rest are programs created in a window of recent blocks (this node can&apos;t list every program), so an older one that isn&apos;t featured is opened by its address below. What a program holds is on the chain, and every name it uses is its deployer&apos;s claim.</>
          ) : (
            <>Every program on this chain, newest first. Featured programs are pinned by this wallet for this chain; a featured title, and the line under it, are this wallet&apos;s description. What a program holds is on the chain, and every name it uses is its deployer&apos;s claim.</>
          )}
        </span>
      </div>
      <form className="row-gap" onSubmit={open} role="search" aria-label="Open a program or code">
        <label>
          Open a program by its address, or a code by its hash
          <input type="text" className="mono" value={q} onChange={(e) => setQ(e.target.value)} placeholder="boo1… or 64 hex characters" spellCheck={false} autoComplete="off" />
        </label>
        <button className="btn small" type="submit" disabled={!q.trim()}>
          Open
        </button>
      </form>
      {scan ? (
        <RecentCreates pins={pins} />
      ) : (
        <Listing
          pins={pins}
          addresses={rows.map((r) => r.address).filter((a): a is string => typeof a === "string")}
          recentLimit={Infinity}
          loading={!all.data && !dormant}
          loadingText="Reading the programs…"
          emptyText={dormant ? "This chain has programs off." : "No program has been created on this chain yet."}
          more={
            rows.length < total ? (
              <p className="card-note">
                {rows.length < LIST_MAX ? (
                  <button className="btn small" type="button" onClick={() => setLimit((l) => Math.min(l + LIST_PAGE, LIST_MAX))}>
                    Show more ({(total - rows.length).toLocaleString("en-US")} older)
                  </button>
                ) : (
                  <>The newest {rows.length} of {total.toLocaleString("en-US")} are listed; open an older one by its address above.</>
                )}
              </p>
            ) : null
          }
        />
      )}
    </section>
  );
}

/** On a node without `/api/programs`: the creates in a window of recent blocks. */
function RecentCreates({ pins }: { pins: readonly FeaturedProgram[] }) {
  const creates = useProgramActivity(CREATES);
  return (
    <Listing
      pins={pins}
      addresses={createdProgramAddresses(creates.txs)}
      loading={creates.loading}
      loadingText="Reading recent blocks…"
      emptyText="No program was created in the recent blocks (or this chain has programs off)."
    />
  );
}

/** The featured pins, then the other programs (each once), with what to say while loading and when there are none. */
function Listing({ pins, addresses, recentLimit, loading, loadingText, emptyText, more }: { pins: readonly FeaturedProgram[]; addresses: readonly string[]; recentLimit?: number; loading: boolean; loadingText: string; emptyText: string; more?: ReactNode }) {
  const listed = listDeployed(addresses, pins.map((p) => p.address), recentLimit);
  const recent = listed.filter((s) => !s.featured);
  const pinKey = pins.map((p) => p.address).join(" ");
  const [report, setReport] = useState<{ key: string; n: number } | null>(null);
  const onShown = useCallback((key: string, n: number) => setReport({ key, n }), []);
  const featuredShown = report?.key === pinKey ? report.n : null;
  const showEmpty = recent.length === 0 && !loading && (pins.length === 0 || featuredShown === 0);
  return (
    <>
      <FeaturedGroup pins={pins} pinKey={pinKey} onShown={onShown} />
      {recent.length > 0 && (
        <>
          {featuredShown != null && featuredShown > 0 && <span className="eyebrow">All programs</span>}
          <div className="programs" data-testid="deployed-programs">
            {recent.map((s) => (
              <DeployedCard key={s.address} address={s.address} />
            ))}
          </div>
        </>
      )}
      {more}
      {recent.length === 0 && loading && <p className="card-note">{loadingText}</p>}
      {showEmpty && (
        <p className="card-note" data-testid="no-deployed">
          {emptyText}
        </p>
      )}
    </>
  );
}

/** Pins for this chain, above the recent creates. A pin the node answers 404 for is left out, with no error. */
function FeaturedGroup({ pins, pinKey, onShown }: { pins: readonly FeaturedProgram[]; pinKey: string; onShown: (key: string, n: number) => void }) {
  const [missingKey, setMissingKey] = useState(pinKey);
  const [missing, setMissing] = useState<ReadonlySet<string>>(() => new Set());
  if (missingKey !== pinKey) {
    setMissingKey(pinKey);
    setMissing(new Set());
  }
  const missingNow = missingKey === pinKey ? missing : new Set<string>();
  const mark = useCallback((address: string, isMissing: boolean) => {
    setMissing((prev) => {
      if (prev.has(address) === isMissing) return prev;
      const next = new Set(prev);
      if (isMissing) next.add(address);
      else next.delete(address);
      return next;
    });
  }, []);
  const shown = pins.filter((p) => !missingNow.has(p.address)).length;
  useEffect(() => { onShown(pinKey, shown); }, [onShown, pinKey, shown]);
  if (pins.length === 0) return null;
  return (
    <div className="stack" data-testid="featured-programs" hidden={shown === 0}>
      <div className="page-head">
        <span className="eyebrow">Featured</span>
        <span className="sub">Pinned for this chain. The title, and the line under the address, are this wallet&apos;s description.</span>
      </div>
      <div className="programs pins">
        {pins.map((p) => (
          <DeployedCard key={p.address} address={p.address} pin={p} onMissing={mark} />
        ))}
      </div>
    </div>
  );
}

function DeployedCard({ address, pin, onMissing }: { address: string; pin?: FeaturedProgram; onMissing?: (address: string, missing: boolean) => void }) {
  const read = useNode<DeployedProgramView>(`/program/${address}`, { every: 30_000 });
  const missing = !!pin && read.error instanceof NodeError && read.error.status === 404;
  useEffect(() => {
    if (!pin || !onMissing) return;
    if (missing) onMissing(address, true);
    else if (read.data) onMissing(address, false);
  }, [pin, onMissing, missing, read.data, address]);
  if (missing) return null;
  const tag = pin ? "program · featured" : "program · deployed";
  if (!read.data) {
    return (
      <div className="card program-card" data-testid={pin ? "featured-card" : undefined}>
        <span className="tag">{tag}</span>
        <h2 className={pin ? undefined : "mono"}>{pin ? pin.title : shortAddress(address)}</h2>
        <span className="dim">{read.error ? `Couldn't read it: ${inertText(read.error.message, 120)}` : "Reading it…"}</span>
      </div>
    );
  }
  const p = deployedProgram(read.data);
  const methods = Array.isArray(read.data.methods) ? read.data.methods : [];
  return (
    <Link href={to.deployed(address)} className="card program-card" data-testid={pin ? "featured-card" : "deployed-card"}>
      <span className="tag">{tag}</span>
      <h2 className={pin ? undefined : "mono"}>{pin ? pin.title : shortAddress(address)}</h2>
      {pin ? (
        <>
          <span className="mono dim" title={address}>{shortAddress(address)}</span>
          <span className="dim">
            <span className="who-can">The wallet&apos;s description. </span>
            {pin.blurb}
          </span>
        </>
      ) : (
        <span className="dim">{p.tagline}</span>
      )}
      <span className="figure">
        <Fang display={read.data.balance_display} /> held
      </span>
      <ul className="iface">
        {methods.slice(0, 3).map((m, i) => (
          <li key={i}>
            <Named text={m.untrusted_name} />({(Array.isArray(m.args) ? m.args : []).map((a) => `${inertText(a.untrusted_name, 24)}: ${typeText(a.type)}`).join(", ")}) <span className="who-can">{inertText(m.kind, 8)}</span>
          </li>
        ))}
        {methods.length > 3 && <li className="who-can">and {methods.length - 3} more</li>}
      </ul>
    </Link>
  );
}
