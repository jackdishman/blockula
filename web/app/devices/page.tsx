"use client";

import { formatFang, parseFang } from "@blockula/sdk";
import Link from "next/link";
import { useState } from "react";
import { RequireWallet } from "@/components/Gate";
import { Copy, ErrorNote, ExplorerLink, Spinner, Tip } from "@/components/ui";
import { BLOCKS_PER_DAY, explorer } from "@/lib/config";
import { friendly } from "@/lib/errors";
import { blocksToHuman, isKeyId, keyCode } from "@/lib/format";
import type { KeyView, TxView } from "@/lib/types";
import { useUnlocked } from "@/lib/wallet";

export default function DevicesPage() {
  return (
    <RequireWallet>
      <Devices />
    </RequireWallet>
  );
}

function Devices() {
  const { account } = useUnlocked();
  if (!account) return <main className="narrow"><Spinner>Loading…</Spinner></main>;
  const owners = account.keys.filter((k) => k.role.type === "owner");
  const sessions = account.keys.filter((k) => k.role.type === "session");
  return (
    <main className="narrow stack-lg">
      <Link href="/" className="back">← Home</Link>
      <h1>Devices &amp; agents</h1>
      <p className="dim">
        Your account isn&apos;t one key: it&apos;s a list of keys, each with its own permissions. Owners can do anything. Agents (session keys) can only send FANG, within a limit, until they expire.
      </p>

      <section className="card">
        <h2>Owner devices</h2>
        {owners.map((k) => <KeyRow key={k.key_id} k={k} canRevoke={owners.length > 1} />)}
      </section>

      <section className="card">
        <h2>Agents with a budget</h2>
        {sessions.length ? sessions.map((k) => <KeyRow key={k.key_id} k={k} canRevoke />) : <p className="dim">No agents yet.</p>}
      </section>

      <AddDevice />
      <AddAgent />
    </main>
  );
}

