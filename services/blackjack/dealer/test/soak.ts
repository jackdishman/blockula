// The blackjack soak: scripts/blackjack-e2e.sh starts a local chain of validators and runs this
// against it. It creates a table, runs the dealer daemon as a child process, plays HANDS hands
// from five accounts at once (three signing for themselves, each bet in two transactions: the
// seed's hash, then the seed; two through a relayer), and checks what the script's header lists:
// every hand verified, the money invariant at every block, the daemon killed and restarted, a
// dealer that stalls, bets whose seed never comes, the caps, a pause, and a clean end.
//
//   BLOCKULA_RPC   the first validator (127.0.0.1:38200–38399 only)
//   BJ_RPCS        every validator's URL, comma-separated (for the agreement check)
//   BJ_DIR         the run's temporary directory (the daemon's files go in $BJ_DIR/dealer)
//   HANDS          hands to play in the soak (default 1000)
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DEFAULT_LIMITS, DROPS_PER_FANG, Outcome, Phase, Table, call, createTable, decodeStep, eventNamed, exposure, fang, type Limits } from "@blockula/blackjack";
import { Client, Wallet } from "@blockula/sdk";
import { Daemon, Watcher, abortOf, cli, guest, localRpc, move, placeBet, playHand, playOut, sleep, sure, until, waitPast, type Play, type Seat } from "./harness.js";

const RPC = localRpc(process.env.BLOCKULA_RPC);
const RPCS = (process.env.BJ_RPCS ?? RPC).split(",").map((r) => localRpc(r));
const HANDS = Number(process.env.HANDS ?? 1000);
assert.ok(Number.isSafeInteger(HANDS) && HANDS >= 20, "HANDS is a whole number, at least 20");
const FANG = DROPS_PER_FANG;
const started = Date.now();
const client = new Client(RPC);
const dir = process.env.BJ_DIR ?? mkdtempSync(join(tmpdir(), "bj-soak."));
mkdirSync(dir, { recursive: true });

/**
 * The table's limits for the soak, and why:
 * - minBet 1 FANG, maxBet 5 FANG: the soak bets 1 to 3 FANG, odd numbers of drops included.
 * - dailyCap 1,000,000 FANG: out of the soak's way (step 7 lowers it to see it bite).
 * - dayBlocks 50: a "day" of 20 seconds, so step 7 sees the daily limit reset.
 * - maxOpenGames 8: the five seats and room for the tests' own hands.
 * - bankrollDivisor 20, maxExposure 100 FANG: the defaults' shape at this bankroll (400 FANG).
 * - revealBlocks 60: 24 seconds at 400 ms a block. A killed daemon is back in about 2 seconds
 *   (5 blocks) and its reveal lands 2 blocks later, so a restart fits ten times over; and the
 *   stall test waits only 24 seconds for the deadline.
 * - idleBlocks 40: 16 seconds to move. The seats move within a second or two, so only the hands
 *   left idle on purpose are stood by the daemon.
 * - depositCap 200 FANG, relayMaxBet 5 FANG, grantMaxBlocks 43,200: the relayed seats deposit
 *   100 FANG each and grant the relayer 5 FANG a bet for the whole run.
 * - seedBlocks 30: 12 seconds to reveal a bet's seed. The seats reveal in the next block or two,
 *   so only the bets whose seed is withheld on purpose are ended by the daemon.
 */
const SOAK_LIMITS: Limits = { ...DEFAULT_LIMITS, minBet: FANG, maxBet: 5n * FANG, dailyCap: 1_000_000n * FANG, dayBlocks: 50n, maxOpenGames: 8n, bankrollDivisor: 20n, maxExposure: 100n * FANG, revealBlocks: 60n, idleBlocks: 40n, depositCap: 200n * FANG, relayMaxBet: 5n * FANG, grantMaxBlocks: 43_200n, seedBlocks: 30n };
const BANKROLL = 400n * FANG;

let daemon: Daemon | undefined;
let watcher: Watcher | undefined;
const step = (text: string) => console.log(`[${Math.round((Date.now() - started) / 1000)} s] ${text}`);

