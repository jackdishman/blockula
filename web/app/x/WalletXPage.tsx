"use client";

import "@/app/explorer.css";
import { booProfileUrl, xHandle, xIntentUrl, xLinkCode, xProofPost, type XLink } from "@blockula/sdk";
import Link from "next/link";
import { useState } from "react";
import { ActionForm } from "@/components/explorer/ActionForm";
import { Ext } from "@/components/explorer/kit";
import { useXChain, useXLink, XChip } from "@/components/explorer/XLink";
import { RequireWallet } from "@/components/Gate";
import { Copy, Tip } from "@/components/ui";
import { useNode } from "@/lib/explorer/node";
import { actionId, X_ACTIONS } from "@/lib/explorer/programs";
import { to } from "@/lib/explorer/routes";
import type { AccountDetail } from "@/lib/explorer/types";
import { useUnlocked } from "@/lib/wallet";

const spec = (id: string) => X_ACTIONS.find((a) => actionId(a) === id)!;

export default function WalletXPage() {
  return (
    <RequireWallet>
      <LinkX />
    </RequireWallet>
  );
}

/**
 * Link an X account to this wallet's account (docs/X-LINKING.md), in three steps: get the code
 * for this account and that handle, post it from that handle, then record the post on the chain.
 * The code holds no secret (it's a hash of the chain, the address and the handle); what proves
 * the link is that the handle posted it and that this account, with an owner key, recorded it.
 */
function LinkX() {
  const { wallet } = useUnlocked();
  const chain = useXChain();
  const acct = useNode<AccountDetail>(`/account/${encodeURIComponent(wallet.account)}`, { every: 5000 });
  const mine = acct.data?.account.address === wallet.account ? acct.data : null;
  const link = useXLink(mine);
  const [typed, setTyped] = useState("");

  let handle: string | null = null;
  let handleErr: string | null = null;
  if (typed.trim()) {
    try {
      handle = xHandle(typed);
    } catch (e) {
      handleErr = e instanceof Error ? `${e.message[0].toUpperCase()}${e.message.slice(1)}.` : "That isn't an X handle.";
    }
  }
  const code = handle && chain ? xLinkCode(chain, wallet.account, handle) : null;
  // The post links the account's profile on this chain: this origin's /boo/<name> (app/boo; in
  // the served wallet the node sends it on to /wallet/boo/<name>/). Not on plain http, where no
  // stranger could open it.
  const text = code ? xProofPost(code, booProfileUrl(window.location.origin, mine?.names[0])) : null;

  return (
    <main className="narrow stack-lg">
      <Link href="/" className="back">← Home</Link>
      <h1>Link your X account</h1>
      <p className="dim m0">Show your X handle on your account and your .boo names. You prove it with one public post, and you can take it away again.</p>

      {link && <Linked link={link} />}

      <section className="card stack" data-testid="x-step-code">
        <span className="eyebrow">Step 1 · Your code</span>
        <label className="field">
          <span>Your X handle</span>
          <input value={typed} onChange={(e) => setTyped(e.target.value)} placeholder="@BlockulaChain" autoCapitalize="none" autoCorrect="off" spellCheck={false} aria-label="Your X handle" />
          {handleErr && <span className="warn small">{handleErr}</span>}
        </label>
        {text && code && handle ? (
          <>
            <pre className="snippet" data-testid="x-post-text">{text}</pre>
            <div className="row-gap">
              <Copy text={text} label="Copy the post" />
              <Ext className="btn small primary" href={xIntentUrl(text)}>Post it on X ↗</Ext>
            </div>
            <span className="dim small">
              Post it from @{handle}, exactly as it is. The code works only for this account, on this chain, from that handle: nobody else can use it, and it gives nothing away.
            </span>
          </>
        ) : (
          <span className="dim small">{chain ? "Type your handle to get the post that proves it's yours." : "Waiting for the node to say which chain this is…"}</span>
        )}
      </section>

      <section className="card stack" data-testid="x-step-record">
        <span className="eyebrow">Step 2 · Record it</span>
        {handle ? (
          // Remounted when the handle changes, so the form always records the handle the code was made for.
          <ActionForm key={handle} spec={spec("link_x")} initial={{ handle }} fixed={["handle"]} compact />
        ) : (
          <span className="dim small">Once you&apos;ve posted, paste the post&apos;s link here and sign. It costs only the fee.</span>
        )}
      </section>

      <Tip>
        The record is public and stays in your account&apos;s history, like every transaction. Anyone can check the link: your post must be by that handle and contain the code. Delete the post, or unlink here, and the link is gone.
      </Tip>
    </main>
  );
}

/** The link in force, and the way out of it. */
function Linked({ link }: { link: XLink }) {
  return (
    <section className="card stack" data-testid="x-linked">
      <span className="eyebrow">Linked now</span>
      <div className="row-gap">
        <XChip link={link} />
        <Link className="small" href={to.tx(link.tx)}>its record →</Link>
      </div>
      <span className="dim small">Linking another handle below replaces this one.</span>
      <details className="action" id="do-unlink_x">
        <summary>
          <span className="title">Unlink @{link.handle}</span>
        </summary>
        <ActionForm spec={spec("unlink_x")} compact />
      </details>
    </section>
  );
}
