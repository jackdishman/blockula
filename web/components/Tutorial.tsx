"use client";

import Link from "next/link";
import { cssVars } from "@/lib/cssvars";
import { useEffect, useState, type ReactNode } from "react";
import { explorer } from "@/lib/config";
import { friendly } from "@/lib/errors";
import { KIT_LABEL } from "@/lib/recovery";
import { loadTutorial, saveTutorial, type TutorialState } from "@/lib/session";
import type { AccountView, Status } from "@/lib/types";
import { useUnlocked } from "@/lib/wallet";
import { ErrorNote, ExplorerLink, Spinner } from "./ui";

const ROMAN = ["I", "II", "III", "IV", "V", "VI", "VII", "VIII"];

interface Item {
  id: string;
  title: string;
  done: boolean;
  learn: string;
  action?: ReactNode;
}

export function useTutorial() {
  const [t, setT] = useState<TutorialState>({});
  useEffect(() => {
    setT(loadTutorial());
    // The explorer link can be clicked anywhere in the app; re-read when we come back to the tab.
    const sync = () => setT(loadTutorial());
    window.addEventListener("focus", sync);
    return () => window.removeEventListener("focus", sync);
  }, []);
  const update = (patch: Partial<TutorialState>) => {
    const next = { ...loadTutorial(), ...patch };
    saveTutorial(next);
    setT(next);
  };
  return [t, update] as const;
}

export function Tutorial({ state, update }: { state: TutorialState; update: (p: Partial<TutorialState>) => void }) {
  const { wallet, account, status, refresh } = useUnlocked();
  const [open, setOpen] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const faucet = async () => {
    setBusy(true);
    setError(null);
    try {
      await wallet.faucet();
      await refresh();
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  const items = account ? buildItems(account, status, state, busy, faucet) : [];
  for (const i of items) i.done ||= !!state.done?.includes(i.id);
  // Remember ticks so they survive e.g. revoking the agent you set up.
  const newlyDone = items.filter((i) => i.done && !state.done?.includes(i.id)).map((i) => i.id).join();
  useEffect(() => {
    if (newlyDone) update({ done: [...(state.done ?? []), ...newlyDone.split(",")] });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [newlyDone]);

  if (!account || state.dismissed) return null;
  const done = items.filter((i) => i.done).length;
  return (
    <section className="card learn">
      <div className="row-between">
        <h2>Learn by doing</h2>
        <span className="row-gap">
          <span className="dim small">{done} of {items.length}</span>
          <button className="link small" onClick={() => update({ dismissed: true })}>Hide</button>
        </span>
      </div>
      <div className="progress"><span ref={cssVars({ "--w": `${(done / items.length) * 100}%` })} /></div>
      <ul className="checklist">
        {items.map((i, n) => (
          <li key={i.id} className={i.done ? "done" : ""}>
            <button className="check-row" onClick={() => setOpen(open === i.id ? null : i.id)} aria-expanded={open === i.id}>
              <span className="check" aria-label={i.done ? "Done" : undefined}>{i.done ? "✓" : ROMAN[n]}</span>
              <span className="grow">{i.title}</span>
              <span className="check-chevron" aria-hidden>{open === i.id ? "−" : "+"}</span>
            </button>
            {open === i.id && (
              <div className="check-body">
                <p>{i.learn}</p>
                {!i.done && i.action}
              </div>
            )}
          </li>
        ))}
      </ul>
      <ErrorNote error={error} />
      {done === items.length && <p className="finale">You&apos;ve mastered the basics. The night is yours.</p>}
    </section>
  );
}

function buildItems(account: AccountView, status: Status | null, state: TutorialState, busy: boolean, faucet: () => void): Item[] {
  const me = account.account.address;
  const txs = account.recent_txs;
  const sent = txs.find((t) => t.kind === "transfer" && t.from.address === me);
  const ownerKeys = account.keys.filter((k) => k.role.type === "owner" && k.label !== KIT_LABEL).length;
  const kits = account.keys.filter((k) => k.role.type === "owner" && k.label === KIT_LABEL).length;
  const friend = status?.validator.address;
  return [
    {
      id: "fang",
      title: "Get some FANG",
      done: BigInt(account.balance) > 0n || txs.some((t) => t.to?.address === me),
      learn: "FANG is Blockula's currency (1 FANG = 1,000,000,000 drops). Your balance isn't stored in this app; it lives on the blockchain, a shared ledger every node agrees on. Test chains have a faucet that hands it out free.",
      action: status?.faucet ? (
        <button className="btn small" disabled={busy} onClick={faucet}>{busy ? <Spinner>Dripping…</Spinner> : "Get 100 test FANG"}</button>
      ) : (
        <span className="dim small">Ask a friend to send you some: share your name from Receive.</span>
      ),
    },
    {
      id: "name",
      title: "Claim a .boo name",
      done: account.names.length > 0,
      learn: "Your real address is a long boo1… string derived from your public key. A .boo name points to it, like a domain name points to a server, so friends can pay vlad.boo instead of copying gibberish.",
      action: <Link className="btn small" href="/name">Pick a name</Link>,
    },
    {
      id: "send",
      title: "Send FANG to a friend",
      done: !!sent,
      learn: "Sending signs a transaction with your quantum-safe ML-DSA key. A validator seals it into a block about a second later, and then it's final. A tiny fee, priced by size, is burned forever.",
      action: (
        <span className="row-gap">
          <Link className="btn small" href="/send">Send</Link>
          {friend && <Link className="btn small ghost" href={`/send?to=${friend}&amount=1&memo=${encodeURIComponent("first fang")}`}>Try: 1 FANG to the validator</Link>}
        </span>
      ),
    },
    {
      id: "explorer",
      title: "View your transaction in the explorer",
      done: !!state.viewedExplorer,
      learn: "A blockchain is public: anyone can check that your transaction happened, but only your key could have signed it. The explorer shows the block it landed in and the signature that proves it.",
      action: txs[0] ? <ExplorerLink className="btn small" href={explorer.tx((sent ?? txs[0]).id)}>Open its receipt</ExplorerLink> : <span className="dim small">Make a transaction first.</span>,
    },
    {
      id: "device",
      title: "Link a second device",
      done: ownerKeys > 1,
      learn: "An account can hold several keys. Adding your phone or laptop as a co-owner means losing one device doesn't lose your wallet.",
      action: <Link className="btn small" href="/devices#device">Link a device</Link>,
    },
    {
      id: "kit",
      title: "Make a recovery kit",
      done: kits > 0,
      learn: "A recovery kit is one more owner key, written down as 24 words and kept offline. If every device and passkey is lost, the words add a new device to your account.",
      action: <Link className="btn small" href="/devices#recovery">Make a kit</Link>,
    },
    {
      id: "agent",
      title: "Give an agent a spending limit",
      done: account.keys.some((k) => k.role.type === "session"),
      learn: "A session key lets an AI agent or app spend from your account, but only up to a daily limit, only to send FANG, and only until it expires. You can revoke it any time.",
      action: <Link className="btn small" href="/devices#agent">Set up an agent</Link>,
    },
  ];
}
