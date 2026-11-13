"use client";

// The spectator view of one hand: the table's drawing, where the hand stands, the note and a link
// out to the full page. Read-only: it has no wallet, signs nothing and writes nothing to storage.
import "../blackjack.css";
import { outcomeWords, snapshotOfGame, snapshotOfSettled, type TableSnapshot } from "@blockula/blackjack";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { TableFigure } from "@/components/blackjack/TableFigure";
import { useTable } from "@/components/blackjack/useTable";
import { checkSettledHand, payoutWords } from "@/lib/blackjack";
import { COPY, NOTE } from "@/lib/blackjack-copy";
import { shareTarget, targetQuery } from "@/lib/blackjack-share";
import { BLACKJACK_TABLE } from "@/lib/config";

export default function EmbedView() {
  const params = useSearchParams();
  const target = shareTarget({ table: params.get("table"), player: params.get("player"), game: params.get("game") }, BLACKJACK_TABLE);
  // The full page opens in its own tab: a frame is no place to sign anything.
  const out = (
    <Link href={`/blackjack${targetQuery({ table: target.table })}`} target="_blank" rel="noopener" prefetch={false} data-testid="bj-embed-open">
      {COPY.embed.open} ↗
    </Link>
  );
  return (
    <main className="bj-embed" data-testid="bj-embed">
      {target.table && (target.player || target.game) ? <Watch table={target.table} player={target.player ?? null} game={target.game ? BigInt(target.game) : null} /> : <p className="bj-embed-empty">{COPY.embed.nothing}</p>}
      <div className="bj-embed-foot">
        <p className="bj-note" data-testid="bj-note">
          {NOTE}
        </p>
        {out}
      </div>
    </main>
  );
}

function Watch({ table, player, game }: { table: string; player: string | null; game: bigint | null }) {
  const live = useTable(table, { player, gameId: game });
  const s = live.state;
  if (!s) return <p className="bj-embed-empty">{live.error ? COPY.notATable : "Reading the table…"}</p>;
  let snapshot: TableSnapshot | null = null;
  let words: string = COPY.embed.noHand;
  if (s.game) {
    snapshot = snapshotOfGame(s.game);
  } else if (game !== null && !player) words = COPY.embed.gone;
  else if (s.player?.last) {
    // A settled hand's record keeps no cards: they are recomputed here from its last link and its seed.
    const last = s.player.last;
    const check = checkSettledHand(last);
    if (check.ok) snapshot = snapshotOfSettled(last, { player: check.player, dealer: check.dealer });
    words = check.ok ? `${outcomeWords(last.outcome)}. ${payoutWords(last.stake, last.payout)}` : check.verdict;
  }
  return (
    <>
      {/* The drawing says where the hand stands (and its label says it in words); without one, a line does. */}
      {snapshot ? (
        <TableFigure snapshot={snapshot} list={false} />
      ) : (
        <p className="bj-embed-empty" role="status" data-testid="bj-embed-status">
          {words}
        </p>
      )}
    </>
  );
}
