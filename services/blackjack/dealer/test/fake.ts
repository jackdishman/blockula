// A table in memory for the dealer's offline tests: the program's turns, deadlines and abort
// codes (node/vm/guests/blackjack), without the money. It is both what the dealer reads and what
// it sends to, and a test can make any send go wrong the ways a real one can.
import { DEFAULT_LIMITS, Outcome, Phase, card, dealers, hexBytes, natural, payout, prevLink, total, handOf, type Game, type House, type Limits, type TableCall, type TableEvent } from "@blockula/blackjack";
import type { SendOptions, Sent, TableReader, TxSender } from "../src/dealer.js";

export const DEALER = "boo1dealer";
export const OWNER = "boo1owner";

/** What a test makes of one send: it lands, it never arrives, it lands unseen, the node holds it, the node refuses it, the sender breaks. */
export type Fate = "ok" | "lost" | "ghost" | "held" | "unsent" | "throw" | "dry";

function u64(b: Uint8Array, at: number): bigint {
  return new DataView(b.buffer, b.byteOffset).getBigUint64(at, true);
}

/** A counted input: head (8) ‖ n (4) ‖ n × 32. */
function counted(inputHex: string): { head: bigint; items: string[] } {
  const b = hexBytes(inputHex);
  const n = new DataView(b.buffer, b.byteOffset).getUint32(8, true);
  const items: string[] = [];
  for (let i = 0; i < n; i++) items.push(Buffer.from(b.subarray(12 + 32 * i, 44 + 32 * i)).toString("hex"));
  return { head: u64(b, 0), items };
}

export interface SettledHand {
  game: bigint;
  outcome: Outcome;
  clientSeed: string;
  link: string;
  dealt: number;
  nPlayer: number;
  stake: bigint;
  payout: bigint;
  commit: string;
  /** Who sent the call that settled it. */
  by: string;
}

export class FakeTable implements TableReader, TxSender {
  /** Across every table of a test run: a timeout from the dealer that reached a hand it owed cards on. */
  static readonly violations: string[] = [];
  readonly address = DEALER;
  height = 100;
  dealer = DEALER;
  nextCommit = 0n;
  nextGame = 0n;
  limits_: Limits = { ...DEFAULT_LIMITS, revealBlocks: 20n, idleBlocks: 30n };
  readonly commitments = new Map<bigint, string>();
  readonly games = new Map<bigint, Game>();
  readonly commitOf = new Map<bigint, string>();
  open: bigint[] = [];
  readonly settled: SettledHand[] = [];
  /** Every call the dealer asked to send, in order. */
  readonly sends: { call: TableCall; validForBlocks?: number; fuelLimit?: number; fate: Fate; id: string }[] = [];
  /** What the next sends become (default: every send lands). */
  fate: (c: TableCall) => Fate = () => "ok";
  /** Runs just before a send is applied: a test's chance to move the table first. */
  beforeApply: (c: TableCall) => void = () => undefined;
  balance_ = 1_000_000_000_000n;
  /** Reads that should fail: a node that's down. */
  down = false;
  private readonly results = new Map<string, Sent>();
  private readonly waiting = new Map<string, TableCall>();
  private txs = 0;

  // ---- what the dealer reads

  private up(): void {
    if (this.down) throw new Error("the node didn't answer");
  }

  async house(): Promise<House & { height: number }> {
    this.up();
    return { owner: OWNER, dealer: this.dealer, bankroll: 0n, reserved: 0n, stakes: 0n, table: 0n, nextCommit: this.nextCommit, nextGame: this.nextGame, paused: false, open: this.open.length, height: this.height };
  }

  async limits(): Promise<Limits> {
    this.up();
    return { ...this.limits_ };
  }

  async openGames(): Promise<bigint[]> {
    this.up();
    return [...this.open];
  }

  async game(id: bigint): Promise<Game | null> {
    this.up();
    const g = this.games.get(id);
    return g ? { ...g, playerCards: [...g.playerCards], dealerCards: [...g.dealerCards] } : null;
  }

