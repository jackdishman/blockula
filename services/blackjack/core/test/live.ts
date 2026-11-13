// The table on a real node, through the SDK: `blockula dev --programs` (or BLOCKULA_RPC on any
// local chain with programs on and a faucet). Creates a table, plays hands from an account and
// through a relayer, lets a dealer and a player run out of time, and checks after every step that
// the program's balance is bankroll + reserved + stakes + table and that every card recomputes.
//
//   BLOCKULA_HOME=$(mktemp -d) blockula dev --programs --port 38100 &
//   BLOCKULA_RPC=http://127.0.0.1:38100 npm run test:live -w @blockula/blackjack
import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { Client, ProgramFailedError, Wallet } from "@blockula/sdk";
import { DEFAULT_LIMITS, DROPS_PER_FANG, Outcome, Phase, Table, call, chainFromTop, createTable, decodeGame, decodeGameId, decodeHouse, decodeLimits, decodePlayer, decodeStep, eventNamed, eventsOf, input, owedLinks, payout, relaySeed, send, toHex, verifyHand, viewBytes, type Game, type TableCall } from "../src/index.js";

const RPC = process.env.BLOCKULA_RPC;
if (!RPC) {
  console.log("- BLOCKULA_RPC isn't set: the live table test is skipped (see the file's header)");
  process.exit(0);
}
if (!/^http:\/\/(127\.0\.0\.1|localhost):\d+$/.test(RPC)) throw new Error("the live test runs against a local chain only");
const client = new Client(RPC, { network: "local" });
await client.status();
const FANG = DROPS_PER_FANG;
const guest = (file: string) => new Uint8Array(readFileSync(new URL(`../../../../node/vm/tests/programs/${file}`, import.meta.url)));

const wallet = async (drips = 1) => {
  const w = Wallet.generate(client);
  for (let i = 0; i < drips; i++) await w.faucet();
  return w;
};
const [owner, dealer, alice, bob, bot] = await Promise.all([wallet(4), wallet(), wallet(), wallet(), wallet()]);
const program = await createTable(owner, guest("blackjack.wasm"), guest("blackjack.iface"), dealer.address);
const table = Table.at(RPC, program);
console.log("table:", program);

const go = (w: Wallet, c: TableCall) => send(w, program, c);
const refused = async (w: Wallet, c: TableCall, code: number, what: string) => {
  await assert.rejects(go(w, c), (e: unknown) => e instanceof ProgramFailedError && e.receipt.outcome.abort_code === code, `${what}: abort ${code}`);
};
const balanceOf = async (address: string) => BigInt((await client.account(address)).balance);
const height = async () => (await client.status()).height as number;
const waitPast = async (h: bigint) => {
  while (BigInt(await height()) <= h) await new Promise((r) => setTimeout(r, 300));
};
/** The invariant, from the chain: the program's balance is the four counters' sum. */
async function invariant(what: string) {
  const h = await table.house();
  assert.equal(await balanceOf(program), h.bankroll + h.reserved + h.stakes + h.table, `${what}: balance = bankroll + reserved + stakes + table`);
  return h;
}

// ---- the table as created: the owner, the dealer, the default limits, nothing held ----
let h = await invariant("created");
assert.deepEqual([h.owner, h.dealer, h.bankroll, h.open, h.paused], [owner.address, dealer.address, 0n, 0, false]);
assert.deepEqual(await table.limits(), DEFAULT_LIMITS);
assert.equal((await client.program(program)).accepts_transfers, false, "nothing reaches the table but its methods");
await assert.rejects(alice.send(program, "1"), "a plain transfer to the table is refused");

// The views say what the storage holds (a dev node serves views).
const view = async (method: string, inp = "") => {
  const v = await client.view(program, method, inp);
  assert.ok(v.ok, `${method} view`);
  return v.return_data as string;
};
assert.deepEqual(decodeLimits(viewBytes(await view("limits"))), DEFAULT_LIMITS);
const houseView = viewBytes(await view("house"));
assert.deepEqual({ ...decodeHouse(houseView.slice(0, 228), table.prefix), height: h.height }, h);

// ---- the owner: quick deadlines for this test, a bankroll, a relayer ----
await refused(alice, call.setLimits(DEFAULT_LIMITS), 42, "limits by a stranger");
const limits = { ...DEFAULT_LIMITS, revealBlocks: 6n, idleBlocks: 8n };
await go(owner, call.setLimits(limits));
assert.deepEqual(await table.limits(), limits);
await go(owner, call.fund(300n * FANG));
await go(owner, call.approveRelayer(bot.address, true));
assert.equal(await table.relayerApproved(bot.address), true);
h = await invariant("funded");
assert.equal(h.bankroll, 300n * FANG);

