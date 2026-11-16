"use client";

// The blackjack table (docs/BLACKJACK.md): one hand against the dealer, played with FANG. The
// player signs `bet`, `reveal_seed`, `hit` and `stand` with their own passkey wallet, each through
// the wallet's review-then-sign flow for program calls (components/blackjack/TableCall.tsx).
//
// A bet is two transactions. The first carries the stake and only the hash of a seed this browser
// made and keeps (lib/blackjack.ts `rememberSeed`). Once it is in a block the page presents the
// second, `reveal_seed`, by itself: on the bet's receipt, and on any later load where the player's
// hand still waits for its seed and this browser holds the seed that opens the hand's hash. A hand
// whose seed isn't here can't be revealed from here, and the page says so. A separate dealer
// reveals the cards: this page only waits, shows the deadline, and offers the claim when the dealer
// misses it. Everything on the table is read from the program's stored records, and every card can
// be recomputed here ("Check this hand").
//
// The cards lead: the table, the hand (open, or the last one settled), its status and the buttons
// that act on it, with "Play again" once a hand is settled. The rules, the fairness notes, the
// check, the log, the table's record and the table balance sit under one closed "Fairness &
// details".
import "@/app/explorer.css";
import "./blackjack.css";
import { Phase, call, eventNamed, eventsOf, fang, outcomeWords, seedHash, snapshotOfGame, snapshotOfSettled, type Game, type LastHand, type TableEvent } from "@blockula/blackjack";
import { checkReceipt } from "@blockula/sdk";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { CallerNote, useTableSigner, type CallRequest, type TableSigner } from "@/components/blackjack/TableCall";
import { TableFigure } from "@/components/blackjack/TableFigure";
import { useTable, type TableState } from "@/components/blackjack/useTable";
import { VerifyView } from "@/components/blackjack/Verify";
import { Stat } from "@/components/explorer/kit";
import { Spinner } from "@/components/ui";
import {
  ADULT_KEY,
  addHandTx,
  betProblem,
  betPrefill,
  bindBet,
  checkOpenHand,
  checkSettledHand,
  chooseTable,
  dailyLeft,
  fangText,
  grantProblem,
  handState,
  logLine,
  maxBetNow,
  newSeed,
  noteBetTx,
  noteRevealTx,
  parseAmount,
  payoutWords,
  playAgain,
  rememberSeed,
  seatOf,
  storeKey,
  storedHand,
  tableProblem,
  type BetContext,
  type KeyValue,
  type StoredHand,
} from "@/lib/blackjack";
import { COPY, NOTE, rulesText } from "@/lib/blackjack-copy";
import { BLACKJACK_TABLE } from "@/lib/config";
import { inertText } from "@/lib/explorer/inert";
import { OTHER_NODES, nodeGet } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import { useWallet } from "@/lib/wallet";

const storage = (): KeyValue | null => {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
};

/** The note, as every blackjack page shows it. */
export function Note() {
  return (
    <p className="bj-note" data-testid="bj-note">
      {NOTE}
    </p>
  );
}

export default function BlackjackView() {
  const params = useSearchParams();
  const { client } = useWallet();
  const choice = chooseTable(params.get("table"), BLACKJACK_TABLE, client.pin.prefix);
  return (
    <main className="wide stack-lg bj">
      <div className="page-head">
        <span className="eyebrow">{COPY.eyebrow}</span>
        <h1>{COPY.title}</h1>
        <span className="sub">{COPY.sub}</span>
      </div>
      <Note />
      {choice.state === "none" && (
        <section className="card" data-testid="bj-no-table">
          <h2>{COPY.noTable.title}</h2>
          <p className="m0">{COPY.noTable.body}</p>
        </section>
      )}
      {choice.state === "bad" && (
        <section className="card" data-testid="bj-bad-table">
          <h2>{COPY.badTable.title}</h2>
          <p className="m0">
            {choice.from === "link" ? COPY.badTable.fromLink : COPY.badTable.fromBuild} {choice.why}
          </p>
          <p className="mono break dim">{inertText(choice.given, 120)}</p>
        </section>
      )}
      {choice.state === "ok" && <TablePage key={choice.address} address={choice.address} />}
    </main>
  );
}

