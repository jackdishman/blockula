"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useEffect, useRef, useState } from "react";
import { Splash, useAccountResolution } from "@/components/Gate";
import { epitaph, NameClaim } from "@/components/NameClaim";
import { Apparition } from "@/components/Haunt";
import { RecoveryKitSetup } from "@/components/RecoveryKit";
import { ErrorNote, Spinner, Tip } from "@/components/ui";
import type { PropKind } from "@/lib/ascii/scenes";
import { season } from "@/lib/ascii/settings";
import { localKeysAllowed } from "@/lib/config";
import { friendly, isPrfUnsupported } from "@/lib/errors";
import { createLocalKey, createPasskey, unlockPasskey } from "@/lib/keys";
import type { Hint } from "@/lib/session";
import { useWallet } from "@/lib/wallet";

type Step = 0 | 1 | 2 | "start" | "have" | "prf" | "fund" | "kit" | "name" | "done";

const INTRO: { ornament: PropKind; chapter: string; title: string; body: string }[] = [
  {
    ornament: "bat",
    chapter: "Chapter I",
    title: "A wallet is a key, not a vault",
    body: "Your FANG lives on the Blockula blockchain, a public ledger kept by many computers. A wallet holds the secret key that proves which coins are yours and lets you move them.",
  },
  {
    ornament: "key",
    chapter: "Chapter II",
    title: "No seed phrase. Just your passkey.",
    body: "Face ID, Touch ID or your password manager unlocks your wallet. There's no seed phrase to type in, and this site never stores your key: your passkey re-creates it each time. A paper recovery kit is your backup if every device is lost.",
  },
  {
    ornament: "moon",
    chapter: "Chapter III",
    title: "Safe from tomorrow's quantum computers",
    body: "A large quantum computer could forge the signatures Bitcoin and Ethereum use. Blockula signs with ML-DSA-65, a NIST post-quantum standard. Your passkey only unlocks that key; it never signs anything on-chain.",
  },
];

