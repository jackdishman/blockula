"use client";

import type { SeanceClaimRef } from "@blockula/sdk";
import { useEffect, useState } from "react";
import { ErrorNote, ExplorerLink, Spinner } from "@/components/ui";
import { friendly } from "@/lib/errors";
import { ballotKey, type BallotStore, loadBallots, newSalt, prUrl, saveBallot, sealedBallot, spamIds, type Vote, voteString } from "@/lib/explorer/ballot";
import { to } from "@/lib/explorer/routes";
import type { SeanceView } from "@/lib/explorer/types";
import { useSpendCheck } from "@/lib/spend";
import { useWallet } from "@/lib/wallet";
import { useInlineUnlock } from "./ActionForm";
import { Section } from "./kit";

type Pair = [SeanceClaimRef, SeanceClaimRef];

/** This origin's localStorage, or null where the browser won't give it (blocked site data). */
function storage(): BallotStore | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
}

const CHOICES: { v: Vote; label: string }[] = [
  { v: "a", label: "A mattered more" },
  { v: "=", label: "About the same" },
  { v: "b", label: "B mattered more" },
];

/** A claim, as the chain names it: repo and PR as text, and a link to the PR only when the repo is a plain `owner/name`. */
function Claim({ c, side }: { c: SeanceClaimRef; side: "A" | "B" }) {
  const url = prUrl(c.repo, c.pr);
  const text = `${c.repo}#${c.pr}`;
  return (
    <span className="ballot-claim">
      <b>{side}</b>{" "}
      {url ? (
        <a href={url} target="_blank" rel="noopener noreferrer nofollow" data-testid="pr-link">
          {text} ↗
        </a>
      ) : (
        <span className="mono break">{text}</span>
      )}{" "}
      <ExplorerLink href={to.claim(c.id)} className="dim small">
        claim #{c.id}
      </ExplorerLink>
    </span>
  );
}

/**
 * The juror's part of a Séance page, for the unlocked account when it holds a seat: its assigned
 * pairs, then sealing a judgment of them while the Séance seals, and revealing it while it reveals.
 * Only owner keys sit on juries, so an agent key is told so instead.
 */