function KeyRow({ k, canRevoke }: { k: KeyView; canRevoke: boolean }) {
  const { wallet, status, refresh } = useUnlocked();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mine = k.key_id === wallet.keyId;
  const height = status?.height ?? 0;
  const bt = status?.block_time_ms ?? 1000;

  const revoke = async () => {
    const msg = mine
      ? "Revoke THIS device's key? You'll lose access from here immediately."
      : `Revoke “${k.label}”? It will stop working on the next block.`;
    if (!confirm(msg)) return;
    setBusy(true);
    setError(null);
    try {
      await wallet.removeKey(k.key_id);
      await refresh();
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  let detail = null;
  if (k.role.type === "session") {
    const r = k.role;
    const windowOver = height >= k.window_start + r.period_blocks;
    const spent = windowOver ? 0n : BigInt(k.spent_this_period);
    const expired = height > r.expires_at;
    detail = (
      <span className="dim small block">
        {formatFang(r.limit)} per {blocksToHuman(r.period_blocks, bt)} · spent {formatFang(spent)} this period ·{" "}
        {expired ? <span className="warn">expired at block #{r.expires_at}</span> : <>expires block #{r.expires_at} (in ~{blocksToHuman(r.expires_at - height, bt).replace(/^day$/, "1 day")})</>}
      </span>
    );
  }

  return (
    <div className="key-row">
      <div className="grow">
        <span className="key-label">{k.label}</span> {mine && <span className="pill">this device</span>}
        <span className="dim small block mono">{keyCode(k.key_id)} · added {k.added_at ? `at block #${k.added_at}` : "at creation"}</span>
        {detail}
        <ErrorNote error={error} />
      </div>
      {canRevoke && (
        <button className="btn small danger" disabled={busy} onClick={revoke}>
          {busy ? <Spinner /> : "Revoke"}
        </button>
      )}
    </div>
  );
}

function AddDevice() {
  const { wallet, refresh } = useUnlocked();
  const [id, setId] = useState("");
  const [label, setLabel] = useState("My phone");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<TxView | null>(null);
  const key = id.trim().toLowerCase();
  const valid = isKeyId(key);

  const add = async () => {
    setBusy(true);
    setError(null);
    try {
      const tx = (await wallet.addOwnerKey(key, label.trim() || "Another device")) as TxView;
      setDone(tx);
      setId("");
      await refresh();
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="card stack" id="device">
      <h2>Link a device</h2>
      <ol className="steps">
        <li>On the new device, open this wallet and choose <b>Link this device to a wallet on another device</b>.</li>
        <li>It shows a key id. Paste it here.</li>
        <li>Check the code matches, then add it. The new device opens automatically.</li>
      </ol>
      <label className="field">
        <span>New device&apos;s key id</span>
        <input value={id} onChange={(e) => setId(e.target.value)} placeholder="64 hex characters" className="mono" autoCapitalize="none" spellCheck={false} />
      </label>
      {key && (valid ? <span className="ok">Check code: <b className="code inline">{keyCode(key)}</b></span> : <span className="warn">A key id is 64 characters of 0–9 and a–f.</span>)}
      <label className="field">
        <span>Name it</span>
        <input value={label} onChange={(e) => setLabel(e.target.value)} maxLength={64} />
      </label>
      <button className="btn primary" disabled={!valid || busy} onClick={add}>
        {busy ? <Spinner>Adding…</Spinner> : "Add as owner device"}
      </button>
      <ErrorNote error={error} />
      {done && <p className="ok">✓ {done.summary}. <ExplorerLink href={explorer.tx(done.id)}>View ↗</ExplorerLink></p>}
      <Tip>The new device gets full control, just like this one. That&apos;s your backup: if you lose one device, the other still works, and you can revoke the lost one here.</Tip>
    </section>
  );
}

function AddAgent() {
  const { wallet, account, status, refresh } = useUnlocked();
  const [id, setId] = useState("");
  const [label, setLabel] = useState("claude");
  const [limit, setLimit] = useState("10");
  const [days, setDays] = useState("30");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<{ tx: TxView; limit: string; days: number } | null>(null);
  const key = id.trim().toLowerCase();
  const nDays = Number(days);
  let limitOk = true;
  try {
    limitOk = parseFang(limit) > 0n;
  } catch {
    limitOk = false;
  }
  const valid = isKeyId(key) && limitOk && Number.isInteger(nDays) && nDays > 0 && nDays <= 365;
  const me = account?.names[0] ?? wallet.account;
  // A "day" in blocks. A fixed constant: block timing reported by the node isn't trusted here.
  const day = BLOCKS_PER_DAY;

  const add = async () => {
    setBusy(true);
    setError(null);
    try {
      const tx = (await wallet.addSessionKey(key, label.trim() || "agent", { limit, periodBlocks: day, expiresInBlocks: nDays * day })) as TxView;
      setDone({ tx, limit, days: nDays });
      setId("");
      await refresh();
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  const snippet = `Your Blockula key may now spend from ${me}
(up to ${done?.limit ?? limit} FANG per day, sends only, for ${done?.days ?? nDays} days).

MCP:  call use_account with account "${me}"
SDK:  await wallet.actAs("${me}");
      await wallet.send("friend.boo", "1");`;

  return (
    <section className="card stack" id="agent">
      <h2>Give an agent a budget</h2>
      <p className="dim">
        Let an AI agent pay for things without handing over your wallet. It gets its own key, a daily limit and an expiry date, and can only send FANG.
      </p>
      <ol className="steps">
        <li>In your agent (e.g. Claude Code with the Blockula MCP), run <code>create_wallet</code>.</li>
        <li>Copy the <code>key_id</code> it prints and paste it here.</li>
      </ol>
      <label className="field">
        <span>Agent&apos;s key id</span>
        <input value={id} onChange={(e) => setId(e.target.value)} placeholder="64 hex characters" className="mono" autoCapitalize="none" spellCheck={false} />
      </label>
      {key && !isKeyId(key) && <span className="warn">A key id is 64 characters of 0–9 and a–f.</span>}
      <div className="grid-2">
        <label className="field">
          <span>Label</span>
          <input value={label} onChange={(e) => setLabel(e.target.value)} maxLength={64} />
        </label>
        <label className="field">
          <span>Daily limit</span>
          <div className="suffix-input">
            <input value={limit} onChange={(e) => setLimit(e.target.value)} inputMode="decimal" />
            <span className="suffix">FANG</span>
          </div>
        </label>
        <label className="field">
          <span>Expires after</span>
          <div className="suffix-input">
            <input value={days} onChange={(e) => setDays(e.target.value)} inputMode="numeric" />
            <span className="suffix">days</span>
          </div>
        </label>
      </div>
      <button className="btn primary" disabled={!valid || busy} onClick={add}>
        {busy ? <Spinner>Granting…</Spinner> : `Allow ${limitOk ? limit : "…"} FANG/day`}
      </button>
      {valid && (
        <p className="dim small">
          You&apos;ll sign: key <span className="mono">{key.slice(0, 12)}…</span> may send at most {limit} FANG (amount + fees) every {BLOCKS_PER_DAY.toLocaleString()} blocks (~1 day), transfers only, until {nDays} days of blocks from now. Revoke it any time.
        </p>
      )}
      <ErrorNote error={error} />
      {done && (
        <div className="stack">
          <p className="ok">✓ {done.tx.summary}. <ExplorerLink href={explorer.tx(done.tx.id)}>View ↗</ExplorerLink></p>
          <span className="dim small">Tell your agent:</span>
          <pre className="snippet">{snippet}</pre>
          <Copy text={snippet} label="Copy for your agent" />
        </div>
      )}
      <Tip>The limit counts the amount plus fees and resets every {day.toLocaleString()} blocks (about a day). Revoke any time above; it stops working on the very next block.</Tip>
    </section>
  );
}