/** The 18+ answer, kept in this browser. */
function useAdult(): [boolean, (v: boolean) => void] {
  const [adult, setAdult] = useState(false);
  useEffect(() => {
    try {
      setAdult(storage()?.getItem(ADULT_KEY) === "1");
    } catch {
      /* storage unavailable: ask each time */
    }
  }, []);
  return [
    adult,
    (v) => {
      setAdult(v);
      try {
        if (v) window.localStorage.setItem(ADULT_KEY, "1");
        else window.localStorage.removeItem(ADULT_KEY);
      } catch {
        /* storage unavailable: the answer holds for this visit */
      }
    },
  ];
}

/** The table events in the receipts of the transactions this page sent for a hand, in order. */
function useHandEvents(program: string, txs: readonly string[], tick: unknown): TableEvent[] {
  const { client } = useWallet();
  const cache = useRef(new Map<string, TableEvent[]>());
  // A transaction this page just signed isn't on the node yet: it's read from the next poll on.
  const known = useRef<Set<string> | null>(null);
  const [, setVersion] = useState(0);
  const ids = txs.join(",");
  useEffect(() => {
    let live = true;
    const list = ids ? ids.split(",") : [];
    const first = known.current === null;
    const seen = (known.current ??= new Set(list));
    for (const id of list) {
      if (cache.current.has(id)) continue;
      if (!first && !seen.has(id)) {
        seen.add(id);
        continue;
      }
      nodeGet<unknown>(`/tx/${id}`).then(
        (tx) => {
          try {
            const receipt = checkReceipt(client.rpc, tx);
            // No receipt yet: still pending. It's read again on the next poll.
            if (!receipt) return;
            cache.current.set(id, eventsOf(receipt, program));
            if (live) setVersion((v) => v + 1);
          } catch {
            cache.current.set(id, []);
          }
        },
        () => undefined,
      );
    }
    return () => {
      live = false;
    };
  }, [ids, program, client, tick]);
  return (ids ? ids.split(",") : []).flatMap((id) => cache.current.get(id) ?? []);
}

