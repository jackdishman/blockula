"use client";

import { isAddress, parseFang, Wallet, type Tx } from "@blockula/sdk";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useState } from "react";
import { ErrorNote, Spinner } from "@/components/ui";
import { friendly } from "@/lib/errors";
import { checkTx } from "@/lib/explorer/check";
import { formatDrops } from "@/lib/explorer/ledger";
import { submitTx } from "@/lib/explorer/node";
import type { Preflight } from "@/lib/explorer/preflight";
import { type ActionSpec, type Param, signature, type Values } from "@/lib/explorer/programs";
import { to } from "@/lib/explorer/routes";
import { isKeyId, shortAddr } from "@/lib/format";
import { unlockPasskey } from "@/lib/keys";
import { useWallet } from "@/lib/wallet";
import { Amount, Term } from "./kit";
import { PreflightView } from "./PreflightView";

const utf8 = (s: string) => new TextEncoder().encode(s).length;

/** What's wrong with one field as typed, or null. */
function check(p: Param, raw: string): string | null {
  const v = raw.trim();
  if (!v) return p.optional ? null : "Required.";
  switch (p.type) {
    case "fang":
      try {
        return parseFang(v) > 0n ? null : "More than zero.";
      } catch {
        return "A number like 1 or 0.25 (up to 9 decimals).";
      }
    case "account":
      if (/^boo1/i.test(v)) return isAddress(v.toLowerCase()) ? null : "That address doesn't look right.";
      return /^[a-z0-9-]{3,32}(\.boo)?$/i.test(v) ? null : "A .boo name or a boo1… address.";
    case "name":
      return /^[a-z0-9](?:[a-z0-9-]{1,30}[a-z0-9])(\.boo)?$/i.test(v) ? null : "3 to 32 of a–z, 0–9 and dashes, not starting or ending with a dash.";
    case "int":
      return /^\d+$/.test(v) && Number.isSafeInteger(Number(v)) ? null : "A whole number.";
    case "days":
      return /^\d+$/.test(v) && Number(v) >= 1 && Number(v) <= 365 ? null : "1 to 365 days.";
    case "repo":
      return /^[\w.-]+\/[\w.-]+$/.test(v) ? null : "Like owner/name.";
    case "key_id":
      return isKeyId(v) ? null : "64 characters of 0–9 and a–f.";
    case "text":
      return p.max && utf8(v) > p.max ? `At most ${p.max} bytes.` : null;
  }
}

/**
 * Unlock right here, so acting on a page doesn't mean leaving it. Covers the usual case (this
 * browser's key, its account known); anything else goes through the wallet's own unlock screen.
 */
export function useInlineUnlock() {
  const { client, hint, unlocked } = useWallet();
  const router = useRouter();
  return async () => {
    if (!hint) return router.push("/welcome");
    if (hint.kind === "local" && hint.seed && hint.account) {
      const w = Wallet.fromSeed(hint.seed, client);
      await w.actAs(hint.account);
      return unlocked(w, hint);
    }
    if (hint.kind === "passkey") {
      const { wallet: w, credentialId } = await unlockPasskey(client, hint.credentialId);
      if (w.keyId === hint.keyId && hint.account) {
        await w.actAs(hint.account);
        return unlocked(w, { ...hint, credentialId });
      }
    }
    router.push("/");
  };
}

interface Preview {
  tx: Tx;
  headline: string;
  /** Typed names resolved to the addresses actually signed. */
  resolved: { label: string; typed: string; address: string }[];
  /** What it will do, and whether the chain will take it. */
  pf: Preflight;
  blockMs: number;
}


/**
 * Any program action: typed fields, then a preview of the exact signed transaction (who signs,
 * with which key, the fee, what leaves the account), then submit and land on its receipt.
 */
