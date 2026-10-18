"use client";

import { describeAction, parseFang, type Resolution, type Tx } from "@blockula/sdk";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useState } from "react";
import { ErrorNote, Spinner } from "@/components/ui";
import { BLOCKS_PER_DAY, MAINNET_CHAIN_ID, MAINNET_PREFIX } from "@/lib/config";
import { friendly } from "@/lib/errors";
import { checkTx } from "@/lib/explorer/check";
import { checkField } from "@/lib/explorer/fields";
import { formatDrops } from "@/lib/explorer/ledger";
import { submitTx } from "@/lib/explorer/node";
import type { Preflight } from "@/lib/explorer/preflight";
import { type ActionSpec, type Param, signature, type Values } from "@/lib/explorer/programs";
import { to } from "@/lib/explorer/routes";
import { shortAddr } from "@/lib/format";
import { openLocalKey, unlockPasskey } from "@/lib/keys";
import { useWallet } from "@/lib/wallet";
import { Amount, Term } from "./kit";
import { PreflightView } from "./PreflightView";

/**
 * Unlock right here, so acting on a page doesn't mean leaving it. Covers the usual case (this
 * browser's key, its account known), under the same rules as the wallet's own unlock screen
 * (a browser-only key never opens on mainnet); anything else goes through that screen.
 */
