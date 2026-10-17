"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { encodeKeyGrant, type Resolution } from "@blockula/sdk";
import { useEffect, useState } from "react";
import { Splash, Unlock } from "@/components/Gate";
import { Ornament } from "@/components/Ornament";
import { Copy, ErrorNote, Spinner, Tip } from "@/components/ui";
import { localKeysAllowed } from "@/lib/config";
import { friendly, isPrfUnsupported } from "@/lib/errors";
import { keyCode } from "@/lib/format";
import { createLocalKey, createPasskey, type AccountChoice } from "@/lib/keys";
import type { AccountView } from "@/lib/types";
import { useWallet } from "@/lib/wallet";

/**
 * The NEW device's side of linking: make a key, ask which account it joins, show its grant code
 * (the key's consent to that account, which adding it needs: audit gap-7-2), and wait until the
 * account's own key list has it. Never a list of accounts from /api/key, which isn't this
 * device's to trust before it's on one.
 */
export default function LinkPage() {
  const { ready, hint, wallet, client, unlocked, forget } = useWallet();
  const router = useRouter();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [prf, setPrf] = useState(false);
  const [target, setTarget] = useState("");
  const [joining, setJoining] = useState<{ res: Resolution; code: string } | null>(null);

  const waiting = !!wallet && !!hint && !hint.account;

  const finish = async (c: AccountChoice) => {
    if (!wallet || !hint) return;
    await wallet.actAs(c.address);
    unlocked(wallet, { ...hint, account: c.address, name: c.name ?? undefined });
    router.replace("/");
  };

  const grantFor = async () => {
    if (!wallet) return;
    setBusy(true);
    setError(null);
    try {
      const res = await client.resolution(target.trim());
      setJoining({ res, code: encodeKeyGrant(await wallet.keyGrant(res.address)) });
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  // Poll the chosen account's own key list until an owner adds this key there.
  useEffect(() => {
    if (!waiting || !wallet || !joining) return;
    let live = true;
    const tick = async () => {
      try {
        const a = (await client.account(joining.res.address)) as AccountView;
        const k = a.keys.find((x) => x.key_id === wallet.keyId);
        if (!live || !k) return;
        await finish({ address: joining.res.address, name: a.names[0] ?? null, short: a.account.short, role: k.role.type, label: k.label, balance: a.balance_display });
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
  }, [waiting, wallet, joining, client]);

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

  if (wallet && hint?.account)
    return (
      <main className="narrow center stack-lg">
        <Ornament kind="key" className="hero-ornament" />
        <h1>This device is already set up</h1>
        <p className="dim">It acts for {hint.name ?? "your account"}. To link another device, open Devices &amp; agents.</p>
        <Link className="btn primary" href="/">Go home</Link>
      </main>
    );

  if (waiting && wallet && !joining)
    return (
      <main className="narrow stack-lg">
        <div className="center">
          <Ornament kind="candle" className="hero-ornament" />
          <h1>Which account?</h1>
          <p className="dim">Enter the .boo name or address of the account this device should join. On your other device it&apos;s at the top of <b>Devices &amp; agents → Link a device</b>.</p>
        </div>
        <section className="card stack">
          <label className="field">
            <span>Your account</span>
            <input value={target} onChange={(e) => setTarget(e.target.value)} placeholder="yourname.boo or boo1…" autoCapitalize="none" spellCheck={false} />
          </label>
          <button className="btn primary" disabled={!target.trim() || busy} onClick={grantFor}>{busy ? <Spinner>Signing…</Spinner> : "Make this device's grant code"}</button>
          <ErrorNote error={error} />
        </section>
        <Tip>This device signs its consent to joining that account, and only that one. Nobody else can put this device on their account.</Tip>
        <button className="link center-self" onClick={() => { forget(); router.replace("/welcome"); }}>Cancel and start over</button>
      </main>
    );

  if (waiting && wallet && joining)
    return (
      <main className="narrow stack-lg">
        <div className="center">
          <Ornament kind="candle" className="hero-ornament" />
          <h1>Almost there</h1>
          <p className="dim">On a device that already has your wallet, open <b>Devices &amp; agents → Link a device</b> and paste this grant code.</p>
        </div>
        <section className="card stack">
          <span className="eyebrow">Joining</span>
          <div className="mono address">{joining.res.name ? `${joining.res.name}.boo · ` : ""}{joining.res.address}</div>
          {!joining.res.proven && <span className="warn small">Looked up on your node only, not proven: check this address on your other device.</span>}
          <span className="eyebrow">This device&apos;s grant code</span>
          <div className="mono address small">{joining.code.slice(0, 48)}…</div>
          <Copy text={joining.code} label="Copy grant code" />
          <span className="eyebrow">Check code (should match on the other device)</span>
          <div className="code">{keyCode(wallet.keyId)}</div>
        </section>
        <p className="center"><Spinner>Waiting to be added…</Spinner></p>
        <Tip>
          The grant code holds this device&apos;s public key and its signed consent to joining this one account. It&apos;s safe to send over chat or email: it lets your other device <i>grant</i> access, but can&apos;t spend anything by itself.
        </Tip>
        <button className="link center-self" onClick={() => setJoining(null)}>Pick another account</button>
        <button className="link center-self" onClick={() => { forget(); router.replace("/welcome"); }}>Cancel and start over</button>
      </main>
    );

  return (
    <main className="narrow stack-lg">
      <Link href="/welcome" className="back">← Back</Link>
      <div className="center">
        <Ornament kind="key" className="hero-ornament" />
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
          {localKeysAllowed(client.pin.chainId) && <button className="btn" onClick={() => make("local")}>Use a browser-only key (less secure, for testing)</button>}
        </section>
      )}
    </main>
  );
}