function TablePage({ address }: { address: string }) {
  const { client, wallet, hint, account, status } = useWallet();
  const me = hint?.account ?? null;
  const live = useTable(address, { player: me });
  const signer = useTableSigner(address, () => void live.reload());
  const [adult, setAdult] = useAdult();
  const [stored, setStored] = useState(0);
  const touch = useCallback(() => setStored((n) => n + 1), []);
  const blockMs = status?.block_time_ms ?? 1000;
  const chainId = status?.chain_id ?? client.pin.chainId ?? "";
  const key = storeKey(chainId, address);
  const myName = account?.names[0] ?? hint?.name;
  const s = live.state;

  const seat = s?.player ? seatOf(s.player, s.game) : null;
  const open = seat?.state === "open" ? seat.game : null;
  const last = seat?.last ?? null;
  // What this browser kept for the open hand and for the last one, re-read after each of its own writes.
  // While the chain has only a seed's hash, the hand's seed is found by that hash.
  const openKept = useMemo(() => (open ? storedHand(storage(), key, open.id, open.clientSeed, { account: open.player, seedHash: open.seedHash }) : null), [open, key, stored]);
  const lastKept = useMemo(() => (last && me ? storedHand(storage(), key, last.game, last.clientSeed, { account: me, seedHash: last.seedHash }) : null), [last, me, key, stored]);

  // The hand this browser's seed belongs to is filed under its id as soon as the chain shows it,
  // so the seed's reveal and every later move are kept with it (the bet's receipt may never have
  // been read here).
  const filed = useRef(new Set<string>());
  useEffect(() => {
    if (!open || !openKept || open.player !== me) return;
    const id = `${key}/${open.id}`;
    if (filed.current.has(id)) return;
    filed.current.add(id);
    bindBet(storage(), key, openKept.seed, { game: open.id, seedHash: open.seedHash ?? openKept.seedHash });
    touch();
  }, [open, openKept, me, key, touch]);

  // Step 2 of 2, presented by itself: the hand waits for its seed, the seed is here, and there is
  // still time. Once per hand; after that the hand's own button brings the review back.
  const canSignNow = signer.caller.kind === "owner";
  const start = useRef(signer.start);
  start.current = signer.start;
  const presented = useRef(new Set<string>());
  const openId = open?.id ?? null;
  const awaitsSeed = !!open && open.phase === Phase.Seed && open.player === me && !!openKept && BigInt(s?.house.height ?? 0) < open.deadline;
  const reveal = useCallback(() => {
    if (!open || !openKept) return;
    void start.current(revealRequest(open.id, openKept.seed, key, touch));
  }, [open, openKept, key, touch]);
  useEffect(() => {
    if (!awaitsSeed || openId === null || !canSignNow || signer.active || !signer.info) return;
    const id = `${key}/${openId}`;
    if (presented.current.has(id)) return;
    presented.current.add(id);
    reveal();
  }, [awaitsSeed, openId, canSignNow, signer.active, signer.info, key, reveal]);

  // A bet whose receipt this page never read (it was closed, or the answer was lost): its hand is
  // found by its seed, and the commitment comes from the bet's receipt once.
  const recovered = useRef(new Set<string>());
  useEffect(() => {
    const kept = openKept;
    if (!open || !kept || kept.commit || !kept.betTx || recovered.current.has(kept.betTx)) return;
    recovered.current.add(kept.betTx);
    nodeGet<unknown>(`/tx/${kept.betTx}`).then(
      (tx) => {
        try {
          const receipt = checkReceipt(client.rpc, tx);
          const bet = receipt && eventNamed(eventsOf(receipt, address), "bet");
          if (bet && me && bet.seedHash === seedHash(me, kept.seed)) {
            bindBet(storage(), key, kept.seed, { game: bet.game, commit: bet.commit, txId: kept.betTx, seedHash: bet.seedHash });
            touch();
          }
        } catch {
          /* no receipt to read: the check says the commitment isn't held */
        }
      },
      () => undefined,
    );
  }, [open, openKept, me, client, address, key, touch]);

  if (live.error && !s)
    return (
      <section className="card" data-testid="bj-not-a-table">
        <h2>{COPY.badTable.title}</h2>
        <p className="m0">{COPY.notATable}</p>
        <p className="mono break dim">{address}</p>
        <p className="dim small">{inertText(live.error, 300)}</p>
        <Link href={to.deployed(address)}>The program&apos;s page</Link>
      </section>
    );
  if (!s) return <Spinner>Reading the table…</Spinner>;

  const height = s.house.height;
  const transferable = account ? BigInt((account as unknown as { transferable?: string }).transferable ?? account.balance) : null;
  const ctx: BetContext | null = s.player ? { limits: s.limits, house: s.house, player: s.player, height, commitmentReady: s.commitmentReady, transferable } : null;
  const canSign = signer.caller.kind === "owner";

  const most = maxBetNow(s.limits, s.house);

  return (
    <>
      {signer.infoError && (
        <p className="warn m0" role="alert" data-testid="bj-info-error">
          {signer.infoError}
        </p>
      )}
      <section className="card bj-play" data-testid="bj-play">
        <div className="card-head">
          <h2>{!open && last && me ? COPY.hand.lastTitle : COPY.hand.title}</h2>
          {me && (open || last) && <span className="dim small">hand {(open ? open.id : last!.game).toString()}</span>}
        </div>
        {!me ? (
          <CallerNote signer={signer} />
        ) : seat?.state === "settling" ? (
          <Spinner>{COPY.hand.settling}</Spinner>
        ) : open ? (
          <OpenHand game={open} me={me} myName={myName} height={height} blockMs={blockMs} adult={adult} signer={signer} kept={openKept} storeKey={key} touch={touch} reveal={reveal} />
        ) : (
          <>
            <div className={last ? "cols bj-hand" : "bj-hand"}>
              {last && <LastHandView last={last} kept={lastKept} myName={myName} me={me} />}
              <div className="stack">
                {!last && <p className="m0 dim" data-testid="bj-no-hand">{COPY.hand.none}</p>}
                {/* A new form for each settled hand: it starts from that hand's stake. */}
                {ctx && <BetForm key={last ? last.game.toString() : "first"} ctx={ctx} me={me} adult={adult} signer={signer} storeKey={key} touch={touch} canSign={canSign} last={last} />}
              </div>
            </div>
            {signer.panel}
          </>
        )}
        {me && (
          <div className="bj-gate-row">
            <label className="bj-gate" data-testid="bj-gate">
              <input type="checkbox" checked={adult} onChange={(e) => setAdult(e.target.checked)} />
              <span>{COPY.gate.label}</span>
            </label>
            <span className="dim small">{COPY.gate.why}</span>
            {wallet && s.player && (
              <span className="dim small" data-testid="bj-daily">
                Left to bet today: {fangText(dailyLeft(s.limits, s.player, height))}.
              </span>
            )}
          </div>
        )}
      </section>

      <details className="card bj-more" data-testid="bj-details">
        <summary className="bj-summary" data-testid="bj-details-summary">
          {COPY.details.title} <span className="dim small bj-summary-hint">{COPY.details.hint}</span>
        </summary>
        <div className="stack-lg bj-more-body">
          <p className="m0 dim">{COPY.details.lede}</p>
          {me && open ? <OpenHandDetails game={open} me={me} kept={openKept} tick={s} program={address} /> : me && last ? <LastHandDetails last={last} kept={lastKept} me={me} /> : null}
          <div className="stack">
            <h3 className="bj-rule">{COPY.details.table}</h3>
            <div className="figures" data-testid="bj-house">
              <Stat v={fang(s.house.bankroll)} k="FANG in the house's bankroll" />
              <Stat v={`${s.house.open} of ${s.limits.maxOpenGames}`} k="hands open" />
              <Stat v={`${fang(s.limits.minBet)} to ${fang(most < s.limits.minBet ? s.limits.maxBet : most)}`} k="FANG a bet, right now" />
              <Stat v={s.house.paused ? "paused" : "open"} k="new bets" />
            </div>
            <p className="card-note m0" data-testid="bj-table">
              The table is the program <span className="mono break">{address}</span>. <Link href={to.deployed(address)}>See the program&apos;s record</Link>
            </p>
          </div>
          <div className="stack" data-testid="bj-rules">
            {rulesText(s.limits, blockMs).map((r) => (
              <div key={r.title}>
                <h3 className="bj-rule">{r.title}</h3>
                {r.body.map((p, i) => (
                  <p key={i} className="bj-rule-p">
                    {p}
                  </p>
                ))}
              </div>
            ))}
          </div>
          {me && s.player && <BalancePanel state={s} signer={signer} adult={adult} height={height} prefix={client.pin.prefix} />}
        </div>
      </details>
    </>
  );
}

