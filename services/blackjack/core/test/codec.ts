// The program's bytes, the fairness check and the dealer's helper, offline: layouts against bytes
// written out by hand from the program's doc comment, hands against the Rust vectors.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { blake3 } from "@noble/hashes/blake3.js";
import { ABORTS, DEFAULT_LIMITS, EMPTY_PLAYER, Outcome, Phase, Table, abortWords, addressBytes, altText, bytesAddress, call, card, chainFromTop, decodeEvent, decodeGame, decodeGameId, decodeHouse, decodeLimits, decodeOpenList, decodePlayer, decodeStep, eventsOf, fang, handOf, input, owedLinks, prefixOf, relaySeed, seedHash, snapshotOfGame, snapshotOfSettled, storageKey, toHex, verifyHand, viewBytes, type Game } from "../src/index.js";

const le = (n: bigint | number, bytes = 8) => {
  let x = BigInt(n);
  let out = "";
  for (let i = 0; i < bytes; i++, x >>= 8n) out += (x & 0xffn).toString(16).padStart(2, "0");
  return out;
};
const A = (n: number) => new Uint8Array(32).fill(n);
const [alice, bot, house] = [bytesAddress(A(1), "boo"), bytesAddress(A(2), "boo"), bytesAddress(A(3), "boo")];
const [a, b, h] = [toHex(A(1)), toHex(A(2)), toHex(A(3))];
const s1 = "aa".repeat(32);
const s2 = "bb".repeat(32);

// ---- addresses ----
assert.deepEqual(addressBytes(alice), A(1));
assert.equal(prefixOf(alice), "boo");
assert.equal(prefixOf(bytesAddress(A(1), "tboo")), "tboo");
assert.throws(() => addressBytes("boo1qqqq"));
assert.throws(() => bytesAddress(new Uint8Array(31), "boo"));

// ---- inputs: Borsh for the interface's types ----
assert.equal(input.init(alice), a);
assert.equal(input.bet(s1), s1);
assert.equal(input.revealSeed(9n, s2), le(9) + s2);
assert.equal(input.defund(5n), le(5));
assert.equal(input.pause(true), "01");
assert.equal(input.approveRelayer(bot, false), b + "00");
assert.equal(input.commitBatch(7n, [s1, s2]), le(7) + le(2, 4) + s1 + s2);
assert.equal(input.reveal(9n, [s2]), le(9) + le(1, 4) + s2);
assert.equal(input.skipCommits(3), le(3, 4));
assert.equal(input.setRelayer(bot, 1n, 2n, 3n), b + le(1) + le(2) + le(3));
assert.equal(input.relayBet(alice, 4n, 5n), a + le(4) + le(5));
assert.equal(input.relayHit(6n, 7n), le(6) + le(7));
assert.equal(input.relayWithdraw(alice, 8n), a + le(8));
assert.equal(input.setLimits(DEFAULT_LIMITS), [1_000_000_000n, 50_000_000_000n, 500_000_000_000n, 43_200n, 10n, 20n, 750_000_000_000n, 150n, 1_800n, 200_000_000_000n, 25_000_000_000n, 43_200n, 300n].map((n) => le(n)).join(""));
assert.equal(input.setLimits(DEFAULT_LIMITS).length, 2 * 104);
assert.throws(() => input.reveal(1n, []), /1 to 12/);
assert.throws(() => input.reveal(1n, Array(13).fill(s1)), /1 to 12/);
assert.throws(() => input.commitBatch(0n, Array(65).fill(s1)), /1 to 64/);
assert.throws(() => input.bet("zz".repeat(32)));
assert.throws(() => input.defund(-1n));
assert.throws(() => input.defund(1n << 64n));
// A relayed bet's seed: the program's formula, with a stock BLAKE3.
const pre = new Uint8Array(64);
pre.set(new TextEncoder().encode("blockula/blackjack/relay"));
pre.set(A(1), 24);
pre.set(Uint8Array.from(Buffer.from(le(1_859_000_000_000_000_123n), "hex")), 56);
assert.equal(relaySeed(alice, 1_859_000_000_000_000_123n), toHex(blake3(pre)));
// What a bet commits to: the program's formula, with a stock BLAKE3, and the program's own vectors.
const seedPre = new Uint8Array(87);
seedPre.set(new TextEncoder().encode("blockula/blackjack/seed"));
seedPre.set(A(1), 23);
seedPre.set(Uint8Array.from(Buffer.from(s1, "hex")), 55);
assert.equal(seedHash(alice, s1), toHex(blake3(seedPre)));
assert.notEqual(seedHash(alice, s1), seedHash(bot, s1), "the account is part of the hash");
assert.throws(() => seedHash(alice, s1.slice(2)));
const seedVectors = (JSON.parse(readFileSync(new URL("./vectors.json", import.meta.url), "utf8")) as { seed_hashes: { player: string; client_seed: string; seed_hash: string }[] }).seed_hashes;
assert.ok(seedVectors.length >= 8);
for (const x of seedVectors) assert.equal(seedHash(bytesAddress(Uint8Array.from(Buffer.from(x.player, "hex")), "boo"), x.client_seed), x.seed_hash);

