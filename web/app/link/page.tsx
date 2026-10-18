"use client";

import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { encodeKeyGrant, isAddress, type Resolution } from "@blockula/sdk";
import { Suspense, useEffect, useState, type ReactNode } from "react";
import { Splash, Unlock } from "@/components/Gate";
import { Apparition } from "@/components/Haunt";
import { knownChain } from "@/components/PayLinkNotice";
import { Copy, ErrorNote, Spinner, Tip } from "@/components/ui";
import { localKeysAllowed, MAINNET_CHAIN_ID, NETWORK } from "@/lib/config";
import { friendly, isPrfUnsupported } from "@/lib/errors";
import { keyCode } from "@/lib/format";
import { createLocalKey, createPasskey, type AccountChoice } from "@/lib/keys";
import { checkLinkChain, linkChain } from "@/lib/paylink";
import type { AccountView } from "@/lib/types";
import { useWallet } from "@/lib/wallet";

export default function LinkPage() {
  return (
    <Suspense fallback={<Splash />}>
      <LinkDevice />
    </Suspense>
  );
}

/**
 * The account a device link (lib/paylink.ts `deviceLink`, the QR code on the other device) asks
 * this device to join: an address, checked to be this chain's like a payment link's chain
 * (`checkLinkChain`), or why it can't be used. The user still checks it before this device signs
 * its consent: a link is anyone's text.
 */
function useDeviceLink(): { address: string | null; problem: ReactNode | null } {
  const params = useSearchParams();
  const { client, status } = useWallet();
  const raw = params.get("account")?.trim().toLowerCase();
  if (!raw) return { address: null, problem: null };
  const check = checkLinkChain(linkChain(params), client.pin);
  if (check.state === "mismatch") {
    const known = check.why === "chain" ? knownChain(linkChain(params)?.chainId ?? "") : null;
    return {
      address: null,
      problem:
        check.why === "chain" && known ? (
          <>This link was made on {known.label}, but this wallet is on {NETWORK.label}.{known.inMenu ? <> Switch networks in the header menu, then open the link again.</> : null}</>
        ) : check.why === "chain" ? (
          <>This link was made for a network this wallet doesn&apos;t know, not {NETWORK.label}.</>
        ) : check.why === "genesis" ? (
          <>This link was made for another instance of {NETWORK.label}, not the one this wallet is on.</>
        ) : (
          <>This link names its network in a form no Blockula network uses.</>
        ),
    };
  }
  // Until the node has said which chain (and prefix) it is, the link waits.
  if (check.state === "pending" || !status) return { address: null, problem: null };
  if (!isAddress(raw, client.prefix)) return { address: null, problem: <>This link&apos;s account isn&apos;t a {NETWORK.label} address.</> };
  return { address: raw, problem: null };
}

/**
 * The NEW device's side of linking: make a key, ask which account it joins (or take it from the
 * other device's link), show its grant code (the key's consent to that account, which adding it
 * needs: audit gap-7-2), and wait until the account's own key list has it. Never a list of
 * accounts from /api/key, which isn't this device's to trust before it's on one.
 */