/** The second transaction of a bet: the seed this browser kept for hand `game`, for review. */
function revealRequest(game: bigint, seed: string, key: string, touch: () => void): CallRequest {
  return {
    call: call.revealSeed(game, seed),
    does: `Reveal your seed for hand ${game} (${COPY.seed.step})`,
    waiting: COPY.seed.waiting,
    notes: [
      {
        k: COPY.seed.label,
        v: (
          <>
            <span className="mono break" data-testid="bj-review-seed">{seed}</span>
            <span className="dim small block">{COPY.seed.note}</span>
          </>
        ),
      },
      {
        k: COPY.seed.sentLabel,
        // One node only is a warning, so it stays in the summary; several is a detail.
        show: !OTHER_NODES.length,
        v: <span className={OTHER_NODES.length ? "small" : "warn small"} data-testid="bj-review-nodes">{OTHER_NODES.length ? COPY.seed.sentMany(OTHER_NODES.length) : COPY.seed.sentOne}</span>,
      },
    ],
    warn: COPY.seed.warn,
    // The one call a node could profit from keeping to itself: it goes through every node the wallet knows.
    everyNode: true,
    onSigned: (id) => (noteRevealTx(storage(), key, game, id), touch()),
    onApplied: () => touch(),
  };
}

function BetForm({ ctx, me, adult, signer, storeKey: key, touch, canSign, last }: { ctx: BetContext; me: string; adult: boolean; signer: TableSigner; storeKey: string; touch: () => void; canSign: boolean; last: LastHand | null }) {
  // The field starts from the last hand's stake. An amount the table no longer takes says so at once.
  const [raw, setRaw] = useState(() => betPrefill(last));
  const [touched, setTouched] = useState(() => !!last && betProblem(betPrefill(last), ctx) !== null);
  const blocked = tableProblem(ctx);
  const problem = betProblem(raw, ctx);
  const most = maxBetNow(ctx.limits, ctx.house);
  // "Play again": the last stake goes in the field and through the same checks and the same review.
  const again = () => {
    if (!last) return;
    const next = playAgain(last, ctx);
    setRaw(next.amount);
    place(next.amount);
  };
  const review = () => place(raw);
  const place = (amount: string) => {
    setTouched(true);
    const stake = parseAmount(amount);
    if (betProblem(amount, ctx) || stake === null || !adult) return;
    // A new seed for every bet, kept in this browser before anything is signed: the bet sends
    // only its hash, and the seed must still be here to be revealed once the bet is in a block.
    const seed = newSeed();
    const hash = seedHash(me, seed);
    rememberSeed(storage(), key, seed, Date.now(), hash);
    touch();
    void signer.start({
      call: call.bet(stake, hash),
      does: `Bet ${fangText(stake)} on a new hand (${COPY.bet.step})`,
      waiting: COPY.bet.waiting,
      // The bet's input is the seed's hash, so the review says so once, beside the input.
      inputHint: COPY.bet.seed,
      onSigned: (id) => (noteBetTx(storage(), key, seed, id), touch()),
      onApplied: (id, events) => {
        // Filed under its hand with the commitment it took. The page then presents the seed's reveal by itself.
        const bet = eventNamed(events, "bet");
        if (bet && bet.seedHash === hash) bindBet(storage(), key, seed, { game: bet.game, commit: bet.commit, txId: id, seedHash: hash });
        touch();
      },
    });
  };
  return (
    <div className="stack" data-testid="bj-bet-form">
      {last && (
        <div className="stack bj-again">
          <button className="btn primary big self-start" disabled={!!blocked || !adult || !canSign || signer.active} onClick={again} data-testid="bj-play-again">
            {COPY.bet.again}
          </button>
          {!blocked && <span className="dim small" data-testid="bj-play-again-note">{COPY.bet.againNote(fangText(last.stake))}</span>}
        </div>
      )}
      {!last && <h3 className="bj-rule">{COPY.bet.title}</h3>}
      {blocked ? (
        <p className="warn m0" data-testid="bj-bet-blocked">{blocked}</p>
      ) : (
        <>
          <label className="field">
            <span>{COPY.bet.label}</span>
            <div className="suffix-input">
              <input value={raw} onChange={(e) => setRaw(e.target.value)} onBlur={() => setTouched(true)} inputMode="decimal" placeholder={fang(ctx.limits.minBet)} aria-label={COPY.bet.label} disabled={!adult || !canSign || signer.active} data-testid="bj-bet-input" />
              <span className="suffix">FANG</span>
            </div>
            {touched && problem ? (
              <span className="warn small" data-testid="bj-bet-problem">{problem}</span>
            ) : (
              <span className="dim small" data-testid="bj-bet-range">
                From {fangText(ctx.limits.minBet)} to {fangText(most)} right now. {fangText(dailyLeft(ctx.limits, ctx.player, ctx.height))} left today.
              </span>
            )}
          </label>
          {!adult && <span className="warn small" data-testid="bj-gate-needed">{COPY.gate.needed}</span>}
          {canSign ? (
            <button className={`btn self-start${last ? "" : " primary"}`} disabled={!adult || signer.active} onClick={review} data-testid="bj-bet-review">
              {COPY.bet.review}
            </button>
          ) : (
            <CallerNote signer={signer} />
          )}
          <span className="dim small" data-testid="bj-bet-two">{COPY.bet.two}</span>
        </>
      )}
    </div>
  );
}

