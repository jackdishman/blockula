"use client";

// Shared accounts (multisig) in the wallet: make one, see its owners and open proposals, propose
// an action, review one and approve it. Words and rules live in lib/multisig.ts. Everything shown
// about a proposal is rendered here from its action, the content an approval binds by its digest
// (`reviewAction`, `actionDigest`), never from the node's summary (audit gap-7-6).
import "@/app/creators.css";
import "@/app/explorer.css";
import "@/app/onboarding.css";
import { decodeKeyGrant, describeAction, encodeKeyGrant, isAddress, type Action, type KeyGrant } from "@blockula/sdk";
import Link from "next/link";
import { useEffect, useMemo, useState } from "react";
import { ActionForm } from "@/components/explorer/ActionForm";
import { Party, ReviewCard, ReviewDetails } from "@/components/explorer/Review";
import { Copy, ErrorNote, ExplorerLink, Spinner, Tip } from "@/components/ui";
import { explorer, MAINNET_PREFIX, walletUrl } from "@/lib/config";
import { multisigRule, multisigShape } from "@/lib/creators";
import { friendly } from "@/lib/errors";
import { NodeError, useNode } from "@/lib/explorer/node";
import { actionId, PROGRAMS } from "@/lib/explorer/programs";
import type { MultisigView } from "@/lib/explorer/types";
import { aboutWhen, blocksToHuman, keyCode, shortAddr } from "@/lib/format";
import { addOwnerSpec, agentBrief, asProposal, cosignLink, createWarnings, digestCheck, ownerKeyIds, proposable, removeKeySpec, reviewHref, reviewPending, sharedHref, thresholdProblem, thresholdSpec, type PendingReview } from "@/lib/multisig";
import { SAFETY } from "@/lib/onboarding-copy";
import { useSpendCheck } from "@/lib/spend";
import type { KeyAccounts, KeyView, TxView } from "@/lib/types";
import { useUnlocked } from "@/lib/wallet";

type Pending = MultisigView["pending"][number];

/**
 * An account's multisig record, kept fresh: `ms` when it's a shared account, `plain` when the
 * node says it isn't one (404). Only the node's answer for this very account counts.
 */
export function useMultisig(account: string | null) {
  // Most accounts aren't one (the node answers 404), so it's read once, and kept fresh only for a shared account.
  const [poll, setPoll] = useState(false);
  const live = useNode<MultisigView>(account ? `/multisig/${encodeURIComponent(account)}` : null, { every: poll ? 5000 : undefined });
  if (live.data && !poll) setPoll(true);
  const plain = live.error instanceof NodeError && live.error.status === 404;
  const ms = !plain && live.data && live.data.account?.address === account ? live.data : null;
  return { ms, plain, loading: !ms && !plain && !live.error, error: plain ? null : live.error, reload: live.reload };
}

/** How long a proposal waits for approvals on this chain, in words, once the node has said. */
function useExpiry(): string | undefined {
  const { status } = useUnlocked();
  const params = useNode<{ params?: { multisig_expiry_blocks?: number | string } }>("/params");
  const blocks = Number(params.data?.params?.multisig_expiry_blocks);
  if (!Number.isSafeInteger(blocks) || blocks <= 0 || !status) return undefined;
  return blocksToHuman(blocks, status.block_time_ms).replace(/^day$/, "1 day");
}

/** Makes the wallet act for another account this key is on (as the unlock screen's account picker does). */
function useSwitchAccount() {
  const { wallet, hint, unlocked } = useUnlocked();
  return async (address: string, name?: string | null) => {
    await wallet.actAs(address);
    unlocked(wallet, { ...hint, account: address, name: name ?? undefined });
  };
}

/** The chain this wallet is pinned to, for links (a check for whoever opens one, never a switch). */
function useLinkChain() {
  const { client, status } = useUnlocked();
  return client.pin.chainId && (client.pin.genesisHash || status) ? client.pin : undefined;
}

// ---------------------------------------------------------------------------------------------
// The hub: this account, this device's key, and the other accounts the key co-owns.

