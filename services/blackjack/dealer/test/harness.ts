// What the live test and the soak share: the daemon as a child process, a watcher that reads
// every block (the table's events, each receipt's fuel, the money invariant) and seats that play
// hands the way a player's wallet or a relayer would. Local chains only.
import assert from "node:assert/strict";
import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { closeSync, openSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { Outcome, Phase, Table, call, decodeGame, decodeGameId, decodeHouse, decodeOpenList, eventNamed, eventsOf, exposure, payout, relaySeed, send, storageKey, verifyHand, type Game, type TableCall, type TableEvent } from "@blockula/blackjack";
import { ApiError, Client, ProgramFailedError, TxFailedError, TxPendingError, checkReceipt, type ProgramReceipt, type Wallet } from "@blockula/sdk";

export const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
export const CLI = fileURLToPath(new URL("../src/cli.ts", import.meta.url));
export const guest = (file: string) => new Uint8Array(readFileSync(new URL(`../../../../node/vm/tests/programs/${file}`, import.meta.url)));

/** Refuses anything but a node on this machine, on a port this work may use. */
export function localRpc(rpc: string | undefined): string {
  if (!rpc || !/^http:\/\/127\.0\.0\.1:38[23]\d\d$/.test(rpc)) throw new Error("this test runs against a local chain on 127.0.0.1:38200–38399 only");
  return rpc;
}

export type BetEvent = Extract<TableEvent, { name: "bet" }>;
export type SettledEvent = Extract<TableEvent, { name: "settled" }>;
export interface Sure {
  id: string;
  height: number;
  receipt: ProgramReceipt;
  events: TableEvent[];
}

/** The abort code, if `e` is the table refusing a call. */
export const abortOf = (e: unknown): number | undefined => (e instanceof ProgramFailedError ? e.receipt.outcome.abort_code : undefined);

// ---------------------------------------------------------------------------------- the daemon

export interface DaemonOptions {
  /** The daemon's directory: key, secret, store, log. */
  dir: string;
  rpc: string;
  table: string;
  env?: Record<string, string>;
  args?: string[];
}

/** The dealer's command line, run to its end: `init`, `status`. Returns what it printed. */
export function cli(o: DaemonOptions, command: string, args: string[] = []): string {
  return execFileSync(process.execPath, ["--import", "tsx", CLI, command, ...args], { env: daemonEnv(o), encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
}

function daemonEnv(o: DaemonOptions): NodeJS.ProcessEnv {
  // Settings by environment: nothing secret is in it or in the arguments (the key and the secret are files).
  return { PATH: process.env.PATH, HOME: process.env.HOME, TMPDIR: process.env.TMPDIR, BJ_DEALER_RPC: o.rpc, BJ_DEALER_TABLE: o.table, BJ_DEALER_DIR: o.dir, ...o.env };
}

export class Daemon {
  private child?: ChildProcess;
  private exit?: Promise<number | null>;
  starts = 0;
  readonly logPath: string;

  constructor(readonly o: DaemonOptions) {
    this.logPath = join(o.dir, "daemon.log");
  }

  get running(): boolean {
    return this.child !== undefined;
  }

  /** Starts `blackjack-dealer run` as one process (no wrapper), its output appended to daemon.log. */
  start(args: string[] = []): void {
    assert.equal(this.child, undefined, "the daemon is already running");
    const out = openSync(this.logPath, "a");
    const child = spawn(process.execPath, ["--import", "tsx", CLI, "run", ...(this.o.args ?? []), ...args], { env: daemonEnv(this.o), stdio: ["ignore", out, out] });
    closeSync(out);
    this.child = child;
    this.starts++;
    // For whoever cleans up if this process dies first.
    writeFileSync(join(this.o.dir, "daemon.pid"), `${child.pid}\n`);
    this.exit = new Promise((resolve) => child.once("exit", (code) => resolve(code)));
    void this.exit.then(() => {
      if (this.child === child) this.child = undefined;
    });
  }

  /** SIGKILL: no chance to finish anything. */
  async kill9(): Promise<void> {
    const child = this.child;
    if (!child) return;
    child.kill("SIGKILL");
    await this.exit;
    this.child = undefined;
  }

  /** SIGTERM: the daemon finishes what's on its way and exits 0. */
  async stop(): Promise<number | null> {
    const child = this.child;
    if (!child) return null;
    child.kill("SIGTERM");
    const code = await this.exit!;
    this.child = undefined;
    return code;
  }

  /** The daemon's log lines so far. */
  log(): Record<string, unknown>[] {
    let text = "";
    try {
      text = readFileSync(this.logPath, "utf8");
    } catch {
      return [];
    }
    return text.split("\n").filter((l) => l.startsWith("{")).map((l) => JSON.parse(l) as Record<string, unknown>);
  }

  /** Everything the daemon printed, as text. */
  raw(): string {
    try {
      return readFileSync(this.logPath, "utf8");
    } catch {
      return "";
    }
  }
}

// ---------------------------------------------------------------------------------- the watcher

/** A fuel figure a method's receipts showed. */
export interface FuelSeen {
  calls: number;
  max: bigint;
  total: bigint;
}

/**
 * Reads every block as it lands: the table's events (every bet's commitment, every settled hand,
 * verified on the spot), each call's fuel, and at each new height the money invariant from the
 * chain's own state. The first violation stops it and fails everything that waits on it.
 */
export class Watcher {
  readonly bets = new Map<bigint, BetEvent>();
  readonly settled = new Map<bigint, SettledEvent & { height: number; tx: string; by: string }>();
  readonly fuel = new Map<string, FuelSeen>();
  /** Every link the chain has published (the `cards` events'): none may be in the daemon's log. */
  readonly links = new Set<string>();
  readonly outcomes = new Map<number, number>();
  /** Idle stands: the `stand` events with `idle`, by game, and who sent the timeout. */
  readonly idleStands = new Map<bigint, string>();
  failure: Error | undefined;
  heightsChecked = 0;
  heightsSkipped = 0;
  /** Blocks whose invariant read was retried because the height moved between its reads. */
  skewRetries = 0;
  firstHeight = 0;
  lastHeight = 0;
  /** The bankroll the events add up to: funded − defunded + what settled hands left. Checked at the end. */
  expectedBankroll = 0n;
  private scanned = 0;
  private stopped = false;
  private loop?: Promise<void>;

  constructor(
    readonly client: Client,
    readonly table: Table,
    readonly program: string,
  ) {}

  start(fromHeight: number): void {
    this.scanned = fromHeight;
    this.firstHeight = fromHeight + 1;
    this.loop = this.run();
  }

  async stop(): Promise<void> {
    this.stopped = true;
    await this.loop;
  }

  /** Throws the first violation, if there was one. */
  check(): void {
    if (this.failure) throw this.failure;
  }

  private async run(): Promise<void> {
    let lastChecked = this.scanned;
    while (!this.stopped && !this.failure) {
      try {
        const tip = Number((await this.client.status()).height);
        if (tip > lastChecked) {
          if (lastChecked && tip > lastChecked + 1) this.heightsSkipped += tip - lastChecked - 1;
          await this.invariant();
          lastChecked = tip;
        }
        for (let h = this.scanned + 1; h <= tip; h++) {
          await this.block(h);
          this.scanned = h;
          this.lastHeight = h;
        }
      } catch (e) {
        if (e instanceof assert.AssertionError || (e instanceof Error && e.message.startsWith("VIOLATION"))) {
          this.failure = e;
          return;
        }
        // The node didn't answer: ask again.
      }
      await sleep(60);
    }
  }

  private async api(path: string): Promise<any> {
    const res = await fetch(`${this.client.rpc}/api${path}`, { signal: AbortSignal.timeout(10_000) });
    if (!res.ok) throw new Error(`${path} answered ${res.status}`);
    return res.json();
  }

  /** One block's program transactions: events, fuel. */
  private async block(height: number): Promise<void> {
    const b = await this.api(`/block/${height}`);
    for (const t of b.txs as any[]) {
      if (t?.action?.type !== "call_program") continue;
      const tx = t.program === undefined ? await this.api(`/tx/${t.id}`) : t;
      const receipt = checkReceipt(this.client.rpc, tx);
      if (!receipt) continue;
      const first = tx.action.calls?.[0];
      if (first?.program !== this.program) continue;
      const method = String(first.method);
      const seen = this.fuel.get(method) ?? { calls: 0, max: 0n, total: 0n };
      seen.calls++;
      seen.total += receipt.facts.fuel.used;
      if (receipt.facts.fuel.used > seen.max) seen.max = receipt.facts.fuel.used;
      this.fuel.set(method, seen);
      if (!receipt.outcome.ok) continue;
      for (const e of eventsOf(receipt, this.program)) this.event(e, height, String(tx.id), String(tx.from?.address ?? tx.from), receipt);
    }
  }

  private event(e: TableEvent, height: number, tx: string, by: string, receipt: ProgramReceipt): void {
    if (e.name === "bet") this.bets.set(e.game, e);
    else if (e.name === "cards") for (const c of e.cards) this.links.add(c.link);
    else if (e.name === "fund") this.expectedBankroll += e.amount;
    else if (e.name === "defund") this.expectedBankroll -= e.amount;
    else if (e.name === "stand" && e.idle) this.idleStands.set(e.game, by);
    else if (e.name === "settled") {
      const bet = this.bets.get(e.game);
      assert.ok(bet, `VIOLATION: game ${e.game} settled without a bet event before it`);
      // Every card recomputed from the settled event, down to the commitment the bet took.
      const proof = verifyHand({ ...e, commit: bet.commit });
      assert.equal(e.stake, bet.stake, `VIOLATION: game ${e.game}'s stake`);
      assert.equal(e.clientSeed, bet.clientSeed, `VIOLATION: game ${e.game}'s client seed`);
      assert.equal(e.payout, payout(e.stake, e.outcome), `VIOLATION: game ${e.game}'s payout`);
      assert.equal(e.toTable, bet.table && e.payout > 0n, `VIOLATION: game ${e.game} paid to the wrong place`);
      assert.equal(proof.forfeited, e.outcome === Outcome.Forfeit || e.outcome === Outcome.ForfeitDeal);
      // What moved: the program paid exactly the payout to the account (or nothing, to a table balance),
      // and the player got it (with the hand's storage deposit, which the bettor had bonded).
      const change = (account: string) => receipt.facts.balance_changes.find((c) => c.account === account)?.change ?? 0n;
      assert.equal(change(this.program), e.toTable ? 0n : -e.payout, `VIOLATION: game ${e.game}: the program's balance moved by ${change(this.program)}, not the payout`);
      if (!e.toTable && e.player !== by) {
        const got = change(e.player);
        assert.ok(got >= e.payout && got < e.payout + 200_000_000n, `VIOLATION: game ${e.game}: the player got ${got} drops for a payout of ${e.payout}`);
      }
      this.expectedBankroll += e.stake - e.payout;
      this.outcomes.set(e.outcome, (this.outcomes.get(e.outcome) ?? 0) + 1);
      this.settled.set(e.game, { ...e, height, tx, by });
    }
  }

  /**
   * The money, at the chain's tip: the program's balance is bankroll + reserved + stakes + table,
   * and `reserved` and `stakes` are the open hands' sums. Every read says which height's state it
   * is; a set that straddles a block is read again.
   */
  async invariant(): Promise<void> {
    for (let attempt = 0; attempt < 40; attempt++) {
      const a = await this.table.entry(storageKey.house);
      if (a.value === null) throw new Error("VIOLATION: the house record is gone");
      const house = decodeHouse(a.value, this.table.prefix);
      const list = await this.table.entry(storageKey.open);
      const ids = list.value === null ? [] : decodeOpenList(list.value, house.open);
      const games = await Promise.all(ids.map((id) => this.table.entry(storageKey.game(id))));
      const balance = BigInt((await this.client.account(this.program)).balance);
      const b = await this.table.entry(storageKey.house);
      if ([list.height, ...games.map((g) => g.height), b.height].some((h) => h !== a.height) || b.value !== a.value) {
        this.skewRetries++;
        continue;
      }
      const at = `at block ${a.height}`;
      assert.equal(balance, house.bankroll + house.reserved + house.stakes + house.table, `VIOLATION ${at}: the program holds ${balance} drops; bankroll ${house.bankroll} + reserved ${house.reserved} + stakes ${house.stakes} + table ${house.table}`);
      let reserved = 0n;
      let stakes = 0n;
      for (const [i, g] of games.entries()) {
        assert.ok(g.value !== null, `VIOLATION ${at}: game ${ids[i]} is on the open list without a record`);
        const game = decodeGame(ids[i], g.value, this.table.prefix);
        reserved += exposure(game.stake);
        stakes += game.stake;
      }
      assert.equal(ids.length, house.open, `VIOLATION ${at}: the open list's length`);
      assert.equal(house.reserved, reserved, `VIOLATION ${at}: reserved isn't the open hands' exposure`);
      assert.equal(house.stakes, stakes, `VIOLATION ${at}: stakes isn't the open hands' stakes`);
      this.heightsChecked++;
      return;
    }
    throw new Error("VIOLATION: couldn't read the table at one height in 40 tries");
  }

  /** Waits until the watcher has read the block that settled `game`. */
  async settledEvent(game: bigint, waitMs = 120_000): Promise<SettledEvent & { height: number; tx: string; by: string }> {
    const until = Date.now() + waitMs;
    for (;;) {
      this.check();
      const s = this.settled.get(game);
      if (s) return s;
      if (Date.now() > until) throw new Error(`game ${game} didn't settle in ${waitMs} ms`);
      await sleep(50);
    }
  }

  /** Waits until the watcher has read every block up to the chain's tip now. */
  async caughtUp(): Promise<void> {
    const tip = Number((await this.client.status()).height);
    while (this.scanned < tip) {
      this.check();
      await sleep(50);
    }
  }
}

// ---------------------------------------------------------------------------------- playing

/**
 * The fuel a player's or the owner's call reserves here. A call pays for the fuel it reserves and
 * a block carries 40,000,000 of it, so at the default (the most one transaction may have) three
 * calls fill a block; these are about three times what each method was seen to use.
 */
export function fuelFor(method: string): number {
  if (method === "bet" || method === "relay_bet") return 800_000;
  if (method === "timeout") return 1_000_000;
  return 500_000;
}

/**
 * Sends one call and waits for it however long it takes: a transaction that was submitted and
 * not seen final in time is followed by its id, never sent again.
 */
export async function sure(client: Client, program: string, wallet: Wallet, c: TableCall): Promise<Sure> {
  const done = (tx: any): Sure => {
    const receipt = checkReceipt(client.rpc, tx);
    if (!receipt) throw new Error(`tx ${tx.id} is final without a receipt`);
    return { id: String(tx.id), height: Number(tx.height), receipt, events: eventsOf(receipt, program) };
  };
  let signed: string | undefined;
  try {
    const r = await send(wallet, program, c, { fuelLimit: fuelFor(c.method), onSigned: (_tx, id) => void (signed = id) });
    return { id: String(r.tx.id), height: Number(r.tx.height), receipt: r.receipt, events: eventsOf(r.receipt, program) };
  } catch (e) {
    if (e instanceof ProgramFailedError) throw e;
    const id = e instanceof TxPendingError ? e.id : e instanceof TxFailedError && e.dropped ? e.id : signed;
    if (id === undefined || (e instanceof ApiError && e.status >= 400 && e.status < 500)) throw e;
    // Submitted, fate unknown: follow it.
    for (const until = Date.now() + 120_000; Date.now() < until; await sleep(300)) {
      const tx = await client.tx(id).catch(() => null);
      if (tx?.status === "finalized") return done(tx);
      if (tx?.status === "failed" && tx.dropped !== true && tx.action !== undefined) {
        const receipt = checkReceipt(client.rpc, tx);
        if (receipt && !receipt.outcome.ok) throw new ProgramFailedError(id, "failed", tx, receipt);
        throw new Error(`tx ${id} failed: ${String(tx.error)}`);
      }
    }
    throw new Error(`tx ${id} was submitted and never became final`);
  }
}

/** Someone at the table: an account that signs its own bets and moves, or one played by a relayer from its table balance. */
export interface Seat {
  name: string;
  wallet: Wallet;
  /** The relayer that plays this seat's table balance, if it's a relayed seat. */
  relayer?: Wallet;
  /** Hits below this total. */
  hitBelow: number;
}

let posts = 1_900_000_000_000_000_000n;
const nextPost = () => ++posts;

export interface Played {
  id: bigint;
  bet: BetEvent;
  betHeight: number;
  settled: SettledEvent & { height: number; tx: string; by: string };
}

export interface Play {
  client: Client;
  table: Table;
  program: string;
  watcher: Watcher;
  /** Called with the hand once it's open and it's the player's move for the first time (the test's hook for a restart mid-hand). */
  onPlayerTurn?: (g: Game) => Promise<void>;
}

/** Places a bet for `seat`. Returns the hand's id and its bet event. Throws `ProgramFailedError` when the table refuses it. */
export async function placeBet(p: Play, seat: Seat, stake: bigint): Promise<{ id: bigint; bet: BetEvent; height: number }> {
  const seed = randomBytes(32).toString("hex");
  const post = nextPost();
  const r = seat.relayer ? await sure(p.client, p.program, seat.relayer, call.relayBet(seat.wallet.address, stake, post)) : await sure(p.client, p.program, seat.wallet, call.bet(stake, seed));
  const id = decodeGameId(r.receipt.claims.return_data[0].data);
  const bet = eventNamed(r.events, "bet")!;
  assert.deepEqual([bet.game, bet.player, bet.stake, bet.table], [id, seat.wallet.address, stake, seat.relayer !== undefined]);
  assert.equal(bet.clientSeed, seat.relayer ? relaySeed(seat.wallet.address, post) : seed, "the client seed is the player's (or the post's)");
  return { id, bet, height: r.height };
}

/** The player's move on an open hand: its own, or its relayer's. Returns false when the hand had moved on (the idle timeout got there first). */
export async function move(p: Play, seat: Seat, id: bigint, hit: boolean): Promise<boolean> {
  const c = seat.relayer ? (hit ? call.relayHit(id, nextPost()) : call.relayStand(id, nextPost())) : hit ? call.hit(id) : call.stand(id);
  try {
    await sure(p.client, p.program, seat.relayer ?? seat.wallet, c);
    return true;
  } catch (e) {
    if (abortOf(e) === 56 || abortOf(e) === 54) return false;
    throw e;
  }
}

/** Plays the open hand `id` to its end as `seat` would, and returns its settled event (verified by the watcher). */
export async function playOut(p: Play, seat: Seat, id: bigint): Promise<SettledEvent & { height: number; tx: string; by: string }> {
  let told = false;
  for (;;) {
    p.watcher.check();
    const g = await p.table.game(id);
    if (g === null) break;
    if (g.phase === Phase.Player) {
      if (!told && p.onPlayerTurn) {
        told = true;
        await p.onPlayerTurn(g);
      }
      await move(p, seat, id, g.playerTotal < seat.hitBelow);
    } else await sleep(80);
  }
  return p.watcher.settledEvent(id);
}

/** One whole hand: the bet, the moves, the settled event. */
export async function playHand(p: Play, seat: Seat, stake: bigint): Promise<Played> {
  const { id, bet, height } = await placeBet(p, seat, stake);
  return { id, bet, betHeight: height, settled: await playOut(p, seat, id) };
}

/** Waits for the chain to pass `height`. */
export async function waitPast(client: Client, height: bigint | number): Promise<void> {
  while (BigInt((await client.status()).height) <= BigInt(height)) await sleep(150);
}

/** Waits until `ok()` is true. */
export async function until(what: string, ok: () => Promise<boolean> | boolean, waitMs = 60_000): Promise<void> {
  for (const end = Date.now() + waitMs; ; await sleep(100)) {
    if (await ok()) return;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
  }
}