export function JurorBallot({ s }: { s: SeanceView }) {
  const { wallet, account, client, hint, refresh } = useWallet();
  const check = useSpendCheck();
  const unlock = useInlineUnlock();
  const me = account?.account.address ?? hint?.account ?? null;
  const seat = me ? s.jurors.find((j) => j.juror.address === me) : undefined;
  const [pairs, setPairs] = useState<Pair[] | null>(null);
  const [picks, setPicks] = useState<(Vote | undefined)[]>([]);
  const [spam, setSpam] = useState<Set<number>>(new Set());
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<{ what: "seal" | "reveal"; id: string; summary: string } | null>(null);

  const seated = !!seat;
  useEffect(() => {
    if (!seated || !me) return;
    let live = true;
    client
      .seancePairs(s.id, me)
      .then((p) => live && setPairs(p))
      .catch((e) => live && setError(friendly(e)));
    return () => {
      live = false;
    };
  }, [client, s.id, me, seated]);

  if (!seat || !me) return null;
  const key = ballotKey(client.pin, me, s.id);
  const ballot = sealedBallot(loadBallots(storage(), key), s.id, me, seat.commitment);
  const myKey = account?.keys.find((k) => k.key_id === wallet?.keyId);
  const agentKey = myKey?.role.type === "session";

  const run = async (what: "seal" | "reveal", act: () => Promise<{ id: string; summary: string }>) => {
    setBusy(what);
    setError(null);
    try {
      // A judgment is signed only right after the passkey checks it's you.
      setBusy("check");
      await check();
      setBusy(what);
      setDone({ what, ...(await act()) });
      refresh();
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(null);
    }
  };

  const seal = () => {
    if (!wallet || !pairs) return;
    const v = voteString(picks, pairs.length);
    if ("missing" in v) return setError(`Judge every pair first: ${v.missing} still ${v.missing === 1 ? "needs" : "need"} a vote.`);
    const flagged = spamIds(spam, pairs.flat().map((c) => c.id));
    const b = { votes: v.votes, spam: flagged, salt: newSalt() };
    // The salt is stored before anything is signed, so a seal that lands always has it here.
    if (!saveBallot(storage(), key, b)) return setError("This browser won't store your ballot (private window, or site data blocked), and revealing needs it. Nothing was sealed. Seal from a normal window, or with the CLI.");
    return run("seal", async () => {
      const r = await wallet.sealVotes(s.id, b.votes, b.spam, b.salt, { pairs });
      return { id: r.tx.id, summary: r.tx.summary };
    });
  };

  const reveal = () => {
    if (!wallet || !ballot) return;
    return run("reveal", async () => {
      const tx = await wallet.revealVotes(s.id, ballot.votes, ballot.spam, ballot.salt);
      return { id: tx.id, summary: tx.summary };
    });
  };

  const body = () => {
    if (!wallet) {
      return (
        <button className="btn primary self-start" disabled={!!busy} onClick={() => unlock().catch((e) => setError(friendly(e)))}>
          Unlock to judge
        </button>
      );
    }
    if (agentKey) return <p className="warn small" data-testid="juror-agent-key">This device signs with an agent key (“{myKey?.label}”). Agent keys can only send transfers, and jurors must be people: seal and reveal from an owner device.</p>;
    if (s.phase === "closed") return <p className="dim m0">This Séance is closed.</p>;
    if (s.phase === "revealing") {
      if (seat.revealed) return <p className="ok m0">✓ Your votes are revealed.</p>;
      if (!seat.committed) return <p className="dim m0">You didn&apos;t seal a judgment in time, so there&apos;s nothing to reveal.</p>;
      if (!ballot) return <p className="warn m0" data-testid="salt-elsewhere">Your sealed ballot&apos;s secret salt isn&apos;t in this browser. Reveal from the device that sealed it, or with the CLI that did (<span className="mono">blockula contrib reveal</span>), before block #{s.reveal_until}.</p>;
      return (
        <>
          <p className="m0">Reveal your sealed votes before block #{s.reveal_until}, or you lose part of your bond and 2 Soul.</p>
          <button className="btn primary self-start" disabled={!!busy} onClick={reveal}>
            {busy === "check" ? <Spinner>Waiting for your passkey…</Spinner> : busy === "reveal" ? <Spinner>Revealing…</Spinner> : "Reveal my votes"}
          </button>
        </>
      );
    }
    if (s.phase !== "sealing") return null;
    if (seat.committed || done?.what === "seal") {
      // Sent from here, or seen on chain: no second seal from this page (the chain takes one per juror).
      return ballot || !seat.committed ? (
        <p className="ok m0" data-testid="sealed-here">✓ Sealed from this browser. Come back after block #{s.commit_until} to reveal, and don&apos;t clear this site&apos;s data before then.</p>
      ) : (
        <p className="dim m0">You sealed a judgment from another device or the CLI. Reveal it from there after block #{s.commit_until}.</p>
      );
    }
    if (!pairs) return error ? null : <Spinner>Reading your pairs…</Spinner>;
    return (
      <>
        <p className="m0">For each pair, which pull request mattered more to the project? Mark a claim as spam only if it isn&apos;t real work.</p>
        <ol className="ballot" data-testid="ballot-pairs">
          {pairs.map(([a, b], i) => (
            <li key={`${i}-${a.id}-${b.id}`} className="ballot-pair" data-testid="ballot-pair">
              <div className="ballot-claims">
                <Claim c={a} side="A" />
                <Claim c={b} side="B" />
              </div>
              <div className="ballot-choices" role="radiogroup" aria-label={`Pair ${i + 1}`}>
                {CHOICES.map((c) => (
                  <label key={c.v} className="ballot-choice">
                    <input type="radio" name={`pair-${s.id}-${i}`} checked={picks[i] === c.v} onChange={() => setPicks((p) => Object.assign([...p], { [i]: c.v }))} />
                    {c.label}
                  </label>
                ))}
              </div>
            </li>
          ))}
        </ol>
        <fieldset className="ballot-spam">
          <legend className="small">Spam (optional)</legend>
          {[...new Map(pairs.flat().map((c) => [c.id, c])).values()].map((c) => (
            <label key={c.id} className="ballot-choice">
              <input type="checkbox" checked={spam.has(c.id)} onChange={(e) => setSpam((x) => { const y = new Set(x); if (e.target.checked) y.add(c.id); else y.delete(c.id); return y; })} />
              <span className="mono break">{`${c.repo}#${c.pr}`}</span> is spam
            </label>
          ))}
        </fieldset>
        <p className="warn small m0" data-testid="salt-warning">
          Sealing keeps a secret salt in this browser until you reveal (after block #{s.commit_until}). If you clear this site&apos;s data before then, your vote can&apos;t be revealed and you pay the no-show penalty: part of your bond and 2 Soul.
        </p>
        <button className="btn primary self-start" disabled={!!busy} onClick={seal}>
          {busy === "check" ? <Spinner>Waiting for your passkey…</Spinner> : busy === "seal" ? <Spinner>Sealing…</Spinner> : `Seal my votes on ${pairs.length} pair${pairs.length === 1 ? "" : "s"}`}
        </button>
      </>
    );
  };

  return (
    <Section title="Your ballot" className="stack" aside={<span className="dim small">you sit on this jury</span>}>
      <div className="stack" data-testid="juror-ballot">
        {body()}
        {done && (
          <p className="ok m0" data-testid="ballot-done">
            ✓ {done.summary}. <ExplorerLink href={to.tx(done.id)}>View ↗</ExplorerLink>
          </p>
        )}
        <ErrorNote error={error} />
      </div>
    </Section>
  );
}

