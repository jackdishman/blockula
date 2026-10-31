"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import { Amount, Hash, Loading, Named, NotFound, Section, Stat, Term, useMe, Who } from "@/components/explorer/kit";
import { Ledger } from "@/components/explorer/Ledger";
import { Copy } from "@/components/ui";
import { nodeExplorer, RPC } from "@/lib/config";
import { typeText, shortAddress, inertText } from "@/lib/explorer/inert";
import { parseDrops } from "@/lib/explorer/ledger";
import { useNode } from "@/lib/explorer/node";
import { upgradeSentence } from "@/lib/explorer/programs";
import { to } from "@/lib/explorer/routes";
import type { AccountDetail, BlockView, ChainStatus, DeployedProgramView, ProgramMethodView, StoredCodeView } from "@/lib/explorer/types";
import { plural } from "@/lib/format";

/** An address as the chain writes one: the only thing a program's page is named by. */
const isAddress = (s: string) => /^t?boo1[02-9ac-hj-np-z]{40,}$/.test(s);
const isHash = (s: string) => /^[0-9a-f]{64}$/.test(s);
const hex = (s: string) => Array.from(new TextEncoder().encode(s), (b) => b.toString(16).padStart(2, "0")).join("");

/** A program anyone deployed (or the code one is made of, by its hash): read only. Everything it names is its deployer's claim. */
export default function ProgramPage() {
  const { address } = useParams<{ address: string }>();
  const id = decodeURIComponent(address);
  if (isHash(id)) return <CodePage hash={id} />;
  if (!isAddress(id)) {
    return (
      <main className="wide stack-lg">
        <section className="card">
          <h1>No such program</h1>
          <p className="dim">A program is named by its address (boo1… or tboo1…), never by a .boo name, which moves with its owner. A stored code is named by its 64-character hash.</p>
          <Link href={to.programs()}>All programs</Link>
        </section>
      </main>
    );
  }
  return <DeployedPage address={id} />;
}

