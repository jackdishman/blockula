/**
 * A blackjack table in memory, for the tests and the fixtures: the program's relayed path with
 * its rules, limits and abort codes, the rules of `@blockula/blackjack`, and real hash chains.
 * The cards are the ones a real table would deal from the same chains and post ids, and every
 * settled hand passes `verifyHand`.
 *
 * Everything is deterministic from `seed`. Blocks follow the clock (one every `blockMs`), the
 * dealer reveals `dealerDelay` blocks after it owes a card (or never), and a test can make
 * the next transaction fail the way a killed process or a dropped connection would.
 */
import { createHash } from "node:crypto";
import { DEFAULT_LIMITS, DROPS_PER_FANG, Hand, Outcome, Phase, bytesAddress, card, chainFromTop, dealers, exposure, natural, payout, relaySeed, total, type Game, type House, type Limits, type PlayerRecord, type TableEvent } from "@blockula/blackjack";
import { checkXProof } from "@blockula/sdk";
import { TableRefused, type Clock, type Landed, type LinkClaim, type OnSigned, type TableDriver, type TxState, type VerifyLink } from "./types.js";

const sha = (s: string) => createHash("sha256").update(s).digest();

/** A clock a test moves by hand: `sleep` moves it forward and returns at once. */
export class FakeClock implements Clock {
  constructor(private t = Date.UTC(2026, 0, 1)) {}
  now(): number {
    return this.t;
  }
  advance(ms: number): void {
    this.t += ms;
  }
  async sleep(ms: number): Promise<void> {
    this.t += ms;
  }
}

/** What the process "dying" looks like to a test. */
export class Crash extends Error {
  constructor(where: string) {
    super(`crash: ${where}`);
    this.name = "Crash";
  }
}

/** Where the next transaction fails. */
export type Fault =
  /** Before it's signed: nothing exists. */
  | "before-sign"
  /** Signed (the bot was told its id), never submitted. */
  | "before-submit"
  /** Submitted and run, but the bot never heard. */
  | "after-submit";

interface FakePlayer {
  balance: bigint;
  day: bigint;
  wagered: bigint;
  openGame: bigint | null;
  grant: { relayer: string; maxBet: bigint; dailyCap: bigint; expiresAt: bigint; day: bigint; wagered: bigint } | null;
  last: PlayerRecord["last"];
}

interface FakeGame extends Game {
  /** A relayed hand's seed is derived at the bet: it never waits for one (`Phase.Seed`). */
  clientSeed: string;
  seedHash: null;
  hand: Hand;
  chain: string[];
  /** The block the dealer reveals in, or `null` when it owes nothing or never will. */
  revealAt: bigint | null;
}

export interface FakeTableOptions {
  seed: string;
  clock: Clock;
  blockMs?: number;
  limits?: Limits;
  /** The house's bankroll, drops. */
  bankroll?: bigint;
  /** Blocks the dealer takes to reveal; `null`: it never does. */
  dealerDelay?: number | null;
  /** Blocks a relayed transaction stays valid after it's signed. */
  validBlocks?: number;
}

/** What flowed in and out of one account's table balance: the fixtures reconcile against it. */
export interface Ledger {
  deposited: bigint;
  withdrawn: bigint;
  staked: bigint;
  paid: bigint;
}

export class FakeTable implements TableDriver {
  readonly relayer: string;
  readonly limits_: Limits;
  readonly house_: House;
  readonly players = new Map<string, FakePlayer>();
  readonly games = new Map<bigint, FakeGame>();
  readonly ledger = new Map<string, Ledger>();
  /** Every `relayed` event, in order. */
  readonly relayed: Extract<TableEvent, { name: "relayed" }>[] = [];
  /** Every settled hand, in order. */
  readonly settledHands: Extract<TableEvent, { name: "settled" }>[] = [];
  /** FANG the program holds: what its methods took in less what they paid out. */
  balance = 0n;
  dealerDelay: number | null;
  /** The most hands ever open at once for one account: the rules allow one. */
  maxOpenPerPlayer = 0;
  /** Set to fail the next transaction; cleared when it does. */
  fault: Fault | null = null;
  /** After a crash every call fails until `revive`: the process is gone. */
  dead = false;
  relayerApproved = true;
  private readonly links = new Map<string, LinkClaim>();
  /** The local stand-in for posts on X: by id. */
  readonly posts = new Map<string, { handle: string; text: string }>();
  private readonly txs = new Map<string, TxState>();
  private readonly clock: Clock;
  private readonly seed: string;
  private readonly blockMs: number;
  private readonly t0: number;
  private readonly validBlocks: number;
  private txCount = 0;