try {
  const status = await client.status();
  const upgrade = (await (await fetch(`${RPC}/api/upgrade`)).json()) as { protocol_version: number };
  assert.equal(upgrade.protocol_version, 2, "blackjack runs on protocol v2");

  // ------------------------------------------------------------------ 1. the table
  const wallet = async (drips: number) => {
    const w = Wallet.generate(client);
    for (let i = 0; i < drips; i++) await w.faucet();
    return w;
  };
  const [owner, alice, bob, carol, dana, erin, relayer, stranger] = await Promise.all([wallet(6), wallet(2), wallet(2), wallet(2), wallet(2), wallet(2), wallet(1), wallet(1)]);
  const o = { dir: join(dir, "dealer"), rpc: RPC, table: "" };
  const dealerAddress = /^dealer address: (\S+)$/m.exec(cli(o, "init"))?.[1];
  assert.ok(dealerAddress, "init prints the dealer's address");
  await client.faucet(dealerAddress);
  const program = await createTable(owner, guest("blackjack.wasm"), guest("blackjack.iface"), dealerAddress);
  o.table = program;
  const table = Table.at(RPC, program);
  watcher = new Watcher(client, table, program);
  watcher.start(Number((await client.status()).height));
  const w = watcher;
  const play: Play = { client, table, program, watcher };
  const go = (who: Wallet, c: Parameters<typeof sure>[3]) => sure(client, program, who, c);
  const refused = async (who: Wallet, c: Parameters<typeof sure>[3], code: number, what: string) => {
    await assert.rejects(go(who, c), (e: unknown) => abortOf(e) === code, `${what}: abort ${code}`);
  };
  const balanceOf = async (address: string) => BigInt((await client.account(address)).balance);
  const height = async () => Number((await client.status()).height);

  await go(owner, call.setLimits(SOAK_LIMITS));
  await go(owner, call.fund(BANKROLL));
  await go(owner, call.approveRelayer(relayer.address, true));
  const seats: Seat[] = [
    { name: "alice", wallet: alice, hitBelow: 17 },
    { name: "bob", wallet: bob, hitBelow: 15 },
    { name: "carol", wallet: carol, hitBelow: 12 },
    { name: "dana (relayed)", wallet: dana, relayer, hitBelow: 17 },
    { name: "erin (relayed)", wallet: erin, relayer, hitBelow: 14 },
  ];
  const [aliceSeat, bobSeat, carolSeat, danaSeat] = seats;
  const grantUntil = BigInt(await height()) + 43_000n;
  for (const s of seats.filter((x) => x.relayer)) {
    await go(s.wallet, call.deposit(100n * FANG));
    await go(s.wallet, call.setRelayer(relayer.address, 5n * FANG, 1_000_000n * FANG, grantUntil));
  }
  step(`✓ 1. table ${program} on ${status.chain_id}: bankroll ${fang(BANKROLL)} FANG, the soak's limits, a relayer approved and granted by two seats`);

  // ------------------------------------------------------------------ 2. the daemon
  daemon = new Daemon({ ...o, env: { BJ_DEALER_POLL_MS: "150", BJ_DEALER_LOW_WATER: "32", BJ_DEALER_TARGET: "96", BJ_DEALER_KEEP_SETTLED: "16" } });
  const d = daemon;
  d.start();
  await until("the first commitments", async () => (await table.house()).nextCommit >= 96n, 120_000);
  step(`✓ 2. the dealer daemon (${dealerAddress}) is running as a child process and committed 96 chains ahead`);

  // ------------------------------------------------------------------ 3–6. the soak
  let done = 0;
  let noSeed = 0;
  let gate: Promise<void> | null = null;
  let busy = 0;
  let lastProgress = Date.now();
  const tableBalance = async (s: Seat) => (await table.player(s.wallet.address)).balance;
  const stakeNow = () => FANG + BigInt(Math.floor(Math.random() * 2_000_000_001));
  const seatLoop = async (seat: Seat) => {
    while (done < HANDS) {
      if (gate) await gate;
      if (done >= HANDS) break;
      busy++;
      try {
        const stake = stakeNow();
        // A relayed seat plays its table balance: topped up before it runs short.
        if (seat.relayer && (await tableBalance(seat)) < 10n * FANG) await go(seat.wallet, call.deposit(50n * FANG));
        const before = seat.relayer ? await tableBalance(seat) : 0n;
        const h = await playHand(play, seat, stake);
        assert.ok(h.settled.outcome <= Outcome.Bust, `game ${h.id} (${seat.name}) was forfeited (outcome ${h.settled.outcome}): the dealer missed a deadline`);
        assert.equal(h.settled.by, dealerAddress, `game ${h.id} was settled by the daemon's reveal`);
        // The player's record keeps the hand the event described.
        const rec = await table.player(seat.wallet.address);
        if (rec.last?.game === h.id) assert.deepEqual(rec.last, { game: h.id, outcome: h.settled.outcome, dealt: h.settled.dealt, nPlayer: h.settled.nPlayer, stake, payout: h.settled.payout, clientSeed: h.settled.clientSeed, seedHash: null, link: h.settled.link });
        // A relayed hand moves the table balance by exactly the payout less the stake.
        if (seat.relayer) assert.equal(rec.balance, before - stake + h.settled.payout, `${seat.name}'s table balance after game ${h.id}`);
        done++;
        lastProgress = Date.now();
        if (done % 50 === 0) step(`… ${done} hands (block ${h.settled.height}, invariant checked at ${w.heightsChecked} heights)`);
      } finally {
        busy--;
      }
    }
  };
  /** Holds the seats between hands; returns the function that lets them go on. */
  const hold = async (): Promise<() => void> => {
    let release!: () => void;
    gate = new Promise<void>((r) => (release = r));
    await until("the seats to finish their hands", async () => busy === 0 && (await table.openGames()).length === 0, 180_000);
    return () => {
      gate = null;
      release();
    };
  };
  const reached = (n: number) => until(`${n} hands`, () => (w.check(), done >= n), 3_600_000);
  let restarts = 0;

  const director = (async () => {
    // 5a. SIGKILL with hands mid-play, and straight back up.
    await reached(Math.floor(HANDS * 0.3));
    await until("a hand mid-play", async () => (await table.openGames()).length > 0, 60_000);
    const open1 = await table.openGames();
    await d.kill9();
    await sleep(500);
    d.start();
    restarts++;
    step(`✓ 5a. SIGKILL at ${done} hands with ${open1.length} hand(s) mid-play (games ${open1.join(", ")}); the daemon is back`);

    // 6. the dealer stalls; then an idle player.
    await reached(Math.floor(HANDS * 0.55));
    const resume = await hold();
    assert.equal(await d.stop(), 0, "SIGTERM stops the daemon cleanly");
    const h0 = await table.house();
    const stake = 2n * FANG;
    const stalled = await placeBet(play, aliceSeat, stake);
    const h1 = await table.house();
    assert.deepEqual([h1.bankroll, h1.reserved, h1.stakes], [h0.bankroll - exposure(stake), exposure(stake), stake], "the bet set its exposure aside");
    const g = await table.game(stalled.id);
    // Both of the bet's transactions landed: the seed is on the chain and the deal is owed.
    assert.ok(g && g.phase === Phase.Deal && g.clientSeed === stalled.seed);
    await refused(stranger, call.timeout(stalled.id), 60, "a timeout before the deadline");
    await waitPast(client, g.deadline);
    assert.equal((await table.game(stalled.id))?.phase, Phase.Deal, "nobody dealt it");
    const aliceBefore = await balanceOf(alice.address);
    const t = await go(stranger, call.timeout(stalled.id));
    assert.deepEqual(decodeStep(t.receipt.claims.return_data[0].data), { phase: Phase.Done, outcome: Outcome.ForfeitDeal });
    const forfeit = eventNamed(t.events, "settled")!;
    // 3:2 on top of the stake, the most the hand could still have paid, all of it from what the bet reserved.
    assert.deepEqual([forfeit.outcome, forfeit.payout, forfeit.toTable], [Outcome.ForfeitDeal, stake + exposure(stake), false]);
    const got = (await balanceOf(alice.address)) - aliceBefore;
    assert.ok(got >= 5n * FANG && got < 5n * FANG + FANG / 10n, `alice got ${fang(got)} FANG: 2.5 × the stake and the hand's storage deposit`);
    const h2 = await table.house();
    assert.deepEqual([h2.bankroll, h2.reserved, h2.stakes, h2.open], [h0.bankroll - exposure(stake), 0n, 0n, 0], "the reserve paid the player; the rest of the bankroll is untouched");
    d.start();
    restarts++;
    step(`✓ 6a. the dealer stalled on game ${stalled.id}: at block ${t.height} (deadline ${g.deadline}) a stranger's timeout paid the player ${fang(forfeit.payout)} FANG for a stake of ${fang(stake)}, from the hand's reserve`);
    for (let tries = 0; ; tries++) {
      assert.ok(tries < 20, "a hand that reaches the player's move");
      const idle = await placeBet(play, bobSeat, FANG);
      await until("the deal", async () => {
        const now = await table.game(idle.id);
        return now === null || now.phase === Phase.Player;
      });
      const now = await table.game(idle.id);
      if (now === null) continue; // a natural, settled at the deal
      await waitPast(client, now.deadline);
      const settled = await w.settledEvent(idle.id);
      assert.equal(w.idleStands.get(idle.id), dealerAddress, "the daemon's timeout stood the idle player");
      assert.ok(settled.outcome <= Outcome.Bust && settled.by === dealerAddress);
      step(`✓ 6b. an idle player on game ${idle.id} was stood by the daemon's timeout after block ${now.deadline}, and the hand was dealt to its end at block ${settled.height}`);
      break;
    }

    // 6c. bets whose seed never comes: the daemon deals nothing, and ends each at the seed's deadline.
    {
      const h3 = await table.house();
      const quiet = [
        { seat: aliceSeat, stake: FANG },
        { seat: bobSeat, stake: 2n * FANG + 1n },
        { seat: carolSeat, stake: 3n * FANG },
      ];
      const bets = [];
      for (const q of quiet) bets.push({ ...q, ...(await placeBet(play, q.seat, q.stake, { reveal: false })) });
      const total = quiet.reduce((n, q) => n + q.stake, 0n);
      const reserve = quiet.reduce((n, q) => n + exposure(q.stake), 0n);
      const h4 = await table.house();
      assert.deepEqual([h4.bankroll, h4.reserved, h4.stakes, h4.open], [h3.bankroll - reserve, reserve, total, 3], "each bet set its exposure aside and took a seat");
      for (const b of bets) {
        const g = await table.game(b.id);
        assert.ok(g && g.phase === Phase.Seed && g.clientSeed === null && g.dealt === 0, "the hand waits for its seed, with no card");
      }
      // (Only the newest: the first one's deadline is a few seconds off by now.)
      await refused(stranger, call.timeout(bets[2].id), 60, "a timeout before the seed's deadline");
      for (const b of bets) {
        const settled = await w.settledEvent(b.id);
        assert.deepEqual([settled.outcome, settled.payout, settled.dealt, settled.by], [Outcome.NoSeed, 0n, 0, dealerAddress], `game ${b.id} was ended by the daemon's own timeout`);
        assert.ok(settled.height > Number(b.bet.deadline), "past the seed's deadline, not before");
        await refused(b.seat.wallet, call.revealSeed(b.id, b.seed), 54, "a seed for a hand that's over");
        noSeed++;
      }
      await until("the three seats to be free", async () => (await table.house()).open === 0);
      const h5 = await table.house();
      assert.deepEqual([h5.bankroll, h5.reserved, h5.stakes], [h3.bankroll + total, 0n, 0n], "the stakes are the bankroll's and the reserves are free");
      step(`✓ 6c. three bets (games ${bets.map((b) => b.id).join(", ")}) never revealed their seed: the daemon dealt none of them and its timeouts ended each after its deadline; ${fang(total)} FANG of stakes went to the bankroll`);
    }
    resume();

    // 5b. SIGKILL again, this time down for three seconds while the seats keep betting and moving.
    await reached(Math.floor(HANDS * 0.75));
    await d.kill9();
    const downAt = await height();
    await sleep(3000);
    const open2 = await table.openGames();
    d.start();
    restarts++;
    step(`✓ 5b. SIGKILL at ${done} hands (block ${downAt}), down for 3 seconds with ${open2.length} hand(s) waiting; the daemon is back`);
  })();

  const watchdog = (async () => {
    while (done < HANDS) {
      w.check();
      if (!gate && Date.now() - lastProgress > 240_000) throw new Error(`no hand finished in 4 minutes (at ${done} hands)`);
      await sleep(500);
    }
  })();
  await Promise.all([...seats.map(seatLoop), director, watchdog]);
  await until("no open hands", async () => (await table.openGames()).length === 0);
  await w.caughtUp();
  w.check();
  const forfeits = [...w.settled.values()].filter((s) => s.outcome === Outcome.Forfeit || s.outcome === Outcome.ForfeitDeal);
  assert.equal(forfeits.length, 1, "the only forfeited hand is the one the stall test made");
  const unrevealed = [...w.settled.values()].filter((s) => s.outcome === Outcome.NoSeed);
  assert.equal(unrevealed.length, noSeed, "the only bets that ended without a seed are the ones that withheld it on purpose");
  assert.equal(noSeed, 3);
  step(`✓ 3–6. ${done} hands played by 5 seats (3 signing their own, 2 relayed), every one verified from its settled event to its bet's commitment; ${restarts} restarts, no hand forfeited by them`);

  // ------------------------------------------------------------------ 7. the caps
  {
    await refused(alice, call.bet(FANG - 1n, "11".repeat(32)), 47, "a bet under the minimum");
    await refused(alice, call.bet(5n * FANG + 1n, "11".repeat(32)), 48, "a bet over the maximum");
    // The bankroll's share: with the maximum out of the way, a bet over a twentieth of the free bankroll.
    await go(owner, call.setLimits({ ...SOAK_LIMITS, maxBet: 100n * FANG }));
    const free = (await table.house()).bankroll;
    await refused(alice, call.bet(free / 20n + 1n, "11".repeat(32)), 48, "a bet over a twentieth of the bankroll");
    // A day of 50 blocks, 6 FANG a day, two open hands at most.
    const capped: Limits = { ...SOAK_LIMITS, maxBet: 3n * FANG, dailyCap: 6n * FANG, maxOpenGames: 2n };
    await go(owner, call.setLimits(capped));
    const dayOf = (h: number) => Math.floor(h / 50);
    for (let tries = 0; ; tries++) {
      assert.ok(tries < 5, "two hands and a refusal inside one 50-block day");
      // From the start of a day: two hands of 3 FANG reach the limit, and a third bet is refused.
      await until("the start of a day", async () => (await height()) % 50 < 4, 60_000);
      const a = await playHand(play, aliceSeat, 3n * FANG);
      const b = await playHand(play, aliceSeat, 3n * FANG);
      let code: number | undefined;
      let third: Awaited<ReturnType<typeof placeBet>> | undefined;
      try {
        third = await placeBet(play, aliceSeat, FANG);
      } catch (e) {
        code = abortOf(e);
        if (code === undefined) throw e;
      }
      if (third) {
        // The day turned before the third bet: it was allowed, rightly. Finish it and try again.
        assert.ok(dayOf(third.height) > dayOf(a.betHeight), "a bet past the daily limit went through inside the day");
        await playOut(play, aliceSeat, third.id);
        continue;
      }
      assert.equal(code, 50, "a bet past the daily limit");
      const refusedAt = await height();
      if (dayOf(b.betHeight) !== dayOf(a.betHeight)) continue;
      // The next day the limit is whole again.
      await until("the next day", async () => dayOf(await height()) > dayOf(refusedAt), 60_000);
      const next = await playHand(play, aliceSeat, FANG);
      assert.ok(dayOf(next.betHeight) > dayOf(a.betHeight));
      step(`✓ 7a. min bet (47), max bet (48), the bankroll's twentieth (48); the daily limit: 6 FANG wagered in day ${dayOf(a.betHeight)}, a third bet refused (50), and allowed again in day ${dayOf(next.betHeight)}`);
      break;
    }
    // Two hands held open at the player's move fill the table.
    const openAt = async (seat: Seat) => {
      for (;;) {
        const bet = await placeBet(play, seat, FANG);
        await until("the deal", async () => {
          const now = await table.game(bet.id);
          return now === null || now.phase === Phase.Player;
        });
        if ((await table.game(bet.id)) !== null) return bet.id;
        await w.settledEvent(bet.id);
      }
    };
    const held1 = await openAt(bobSeat);
    const held2 = await openAt(carolSeat);
    assert.deepEqual((await table.openGames()).sort(), [held1, held2].sort());
    await refused(relayer, call.relayBet(dana.address, FANG, 77n), 51, "a third open hand");
    // Paused: no new bet, by an account or a relayer; everything else carries on.
    await go(owner, call.pause(true));
    assert.equal((await table.house()).paused, true);
    await move(play, bobSeat, held1, false);
    const s1 = await w.settledEvent(held1);
    assert.ok(s1.outcome <= Outcome.Bust && s1.by === dealerAddress, "a hand is dealt to its end while paused");
    await refused(alice, call.bet(FANG, "11".repeat(32)), 46, "a bet while paused");
    await refused(relayer, call.relayBet(dana.address, FANG, 78n), 46, "a relayed bet while paused");
    const danaTable = await tableBalance(danaSeat);
    const danaBefore = await balanceOf(dana.address);
    const wd = await go(dana, call.withdraw(FANG));
    assert.equal(await tableBalance(danaSeat), danaTable - FANG);
    assert.equal((await balanceOf(dana.address)) - danaBefore, FANG - wd.receipt.facts.fee.bytes - wd.receipt.facts.fee.fuel - wd.receipt.facts.fee.tip, "a withdrawal while paused pays the account");
    // The second hand is left alone: the daemon's timeout stands its player and deals it, paused or not.
    const idle = await table.game(held2);
    assert.ok(idle && idle.phase === Phase.Player);
    const s2 = await w.settledEvent(held2, 120_000);
    assert.equal(w.idleStands.get(held2), dealerAddress);
    assert.ok(s2.outcome <= Outcome.Bust && s2.height > Number(idle.deadline));
    await go(owner, call.pause(false));
    await go(owner, call.setLimits(SOAK_LIMITS));
    const after = await playHand(play, carolSeat, FANG);
    assert.ok(after.settled.outcome <= Outcome.Bust);
    step("✓ 7b. two open hands fill the table (51); paused: bets and relayed bets refused (46) while a hand was dealt to its end, an idle one was timed out and dealt, and a withdrawal was paid");
  }

  // ------------------------------------------------------------------ 8. the end
  await until("no open hands", async () => (await table.openGames()).length === 0);
  assert.equal(await d.stop(), 0, "the daemon stops cleanly");
  // The relayer sends each relayed seat's table balance back to its depositor.
  for (const s of seats.filter((x) => x.relayer)) {
    const owed = await tableBalance(s);
    const before = await balanceOf(s.wallet.address);
    if (owed > 0n) await go(relayer, call.relayWithdraw(s.wallet.address, 1n));
    assert.equal((await balanceOf(s.wallet.address)) - before, owed, `${s.name}'s table balance went back to its depositor`);
  }
  await w.caughtUp();
  const house = await table.house();
  assert.deepEqual([house.reserved, house.stakes, house.table, house.open], [0n, 0n, 0n, 0], "nothing is left on the table");
  assert.equal(house.bankroll, w.expectedBankroll, "the bankroll is what was funded and what every settled hand left it");
  const ownerBefore = await balanceOf(owner.address);
  const out = await go(owner, call.defund(house.bankroll));
  assert.equal((await balanceOf(owner.address)) - ownerBefore, house.bankroll - out.receipt.facts.fee.bytes - out.receipt.facts.fee.fuel - out.receipt.facts.fee.tip, "the owner has the whole bankroll back");
  assert.equal(await balanceOf(program), 0n, "the program holds nothing");
  await w.caughtUp();
  await w.invariant();
  await w.stop();
  w.check();

  // The daemon's log: no error, and nothing the chain didn't already know.
  const lines = d.log();
  const errors = lines.filter((l) => l.level === "error");
  assert.deepEqual(errors, [], "the daemon logged no error");
  assert.ok(lines.some((l) => l.action === "pruned") || w.settled.size < 80, "settled secrets were pruned");
  const raw = d.raw();
  for (const link of w.links) assert.ok(!raw.includes(link), "a revealed link is in the daemon's log");
  const txs = new Set(lines.map((l) => l.tx).filter((t): t is string => typeof t === "string"));
  for (const run of raw.match(/[0-9a-f]{32,}/g) ?? []) assert.ok(txs.has(run), "long hex in the daemon's log that isn't a transaction's id");
  const final = JSON.parse(cli(o, "status", ["--json"])) as { stored: number; balance: string };

  // Every validator agrees on a recent block.
  const at = (await height()) - 2;
  const hashes = await Promise.all(RPCS.map(async (rpc) => ((await (await fetch(`${rpc}/api/block/${at}`)).json()) as { hash: string }).hash));
  assert.equal(new Set(hashes).size, 1, `the validators disagree about block ${at}`);

  const names: Record<number, string> = { 1: "blackjack", 2: "win", 3: "push", 4: "lose", 5: "bust", 6: "forfeit", 7: "forfeit at the deal", 8: "seed never revealed" };
  const histogram = [...w.outcomes].sort((a, b) => a[0] - b[0]).map(([k, n]) => `${names[k]} ${n}`).join(", ");
  const fuel = [...w.fuel].sort((a, b) => a[0].localeCompare(b[0])).map(([m, f]) => `  ${m.padEnd(16)} max ${f.max.toString().padStart(7)}   mean ${(f.total / BigInt(f.calls)).toString().padStart(7)}   (${f.calls} calls)`).join("\n");
  const count = (action: string) => lines.filter((l) => l.action === action).length;
  console.log(
    [
      "",
      "── blackjack soak: summary ──",
      `hands settled      ${w.settled.size} (${done} in the soak, the rest in the tests around it), every one verified`,
      `outcomes           ${histogram}`,
      `forfeited          ${forfeits.length} (the stall test's game ${forfeits[0].game}); none from a restart`,
      `seed never came    ${unrevealed.length} (games ${unrevealed.map((s) => s.game).join(", ")}), each ended by the daemon's timeout, the stake to the bankroll`,
      `daemon             ${d.starts} starts: ${restarts - 1} after SIGKILL, 1 after the stall; ${count("reveal")} reveals, ${count("commit")} commits, ${count("timeout")} timeouts, ${count("pruned")} prunes, ${lines.filter((l) => l.level === "warn").length} warnings, 0 errors`,
      `daemon's account   ${fang(100n * FANG - BigInt(final.balance))} FANG spent on fees and deposits; ${final.stored} secrets left in its store`,
      `heights            blocks ${w.firstHeight}–${w.lastHeight}: the invariant held at ${w.heightsChecked} of them (${w.heightsSkipped} passed between two reads, ${w.skewRetries} reads retried for straddling a block)`,
      `bankroll           ${fang(BANKROLL)} FANG funded, ${fang(house.bankroll)} FANG defunded to the owner; the program holds 0`,
      `validators         ${RPCS.length} agree on block ${at} (${hashes[0].slice(0, 16)}…)`,
      "fuel used a call, from the receipts:",
      fuel,
      `wall time          ${Math.round((Date.now() - started) / 1000)} s`,
      "",
    ].join("\n"),
  );
  console.log(`✅ the blackjack soak passed: ${done} hands`);
} catch (e) {
  console.error(`❌ the soak failed after ${Math.round((Date.now() - started) / 1000)} s`);
  throw e;
} finally {
  await daemon?.kill9();
  await watcher?.stop().catch(() => undefined);
}