function OpenHand({ game, me, myName, height, blockMs, adult, signer, kept, storeKey: key, touch, reveal }: { game: Game; me: string; myName?: string; height: number; blockMs: number; adult: boolean; signer: TableSigner; kept: StoredHand | null; storeKey: string; touch: () => void; reveal: () => void }) {
  const hs = handState(game, me, height, blockMs);
  const snapshot = useMemo(() => snapshotOfGame(game, { player: myName, txId: kept?.betTx }), [game, myName, kept?.betTx]);
  const canSign = signer.caller.kind === "owner";
  const move = (what: "hit" | "stand" | "timeout", does: string) =>
    void signer.start({
      call: what === "hit" ? call.hit(game.id) : what === "stand" ? call.stand(game.id) : call.timeout(game.id),
      does: `${does} (hand ${game.id})`,
      onSigned: (id) => (addHandTx(storage(), key, game.id, id), touch()),
    });
  const disabled = !adult || !canSign || signer.active;
  const seedStep = hs.waitsFor === "seed" && hs.mine;
  return (
    <div className="stack" data-testid="bj-open-hand">
      <div className="cols bj-hand">
      <TableFigure snapshot={snapshot} mine={hs.mine} />
      <div className="stack">
      <p className="m0 bj-status" role="status" data-testid="bj-status">
        {hs.words}
      </p>
      <p className="m0 dim small" data-testid="bj-deadline">
        Deadline: block {game.deadline.toLocaleString("en-US")}. The chain is at block {height.toLocaleString("en-US")}.
      </p>
      {game.table && <p className="m0 dim small">{COPY.moves.relayed}</p>}
      {seedStep && !hs.seedLate && kept && (
        // Step 2 of 2. The review is presented by itself; this brings it back after "Back" or a failure.
        <div className="stack" data-testid="bj-seed-step">
          <div className="row-gap">
            <button className="btn primary" disabled={!canSign || signer.active} onClick={reveal} data-testid="bj-reveal">
              {COPY.seed.reveal}
            </button>
            <span className="dim small">{COPY.seed.step}</span>
          </div>
          <span className="warn small" data-testid="bj-seed-warn">{COPY.seed.warn}</span>
          {!canSign && <CallerNote signer={signer} />}
        </div>
      )}
      {seedStep && !hs.seedLate && kept?.revealTx && !signer.active && (
        // A reveal was signed here and the hand still has no seed: it isn't in a block.
        <p className="warn m0" role="alert" data-testid="bj-seed-unlanded">
          {COPY.seed.unlanded} {OTHER_NODES.length ? COPY.seed.unlandedMany(OTHER_NODES.length) : COPY.seed.unlandedOne} {COPY.seed.unlandedAgain}{" "}
          <Link href={to.tx(kept.revealTx)} data-testid="bj-seed-unlanded-tx">
            {COPY.seed.unlandedSee}
          </Link>
        </p>
      )}
      {seedStep && !hs.seedLate && !kept && (
        <p className="warn m0" role="alert" data-testid="bj-seed-missing">
          {COPY.seed.missing}
        </p>
      )}
      {hs.waitsFor === "seed" && hs.seedLate && (
        <div className="stack" data-testid="bj-seed-late">
          {hs.mine && <p className="warn m0">{COPY.seed.late}</p>}
          <div className="row-gap">
            <button className="btn" disabled={!canSign || signer.active} onClick={() => move("timeout", `${COPY.seed.end}: ${COPY.seed.endDoes.toLowerCase()}`)} data-testid="bj-end">
              {COPY.seed.end}
            </button>
          </div>
          {!canSign && <CallerNote signer={signer} />}
        </div>
      )}
      {(hs.canMove || hs.canClaim) && (
        <div className="row-gap" data-testid="bj-moves">
          {hs.canMove && (
            <>
              <button className="btn primary" disabled={disabled} onClick={() => move("hit", `${COPY.moves.hit}: ${COPY.moves.hitDoes.toLowerCase()}`)} data-testid="bj-hit">
                {COPY.moves.hit}
              </button>
              <button className="btn primary" disabled={disabled} onClick={() => move("stand", `${COPY.moves.stand}: ${COPY.moves.standDoes.toLowerCase()}`)} data-testid="bj-stand">
                {COPY.moves.stand}
              </button>
            </>
          )}
          {hs.canClaim && (
            <button className="btn primary" disabled={disabled} onClick={() => move("timeout", `${COPY.moves.claim}: ${COPY.moves.claimDoes.toLowerCase()}`)} data-testid="bj-claim">
              {COPY.moves.claim}
            </button>
          )}
        </div>
      )}
      {(hs.canMove || hs.canClaim) && !adult && <span className="warn small" data-testid="bj-gate-needed">{COPY.gate.needed}</span>}
      {(hs.canMove || hs.canClaim) && !canSign && <CallerNote signer={signer} />}
      </div>
      </div>
      {signer.panel}
    </div>
  );
}

