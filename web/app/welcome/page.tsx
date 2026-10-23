"use client";

import { formatFang } from "@blockula/sdk";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { Suspense, useEffect, useRef, useState } from "react";
import { HowFangReachesYou, PayRequestBanner } from "@/components/FirstFang";
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
import { canAffordName, cheapestName, safeNext, welcomePayment } from "@/lib/onboarding";
import { BRAND, FIRST_FANG } from "@/lib/onboarding-copy";
import type { Hint } from "@/lib/session";
import { useWallet } from "@/lib/wallet";

type Step = 0 | 1 | 2 | "start" | "have" | "prf" | "fund" | "kit" | "name" | "done";

/** How long the fund step waits for the node before offering a way on without it. */
const NODE_PATIENCE_MS = 8000;

/** The story before the wallet. Chapters I and III's words are in lib/onboarding-copy.ts. */
const INTRO: { ornament: PropKind; chapter: string; title: string; body: string }[] = [
  {
    ornament: "bat",
    chapter: "Chapter I",
    title: "A wallet is a key, not a vault",
    body: BRAND.chapterOne,
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
    title: BRAND.chapterThreeTitle,
    body: BRAND.chapterThreeBody,
  },
];

export default function WelcomePage() {
  return (
    <Suspense>
      <Welcome />
    </Suspense>
  );
}

function Welcome() {
  const { ready, hint, wallet, account, client, status, unlocked, refresh } = useWallet();
  const router = useRouter();
  // A payment link opened without a wallet (RequireWallet): onboarding ends there instead of Home,
  // unless it was made for another chain (checked as Send checks it), which Send won't fill.
  const next = safeNext(useSearchParams().get("next"));
  const { request, check: linkCheck, payment } = welcomePayment(next, client.pin);
  const home = payment ?? "/";
  const [step, setStep] = useState<Step>(0);
  const [nickname, setNickname] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [funded, setFunded] = useState<string | null>(null);
  const [claimed, setClaimed] = useState<string | null>(null);
  const [typed, setTyped] = useState("");
  const { resolve, picker } = useAccountResolution(home);
  const inFlow = useRef(false);

  // Already set up on this browser? Go home, or to the payment (unless we're mid-onboarding).
  useEffect(() => {
    if (ready && hint && !inFlow.current) router.replace(home);
  }, [ready, hint, router, home]);

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

  // A node that doesn't answer mustn't hold the new wallet hostage: after a while, offer to go on.
  const [stalled, setStalled] = useState(false);
  useEffect(() => {
    if (step !== "fund" || status) return;
    const t = setTimeout(() => setStalled(true), NODE_PATIENCE_MS);
    return () => clearTimeout(t);
  }, [step, status]);

  // The name step: the cheapest name's price (a governance parameter), to tell whether to offer one yet.
  const [cheapest, setCheapest] = useState<bigint | null>(null);
  useEffect(() => {
    if (step !== "name") return;
    let live = true;
    client
      .params()
      .then((p: { params?: Record<string, unknown> } | null) => live && setCheapest(cheapestName(p?.params)))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [step, client]);

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

  const ask = request && linkCheck && <PayRequestBanner request={request} check={linkCheck} />;

  if (typeof step === "number") {
    const s = INTRO[step];
    return (
      <main className="narrow center onboarding">
        {ask}
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
        {ask}
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
        {ask}
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
            Blockula derives your {BRAND.signingKey} from a secret your passkey produces through a feature called <b>PRF</b>. The passkey provider you picked doesn&apos;t support it.
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

  if (step === "fund") {
    const toKit = (
      <button className="btn primary big" onClick={() => { setError(null); setStep("kit"); }}>
        Next: your recovery kit
      </button>
    );
    // Until the node says whether this chain has a faucet, nothing about faucets (a chain without
    // one goes straight on to the kit).
    if (!status?.faucet)
      return (
        <main className="narrow center stack-lg" data-testid="fund-waiting">
          <Apparition kind="bat" />
          <h1>{stalled && !status ? "Your wallet is created" : "Checking the network…"}</h1>
          {stalled && !status ? (
            <>
              <p className="lead">The network isn&apos;t answering right now. You can continue, and check your balance later.</p>
              {toKit}
            </>
          ) : (
            <Spinner>Asking the node about this chain…</Spinner>
          )}
        </main>
      );
    return (
      <main className="narrow center stack-lg">
        <Apparition kind="bat" />
        <h1>{funded ? `${funded} landed!` : "Fetching some test FANG…"}</h1>
        <p className="lead">
          {funded
            ? `That's FANG, ${BRAND.unit}. The faucet just sent it to you in a real transaction, sealed into a block. Let's use some of it to claim a name.`
            : "This chain has a faucet that gives new wallets free FANG to try things with."}
        </p>
        <p className="dim small onb-novalue" data-testid="fang-no-value">{FIRST_FANG.noValue}</p>
        {!funded && !error && <Spinner>Waiting for the next block…</Spinner>}
        <ErrorNote error={error} />
        {(funded || error) && toKit}
      </main>
    );
  }

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

  if (step === "name") {
    // A chain without a faucet, and not enough FANG for any name: nothing to claim yet, and no
    // faucet to send the user to. Say so, and when (the heading stays: it's the same step).
    const balance = BigInt(account?.balance ?? "0");
    if (!status?.faucet && !canAffordName(balance, cheapest))
      return (
        <main className="narrow stack-lg" data-testid="name-later">
          <div className="center">
            <Apparition kind="grave" />
            <span className="eyebrow">Your name, later</span>
            <h1>Claim your .boo name</h1>
            <p className="dim">
              A name like <b>vlad.boo</b> points to your long address, so people can send to it. Names cost FANG{cheapest !== null ? `, from ${formatFang(cheapest)} for a year` : ""}, and this wallet {balance > 0n ? "doesn't have enough yet" : "has none yet"}. Once some reaches you, claim one from Home.
            </p>
          </div>
          <button className="btn primary big center-self" onClick={() => setStep("done")}>Skip for now</button>
        </main>
      );
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
  }

  // The node says this chain has no faucet: say how FANG does arrive. (With the node silent, nothing
  // is known either way, so nothing is said.)
  const firstFang = !!status && !status.faucet;
  return (
    <main className="narrow center stack-lg">
      {/* Never the first render (so no hydration to match): fine to read the date here. */}
      <Apparition kind={season() === "halloween" ? "pumpkin" : "moon"} />
      <h1>{claimed ? `Welcome to the night, ${claimed}` : "Your wallet is ready"}</h1>
      <p className="lead">
        {BRAND.accountSigns} Next time, just unlock with your passkey. Linking a second device is a good idea too, and if you skipped the recovery kit, make one from Devices &amp; agents.
      </p>
      {firstFang && <HowFangReachesYou />}
      {ask}
      <Tip>Your home screen has a short checklist that walks you through sending, the explorer, and giving an AI agent a budget.</Tip>
      <button className="btn primary big" onClick={() => router.replace(home)}>{payment ? "Continue to the payment" : "Open my wallet"}</button>
    </main>
  );
}