export function SharedAccounts({ linked }: { linked: string | null }) {
  const { wallet, account } = useUnlocked();
  const me = wallet.account;
  const { ms, plain, error, reload } = useMultisig(me);
  if (!account) return <main className="narrow"><Spinner>Loading your account…</Spinner></main>;
  const myKey = account.keys.find((k) => k.key_id === wallet.keyId);
  return (
    <main className="narrow stack-lg">
      <Link href="/" className="back">← Home</Link>
      <h1>Shared accounts</h1>
      <p className="dim">
        A shared account acts only when enough of its owners agree: 2 of 3 people, say, or you and your AI agent together. Each owner keeps their own key, and the chain counts the approvals. Nobody holds anyone else&apos;s key.
      </p>

      {linked && linked !== me && <OtherAccount target={linked} what="That link is about another account" />}

      {ms ? (
        <SharedAccount ms={ms} account={me} myKey={myKey} />
      ) : plain ? (
        <MakeShared keys={account.keys} myKey={myKey} onDone={reload} />
      ) : error ? (
        <section className="card"><ErrorNote error={friendly(error)} /></section>
      ) : (
        <section className="card"><Spinner>Asking the chain about this account…</Spinner></section>
      )}

      <YourKey />
      <CoOwned current={me} />
    </main>
  );
}

/** A shared account's owners and threshold, its open proposals, and a form to propose an action. */
function SharedAccount({ ms, account, myKey }: { ms: MultisigView; account: string; myKey?: KeyView }) {
  const { wallet, account: view, status } = useUnlocked();
  const owner = myKey?.role.type === "owner";
  const shape = multisigShape(ms.threshold, ms.owners.length);
  const agents = view?.keys.filter((k) => k.role.type === "session").length ?? 0;
  const brief = agentBrief(account, shape);
  return (
    <>
      <section className="card stack" data-testid="shared-account">
        <div className="row-between">
          <h2>This account is shared</h2>
          <span className="chip" data-testid="shared-shape">{shape}</span>
        </div>
        <p className="m0" data-testid="shared-rule">
          {multisigRule(ms.threshold, ms.owners.length)}{agents ? "; agents with a budget still send within their limits" : ""}.
        </p>
        <span className="mono small break">{account}</span>
        <div>
          {ms.owners.map((o) => (
            <div className="key-row" key={o.key_id} data-testid="shared-owner">
              <div className="grow">
                <span className="key-label">{o.label || "an owner key"}</span> {o.key_id === wallet.keyId && <span className="pill">this device</span>}
                <span className="dim small block mono">{keyCode(o.key_id)}</span>
              </div>
              <Copy text={o.key_id} label="Copy key id" />
            </div>
          ))}
        </div>
        <div className="row-gap">
          <ExplorerLink className="small" href={explorer.account(account)}>Its page in the explorer →</ExplorerLink>
          <Copy text={`${walletUrl().replace(/\/$/, "")}${sharedHref(account)}`} label="Copy this page's link" />
        </div>
        <details>
          <summary className="small">Is one of the owners an AI agent? What to tell it</summary>
          <div className="stack">
            <pre className="snippet" data-testid="shared-agent-brief">{brief}</pre>
            <Copy text={brief} label="Copy for your agent" />
          </div>
        </details>
      </section>

      <section className="card stack" id="proposals">
        <div className="row-between">
          <h2>Open proposals</h2>
          <span className="dim small">{ms.pending.length ? `${ms.pending.length} waiting` : "none waiting"}</span>
        </div>
        {ms.pending.length ? (
          <ul className="ledger" data-testid="shared-pending">
            {ms.pending.map((p) => (
              <PendingRow key={p.id} p={p} ms={ms} account={account} head={status?.height} />
            ))}
          </ul>
        ) : (
          <p className="dim m0">Nothing is waiting for approval.</p>
        )}
        <Tip>Each proposal is described here from the action itself, the content an approval binds by its digest, not from the node&apos;s words.</Tip>
      </section>

      {owner ? (
        <Propose account={account} owners={ms.owners.map((o) => o.key_id)} threshold={ms.threshold} />
      ) : (
        <section className="card">
          <p className="dim m0">This device signs with {myKey ? `an agent key (“${myKey.label}”)` : "a key that isn't on this account"}, so it can&apos;t propose or approve. Owner keys do.</p>
        </section>
      )}
    </>
  );
}