// ---- the dealer: chains committed ahead ----
const chains = new Map<bigint, string[]>();
async function commit(n: number) {
  const first = (await table.house()).nextCommit;
  const made = Array.from({ length: n }, () => chainFromTop(toHex(randomBytes(32))));
  made.forEach((c, i) => chains.set(first + BigInt(i), c));
  const r = await go(dealer, call.commitBatch(first, made.map((c) => c[0])));
  assert.deepEqual(eventNamed(eventsOf(r.receipt, program), "commit"), { name: "commit", first, commitments: made.map((c) => c[0]) });
}
await refused(alice, call.commitBatch(0n, ["00".repeat(32)]), 43, "a commitment by a stranger");
await commit(8);
await refused(dealer, call.commitBatch(0n, ["00".repeat(32)]), 61, "the first batch again");
assert.equal(await table.commitment(0n), chains.get(0n)![0]);
assert.equal(await view("commitment", input.commitment(0n)), chains.get(0n)![0]);

/** As the dealer: reveals everything hand `id` is owed now. */
async function deal(id: bigint) {
  const g = (await table.game(id)) as Game;
  const r = await go(dealer, call.reveal(id, owedLinks(g, chains.get(id)!)));
  return { r, ...decodeStep(r.receipt.claims.return_data[0].data), events: eventsOf(r.receipt, program) };
}

/** Plays hand `id` to its end: `move` signs the player's moves; hits below 17. Returns the settled event. */
async function playOut(id: bigint, move: (hit: boolean) => Promise<unknown>) {
  for (;;) {
    const step = await deal(id);
    await invariant(`hand ${id} dealt`);
    if (step.outcome !== null) return eventNamed(step.events, "settled")!;
    const g = (await table.game(id)) as Game;
    assert.equal(g.phase, Phase.Player);
    await move(g.playerTotal < 17);
  }
}

// ---- a hand from an account ----
await refused(alice, call.bet(FANG - 1n, "11".repeat(32)), 47, "a bet under the minimum");
await refused(alice, call.bet(16n * FANG, "11".repeat(32)), 48, "a bet over a twentieth of the bankroll");
const seed = toHex(randomBytes(32));
const before = await balanceOf(alice.address);
let r = await go(alice, call.bet(2n * FANG, seed));
const id = decodeGameId(r.receipt.claims.return_data[0].data);
const bet = eventNamed(eventsOf(r.receipt, program), "bet")!;
assert.deepEqual([bet.game, bet.player, bet.stake, bet.clientSeed, bet.commit, bet.table], [id, alice.address, 2n * FANG, seed, chains.get(id)![0], false]);
h = await invariant("bet");
assert.deepEqual([h.reserved, h.stakes, h.open], [3n * FANG, 2n * FANG, 1]);
assert.deepEqual(await table.openGames(), [id]);
await refused(alice, call.bet(FANG, seed), 52, "a second hand");
await refused(bob, call.hit(id), 55, "another account's move");
await refused(dealer, call.reveal(id, ["00".repeat(32)]), 58, "a link that isn't the chain's");
// The view of the hand is its stored record and its totals.
const settled = await (async () => {
  const first = await deal(id);
  if (first.outcome !== null) return eventNamed(first.events, "settled")!;
  const g = (await table.game(id)) as Game;
  const viewed = viewBytes(await view("game", input.game(id)));
  assert.deepEqual(decodeGame(id, viewed.slice(0, 314), table.prefix), g);
  assert.equal(parseInt(viewed.slice(314, 316), 16), g.playerTotal);
  await go(alice, g.playerTotal < 17 ? call.hit(id) : call.stand(id));
  return playOut(id, (hit) => go(alice, hit ? call.hit(id) : call.stand(id)));
})();
// Every card recomputes from the settled event alone, down to the bet's commitment.
const proof = verifyHand({ ...settled, commit: bet.commit });
assert.equal(proof.outcome ?? settled.outcome, settled.outcome);
assert.equal(settled.payout, payout(2n * FANG, settled.outcome));
assert.equal(settled.toTable, false);
// The player's record keeps the same hand; the view of it agrees.
const rec = await table.player(alice.address);
assert.deepEqual(rec.last, { game: id, outcome: settled.outcome, dealt: settled.dealt, nPlayer: settled.nPlayer, stake: settled.stake, payout: settled.payout, clientSeed: seed, link: settled.link });
assert.deepEqual(decodePlayer(viewBytes(await view("player", input.player(alice.address))).slice(0, 390), table.prefix), rec);
assert.equal(await table.game(id), null);
h = await invariant("settled");
assert.deepEqual([h.reserved, h.stakes, h.open, h.bankroll], [0n, 0n, 0, 302n * FANG - settled.payout]);
// The account got the payout; what it's out beyond the stake is fees (and its record's deposit).
const spent = before - 2n * FANG + settled.payout - (await balanceOf(alice.address));
assert.ok(spent > 0n && spent < FANG / 5n, `fees and the player record's deposit: ${spent} drops`);
console.log(`✓ a hand from an account: ${proof.player.length} cards against ${proof.dealer.length}, outcome ${settled.outcome}, verified from its settled event`);