/** The open hand's check and its log, for "Fairness & details". */
function OpenHandDetails({ game, me, kept, tick, program }: { game: Game; me: string; kept: StoredHand | null; tick: unknown; program: string }) {
  const [checking, setChecking] = useState(false);
  const events = useHandEvents(program, kept?.txs ?? [], tick);
  return (
    <div className="stack" data-testid="bj-hand-details">
      <div className="row-gap">
        <button className="btn small" onClick={() => setChecking((c) => !c)} data-testid="bj-check-open" aria-expanded={checking}>
          {COPY.verify.title}
        </button>
        <span className="dim small">{COPY.verify.lede}</span>
      </div>
      {checking && <VerifyView check={checkOpenHand(game, kept)} mine={game.player === me} />}
      <div data-testid="bj-log">
        <h3 className="bj-rule">{COPY.log.title}</h3>
        {events.length === 0 ? (
          <p className="m0 dim small">{COPY.log.empty}</p>
        ) : (
          <ol className="bj-log">
            {events.map((e, i) => {
              const line = logLine(e, me);
              return line ? <li key={i}>{line}</li> : null;
            })}
          </ol>
        )}
      </div>
    </div>
  );
}

/** The last hand on the table: its cards and how it ended. */
function LastHandView({ last, kept, myName, me }: { last: LastHand; kept: StoredHand | null; myName?: string; me: string | null }) {
  // The record keeps no cards: they are recomputed here, which is the check itself.
  const check = useMemo(() => checkSettledHand(last, kept, me ?? undefined), [last, kept, me]);
  const snapshot = useMemo(() => (check.ok ? snapshotOfSettled(last, { player: check.player, dealer: check.dealer }, { player: myName, txId: kept?.betTx }) : null), [check, last, myName, kept?.betTx]);
  return (
    <div className="stack" data-testid="bj-last-hand">
      {snapshot ? <TableFigure snapshot={snapshot} mine /> : <p className="warn m0">{check.verdict}</p>}
      <p className="m0 bj-status" role="status" data-testid="bj-last-outcome">
        {outcomeWords(last.outcome)}. {payoutWords(last.stake, last.payout)}
      </p>
    </div>
  );
}