/** What this wallet reads from a pending proposal; a ballot is read against the governance proposal it names. */
function usePendingReview(p: Pending | undefined, ms: MultisigView | null, head?: number): { review: PendingReview | null; proposal: unknown } {
  const { wallet } = useUnlocked();
  const action = p?.action as Record<string, unknown> | undefined;
  const ballot = action?.type === "vote" && Number.isSafeInteger(action.proposal_id) ? (action.proposal_id as number) : null;
  const proposal = useNode<unknown>(ballot != null ? `/proposal/${ballot}` : null);
  const review = p && ms ? reviewPending(p, ms, { proposal: proposal.data, head, myKeyId: wallet.keyId }) : null;
  return { review, proposal: proposal.data };
}

function PendingRow({ p, ms, account, head }: { p: Pending; ms: MultisigView; account: string; head?: number }) {
  const { status } = useUnlocked();
  const chain = useLinkChain();
  const { review } = usePendingReview(p, ms, head);
  if (!review) return null;
  const left = head != null ? review.expiresAt - head : null;
  return (
    <li className="cre-ms-action" data-testid="shared-proposal">
      <div className="row-between">
        <span className="cre-ms-id">#{review.id}</span>
        <span className={`pill ${review.expired || review.superseded ? "dim" : "pending"}`} data-testid="shared-approvals">
          {review.approvals} of {review.threshold} approvals
        </span>
      </div>
      <p className="cre-ms-does" data-testid="shared-does">{review.does}</p>
      <span className="small dim block">
        {review.expired ? "expired at" : "expires at"} block #{review.expiresAt.toLocaleString("en-US")}
        {!review.expired && left != null && left > 0 ? ` (${aboutWhen(left, status?.block_time_ms)})` : ""}
        {review.approvedByMe ? " · you approved it" : ""}
      </span>
      <div className="row-gap">
        <Link className="btn small" href={reviewHref(account, review.id, review.digest)}>
          {review.approvable ? "Review & approve" : "Review"}
        </Link>
        <Copy text={cosignLink(walletUrl(), account, review.id, review.digest, chain)} label="Copy co-sign link" />
      </div>
    </li>
  );
}

/** Propose an action for the shared account: the explorer's typed forms, wrapped in a proposal. */
function Propose({ account, owners, threshold }: { account: string; owners: string[]; threshold: number }) {
  const specs = useMemo(() => {
    const native = PROGRAMS.flatMap((p) => p.actions).filter(proposable);
    return [...native.filter((s) => s.kind === "transfer"), addOwnerSpec(account), removeKeySpec(), thresholdSpec(owners), ...native.filter((s) => s.kind !== "transfer")];
  }, [account, owners]);
  const [picked, setPicked] = useState(actionId(specs[0]));
  const spec = specs.find((s) => actionId(s) === picked) ?? specs[0];
  return (
    <section className="card stack" id="propose" data-testid="shared-propose">
      <h2>Propose an action</h2>
      <p className="dim m0">
        {threshold <= 1
          ? "The threshold is 1, so what you propose runs at once, as the account."
          : `Your proposal counts as your approval. It runs, as the account, once ${threshold - 1 === 1 ? "one more owner key approves" : `${threshold - 1} more owner keys approve`} it. Then send them its co-sign link from the list above.`}
      </p>
      <label className="field">
        <span>Action</span>
        <select className="field-select" value={picked} onChange={(e) => setPicked(e.target.value)} aria-label="Action to propose">
          {specs.map((s) => (
            <option key={actionId(s)} value={actionId(s)}>{s.title}</option>
          ))}
        </select>
      </label>
      <ActionForm key={actionId(spec)} spec={asProposal(spec)} />
    </section>
  );
}