  constructor(o: FakeTableOptions) {
    this.seed = o.seed;
    this.clock = o.clock;
    this.blockMs = o.blockMs ?? 2_000;
    this.t0 = o.clock.now();
    this.limits_ = { ...(o.limits ?? DEFAULT_LIMITS) };
    this.dealerDelay = o.dealerDelay === undefined ? 0 : o.dealerDelay;
    this.validBlocks = o.validBlocks ?? 20;
    this.relayer = this.account("relayer");
    const bankroll = o.bankroll ?? 1_000n * DROPS_PER_FANG;
    this.house_ = { owner: this.account("owner"), dealer: this.account("dealer"), bankroll, reserved: 0n, stakes: 0n, table: 0n, nextCommit: 1n << 32n, nextGame: 0n, paused: false, open: 0 };
    this.balance = bankroll;
  }

  /** A deterministic address for a name. */
  account(name: string): string {
    return bytesAddress(new Uint8Array(sha(`${this.seed}:account:${name}`)), "boo");
  }

  private now(): bigint {
    return 100n + BigInt(Math.floor((this.clock.now() - this.t0) / this.blockMs));
  }

  private rec(account: string): FakePlayer {
    let p = this.players.get(account);
    if (!p) this.players.set(account, (p = { balance: 0n, day: 0n, wagered: 0n, openGame: null, grant: null, last: null }));
    return p;
  }

  private led(account: string): Ledger {
    let l = this.ledger.get(account);
    if (!l) this.ledger.set(account, (l = { deposited: 0n, withdrawn: 0n, staked: 0n, paid: 0n }));
    return l;
  }

  /** The invariant of the program: every drop it holds is in one of the four counters. */
  invariant(): boolean {
    const h = this.house_;
    return this.balance === h.bankroll + h.reserved + h.stakes + h.table;
  }

  // ---- what players and the owner do with their own keys (not the relayer's to do) ----

  deposit(account: string, amount: bigint): void {
    this.sync();
    const p = this.rec(account);
    if (amount === 0n) throw new TableRefused(44);
    if (p.balance + amount > this.limits_.depositCap) throw new TableRefused(62);
    p.balance += amount;
    this.house_.table += amount;
    this.balance += amount;
    this.led(account).deposited += amount;
  }

  /** `set_relayer` to this table's relayer. `expiresIn`: blocks from now. */
  grant(account: string, maxBet: bigint, dailyCap: bigint, expiresIn: bigint, relayer: string = this.relayer): void {
    this.sync();
    if (relayer === this.relayer && !this.relayerApproved) throw new TableRefused(64);
    const l = this.limits_;
    if (!(maxBet >= 1n && maxBet <= l.relayMaxBet && dailyCap >= maxBet && expiresIn > 0n && expiresIn <= l.grantMaxBlocks)) throw new TableRefused(67);
    this.rec(account).grant = { relayer, maxBet, dailyCap, expiresAt: this.now() + expiresIn, day: 0n, wagered: 0n };
  }

  revoke(account: string): void {
    this.rec(account).grant = null;
  }

  pause(on: boolean): void {
    this.house_.paused = on;
  }

  /** The account's link record: it names `handle` and the proof post `postId`. */
  setLink(account: string, handle: string, postId: string): LinkClaim {
    const claim = { handle, post_id: postId, code: `blockula-x-${sha(`${this.seed}:code:${account}:${handle.toLowerCase()}`).toString("hex").slice(0, 40)}` };
    this.links.set(account, claim);
    return claim;
  }

  unlink(account: string): void {
    this.links.delete(account);
  }

  /** Links `account` to `handle` with a proof post by `postedBy` (the handle itself unless a test says otherwise). */
  linkWithProof(account: string, handle: string, postId: string, postedBy: string = handle): void {
    const claim = this.setLink(account, handle, postId);
    this.posts.set(postId, { handle: postedBy, text: `Linking my Blockula account: ${claim.code}` });
  }