/** The last hand's check, for "Fairness & details". */
function LastHandDetails({ last, kept, me }: { last: LastHand; kept: StoredHand | null; me: string | null }) {
  const [checking, setChecking] = useState(false);
  const check = useMemo(() => checkSettledHand(last, kept, me ?? undefined), [last, kept, me]);
  return (
    <div className="stack" data-testid="bj-hand-details">
      <div className="row-gap">
        <button className="btn small" onClick={() => setChecking((c) => !c)} data-testid="bj-check-last" aria-expanded={checking}>
          {COPY.verify.title}
        </button>
        <span className="dim small">{COPY.verify.lede}</span>
      </div>
      {checking && <VerifyView check={check} mine />}
    </div>
  );
}

/** The table balance, for play from X: add to it, take it back, let a relayer play it, end that. */
function BalancePanel({ state, signer, adult, height, prefix }: { state: TableState; signer: TableSigner; adult: boolean; height: number; prefix?: string }) {
  const p = state.player;
  const [amount, setAmount] = useState("");
  const [grant, setGrant] = useState({ relayer: "", maxBet: "", dailyCap: "", blocks: "" });
  const [said, setSaid] = useState<string | null>(null);
  if (!p) return null;
  const canSign = signer.caller.kind === "owner";
  const disabled = !canSign || signer.active;
  const drops = parseAmount(amount);
  const deposit = () => {
    if (drops === null || drops === 0n) return setSaid("Enter an amount of FANG, like 5.");
    if (p.balance + drops > state.limits.depositCap) return setSaid(`A table balance holds at most ${fangText(state.limits.depositCap)}.`);
    setSaid(null);
    void signer.start({ call: call.deposit(drops), does: `Add ${fangText(drops)} to your table balance` });
  };
  const withdraw = () => {
    if (drops === null || drops === 0n) return setSaid("Enter an amount of FANG, like 5.");
    if (drops > p.balance) return setSaid(`Your table balance holds ${fangText(p.balance)}.`);
    setSaid(null);
    void signer.start({ call: call.withdraw(drops), does: `Take ${fangText(drops)} back from your table balance` });
  };
  const makeGrant = () => {
    const problem = grantProblem(grant, state.limits, prefix);
    if (problem) return setSaid(problem);
    setSaid(null);
    const until = BigInt(height) + BigInt(grant.blocks.trim());
    const [maxBet, cap] = [parseAmount(grant.maxBet) as bigint, parseAmount(grant.dailyCap) as bigint];
    void signer.start({
      call: call.setRelayer(grant.relayer.trim(), maxBet, cap, until),
      does: `Let a relayer play your table balance: up to ${fangText(maxBet)} a bet and ${fangText(cap)} a day, until block ${until.toLocaleString("en-US")}`,
      notes: [{ k: "Relayer", v: <span className="mono break">{grant.relayer.trim()}</span>, show: true }, { k: "What it allows", v: COPY.balance.grant, show: true }],
    });
  };
  const field = (k: keyof typeof grant, label: string, placeholder: string, mono = false) => (
    <label className="field">
      <span>{label}</span>
      <input className={mono ? "mono" : undefined} value={grant[k]} onChange={(e) => setGrant((g) => ({ ...g, [k]: e.target.value }))} placeholder={placeholder} aria-label={label} autoCapitalize="none" autoCorrect="off" spellCheck={false} />
    </label>
  );
  return (
    <section className="card" data-testid="bj-balance">
      <details>
        <summary className="bj-summary">{COPY.balance.title}</summary>
        <div className="stack bj-balance-body">
          <p className="m0 dim">{COPY.balance.lede}</p>
          <p className="m0" data-testid="bj-balance-holds">
            {COPY.balance.holds}: <b>{fangText(p.balance)}</b> <span className="dim small">(at most {fangText(state.limits.depositCap)})</span>
          </p>
          <label className="field">
            <span>Amount</span>
            <div className="suffix-input">
              <input value={amount} onChange={(e) => setAmount(e.target.value)} inputMode="decimal" placeholder="5" aria-label="Table balance amount" />
              <span className="suffix">FANG</span>
            </div>
          </label>
          <div className="row-gap">
            <button className="btn" disabled={disabled || !adult} onClick={deposit} data-testid="bj-deposit">
              {COPY.balance.deposit}
            </button>
            <button className="btn" disabled={disabled} onClick={withdraw} data-testid="bj-withdraw">
              {COPY.balance.withdraw}
            </button>
          </div>
          <h3 className="bj-rule">{COPY.balance.grantTitle}</h3>
          <p className="m0 dim" data-testid="bj-grant-words">{COPY.balance.grant}</p>
          {p.grant ? (
            <p className="m0" data-testid="bj-grant">
              {COPY.balance.current}: <span className="mono break">{p.grant.relayer}</span> may bet up to {fangText(p.grant.maxBet)} a hand and {fangText(p.grant.dailyCap)} a day, until block {p.grant.expiresAt.toLocaleString("en-US")}
              {p.grant.expiresAt < BigInt(height) ? " (expired)" : ""}.
            </p>
          ) : (
            <p className="m0 dim">{COPY.balance.none}</p>
          )}
          <div className="grid-2">
            {field("relayer", COPY.balance.relayer, "boo1…", true)}
            {field("maxBet", `${COPY.balance.maxBet} (at most ${fang(state.limits.relayMaxBet)} FANG)`, "5")}
            {field("dailyCap", COPY.balance.dailyCap, "20")}
            {field("blocks", `${COPY.balance.blocks} (at most ${state.limits.grantMaxBlocks.toLocaleString("en-US")})`, "3600")}
          </div>
          <div className="row-gap">
            <button className="btn" disabled={disabled || !adult} onClick={makeGrant} data-testid="bj-grant-review">
              {COPY.balance.make}
            </button>
            {p.grant && (
              <button className="btn danger" disabled={disabled} onClick={() => void signer.start({ call: call.revoke(), does: "End the relayer's grant" })} data-testid="bj-revoke">
                {COPY.balance.revoke}
              </button>
            )}
          </div>
          {said && <p className="warn small m0" role="alert">{said}</p>}
          {!canSign && <CallerNote signer={signer} />}
          <p className="card-note">A review of each of these appears with your hand, at the top of the page.</p>
        </div>
      </details>
    </section>
  );
}