// ---- storage keys ----
assert.equal(storageKey.house, "48");
assert.equal(storageKey.game(258n), "67" + le(258));
assert.equal(storageKey.commitment(1n), "63" + le(1));
assert.equal(storageKey.player(alice), "70" + a);
assert.equal(storageKey.relayer(bot), "72" + b);

// ---- records ----
const houseHex = h + b + le(100) + le(15) + le(10) + le(7) + le(40) + le(33) + "01" + "02";
assert.deepEqual(decodeHouse(houseHex, "boo"), { owner: house, dealer: bot, bankroll: 100n, reserved: 15n, stakes: 10n, table: 7n, nextCommit: 40n, nextGame: 33n, paused: true, open: 2 });
assert.throws(() => decodeHouse(houseHex + "00", "boo"));
assert.deepEqual(decodeLimits(input.setLimits(DEFAULT_LIMITS)), DEFAULT_LIMITS);
const cards20 = "0c1a" + "00".repeat(18);
const cards12 = "05" + "00".repeat(11);
const gameHex = a + le(2_000_000_000) + s1 + s2 + le(900) + le(77) + "02" + "01" + "03" + "02" + "01" + cards20 + cards12;
const game = decodeGame(33n, gameHex, "boo");
assert.deepEqual(game, { id: 33n, player: alice, stake: 2_000_000_000n, clientSeed: s1, seedHash: null, link: s2, deadline: 900n, postId: 77n, phase: Phase.Player, table: true, dealt: 3, playerCards: [12, 26], dealerCards: [5], playerTotal: 21, dealerTotal: 6 });
assert.throws(() => decodeGame(33n, gameHex.slice(2), "boo"));
// A hand waiting for its seed: the same bytes are the seed's hash, and no seed is known.
const waitingHex = a + le(2_000_000_000) + s1 + s2 + le(900) + le(0) + "00" + "00" + "00" + "00" + "00" + "00".repeat(32);
const waiting = decodeGame(34n, waitingHex, "boo");
assert.deepEqual([waiting.phase, waiting.clientSeed, waiting.seedHash, waiting.dealt], [Phase.Seed, null, s1, 0]);
assert.throws(() => decodeGame(34n, waitingHex.slice(0, 240) + "06" + waitingHex.slice(242), "boo"), /phase/);
const lastHex = le(32) + "02" + "05" + "03" + le(2_000_000_000) + le(4_000_000_000) + s1 + s2;
const playerHex = le(5) + le(9) + le(3_000_000_000) + le(34) + b + le(11) + le(12) + le(13) + le(9) + le(14) + lastHex;
assert.deepEqual(decodePlayer(playerHex, "boo"), {
  balance: 5n,
  day: 9n,
  wagered: 3_000_000_000n,
  openGame: 33n,
  grant: { relayer: bot, maxBet: 11n, dailyCap: 12n, expiresAt: 13n, day: 9n, wagered: 14n },
  last: { game: 32n, outcome: Outcome.Win, dealt: 5, nPlayer: 3, stake: 2_000_000_000n, payout: 4_000_000_000n, clientSeed: s1, seedHash: null, link: s2 },
});
// A last hand whose seed never came: outcome 8, the hash where the seed would be.
const noSeedHex = playerHex.slice(0, 2 * 104) + le(32) + "08" + "00" + "00" + le(2_000_000_000) + le(0) + s1 + s2;
assert.deepEqual(decodePlayer(noSeedHex, "boo").last, { game: 32n, outcome: Outcome.NoSeed, dealt: 0, nPlayer: 0, stake: 2_000_000_000n, payout: 0n, clientSeed: null, seedHash: s1, link: s2 });
assert.deepEqual(decodePlayer("00".repeat(195), "boo"), EMPTY_PLAYER);
assert.deepEqual(decodeOpenList(le(4) + le(9) + "00".repeat(240), 2), [4n, 9n]);
assert.equal(viewBytes(le(3, 4) + "010203"), "010203");
assert.throws(() => viewBytes(le(4, 4) + "010203"));
assert.deepEqual(decodeStep("0502"), { phase: Phase.Done, outcome: Outcome.Win });
assert.deepEqual(decodeStep("0200"), { phase: Phase.Player, outcome: null });
assert.deepEqual(decodeStep("0508"), { phase: Phase.Done, outcome: Outcome.NoSeed });
assert.throws(() => decodeStep("0509"));
assert.throws(() => decodeStep("0600"));
assert.equal(decodeGameId(le(1n << 60n)), 1n << 60n);

