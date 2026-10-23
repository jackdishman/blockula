"use client";

import Link from "next/link";
import "@/app/onboarding.css";
import { shortAddr } from "@/lib/format";
import type { PayRequest } from "@/lib/onboarding";
import { FIRST_FANG } from "@/lib/onboarding-copy";
import type { LinkCheck } from "@/lib/paylink";

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
 * The payment a link (or an account page's Pay button) opens, while its visitor makes a wallet:
 * what the link says, as its claim, never as who asked. Only what lib/onboarding.ts `payRequest`
 * let through (a well-formed name or address, a valid amount), never the memo. `check` is the
 * link's chain against the wallet's, as Send checks it (lib/paylink.ts `checkLinkChain`): a link
 * made for another chain fills nothing there, so it's only said that it won't; until the wallet
 * knows its own chain, nothing. Send checks the link again before anything is signed.
 */
export function PayRequestBanner({ request, check }: { request: PayRequest; check: LinkCheck }) {
  if (check.state === "pending") return null;
  if (check.state === "mismatch")
    return (
      <div className="banner onb-ask" data-testid="pay-request">
        This payment link isn&apos;t for this network, so it won&apos;t be filled in here.
      </div>
    );
  const who = request.to ? (request.to.endsWith(".boo") ? request.to : shortAddr(request.to)) : null;
  const lead = request.amount ? `This link asks for ${request.amount} FANG${who ? ` to ${who}` : ""}.` : who ? `This link opens a payment to ${who}.` : "This link opens a payment.";
  return (
    <div className="banner onb-ask" data-testid="pay-request">
      <b>{lead}</b> Once your wallet is ready, you&apos;ll see the payment filled in, and nothing is sent until you confirm it.
    </div>
  );
}
