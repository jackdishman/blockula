// The dealer daemon on a real local chain: a table is created, the daemon runs as a child
// process (the command line, as an operator runs it), three accounts play 30 hands, and the
// daemon is killed with SIGKILL in the middle of a hand and started again. No hand may be
// forfeited, and every settled hand must verify down to the commitment its bet took.
//
// Needs a local chain with programs on and a faucet, on 127.0.0.1:38200–38399:
//
//   TEST=live scripts/blackjack-e2e.sh        # starts one (4 validators) and runs this
//   HOLD=1 VALIDATORS=1 BASE_PORT=38200 scripts/blackjack-e2e.sh &
//   BLOCKULA_RPC=http://127.0.0.1:38200 npm run test:live -w @blockula/blackjack-dealer
import assert from "node:assert/strict";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DEFAULT_LIMITS, DROPS_PER_FANG, Phase, Table, call, createTable, dealers } from "@blockula/blackjack";
import { Client, Wallet } from "@blockula/sdk";
import { Daemon, Watcher, cli, guest, localRpc, placeBet, playHand, playOut, sleep, sure, until, waitPast, type Play, type Played, type Seat } from "./harness.js";

if (!process.env.BLOCKULA_RPC) {
  console.log("- BLOCKULA_RPC isn't set: the dealer's live test is skipped (see the file's header)");
  process.exit(0);
}
const RPC = localRpc(process.env.BLOCKULA_RPC);
const HANDS = Number(process.env.HANDS ?? 30);
const FANG = DROPS_PER_FANG;
const client = new Client(RPC);
await client.status();
const dir = mkdtempSync(join(process.env.BJ_DIR ?? tmpdir(), "bj-dealer-live."));
let daemon: Daemon | undefined;
let watcher: Watcher | undefined;