// ---- a table balance, a grant, a relayed hand ----
await go(bob, call.deposit(20n * FANG));
await refused(bot, call.relayBet(bob.address, FANG, 1n), 64, "a relayed bet without a grant");
const now = BigInt(await height());
await refused(bob, call.setRelayer(alice.address, FANG, FANG, now + 100n), 64, "a grant to an account the owner didn't approve");
await go(bob, call.setRelayer(bot.address, 5n * FANG, 10n * FANG, now + 400n));
await refused(bot, call.relayBet(bob.address, 6n * FANG, 1n), 66, "a relayed bet over the grant");
const post = 1_859_000_000_000_000_123n;
const [bobBefore, botBefore] = [await balanceOf(bob.address), await balanceOf(bot.address)];
r = await go(bot, call.relayBet(bob.address, 2n * FANG, post));
const rid = decodeGameId(r.receipt.claims.return_data[0].data);
const events = eventsOf(r.receipt, program);
const rbet = eventNamed(events, "bet")!;
assert.deepEqual([rbet.clientSeed, rbet.table, rbet.postId], [relaySeed(bob.address, post), true, post]);
assert.deepEqual(eventNamed(events, "relayed"), { name: "relayed", player: bob.address, relayer: bot.address, postId: post, action: "bet", game: rid });
assert.equal((await table.player(bob.address)).balance, 18n * FANG);
await refused(alice, call.relayStand(rid, 2n), 64, "a relayed move by a stranger");
const rsettled = await playOut(rid, (hit) => go(bot, hit ? call.relayHit(rid, post + 1n) : call.relayStand(rid, post + 2n)));
verifyHand({ ...rsettled, commit: rbet.commit });
assert.equal(rsettled.toTable, rsettled.payout > 0n);
assert.equal((await table.player(bob.address)).balance, 18n * FANG + rsettled.payout);
assert.equal(await balanceOf(bob.address), bobBefore, "a relayed hand moves no account");
// The relayer sends the balance back to its depositor, and nowhere else.
r = await go(bot, call.relayWithdraw(bob.address, post + 3n));
assert.equal(await balanceOf(bob.address), bobBefore + 18n * FANG + rsettled.payout);
assert.ok((await balanceOf(bot.address)) < botBefore, "the relayer only paid fees");
await refused(bot, call.withdraw(FANG), 63, "the relayer has no balance of its own to take");
h = await invariant("relayed hand");
assert.equal(h.table, 0n);
console.log(`✓ a relayed hand: seed from post ${post}, outcome ${rsettled.outcome}, paid to the table balance and withdrawn to its depositor`);

// ---- a dealer that never deals: the player wins ----
r = await go(alice, call.bet(2n * FANG, toHex(randomBytes(32))));
const stalled = decodeGameId(r.receipt.claims.return_data[0].data);
let g = (await table.game(stalled)) as Game;
await refused(bob, call.timeout(stalled), 60, "a timeout before the deadline");
await waitPast(g.deadline);
const aliceBefore = await balanceOf(alice.address);
r = await go(bob, call.timeout(stalled));
assert.deepEqual(decodeStep(r.receipt.claims.return_data[0].data), { phase: Phase.Done, outcome: Outcome.ForfeitDeal });
// 2.5 × the stake, and the hand's storage deposit back (the bettor bonded it).
const got = (await balanceOf(alice.address)) - aliceBefore;
assert.ok(got >= 5n * FANG && got < 5n * FANG + FANG / 10n, `2.5 × the stake and the hand's deposit: ${got} drops`);
const forfeit = eventNamed(eventsOf(r.receipt, program), "settled")!;
assert.equal(verifyHand(forfeit).forfeited, true);
await invariant("dealer timeout");
console.log("✓ a dealer that never dealt: the player was paid 2.5 × by timeout");

// ---- a player who never moves: they stand, and the dealer finishes ----
let idle = 0n;
for (;;) {
  r = await go(alice, call.bet(FANG, toHex(randomBytes(32))));
  idle = decodeGameId(r.receipt.claims.return_data[0].data);
  if ((await deal(idle)).outcome === null) break; // not a natural: it's the player's turn
}
g = (await table.game(idle)) as Game;
await waitPast(g.deadline);
r = await go(dealer, call.timeout(idle));
assert.deepEqual(decodeStep(r.receipt.claims.return_data[0].data), { phase: Phase.Dealer, outcome: null });
assert.equal(eventNamed(eventsOf(r.receipt, program), "stand")?.idle, true);
await refused(alice, call.hit(idle), 56, "a move after standing");
const finished = await deal(idle);
assert.ok(finished.outcome !== null);
verifyHand(eventNamed(finished.events, "settled")!);
await invariant("idle player");
console.log("✓ an idle player stood by timeout and the dealer finished the hand");

// ---- a pause stops new bets only ----
await go(bob, call.deposit(5n * FANG));
await go(owner, call.pause(true));
await refused(alice, call.bet(FANG, seed), 46, "a bet while paused");
await go(bob, call.withdraw(5n * FANG));
await go(owner, call.pause(false));

// ---- every drop accounted for: the owner takes the bankroll out and nothing is left ----
h = await invariant("the end");
assert.deepEqual([h.reserved, h.stakes, h.table, h.open], [0n, 0n, 0n, 0]);
await refused(owner, call.defund(h.bankroll + 1n), 63, "more than the bankroll");
await go(owner, call.defund(h.bankroll));
assert.equal(await balanceOf(program), 0n);
console.log("✅ the live table test passed: every hand verified, the invariant held at every step");
