"use client";

import { Wallet } from "@blockula/sdk";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useEffect, useRef, useState } from "react";
import { AccountPicker } from "@/components/AccountPicker";
import { Apparition } from "@/components/Haunt";
import { ErrorNote, ExplorerLink, Spinner, Tip } from "@/components/ui";
import { explorer, NETWORK } from "@/lib/config";
import { friendly } from "@/lib/errors";
import { keyCode } from "@/lib/format";
import { createPasskey, findAccounts, type AccountChoice } from "@/lib/keys";
import { KIT_LABEL, KIT_WORDS, phraseProblem, splitWords, wipeWallet, wordsToSeed } from "@/lib/recovery";
import type { Hint } from "@/lib/session";
import type { TxView } from "@/lib/types";
import { useWallet } from "@/lib/wallet";

type Stage =
  | { s: "enter" }
  | { s: "pick"; kit: Wallet; list: AccountChoice[] }
  | { s: "device"; kit: Wallet; account: AccountChoice; device?: { wallet: Wallet; hint: Hint } }
  | { s: "cleanup"; account: AccountChoice; kitKeyId: string; deviceKeyId: string; tx: TxView };

/**
 * Restore with a recovery kit: the kit's words become an owner key for one page, long enough to
 * add a passkey from this device to the account. Then the key is wiped and the new passkey takes
 * over (revoking the lost device, ideally replacing the kit).
 */