/** A pasted grant code, parsed (or why it can't be), and whether it's for this account. */
function parseGrant(code: string, account: string): { grant: KeyGrant | null; problem: string | null } {
  if (!code.trim()) return { grant: null, problem: null };
  try {
    const grant = decodeKeyGrant(code);
    if (grant.account !== account) return { grant: null, problem: `This code is for another account (${grant.account.slice(0, 12)}…). The other side must make it for ${account}.` };
    return { grant, problem: null };
  } catch (e) {
    return { grant: null, problem: friendly(e) };
  }
}

/** An ordinary account: add co-owners (each by its own grant code), pick the threshold, read what it means, sign. */
function MakeShared({ keys, myKey, onDone }: { keys: KeyView[]; myKey?: KeyView; onDone: () => void }) {
  const { wallet, refresh } = useUnlocked();
  const spendCheck = useSpendCheck();
  const expiry = useExpiry();
  const owners = keys.filter((k) => k.role.type === "owner");
  const ids = ownerKeyIds(keys);
  const agents = keys.length - owners.length;
  const [threshold, setThreshold] = useState(2);
  const [understood, setUnderstood] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<TxView | null>(null);
  const n = owners.length;
  const m = Math.min(threshold, Math.max(1, n));
  const problem = thresholdProblem(m, n);
  const action: Action = { type: "multisig_create", owners: ids, threshold: m };

  if (myKey?.role.type !== "owner")
    return (
      <section className="card">
        <h2>This account isn&apos;t shared</h2>
        <p className="dim m0">This device signs with an agent key, and only an owner key can make an account shared.</p>
      </section>
    );

  const create = async () => {
    setBusy(true);
    setError(null);
    try {
      await spendCheck();
      const tx = (await wallet.multisigCreate(ids, m)) as TxView;
      setDone(tx);
      await refresh();
      onDone();
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="card stack" data-testid="make-shared">
      <h2>Make this account shared</h2>
      <p className="dim m0">
        Its owners will be exactly this account&apos;s owner keys, so add each co-owner first. They, or their agent, make a grant code for this account; a key id alone can&apos;t be added, because a key joins an account only with its own consent.
      </p>
      <div>
        {owners.map((k) => (
          <div className="key-row" key={k.key_id} data-testid="owner-key">
            <div className="grow">
              <span className="key-label">{k.label}</span> {k.key_id === wallet.keyId && <span className="pill">this device</span>}
              <span className="dim small block mono">{keyCode(k.key_id)}</span>
            </div>
          </div>
        ))}
      </div>
      <AddCoOwner />
      <hr className="rule" />
      {n < 2 ? (
        <p className="dim m0" data-testid="needs-coowner">Add at least one co-owner above before making the account shared: with one owner key there&apos;s nobody to share it with.</p>
      ) : (
        <>
          <label className="field">
            <span>Approvals needed</span>
            <select className="field-select" value={m} onChange={(e) => { setThreshold(Number(e.target.value)); setUnderstood(false); }} aria-label="Approvals needed" data-testid="threshold">
              {Array.from({ length: n }, (_, i) => i + 1).map((t) => (
                <option key={t} value={t}>{t} of {n}</option>
              ))}
            </select>
          </label>
          {problem && <span className="warn">{problem}</span>}
          <div className="card warn-card stack" data-testid="shared-warning">
            <span className="eyebrow">Before you sign</span>
            <ul className="steps">
              {createWarnings(m, n, { agents, expiry }).map((w) => (
                <li key={w}>{w}</li>
              ))}
            </ul>
          </div>
          <p className="dim small m0">
            You&apos;ll sign: <span data-testid="shared-signs">{describeAction(action)}</span>
          </p>
          <label className="row-gap small">
            <input type="checkbox" checked={understood} onChange={(e) => setUnderstood(e.target.checked)} data-testid="understood" />
            <span>I understand this can&apos;t be undone, and that every owner action will need {m} of {n} approvals</span>
          </label>
          <button className="btn primary" disabled={!understood || !!problem || busy} onClick={create}>
            {busy ? <Spinner>Waiting for your passkey…</Spinner> : `Make it a ${m}-of-${n} shared account`}
          </button>
        </>
      )}
      <ErrorNote error={error} />
      {done && <p className="ok">✓ {done.summary}. <ExplorerLink href={explorer.tx(done.id)}>View ↗</ExplorerLink></p>}
    </section>
  );
}

/** Adds a person's or an agent's key as an owner, from the grant code its holder made for this account. */
function AddCoOwner() {
  const { wallet, refresh } = useUnlocked();
  const spendCheck = useSpendCheck();
  const [code, setCode] = useState("");
  const [label, setLabel] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<TxView | null>(null);
  const { grant, problem } = parseGrant(code, wallet.account);

  const add = async () => {
    if (!grant) return;
    setBusy(true);
    setError(null);
    try {
      await spendCheck();
      const tx = (await wallet.addOwnerKey(grant, label.trim() || "Co-owner")) as TxView;
      setDone(tx);
      setCode("");
      setLabel("");
      await refresh();
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="stack" data-testid="add-coowner">
      <h3 className="m0">Add a co-owner</h3>
      <ol className="steps">
        <li>
          Send them this account&apos;s address: <span className="mono break" data-testid="shared-address">{wallet.account}</span> <Copy text={wallet.account} label="Copy address" />
        </li>
        <li>A person opens their own wallet → <b>Shared accounts → Join a shared account</b> and pastes it. An agent (the Blockula MCP) calls <code>use_account</code> with it. Either way you get back a grant code.</li>
        <li>Paste the code here, check the key&apos;s code with them, and add it.</li>
      </ol>
      <label className="field">
        <span>Co-owner&apos;s grant code</span>
        <textarea value={code} onChange={(e) => setCode(e.target.value)} placeholder="bkgrant1.…" className="mono" autoCapitalize="none" spellCheck={false} rows={3} />
      </label>
      {code.trim() && (grant ? <span className="ok" data-testid="coowner-check">Check code: <b className="code inline">{keyCode(grant.key_id)}</b> (the start and end of their key id)</span> : <span className="warn">{problem}</span>)}
      <label className="field">
        <span>Name it <span className="dim onb-public">{SAFETY.publicLabelShort}</span></span>
        <input value={label} onChange={(e) => setLabel(e.target.value)} maxLength={64} placeholder="Mina, or claude" />
      </label>
      <p className="onb-warn" data-testid="coowner-warning">
        Until the account is shared, an owner key has full control of it on its own: it can send everything and remove your keys. Add only keys whose holders you trust, and make the account shared before you fund it.
      </p>
      <button className="btn" disabled={!grant || busy} onClick={add}>
        {busy ? <Spinner>Adding…</Spinner> : "Add as an owner"}
      </button>
      <ErrorNote error={error} />
      {done && <p className="ok">✓ {done.summary}. <ExplorerLink href={explorer.tx(done.id)}>View ↗</ExplorerLink></p>}
    </div>
  );
}

/** This device's key id, and its grant code for someone else's shared account. */
function YourKey() {
  const { wallet, client } = useUnlocked();
  const [target, setTarget] = useState("");
  const [code, setCode] = useState<{ account: string; code: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const prefix = client.pin.prefix ?? MAINNET_PREFIX;
  const address = target.trim().toLowerCase();
  const valid = isAddress(address, prefix);
  const own = address === wallet.account;

  const make = async () => {
    setBusy(true);
    setError(null);
    try {
      // Always an address (the other owner reads it off their own wallet), so no node's word for a name is signed.
      setCode({ account: address, code: encodeKeyGrant(await wallet.keyGrant(address)) });
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="card stack" id="join" data-testid="your-key">
      <h2>Join a shared account</h2>
      <p className="dim m0">This device&apos;s key can co-own someone else&apos;s account too. Your own account stays as it is.</p>
      <div>
        <span className="eyebrow">This device&apos;s key id</span>
        <div className="mono small break" data-testid="my-key-id">{wallet.keyId}</div>
        <div className="row-gap">
          <Copy text={wallet.keyId} label="Copy key id" />
          <span className="dim small">check code {keyCode(wallet.keyId)}</span>
        </div>
      </div>
      <label className="field">
        <span>The shared account&apos;s address</span>
        <input value={target} onChange={(e) => { setTarget(e.target.value); setCode(null); }} placeholder={`${prefix}1…`} className="mono" autoCapitalize="none" autoCorrect="off" spellCheck={false} aria-label="The shared account's address" />
      </label>
      {address && !valid && <span className="warn small">An address of this chain ({prefix}1…), not a name: ask its owner to copy it from their wallet.</span>}
      {own && <span className="warn small">That&apos;s the account this wallet already acts for.</span>}
      <button className="btn" disabled={!valid || own || busy} onClick={make}>
        {busy ? <Spinner>Signing…</Spinner> : "Make my grant code"}
      </button>
      <ErrorNote error={error} />
      {code && (
        <div className="stack" data-testid="join-grant">
          <p className="m0">Send this code to the account&apos;s owner. It holds no secret: it&apos;s this key&apos;s consent to joining <span className="mono">{shortAddr(code.account)}</span>, and nothing else. They&apos;ll see the check code <b className="code inline">{keyCode(wallet.keyId)}</b>.</p>
          <pre className="snippet">{code.code}</pre>
          <Copy text={code.code} label="Copy grant code" />
          <span className="dim small">Once they&apos;ve added it, the account appears below.</span>
        </div>
      )}
    </section>
  );
}

/** The other accounts this device's key is on, read from the node; each one's own key list is checked before the wallet switches to it. */
function CoOwned({ current }: { current: string }) {
  const { wallet, client, status } = useUnlocked();
  const [list, setList] = useState<KeyAccounts["accounts"] | null>(null);
  const tick = status ? Math.floor(status.height / 10) : 0;
  useEffect(() => {
    let live = true;
    (async () => {
      const out: KeyAccounts["accounts"] = [];
      let after: string | undefined;
      for (let page = 0; page < 5; page++) {
        const res = (await client.keyAccounts(wallet.keyId, after ? { after } : {})) as KeyAccounts;
        out.push(...res.accounts);
        if (!res.next) break;
        after = res.next;
      }
      const seen = new Set<string>();
      if (live) setList(out.filter((a) => !seen.has(a.account.address) && !!seen.add(a.account.address)));
    })().catch(() => live && setList([]));
    return () => {
      live = false;
    };
  }, [client, wallet.keyId, tick]);
  const others = (list ?? []).filter((a) => a.account.address !== current);
  if (!others.length) return null;
  return (
    <section className="card stack" data-testid="co-owned">
      <h2>Other accounts this key is on</h2>
      {others.map((a) => (
        <CoOwnedRow key={a.account.address} address={a.account.address} role={a.role.type} />
      ))}
    </section>
  );
}

function CoOwnedRow({ address, role }: { address: string; role: string }) {
  const { ms } = useMultisig(address);
  const go = useSwitchAccount();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const open = async () => {
    setBusy(true);
    setError(null);
    try {
      await go(address, ms?.account.name);
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="key-row" data-testid="co-owned-row">
      <div className="grow">
        <span className="key-label">{ms?.account.name ?? shortAddr(address)}</span> {ms && <span className="chip">{multisigShape(ms.threshold, ms.owners.length)}</span>}
        <span className="dim small block mono break">{address}</span>
        <span className="dim small block">{role === "owner" ? "owner key" : "agent key (sends only)"}{ms?.pending.length ? ` · ${ms.pending.length} waiting for approval` : ""}</span>
        <ErrorNote error={error} />
      </div>
      <button className="btn small" disabled={busy} onClick={open}>{busy ? <Spinner /> : "Switch to it"}</button>
    </div>
  );
}

/**
 * The page is about `target`, and the wallet acts for another account. If this device's key is on
 * `target` (by that account's own key list), offer to switch; otherwise say how to join.
 */
export function OtherAccount({ target, what }: { target: string; what: string }) {
  const { wallet, account } = useUnlocked();
  const there = useNode<{ account: { name: string | null }; keys: KeyView[] }>(`/account/${encodeURIComponent(target)}`);
  const go = useSwitchAccount();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const key = there.data?.keys.find((k) => k.key_id === wallet.keyId);
  const now = account?.names[0] ?? shortAddr(wallet.account);
  const open = async () => {
    setBusy(true);
    setError(null);
    try {
      await go(target, there.data?.account.name);
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className="card warn-card stack" data-testid="other-account">
      <p className="m0">
        {what}: <span className="mono break">{target}</span>. This wallet is acting for {now}.
      </p>
      {!there.data && !there.error ? (
        <Spinner>Looking for this device&apos;s key there…</Spinner>
      ) : key?.role.type === "owner" ? (
        <>
          <p className="m0">This device&apos;s key is one of that account&apos;s owner keys (“{key.label}”). Switch to it to propose and approve. You can switch back from the same page, or when you unlock.</p>
          <button className="btn primary" disabled={busy} onClick={open}>{busy ? <Spinner>Switching…</Spinner> : "Switch to that account"}</button>
        </>
      ) : key ? (
        <p className="m0">This device&apos;s key is an agent key there (“{key.label}”): it can send within its limit, but only owner keys propose and approve.</p>
      ) : (
        <p className="m0">This device&apos;s key isn&apos;t on that account, so it can read but not approve. To join it, make a grant code under <Link href="/shared#join">Join a shared account</Link> and send it to one of its owners.</p>
      )}
      <ErrorNote error={error} />
    </section>
  );
}

// ---------------------------------------------------------------------------------------------
// Review one proposal, and approve it.

export function ReviewProposal({ account, id, linkedDigest }: { account: string; id: number; linkedDigest: string | null }) {
  const { wallet, status, refresh } = useUnlocked();
  const spendCheck = useSpendCheck();
  const chain = useLinkChain();
  const { ms, plain, error: readError, reload } = useMultisig(account);
  const p = ms?.pending.find((x) => x.id === id);
  const { review, proposal } = usePendingReview(p, ms, status?.height);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<{ tx: TxView; ran: boolean } | null>(null);
  const acting = wallet.account === account;

  if (done)
    return (
      <section className="card stack" data-testid="approved">
        <h2>{done.ran ? "Approved, and it ran" : "Approved"}</h2>
        <p className="ok m0">✓ {done.tx.summary}. <ExplorerLink href={explorer.tx(done.tx.id)}>View ↗</ExplorerLink></p>
        <p className="dim m0">{done.ran ? "Your approval reached the threshold, so the chain ran the action as the account." : "It waits for the other owners' approvals."}</p>
        <Link className="btn" href={sharedHref(account)}>Back to the shared account</Link>
      </section>
    );
  if (plain) return <section className="card"><p className="m0" data-testid="review-missing"><span className="mono break">{account}</span> isn&apos;t a shared account on this chain.</p></section>;
  if (readError) return <section className="card"><ErrorNote error={friendly(readError)} /></section>;
  if (!ms) return <section className="card"><Spinner>Reading the proposal from the chain…</Spinner></section>;
  if (!p || !review)
    return (
      <section className="card stack" data-testid="review-missing">
        <p className="m0">There&apos;s no open proposal #{id} on this account. It has run, it expired, or it never existed.</p>
        <ExplorerLink href={explorer.account(account)}>See the account&apos;s history →</ExplorerLink>
      </section>
    );

  const check = digestCheck(linkedDigest, review.digest);
  const labels = new Map(ms.owners.map((o) => [o.key_id, o.label || keyCode(o.key_id)]));
  const left = status ? review.expiresAt - status.height : null;
  const canSign = acting && review.approvable && check !== "mismatch";

  const approve = async () => {
    if (!review.digest) return;
    setBusy(true);
    setError(null);
    try {
      await spendCheck();
      // Binds the action shown here: the SDK refuses, before signing, one that reads differently by now.
      const tx = (await wallet.multisigApprove(id, { action: p.action as unknown as Action, digest: review.digest, proposal })) as TxView;
      setDone({ tx, ran: review.wouldRun });
      await refresh();
      reload();
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      {!acting && <OtherAccount target={account} what="This proposal belongs to the shared account" />}
      <ReviewCard
        testid="review"
        className="card stack"
        reveal={false}
        aside={<span className={`pill ${review.expired || review.superseded ? "dim" : "pending"}`} data-testid="review-approvals">{review.approvals} of {review.threshold} approvals</span>}
        action={review.does}
        actionTestid="review-does"
        rows={[
          // The address leads: a name here is only the node's word for it.
          { k: "Account", v: <Party address={account} />, hint: `${multisigShape(ms.threshold, ms.owners.length)}${ms.account.name ? ` · your node calls it ${ms.account.name}` : ""}` },
          {
            k: "Approved by",
            testid: "review-by",
            v: review.approvedBy.length ? review.approvedBy.map((k) => (k === wallet.keyId ? `${labels.get(k)} (this device)` : labels.get(k))).join(", ") : "nobody yet",
          },
          { k: "Expires", v: <>block #{review.expiresAt.toLocaleString("en-US")} {!review.expired && left != null && left > 0 && <span className="dim small">({aboutWhen(left, status?.block_time_ms)})</span>}</> },
        ]}
        details={
          <ReviewDetails
            rows={[
              { k: "Account", v: <span className="mono small break">{account}</span> },
              { k: "Proposal", v: `#${review.id}` },
              review.strayApprovals.length > 0 && { k: "Not counted", v: `${review.strayApprovals.length} from a key that isn't an owner any more` },
              { k: "Digest", v: <span className="mono small break" data-testid="review-digest">{review.digest ?? "can't be computed"}</span>, hint: "This wallet rendered the action above from the action itself. Your approval signs this digest, so it can't count for any other action." },
              { k: "Fee", v: "Paid from the shared account." },
            ]}
          />
        }
        raw={
          <details className="raw">
            <summary>The exact action an approval binds</summary>
            <pre>{JSON.stringify(p.action, null, 2)}</pre>
          </details>
        }
      >
        {check === "mismatch" && (
          <p className="error" role="alert" data-testid="review-mismatch">
            This isn&apos;t the action the link was made for. Whoever sent the link saw an action with another digest, so the node is showing something else now, or the link was changed. Don&apos;t approve it; ask them what they proposed.
          </p>
        )}
        {check === "match" && <p className="ok small m0" data-testid="review-match">✓ The same action the link&apos;s sender saw (its digest matches the link&apos;s).</p>}
        {review.superseded && <p className="dim small m0 wrap-any">The node says: {review.superseded}</p>}
        {review.why && <p className="warn m0" data-testid="review-why">{review.why}</p>}
        {canSign && (
          <>
            <p className="small m0" data-testid="review-effect">
              {review.wouldRun
                ? `Yours is approval ${review.approvals + 1} of ${review.threshold}: the chain runs the action at once, as the account. If the action fails, so does your approval, and the proposal stays open.`
                : `Yours is approval ${review.approvals + 1} of ${review.threshold}: it then waits for ${review.threshold - review.approvals - 1} more.`}
            </p>
            <button className="btn primary" disabled={busy} onClick={approve}>
              {busy ? <Spinner>Waiting for your passkey…</Spinner> : review.wouldRun ? "Approve and run it" : "Approve"}
            </button>
          </>
        )}
        <ErrorNote error={error} />
        <div className="row-gap">
          <Copy text={cosignLink(walletUrl(), account, review.id, review.digest, chain)} label="Copy co-sign link" />
          <Link className="small" href={sharedHref(account)}>All of this account&apos;s proposals →</Link>
        </div>
      </ReviewCard>
    </>
  );
}

// ---------------------------------------------------------------------------------------------
// Home: a shared account says so, since Send, Stake and Governance no longer work for its owner keys.

export function SharedBanner() {
  const { wallet } = useUnlocked();
  const { ms } = useMultisig(wallet.account);
  if (!ms) return null;
  const waiting = ms.pending.filter((p) => !p.expired).length;
  return (
    <div className="banner" data-testid="shared-banner">
      <b>This is a {multisigShape(ms.threshold, ms.owners.length).replace("multisig", "shared account")}.</b> Its owner keys propose and approve; they can&apos;t send directly.{" "}
      <Link href="/shared">{waiting ? `${waiting} waiting for approval` : "Shared accounts"}</Link>
    </div>
  );
}
