"use client";

import Link from "next/link";
import { MAINNET_CHAIN_ID, NETWORK, NETWORKS } from "@/lib/config";
import type { LinkChain, LinkCheck, WalletChain } from "@/lib/paylink";

/**
 * A chain id as the wallet knows it: a configured network's label (or Mainnet's, by its chain id),
 * else null. A link's chain id is the sender's text, so the wallet never shows it as its own
 * words, even when it's well-formed (audit review-r3-wallet-4).
 */
const knownChain = (chainId: string): { label: string; inMenu: boolean } | null => {
  const configured = Object.values(NETWORKS).find((n) => n?.chainId === chainId);
  if (configured) return { label: configured.label, inMenu: true };
  return chainId === MAINNET_CHAIN_ID ? { label: "Mainnet", inMenu: false } : null;
};

/**
 * What Send says about a payment link that filled (or didn't fill) the form: the chain it was made
 * on against the wallet's (lib/paylink.ts `checkLinkChain`, audit main-11-1). Every refusal is the
 * wallet's own fixed text: it names a network only by a label this wallet knows.
 */
export function PayLinkNotice({ check, link, wallet }: { check: LinkCheck; link: LinkChain | null; wallet: WalletChain }) {
  if (check.state === "mismatch") {
    const known = check.why === "chain" && link ? knownChain(link.chainId) : null;
    // `genesis`: the link's chain id is the wallet's own; its genesis is GENESIS_HEX hex digits.
    const here = (wallet.chainId && knownChain(wallet.chainId)?.label) || NETWORK.label;
    return (
      <div className="banner danger" role="alert" data-testid="paylink-refused">
        {check.why === "malformed" ? (
          <>This payment link names its network in a form no Blockula network uses, so it isn&apos;t filled in here. Ask whoever sent it for a new link.</>
        ) : check.why === "chain" ? (
          known ? (
            <>
              This payment link was made for {known.label}, but your wallet is on {NETWORK.label}. A .boo name can belong to someone else on another network, so the link isn&apos;t filled in here.
              {known.inMenu ? <> To pay it on {known.label}, switch networks in the header menu, then open the link again.</> : null}
            </>
          ) : (
            <>This payment link was made for a network this wallet doesn&apos;t know, not {NETWORK.label}. A .boo name can belong to someone else on another network, so the link isn&apos;t filled in here.</>
          )
        ) : (
          <>
            This payment link was made for another instance of {here} (genesis {link?.genesis ?? "?"}…), not the one your wallet is on{wallet.genesisHash ? ` (genesis ${wallet.genesisHash.slice(0, 16)}…)` : ""}, so the link isn&apos;t filled in here.
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
