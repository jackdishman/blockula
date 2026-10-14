"use client";

import { parseFang } from "@blockula/sdk";
import { useEffect, useState } from "react";
import { friendly } from "@/lib/errors";
import type { NameView } from "@/lib/types";
import { useUnlocked } from "@/lib/wallet";
import { ErrorNote, Spinner } from "./ui";

type Check = { state: "idle" } | { state: "checking" } | { state: "ok"; view: NameView } | { state: "bad"; msg: string };

const clean = (s: string) => s.trim().toLowerCase().replace(/\.boo$/, "");

export function NameClaim({ onClaimed, suggestion = "" }: { onClaimed: (name: string) => void; suggestion?: string }) {
  const { client, wallet, account, refresh } = useUnlocked();
  const [input, setInput] = useState(suggestion);
  const [check, setCheck] = useState<Check>({ state: "idle" });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const n = clean(input);

  useEffect(() => {
    if (!n) return setCheck({ state: "idle" });
    if (n.length < 3) return setCheck({ state: "bad", msg: "At least 3 characters." });
    setCheck({ state: "checking" });
    let live = true;
    const t = setTimeout(async () => {
      try {
        const view = (await client.name(n)) as NameView;
        if (live) setCheck({ state: "ok", view });
      } catch (e) {
        if (live) setCheck({ state: "bad", msg: friendly(e) });
      }
    }, 250);
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [n, client]);

  const view = check.state === "ok" ? check.view : null;
  const balance = BigInt(account?.balance ?? "0");
  // Price plus a generous allowance for the (tiny, size-based) network fee.
  const short = view?.available && view.price ? balance < BigInt(view.price) + parseFang("0.001") : false;

  const claim = async () => {
    setBusy(true);
    setError(null);
    try {
      await wallet.registerName(n);
      await refresh();
      onClaimed(`${n}.boo`);
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="stack">
      <label className="field">
        <span>Your name</span>
        <div className="suffix-input">
          <input value={input} onChange={(e) => setInput(e.target.value)} placeholder="vlad" autoCapitalize="none" autoCorrect="off" spellCheck={false} maxLength={36} />
          <span className="suffix">.boo</span>
        </div>
      </label>
      <div className="name-status" aria-live="polite">
        {check.state === "checking" && <Spinner>Checking the crypt…</Spinner>}
        {check.state === "bad" && <span className="warn">{check.msg}</span>}
        {view?.available && (
          <span className="ok">
            ✓ {view.name} is available · {view.price_display}
          </span>
        )}
        {view && !view.available && (
          <span className="warn">
            {view.name} is taken (owned by {view.owner?.short})
          </span>
        )}
      </div>
      {short && view?.price && <p className="warn">You need {view.price_display} plus a tiny fee. Grab test FANG from the faucet first.</p>}
      <p className="dim small">
        Shorter names cost more: 3 letters 100 FANG, 4 letters 25, 5+ letters 5. The payment goes to the community treasury.
      </p>
      <button className="btn primary" disabled={!view?.available || busy || short} onClick={claim}>
        {busy ? <Spinner>Claiming…</Spinner> : view?.available ? `Claim ${view.name}` : "Claim name"}
      </button>
      <ErrorNote error={error} />
    </div>
  );
}