  async commitment(k: bigint): Promise<string | null> {
    this.up();
    return this.commitments.get(k) ?? null;
  }

  async balance(): Promise<bigint> {
    return this.balance_;
  }

  // ---- what the dealer sends

  async send(c: TableCall, opts: SendOptions = {}): Promise<Sent> {
    const fate = this.fate(c);
    const id = (++this.txs).toString(16).padStart(64, "0");
    this.sends.push({ call: c, validForBlocks: opts.validForBlocks, fuelLimit: opts.fuelLimit, fate, id });
    const validUntil = this.height + (opts.validForBlocks ?? 600);
    switch (fate) {
      case "ok":
        return this.land(id, c, DEALER);
      case "dry":
        // It ran out of the fuel it reserved, unless it reserved the chain's maximum.
        if (opts.fuelLimit === undefined) return this.land(id, c, DEALER);
        this.height++;
        return { status: "aborted", id, height: this.height, code: undefined, trap: "out_of_fuel" };
      case "ghost":
        this.land(id, c, DEALER);
        return { status: "unknown", id, held: false, validUntil };
      case "held":
        this.waiting.set(id, c);
        return { status: "unknown", id, held: true, validUntil };
      case "lost":
        return { status: "unknown", id, held: false, validUntil };
      case "unsent":
        return { status: "unsent", reason: "the node refused it" };
      case "throw":
        throw new Error("the sender broke");
    }
  }

  async check(id: string): Promise<Sent> {
    this.up();
    return this.results.get(id) ?? { status: "unknown", id, held: this.waiting.has(id) };
  }

  /** A transaction the node held gets its block. */
  release(id: string): void {
    const c = this.waiting.get(id);
    if (!c) throw new Error("nothing held under that id");
    this.waiting.delete(id);
    this.land(id, c, DEALER);
  }

  /** The node drops what it held: it never runs. */
  drop(id: string): void {
    this.waiting.delete(id);
  }

  mine(blocks = 1): void {
    this.height += blocks;
  }

  /** Runs `c` from `by` in the next block and remembers what became of it. */
  private land(id: string, c: TableCall, by: string): Sent {
    this.beforeApply(c);
    this.height++;
    if (c.method === "timeout") {
      const g = this.games.get(u64(hexBytes(c.input, 8), 0));
      if (g && dealers(g.phase)) FakeTable.violations.push(`the dealer's timeout met game ${g.id} in the dealer's phase at block ${this.height} (deadline ${g.deadline})`);
    }
    let sent: Sent;
    try {
      sent = { status: "final", id, height: this.height, events: this.apply(c, by), fuel: 1000n };
    } catch (e) {
      if (typeof e !== "number") throw e;
      sent = { status: "aborted", id, height: this.height, code: e };
    }
    this.results.set(id, sent);
    return sent;
  }

  /** Any account's call (a player's, a stranger's): returns its events, or throws the abort code. */
  call(c: TableCall, by: string): TableEvent[] {
    this.height++;
    return this.apply(c, by);
  }

