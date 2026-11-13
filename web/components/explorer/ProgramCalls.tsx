"use client";

// Calling a deployed program from its page (docs/EXPLORER.md, docs/PROGRAMS.md 11.3): its views run on the
// node, signed out too; its calls are reviewed (the program, the method, the encoded input, the value, the
// storage limit, the full version digest the call binds and the most it can cost), then signed with an owner
// key right after a fresh passkey check. Forms are made from the record the SDK checked (`client.program`),
// never from a node's unchecked answer; arguments are encoded by the SDK's ABI codec; what a program returns
// is its claim, shown as text. A transaction that was sent is never offered again: one whose outcome isn't
// known is checked by its id.
import { ApiError, checkReceipt, describeAction, loadFuel, outcomeWords, ProgramAnswerError, TxFailedError, TxPendingError, txId, type ProgramInfo, type ProgramReceipt, type ProgramType, type ProgramViewResult, type Tx } from "@blockula/sdk";
import Link from "next/link";
import { useEffect, useState } from "react";
import { useMultisig } from "@/components/shared";
import { ErrorNote, Spinner } from "@/components/ui";
import { MAINNET_PREFIX } from "@/lib/config";
import { inertText, typeText } from "@/lib/explorer/inert";
import { formatDrops } from "@/lib/explorer/ledger";
import { NodeError, submitTx } from "@/lib/explorer/node";
import { checkCall, encodeDraft, fieldProblem, initialDraft, leafOf, readInput, readReturn, sameCall, shownValue, specAction, specsFromProgram, type Draft, type ProgramCallSpec, type ProgramParam, type ProgramViewSpec, type Shown } from "@/lib/explorer/program-calls";
import type { Param } from "@/lib/explorer/programs";
import { to } from "@/lib/explorer/routes";
import { chainUnit, shortAddr } from "@/lib/format";
import { PasskeyCheckError } from "@/lib/keys";
import { previewer, unsignedOf, useSpendCheck } from "@/lib/spend";
import { useWallet } from "@/lib/wallet";
import { useInlineUnlock } from "./ActionForm";
import { Amount, Named, Term } from "./kit";

/** An error's words, as text: the SDK's or the node's (the chain's own), stripped of what hides text, never rewritten into advice to try again. */
const words = (e: unknown) => inertText(e instanceof Error ? e.message : String(e), 600);
const utf8 = (s: string) => new TextEncoder().encode(s).length;

/** Who's here, for a call: only an owner key of a personal account signs one. */
type Caller = { kind: "signed-out"; hint: boolean } | { kind: "reading" } | { kind: "agent"; label: string } | { kind: "shared"; threshold: number; owners: number } | { kind: "owner"; label: string };

/**
 * A deployed program's views and calls. `shownDigest` is the version digest of the record the page shows:
 * a call binds it (`Wallet.call`'s `versionDigest`), so it's refused, uncharged, if the program isn't
 * that one any more.
 */
