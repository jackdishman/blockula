"use client";

import type { Wallet } from "@blockula/sdk";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useEffect, useState, type ReactNode } from "react";
import { localKeysAllowed } from "@/lib/config";
import { friendly, isPrfUnsupported } from "@/lib/errors";
import { findAccounts, hintAccount, openLocalKey, unlockPasskey, type AccountChoice } from "@/lib/keys";
import type { Hint } from "@/lib/session";
import { useWallet } from "@/lib/wallet";
import { AccountPicker } from "./AccountPicker";
import { Apparition } from "./Haunt";
import { ErrorNote, Spinner } from "./ui";

export function Splash({ text = "Rising from the grave…" }: { text?: string }) {
  return (
    <div className="splash">
      <Apparition kind="coffin" />
      <p className="dim">{text}</p>
    </div>
  );
}

/** Renders children only when a wallet is unlocked; otherwise unlock or onboard. */
export function RequireWallet({ children }: { children: ReactNode }) {
  const { ready, hint, wallet } = useWallet();
  const router = useRouter();
  useEffect(() => {
    if (!ready) return;
    if (!hint) router.replace("/welcome");
    else if (wallet && !hint.account) router.replace("/link");
  }, [ready, hint, wallet, router]);
  if (!ready || !hint) return <Splash />;
  if (!wallet) return <Unlock />;
  if (!hint.account) return <Splash />;
  return <>{children}</>;
}

/** Shared "a key is unlocked; now which account does it act for?" step. */
export function useAccountResolution() {
  const { client, unlocked } = useWallet();
  const router = useRouter();
  const [choices, setChoices] = useState<{ wallet: Wallet; hint: Hint; list: AccountChoice[] } | null>(null);

  const finish = async (w: Wallet, h: Hint, c: AccountChoice) => {
    await w.actAs(c.address);
    unlocked(w, { ...h, account: c.address, name: c.name ?? undefined });
    router.replace("/");
  };

  const resolve = async (w: Wallet, h: Hint, opts: { waitingForLink?: boolean } = {}) => {
    const list = await findAccounts(client, w);
    if (opts.waitingForLink && list.every((c) => c.address === w.address)) {
      unlocked(w, { ...h, account: undefined });
      // Already on /link? Keep the other device's link (its account) it was opened with.
      router.replace(window.location.pathname === "/link" ? `/link${window.location.search}` : "/link");
      return;
    }
    if (list.length === 1) return finish(w, h, list[0]);
    setChoices({ wallet: w, hint: h, list });
  };

  const picker = choices ? <AccountPicker choices={choices.list} onPick={(c) => finish(choices.wallet, choices.hint, c)} /> : null;
  return { resolve, picker };
}

export function Unlock() {
  const { client, hint, unlocked, forget } = useWallet();
  const h = hint as Hint;
  const router = useRouter();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { resolve, picker } = useAccountResolution();
  const localOk = localKeysAllowed(client.pin.chainId);

  const go = async () => {
    setBusy(true);
    setError(null);
    try {
      // An account saved on another chain isn't this one's: its key's accounts here are found
      // again (it isn't a device waiting to be linked).
      if (h.kind === "local") {
        const w = openLocalKey(client, h);
        const account = await hintAccount(client, h);
        if (account) {
          await w.actAs(account);
          unlocked(w, h);
        } else await resolve(w, h, { waitingForLink: !h.account });
        return;
      }
      const { wallet: w, credentialId } = await unlockPasskey(client, h.credentialId);
      const next: Hint = { ...h, credentialId };
      const account = await hintAccount(client, h);
      if (w.keyId === h.keyId && account) {
        await w.actAs(account);
        unlocked(w, next);
      } else {
        // A different passkey than last time, a device still waiting to be linked, or an account from another chain.
        await resolve(w, { v: 1, kind: "passkey", credentialId, keyId: w.keyId }, { waitingForLink: w.keyId === h.keyId && !h.account });
      }
    } catch (e) {
      setError(isPrfUnsupported(e) && localOk ? `${friendly(e)} Start over to use a browser-only key instead.` : friendly(e));
    } finally {
      setBusy(false);
    }
  };

  // Browser-only keys have no prompt to wait for.
  useEffect(() => {
    if (h.kind === "local" && localOk) go();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  if (picker) return <main className="narrow">{picker}</main>;

  return (
    <main className="narrow center">
      <Apparition kind="key" busy={busy} />
      <span className="eyebrow">The door is locked</span>
      <h1>Welcome back{h.name ? `, ${h.name}` : ""}</h1>
      <p className="dim">
        Your wallet stays locked when you're away. We never store your key, so your passkey re-derives it each visit.
      </p>
      <button className="btn primary big" disabled={busy} onClick={go}>
        {busy ? <Spinner>Waiting for your passkey…</Spinner> : h.kind === "local" ? "Unlock browser-only key" : "Unlock with passkey"}
      </button>
      <ErrorNote error={error} />
      {h.kind === "passkey" && <Link href="/recover" className="link">Lost your passkey? Restore with your recovery kit</Link>}
      <button
        className="link"
        onClick={() => {
          if (h.kind === "local" && !confirm("This browser-only key lives only here. Forgetting it means losing anything on it unless another device is linked. Continue?")) return;
          forget();
          router.replace("/welcome");
        }}
      >
        Use a different wallet
      </button>
    </main>
  );
}