export default function RecoverPage() {
  const { client, unlocked } = useWallet();
  const router = useRouter();
  const [stage, setStage] = useState<Stage>({ s: "enter" });
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Whatever happens, the kit key doesn't outlive this page.
  const kitRef = useRef<Wallet | null>(null);
  useEffect(() => () => {
    if (kitRef.current) wipeWallet(kitRef.current);
  }, []);

  const typed = splitWords(text).length;
  const problem = typed >= KIT_WORDS ? phraseProblem(text) : null;

  const find = async () => {
    setBusy(true);
    setError(null);
    let kit: Wallet | null = null;
    try {
      kit = Wallet.fromSeed(wordsToSeed(text), client);
      kitRef.current = kit;
      const list = (await findAccounts(client, kit)).filter((a) => a.role === "owner" && a.address !== kit?.address);
      // The words have been read: off the page they go (kept only if the node couldn't be asked).
      setText("");
      if (!list.length) {
        wipeWallet(kit);
        setError(`This kit isn't an owner of any account on ${NETWORK.label}. Check the network at the top of the page, and that the kit was activated (Devices & agents → Recovery kit).`);
        return;
      }
      setStage(list.length === 1 ? { s: "device", kit, account: list[0] } : { s: "pick", kit, list });
    } catch (e) {
      if (kit) wipeWallet(kit);
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  const restore = async () => {
    if (stage.s !== "device") return;
    const { kit, account } = stage;
    setBusy(true);
    setError(null);
    try {
      // Keep the new passkey if a later step fails, so a retry doesn't make another one.
      const device = stage.device ?? (await createPasskey(client, `Blockula (${account.name ?? account.short})`));
      if (!stage.device) setStage({ ...stage, device });
      await kit.actAs(account.address);
      // The new passkey consents to joining the account (audit gap-7-2); the kit adds it.
      const tx = (await kit.addOwnerKey(await device.wallet.keyGrant(account.address), "Restored device")) as TxView;
      await device.wallet.actAs(account.address);
      unlocked(device.wallet, { ...device.hint, account: account.address, name: account.name ?? undefined });
      setStage({ s: "cleanup", account, kitKeyId: kit.keyId, deviceKeyId: device.wallet.keyId, tx });
      wipeWallet(kit);
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  if (stage.s === "pick")
    return (
      <main className="narrow">
        <AccountPicker choices={stage.list} onPick={(account) => setStage({ s: "device", kit: stage.kit, account })} />
      </main>
    );

  if (stage.s === "device")
    return (
      <main className="narrow stack-lg">
        <div className="center">
          <Apparition kind="key" />
          <span className="eyebrow">Kit accepted</span>
          <h1>Found {stage.account.name ?? stage.account.short}</h1>
          <p className="dim">
            Now make a passkey on this device. Your kit signs one transaction adding it to the account as an owner, and then this page forgets the kit.
          </p>
        </div>
        <section className="card stack">
          <button className="btn primary big" disabled={busy} onClick={restore}>
            {busy ? <Spinner>{stage.device ? "Adding this device…" : "Waiting for your passkey…"}</Spinner> : stage.device ? "Add this device to my account" : "Create a passkey for this device"}
          </button>
          <ErrorNote error={error} />
        </section>
      </main>
    );

  if (stage.s === "cleanup") return <Cleanup {...stage} onDone={() => router.replace("/")} />;

  return (
    <main className="narrow stack-lg">
      <Link href="/welcome" className="back">← Back</Link>
      <div className="center">
        <Apparition kind="candle" />
        <h1>Restore with a recovery kit</h1>
        <p className="dim">Type the {KIT_WORDS} words from your kit, in order. They stay in this browser: nothing you type here is sent anywhere.</p>
      </div>
      <section className="card stack">
        <label className="field">
          <span>Recovery words</span>
          <textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            rows={6}
            placeholder="word1 word2 word3 …"
            autoComplete="off"
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            data-testid="kit-input"
          />
        </label>
        <div className="name-status" aria-live="polite">
          {problem ? <span className="warn">{problem}</span> : typed > 0 && <span className="dim small">{typed} of {KIT_WORDS} words</span>}
        </div>
        <button className="btn primary" disabled={busy || typed !== KIT_WORDS || !!problem} onClick={find}>
          {busy ? <Spinner>Looking for your account…</Spinner> : "Find my account"}
        </button>
        <ErrorNote error={error} />
      </section>
      <Tip>No kit? If another device or passkey still opens your wallet, use it to link this one instead (Devices &amp; agents → Link a device).</Tip>
    </main>
  );
}

/** After restoring: revoke the lost device, and replace the kit whose words were just typed in. */
function Cleanup({ account, kitKeyId, deviceKeyId, tx, onDone }: { account: AccountChoice; kitKeyId: string; deviceKeyId: string; tx: TxView; onDone: () => void }) {
  const { wallet, account: view, refresh } = useWallet();
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const others = (view?.keys ?? []).filter((k) => k.role.type === "owner" && k.key_id !== deviceKeyId);

  const revoke = async (keyId: string) => {
    if (!wallet) return;
    setBusy(keyId);
    setError(null);
    try {
      await wallet.removeKey(keyId);
      await refresh();
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <main className="narrow stack-lg" data-testid="restore-done">
      <div className="center">
        <Apparition kind="moon" />
        <span className="eyebrow">Restored</span>
        <h1>Welcome back{account.name ? `, ${account.name}` : ""}</h1>
        <p className="ok">✓ {tx.summary}. <ExplorerLink href={explorer.tx(tx.id)}>View ↗</ExplorerLink></p>
      </div>
      <section className="card stack">
        <h2>Lock out what you lost</h2>
        <p className="dim">This device&apos;s passkey is an owner now. Revoke the device you lost, so whoever finds it can&apos;t use it.</p>
        {!view && <Spinner>Loading your keys…</Spinner>}
        {others.map((k) => (
          <div className="key-row" key={k.key_id}>
            <div className="grow">
              <span className="key-label">{k.label}</span> {k.key_id === kitKeyId && <span className="pill">the kit you just used</span>}
              <span className="dim small block mono">{keyCode(k.key_id)}</span>
            </div>
            <button className="btn small danger" disabled={!!busy} onClick={() => revoke(k.key_id)}>
              {busy === k.key_id ? <Spinner /> : "Revoke"}
            </button>
          </div>
        ))}
        <ErrorNote error={error} />
        <Tip>
          The {KIT_LABEL}&apos;s words have now been typed into a browser. Make a new kit from <Link href="/devices#recovery">Devices &amp; agents</Link>, then revoke this one.
        </Tip>
      </section>
      <button className="btn primary big center-self" onClick={onDone}>Open my wallet</button>
    </main>
  );
}