  /** The stand-in for the check against X: it reads this table's local posts and nothing else. */
  verifier(): VerifyLink {
    return async (claim) => {
      const post = this.posts.get(claim.post_id);
      if (!post) return { verified: false };
      return { verified: checkXProof(claim, { id: claim.post_id, handle: post.handle, untrusted_text: post.text }).verified };
    };
  }

  /** The dealer calls `timeout` on a player who ran out of time: they stand, and the dealer plays on. */
  idleTimeout(id: bigint): void {
    this.sync();
    const g = this.games.get(id);
    if (!g) throw new TableRefused(54);
    if (this.now() <= g.deadline) throw new TableRefused(60);
    if (!g.hand.stand()) throw new TableRefused(56);
    this.owes(g);
  }

  /** Blocks until hand `id`'s deadline has passed, in milliseconds of clock. */
  msPastDeadline(id: bigint): number {
    const g = this.games.get(id);
    if (!g) return 0;
    return Math.max(0, Number(g.deadline + 1n - this.now())) * this.blockMs;
  }

  /** The node forgets every transaction (a pruned node, another node): only the table's state is left to ask. */
  forgetTxs(): void {
    this.txs.clear();
  }

  revive(): void {
    this.dead = false;
    this.fault = null;
  }

  // ---- the chain's side ----

  /** Runs the dealer's reveals that are due. Every read and call starts here. */
  private sync(): void {
    if (this.dead) throw new Crash("the process is gone");
    const now = this.now();
    for (const g of [...this.games.values()].sort((a, b) => (a.id < b.id ? -1 : 1))) {
      if (g.revealAt !== null && g.revealAt <= now) this.reveal(g);
    }
  }

  /** Copies the hand's state into its record and sets what the dealer owes. */
  private owes(g: FakeGame): void {
    const now = this.now();
    g.phase = g.hand.phase;
    g.dealt = g.hand.dealt;
    g.playerCards = [...g.hand.player];
    g.dealerCards = [...g.hand.dealer];
    g.playerTotal = total(g.playerCards).total;
    g.dealerTotal = total(g.dealerCards).total;
    if (dealers(g.hand.phase)) {
      g.deadline = now + this.limits_.revealBlocks;
      g.revealAt = this.dealerDelay === null ? null : now + BigInt(this.dealerDelay);
    } else {
      g.revealAt = null;
    }
  }

  /** The dealer reveals every link the hand is owed now. */
  private reveal(g: FakeGame): void {
    let outcome: Outcome | null = null;
    while (dealers(g.hand.phase) && outcome === null) {
      const j = g.hand.dealt + 1;
      const step = g.hand.deal(card(g.chain[j], g.clientSeed, g.id, j));
      g.link = g.chain[j];
      if (step && step.kind === "settled") outcome = step.outcome;
    }
    const deadline = g.deadline;
    this.owes(g);
    if (outcome !== null) this.settle(g, outcome);
    else if (g.hand.phase === Phase.Player) g.deadline = this.now() + this.limits_.idleBlocks;
    else g.deadline = deadline;
  }

  private settle(g: FakeGame, outcome: Outcome): Extract<TableEvent, { name: "settled" }> {
    const h = this.house_;
    const pays = payout(g.stake, outcome);
    const reserve = exposure(g.stake);
    h.reserved -= reserve;
    h.stakes -= g.stake;
    h.bankroll += g.stake + reserve - pays;
    h.open -= 1;
    this.games.delete(g.id);
    const p = this.rec(g.player);
    p.openGame = null;
    p.last = { game: g.id, outcome, dealt: g.hand.dealt, nPlayer: g.hand.player.length, stake: g.stake, payout: pays, clientSeed: g.clientSeed, seedHash: null, link: g.link };
    if (pays > 0n) {
      p.balance += pays;
      h.table += pays;
      this.led(g.player).paid += pays;
    }
    const ev = { name: "settled" as const, game: g.id, player: g.player, outcome, stake: g.stake, payout: pays, toTable: pays > 0n, dealt: g.hand.dealt, nPlayer: g.hand.player.length, clientSeed: g.clientSeed, seedHash: null, link: g.link, playerTotal: total(g.hand.player).total, dealerTotal: total(g.hand.dealer).total };
    this.settledHands.push(ev);
    return ev;
  }