  private apply(c: TableCall, by: string): TableEvent[] {
    switch (c.method) {
      case "commit_batch": {
        const { head: first, items } = counted(c.input);
        if (by !== this.dealer) throw 43;
        if (first !== this.nextCommit) throw 61;
        items.forEach((commit, i) => this.commitments.set(first + BigInt(i), commit));
        this.nextCommit = first + BigInt(items.length);
        return [{ name: "commit", first, commitments: items }];
      }
      case "reveal": {
        const { head: id, items } = counted(c.input);
        const g = this.games.get(id);
        if (!g) throw 54;
        if (!dealers(g.phase)) throw 57;
        const hand = handOf(g);
        const first = g.dealt + 1;
        const cards: { link: string; card: number }[] = [];
        let link = g.link;
        let outcome: Outcome | null = null;
        for (const next of items) {
          if (!dealers(hand.phase)) throw 59;
          if (prevLink(next) !== link) throw 58;
          const cardNo = card(next, g.clientSeed, id, hand.dealt + 1);
          const step = hand.deal(cardNo);
          link = next;
          cards.push({ link: next, card: cardNo });
          if (step?.kind === "settled") outcome = step.outcome;
        }
        const deadline = hand.phase === Phase.Player ? BigInt(this.height) + this.limits_.idleBlocks : g.deadline;
        const now: Game = { ...g, link, deadline, phase: hand.phase, dealt: hand.dealt, playerCards: [...hand.player], dealerCards: [...hand.dealer], playerTotal: total(hand.player).total, dealerTotal: total(hand.dealer).total };
        const events: TableEvent[] = [{ name: "cards", game: id, player: g.player, first, cards, phase: hand.phase, playerTotal: now.playerTotal, dealerTotal: now.dealerTotal, deadline }];
        if (outcome === null) this.games.set(id, now);
        else events.push(this.settle(now, outcome, by));
        return events;
      }
      case "timeout": {
        const id = u64(hexBytes(c.input, 8), 0);
        const g = this.games.get(id);
        if (!g) throw 54;
        if (BigInt(this.height) <= g.deadline) throw 60;
        if (g.phase === Phase.Player) {
          const deadline = BigInt(this.height) + this.limits_.revealBlocks;
          this.games.set(id, { ...g, phase: Phase.Dealer, deadline });
          return [{ name: "stand", game: id, player: g.player, deadline, idle: true }];
        }
        // The dealer missed its deadline: the player is paid the most the hand could still pay.
        return [this.settle(g, g.phase === Phase.Deal || natural(g.playerCards) ? Outcome.ForfeitDeal : Outcome.Forfeit, by)];
      }
      default:
        throw new Error(`the fake table has no ${c.method}`);
    }
  }

  private settle(g: Game, outcome: Outcome, by: string): TableEvent {
    this.games.delete(g.id);
    this.open = this.open.filter((id) => id !== g.id);
    const paid = payout(g.stake, outcome);
    this.settled.push({ game: g.id, outcome, clientSeed: g.clientSeed, link: g.link, dealt: g.dealt, nPlayer: g.playerCards.length, stake: g.stake, payout: paid, commit: this.commitOf.get(g.id)!, by });
    return { name: "settled", game: g.id, player: g.player, outcome, stake: g.stake, payout: paid, toTable: false, dealt: g.dealt, nPlayer: g.playerCards.length, clientSeed: g.clientSeed, link: g.link, playerTotal: g.playerTotal, dealerTotal: g.dealerTotal };
  }

  // ---- what players do

  /** A bet: takes the next commitment. Returns the hand's id. */
  bet(player: string, clientSeed: string, stake = 1_000_000_000n): bigint {
    const id = this.nextGame;
    const commit = this.commitments.get(id);
    if (commit === undefined || id >= this.nextCommit) throw 53;
    this.height++;
    this.commitments.delete(id);
    this.commitOf.set(id, commit);
    this.nextGame = id + 1n;
    this.open.push(id);
    this.games.set(id, { id, player, stake, clientSeed, link: commit, deadline: BigInt(this.height) + this.limits_.revealBlocks, postId: 0n, phase: Phase.Deal, table: false, dealt: 0, playerCards: [], dealerCards: [], playerTotal: 0, dealerTotal: 0 });
    return id;
  }

  /** The player's move on an open hand. */
  move(id: bigint, hit: boolean): void {
    const g = this.games.get(id);
    if (!g) throw 54;
    if (g.phase !== Phase.Player) throw 56;
    this.height++;
    this.games.set(id, { ...g, phase: hit ? Phase.Hit : Phase.Dealer, deadline: BigInt(this.height) + this.limits_.revealBlocks });
  }

  /** The owner drops unused commitments. */
  skip(count: number): void {
    for (let i = 0; i < count && this.nextGame < this.nextCommit; i++) this.commitments.delete(this.nextGame++);
  }
}
