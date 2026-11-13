"use client";

// "Check this hand": every card recomputed in this browser from what the chain recorded (the newest
// link revealed, how many were, the player's seed), each link hashed down to the commitment, and
// the outcome and the payout held to the rules. The work is lib/blackjack.ts `checkOpenHand` and
// `checkSettledHand` over the shared library's `verifyHand`; nothing here reads a card from a
// server's rendering.
import { outcomeWords } from "@blockula/blackjack";
import type { HandCheck } from "@/lib/blackjack";
import { cardsText, fangText, payoutWords, totalText } from "@/lib/blackjack";
import { COPY } from "@/lib/blackjack-copy";

export function VerifyView({ check, mine }: { check: HandCheck; mine: boolean }) {
  return (
    <div className="stack" data-testid="bj-verify" data-ok={check.ok ? "yes" : "no"}>
      <p className={`bj-verdict ${check.ok ? (check.commitCheck === "matches" ? "ok" : "") : "bad"}`} role="status" data-testid="bj-verdict">
        {check.verdict}
      </p>
      <dl className="details tight-150">
        <dt>Hand</dt>
        <dd>{check.game.toString()}</dd>
        <dt>{COPY.verify.seed}</dt>
        <dd>
          <span className="mono break" data-testid="bj-verify-seed">{check.clientSeed}</span>
          <span className="dim small block">{check.seed === "yours" ? COPY.verify.seedYours : COPY.verify.seedUnknown}</span>
        </dd>
        {check.ok && (
          <>
            <dt>{COPY.verify.commitment}</dt>
            <dd>
              <span className="mono break" data-testid="bj-verify-commit">{check.commit}</span>
              <span className="dim small block" data-testid="bj-verify-commit-check">{check.commitCheck === "matches" ? COPY.verify.commitmentMatches : COPY.verify.commitmentUnknown}</span>
            </dd>
            <dt>Links</dt>
            <dd>
              {check.steps.length === 0 ? (
                <span className="dim">{COPY.verify.noLinks}</span>
              ) : (
                <ol className="bj-steps" data-testid="bj-verify-steps">
                  {check.steps.map((s) => (
                    <li key={s.index}>
                      <span className="mono break">{s.link}</span>
                      <span className="block">
                        gives card {s.index}: <b>{s.label}</b> <span className="dim">({s.name})</span>, to {s.to === "dealer" ? "the dealer" : mine ? "you" : "the player"}
                      </span>
                    </li>
                  ))}
                </ol>
              )}
              <span className="dim small block">{COPY.verify.links}</span>
            </dd>
            <dt>{mine ? "Your cards" : "Player's cards"}</dt>
            <dd data-testid="bj-verify-player">
              {cardsText(check.player)} <span className="dim">({totalText(check.player)})</span>
            </dd>
            <dt>Dealer&apos;s cards</dt>
            <dd data-testid="bj-verify-dealer">
              {cardsText(check.dealer)} <span className="dim">({totalText(check.dealer)})</span>
            </dd>
            <dt>Outcome</dt>
            <dd data-testid="bj-verify-outcome">
              {check.outcome === null ? (
                <span className="dim">The hand isn&apos;t settled yet.</span>
              ) : (
                <>
                  {outcomeWords(check.outcome)}. {check.payout !== null && payoutWords(check.stake, check.payout)}
                  {check.forfeited && <span className="dim small block">{COPY.verify.forfeited}</span>}
                </>
              )}
            </dd>
            <dt>Bet</dt>
            <dd>{fangText(check.stake)}</dd>
          </>
        )}
      </dl>
    </div>
  );
}