function LinkDevice() {
  const { ready, hint, wallet, client, unlocked, forget } = useWallet();
  const router = useRouter();
  const linked = useDeviceLink();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [prf, setPrf] = useState(false);
  const [target, setTarget] = useState("");
  /** The user chose to type an account instead of the one the link names. */
  const [manual, setManual] = useState(false);
  const [joining, setJoining] = useState<{ res: Resolution; code: string } | null>(null);
  /** An account name only the node vouched for, on mainnet: its address waits for the user to check it (audit gap-7-1). */
  const [checking, setChecking] = useState<Resolution | null>(null);
  const [matches, setMatches] = useState(false);
  const [canShare, setCanShare] = useState(false);
  useEffect(() => setCanShare(typeof navigator.share === "function"), []);

  const waiting = !!wallet && !!hint && !hint.account;

  const finish = async (c: AccountChoice) => {
    if (!wallet || !hint) return;
    await wallet.actAs(c.address);
    unlocked(wallet, { ...hint, account: c.address, name: c.name ?? undefined });
    router.replace("/");
  };

  /** Signs this device's consent to joining `res.address` (an address the user typed, proved, or checked). */
  const grant = async (res: Resolution) => {
    if (!wallet) return;
    setBusy(true);
    setError(null);
    try {
      setJoining({ res, code: encodeKeyGrant(await wallet.keyGrant(res.address)) });
      setChecking(null);
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  const grantFor = async (input: string) => {
    if (!wallet) return;
    setBusy(true);
    setError(null);
    try {
      const res = await client.resolution(input.trim());
      // The grant names an address, so a name has to become one. Proven by the network's
      // witnesses, or typed as an address, it's signed for; on mainnet a name only the node vouched
      // for waits until the user has checked the address against their other device.
      const mainnet = NETWORK.mainnet === true || client.pin.chainId === MAINNET_CHAIN_ID;
      if (!res.proven && mainnet) {
        setMatches(false);
        setChecking(res);
        setBusy(false);
        return;
      }
      await grant(res);
    } catch (e) {
      setError(friendly(e));
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

  const startOver = () => {
    forget();
    router.replace("/welcome");
  };

  if (!ready) return <Splash />;
  if (hint && !wallet) return <Unlock />;

  if (wallet && hint?.account)
    return (
      <main className="narrow center stack-lg">
        <Apparition kind="key" />
        <h1>This device is already set up</h1>
        <p className="dim">
          {linked.address === hint.account
            ? "It's already linked to that account."
            : `It acts for ${hint.name ?? "your account"}. To link another device to it, open Devices & agents.`}
        </p>
        <Link className="btn primary" href="/">Go home</Link>
      </main>
    );

  if (waiting && wallet && !joining && checking)
    return (
      <main className="narrow stack-lg">
        <div className="center">
          <Apparition kind="candle" />
          <h1>Check your account&apos;s address</h1>
          <p className="dim">Your node says {checking.name ? <b>{checking.name}.boo</b> : "that account"} is the address below, but nothing proved it, and a node can point a name anywhere. This device will only agree to join that address.</p>
        </div>
        <section className="card stack" data-testid="check-address">
          <span className="eyebrow">Your node says</span>
          <div className="mono address break">{checking.address}</div>
          <p className="small">On your other device, open <b>Devices &amp; agents → Link a device</b>: it shows your account&apos;s address. Compare every character.</p>
          <label className="row-gap small">
            <input type="checkbox" checked={matches} onChange={(e) => setMatches(e.target.checked)} />
            <span>It&apos;s exactly the address my other device shows</span>
          </label>
          <button className="btn primary" disabled={!matches || busy} onClick={() => grant(checking)}>{busy ? <Spinner>Signing…</Spinner> : "It matches: make the grant code"}</button>
          <ErrorNote error={error} />
        </section>
        <button className="link center-self" onClick={() => setChecking(null)}>It doesn&apos;t match: pick another account</button>
      </main>
    );

  // Opened from the other device's link: its account, for the user to confirm.
  if (waiting && wallet && !joining && linked.address && !manual)
    return (
      <main className="narrow stack-lg">
        <div className="center">
          <Apparition kind="candle" />
          <h1>Join this account?</h1>
          <p className="dim">This is the account from your other device&apos;s link. It should match the address under <b>Link a device</b> there.</p>
        </div>
        <section className="card stack" data-testid="link-account">
          <span className="eyebrow">Your account</span>
          <div className="mono address break">{linked.address}</div>
          <button className="btn primary big" disabled={busy} onClick={() => grantFor(linked.address as string)}>{busy ? <Spinner>Signing…</Spinner> : "Make this device's grant code"}</button>
          <ErrorNote error={error} />
        </section>
        <Tip>This device signs its consent to joining that account, and only that one. Nobody else can put this device on their account.</Tip>
        <button className="link center-self" onClick={() => setManual(true)}>That&apos;s not my account: enter it myself</button>
        <button className="link center-self" onClick={startOver}>Cancel and start over</button>
      </main>
    );

  if (waiting && wallet && !joining)
    return (
      <main className="narrow stack-lg">
        <div className="center">
          <Apparition kind="candle" />
          <h1>Which account?</h1>
          <p className="dim">Enter the .boo name or address of the account this device should join. On your other device it&apos;s under <b>Devices &amp; agents → Link a device</b>.</p>
        </div>
        <section className="card stack">
          <label className="field">
            <span>Your account</span>
            <input value={target} onChange={(e) => setTarget(e.target.value)} placeholder="yourname.boo or boo1…" autoCapitalize="none" spellCheck={false} />
          </label>
          <button className="btn primary" disabled={!target.trim() || busy} onClick={() => grantFor(target)}>{busy ? <Spinner>Signing…</Spinner> : "Make this device's grant code"}</button>
          <ErrorNote error={error} />
        </section>
        <Tip>This device signs its consent to joining that account, and only that one. Nobody else can put this device on their account.</Tip>
        <button className="link center-self" onClick={startOver}>Cancel and start over</button>
      </main>
    );

  if (waiting && wallet && joining)
    return (
      <main className="narrow stack-lg">
        <div className="center">
          <Apparition kind="candle" />
          <h1>Almost there</h1>
          <p className="dim">Send this grant code to the device that already has your wallet, and paste it there under <b>Devices &amp; agents → Link a device</b>. This page opens your wallet once it&apos;s added.</p>
        </div>
        <section className="card stack">
          <span className="eyebrow">Joining</span>
          <div className="mono address">{joining.res.name ? `${joining.res.name}.boo · ` : ""}{joining.res.address}</div>
          {!joining.res.proven && <span className="warn small">Looked up on your node only, not proven: check this address on your other device.</span>}
          <span className="eyebrow">This device&apos;s grant code</span>
          <div className="mono address small">{joining.code.slice(0, 48)}…</div>
          <div className="row-gap">
            <Copy text={joining.code} label="Copy grant code" />
            {canShare && (
              <button type="button" className="btn small ghost" onClick={() => navigator.share({ text: joining.code }).catch(() => {})}>
                Share…
              </button>
            )}
          </div>
          <span className="eyebrow">Check code (should match on the other device)</span>
          <div className="code">{keyCode(wallet.keyId)}</div>
        </section>
        <p className="center"><Spinner>Waiting to be added…</Spinner></p>
        <Tip>
          The grant code holds this device&apos;s public key and its signed consent to joining this one account. It&apos;s safe to AirDrop, message or email to yourself: it lets your other device <i>grant</i> access, but can&apos;t spend anything by itself.
        </Tip>
        <button className="link center-self" onClick={() => setJoining(null)}>Pick another account</button>
        <button className="link center-self" onClick={startOver}>Cancel and start over</button>
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
      {linked.problem && <div className="banner danger" role="alert" data-testid="device-link-refused">{linked.problem} You can still enter your account by hand.</div>}
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
      {!linked.address && !linked.problem && (
        <Tip>Quickest: on the device that has your wallet, open <b>Devices &amp; agents → Link a device</b> and scan its code with this device&apos;s camera.</Tip>
      )}
    </main>
  );
}
