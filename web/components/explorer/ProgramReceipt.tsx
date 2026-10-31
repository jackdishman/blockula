"use client";

import { outcomeWords, type ProgramOutcome as SdkOutcome } from "@blockula/sdk";
import Link from "next/link";
import { shortAddress } from "@/lib/explorer/inert";
import { parseDrops, signedDrops } from "@/lib/explorer/ledger";
import { to } from "@/lib/explorer/routes";
import type { ProgramSection, TxDetail } from "@/lib/explorer/types";
import { Amount, Hash, Named, Section, Term } from "./kit";

/** An address as the chain writes one: the only kind of text that becomes a link here. */
const isAddress = (s: unknown): s is string => typeof s === "string" && /^t?boo1[02-9ac-hj-np-z]{40,}$/.test(s);

/** An account a receipt names: its page when it's an address, else the text, inert. */
function Acct({ address }: { address: unknown }) {
  if (!isAddress(address)) return <Named text={address} max={24} />;
  return (
    <Link href={to.account(address)} className="mono nowrap" title={address}>
      {shortAddress(address)}
    </Link>
  );
}

/** Return data or an event's data: lowercase hex when it's that (the first part of it, and its size), else inert text. */
function Hex({ data }: { data: unknown }) {
  if (typeof data !== "string" || !/^([0-9a-f]{2})*$/.test(data)) return <Named text={data} max={32} />;
  if (data === "") return <span className="dim">nothing</span>;
  return (
    <span className="mono">
      0x{data.slice(0, 128)}
      {data.length > 128 ? "…" : ""} <span className="dim small">({data.length / 2} byte{data.length === 2 ? "" : "s"})</span>
    </span>
  );
}

const big = parseDrops;
const sum = (...xs: (string | undefined)[]) => xs.reduce((s, x) => s + big(x), 0n);

/**
 * A program transaction's receipt (docs/PROGRAMS-V0.md §8.1, §8.4): what the chain did (the outcome, the
 * fee and fuel, the deposits, the balance changes: facts the state holds too), then, apart and marked, what
 * the programs said happened (what each call returned, the events, an abort message: claims the chain
 * checks nothing of). Every name in it is the program's deployer's; none is ever markup.
 */
export function ProgramReceipt({ t }: { t: TxDetail }) {
  const p = t.program;
  if (p === undefined) return null;
  if (p === null) {
    return (
      <Section title="What the program did" aside={<Term k="program">program</Term>}>
        <p className="card-note" data-testid="program-facts">
          {t.status === "pending" ? "It hasn't run yet: its receipt is here once a block has run it." : "This node no longer holds the block it ran in, so its receipt isn't shown."}
        </p>
      </Section>
    );
  }
  return (
    <>
      <Facts p={p} t={t} />
      <Claims p={p} />
    </>
  );
}

