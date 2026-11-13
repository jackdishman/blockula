// A whole world for one scripted thread: a fake clock, the in-memory table, a store, a sink that
// keeps replies, a renderer that draws nothing (it records the snapshot and returns the real alt
// text) and the stand-in link check. The fixture generator and the fixture test both run threads
// through it, so a thread's steps mean the same in each.
import { altText, type TableSnapshot } from "@blockula/blackjack";
import { Bot, FakeClock, FakeTable, MemorySink, MemoryStore, type BotConfig, type Handled, type Post, type Store, type Written } from "../src/index.js";

export interface PlayerSetup {
  /** The account's name in this world (its address is derived from it). */
  name: string;
  /** The X handle the account links, or `null` for none. */
  handle: string | null;
  /** `verified`: the proof post is by the handle. `wrong-author`: by someone else. `no-post`: the post isn't there. */
  link: "verified" | "wrong-author" | "no-post" | "none";
  /** Drops deposited before the thread starts. */
  deposit: string;
  /** The grant to the relayer, or `null`. */
  grant: { maxBet: string; dailyCap: string; expiresIn: string } | null;
}

export interface Setup {
  /** Blocks the dealer takes to reveal; `null`: never. */
  dealerDelay: number | null;
  players: PlayerSetup[];
  /** The house's bankroll, drops (1,000 FANG unless given). */
  bankroll?: string;
  config?: Partial<BotConfig>;
}

export type ChainOp =
  | { op: "pause"; on: boolean }
  | { op: "deposit"; name: string; amount: string }
  | { op: "grant"; name: string; maxBet: string; dailyCap: string; expiresIn: string }
  | { op: "revoke"; name: string }
  | { op: "dealer"; delay: number | null }
  /** The player ran out of time: the clock passes the deadline and the dealer stands them. */
  | { op: "idle"; name: string }
  /** The clock passes the dealer's deadline on the player's hand. */
  | { op: "past-deadline"; name: string };

export type Step = { t: "post"; post: Post } | { t: "wait"; ms: number } | { t: "tick" } | { t: "chain"; chain: ChainOp };

export interface Thread {
  name: string;
  kind: string;
  seed: string;
  setup: Setup;
  steps: Step[];
}

/** A reply as the expected file keeps it: everything but the picture's bytes. */
export interface ExpectedReply {
  id: string;
  text: string;
  alt: string | null;
  meta: Written["meta"];
}

export const expectedOf = (r: Written): ExpectedReply => ({ id: r.id, text: r.text, alt: r.image ? r.image.alt : null, meta: r.meta });

const big = (s: string) => BigInt(s);

export class World {
  readonly clock = new FakeClock();
  readonly table: FakeTable;
  store: Store;
  sink: MemorySink = new MemorySink();
  /** Every snapshot the bot asked a picture of. */
  readonly snapshots: TableSnapshot[] = [];
  bot: Bot;
  /** Each post's result, in order. */
  readonly results: [string, string][] = [];

  constructor(
    readonly seed: string,
    readonly setup: Setup,
    store: Store = new MemoryStore(),
  ) {
    this.store = store;
    this.table = new FakeTable({ seed, clock: this.clock, dealerDelay: setup.dealerDelay, bankroll: setup.bankroll === undefined ? undefined : big(setup.bankroll) });
    setup.players.forEach((p, i) => {
      const account = this.table.account(p.name);
      if (big(p.deposit) > 0n) this.table.deposit(account, big(p.deposit));
      if (p.grant) this.table.grant(account, big(p.grant.maxBet), big(p.grant.dailyCap), big(p.grant.expiresIn));
      if (p.handle !== null && p.link !== "none") {
        const proof = String(1_700_000_000_000_000_000n + BigInt(i));
        if (p.link === "no-post") this.table.setLink(account, p.handle, proof);
        else this.table.linkWithProof(account, p.handle, proof, p.link === "verified" ? p.handle : "SomeoneElse");
      }
    });
    this.bot = this.newBot();
  }

  /** The process starts again: a new bot over the same table, with the store as it is on disk. */
  restart(store: Store): Bot {
    this.table.revive();
    this.store = store;
    this.bot = this.newBot();
    return this.bot;
  }

  /** A bot over this world's table and store. */
  newBot(): Bot {
    return new Bot({
      table: this.table,
      store: this.store,
      verifyLink: this.table.verifier(),
      render: (snapshot) => {
        this.snapshots.push(snapshot);
        return { png: null, alt: altText(snapshot) };
      },
      sink: this.sink,
      clock: this.clock,
      config: this.setup.config,
    });
  }

  async apply(step: Step, bot: Bot = this.bot): Promise<Handled | Written[] | null> {
    switch (step.t) {
      case "post": {
        const handled = await bot.handle(step.post);
        this.results.push([step.post.id, handled.result]);
        return handled;
      }
      case "wait":
        this.clock.advance(step.ms);
        return null;
      case "tick":
        return bot.tick();
      case "chain":
        this.chain(step.chain);
        return null;
    }
  }

  private chain(c: ChainOp): void {
    const t = this.table;
    switch (c.op) {
      case "pause":
        return t.pause(c.on);
      case "deposit":
        return t.deposit(t.account(c.name), big(c.amount));
      case "grant":
        return t.grant(t.account(c.name), big(c.maxBet), big(c.dailyCap), big(c.expiresIn));
      case "revoke":
        return t.revoke(t.account(c.name));
      case "dealer":
        t.dealerDelay = c.delay;
        return;
      case "idle":
      case "past-deadline": {
        const open = t.players.get(t.account(c.name))?.openGame;
        if (open === null || open === undefined) throw new Error(`${c.name} has no open hand`);
        this.clock.advance(t.msPastDeadline(open));
        if (c.op === "idle") t.idleTimeout(open);
        return;
      }
    }
  }
}

/** Runs a thread from its steps alone. */
export async function runThread(thread: Thread): Promise<World> {
  const world = new World(thread.seed, thread.setup);
  for (const step of thread.steps) await world.apply(step);
  return world;
}
