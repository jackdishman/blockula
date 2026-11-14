/**
 * Writes pictures to a directory.
 *
 *   tsx src/cli.ts <outdir>            a gallery: the poster and a hand at each stage
 *   tsx src/cli.ts --poster <outdir>   poster.png and poster.alt.txt only
 */
import { mkdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { Outcome, Phase, payout, type TableSnapshot } from "@blockula/blackjack";
import { renderPoster, renderTable, type Picture } from "./index.js";

/** A card from its rank ("A", "2" … "10", "J", "Q", "K") and suit letter (s, h, d, c). */
const c = (label: string): number => ["A", "2", "3", "4", "5", "6", "7", "8", "9", "10", "J", "Q", "K"].indexOf(label.slice(0, -1)) + 13 * "shdc".indexOf(label.slice(-1));
const cards = (labels: string): number[] => labels.split(" ").map(c);

const FANG = 1_000_000_000n;
const TX = "9f2c41d07ab35e6610c4d8e2f7b90a13c5e87d24f6a1b0c39d7e52f48a6b1c0d";

function settled(id: bigint, player: string, dealer: string, outcome: Outcome, stake: bigint, holeHidden = false): TableSnapshot {
  return { gameId: id, playerCards: cards(player), dealerCards: cards(dealer), holeHidden, stake, phase: Phase.Done, outcome, payout: payout(stake, outcome), txId: TX, player: "mina.boo" };
}

/** The gallery's hands, by file name. */
export const GALLERY: Readonly<Record<string, TableSnapshot>> = {
  "waiting": { gameId: 40n, playerCards: [], dealerCards: [], holeHidden: false, stake: 5n * FANG, phase: Phase.Deal, outcome: null, payout: null, player: "mina.boo" },
  "seed-wait": { gameId: 39n, playerCards: [], dealerCards: [], holeHidden: false, stake: 5n * FANG, phase: Phase.Seed, outcome: null, payout: null, player: "mina.boo" },
  "deal": { gameId: 41n, playerCards: cards("9h 7c"), dealerCards: cards("Ks"), holeHidden: true, stake: 5n * FANG, phase: Phase.Player, outcome: null, payout: null, txId: TX, player: "mina.boo" },
  "player-turn": { gameId: 42n, playerCards: cards("5d 3s 2h 6c"), dealerCards: cards("10d"), holeHidden: true, stake: 2n * FANG + FANG / 2n, phase: Phase.Player, outcome: null, payout: null, txId: TX, player: "@a_rather_long_handle_that_keeps_going" },
  "blackjack": settled(43n, "As Kh", "9c", Outcome.Blackjack, 10n * FANG, true),
  "win": settled(44n, "10h 9s", "Qd 8c", Outcome.Win, 5n * FANG),
  "push": settled(45n, "Jc 8d", "9h 9s", Outcome.Push, 5n * FANG),
  "dealer-wins": settled(46n, "10s 7h", "6d 4c Qs", Outcome.Lose, 5n * FANG),
  "bust": settled(47n, "9d 6s Kc", "7h", Outcome.Bust, 5n * FANG, true),
  "timeout-dealer": settled(48n, "8h 4d", "Jd", Outcome.Forfeit, 5n * FANG, true),
  "timeout-deal": { ...settled(49n, "As", "9c", Outcome.ForfeitDeal, 5n * FANG), playerCards: [], dealerCards: [], holeHidden: false },
  "no-seed": { gameId: 50n, playerCards: [], dealerCards: [], holeHidden: false, stake: 5n * FANG, phase: Phase.Done, outcome: Outcome.NoSeed, payout: 0n, player: "mina.boo" },
  "long-hand": {
    gameId: 18446744073709551615n,
    playerCards: cards("As Ah Ad Ac As Ah Ad Ac 2s 2h 2d 2c As Ah Ad Ac As Ah Ad Ac"),
    dealerCards: cards("As Ah Ad Ac 2s 2h As Ad 2c As Ah 3d"),
    holeHidden: false,
    stake: 100n * FANG,
    phase: Phase.Dealer,
    outcome: null,
    payout: null,
    txId: TX,
    player: "a-very-long-name-that-goes-on-and-on.boo",
  },
};

function write(dir: string, name: string, p: Picture, withSvg: boolean): void {
  writeFileSync(join(dir, `${name}.png`), p.png);
  writeFileSync(join(dir, `${name}.alt.txt`), `${p.alt}\n`);
  if (withSvg) writeFileSync(join(dir, `${name}.svg`), p.svg);
}

function main(args: string[]): void {
  const posterOnly = args[0] === "--poster";
  const out = args[posterOnly ? 1 : 0];
  if (!out) {
    console.error("usage: tsx src/cli.ts [--poster] <outdir>");
    process.exit(2);
  }
  const dir = resolve(out);
  mkdirSync(dir, { recursive: true });
  write(dir, "poster", renderPoster(), !posterOnly);
  if (!posterOnly) for (const [name, s] of Object.entries(GALLERY)) write(dir, name, renderTable(s), true);
  console.log(`wrote ${posterOnly ? "the poster" : `${Object.keys(GALLERY).length + 1} pictures`} to ${dir}`);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) main(process.argv.slice(2));
