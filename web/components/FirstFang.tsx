"use client";

import Link from "next/link";
import "@/app/onboarding.css";
import { shortAddr } from "@/lib/format";
import type { PayRequest } from "@/lib/onboarding";
import { FIRST_FANG } from "@/lib/onboarding-copy";

/** Where FANG comes from on a chain without a faucet: someone sends it. It has no value either way. */
export function HowFangReachesYou() {
  return (
    <section className="card stack onb-card" data-testid="first-fang" aria-labelledby="onb-first-fang">
      <h2 id="onb-first-fang">{FIRST_FANG.title}</h2>
      <ul className="onb-list">
        <li>
          {FIRST_FANG.receive} <Link href="/receive">{FIRST_FANG.receiveLink}</Link>
        </li>
        <li>{FIRST_FANG.noFaucet}</li>
        <li>{FIRST_FANG.noValue}</li>
      </ul>
    </section>
  );
}

/**
 * The payment a link (or an account page's Pay button) asked for, while its visitor makes a
 * wallet. Only what lib/onboarding.ts `payRequest` let through (a well-formed name or address, a
 * valid amount), never the memo: Send checks the link again before anything is signed.
 */
export function PayRequestBanner({ request }: { request: PayRequest }) {
  const who = request.to ? (request.to.endsWith(".boo") ? request.to : shortAddr(request.to)) : "Someone";
  const lead = request.amount ? `${who} asked you for ${request.amount} FANG.` : request.to ? `Paying ${who}.` : "Someone sent you a payment link.";
  return (
    <div className="banner onb-ask" data-testid="pay-request">
      <b>{lead}</b> Once your wallet is ready, you&apos;ll see the payment filled in, and nothing is sent until you confirm it.
    </div>
  );
}
