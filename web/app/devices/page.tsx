"use client";

import { decodeKeyGrant, formatFang, parseFang, type KeyGrant } from "@blockula/sdk";
import Link from "next/link";
import { useEffect, useState } from "react";
import { RequireWallet } from "@/components/Gate";
import { RecoveryKitSetup } from "@/components/RecoveryKit";
import { Apparition } from "@/components/Haunt";
import { QrCode } from "@/components/QrCode";
import { Copy, ErrorNote, ExplorerLink, Spinner, Tip } from "@/components/ui";
import { BLOCKS_PER_DAY, DEFAULT_NETWORK, explorer, NETWORK, walletUrl } from "@/lib/config";
import { friendly } from "@/lib/errors";
import { blocksToHuman, keyCode } from "@/lib/format";
import { deviceLink } from "@/lib/paylink";
import { KIT_LABEL } from "@/lib/recovery";
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
  // Keys revoked while this page is open, laid to rest under a gravestone.
  const [buried, setBuried] = useState<string[]>([]);
  if (!account) return <main className="narrow"><Spinner>Loading…</Spinner></main>;
  const owners = account.keys.filter((k) => k.role.type === "owner");
  const devices = owners.filter((k) => k.label !== KIT_LABEL);
  const sessions = account.keys.filter((k) => k.role.type === "session");
  const bury = (label: string) => setBuried((b) => [label, ...b]);
  return (
    <main className="narrow stack-lg">
      <Link href="/" className="back">← Home</Link>
      {/* The agents' Familiar. */}
      <Apparition kind="cat" />
      <h1>Devices &amp; agents</h1>
      <p className="dim">
        Your account isn&apos;t one key: it&apos;s a list of keys, each with its own permissions. Owners can do anything. Agents (session keys) can only send FANG, within a limit, until they expire.
      </p>

      <section className="card">
        <h2>Owner devices</h2>
        {devices.map((k) => <KeyRow key={k.key_id} k={k} canRevoke={owners.length > 1} onRevoked={bury} />)}
      </section>

      <RecoverySection kits={owners.filter((k) => k.label === KIT_LABEL)} canRevoke={owners.length > 1} onRevoked={bury} />

      <section className="card">
        <h2>Agents with a budget</h2>
        {sessions.length ? sessions.map((k) => <KeyRow key={k.key_id} k={k} canRevoke onRevoked={bury} />) : <p className="dim">No agents yet.</p>}
      </section>

      {buried.length > 0 && (
        <section className="card center graveyard">
          <Apparition kind="grave" text={`R.I.P.\n${buried[0].length > 12 ? `${buried[0].slice(0, 11)}.` : buried[0]}`} />
          <h2>Laid to rest</h2>
          <p className="dim">{buried.join(", ")} can no longer sign for this account.</p>
        </section>
      )}

      <AddDevice />
      <AddAgent />
    </main>
  );
}

