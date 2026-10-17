"use client";

import Link from "next/link";
import { NETWORK, NETWORKS } from "@/lib/config";
import type { LinkChain, LinkCheck, WalletChain } from "@/lib/paylink";

/** A chain id as the wallet knows it: a configured network's label, else the id itself. */
const chainLabel = (chainId: string) => Object.values(NETWORKS).find((n) => n?.chainId === chainId)?.label ?? `chain “${chainId.slice(0, 64)}”`;

/**
 * What Send says about a payment link that filled (or didn't fill) the form: the chain it was made
 * on against the wallet's (lib/paylink.ts `checkLinkChain`, audit main-11-1).
 */
export function PayLinkNotice({ check, link, wallet }: { check: LinkCheck; link: LinkChain | null; wallet: WalletChain }) {
  if (check.state === "mismatch") {
    const made = link ? chainLabel(link.chainId) : "another network";
    return (
      <div className="banner danger" role="alert" data-testid="paylink-refused">
        {check.why === "chain" ? (
          <>
            This payment link was made for {made}, but your wallet is on {NETWORK.label}. A .boo name can belong to someone else on another network, so the link isn&apos;t filled in here. To pay it on {made}, switch networks in the header menu, then open the link again.
          </>
        ) : (
          <>
            This payment link was made for another instance of {made} (genesis {link?.genesis?.slice(0, 16) ?? "?"}…), not the one your wallet is on{wallet.genesisHash ? ` (genesis ${wallet.genesisHash.slice(0, 16)}…)` : ""}, so the link isn&apos;t filled in here.
          </>
        )}{" "}
        <Link className="link" href="/send">Start a new payment instead</Link>
      </div>
    );
  }
  if (check.state === "pending") return <div className="banner" data-testid="paylink-pending">Checking which network this payment link was made for…</div>;
  return (
    <div className="banner" data-testid="paylink-filled">
      {check.state === "match"
        ? <>A payment link made for {NETWORK.label} filled in this payment. Double-check who it goes to and how much before sending.</>
        : <>A link filled in this payment. It doesn&apos;t say which network it was made for, so check with whoever sent it that they meant {NETWORK.label}. Double-check who it goes to and how much before sending.</>}
    </div>
  );
}
