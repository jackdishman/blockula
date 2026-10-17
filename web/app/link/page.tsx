"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useEffect, useState } from "react";
import { Splash, Unlock } from "@/components/Gate";
import { AccountPicker } from "@/components/AccountPicker";
import { Apparition } from "@/components/Haunt";
import { Copy, ErrorNote, Spinner, Tip } from "@/components/ui";
import { friendly, isPrfUnsupported } from "@/lib/errors";
import { keyCode } from "@/lib/format";
import { createLocalKey, createPasskey, findAccounts, type AccountChoice } from "@/lib/keys";
import { useWallet } from "@/lib/wallet";

/** The NEW device's side of linking: make a key, show its id, wait to be added. */
export default function LinkPage() {
  const { ready, hint, wallet, client, unlocked, forget } = useWallet();
  const router = useRouter();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [prf, setPrf] = useState(false);
  const [choices, setChoices] = useState<AccountChoice[] | null>(null);

  const waiting = !!wallet && !!hint && !hint.account;

  const finish = async (c: AccountChoice) => {
    if (!wallet || !hint) return;
    await wallet.actAs(c.address);
    unlocked(wallet, { ...hint, account: c.address, name: c.name ?? undefined });
    router.replace("/");
  };

  // Poll until an owner adds this key to their account.
  useEffect(() => {
    if (!waiting || !wallet || choices) return;
    let live = true;
    const tick = async () => {
      try {
        const list = (await findAccounts(client, wallet)).filter((c) => c.address !== wallet.address);
        if (!live || !list.length) return;
        if (list.length === 1) await finish(list[0]);
        else setChoices(list);
      } catch {
        /* node hiccup; keep polling */
      }
    };
    tick();
    const t = setInterval(tick, 2000);
    return () => {
      live = false;
      clearInterval(t);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [waiting, wallet, choices, client]);

  const make = async (kind: "passkey" | "local") => {
    setBusy(true);
    setError(null);
    try {
      const { wallet: w, hint: h } = kind === "passkey" ? await createPasskey(client, "Blockula (linked device)") : createLocalKey(client);
      unlocked(w, { ...h, account: undefined });
    } catch (e) {
      if (isPrfUnsupported(e)) setPrf(true);
      else setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  if (!ready) return <Splash />;
  if (hint && !wallet) return <Unlock />;
  if (choices) return <main className="narrow"><AccountPicker choices={choices} onPick={finish} /></main>;

  if (wallet && hint?.account)
    return (
      <main className="narrow center stack-lg">
        <Apparition kind="key" />
        <h1>This device is already set up</h1>
        <p className="dim">It acts for {hint.name ?? "your account"}. To link another device, open Devices &amp; agents.</p>
        <Link className="btn primary" href="/">Go home</Link>
      </main>
    );

  if (waiting && wallet)
    return (
      <main className="narrow stack-lg">
        <div className="center">
          <Apparition kind="candle" />
          <h1>Almost there</h1>
          <p className="dim">On a device that already has your wallet, open <b>Devices &amp; agents → Link a device</b> and paste this key id.</p>
        </div>
        <section className="card stack">
          <span className="eyebrow">This device&apos;s key id</span>
          <div className="mono address">{wallet.keyId}</div>
          <Copy text={wallet.keyId} label="Copy key id" />
          <span className="eyebrow">Check code (should match on the other device)</span>
          <div className="code">{keyCode(wallet.keyId)}</div>
        </section>
        <p className="center"><Spinner>Waiting to be added…</Spinner></p>
        <Tip>
          A key id is a fingerprint of this device&apos;s public key. It&apos;s safe to send over chat or email: it lets your other device <i>grant</i> access, but can&apos;t spend anything by itself.
        </Tip>
        <button className="link center-self" onClick={() => { forget(); router.replace("/welcome"); }}>Cancel and start over</button>
      </main>
    );

  return (
    <main className="narrow stack-lg">
      <Link href="/welcome" className="back">← Back</Link>
      <div className="center">
        <Apparition kind="key" />
        <h1>Link this device</h1>
        <p className="dim">
          This device gets its own passkey and key. Your other device then adds it as a co-owner of your account. Nothing secret ever leaves either device.
        </p>
      </div>
      {!prf ? (
        <section className="card stack">
          <button className="btn primary big" disabled={busy} onClick={() => make("passkey")}>
            {busy ? <Spinner>Waiting for your passkey…</Spinner> : "Create a passkey for this device"}
          </button>
          <ErrorNote error={error} />
        </section>
      ) : (
        <section className="card stack warn-card">
          <p>This passkey provider doesn&apos;t support <b>PRF</b>, which Blockula needs to derive your key. Try iCloud Keychain, Google Password Manager or 1Password.</p>
          <button className="btn primary" onClick={() => setPrf(false)}>← Try another passkey</button>
          <button className="btn" onClick={() => make("local")}>Use a browser-only key (less secure, for testing)</button>
        </section>
      )}
    </main>
  );
}