function DeployedPage({ address }: { address: string }) {
  const me = useMe();
  const program = useNode<DeployedProgramView>(`/program/${address}`, { every: 10_000 });
  const account = useNode<AccountDetail>(`/account/${address}`, { every: 5_000 });
  const status = useNode<ChainStatus>("/status", { every: 10_000 });
  const p = program.data;
  // The block that made it holds the create's receipt: the transaction that did.
  const block = useNode<BlockView>(p ? `/block/${p.created_at}` : null);
  if (program.error && !p) return <NotFound what="program" error={program.error} />;
  if (!p) return <Loading>Finding the program…</Loading>;
  const creation = block.data?.txs.find((t) => t.kind === "create_program" && t.program?.facts?.created === address);
  const head = status.data?.height;
  const methods = Array.isArray(p.methods) ? p.methods : null;
  return (
    <main className="wide stack-lg">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.programs()}>Programs</Link>
        <span>/</span>
        <span>Deployed</span>
        <span>/</span>
        <span className="mono">{shortAddress(address)}</span>
      </nav>
      <div className="page-head">
        <span className="eyebrow">
          <Term k="program">program · deployed</Term>
        </span>
        <h1 className="mono wrap-any">{address}</h1>
        <span className="sub">WebAssembly anyone can deploy, run by the chain under a fuel meter. What it holds and what it stores are facts the state proves; every name it uses is its deployer&apos;s claim.</span>
      </div>
      <div className="figures">
        <Stat v={<Amount drops={parseDrops(p.balance)} unit={false} />} k="FANG it holds" />
        <Stat v={p.storage.items.toLocaleString("en-US")} k="storage entries" />
        <Stat v={p.version} k={<Term k="digest">version</Term>} />
        <Stat v={p.created_at.toLocaleString("en-US")} k="created at block" />
      </div>
      <div className="cols">
        <div className="stack-lg">
          <Section title="The record" aside={<span className="dim small">what the chain holds</span>}>
            <dl className="details tight-150" data-testid="program-record">
              <dt>Address</dt>
              <dd>
                <Hash value={address} full />
              </dd>
              <dt>Created by</dt>
              <dd>
                <Who who={p.creator} me={me?.address} />
                {creation && (
                  <span className="dim small block">
                    in <Link href={to.tx(creation.id)}>this transaction</Link>, block <Link href={to.block(p.created_at)}>#{p.created_at.toLocaleString("en-US")}</Link>
                  </span>
                )}
              </dd>
              <dt>Code</dt>
              <dd>
                <Hash value={String(p.code.wasm_hash)} href={to.deployed(String(p.code.wasm_hash))} />
                <span className="dim small block">{p.code.size == null ? "size unknown" : `${p.code.size.toLocaleString("en-US")} bytes`}</span>
              </dd>
              <dt>Interface</dt>
              <dd>
                <Hash value={String(p.code.interface_hash)} />
              </dd>
              <dt>
                <Term k="digest">Version digest</Term>
              </dt>
              <dd data-testid="version-digest">
                <Hash value={String(p.version_digest)} full />
                <span className="dim small block">What a call binds: it runs only while the program still has exactly this code, interface and version ({p.version}).</span>
              </dd>
              <dt>Upgrades</dt>
              <dd data-testid="upgradability">
                {upgradeSentence(p)}
                {p.authority && (
                  <span className="block">
                    authority <Who who={p.authority} me={me?.address} />
                  </span>
                )}
              </dd>
              <dt>Takes plain transfers</dt>
              <dd>{p.accepts_transfers ? "yes" : "no"}</dd>
              <dt>Storage</dt>
              <dd>
                {plural(p.storage.items, "entry", "entries")}, {p.storage.bytes.toLocaleString("en-US")} bytes, <Amount drops={parseDrops(p.storage.deposit)} exact /> bonded
                {parseDrops(p.storage.sponsored) > 0n && (
                  <span className="dim small block">
                    <Amount drops={parseDrops(p.storage.sponsored)} exact /> of it paid by the program itself
                  </span>
                )}
              </dd>
              <dt>Record deposit</dt>
              <dd>
                <Amount drops={parseDrops(p.record_deposit)} exact /> <span className="dim small">bonded for good by</span> <Who who={p.record_payer} me={me?.address} />
              </dd>
              <dt>Proof</dt>
              <dd>
                <a href={`${RPC}/api/proof/66/${hex(address)}`} target="_blank" rel="noreferrer">
                  its record with a Merkle proof
                </a>{" "}
                <span className="dim small">against the state root (namespace 66)</span>
              </dd>
            </dl>
            <p className="card-note">
              Check it against another source: <a href={nodeExplorer.program(address)} target="_blank" rel="noreferrer">the node&apos;s own explorer ↗</a>
            </p>
          </Section>
          <Section title="Methods" aside={<span className="dim small">named by the deployer</span>}>
            {methods === null ? (
              <p className="card-note">This node couldn&apos;t read the program&apos;s interface.</p>
            ) : (
              <ul className="method-list" data-testid="methods">
                {methods.map((m, i) => (
                  <li key={i}>
                    <MethodLine m={m} />
                  </li>
                ))}
              </ul>
            )}
            <p className="card-note">These names and types are the deployer&apos;s claim, shown as text. The chain enforces each method&apos;s kind (a transaction, a read, or the initializer) and whether it takes FANG, nothing else about it.</p>
          </Section>
        </div>
        <div className="stack-lg">
          <Section title="Call it" aside={<span className="dim small">read only here</span>}>
            <p className="card-note">This page signs nothing. Calls to a deployed program are signed with the CLI or the SDK, which show the digest and the fee, and refuse a reservation no block could carry.</p>
            <pre className="raw">
              <code>
                {`blockula program view ${address} <method>\nblockula program call ${address} <method> --storage-limit 1`}
              </code>
            </pre>
            <div className="row-gap">
              <Copy text={`blockula program view ${address} <method>`} label="Copy the view command" />
            </div>
          </Section>
        </div>
      </div>
      <Section title="Recent transactions" aside={<span className="dim small">calls naming this program, and what it did</span>}>
        {account.error && !account.data ? (
          <p className="error" role="alert">Couldn&apos;t load its transactions: {account.error.message}</p>
        ) : (
          <Ledger txs={account.data?.recent_txs ?? []} side={address} me={me} head={head} blockMs={status.data?.block_time_ms} showFee empty={account.loading ? "Reading its history…" : "No transaction has named it yet."} />
        )}
      </Section>
    </main>
  );
}

