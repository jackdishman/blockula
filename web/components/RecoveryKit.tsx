"use client";

import { useState } from "react";
import { explorer, NETWORK } from "@/lib/config";
import { friendly } from "@/lib/errors";
import { shortAddr } from "@/lib/format";
import { confirmPositions, generateKit, KIT_LABEL, kitText, wordIndex, type Kit } from "@/lib/recovery";
import type { TxView } from "@/lib/types";
import { useUnlocked } from "@/lib/wallet";
import { ErrorNote, ExplorerLink, Spinner, Tip } from "./ui";

type Stage =
  | { s: "intro" }
  | { s: "show"; kit: Kit }
  | { s: "confirm"; kit: Kit; ask: [number, number] }
  | { s: "activate"; grant: string }
  | { s: "done"; tx: TxView }
  | { s: "pending" };

/**
 * Make a recovery kit: a new owner key whose 24 words the user writes down or prints. The words
 * exist only in this component's state and are dropped once the user has proved they kept them;
 * only the key id (public) is used to add the key on-chain.
 */
export function RecoveryKitSetup({
  onFinish,
  finishLabel = "Done",
  onSkip,
  skipLabel = "Skip for now (make one later in Devices & agents)",
}: {
  onFinish?: () => void;
  finishLabel?: string;
  onSkip?: () => void;
  skipLabel?: string;
}) {
  const { wallet, account, client, status, updateHint, refresh } = useUnlocked();
  const [stage, setStage] = useState<Stage>({ s: "intro" });
  const [answers, setAnswers] = useState(["", ""]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const make = async () => {
    setError(null);
    setBusy(true);
    let made;
    try {
      made = await generateKit(wallet.account, client);
    } catch (e) {
      return setError(friendly(e));
    } finally {
      setBusy(false);
    }
    const { words, keyId, grant } = made;
    setStage({
      s: "show",
      kit: {
        words,
        keyId,
        grant,
        account: wallet.account,
        name: account?.names[0],
        network: NETWORK.label,
        chainId: status?.chain_id ?? client.pin.chainId ?? "",
        created: new Date().toISOString().slice(0, 10),
        walletUrl: window.location.origin,
      },
    });
  };

  const activate = async (grant: string) => {
    setBusy(true);
    setError(null);
    try {
      if (account && BigInt(account.balance) === 0n) throw new Error("insufficient balance: has 0 FANG");
      const tx = (await wallet.addOwnerKey(grant, KIT_LABEL)) as TxView;
      updateHint({ pendingKit: undefined });
      setStage({ s: "done", tx });
      await refresh();
    } catch (e) {
      // The kit stays pending in the hint: Home and Devices offer to activate it later.
      if (/insufficient balance/.test(e instanceof Error ? e.message : String(e))) setStage({ s: "pending" });
      else setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  const check = () => {
    if (stage.s !== "confirm") return;
    const { kit, ask } = stage;
    const wrong = ask.filter((p, i) => wordIndex(answers[i]) === undefined || wordIndex(answers[i]) !== wordIndex(kit.words[p]));
    if (wrong.length) return setError(`That isn't word #${wrong[0] + 1} on your kit. Check what you wrote down.`);
    setAnswers(["", ""]);
    // The words have done their job: from here on only the (public) key id and grant are kept, in
    // the hint until the key is on-chain, so a kit written down is never silently lost.
    const grant = kit.grant;
    if (!grant) return setError("This kit has no grant to add it with. Make a new kit.");
    updateHint({ pendingKit: { keyId: kit.keyId, created: kit.created, grant } });
    setStage({ s: "activate", grant });
    activate(grant);
  };

  const skip = onSkip && (
    <button className="link center-self no-print" onClick={onSkip}>{skipLabel}</button>
  );

  if (stage.s === "intro")
    return (
      <div className="stack-lg">
        <section className="card stack" data-testid="kit-intro">
          <p>
            A <b>recovery kit</b> is a paper backup: 24 words that act as one more owner key on your account. If you ever lose every device and passkey, the words let you add a new one.
          </p>
          <p className="dim">
            The words are made here, in this browser, and never sent anywhere. Anyone who finds them can take your account, so keep the kit offline, like a passport.
          </p>
          <button className="btn primary" disabled={busy} onClick={make}>Make my recovery kit</button>
          <ErrorNote error={error} />
        </section>
        {skip}
      </div>
    );

  if (stage.s === "show")
    return (
      <div className="stack-lg">
        <KitSheet kit={stage.kit} />
        <div className="row-gap no-print">
          <button className="btn" onClick={() => window.print()}>Print</button>
          <button className="btn ghost" onClick={() => download(stage.kit)}>Download (.txt)</button>
        </div>
        <Tip>
          Print it or write the words down, in order. Downloaded files tend to end up in cloud backups: if you download the kit, move it to a USB stick and delete the original.
        </Tip>
        <button
          className="btn primary no-print"
          onClick={() => {
            setError(null);
            setStage({ s: "confirm", kit: stage.kit, ask: confirmPositions() });
          }}
        >
          I&apos;ve stored my kit
        </button>
        {skip}
      </div>
    );

  if (stage.s === "confirm")
    return (
      <section className="card stack" data-testid="kit-confirm">
        <h2>Check your kit</h2>
        <p className="dim">To make sure it&apos;s safely written down, type two of its words.</p>
        <div className="grid-2">
          {stage.ask.map((p, i) => (
            <label className="field" key={p}>
              <span>Word #{p + 1}</span>
              <input
                value={answers[i]}
                onChange={(e) => setAnswers(answers.map((a, j) => (j === i ? e.target.value : a)))}
                autoComplete="off"
                autoCapitalize="none"
                autoCorrect="off"
                spellCheck={false}
              />
            </label>
          ))}
        </div>
        <button className="btn primary" disabled={!answers[0].trim() || !answers[1].trim()} onClick={check}>Confirm</button>
        <ErrorNote error={error} />
        <button className="link center-self" onClick={() => setStage({ s: "show", kit: stage.kit })}>← Show my kit again</button>
      </section>
    );

  if (stage.s === "activate")
    return (
      <section className="card stack">
        <h2>Adding the kit to your account</h2>
        {busy ? <Spinner>Sealing the kit&apos;s key into a block…</Spinner> : <ErrorNote error={error} />}
        {!busy && error && <button className="btn primary" onClick={() => activate(stage.grant)}>Try again</button>}
      </section>
    );

  if (stage.s === "pending")
    return (
      <section className="card stack warn-card" data-testid="kit-pending">
        <h2>Kit saved, not active yet</h2>
        <p>
          Adding the kit to your account is a transaction, and it needs a tiny fee. Once you have some FANG, activate it from <b>Devices &amp; agents → Recovery kit</b>. Until then the words can&apos;t open anything.
        </p>
        {onFinish && <button className="btn primary" onClick={onFinish}>{finishLabel}</button>}
      </section>
    );

  return (
    <section className="card stack" data-testid="kit-done">
      <h2>Your recovery kit is active</h2>
      <p className="ok">✓ {stage.tx.summary}. <ExplorerLink href={explorer.tx(stage.tx.id)}>View ↗</ExplorerLink></p>
      <p className="dim">If you lose every device, choose <b>Restore with a recovery kit</b> on a new one. The words have been cleared from this page.</p>
      {onFinish && <button className="btn primary" onClick={onFinish}>{finishLabel}</button>}
    </section>
  );
}

/** The printable kit. Print styles (globals.css) hide everything else on the page. */
export function KitSheet({ kit }: { kit: Kit }) {
  return (
    <section className="card plate kit-sheet" data-testid="recovery-kit">
      <div className="center">
        <span className="eyebrow">Recovery kit</span>
        <h2 className="kit-title">{kit.name ?? shortAddr(kit.account)}</h2>
      </div>
      <ol className="kit-words" data-testid="kit-words">
        {kit.words.map((w, i) => (
          <li key={i}>
            <span className="kit-n">{i + 1}</span>
            <span className="kit-w">{w}</span>
          </li>
        ))}
      </ol>
      <dl className="kv kit-facts">
        <dt>Account</dt>
        <dd className="mono">{kit.account}</dd>
        <dt>Network</dt>
        <dd>{kit.network} · <span className="mono">{kit.chainId}</span></dd>
        <dt>Kit key</dt>
        <dd className="mono" data-testid="kit-key-id">{kit.keyId}</dd>
        <dt>Made</dt>
        <dd>{kit.created}</dd>
        <dt>To restore</dt>
        <dd>Open {kit.walletUrl}, choose <i>Restore with a recovery kit</i> and type the words. Then revoke the lost device and make a new kit.</dd>
      </dl>
      <p className="kit-warning">Anyone holding these words controls this account. Keep the kit offline, and only ever type the words into the Blockula wallet.</p>
    </section>
  );
}

function download(kit: Kit) {
  const url = URL.createObjectURL(new Blob([kitText(kit)], { type: "text/plain" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = `blockula-recovery-kit-${(kit.name ?? kit.account.slice(0, 12)).replace(/[^a-z0-9.-]/gi, "")}.txt`;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