function KeyRow({ k, canRevoke, onRevoked }: { k: KeyView; canRevoke: boolean; onRevoked: (label: string) => void }) {
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
      onRevoked(k.label);
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

/** The account's recovery kits (owner keys kept on paper), a kit waiting for fee money, and making a new one. */
function RecoverySection({ kits, canRevoke, onRevoked }: { kits: KeyView[]; canRevoke: boolean; onRevoked: (label: string) => void }) {
  const { wallet, account, hint, updateHint, refresh } = useUnlocked();
  const [making, setMaking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pending = hint.pendingKit && !account?.keys.some((k) => k.key_id === hint.pendingKit?.keyId) ? hint.pendingKit : null;

  const activate = async () => {
    if (!pending) return;
    setBusy(true);
    setError(null);
    try {
      // Kits made before kits carried their grant can't be added without the words: make a new one.
      if (!pending.grant) throw new Error("This kit was made before kits carried their consent to your account, so it can't be added from here. Make a new kit and forget this one.");
      await wallet.addOwnerKey(pending.grant, KIT_LABEL);
      updateHint({ pendingKit: undefined });
      await refresh();
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <section className="card stack" id="recovery">
        <h2>Recovery kit</h2>
        {kits.length ? (
          kits.map((k) => <KeyRow key={k.key_id} k={k} canRevoke={canRevoke} onRevoked={onRevoked} />)
        ) : (
          <p className="dim">No recovery kit yet. If every device and passkey were lost, nothing could get you back in.</p>
        )}
        {pending && !making && (
          <div className="stack warn-card card" data-testid="kit-pending">
            <p>
              The kit you made on {pending.created} (key <span className="mono">{keyCode(pending.keyId)}</span>) isn&apos;t on your account yet. Adding it needs a tiny fee.
            </p>
            <button className="btn primary" disabled={busy} onClick={activate}>{busy ? <Spinner>Activating…</Spinner> : "Activate my kit"}</button>
            <button className="link" onClick={() => updateHint({ pendingKit: undefined })}>I lost it: forget this kit</button>
            <ErrorNote error={error} />
          </div>
        )}
        {!making && <button className="btn" onClick={() => setMaking(true)}>{kits.length ? "Make a new kit" : "Make a recovery kit"}</button>}
        <Tip>A kit is 24 words on paper that act as an owner key. Made a new one? Revoke the old kit above, so a lost copy stops working.</Tip>
      </section>
      {making && <RecoveryKitSetup onFinish={() => setMaking(false)} onSkip={() => setMaking(false)} skipLabel="Cancel" />}
    </>
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

function AddDevice() {
  const { wallet, client, status, refresh } = useUnlocked();
  const [id, setId] = useState("");
  const [label, setLabel] = useState("My phone");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<TxView | null>(null);
  const [canShare, setCanShare] = useState(false);
  useEffect(() => setCanShare(typeof navigator.share === "function"), []);
  // The new device's grant: its consent to joining this account (audit gap-7-2), not just its key id.
  const { grant, problem } = parseGrant(id, wallet.account);
  const valid = grant !== null;
  // Opens /link on the new device with this account filled in. Like a payment link it says which
  // chain it's for, a check there, never a switch: complete once the node's status has filled in
  // what configuration didn't.
  const chain = client.pin.chainId && (client.pin.genesisHash || status) ? client.pin : null;
  const link = chain ? deviceLink(walletUrl(), wallet.account, chain) : null;

  const add = async () => {
    if (!grant) return;
    setBusy(true);
    setError(null);
    try {
      const tx = (await wallet.addOwnerKey(grant, label.trim() || "Another device")) as TxView;
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
      <p className="dim">Add your phone, tablet or another computer as a co-owner. It makes its own passkey, so nothing secret leaves this device.</p>
      <ol className="steps">
        <li>On the new device, scan this code with the camera, or open the link there.</li>
        <li>It makes a passkey and shows a grant code. Send that code here (AirDrop, or a message or email to yourself: it holds no secret) and paste it below.</li>
        <li>Check the codes match, then add it. The new device opens your wallet by itself.</li>
      </ol>
      {link ? (
        <>
          <div className="qr-frame">
            <QrCode text={link} label="QR code: link a new device to this account" />
          </div>
          <div className="row-gap center-row">
            <Copy text={link} label="Copy link" />
            {canShare && (
              <button type="button" className="btn small ghost" onClick={() => navigator.share({ url: link }).catch(() => {})}>
                Share…
              </button>
            )}
          </div>
        </>
      ) : (
        <Spinner>Making your device link…</Spinner>
      )}
      <p className="dim small">
        No camera? On the new device, open {new URL(walletUrl()).host}, choose <b>I already have a wallet → Link this device</b>, and enter this address:{" "}
        <span className="mono break" data-testid="device-account">{wallet.account}</span> <Copy text={wallet.account} label="Copy address" />
      </p>
      {NETWORK.key !== DEFAULT_NETWORK.key && (
        <span className="warn small">You&apos;re on {NETWORK.label}. Switch the new device to {NETWORK.label} too (the header menu) before you open the link there.</span>
      )}
      <hr className="rule" />
      <label className="field">
        <span>New device&apos;s grant code</span>
        <textarea value={id} onChange={(e) => setId(e.target.value)} placeholder="bkgrant1.…" className="mono" autoCapitalize="none" spellCheck={false} rows={3} />
      </label>
      {id.trim() && (grant ? <span className="ok">Check code: <b className="code inline">{keyCode(grant.key_id)}</b> (the new device shows the same)</span> : <span className="warn">{problem}</span>)}
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
  // The agent's grant: its consent to spending from this account (audit gap-7-2).
  const { grant, problem } = parseGrant(id, wallet.account);
  const nDays = Number(days);
  let limitOk = true;
  try {
    limitOk = parseFang(limit) > 0n;
  } catch {
    limitOk = false;
  }
  const valid = grant !== null && limitOk && Number.isInteger(nDays) && nDays > 0 && nDays <= 365;
  // The agent is told the account's address, the wallet's own (not a node's answer): on mainnet an
  // agent won't act for a name only one node vouched for (the SDK's actAs refuses it, the MCP's
  // use_account asks to confirm the address), so the name is only a label (review-r3-clients-4).
  const me = wallet.account;
  const name = account?.names[0];
  const who = name ? `${name} (${me})` : me;
  // A "day" in blocks. A fixed constant: block timing reported by the node isn't trusted here.
  const day = BLOCKS_PER_DAY;

  const add = async () => {
    if (!grant) return;
    setBusy(true);
    setError(null);
    try {
      const tx = (await wallet.addSessionKey(grant, label.trim() || "agent", { limit, periodBlocks: day, expiresInBlocks: nDays * day })) as TxView;
      setDone({ tx, limit, days: nDays });
      setId("");
      await refresh();
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  const snippet = `Your Blockula key may now spend from ${who}
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
        <li>
          Tell your agent (e.g. Claude Code with the Blockula MCP) to <code>use_account</code> with this account&apos;s address
          {name ? <> ({name})</> : null}: <span className="mono break" data-testid="agent-account">{me}</span> <Copy text={me} label="Copy address" />
        </li>
        <li>It answers with a <code>grant_code</code> (its key&apos;s consent to this account). Paste it here. If it asks you to confirm an address instead, it should be exactly the one above.</li>
      </ol>
      <label className="field">
        <span>Agent&apos;s grant code</span>
        <textarea value={id} onChange={(e) => setId(e.target.value)} placeholder="bkgrant1.…" className="mono" autoCapitalize="none" spellCheck={false} rows={3} />
      </label>
      {id.trim() && !grant && <span className="warn">{problem}</span>}
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
          You&apos;ll sign: key <span className="mono">{grant?.key_id.slice(0, 12)}…</span> (check code {grant ? keyCode(grant.key_id) : ""}) may send at most {limit} FANG (amount + fees) every {BLOCKS_PER_DAY.toLocaleString()} blocks (~1 day), transfers only, until {nDays} days of blocks from now. Revoke it any time.
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