export function ProgramCalls({ address, shownDigest }: { address: string; shownDigest: string }) {
  const { client, wallet, account, hint } = useWallet();
  const [read, setRead] = useState<{ info: ProgramInfo } | { error: unknown } | null>(null);
  useEffect(() => {
    let live = true;
    client.program(address).then(
      (info) => live && setRead({ info }),
      (error) => live && setRead({ error }),
    );
    return () => {
      live = false;
    };
  }, [client, address, shownDigest]);
  const signedIn = !!wallet && !!hint?.account;
  const { ms } = useMultisig(signedIn ? wallet.account : null);
  const myKey = account?.keys.find((k) => k.key_id === wallet?.keyId);
  const caller: Caller = !signedIn ? { kind: "signed-out", hint: !!hint?.account } : !account ? { kind: "reading" } : myKey?.role.type === "session" ? { kind: "agent", label: myKey.label } : ms ? { kind: "shared", threshold: ms.threshold, owners: ms.owners.length } : { kind: "owner", label: myKey?.label ?? "this device" };
  const prefix = client.pin.prefix ?? MAINNET_PREFIX;

  if (!read) return <Spinner>Reading its interface…</Spinner>;
  if ("error" in read) {
    return (
      <p className="card-note" data-testid="program-calls-refused">
        {read.error instanceof ProgramAnswerError ? "Your node's record of this program doesn't check out, so this page offers no forms for it: " : "Couldn't read the program's interface: "}
        {words(read.error)}
      </p>
    );
  }
  const { info } = read;
  // The checked record and the one the page shows disagree (the program changed, and one of the two reads is newer):
  // no call is reviewed until they agree again. A call already sent keeps its outcome on screen.
  const stale = info.version_digest !== shownDigest;
  const { calls, views } = specsFromProgram(info, shownDigest);
  const me = signedIn ? wallet.account : null;
  return (
    <div className="stack" data-testid="program-calls">
      {stale && (
        <p className="warn small" data-testid="program-changed">
          Your node reports another version digest for this program (<span className="mono break">{info.version_digest}</span>) than the record on this page. Reload the page before you call it.
        </p>
      )}
      {views.length > 0 && <span className="tag">views: reads, signed out too</span>}
      {views.map((v) => (
        <details key={v.method} className="action" data-testid="program-view">
          <summary>
            <Named text={v.method} /> <Signature spec={v} />
          </summary>
          <ViewForm spec={v} prefix={prefix} me={me} />
        </details>
      ))}
      {calls.length > 0 && <span className="tag">calls: signed transactions</span>}
      {calls.map((c) => (
        <details key={c.method} className="action" data-testid="program-call">
          <summary>
            <Named text={c.method} /> <Signature spec={c} />
          </summary>
          <CallForm spec={c} info={info} caller={caller} prefix={prefix} me={me} stale={stale} />
        </details>
      ))}
      {!views.length && !calls.length && <p className="card-note">Its interface has no views or calls: only an initializer, which ran when it was created.</p>}
    </div>
  );
}

/** `(id: u64, seller: address) → u64, payable`: the deployer's names in boxes of their own. */
function Signature({ spec }: { spec: ProgramViewSpec | ProgramCallSpec }) {
  return (
    <span className="sig">
      (
      {spec.args.map((a, i) => (
        <span key={i}>
          {i > 0 && ", "}
          {inertText(a.untrusted_name, 32)}: {typeText(a.type)}
        </span>
      ))}
      ){spec.returns ? ` → ${typeText(spec.returns)}` : ""}
      {spec.mode === "call" && spec.payable ? ", payable" : ""}
    </span>
  );
}

/** A method's argument fields. */
function ArgFields({ params, draft, set, touched, touch, prefix, me }: { params: ProgramParam[]; draft: Draft; set: (k: string, v: string | string[]) => void; touched: Record<string, boolean>; touch: (k: string) => void; prefix: string; me: string | null }) {
  if (!params.length) return <span className="dim small">It takes no arguments.</span>;
  return (
    <div className="stack">
      {params.map((p) => {
        const raw = draft[p.key];
        const problem = fieldProblem(p, raw, prefix);
        const show = touched[p.key] && problem;
        return (
          <div key={p.key} className="field" data-testid={`arg-${p.key}`}>
            <span>
              <Named text={p.label} max={32} /> <span className="dim small">{typeText(p.abi)}</span>
              {p.optional && <span className="dim small"> (optional)</span>}
            </span>
            {p.type === "list" ? (
              <ListField p={p} items={Array.isArray(raw) ? raw : []} set={(v) => set(p.key, v)} touch={() => touch(p.key)} me={me} prefix={prefix} />
            ) : (
              <Leaf p={p} type={p.type} value={typeof raw === "string" ? raw : ""} set={(v) => set(p.key, v)} touch={() => touch(p.key)} me={me} prefix={prefix} />
            )}
            {show ? <span className="warn small">{problem}</span> : <span className="dim small">{p.help}</span>}
            {p.type === "text" && typeof raw === "string" && raw && <span className="dim small">{utf8(raw).toLocaleString("en-US")} of {(p.max ?? 0).toLocaleString("en-US")} bytes</span>}
          </div>
        );
      })}
    </div>
  );
}