/** One method as its interface says it: its name and its arguments' (the deployer's) in boxes of their own, the chain's kind beside. */
function MethodLine({ m }: { m: ProgramMethodView }) {
  const kind = m.kind === "call" ? "call: a transaction runs it" : m.kind === "view" ? "view: a read, nothing signed" : m.kind === "init" ? "init: runs once, when the program is created" : "a kind this explorer doesn't know";
  return (
    <>
      <span className="sig">
        <Named text={m.untrusted_name} />(
        {(Array.isArray(m.args) ? m.args : []).map((a, i) => (
          <span key={i}>
            {i > 0 && ", "}
            <Named text={a.untrusted_name} />: <span className="ty">{typeText(a.type)}</span>
          </span>
        ))}
        ){m.returns ? <> → <span className="ty">{typeText(m.returns)}</span></> : null}
      </span>
      <span className="who-can block">
        {kind}
        {m.payable ? ", payable" : ""}
      </span>
    </>
  );
}

/** A stored code, by its hash: what's stored and who stored it, never its bytes. */
function CodePage({ hash }: { hash: string }) {
  const me = useMe();
  const code = useNode<StoredCodeView>(`/code/${hash}`, { every: 15_000 });
  if (code.error && !code.data) return <NotFound what="program code" error={code.error} />;
  const c = code.data;
  if (!c) return <Loading>Finding the code…</Loading>;
  return (
    <main className="wide stack-lg">
      <nav className="crumbs" aria-label="Breadcrumbs">
        <Link href={to.programs()}>Programs</Link>
        <span>/</span>
        <span>Code</span>
        <span>/</span>
        <span className="mono">{hash.slice(0, 12)}…</span>
      </nav>
      <div className="page-head">
        <span className="eyebrow">
          <Term k="program">program code</Term>
        </span>
        <h1 className="mono wrap-any">{hash}</h1>
        <span className="sub">WebAssembly stored by its hash. Programs are created from it; it is usable from the block after the one that stored it.</span>
      </div>
      <Section title="What's stored">
        <dl className="details tight-150" data-testid="code-record">
          <dt>Hash</dt>
          <dd>
            <Hash value={hash} full />
          </dd>
          <dt>Size</dt>
          <dd>{c.size.toLocaleString("en-US")} bytes</dd>
          <dt>Stored by</dt>
          <dd>{c.uploader ? <Who who={c.uploader} me={me?.address} /> : "—"}</dd>
          <dt>Stored in block</dt>
          <dd>{c.uploaded_at == null ? "—" : <Link href={to.block(c.uploaded_at)}>#{c.uploaded_at.toLocaleString("en-US")}</Link>}</dd>
          <dt>Deposit</dt>
          <dd>{c.deposit == null ? "—" : <Amount drops={parseDrops(c.deposit)} exact />}</dd>
          <dt>Programs using it</dt>
          <dd>{c.refs ?? "—"}</dd>
          <dt>Methods it exports</dt>
          <dd data-testid="exports">
            {(Array.isArray(c.untrusted_exports) ? c.untrusted_exports : []).map((e, i) => (
              <span key={i}>
                {i > 0 && " "}
                <Named text={e} />
              </span>
            ))}
            <span className="dim small block">named by the deployer</span>
          </dd>
        </dl>
        <p className="card-note">{inertText("The code's bytes aren't shown here: read them with the SDK (client.code(hash, { wasm: true }), which checks they hash to this).", 200)}</p>
      </Section>
    </main>
  );
}