// ---- events ----
assert.deepEqual(decodeEvent("bet", le(33) + a + le(2) + s1 + s2 + le(250) + "01" + le(77), "boo"), { name: "bet", game: 33n, player: alice, stake: 2n, seedHash: null, clientSeed: s1, commit: s2, deadline: 250n, table: true, postId: 77n });
// A bet from an account shows the seed's hash; the seed comes in its own event.
assert.deepEqual(decodeEvent("bet", le(33) + a + le(2) + s1 + s2 + le(400) + "00" + le(0), "boo"), { name: "bet", game: 33n, player: alice, stake: 2n, seedHash: s1, clientSeed: null, commit: s2, deadline: 400n, table: false, postId: 0n });
assert.deepEqual(decodeEvent("seed", le(33) + a + s1 + le(250), "boo"), { name: "seed", game: 33n, player: alice, clientSeed: s1, deadline: 250n });
assert.throws(() => decodeEvent("seed", le(33) + a + s1, "boo"), /too short/);
assert.deepEqual(decodeEvent("cards", le(33) + a + "01" + "02" + s1 + "0c" + s2 + "05" + "01" + "0a" + "06" + le(250), "boo"), {
  name: "cards",
  game: 33n,
  player: alice,
  first: 1,
  cards: [
    { link: s1, card: 12 },
    { link: s2, card: 5 },
  ],
  phase: Phase.Deal,
  playerTotal: 10,
  dealerTotal: 6,
  deadline: 250n,
});
assert.deepEqual(decodeEvent("hit", le(33) + a + le(260), "boo"), { name: "hit", game: 33n, player: alice, deadline: 260n });
assert.deepEqual(decodeEvent("stand", le(33) + a + le(260) + "01", "boo"), { name: "stand", game: 33n, player: alice, deadline: 260n, idle: true });
assert.deepEqual(decodeEvent("settled", le(33) + a + "01" + le(2) + le(5) + "01" + "03" + "02" + s1 + s2 + "15" + "0a", "boo"), { name: "settled", game: 33n, player: alice, outcome: Outcome.Blackjack, stake: 2n, payout: 5n, toTable: true, dealt: 3, nPlayer: 2, clientSeed: s1, seedHash: null, link: s2, playerTotal: 21, dealerTotal: 10 });
assert.deepEqual(decodeEvent("settled", le(33) + a + "08" + le(2) + le(0) + "00" + "00" + "00" + s1 + s2 + "00" + "00", "boo"), { name: "settled", game: 33n, player: alice, outcome: Outcome.NoSeed, stake: 2n, payout: 0n, toTable: false, dealt: 0, nPlayer: 0, clientSeed: null, seedHash: s1, link: s2, playerTotal: 0, dealerTotal: 0 });
assert.deepEqual(decodeEvent("relayed", a + b + le(77) + "04" + le(0), "boo"), { name: "relayed", player: alice, relayer: bot, postId: 77n, action: "withdraw", game: 0n });
assert.deepEqual(decodeEvent("commit", le(7) + s1 + s2, "boo"), { name: "commit", first: 7n, commitments: [s1, s2] });
assert.deepEqual(decodeEvent("deposit", a + le(3) + le(8), "boo"), { name: "deposit", player: alice, amount: 3n, balance: 8n });
assert.deepEqual(decodeEvent("revoke", a + "00".repeat(32), "boo"), { name: "revoke", player: alice, relayer: null });
assert.deepEqual(decodeEvent("paused", "01", "boo"), { name: "paused", on: true });
assert.equal(decodeEvent("something_else", "00", "boo"), null);
assert.throws(() => decodeEvent("hit", le(33) + a, "boo"), /too short/);
assert.throws(() => decodeEvent("hit", le(33) + a + le(260) + "00", "boo"), /left over/);
assert.throws(() => decodeEvent("settled", le(33) + a + "09" + le(2) + le(5) + "01" + "03" + "02" + s1 + s2 + "15" + "0a", "boo"), /outcome/);
// Only the table's own events are read from a receipt.
const receipt = { claims: { return_data: [], omitted: null, events: [{ program: house, untrusted_name: "paused", data: "01" }, { program: alice, untrusted_name: "paused", data: "00" }, { program: house, untrusted_name: "nonsense", data: "ff" }] } };
assert.deepEqual(eventsOf(receipt, house), [{ name: "paused", on: true }]);