/** One value's input, by the field's type (a list's items each get one). */
function Leaf({ p, type, value, set, touch, me, prefix, item }: { p: Param; type: Param["type"] | undefined; value: string; set: (v: string) => void; touch: () => void; me: string | null; prefix: string; item?: number }) {
  const label = item === undefined ? p.label : `${p.label}, item ${item + 1}`;
  const common = { value, onChange: (e: { target: { value: string } }) => set(e.target.value), onBlur: touch, "aria-label": label, autoCapitalize: "none", autoCorrect: "off", spellCheck: false };
  if (type === "bool") {
    return (
      <select className="field-select self-start" {...common}>
        {p.optional && <option value="">none</option>}
        <option value="false">false</option>
        <option value="true">true</option>
      </select>
    );
  }
  if (type === "json" || (type === "text" && (p.max ?? 0) > 64)) return <textarea rows={type === "json" ? 4 : 3} {...common} placeholder={type === "json" ? 'e.g. {"some": ["1", "2"]}' : p.optional ? "empty for none" : undefined} />;
  if (type === "fang") {
    return (
      <div className="suffix-input">
        <input {...common} inputMode="decimal" placeholder={p.optional ? "empty for none" : "1.5"} />
        <span className="suffix">{chainUnit()}</span>
      </div>
    );
  }
  const numeric = type === "int" || type === "u64" || type === "u128";
  return (
    <div className="row-gap">
      <input {...common} className={`grow ${type === "address" || type === "hex" ? "mono" : ""}`} inputMode={numeric ? "numeric" : undefined} placeholder={type === "address" ? `${prefix}1…` : type === "hex" ? "hex" : type === "name" ? "alice" : p.optional ? "empty for none" : undefined} />
      {type === "address" && me && value.trim() !== me && (
        <button type="button" className="btn small ghost" onClick={() => set(me)}>
          Use my address
        </button>
      )}
    </div>
  );
}

/** A vec: its items, one field each, up to its bound. */
function ListField({ p, items, set, touch, me, prefix }: { p: ProgramParam; items: string[]; set: (v: string[]) => void; touch: () => void; me: string | null; prefix: string }) {
  const max = p.max ?? 0;
  const leaf = leafOf(p);
  return (
    <div className="stack">
      {items.length === 0 && <span className="dim small">No items: an empty list.</span>}
      {items.map((v, i) => (
        <div key={i} className="row-gap">
          <div className="grow">
            <Leaf p={p} type={p.of} value={v} set={(x) => set(items.map((y, j) => (j === i ? x : y)))} touch={touch} me={me} prefix={prefix} item={i} />
          </div>
          <button type="button" className="btn small ghost" onClick={() => (touch(), set(items.filter((_, j) => j !== i)))}>
            Remove
          </button>
        </div>
      ))}
      {items.length < max && (
        <button type="button" className="btn small" onClick={() => set([...items, p.of === "bool" ? "false" : ""])}>
          Add {items.length ? "another" : "an item"} ({typeText(leaf)})
        </button>
      )}
    </div>
  );
}

/** A value a program returned (or an argument read back from the input), as text. */
function ShownView({ s }: { s: Shown }) {
  if ("leaf" in s) {
    return (
      <>
        <code className={`named ${s.leaf.mono ? "break" : ""}`}>{s.leaf.text}</code>
        {s.leaf.altered && <span className="dim small"> (hidden characters taken out, or cut short: it isn&apos;t shown exactly as the program wrote it)</span>}
      </>
    );
  }
  if ("none" in s) return <span className="dim">none</span>;
  if ("some" in s) return <ShownView s={s.some} />;
  if (!s.items.length) return <span className="dim">an empty list</span>;
  return (
    <ol className="claims">
      {s.items.map((x, i) => (
        <li key={i}>
          <ShownView s={x} />
        </li>
      ))}
    </ol>
  );
}

/** What a method returned, decoded by its declared type, as the program's claim. */
function Returned({ type, hex, prefix }: { type: ProgramType | null; hex: string; prefix: string }) {
  if (type === null) return <span className="dim">nothing (it returns nothing)</span>;
  const r = readReturn(type, hex, prefix);
  if (!r.ok) return <span className="warn">bytes that aren&apos;t a {typeText(type)}: {r.problem}</span>;
  return <ShownView s={shownValue(type, r.value, chainUnit())} />;
}