export function ActionForm({ spec, initial = {}, fixed = [], compact = false }: { spec: ActionSpec; initial?: Record<string, string>; /** Params the page already decides (e.g. the hunt on a hunt's page): set from `initial`, not shown. */ fixed?: string[]; compact?: boolean }) {
  const { wallet, account, client, hint, refresh } = useWallet();
  const router = useRouter();
  const unlock = useInlineUnlock();
  const [raw, setRaw] = useState<Record<string, string>>(() => Object.fromEntries(spec.params.map((p) => [p.key, initial[p.key] ?? p.initial ?? ""])));
  const [touched, setTouched] = useState<Record<string, boolean>>({});
  const [preview, setPreview] = useState<Preview | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const errors = Object.fromEntries(spec.params.map((p) => [p.key, check(p, raw[p.key] ?? "")]));
  const valid = Object.values(errors).every((e) => !e);
  const myKey = account?.keys.find((k) => k.key_id === wallet?.keyId);
  const agentKey = myKey?.role.type === "session";
  const blocked = agentKey && spec.ownerOnly;

  const set = (k: string, v: string) => {
    setRaw((r) => ({ ...r, [k]: v }));
    setPreview(null);
  };

  const toPreview = async () => {
    if (!wallet || !valid) return;
    setBusy("preview");
    setError(null);
    try {
      const values: Values = {};
      const resolved: Preview["resolved"] = [];
      for (const p of spec.params) {
        const v = (raw[p.key] ?? "").trim();
        if (!v && p.optional) continue;
        if (p.type === "fang") values[p.key] = parseFang(v).toString();
        else if (p.type === "int" || p.type === "days") values[p.key] = Number(v);
        else if (p.type === "name") values[p.key] = v.toLowerCase().replace(/\.boo$/, "");
        else if (p.type === "key_id") values[p.key] = v.toLowerCase();
        else if (p.type === "account") {
          // Sign the address, not the name: what you checked is exactly who it's for.
          const address = await client.resolve(v.toLowerCase());
          values[p.key] = address;
          if (address !== v.toLowerCase()) resolved.push({ label: p.label, typed: v.toLowerCase(), address });
        } else values[p.key] = v;
      }
      const st = (await client.status()) as { height: number; block_time_ms?: number };
      const blockMs = st.block_time_ms || 1000;
      const action = spec.build(values, { height: st.height, blockMs });
      const tx = await wallet.build(action, String(raw.memo ?? "").trim());
      const { pf } = await checkTx(tx, wallet.keyId);
      setPreview({ tx, headline: spec.describe(raw), resolved, pf, blockMs });
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(null);
    }
  };

  const send = async () => {
    if (!preview) return;
    setBusy("send");
    setError(null);
    try {
      const id = await submitTx(preview.tx);
      refresh();
      router.push(to.tx(id));
    } catch (e) {
      setError(friendly(e));
      setPreview(null);
    } finally {
      setBusy(null);
    }
  };

  const doUnlock = async () => {
    setBusy("unlock");
    setError(null);
    try {
      await unlock();
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(null);
    }
  };

  const fee = preview ? BigInt(preview.tx.fee) : 0n;
  const refused = !!preview?.pf.refusals.length;
  return (
    <div className="action-body">
      {!compact && <p className="dim" style={{ margin: 0 }}>{spec.blurb}</p>}
      {spec.params.some((p) => !fixed.includes(p.key)) && (
        <div className="grid-2">
          {spec.params.filter((p) => !fixed.includes(p.key)).map((p) => (
            <label key={p.key} className="field" style={p.type === "text" && (p.max ?? 0) > 64 ? { gridColumn: "1 / -1" } : undefined}>
              <span>
                {p.label} {p.optional && <span className="dim">(optional)</span>}
              </span>
              {p.type === "fang" || p.type === "days" ? (
                <div className="suffix-input">
                  <input value={raw[p.key]} onChange={(e) => set(p.key, e.target.value)} onBlur={() => setTouched((t) => ({ ...t, [p.key]: true }))} placeholder={p.placeholder} inputMode={p.type === "fang" ? "decimal" : "numeric"} />
                  <span className="suffix">{p.type === "fang" ? "FANG" : "days"}</span>
                </div>
              ) : (
                <input
                  value={raw[p.key]}
                  onChange={(e) => set(p.key, e.target.value)}
                  onBlur={() => setTouched((t) => ({ ...t, [p.key]: true }))}
                  placeholder={p.placeholder}
                  className={p.type === "key_id" ? "mono" : undefined}
                  inputMode={p.type === "int" ? "numeric" : undefined}
                  autoCapitalize="none"
                  autoCorrect="off"
                  spellCheck={false}
                />
              )}
              {touched[p.key] && errors[p.key] ? <span className="warn small">{errors[p.key]}</span> : p.help ? <span className="dim small">{p.help}</span> : null}
            </label>
          ))}
        </div>
      )}
      <span className="dim small">
        Who can call it: {spec.who}. <span className="mono sig-line">{signature(spec)}</span>
      </span>

      {!wallet || !hint?.account ? (
        hint?.account ? (
          <button className="btn primary" disabled={!!busy} onClick={doUnlock} style={{ alignSelf: "flex-start" }}>
            {busy === "unlock" ? <Spinner>Waiting for your passkey…</Spinner> : "Unlock to sign"}
          </button>
        ) : (
          <span className="small">
            <Link href="/welcome">Create a wallet</Link> to take part. It takes one passkey tap.
          </span>
        )
      ) : blocked ? (
        <span className="warn small">This device signs with an agent key (“{myKey?.label}”), and agent keys can only send transfers. Use an owner device.</span>
      ) : !preview ? (
        <button className="btn primary" disabled={!valid || !!busy} onClick={toPreview} style={{ alignSelf: "flex-start" }}>
          {busy === "preview" ? <Spinner>Preparing…</Spinner> : "Preview"}
        </button>
      ) : (
        <div className="preview" aria-live="polite">
          <span className="eyebrow">Read it twice</span>
          <p className="headline">{preview.headline}</p>
          <dl className="kv">
            <dt>From</dt>
            <dd>
              {account?.names[0] ?? shortAddr(wallet.account)} <span className="dim small">signed by {myKey ? `“${myKey.label}” (${myKey.role.type === "owner" ? "owner" : "agent"})` : "this device"}</span>
            </dd>
            {preview.resolved.map((r) => (
              <div key={r.label} style={{ display: "contents" }}>
                <dt>{r.label}</dt>
                <dd>
                  {r.typed}
                  <div className="mono small break">{r.address}</div>
                </dd>
              </div>
            ))}
            <dt>
              <Term k="fee">Fee</Term>
            </dt>
            <dd>
              {formatDrops(fee)} <span className="dim small">(burned)</span>
            </dd>
            <dt className="total">Total</dt>
            <dd className="total">
              <b>
                <Amount drops={preview.pf.cost} exact />
              </b>{" "}
              <span className="dim small">leaves this account</span>
            </dd>
            <dt>
              <Term k="nonce" />
            </dt>
            <dd>{preview.tx.nonce}</dd>
          </dl>
          <PreflightView p={preview.pf} me={wallet.account} blockMs={preview.blockMs} />
          <details className="raw">
            <summary>The exact transaction you&apos;ll sign</summary>
            <pre>{JSON.stringify({ ...preview.tx, public_key: `${preview.tx.public_key.slice(0, 24)}…`, signature: `${preview.tx.signature.slice(0, 24)}… (${preview.tx.signature.length / 2} bytes)` }, null, 2)}</pre>
          </details>
          <div className="row-gap">
            <button className="btn" disabled={!!busy} onClick={() => setPreview(null)}>
              ← Edit
            </button>
            <button className={`btn grow ${refused ? "" : "primary"}`} disabled={!!busy} onClick={send}>
              {busy === "send" ? <Spinner>Sending…</Spinner> : refused ? "Sign & send anyway" : "Sign & send"}
            </button>
          </div>
        </div>
      )}
      <ErrorNote error={error} />
    </div>
  );
}
