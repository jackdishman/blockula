// Every reply the bot can write: it fits a post on X, holds none of the banned words, carries the
// note where a hand starts or ends, and never needs the picture to be understood.
//
//   npx tsx test/copy.ts
import assert from "node:assert/strict";
import { ABORTS, Outcome } from "@blockula/blackjack";
import { BANNED, NOTE, X_LIMIT, balanceText, bannedIn, endedText, refusalText, settledText, stopText, turnText, waitingText, xLength, type Refusal } from "../src/index.js";

// X's count: code points, and 23 for a link whatever its length.
assert.equal(xLength("abc"), 3);
assert.equal(xLength("10♠ K♥"), 6);
assert.equal(xLength("\u{1f987}"), 1);
assert.equal(xLength("see https://a.b/" + "x".repeat(100)), 4 + 23);
assert.equal(xLength("https://a.b/c and http://d.e"), 23 + 5 + 23);
assert.equal(NOTE, "FANG has no value and can't be bought; no prizes.");

const U64 = (1n << 64n) - 1n;
const F = 1_000_000_000n;
// Amounts with every decimal, the largest a table holds.
const AMOUNTS = [F, 25n * F + 123_456_789n, 200n * F + 123_456_789n, 999_999_999n * F + 999_999_999n];
// Hands from the shortest to longer than any real one (cards 9, 22, 35, 48 are the four tens).
const tens = (n: number) => Array.from({ length: n }, (_, i) => [9, 22, 35, 48][i % 4]);
const HANDS = [[0, 12], [9, 22], tens(5), [0, 13, 26, 39, 1, 14, 27, 40, 2, 15, 28], tens(20)];
const DEALERS = [[], [9], [9, 22], tens(6), tens(12)];

const all: { what: string; text: string; note: boolean }[] = [];
for (const gameId of [0n, 7n, U64]) {
  for (const stake of AMOUNTS) {
    for (const playerCards of HANDS) {
      for (const dealerCards of DEALERS) {
        const h = { gameId, stake, playerCards, dealerCards };
        for (const opened of [true, false]) {
          all.push({ what: "turn", text: turnText({ ...h, dealerCards: dealerCards.slice(0, 1) }, opened), note: opened });
          all.push({ what: "waiting", text: waitingText(h, opened), note: opened });
          all.push({ what: "waiting, no cards", text: waitingText({ ...h, playerCards: [] }, opened), note: opened });
        }
        for (const outcome of Object.values(Outcome)) {
          for (const payout of [0n, ...AMOUNTS]) {
            for (const holeHidden of [true, false]) all.push({ what: "settled", text: settledText({ ...h, outcome, payout, balance: AMOUNTS[3], holeHidden }), note: true });
            all.push({ what: "settled, no cards", text: settledText({ ...h, playerCards: [], dealerCards: [], outcome, payout, balance: AMOUNTS[2], holeHidden: true }), note: true });
          }
        }
      }
    }
    all.push({ what: "ended", text: endedText(gameId, stake), note: true });
    all.push({ what: "stop", text: stopText(stake), note: true });
    for (const open of [true, false]) {
      all.push({ what: "balance", text: balanceText({ balance: stake, grant: null, open }), note: false });
      all.push({ what: "balance", text: balanceText({ balance: stake, grant: { leftToday: AMOUNTS[3], maxBet: AMOUNTS[1], blocksLeft: U64 }, open }), note: false });
    }
  }
}
all.push({ what: "stop", text: stopText(null), note: true });

const REFUSALS: Refusal[] = [
  { reason: "no_link" },
  { reason: "unverified" },
  { reason: "conflict" },
  { reason: "no_grant" },
  { reason: "expired" },
  { reason: "paused" },
  { reason: "hand_open", gameId: U64, here: true },
  { reason: "hand_open", gameId: U64, here: false },
  { reason: "limits", min: AMOUNTS[3], max: AMOUNTS[3] },
  { reason: "daily", left: AMOUNTS[3] },
  { reason: "daily", left: 0n },
  { reason: "balance", balance: AMOUNTS[3] },
  { reason: "no_hand" },
  { reason: "thread" },
  { reason: "waiting" },
  { reason: "finish_first" },
  { reason: "empty" },
  { reason: "stop_no_grant" },
  { reason: "not_sent" },
  { reason: "table", code: undefined },
  // Every abort code the program has, and numbers it doesn't.
  ...Array.from({ length: 120 }, (_, code) => ({ reason: "table" as const, code })),
];
for (const r of REFUSALS) all.push({ what: `refusal ${r.reason}`, text: refusalText(r), note: false });
for (const code of Object.keys(ABORTS)) assert.ok(refusalText({ reason: "table", code: Number(code) }).length > 10);

for (const { what, text, note } of all) {
  assert.ok(xLength(text) <= X_LIMIT, `${what} is ${xLength(text)} long:\n${text}`);
  assert.deepEqual(bannedIn(text), [], `${what} holds a banned word:\n${text}`);
  assert.equal(text.includes(NOTE), note, `${what}: the note${note ? " is missing" : " isn't needed"}:\n${text}`);
  assert.ok(text.trim() === text && text.length > 0 && !text.includes("undefined") && !text.includes("null") && !text.includes("NaN"), `${what}:\n${text}`);
  assert.ok(!/https?:|@/.test(text), `${what} holds a link or a mention:\n${text}`);
}
// A reply about cards names them or counts them; the picture adds nothing the text lacks.
assert.match(turnText({ gameId: 3n, stake: 5n * F, playerCards: [9, 5], dealerCards: [12] }, true), /^Hand 3\. Bet 5 FANG\.\nYou: 10♠ 6♠ \(16\)\.\nDealer: K♠ and one card face down\.\nReply hit or stand\.\nFANG has no value/);
assert.match(settledText({ gameId: 3n, stake: 5n * F, playerCards: [9, 5, 3], dealerCards: [12, 6], outcome: Outcome.Win, payout: 10n * F, balance: 105n * F, holeHidden: false }), /^Hand 3: You win\.\nYou: 10♠ 6♠ 4♠ \(20\)\.\nDealer: K♠ 7♠ \(17\)\.\nBet 5 FANG\. 10 FANG came back to your table balance, now 105 FANG\.\n/);
// The longest hands fall back to totals and counts, and still fit.
assert.match(settledText({ gameId: U64, stake: AMOUNTS[3], playerCards: tens(20), dealerCards: tens(12), outcome: Outcome.Forfeit, payout: AMOUNTS[3], balance: AMOUNTS[3], holeHidden: false }), /with 20 cards/);
assert.deepEqual(bannedIn("A smart contract at the Casino: win money, cash, a JACKPOT, on mainnet, for example in a demo."), [...BANNED]);

console.log(`✓ copy: ${all.length} replies, each at most ${X_LIMIT} as X counts, none with a banned word`);