function useDraft(spec: ProgramViewSpec | ProgramCallSpec, onChange: () => void) {
  const [draft, setDraft] = useState<Draft>(() => initialDraft(spec));
  const [touched, setTouched] = useState<Record<string, boolean>>({});
  return {
    draft,
    touched,
    set: (k: string, v: string | string[]) => {
      setDraft((d) => ({ ...d, [k]: v }));
      onChange();
    },
    touch: (k: string) => setTouched((t) => ({ ...t, [k]: true })),
    touchAll: () => setTouched(Object.fromEntries(Object.keys(draft).map((k) => [k, true]))),
  };
}

/** A view: typed fields, Run, and what it returned. Nothing is signed or paid. */
function ViewForm({ spec, prefix, me }: { spec: ProgramViewSpec; prefix: string; me: string | null }) {
  const { client } = useWallet();
  const [result, setResult] = useState<ProgramViewResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const form = useDraft(spec, () => setResult(null));
  const valid = spec.params.every((p) => !fieldProblem(p, form.draft[p.key], prefix));
  const run = async () => {
    form.touchAll();
    if (!valid) return;
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      const input = encodeDraft(spec, form.draft, prefix);
      setResult(await client.view(spec.program, spec.method, input));
    } catch (e) {
      setError(e instanceof ApiError && e.status === 403 ? `Your node doesn't run views: ${words(e)}` : words(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="action-body">
      <ArgFields params={spec.params} draft={form.draft} set={form.set} touched={form.touched} touch={form.touch} prefix={prefix} me={me} />
      <span className="dim small">A read: your node runs it as of its newest block. Nothing is signed or paid.</span>
      <button className="btn primary self-start" disabled={busy} onClick={run}>
        {busy ? <Spinner>Running…</Spinner> : "Run"}
      </button>
      {result && (
        <div className="preview" aria-live="polite" data-testid="view-result">
          {result.ok ? (
            <>
              <span className="eyebrow">
                What it returned · <Term k="program_claim">the program&apos;s claim</Term>
              </span>
              <div data-testid="view-value">
                <Returned type={spec.returns} hex={result.return_data ?? ""} prefix={prefix} />
              </div>
            </>
          ) : (
            <>
              <span className="eyebrow">It didn&apos;t return</span>
              <p className="m0" data-testid="view-failed">
                {outcomeWords(result.outcome)}.
              </p>
              {result.outcome.untrusted_message && (
                <p className="m0">
                  Its message: <Named text={result.outcome.untrusted_message} max={256} /> <span className="dim small">(the program&apos;s words)</span>
                </p>
              )}
            </>
          )}
          <span className="dim small">
            As of block #{result.height.toLocaleString("en-US")}, {result.fuel_used.toLocaleString("en-US")} fuel. The chain checks nothing a program returns.
          </span>
          {result.ok && (
            <details className="raw">
              <summary>Raw bytes</summary>
              <pre data-testid="view-raw">{result.return_data || "(none)"}</pre>
            </details>
          )}
        </div>
      )}
      <ErrorNote error={error} />
    </div>
  );
}

/** A call as built, before its signature: what the review shows, and the exact transaction signed after the passkey check. */
interface CallReview {
  tx: Tx;
  input: string;
  args: ReturnType<typeof readInput>;
  value: bigint;
  storageLimit: bigint;
  fuel: { run: bigint; load: bigint };
  does: string;
}

/** What became of a call that was signed and sent. Only `applied`, `failed` and `refused` are final enough to go back to the form from. */
type Sent =
  | { state: "waiting"; id: string }
  | { state: "applied"; id: string; summary: string; receipt: ProgramReceipt | null; problem: string | null }
  | { state: "failed"; id: string; summary: string; words: string; message: string | null }
  | { state: "dropped"; id: string; words: string; validUntil: number }
  | { state: "pending"; id: string; validUntil: number }
  | { state: "unknown"; id: string; words: string; validUntil: number }
  | { state: "refused"; id: string; words: string };

/** A call: typed fields (a value only when it's payable, a storage limit), a review of the exact transaction, then a passkey check, signing, and its outcome. */
function CallForm({ spec, info, caller, prefix, me, stale }: { spec: ProgramCallSpec; info: ProgramInfo; caller: Caller; prefix: string; me: string | null; stale: boolean }) {
  const { client, wallet, account, refresh } = useWallet();
  const unlock = useInlineUnlock();
  const check = useSpendCheck();
  const [review, setReview] = useState<CallReview | null>(null);
  const [sent, setSent] = useState<Sent | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const form = useDraft(spec, () => setReview(null));
  const valid = spec.params.every((p) => !fieldProblem(p, form.draft[p.key], prefix));
  // A review is of the record the page shows: when that changes, it's gone (what it built binds the old digest).
  useEffect(() => setReview(null), [spec.versionDigest]);

  const toReview = async () => {
    form.touchAll();
    if (!wallet || !valid) return;
    setBusy("review");
    setError(null);
    try {
      const c = checkCall(spec, form.draft, prefix);
      // The SDK builds the call (it prices the fuel: 11.3's `prepare`), bound to the digest this page shows, and stops
      // before submitting: this wallet signs nothing yet, and the review shows that very transaction.
      const built: { tx?: Tx } = {};
      const stop = new Error("built for review");
      try {
        await previewer(wallet).call(spec.program, spec.method, c.input, {
          value: spec.payable ? c.value : undefined,
          versionDigest: spec.versionDigest,
          storageLimit: c.storageLimit,
          waitMs: 15_000,
          onSigned: (tx) => {
            built.tx = tx;
            throw stop;
          },
        });
      } catch (e) {
        if (e !== stop) throw e;
      }
      const tx = built.tx;
      if (!tx) throw new Error("The call wasn't built, so there's nothing to review. Nothing was signed.");
      if (tx.action.type !== "call_program" || !sameCall(tx.action, specAction(spec, c))) throw new Error("The transaction built for this call isn't the call this form describes, so it isn't offered for signing. Nothing was signed.");
      setReview({ tx, input: c.input, args: readInput(spec.args, c.input, prefix), value: c.value, storageLimit: c.storageLimit, fuel: { run: BigInt(tx.action.fuel_limit), load: loadFuel(info.code.size ?? 0) }, does: describeAction(tx.action) });
    } catch (e) {
      setError(words(e));
    } finally {
      setBusy(null);
    }
  };

  const send = async () => {
    if (!review) return;
    setError(null);
    let signed: Tx;
    try {
      // Every call pays a fee: the passkey checks it's you first, and the key it re-derives signs the transaction reviewed.
      setBusy("check");
      const signer = await check();
      signed = signer.sign(unsignedOf(review.tx));
    } catch (e) {
      setError(e instanceof PasskeyCheckError ? e.message : words(e));
      setBusy(null);
      return;
    }
    // Its id, known before it's sent: an answer that never comes back still leaves the transaction to look up.
    let id = txId(signed);
    setBusy(null);
    setReview(null);
    setSent({ state: "waiting", id });
    try {
      id = await submitTx(signed);
      setSent({ state: "waiting", id });
    } catch (e) {
      // A 4xx is the node refusing it at admission: it never ran and nothing was charged. Anything else, it may have arrived.
      if (e instanceof NodeError && e.status >= 400 && e.status < 500) setSent({ state: "refused", id, words: words(e) });
      else setSent({ state: "unknown", id, words: words(e), validUntil: signed.valid_until });
      return;
    }
    refresh();
    try {
      const done = await client.waitForTx(id, 30_000, signed.valid_until);
      let receipt: ProgramReceipt | null = null;
      let problem: string | null = null;
      try {
        receipt = checkReceipt(client.rpc, done);
        if (!receipt) problem = "your node shows no receipt for it";
      } catch (e) {
        problem = words(e);
      }
      setSent({ state: "applied", id, summary: String(done.summary ?? ""), receipt, problem });
    } catch (e) {
      if (e instanceof TxFailedError && e.dropped) setSent({ state: "dropped", id, words: inertText(e.reason, 600), validUntil: signed.valid_until });
      else if (e instanceof TxFailedError) {
        let receipt: ProgramReceipt | null = null;
        try {
          receipt = checkReceipt(client.rpc, e.tx);
        } catch {
          // Its receipt doesn't check out: the node's own reason stands.
        }
        const failed = receipt && !receipt.outcome.ok ? receipt.outcome : null;
        setSent({ state: "failed", id, summary: String(e.tx?.summary ?? ""), words: failed ? outcomeWords(failed, review.storageLimit) : inertText(e.reason, 600), message: failed?.untrusted_message ?? null });
      } else if (e instanceof TxPendingError) setSent({ state: "pending", id, validUntil: signed.valid_until });
      else setSent({ state: "unknown", id, words: words(e), validUntil: signed.valid_until });
    } finally {
      refresh();
    }
  };

  const doUnlock = async () => {
    setBusy("unlock");
    setError(null);
    try {
      await unlock();
    } catch (e) {
      setError(words(e));
    } finally {
      setBusy(null);
    }
  };

  if (sent) return <CallOutcome spec={spec} sent={sent} prefix={prefix} back={() => setSent(null)} />;
  const storage = typeof form.draft.storage_limit === "string" ? form.draft.storage_limit : "";
  const argValues = review?.args.ok ? review.args.values : null;
  return (
    <div className="action-body">
      <ArgFields params={spec.params} draft={form.draft} set={form.set} touched={form.touched} touch={form.touch} prefix={prefix} me={me} />
      <div className="grid-2">
        {spec.payable && (
          <label className="field">
            <span>Value (optional)</span>
            <div className="suffix-input">
              <input value={String(form.draft.value ?? "")} onChange={(e) => form.set("value", e.target.value)} inputMode="decimal" placeholder="0" aria-label="Value" />
              <span className="suffix">{chainUnit()}</span>
            </div>
            <span className="dim small">FANG sent to the program with the call: it&apos;s payable.</span>
          </label>
        )}
        <label className="field span-all">
          <span>Storage limit</span>
          <div className="suffix-input">
            <input value={storage} onChange={(e) => form.set("storage_limit", e.target.value)} inputMode="decimal" placeholder="0" aria-label="Storage limit" />
            <span className="suffix">{chainUnit()}</span>
          </div>
          <span className="dim small" data-testid="storage-note">
            The most this call may bond as a deposit for what it writes to the program&apos;s storage. At 0 it may write nothing new: a method that writes then traps, and its fee is still charged. This page can&apos;t tell which methods write; the program&apos;s author can.
          </span>
        </label>
      </div>
      <span className="dim small">Who can call it: {spec.who}.</span>

      {caller.kind === "signed-out" ? (
        caller.hint ? (
          <button className="btn primary self-start" disabled={!!busy} onClick={doUnlock} data-testid="call-sign-in">
            {busy === "unlock" ? <Spinner>Waiting for your passkey…</Spinner> : "Unlock to call it"}
          </button>
        ) : (
          <span className="small" data-testid="call-sign-in">
            <Link href="/welcome">Sign in or create a wallet</Link> to call it: views run without one.
          </span>
        )
      ) : caller.kind === "reading" ? (
        <Spinner>Reading your account…</Spinner>
      ) : caller.kind === "agent" ? (
        <span className="warn small" data-testid="call-blocked">
          This device signs with an agent key (“{inertText(caller.label, 40)}”), and agent keys can only send transfers: a program call needs an owner key. Use an owner device.
        </span>
      ) : caller.kind === "shared" ? (
        <span className="warn small" data-testid="call-blocked">
          This is a shared account ({caller.threshold} of {caller.owners} owners approve its actions), and a shared account can&apos;t propose a program call yet. Call it from a personal account.
        </span>
      ) : stale ? (
        <span className="warn small">The program&apos;s record changed while this page was open: reload it, and review the call against the record it shows.</span>
      ) : !review ? (
        <button className="btn primary self-start" disabled={!!busy} onClick={toReview}>
          {busy === "review" ? <Spinner>Preparing…</Spinner> : "Review"}
        </button>
      ) : (
        <div className="preview" aria-live="polite" data-testid="call-review">
          <span className="eyebrow">Read it twice</span>
          <p className="headline">{review.does}</p>
          <dl className="kv">
            <dt>From</dt>
            <dd>
              {account?.names[0] ?? shortAddr(wallet?.account ?? "")} <span className="dim small">signed by “{inertText(caller.label, 40)}” (owner)</span>
            </dd>
            <dt>Program</dt>
            <dd>
              <span className="mono break" data-testid="review-program">{spec.program}</span>
            </dd>
            <dt>Method</dt>
            <dd>
              <Named text={spec.method} /> <span className="dim small">named by its deployer</span>
            </dd>
            <dt>Arguments</dt>
            <dd data-testid="review-args">
              {!spec.args.length ? (
                <span className="dim">none</span>
              ) : argValues ? (
                spec.args.map((a, i) => (
                  <span key={i} className="block">
                    <Named text={a.untrusted_name} max={32} />: <ShownView s={shownValue(a.type, argValues[i], chainUnit())} />
                  </span>
                ))
              ) : review.args.ok ? null : (
                <span className="warn">{review.args.problem}</span>
              )}
              <span className="dim small block">read back from the input below, by the interface&apos;s types</span>
            </dd>
            <dt>Input</dt>
            <dd>
              <span className="mono break" data-testid="review-input">{review.input || "(empty)"}</span>
              <span className="dim small block">{(review.input.length / 2).toLocaleString("en-US")} bytes, Borsh-encoded</span>
            </dd>
            {spec.payable && (
              <>
                <dt>Value</dt>
                <dd data-testid="review-value">
                  <Amount drops={review.value} exact /> <span className="dim small">sent to the program</span>
                </dd>
              </>
            )}
            <dt>Storage limit</dt>
            <dd data-testid="review-storage">
              <Amount drops={review.storageLimit} exact /> <span className="dim small">{review.storageLimit === 0n ? "no new storage: if the method writes, it traps and the fee is still charged" : "at most, bonded as deposit for what it writes"}</span>
            </dd>
            <dt>
              <Term k="digest">Version digest</Term>
            </dt>
            <dd>
              <span className="mono break" data-testid="review-digest">{spec.versionDigest}</span>
              <span className="dim small block">The record on this page (version {info.version}): the call runs only while the program still has exactly this code, interface and version, or it&apos;s refused and nothing is charged.</span>
            </dd>
            <dt>
              <Term k="fuel">Fuel</Term>
            </dt>
            <dd data-testid="review-fuel">
              {(review.fuel.run + review.fuel.load).toLocaleString("en-US")} reserved <span className="dim small">({review.fuel.run.toLocaleString("en-US")} to run, {review.fuel.load.toLocaleString("en-US")} to load its code): paid for whether it&apos;s used or not</span>
            </dd>
            <dt>
              <Term k="fee">Max fee</Term>
            </dt>
            <dd data-testid="review-fee">
              {formatDrops(BigInt(review.tx.fee), chainUnit())} <span className="dim small">at most, fuel included: the base fee for its bytes and the fuel it reserves, burned{BigInt(review.tx.tip ?? 0) > 0n ? `, plus a ${formatDrops(BigInt(review.tx.tip ?? 0), chainUnit())} tip` : ""}</span>
            </dd>
            <dt className="total">Total</dt>
            <dd className="total" data-testid="review-total">
              <b>
                <Amount drops={BigInt(review.tx.fee) + review.value + review.storageLimit} exact />
              </b>{" "}
              <span className="dim small">at most leaves this account</span>
            </dd>
            <dt>
              <Term k="nonce" />
            </dt>
            <dd>{review.tx.nonce}</dd>
          </dl>
          <details className="raw">
            <summary>The exact transaction you&apos;ll sign</summary>
            <pre>{JSON.stringify({ ...review.tx, public_key: `${review.tx.public_key.slice(0, 24)}…`, signature: `${review.tx.signature.length / 2} bytes, made when you sign` }, null, 2)}</pre>
          </details>
          <div className="row-gap">
            <button className="btn" disabled={!!busy} onClick={() => setReview(null)}>
              ← Edit
            </button>
            <button className="btn grow primary" disabled={!!busy} onClick={send}>
              {busy === "check" ? <Spinner>Waiting for your passkey…</Spinner> : "Sign & send"}
            </button>
          </div>
        </div>
      )}
      <ErrorNote error={error} />
    </div>
  );
}

/** A sent call's outcome. One whose fate isn't known (pending, dropped, unknown) is checked by its id, never sent again. */
function CallOutcome({ spec, sent, prefix, back }: { spec: ProgramCallSpec; sent: Sent; prefix: string; back: () => void }) {
  const link = (
    <Link href={to.tx(sent.id)} data-testid="call-tx">
      See the transaction
    </Link>
  );
  const again = (
    <button className="btn small self-start" onClick={back}>
      Back to the form
    </button>
  );
  if (sent.state === "waiting") {
    return (
      <div className="preview" aria-live="polite" data-testid="call-outcome">
        <Spinner>Sent. Waiting for a block…</Spinner>
        {link}
      </div>
    );
  }
  if (sent.state === "refused") {
    return (
      <div className="preview" aria-live="polite" data-testid="call-outcome">
        <span className="eyebrow">Refused</span>
        <p className="m0">Your node refused it before it ran, so nothing was charged. Its words:</p>
        <p className="m0 warn" data-testid="call-words">
          {sent.words}
        </p>
        {again}
      </div>
    );
  }
  if (sent.state === "applied") {
    const returned = sent.receipt?.claims.return_data.find((r) => r.program === spec.program);
    return (
      <div className="preview" aria-live="polite" data-testid="call-outcome">
        <span className="eyebrow">It ran</span>
        <p className="headline" data-testid="call-summary">
          {inertText(sent.summary, 300)}
        </p>
        <dl className="kv">
          <dt>Outcome</dt>
          <dd data-testid="call-words">{sent.receipt ? outcomeWords(sent.receipt.outcome) : `final on your node, but ${sent.problem}`}</dd>
          {sent.receipt && (
            <>
              <dt>
                <Term k="program_claim">Returned</Term>
              </dt>
              <dd data-testid="call-returned">
                {returned ? <Returned type={spec.returns} hex={returned.data} prefix={prefix} /> : <span className="dim">nothing in its receipt</span>}
                <span className="dim small block">the program&apos;s claim: the chain checks none of it</span>
              </dd>
              <dt>Fee charged</dt>
              <dd>
                <Amount drops={sent.receipt.facts.fee.bytes + sent.receipt.facts.fee.fuel + sent.receipt.facts.fee.tip} exact />
              </dd>
            </>
          )}
        </dl>
        <div className="row-gap">
          {link}
          {again}
        </div>
      </div>
    );
  }
  if (sent.state === "failed") {
    return (
      <div className="preview" aria-live="polite" data-testid="call-outcome">
        <span className="eyebrow">It ran and failed</span>
        {sent.summary && <p className="headline" data-testid="call-summary">{inertText(sent.summary, 300)}</p>}
        <p className="m0" data-testid="call-words">
          {sent.words}. Its fee was charged.
        </p>
        {sent.message && (
          <p className="m0">
            Its message: <Named text={sent.message} max={256} /> <span className="dim small">(the program&apos;s words)</span>
          </p>
        )}
        <div className="row-gap">
          {link}
          {again}
        </div>
      </div>
    );
  }
  // Pending, dropped or unknown: it may still run. Check it by its id; don't send it again.
  return (
    <div className="preview" aria-live="polite" data-testid="call-outcome">
      <span className="eyebrow">Sent, not final</span>
      <p className="m0" data-testid="call-words">
        {sent.state === "pending"
          ? "Submitted, and not in a block yet."
          : sent.state === "dropped"
            ? `Your node dropped it without running it, so nothing was charged (${sent.words}). Another node may still include it.`
            : `It may have reached your node: ${sent.words}.`}{" "}
        It can run until block #{sent.validUntil.toLocaleString("en-US")}: check the transaction before doing anything else, and don&apos;t send it again.
      </p>
      {link}
    </div>
  );
}