// ---- calls ----
assert.deepEqual(call.bet(2n, s1), { method: "bet", input: s1, value: 2n, storageLimit: 250_000_000n });
assert.deepEqual(call.revealSeed(3n, s1), { method: "reveal_seed", input: le(3) + s1 });
assert.equal(call.revealSeed(3n, s1).storageLimit, undefined, "a seed's reveal bonds nothing and sends no FANG");
assert.deepEqual(call.hit(3n), { method: "hit", input: le(3) });
assert.equal(call.reveal(3n, [s1]).storageLimit, undefined, "a reveal bonds nothing");
assert.equal(call.timeout(3n).storageLimit, undefined);

// ---- reads: one storage entry by key, through the node's paged route ----
const asked: string[] = [];
const node = (entries: Record<string, string>) => async (path: string) => {
  asked.push(path);
  const after = new URL("http://x" + path).searchParams.get("after") ?? "";
  const keys = Object.keys(entries).sort();
  const next = keys.find((k) => Buffer.compare(Buffer.from(k, "hex"), Buffer.from(after, "hex")) > 0);
  return { entries: next ? [{ key: next, value: entries[next] }] : [], height: 12 };
};
const table = new Table(house, node({ [storageKey.house]: houseHex, [storageKey.limits]: input.setLimits(DEFAULT_LIMITS), [storageKey.game(33n)]: gameHex, [storageKey.player(alice)]: playerHex, [storageKey.commitment(40n)]: s1, [storageKey.open]: le(33) + le(7) + "00".repeat(240), [storageKey.relayer(bot)]: "01" }));
assert.equal(table.prefix, "boo");
assert.equal((await table.house()).bankroll, 100n);
assert.equal((await table.house()).height, 12);
assert.deepEqual(await table.limits(), DEFAULT_LIMITS);
assert.deepEqual(await table.game(33n), game);
assert.equal(await table.game(34n), null, "the next key isn't this one");
assert.equal(await table.game(0n), null);
assert.equal((await table.player(alice)).openGame, 33n);
assert.deepEqual(await table.player(bot), EMPTY_PLAYER);
assert.equal(await table.commitment(40n), s1);
assert.equal(await table.commitment(41n), null);
assert.deepEqual(await table.openGames(), [33n, 7n]);
assert.equal(await table.relayerApproved(bot), true);
assert.equal(await table.relayerApproved(alice), false);
assert.ok(asked.every((p) => p.startsWith(`/program/${house}/storage?limit=1&after=`)));
await assert.rejects(new Table(house, node({})).house(), /isn't a blackjack table/);

// ---- the fairness check and the dealer's helper, against the program's vectors ----
const v = JSON.parse(readFileSync(new URL("./vectors.json", import.meta.url), "utf8")) as { hands: { top: string; client_seed: string; game_id: string; moves: string; commit: string; links: string[]; cards: number[]; player: number[]; dealer: number[]; outcome: number; stake: string; payout: string }[] };
for (const [i, hand] of v.hands.entries()) {
  const id = BigInt(hand.game_id);
  const proof = { game: id, clientSeed: hand.client_seed, link: hand.links[hand.links.length - 1], dealt: hand.links.length, nPlayer: hand.player.length, commit: hand.commit, outcome: hand.outcome as Outcome, stake: BigInt(hand.stake), payout: BigInt(hand.payout) };
  const ok = verifyHand(proof);
  assert.deepEqual([ok.cards, ok.player, ok.dealer, ok.outcome, ok.commit, ok.links, ok.forfeited, ok.moves], [hand.cards, hand.player, hand.dealer, hand.outcome, hand.commit, hand.links, false, hand.moves], `hand ${i}`);
  // Anything changed is caught.
  assert.throws(() => verifyHand({ ...proof, commit: "00".repeat(32) }), /commitment/);
  assert.throws(() => verifyHand({ ...proof, outcome: ((hand.outcome % 5) + 1) as Outcome }), /outcome|payout/);
  assert.throws(() => verifyHand({ ...proof, payout: proof.payout + 1n }), /payout/);
  assert.throws(() => verifyHand({ ...proof, link: "11".repeat(32) }), /commitment/);
  if (hand.links.length > 3) assert.throws(() => verifyHand({ ...proof, dealt: proof.dealt - 1 }), /commitment/);

  // The dealer's helper reveals exactly the vector's links, turn by turn.
  const chain = chainFromTop(hand.top);
  // Before the seed the dealer owes nothing.
  const committed: Game = { id, player: alice, stake: 1n, clientSeed: null, seedHash: seedHash(alice, hand.client_seed), link: hand.commit, deadline: 0n, postId: 0n, phase: Phase.Seed, table: false, dealt: 0, playerCards: [], dealerCards: [], playerTotal: 0, dealerTotal: 0 };
  assert.deepEqual(owedLinks(committed, chain), []);
  assert.throws(() => owedLinks({ ...committed, phase: Phase.Deal }, chain), /seed/);
  const state: Game = { id, player: alice, stake: 1n, clientSeed: hand.client_seed, seedHash: null, link: hand.commit, deadline: 0n, postId: 0n, phase: Phase.Deal, table: false, dealt: 0, playerCards: [], dealerCards: [], playerTotal: 0, dealerTotal: 0 };
  const revealed: string[] = [];
  let moves = hand.moves;
  for (;;) {
    const owed = owedLinks(state, chain);
    assert.ok(owed.length >= 1 && owed.length <= 12, `hand ${i}: ${owed.length} links owed`);
    const table = handOf(state);
    for (const link of owed) {
      revealed.push(link);
      table.deal(card(link, hand.client_seed, id, revealed.length));
    }
    Object.assign(state, { phase: table.phase, dealt: table.dealt, playerCards: table.player, dealerCards: table.dealer });
    if (table.phase === Phase.Done) break;
    assert.equal(table.phase, Phase.Player);
    assert.deepEqual(owedLinks(state, chain), [], "nothing owed on the player's turn");
    if (moves[0] === "H") table.hit();
    else table.stand();
    moves = moves.slice(1);
    state.phase = table.phase;
  }
  assert.deepEqual(revealed, hand.links, `hand ${i}: the dealer reveals the hand's links`);
}
// A forfeit: the cards don't finish the hand, the timeout does, and it pays the most the hand could have.
const open = v.hands.find((x) => x.moves.includes("H"))!;
const dealt3 = { game: BigInt(open.game_id), clientSeed: open.client_seed, link: open.links[2], dealt: 3, nPlayer: 2, commit: open.commit };
assert.equal(verifyHand({ ...dealt3, outcome: Outcome.Forfeit }).forfeited, true);
assert.throws(() => verifyHand({ ...dealt3, outcome: Outcome.ForfeitDeal }), /timeout/);
assert.throws(() => verifyHand({ ...dealt3, outcome: Outcome.Win }), /timeout/);
assert.equal(verifyHand({ ...dealt3, link: open.commit, dealt: 0, nPlayer: 0, outcome: Outcome.ForfeitDeal }).forfeited, true);
assert.equal(verifyHand(dealt3).outcome, null, "an open hand verifies as far as it's dealt");
// The seed against the hash the bet committed to, when the verifier has it.
assert.equal(verifyHand(dealt3).seedChecked, false);
assert.equal(verifyHand({ ...dealt3, seedHash: seedHash(alice, open.client_seed), player: alice }).seedChecked, true);
assert.throws(() => verifyHand({ ...dealt3, seedHash: seedHash(bot, open.client_seed), player: alice }), /committed/);
assert.throws(() => verifyHand({ ...dealt3, seedHash: "00".repeat(32), player: alice }), /committed/);
// A hand whose seed never came: no cards, outcome 8, nothing paid, and the commitment untouched.
const noSeed = { game: 5n, clientSeed: null, seedHash: s1, link: open.commit, dealt: 0, nPlayer: 0, commit: open.commit, outcome: Outcome.NoSeed, stake: 3n, payout: 0n };
const unrevealed = verifyHand(noSeed);
assert.deepEqual([unrevealed.unrevealed, unrevealed.forfeited, unrevealed.cards, unrevealed.outcome, unrevealed.commit], [true, false, [], Outcome.NoSeed, open.commit]);
assert.throws(() => verifyHand({ ...noSeed, payout: 1n }), /pays nothing/);
assert.throws(() => verifyHand({ ...noSeed, outcome: Outcome.ForfeitDeal }), /without its seed/);
assert.throws(() => verifyHand({ ...noSeed, dealt: 1, link: open.links[0] }), /no cards/);
assert.throws(() => verifyHand({ ...noSeed, clientSeed: open.client_seed }), /can't have one/);
assert.throws(() => verifyHand({ ...noSeed, commit: s2 }), /commitment/);
// A hand still waiting for its seed verifies as nothing dealt.
assert.deepEqual([verifyHand({ game: 5n, clientSeed: null, link: open.commit, dealt: 0, nPlayer: 0 }).phase, verifyHand({ game: 5n, clientSeed: null, link: open.commit, dealt: 0, nPlayer: 0 }).unrevealed], [Phase.Seed, false]);

// ---- words ----
const snap = snapshotOfGame(game, { player: "alice.boo" });
assert.deepEqual([snap.holeHidden, snap.outcome, snap.playerCards], [true, null, [12, 26]]);
assert.equal(altText(snap), "A blackjack table on Blockula, hand 33. alice.boo's cards: king of spades and ace of diamonds, blackjack. The dealer shows the six of spades and one card face down, its back a black bat with red wings. Bet: 2 FANG. Your move: hit or stand.");
const done = snapshotOfSettled({ game: 32n, outcome: Outcome.Win, stake: 2_000_000_000n, payout: 4_000_000_000n }, { player: [9, 8], dealer: [5, 22, 12] });
assert.equal(altText(done), "A blackjack table on Blockula, hand 32. The player's cards: ten of spades and nine of spades, 19. The dealer's cards: six of spades, ten of hearts and king of spades, 26. Bet: 2 FANG. You win. 4 FANG came back.");
assert.equal(fang(1_500_000_000n), "1.5");
assert.equal(fang(0n), "0");
assert.equal(abortWords(46), ABORTS[46]);
assert.equal(abortWords(999), "The table refused that.");
assert.equal(abortWords(undefined), "The table refused that.");
for (const text of Object.values(ABORTS)) assert.ok(!/contract|casino|jackpot|cash|demo|example|mainnet/i.test(text), text);

console.log(`✓ codec: inputs, records, events, storage reads, ${v.hands.length} hands verified and dealt, words`);