  /** The caller must be the player's relayer, approved, with a grant that hasn't expired. */
  private liveGrant(p: FakePlayer): void {
    if (!p.grant || p.grant.relayer !== this.relayer || !this.relayerApproved) throw new TableRefused(64);
    if (this.now() > p.grant.expiresAt) throw new TableRefused(65);
  }

  /** One transaction by the relayer: signed, submitted, run. `apply` is the program's method. */
  private async run(onSigned: OnSigned, apply: () => TableEvent[]): Promise<Landed> {
    this.sync();
    if (this.fault === "before-sign") this.die("before the transaction was signed");
    const txId = sha(`${this.seed}:tx:${this.txCount++}`).toString("hex");
    const height = this.now();
    await onSigned(txId, height + BigInt(this.validBlocks));
    if (this.fault === "before-submit") this.die("after signing, before submitting");
    let landed: Landed;
    try {
      landed = { txId, height, events: apply() };
    } catch (e) {
      if (!(e instanceof TableRefused)) throw e;
      this.txs.set(txId, { status: "refused", code: e.code, txId });
      if (this.fault === "after-submit") this.die("after a refused transaction ran");
      throw new TableRefused(e.code, txId);
    }
    this.txs.set(txId, { status: "landed", landed });
    for (const e of landed.events) if (e.name === "relayed") this.relayed.push(e);
    if (this.fault === "after-submit") this.die("after the transaction ran");
    return landed;
  }

  private die(where: string): never {
    this.fault = null;
    this.dead = true;
    throw new Crash(where);
  }

  // ---- TableDriver ----

  async height(): Promise<bigint> {
    this.sync();
    return this.now();
  }

  async house(): Promise<House> {
    this.sync();
    return { ...this.house_ };
  }

  async limits(): Promise<Limits> {
    this.sync();
    return { ...this.limits_ };
  }

  async player(account: string): Promise<PlayerRecord> {
    this.sync();
    const p = this.rec(account);
    return { balance: p.balance, day: p.day, wagered: p.wagered, openGame: p.openGame, grant: p.grant ? { ...p.grant } : null, last: p.last ? { ...p.last } : null };
  }

  async game(id: bigint): Promise<Game | null> {
    this.sync();
    const g = this.games.get(id);
    if (!g) return null;
    return { id: g.id, player: g.player, stake: g.stake, clientSeed: g.clientSeed, seedHash: null, link: g.link, deadline: g.deadline, postId: g.postId, phase: g.phase, table: g.table, dealt: g.dealt, playerCards: [...g.playerCards], dealerCards: [...g.dealerCards], playerTotal: g.playerTotal, dealerTotal: g.dealerTotal };
  }

  async granted(): Promise<string[]> {
    this.sync();
    return [...this.players.entries()].filter(([, p]) => p.grant?.relayer === this.relayer).map(([a]) => a);
  }

  async link(account: string): Promise<LinkClaim | null> {
    this.sync();
    const c = this.links.get(account);
    return c ? { ...c } : null;
  }

  async tx(id: string): Promise<TxState> {
    this.sync();
    return this.txs.get(id) ?? { status: "unknown" };
  }