export function useInlineUnlock() {
  const { client, hint, unlocked } = useWallet();
  const router = useRouter();
  return async () => {
    if (!hint) return router.push("/welcome");
    if (hint.kind === "local" && hint.account) {
      const w = openLocalKey(client, hint);
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

/** A name in the action, and how it gets signed. */
interface Shown {
  label: string;
  /** The name as typed (`alice.boo`). */
  name: string;
  /** What's in the transaction: the name itself, or an address. */
  signed: string;
  /** The address the node (or the witnesses) say it is now. */
  address: string;
  proven: boolean;
  /** Signed as the name: the chain resolves it when the transaction runs. */
  byChain: boolean;
  witnesses: number;
}

interface Preview {
  tx: Tx;
  headline: string;
  /** The signed action, rendered by this client from the action itself (audit gap-7-6). */
  does: string;
  names: Shown[];
  /** For actions that carry a height: the block it's counted from (the one valid_until is). */
  from: number;
  /** What it will do, and whether the chain will take it. */
  pf: Preflight;
  blockMs: number;
}

/**
 * Any program action: typed fields, then a preview of the exact signed transaction (who signs,
 * with which key, the fee, what leaves the account), then submit and land on its receipt.
 *
 * Names are signed by this branch's rules (audit gap-7-1): in fields the chain resolves itself,
 * `Client.recipient`'s answer (the proven owner with witnesses, else the name, so a lying node
 * can't redirect it); in fields that need an address, `Client.resolution`'s, handed to
 * `Wallet.build` so mainnet refuses an unproven one against the very status it signs for. An
 * action carrying a height (a hunt's expiry) is built from that same status read (gap-7-3).
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

  const prefix = client.pin.prefix ?? MAINNET_PREFIX;
  const errors = Object.fromEntries(spec.params.map((p) => [p.key, checkField(p, raw[p.key] ?? "", prefix)]));
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
      const names: Shown[] = [];
      const resolutions: Resolution[] = [];
      for (const p of spec.params) {
        const v = (raw[p.key] ?? "").trim();
        if (!v && p.optional) continue;
        if (p.type === "fang") values[p.key] = parseFang(v).toString();
        else if (p.type === "int" || p.type === "days" || p.type === "years" || p.type === "bps") values[p.key] = Number(v);
        else if (p.type === "name") values[p.key] = v.toLowerCase().replace(/\.boo$/, "");
        else if (p.type === "key_id") values[p.key] = v.toLowerCase();
        else if (p.type === "account") {
          // The chain resolves this field itself: sign the name unless it's proven (gap-7-1).
          const r = await client.recipient(v.toLowerCase());
          values[p.key] = r.signed;
          if (r.name) names.push({ label: p.label, name: `${r.name}.boo`, signed: r.signed, address: r.address, proven: r.proven, byChain: r.byChain, witnesses: client.witnesses.length });
        } else if (p.type === "validator") {
          // This field needs an address: a name is resolved, and `build` refuses it on mainnet unless proven.
          const r = await client.resolution(v.toLowerCase());
          values[p.key] = r.address;
          resolutions.push(r);
          if (r.name) names.push({ label: p.label, name: `${r.name}.boo`, signed: r.address, address: r.address, proven: r.proven, byChain: false, witnesses: client.witnesses.length });
        } else values[p.key] = v;
      }
      let from = 0;
      const tx = await wallet.build(
        (height) => {
          from = height;
          return spec.build(values, { height, account: wallet.account });
        },
        String(raw.memo ?? "").trim(),
        { resolutions },
      );
      const { pf, blockMs } = await checkTx(tx, wallet.keyId, spec.id);
      setPreview({ tx, headline: pf.headline ?? spec.describe(raw), does: describeAction(tx.action), names, from, pf, blockMs });
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
  const expiresAt = preview && preview.tx.action.type === "create_hunt" ? preview.tx.action.expires_at : null;
  const mainnet = client.pin.chainId === MAINNET_CHAIN_ID;
  return (
    <div className="action-body">
      {!compact && <p className="dim m0">{spec.blurb}</p>}
      {spec.params.some((p) => !fixed.includes(p.key)) && (
        <div className="grid-2">
          {spec.params.filter((p) => !fixed.includes(p.key)).map((p) => (
            <label key={p.key} className={`field ${p.type === "text" && (p.max ?? 0) > 64 ? "span-all" : ""}`}>
              <span>
                {p.label} {p.optional && <span className="dim">(optional)</span>}
              </span>
              {p.type === "fang" || p.type === "days" || p.type === "years" ? (
                <div className="suffix-input">
                  <input value={raw[p.key]} onChange={(e) => set(p.key, e.target.value)} onBlur={() => setTouched((t) => ({ ...t, [p.key]: true }))} placeholder={p.placeholder} inputMode={p.type === "fang" ? "decimal" : "numeric"} aria-label={p.label} />
                  <span className="suffix">{p.type === "fang" ? "FANG" : p.type}</span>
                </div>
              ) : (
                <input
                  value={raw[p.key]}
                  onChange={(e) => set(p.key, e.target.value)}
                  onBlur={() => setTouched((t) => ({ ...t, [p.key]: true }))}
                  placeholder={p.type === "account" || p.type === "validator" ? (p.placeholder ?? "").replace(/an address|its address/, `a ${prefix}1… address`) : p.placeholder}
                  className={p.type === "key_id" ? "mono" : undefined}
                  inputMode={p.type === "int" || p.type === "bps" ? "numeric" : undefined}
                  autoCapitalize="none"
                  autoCorrect="off"
                  spellCheck={false}
                  aria-label={p.label}
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
          <button className="btn primary self-start" disabled={!!busy} onClick={doUnlock}>
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
        <button className="btn primary self-start" disabled={!valid || !!busy} onClick={toPreview}>
          {busy === "preview" ? <Spinner>Preparing…</Spinner> : "Preview"}
        </button>
      ) : (
        <div className="preview" aria-live="polite" data-testid="action-preview">
          <span className="eyebrow">Read it twice</span>
          <p className="headline">{preview.headline}</p>
          <dl className="kv">
            <dt>From</dt>
            <dd>
              {account?.names[0] ?? shortAddr(wallet.account)} <span className="dim small">signed by {myKey ? `“${myKey.label}” (${myKey.role.type === "owner" ? "owner" : "agent"})` : "this device"}</span>
            </dd>
            {preview.names.map((r) => (
              <div key={r.label} className="contents">
                <dt>{r.label}</dt>
                <dd>
                  {r.name}
                  <div className="mono small break" data-testid="signed-name">{r.signed}</div>
                  {r.byChain ? (
                    <div className="small" data-testid="by-chain">
                      Signed as the name: the chain acts on whoever owns {r.name} when this transaction runs, so no node can point it elsewhere. Your node says that&apos;s <span className="mono break">{r.address}</span> (not proven).
                    </div>
                  ) : r.proven ? (
                    <div className="dim small">proven by {r.witnesses} other node{r.witnesses === 1 ? "" : "s"}</div>
                  ) : (
                    <div className="warn small">Looked up on your node only, not proven{mainnet ? "" : ". Check this address before you sign"}.</div>
                  )}
                </dd>
              </div>
            ))}
            <dt>Signs</dt>
            <dd data-testid="signs">{preview.does}</dd>
            {expiresAt != null && (
              <>
                <dt>Expires</dt>
                <dd data-testid="expires">
                  block #{expiresAt.toLocaleString("en-US")}{" "}
                  <span className="dim small">
                    {Math.round((expiresAt - preview.from) / BLOCKS_PER_DAY)} days of blocks after #{preview.from.toLocaleString("en-US")}, the height this transaction is signed against. Until then only an award, or a governance refund, releases the escrow.
                  </span>
                </dd>
              </>
            )}
            <dt>
              <Term k="fee">Fee</Term>
            </dt>
            <dd>
              {formatDrops(fee)} <span className="dim small">at most: the base fee of the block that includes it (burned){BigInt(preview.tx.tip ?? 0) > 0n ? ` plus a ${formatDrops(BigInt(preview.tx.tip ?? 0))} tip to its proposer` : ""}</span>
            </dd>
            <dt className="total">Total</dt>
            <dd className="total">
              <b>
                <Amount drops={preview.pf.cost} exact />
              </b>{" "}
              <span className="dim small">at most leaves this account</span>
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