try {
  const wallet = async (drips: number) => {
    const w = Wallet.generate(client);
    for (let i = 0; i < drips; i++) await w.faucet();
    return w;
  };
  const [owner, alice, bob, carol, stranger] = await Promise.all([wallet(4), wallet(1), wallet(1), wallet(1), wallet(1)]);

  // ---- `init` makes the key and the secret and prints the address; the owner creates the table for it
  const o = { dir: join(dir, "dealer"), rpc: RPC, table: "" };
  const printed = cli(o, "init");
  const dealerAddress = /^dealer address: (\S+)$/m.exec(printed)?.[1];
  assert.ok(dealerAddress, "init prints the dealer's address");
  await client.faucet(dealerAddress);
  await client.faucet(dealerAddress);
  const program = await createTable(owner, guest("blackjack.wasm"), guest("blackjack.iface"), dealerAddress);
  o.table = program;
  const table = Table.at(RPC, program);
  console.log(`table ${program}, dealer ${dealerAddress}`);
  // The watcher reads every block from here on: the funding, every bet, every settlement.
  watcher = new Watcher(client, table, program);
  watcher.start(Number((await client.status()).height));
  // 40 blocks to reveal: a killed daemon is back within a few seconds, far inside it. 25 to move.
  const limits = { ...DEFAULT_LIMITS, revealBlocks: 40n, idleBlocks: 25n };
  await sure(client, program, owner, call.setLimits(limits));
  await sure(client, program, owner, call.fund(300n * FANG));

  const play: Play = { client, table, program, watcher };
  const env = { BJ_DEALER_POLL_MS: "150", BJ_DEALER_LOW_WATER: "8", BJ_DEALER_TARGET: "24", BJ_DEALER_KEEP_SETTLED: "4" };
  daemon = new Daemon({ ...o, env });

  // ---- a dry run reads the table and sends nothing
  daemon.start(["--dry-run"]);
  await until("the dry run to say what it would commit", () => daemon!.log().some((l) => l.action === "would_commit"));
  await sleep(1500);
  assert.equal(await daemon.stop(), 0, "SIGTERM ends the daemon cleanly");
  assert.equal((await table.house()).nextCommit, 0n, "a dry run commits nothing");
  assert.equal(existsSync(join(o.dir, "seeds.enc.json")), false, "and stores no secret");
  console.log("✓ a dry run logged what it would commit and sent nothing");

  // ---- the daemon commits ahead; `status` says so
  daemon.start();
  await until("the first commitments", async () => (await table.house()).nextCommit >= 24n);
  const status = JSON.parse(cli(o, "status", ["--json"])) as { address: string; unused: number; servable: number; missing: unknown[]; backup_current: boolean; isDealer: boolean; low_balance: boolean };
  assert.deepEqual([status.address, status.unused, status.servable, status.missing, status.backup_current, status.isDealer, status.low_balance], [dealerAddress, 24, 24, [], true, true, false]);
  assert.match(cli(o, "status"), /commitments {6}24 unused/);
  console.log("✓ the daemon committed 24 chains ahead; status reads the same from the chain and the store");

  // ---- hands from three accounts at once, with a SIGKILL in the middle of one
  const seats: Seat[] = [
    { name: "alice", wallet: alice, hitBelow: 17 },
    { name: "bob", wallet: bob, hitBelow: 15 },
    { name: "carol", wallet: carol, hitBelow: 12 },
  ];
  const played: Played[] = [];
  let next = 0;
  const each = Math.ceil(HANDS / seats.length);
  const loops = seats.map(async (seat) => {
    for (let i = 0; i < each; i++) {
      played.push(await playHand(play, seat, BigInt(1 + ((next++ + i) % 3)) * FANG));
    }
  });
  // Once a few hands are done: kill it while a hand is open, mid-play.
  await until("some hands", () => played.length >= Math.min(6, HANDS / 3), 120_000);
  await until("a hand mid-play", async () => (await table.openGames()).length > 0, 60_000);
  const killedAt = Number((await client.status()).height);
  await daemon.kill9();
  const open = await table.openGames();
  console.log(`… SIGKILL at block ${killedAt} with ${open.length} hand(s) open; restarting`);
  await sleep(1000);
  daemon.start();
  await Promise.all(loops);
  watcher.check();

  // ---- killed with the dealer's move owed: the hand still finishes inside its deadline
  let mid: Played | undefined;
  for (let tries = 0; !mid && tries < 20; tries++) {
    const { id, bet, height } = await placeBet(play, seats[0], 2n * FANG);
    await until("the deal", async () => {
      const g = await table.game(id);
      return g === null || g.phase === Phase.Player;
    });
    if ((await table.game(id)) === null) continue; // a natural: settled at the deal
    await daemon.kill9();
    // The player stands while the daemon is dead: now the dealer owes its hand, and nobody is there.
    await sure(client, program, alice, call.stand(id));
    const owed = await table.game(id);
    assert.ok(owed && dealers(owed.phase));
    await sleep(2000);
    assert.ok(await table.game(id), "nobody dealt it while the daemon was down");
    daemon.start();
    const settled = await playOut(play, seats[0], id);
    assert.ok(BigInt(settled.height) <= owed.deadline, `settled at block ${settled.height}, inside the deadline (block ${owed.deadline})`);
    mid = { id, bet, betHeight: height, settled };
    played.push(mid);
    console.log(`✓ killed with the dealer's cards owed on game ${id}: restarted, and it settled at block ${settled.height} (deadline ${owed.deadline})`);
  }
  assert.ok(mid, "a hand that reached the player's move");

  // ---- a player who never moves is stood by the daemon's own timeout, and the hand is finished
  let idle: bigint | undefined;
  for (let tries = 0; idle === undefined && tries < 20; tries++) {
    const { id } = await placeBet(play, seats[1], FANG);
    await until("the deal", async () => {
      const g = await table.game(id);
      return g === null || g.phase === Phase.Player;
    });
    const g = await table.game(id);
    if (g === null) continue;
    await waitPast(client, g.deadline);
    const settled = await watcher.settledEvent(id);
    assert.equal(watcher.idleStands.get(id), dealerAddress, "the daemon called the timeout that stood the player");
    assert.equal(settled.by, dealerAddress);
    idle = id;
    console.log(`✓ an idle player on game ${id} was stood by the daemon at their deadline, and the hand was dealt to its end`);
  }
  assert.ok(idle !== undefined);

  // ---- the end: nothing forfeited, everything verified, nothing secret in the log
  await until("no open hands", async () => (await table.openGames()).length === 0);
  await watcher.caughtUp();
  watcher.check();
  assert.ok(played.length >= HANDS, `${played.length} hands played`);
  const settledHands = watcher.settled;
  for (const h of played) {
    const s = settledHands.get(h.id)!;
    assert.ok(s.outcome >= 1 && s.outcome <= 5, `game ${h.id} ended by its cards (outcome ${s.outcome}), not a missed deadline`);
    assert.equal(s.by, dealerAddress, "the daemon's reveal settled it");
  }
  assert.equal([...watcher.settled.values()].filter((s) => s.outcome > 5).length, 0, "no hand was forfeited");
  assert.equal(await daemon.stop(), 0);
  const lines = daemon.log();
  assert.ok(lines.filter((l) => l.action === "started").length >= 3, "the daemon started three times");
  if (watcher.settled.size >= 48) assert.ok(lines.some((l) => l.action === "pruned"), "settled secrets were pruned");
  assert.ok(!lines.some((l) => l.level === "error"), `no error in the daemon's log: ${JSON.stringify(lines.find((l) => l.level === "error"))}`);
  // No link the chain ever published is in the daemon's log, and the only long hex in it is transaction ids.
  const raw = daemon.raw();
  assert.ok(watcher.links.size >= 3 * HANDS);
  for (const link of watcher.links) assert.ok(!raw.includes(link), "a revealed link is in the daemon's log");
  const txs = new Set(lines.map((l) => l.tx).filter((t): t is string => typeof t === "string"));
  for (const run of raw.match(/[0-9a-f]{32,}/g) ?? []) assert.ok(txs.has(run), "long hex in the log that isn't a transaction's id");
  const after = JSON.parse(cli(o, "status", ["--json"])) as { open: unknown[]; stored: number; missing: unknown[]; mismatched: unknown[] };
  assert.deepEqual([after.open, after.missing, after.mismatched], [[], [], []]);
  assert.ok(after.stored <= 24 + 4 + 64, `the store stayed small: ${after.stored} secrets`);

  const h = await table.house();
  assert.deepEqual([h.reserved, h.stakes, h.table, h.open], [0n, 0n, 0n, 0]);
  assert.equal(h.bankroll, watcher.expectedBankroll, "the bankroll is what was funded and what the hands left");
  await sure(client, program, owner, call.defund(h.bankroll));
  await watcher.caughtUp();
  await watcher.invariant();
  await watcher.stop();
  watcher.check();
  void stranger;
  const spent = 200n * FANG - BigInt((await client.account(dealerAddress)).balance);
  console.log(`the dealer spent ${Number(spent) / 1e9} FANG on fees and deposits for ${daemon.log().filter((l) => typeof l.tx === "string").length} transactions`);
  const fuel = [...watcher.fuel].map(([m, f]) => `${m} ≤ ${f.max}`).join(", ");
  console.log(`✅ the dealer's live test passed: ${watcher.settled.size} hands settled and verified, none forfeited, across ${daemon.starts} starts of the daemon; the invariant held at ${watcher.heightsChecked} of blocks ${watcher.firstHeight}–${watcher.lastHeight} (${watcher.heightsSkipped} not seen at the tip, ${watcher.skewRetries} reads retried); fuel: ${fuel}`);
} catch (e) {
  if (daemon) console.error(`--- the daemon's log (tail) ---\n${daemon.raw().split("\n").slice(-30).join("\n")}`);
  throw e;
} finally {
  await daemon?.kill9();
  await watcher?.stop().catch(() => undefined);
  if (!process.env.KEEP) rmSync(dir, { recursive: true, force: true });
}