function Facts({ p, t }: { p: ProgramSection; t: TxDetail }) {
  const f = p.facts;
  const outside = f.outside_vm;
  const total = sum(f.fee?.bytes, f.fee?.fuel, f.fee?.tip);
  const outsideTotal = sum(outside?.record_deposit, outside?.upload_deposit, outside?.account_creation_fee);
  // The sentence never quotes the program: an abort's message is shown beside, as its own.
  const words = p.outcome.ok ? "It applied." : `${outcomeWords({ ...p.outcome, untrusted_message: undefined, code: p.outcome.code ?? "?", number: p.outcome.number ?? 0 } as SdkOutcome)}.`;
  return (
    <Section title="What the program did" aside={<span className="dim small">facts the chain applied</span>} className="program-facts">
      <dl className="details tight-150" data-testid="program-facts">
        <dt>How it ended</dt>
        <dd data-testid="program-outcome">
          <span className={p.outcome.ok ? "" : "error"}>{words}</span>
          {!p.outcome.ok && <span className="dim small block">Its fee was charged and nothing else happened.</span>}
        </dd>
        <dt>
          <Term k="fee">Fee</Term>
        </dt>
        <dd>
          <Amount drops={total} exact /> <span className="dim small">= {big(f.fee?.bytes).toString()} drops for its bytes + {big(f.fee?.fuel).toString()} for its fuel{big(f.fee?.tip) > 0n ? ` + ${big(f.fee?.tip).toString()} tip to the proposer` : ""}; the rest burned</span>
        </dd>
        <dt>
          <Term k="fuel">Fuel</Term>
        </dt>
        <dd>
          {Number(f.fuel?.reserved ?? 0).toLocaleString("en-US")} reserved ({Number(f.fuel?.limit ?? 0).toLocaleString("en-US")} to run, {Number(f.fuel?.load ?? 0).toLocaleString("en-US")} to load its code), {Number(f.fuel?.used ?? 0).toLocaleString("en-US")} used, at {Number(f.fuel?.rate ?? 0).toLocaleString("en-US")} milli-drops each.{" "}
          <span className="dim small">Paid for as reserved, never refunded.</span>
        </dd>
        {typeof f.created === "string" && (
          <>
            <dt>Created</dt>
            <dd data-testid="program-created">
              <Link href={to.deployed(f.created)} className="mono">
                {f.created}
              </Link>
            </dd>
          </>
        )}
        {f.upload && (
          <>
            <dt>Stored</dt>
            <dd>
              code <Hash value={String(f.upload.wasm_hash)} /> {f.upload.new_code ? "(new)" : "(already stored)"}
              <span className="block">
                interface <Hash value={String(f.upload.interface_hash)} /> {f.upload.new_interface ? "(new)" : "(already stored)"}
              </span>
            </dd>
          </>
        )}
        {outsideTotal > 0n && (
          <>
            <dt>Left the signer outside the program&apos;s own diff</dt>
            <dd data-testid="program-outside-vm">
              <Amount drops={outsideTotal} exact />{" "}
              <span className="dim small">
                = {[
                  big(outside?.record_deposit) > 0n && `${big(outside?.record_deposit)} record deposit`,
                  big(outside?.upload_deposit) > 0n && `${big(outside?.upload_deposit)} upload deposit`,
                  big(outside?.account_creation_fee) > 0n && `${big(outside?.account_creation_fee)} account creation fee (burned)`,
                ].filter(Boolean).join(" + ")} drops; deposits are bonded for good
              </span>
            </dd>
          </>
        )}
        {big(f.burned_by_programs) > 0n && (
          <>
            <dt>Burned by its programs</dt>
            <dd data-testid="program-burned">
              <Amount drops={big(f.burned_by_programs)} exact />{" "}
              <span className="dim small">= the creation fee of an account a program&apos;s own send opened, out of the programs&apos; balance (not the signer&apos;s): gone for good</span>
            </dd>
          </>
        )}
        {(big(f.storage?.bonded) > 0n || big(f.storage?.refunded) > 0n || Number(f.storage?.bytes_delta ?? 0) !== 0) && (
          <>
            <dt>Storage</dt>
            <dd>
              {Number(f.storage?.bytes_delta ?? 0) >= 0 ? "+" : ""}
              {Number(f.storage?.bytes_delta ?? 0).toLocaleString("en-US")} bytes · <Amount drops={big(f.storage?.bonded)} exact /> bonded · <Amount drops={big(f.storage?.refunded)} exact /> refunded
              <span className="dim small block">Everything this transaction bonded, whoever paid it.</span>
            </dd>
          </>
        )}
        {(f.balance_changes ?? []).length > 0 && (
          <>
            <dt>Moved by its programs</dt>
            <dd data-testid="program-balance-changes">
              {(f.balance_changes ?? []).map((c) => (
                <span key={String(c.account)} className="block">
                  <Acct address={c.account} />: <Amount drops={signedDrops(c.change)} signed exact />
                </span>
              ))}
              <span className="dim small block">The programs&apos; own diff: not the fee, and not what left outside it.</span>
            </dd>
          </>
        )}
      </dl>
      {t.status !== "pending" && <p className="card-note">The signer&apos;s balance changed by exactly −(fee + what left outside the diff) + what its programs moved for it.</p>}
    </Section>
  );
}

function Claims({ p }: { p: ProgramSection }) {
  const returns = p.claims?.return_data ?? [];
  const events = p.claims?.events ?? [];
  const omitted = p.claims?.omitted;
  const message = p.outcome.untrusted_message;
  if (!returns.length && !events.length && !omitted && !message) return null;
  return (
    <Section title="What the program reports" aside={<Term k="program_claim">claims</Term>} className="program-claims">
      <p className="card-note" data-testid="program-claims-note">
        The chain applied the facts above and checks none of this. It&apos;s what the programs said, under names their deployer chose.
      </p>
      <ul className="claims" data-testid="program-claims">
        {message ? (
          <li>
            aborted with the message <Named text={message} max={256} />
          </li>
        ) : null}
        {returns.map((r, i) => (
          <li key={`r${i}`}>
            <Acct address={r.program} /> · <Named text={r.untrusted_method} /> returned <Hex data={r.data} />
            {big(r.value) > 0n && (
              <span className="dim small">
                {" "}
                (sent <Amount drops={big(r.value)} exact />)
              </span>
            )}
          </li>
        ))}
        {events.map((e, i) => (
          <li key={`e${i}`}>
            <Acct address={e.program} /> · emitted <Named text={e.untrusted_name} /> with <Hex data={e.data} />
          </li>
        ))}
        {omitted && (
          <li className="dim">
            {Number(omitted.events).toLocaleString("en-US")} events and {Number(omitted.return_bytes).toLocaleString("en-US")} bytes of return data were left out of this receipt (the block&apos;s claims budget): its facts are all here.
          </li>
        )}
      </ul>
    </Section>
  );
}
