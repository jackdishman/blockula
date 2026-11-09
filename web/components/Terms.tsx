"use client";

import Link from "next/link";
import { useState, type ReactNode } from "react";
import { useTerms } from "@/lib/terms";
import { Apparition } from "./Haunt";

/** The short version every agreement shows above its checkbox (app/terms says it in full). */
export const TERMS_POINTS: { b: string; rest: string }[] = [
  { b: "For education and research only.", rest: "Blockula is an experiment in post-quantum blockchains, not a financial product or service." },
  { b: "The coins have no value.", rest: "Nobody sells them, and they never turn into money or anything else. Don't buy, sell or trade them, or take them as payment for anything real." },
  { b: "Experimental, unaudited software.", rest: "It can have bugs, and a chain can stop or reset. You can lose your wallet, coins and names." },
  { b: "You hold your own key.", rest: "Nobody can recover it for you. Keep your passkey and recovery kit safe." },
  { b: "Everything on a chain is public and permanent.", rest: "Names, memos and payments can't be deleted, so don't put personal information in them." },
];

/**
 * Renders children only once this device has agreed to the current Terms of Use and Privacy
 * Policy; until then, the agreement. Wraps everything that makes, restores, links or unlocks a
 * wallet, so no key exists here before its holder has agreed.
 */
export function RequireTerms({ children }: { children: ReactNode }) {
  const { agreed, agree } = useTerms();
  if (agreed === null) return null;
  if (!agreed) return <TermsAgreement onAgree={agree} />;
  return <>{children}</>;
}

export function TermsAgreement({ onAgree }: { onAgree: () => void }) {
  const [checked, setChecked] = useState(false);
  return (
    <main className="narrow stack-lg" data-testid="terms-agreement">
      <div className="center">
        <Apparition kind="candle" />
        <span className="eyebrow">Before you begin</span>
        <h1>A few things to agree to</h1>
      </div>
      <section className="card stack">
        <ul className="terms-points">
          {TERMS_POINTS.map((p) => (
            <li key={p.b}>
              <b>{p.b}</b> {p.rest}
            </li>
          ))}
        </ul>
        <label className="terms-check">
          <input type="checkbox" checked={checked} onChange={(e) => setChecked(e.target.checked)} data-testid="terms-checkbox" />
          <span>
            I&apos;m 18 or older, I&apos;ve read and agree to the <Link href="/terms" target="_blank">Terms of Use</Link> and the{" "}
            <Link href="/privacy" target="_blank">Privacy Policy</Link>, and I&apos;ll use Blockula for education and research only.
          </span>
        </label>
        <button className="btn primary big" disabled={!checked} onClick={onAgree}>I agree</button>
      </section>
      <p className="dim small center-self">This device remembers that you agreed. Nothing about it is sent anywhere.</p>
    </main>
  );
}