  relayBet(player: string, amount: bigint, postId: bigint, onSigned: OnSigned): Promise<Landed> {
    return this.run(onSigned, () => {
      const [h, l, p, now] = [this.house_, this.limits_, this.rec(player), this.now()];
      this.liveGrant(p);
      const grant = p.grant!;
      if (amount === 0n) throw new TableRefused(44);
      const day = now / l.dayBlocks;
      const granted = (grant.day === day ? grant.wagered : 0n) + amount;
      if (amount > grant.maxBet || amount > l.relayMaxBet || granted > grant.dailyCap) throw new TableRefused(66);
      if (amount > p.balance) throw new TableRefused(63);
      // `open_hand`'s checks, in the program's order.
      if (h.paused) throw new TableRefused(46);
      if (amount < l.minBet) throw new TableRefused(47);
      if (amount > l.maxBet || amount > h.bankroll / l.bankrollDivisor) throw new TableRefused(48);
      const reserve = exposure(amount);
      if (reserve > h.bankroll) throw new TableRefused(49);
      if (h.reserved + reserve > l.maxExposure) throw new TableRefused(69);
      if (BigInt(h.open) >= l.maxOpenGames) throw new TableRefused(51);
      if (p.openGame !== null) throw new TableRefused(52);
      const wagered = (p.day === day ? p.wagered : 0n) + amount;
      if (wagered > l.dailyCap) throw new TableRefused(50);

      grant.day = day;
      grant.wagered = granted;
      p.balance -= amount;
      h.table -= amount;
      this.led(player).staked += amount;
      const id = h.nextGame;
      const chain = chainFromTop(sha(`${this.seed}:chain:${id}`).toString("hex"));
      h.bankroll -= reserve;
      h.reserved += reserve;
      h.stakes += amount;
      h.nextGame = id + 1n;
      h.open += 1;
      p.day = day;
      p.wagered = wagered;
      p.openGame = id;
      const seed = relaySeed(player, postId);
      const g: FakeGame = { id, player, stake: amount, clientSeed: seed, seedHash: null, link: chain[0], deadline: 0n, postId, phase: Phase.Deal, table: true, dealt: 0, playerCards: [], dealerCards: [], playerTotal: 0, dealerTotal: 0, hand: new Hand(), chain, revealAt: null };
      this.games.set(id, g);
      this.owes(g);
      this.maxOpenPerPlayer = Math.max(this.maxOpenPerPlayer, [...this.games.values()].filter((x) => x.player === player).length);
      return [
        { name: "bet", game: id, player, stake: amount, seedHash: null, clientSeed: seed, commit: chain[0], deadline: g.deadline, table: true, postId },
        { name: "relayed", player, relayer: this.relayer, postId, action: "bet", game: id },
      ];
    });
  }

  private move(id: bigint, postId: bigint, hit: boolean, onSigned: OnSigned): Promise<Landed> {
    return this.run(onSigned, () => {
      const g = this.games.get(id);
      if (!g) throw new TableRefused(54);
      if (!g.table) throw new TableRefused(68);
      this.liveGrant(this.rec(g.player));
      if (!(hit ? g.hand.hit() : g.hand.stand())) throw new TableRefused(56);
      this.owes(g);
      const moved: TableEvent = hit ? { name: "hit", game: id, player: g.player, deadline: g.deadline } : { name: "stand", game: id, player: g.player, deadline: g.deadline, idle: false };
      return [moved, { name: "relayed", player: g.player, relayer: this.relayer, postId, action: hit ? "hit" : "stand", game: id }];
    });
  }

  relayHit(game: bigint, postId: bigint, onSigned: OnSigned): Promise<Landed> {
    return this.move(game, postId, true, onSigned);
  }

  relayStand(game: bigint, postId: bigint, onSigned: OnSigned): Promise<Landed> {
    return this.move(game, postId, false, onSigned);
  }

  relayWithdraw(player: string, postId: bigint, onSigned: OnSigned): Promise<Landed> {
    return this.run(onSigned, () => {
      const p = this.rec(player);
      this.liveGrant(p);
      const amount = p.balance;
      if (amount === 0n) throw new TableRefused(63);
      // The whole table balance, to the player's own account: there is nowhere else it can go.
      p.balance = 0n;
      this.house_.table -= amount;
      this.balance -= amount;
      this.led(player).withdrawn += amount;
      return [
        { name: "withdraw", player, amount, balance: 0n },
        { name: "relayed", player, relayer: this.relayer, postId, action: "withdraw", game: 0n },
      ];
    });
  }

  timeout(id: bigint, onSigned: OnSigned): Promise<Landed> {
    return this.run(onSigned, () => {
      const g = this.games.get(id);
      if (!g) throw new TableRefused(54);
      if (this.now() <= g.deadline) throw new TableRefused(60);
      if (g.hand.stand()) {
        this.owes(g);
        return [{ name: "stand", game: id, player: g.player, deadline: g.deadline, idle: true }];
      }
      // The dealer missed a reveal: the player is paid the most the hand could still have paid.
      const outcome = g.hand.phase === Phase.Deal || natural(g.hand.player) ? Outcome.ForfeitDeal : Outcome.Forfeit;
      return [this.settle(g, outcome)];
    });
  }
}