export default function Welcome() {
  const { ready, hint, wallet, client, status, unlocked, refresh } = useWallet();
  const router = useRouter();
  const [step, setStep] = useState<Step>(0);
  const [nickname, setNickname] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [funded, setFunded] = useState<string | null>(null);
  const [claimed, setClaimed] = useState<string | null>(null);
  const [typed, setTyped] = useState("");
  const { resolve, picker } = useAccountResolution();
  const inFlow = useRef(false);

  // Already set up on this browser? Go home (unless we're mid-onboarding).
  useEffect(() => {
    if (ready && hint && !inFlow.current) router.replace("/");
  }, [ready, hint, router]);

  const start = async (w: Parameters<typeof unlocked>[0], h: Hint) => {
    inFlow.current = true;
    unlocked(w, { ...h, account: w.address });
    setStep("fund");
  };

  const create = async () => {
    setBusy("create");
    setError(null);
    try {
      const { wallet: w, hint: h } = await createPasskey(client, nickname.trim() || "Blockula wallet");
      await start(w, h);
    } catch (e) {
      if (isPrfUnsupported(e)) setStep("prf");
      else setError(friendly(e));
    } finally {
      setBusy(null);
    }
  };

  const existing = async () => {
    setBusy("existing");
    setError(null);
    try {
      const { wallet: w, credentialId } = await unlockPasskey(client);
      inFlow.current = true;
      await resolve(w, { v: 1, kind: "passkey", credentialId, keyId: w.keyId });
    } catch (e) {
      inFlow.current = false;
      setError(isPrfUnsupported(e) ? `${friendly(e)} A browser-only key can't open an existing passkey wallet.` : friendly(e));
    } finally {
      setBusy(null);
    }
  };

  const local = async () => {
    const { wallet: w, hint: h } = createLocalKey(client);
    await start(w, h);
  };

  // Step: auto-fund from the faucet if this chain has one (once; status refreshes every few seconds).
  const fundStarted = useRef(false);
  useEffect(() => {
    if (step !== "fund" || !wallet || !status || fundStarted.current) return;
    fundStarted.current = true;
    if (!status.faucet) {
      setStep("kit");
      return;
    }
    wallet
      .faucet()
      .then(async (tx) => {
        await refresh();
        setFunded(tx.amount_display ?? "100 FANG");
      })
      .catch((e) => setError(friendly(e)));
  }, [step, wallet, status, refresh]);

  if (!ready || (hint && !inFlow.current)) return <Splash />;
  if (picker) return <main className="narrow">{picker}</main>;

  // The ways into a wallet this browser hasn't seen: on the start screen and its own step.
  const haveWallet = (
    <>
      <button className="choice" disabled={!!busy} onClick={existing}>
        <span>
          <b>{busy === "existing" ? <Spinner>Waiting for your passkey…</Spinner> : "Open it with my passkey"}</b>
          <span className="dim small block">If your passkey synced here through iCloud Keychain, Google Password Manager or 1Password.</span>
        </span>
        <span aria-hidden>→</span>
      </button>
      <Link className="choice" href="/link">
        <span>
          <b>Link this device</b>
          <span className="dim small block">Your wallet is open on another device. There, open Devices &amp; agents → Link a device and scan its code with this device&apos;s camera, or start here.</span>
        </span>
        <span aria-hidden>→</span>
      </Link>
      <Link className="choice" href="/recover">
        <span>
          <b>Restore with a recovery kit</b>
          <span className="dim small block">The 24 words on paper, if no device opens your wallet any more.</span>
        </span>
        <span aria-hidden>→</span>
      </Link>
    </>
  );

  if (typeof step === "number") {
    const s = INTRO[step];
    return (
      <main className="narrow center onboarding">
        <Apparition kind={s.ornament} />
        <span className="eyebrow">{s.chapter}</span>
        <h1>{s.title}</h1>
        <p className="chapter-body">{s.body}</p>
        <div className="dots">{INTRO.map((_, i) => <span key={i} className={i === step ? "on" : ""} />)}</div>
        <button className="btn primary big" onClick={() => setStep(step < 2 ? ((step + 1) as Step) : "start")}>
          {step < 2 ? "Next" : "Let's go"}
        </button>
        <button className="link" onClick={() => setStep("start")}>Skip intro</button>
        <button className="link" onClick={() => setStep("have")}>I already have a wallet</button>
        <Link className="link small" href="/explore">Just looking? Explore the chain first</Link>
      </main>
    );
  }

  if (step === "start")
    return (
      <main className="narrow stack-lg">
        <div className="center">
          <span className="eyebrow">Your wallet</span>
          <h1>Create your wallet</h1>
          <p className="dim">One tap with your passkey. Takes about five seconds.</p>
        </div>
        <section className="card stack">
          <label className="field">
            <span>Passkey nickname <span className="dim">(optional, only you see this)</span></span>
            <input value={nickname} onChange={(e) => setNickname(e.target.value)} placeholder="Blockula wallet" />
          </label>
          <button className="btn primary big" disabled={!!busy} onClick={create}>
            {busy === "create" ? <Spinner>Waiting for your passkey…</Spinner> : "Create wallet with passkey"}
          </button>
          <ErrorNote error={error} />
        </section>
        <section className="card stack">
          <h2>Already have a wallet?</h2>
          {haveWallet}
        </section>
      </main>
    );

  if (step === "have")
    return (
      <main className="narrow stack-lg">
        <div className="center">
          <Apparition kind="key" />
          <span className="eyebrow">Welcome back</span>
          <h1>Open your wallet here</h1>
          <p className="dim">How should this device get in?</p>
        </div>
        <section className="card stack">
          {haveWallet}
          <ErrorNote error={error} />
        </section>
        <button className="link center-self" onClick={() => { setError(null); setStep("start"); }}>New here? Create a wallet</button>
      </main>
    );

  if (step === "prf")
    return (
      <main className="narrow stack-lg">
        <div className="center">
          <Apparition kind="candle" />
          <h1>Your passkey can&apos;t unlock a wallet (yet)</h1>
        </div>
        <section className="card stack">
          <p>
            Blockula derives your quantum-safe key from a secret your passkey produces through a feature called <b>PRF</b>. The passkey provider you picked doesn&apos;t support it.
          </p>
          <p className="dim">
            Try again with <b>iCloud Keychain</b> (Safari / iOS 18+), <b>Google Password Manager</b> (Chrome), <b>1Password</b>, or a recent YubiKey.
          </p>
          <button className="btn primary" onClick={() => setStep("start")}>← Try another passkey</button>
        </section>
        {localKeysAllowed(client.pin.chainId) && (
          <section className="card stack warn-card">
            <h2>Just testing?</h2>
            <p className="dim">
              We can make a <b>browser-only key</b>: a random secret saved in this browser&apos;s storage. Anyone with access to this browser profile could read it, and clearing site data deletes it. Fine for a test chain, not for anything you care about.
            </p>
            <button className="btn" onClick={local}>Use a browser-only key (less secure, for testing)</button>
          </section>
        )}
      </main>
    );

  if (step === "fund")
    return (
      <main className="narrow center stack-lg">
        <Apparition kind="bat" />
        <h1>{funded ? `${funded} landed!` : "Fetching some test FANG…"}</h1>
        <p className="lead">
          {funded
            ? "That's FANG, Blockula's currency. The faucet just sent it to you in a real transaction, sealed into a block. Let's use some of it to claim a name."
            : "This test chain has a faucet that gives new wallets free FANG to play with."}
        </p>
        {!funded && !error && <Spinner>Waiting for the next block…</Spinner>}
        <ErrorNote error={error} />
        {(funded || error) && <button className="btn primary big" onClick={() => { setError(null); setStep("kit"); }}>Next: your recovery kit</button>}
      </main>
    );

  if (step === "kit")
    return (
      <main className="narrow stack-lg">
        <div className="center">
          <span className="eyebrow">Your backup</span>
          <h1>Make a recovery kit</h1>
          <p className="dim">Passkeys sync between your devices, but providers can lose them or lock you out. A kit on paper still gets you in.</p>
        </div>
        <RecoveryKitSetup onFinish={() => setStep("name")} finishLabel="Next: claim a name" onSkip={() => setStep("name")} />
      </main>
    );

  if (step === "name")
    return (
      <main className="narrow stack-lg">
        <div className="center">
          <Apparition kind="grave" text={epitaph(typed)} />
          <span className="eyebrow">Your name</span>
          <h1>Claim your .boo name</h1>
          <p className="dim">
            Your address is a long string like boo1qx…k3. A name points to it, so friends can send to <b>vlad.boo</b> instead.
          </p>
        </div>
        <section className="card">
          <NameClaim onName={setTyped} onClaimed={(n) => { setClaimed(n); setStep("done"); }} />
        </section>
        <button className="link center-self" onClick={() => setStep("done")}>Skip for now</button>
      </main>
    );

  return (
    <main className="narrow center stack-lg">
      {/* Never the first render (so no hydration to match): fine to read the date here. */}
      <Apparition kind={season() === "halloween" ? "pumpkin" : "moon"} />
      <h1>{claimed ? `Welcome to the night, ${claimed}` : "Your wallet is ready"}</h1>
      <p className="lead">
        You now own a quantum-safe account. Next time, just unlock with your passkey. Linking a second device is a good idea too, and if you skipped the recovery kit, make one from Devices &amp; agents.
      </p>
      <Tip>Your home screen has a short checklist that walks you through sending, the explorer, and giving an AI agent a budget.</Tip>
      <button className="btn primary big" onClick={() => router.replace("/")}>Open my wallet</button>
    </main>
  );
}
