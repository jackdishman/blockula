/**
 * The dealer: what it does each time it looks at the table.
 *
 * The chain is the only state it trusts. Each tick reads the house, the open hands and each
 * hand's record, and from those alone decides what to send:
 *
 * - nothing for a hand whose bet is placed and whose seed isn't revealed yet (a bet is two
 *   transactions: the seed's hash, then the seed). No card can be known before the seed is, and
 *   the program refuses a link until then; the dealer never asks its store for such a hand;
 * - a `reveal` with exactly the links a hand is owed (`owedLinks`), oldest deadline first, once
 *   the seed is on the chain;
 * - a `timeout` for a hand whose player let a deadline pass: the seed's (the hand ends, the stake
 *   is the house's and the seat is free: outcome 8) or a move's (the program stands them and the
 *   next tick deals the rest). Never for a hand in the dealer's own phase: there a timeout pays
 *   the player;
 * - a `commit_batch` when fewer than `lowWater` commitments are left, each chain's secret in the
 *   store and its backup before the commitment leaves this machine.
 *
 * So a restart needs nothing but the chain and the store. Every call is safe to repeat: the
 * program takes a link only as its chain's next (58; one the hand already holds is passed over, so
 * a reveal someone got ahead of still deals the rest), a batch only where the last one ended (61)
 * and a timeout only past the deadline (60), and secrets are kept by game index, so a batch sent
 * again commits the same chains. A transaction whose fate isn't known is followed by its id
 * until it is final, failed, or can no longer run; only one the node has never heard of is
 * planned again.
 *
 * The table reads, the sender and the store are interfaces: `chain.ts` has the real ones, the
 * tests have fakes.
 */
import { ABORTS, MAX_COMMIT_BATCH, MAX_REVEAL_LINKS, Outcome, Phase, call, chainFromTop, dealers, fang, owedLinks, type Game, type House, type Limits, type TableCall, type TableEvent } from "@blockula/blackjack";
import { randomBytes } from "node:crypto";
import { errorText, type LogFields, type Logger } from "./log.js";
import type { SecretStore } from "./store.js";

/** What the dealer reads: `Table` from `@blockula/blackjack` is one. */
export interface TableReader {
  house(): Promise<House & { height: number }>;
  limits(): Promise<Limits>;
  openGames(): Promise<bigint[]>;
  game(id: bigint): Promise<Game | null>;
  commitment(k: bigint): Promise<string | null>;
}

/** What became of a call. */
export type Sent =
  /** It ran in a block. `events` are the table's own, from its receipt. */
  | { status: "final"; id: string; height: number; events: TableEvent[]; fuel?: bigint }
  /** It ran in a block and failed (its fee was charged): the program's abort `code`, or the VM's `trap` (out of fuel, say). */
  | { status: "aborted"; id: string; height?: number; code: number | undefined; trap?: string }
  /** It was signed and may have been submitted: `held` when the node has it waiting. It can run until `validUntil`. */
  | { status: "unknown"; id: string; held: boolean; validUntil?: number }
  /** Nothing was submitted (nothing was signed, or the node refused it). */
  | { status: "unsent"; reason: string };

export interface SendOptions {
  /** Blocks the transaction stays valid. */
  validForBlocks?: number;
  /** The most fuel it may use (and pays for). Without it, the chain's maximum for one transaction. */
  fuelLimit?: number;
}

/**
 * The fuel each call reserves. A call pays for what it reserves, and a block carries only so
 * much reserved fuel (at the chain's maximum for every call, three calls fill a block), so the
 * dealer reserves what its calls use and a wide margin: on a local chain a reveal used at most
 * 130,000 + 57,000 a link + 100,000 to settle, a batch 132,000 + 7,200 a commitment, a timeout
 * 118,000. A call that still runs out is sent again with the chain's maximum.
 */
export const FUEL = {
  reveal: (links: number): number => 400_000 + 100_000 * links,
  commit: (commitments: number): number => 300_000 + 15_000 * commitments,
  timeout: 400_000,
} as const;

/** What the dealer signs with. One transaction at a time, in nonce order. */
export interface TxSender {
  /** The dealer's account. */
  readonly address: string;
  /** Signs and submits one call and waits for it. Never throws: what it couldn't learn is `unknown`. */
  send(c: TableCall, opts?: SendOptions): Promise<Sent>;
  /** Where a transaction sent earlier stands now. Throws when the node can't be asked. */
  check(id: string): Promise<Sent>;
  /** The account's balance, in drops. */
  balance(): Promise<bigint>;
}

export interface DealerOptions {
  lowWater: number;
  target: number;
  batch: number;
  dryRun: boolean;
  /** Secrets of settled games kept before pruning. */
  keepSettled: number;
  /** Warn under this balance (drops). */
  lowBalance: bigint;
  /** Blocks a commit or reveal stays valid. */
  txValidBlocks: number;
  /** A new chain's top: 32 random bytes, hex. Tests replace it. */
  random?: () => string;
  /** Checks after which a transaction the node has never heard of counts as lost. */
  lostAfterChecks?: number;
  /** Settled secrets that pile up before one prune rewrites the store. */
  pruneBatch?: number;
  /** Reserve the chain's maximum fuel for every call instead of `FUEL`'s figures. */
  fullFuel?: boolean;
}

/** A run of game indexes, both ends included. */
export interface Range {
  from: bigint;
  to: bigint;
}

/** What `inspect` found: the table against the store. */
export interface Health {
  height: number;
  /** Whether the sender's account is the table's dealer. */
  isDealer: boolean;
  nextGame: bigint;
  nextCommit: bigint;
  /** Commitments no bet has taken. */
  unused: number;
  /** Of those, the ones whose secret the store holds and that hash to the chain's commitment. */
  servable: number;
  /** Unused commitments whose secret the store doesn't hold. */
  missing: Range[];
  /** Unused commitments the store's secret doesn't hash to. */
  mismatched: bigint[];
  /** Secrets stored for indexes not committed yet (a batch that never landed: they are used next). */
  uncommitted: number;
  stored: number;
  /** `waitsForSeed`: the bet is placed and its seed isn't revealed; the dealer owes nothing yet. */
  open: { id: bigint; phase: Phase; deadline: bigint; dealers: boolean; waitsForSeed: boolean; hasSecret: boolean }[];
}

const OUTCOME_NAMES: Record<Outcome, string> = { 1: "blackjack", 2: "win", 3: "push", 4: "lose", 5: "bust", 6: "forfeit", 7: "forfeit_deal", 8: "no_seed" };
const PHASE_NAMES: Record<Phase, string> = { 0: "seed", 1: "deal", 2: "player", 3: "hit", 4: "dealer", 5: "done" };
/** Aborts that say the table moved on since it was read: someone else's call got there first. */
const MOVED: Record<string, readonly number[]> = { reveal: [54, 57, 58, 59], timeout: [54, 60], commit: [61] };

function ranges(indexes: readonly bigint[]): Range[] {
  const out: Range[] = [];
  for (const k of indexes) {
    const last = out[out.length - 1];
    if (last && last.to + 1n === k) last.to = k;
    else out.push({ from: k, to: k });
  }
  return out;
}

const rangeText = (rs: readonly Range[]): string => rs.map((r) => (r.from === r.to ? `${r.from}` : `${r.from}–${r.to}`)).join(", ");

/** What a transaction's events say, for a log line. Never a link. */
function eventFields(events: readonly TableEvent[]): LogFields {
  const out: Record<string, string | number | number[]> = {};
  for (const e of events) {
    if (e.name === "cards") {
      out.cards = e.cards.map((c) => c.card);
      out.phase = PHASE_NAMES[e.phase];
      out.player_total = e.playerTotal;
      out.dealer_total = e.dealerTotal;
    } else if (e.name === "settled") {
      out.outcome = OUTCOME_NAMES[e.outcome];
      out.stake = fang(e.stake);
      out.payout = fang(e.payout);
    } else if (e.name === "seed") {
      // That it came, never what it is.
      out.seed = "revealed";
    } else if (e.name === "stand") {
      out.stood = e.idle ? "idle" : "player";
    } else if (e.name === "commit") {
      out.first = e.first.toString();
      out.count = e.commitments.length;
    }
  }
  return out;
}

/** The table against the store: what `status` prints and what the daemon checks when it starts. */
export async function inspect(reader: TableReader, store: SecretStore, address: string): Promise<Health> {
  const house = await reader.house();
  const ids = await reader.openGames();
  const games = (await Promise.all(ids.map((id) => reader.game(id)))).filter((g): g is Game => g !== null);
  const missing: bigint[] = [];
  const mismatched: bigint[] = [];
  const unused: bigint[] = [];
  for (let k = house.nextGame; k < house.nextCommit; k++) unused.push(k);
  // A few reads at a time: a table holds a few hundred unused commitments at most.
  for (let i = 0; i < unused.length; i += 16) {
    await Promise.all(
      unused.slice(i, i + 16).map(async (k) => {
        const top = store.top(k);
        if (top === undefined) return void missing.push(k);
        const c = await reader.commitment(k);
        if (c !== null && c !== chainFromTop(top)[0]) mismatched.push(k);
      }),
    );
  }
  const sort = (a: bigint, b: bigint) => (a < b ? -1 : a > b ? 1 : 0);
  missing.sort(sort);
  mismatched.sort(sort);
  return {
    height: house.height,
    isDealer: house.dealer === address,
    nextGame: house.nextGame,
    nextCommit: house.nextCommit,
    unused: unused.length,
    servable: unused.length - missing.length - mismatched.length,
    missing: ranges(missing),
    mismatched,
    uncommitted: store.indexes().filter((k) => k >= house.nextCommit).length,
    stored: store.size,
    open: games.map((g) => {
      const top = store.top(g.id);
      return { id: g.id, phase: g.phase, deadline: g.deadline, dealers: dealers(g.phase), waitsForSeed: g.phase === Phase.Seed, hasSecret: top !== undefined && chainFromTop(top)[g.dealt] === g.link };
    }),
  };
}

/** A call that left (or may have left) and isn't settled yet. */
interface Pending {
  action: string;
  fields: LogFields;
  /** `send` hasn't returned yet. */
  sending: boolean;
  id?: string;
  validUntil?: number;
  /** Checks in a row the node didn't know the transaction. */
  unknown: number;
}

export interface Tick {
  height: number;
  open: number;
  unused: number;
  /** The calls this tick started (or, in a dry run, would have), by their keys. */
  started: string[];
}

export class Dealer {
  private readonly random: () => string;
  private readonly lostAfter: number;
  private readonly pruneBatch: number;
  /** Calls not settled yet, by what they do: one per hand's state, one commit at a time. */
  private readonly busy = new Map<string, Pending>();
  private readonly inflight = new Set<Promise<void>>();
  /** A call that failed waits a while (in ticks, doubling) before the same call is tried again. */
  private readonly backoff = new Map<string, { fails: number; until: number }>();
  private readonly chains = new Map<bigint, string[]>();
  /** Games whose stored secret isn't their chain's. */
  private readonly bad = new Set<bigint>();
  /** Things said once, not every tick. */
  private readonly said = new Map<string, number>();
  /** Calls that ran out of fuel at `FUEL`'s figure: sent again with the chain's maximum. */
  private readonly thirsty = new Set<string>();
  private ticks = 0;
  private toppingUp = false;

  constructor(
    private readonly reader: TableReader,
    private readonly sender: TxSender,
    private readonly store: SecretStore,
    private readonly log: Logger,
    private readonly opts: DealerOptions,
  ) {
    if (opts.batch < 1 || opts.batch > MAX_COMMIT_BATCH) throw new Error(`a batch is 1 to ${MAX_COMMIT_BATCH} commitments`);
    if (opts.target < opts.lowWater) throw new Error("target can't be under lowWater");
    this.random = opts.random ?? (() => randomBytes(32).toString("hex"));
    this.lostAfter = opts.lostAfterChecks ?? 3;
    this.pruneBatch = opts.pruneBatch ?? 32;
  }

  /** Checks the store against the table once, and says what it found. */
  async start(): Promise<Health> {
    const h = await inspect(this.reader, this.store, this.sender.address);
    for (const k of h.mismatched) this.bad.add(k);
    this.log.info("started", {
      address: this.sender.address,
      is_dealer: h.isDealer,
      dry_run: this.opts.dryRun,
      height: h.height,
      unused: h.unused,
      servable: h.servable,
      open: h.open.length,
      stored: h.stored,
      uncommitted: h.uncommitted,
    });
    if (h.mismatched.length) this.log.error("secrets_mismatch", { games: rangeText(ranges(h.mismatched)), advice: "the store holds secrets that don't hash to these commitments (a store from another table or an old backup?). Hands that take them can't be dealt: the owner can pause the table and drop unused commitments with skip_commits" });
    await this.checkBalance();
    return h;
  }

  /** Says something once, and again only after `every` ticks. */
  private once(key: string, every: number, say: () => void): void {
    const last = this.said.get(key);
    if (last !== undefined && this.ticks - last < every) return;
    this.said.set(key, this.ticks);
    say();
  }

  private usable(k: bigint): boolean {
    return this.store.has(k) && !this.bad.has(k);
  }

  /** Hand `g`'s chain, when the store's secret is the one its commitment came from. */
  private chainFor(g: Game): string[] | undefined {
    if (this.bad.has(g.id)) return undefined;
    let chain = this.chains.get(g.id);
    if (!chain) {
      const top = this.store.top(g.id);
      if (top === undefined) return undefined;
      chain = chainFromTop(top);
      this.chains.set(g.id, chain);
    }
    // The newest link on the chain must be this chain's: otherwise the secret is another's.
    if (chain[g.dealt] !== g.link) {
      this.bad.add(g.id);
      this.chains.delete(g.id);
      return undefined;
    }
    return chain;
  }

  /**
   * One look at the table: reads it, follows up what's pending, and starts what's due. Returns
   * once the calls are on their way, not once they're final (`drain` waits for that). Throws
   * when the table can't be read: the caller tries again.
   */
  async tick(): Promise<Tick> {
    this.ticks++;
    const house = await this.reader.house();
    const [ids, limits] = await Promise.all([this.reader.openGames(), this.reader.limits()]);
    const games = (await Promise.all(ids.map((id) => this.reader.game(id)))).filter((g): g is Game => g !== null);
    // The hand closest to its deadline first: calls are signed in this order.
    games.sort((a, b) => (a.deadline < b.deadline ? -1 : a.deadline > b.deadline ? 1 : a.id < b.id ? -1 : 1));
    const height = house.height;
    const started: string[] = [];

    await this.followUp(height);
    for (const g of games) this.planHand(g, height, limits, started);
    this.planCommit(house, height, started);
    await this.prune(house, ids);
    if (this.ticks % 30 === 0) await this.checkBalance();

    const open = new Set(ids);
    for (const k of this.chains.keys()) if (!open.has(k)) this.chains.delete(k);
    if (this.backoff.size > 512) for (const [k, b] of this.backoff) if (b.until < this.ticks) this.backoff.delete(k);
    if (this.thirsty.size > 256) this.thirsty.clear();
    if (this.said.size > 2048) for (const [k, at] of this.said) if (this.ticks - at > 600) this.said.delete(k);
    return { height, open: games.length, unused: Number(house.nextCommit - house.nextGame), started };
  }

  /** Waits for every call on its way to come back. */
  async drain(): Promise<void> {
    while (this.inflight.size) await Promise.allSettled([...this.inflight]);
  }

  private planHand(g: Game, height: number, limits: Limits, started: string[]): void {
    if (g.phase === Phase.Seed) {
      // The bet is placed; its seed isn't revealed. Nothing is owed, and nothing of the chain is
      // touched. Once the block a call would land in is past the seed's deadline, the hand is
      // the house's: the timeout ends it and frees its seat. Short validity, as for an idle
      // player: a copy that arrives late must not meet the hand dealt and past another deadline.
      if (BigInt(height) < g.deadline) return;
      const validForBlocks = Math.max(2, Math.min(this.opts.txValidBlocks, Number(limits.revealBlocks / 2n)));
      this.begin(`timeout:seed:${g.id}`, "timeout", call.timeout(g.id), { validForBlocks, fuelLimit: FUEL.timeout }, { game: g.id, turn: "seed", deadline: g.deadline, read_at: height }, started);
      return;
    }
    if (dealers(g.phase)) {
      if (g.clientSeed === null) {
        // Never the case on a real table (the program sets the seed with the phase); never deal blind.
        this.once(`noseed:${g.id}`, 120, () => this.log.error("hand_without_seed", { game: g.id, height, phase: PHASE_NAMES[g.phase] }));
        return;
      }
      const chain = this.chainFor(g);
      if (!chain) {
        this.once(`lost:${g.id}`, 120, () =>
          this.log.error("hand_without_secret", {
            game: g.id,
            height,
            phase: PHASE_NAMES[g.phase],
            deadline: g.deadline,
            advice: "the store has no secret for this hand's chain, so it can't be dealt: at its deadline anyone may call timeout and the player is paid. If the store was lost, restore it from the backup, or pause the table and have the owner drop the unused commitments with skip_commits",
          }),
        );
        return;
      }
      if (BigInt(height) > g.deadline) this.once(`late:${g.id}:${g.dealt}`, 120, () => this.log.warn("past_deadline", { game: g.id, height, deadline: g.deadline }));
      const links = owedLinks(g, chain).slice(0, MAX_REVEAL_LINKS);
      this.begin(`reveal:${g.id}:${g.phase}:${g.dealt}`, "reveal", call.reveal(g.id, links), { validForBlocks: this.opts.txValidBlocks, fuelLimit: FUEL.reveal(links.length) }, { game: g.id, turn: PHASE_NAMES[g.phase], links: links.length, deadline: g.deadline, read_at: height }, started);
    } else if (g.phase === Phase.Player && BigInt(height) >= g.deadline) {
      // The player's turn, and the block this lands in is past their deadline: they stand.
      // Its validity is short, well inside `reveal_blocks`, so a copy that arrives late can't
      // find the hand in the dealer's phase with a deadline already behind it.
      const validForBlocks = Math.max(2, Math.min(this.opts.txValidBlocks, Number(limits.revealBlocks / 2n)));
      this.begin(`timeout:${g.id}:${g.dealt}`, "timeout", call.timeout(g.id), { validForBlocks, fuelLimit: FUEL.timeout }, { game: g.id, turn: "player", deadline: g.deadline, read_at: height }, started);
    }
  }

  private planCommit(house: House, height: number, started: string[]): void {
    // What's committed and untaken, and which of it this store can deal.
    const lost: bigint[] = [];
    for (let k = house.nextGame; k < house.nextCommit; k++) if (!this.usable(k)) lost.push(k);
    if (lost.length) {
      const rs = ranges(lost);
      this.once(`lost-commits:${rangeText(rs)}`, 300, () =>
        this.log.error("lost_secrets", {
          height,
          games: rangeText(rs),
          count: lost.length,
          next_game: house.nextGame,
          advice: `the table holds ${lost.length} commitment(s) this store has no secret for (games ${rangeText(rs)}). A bet that takes one can't be dealt and pays the player at its deadline. Restore the store from its backup; or the owner pauses the table and calls skip_commits (it drops unused commitments from game ${house.nextGame} on, at most ${MAX_COMMIT_BATCH} a call) until game ${rs[rs.length - 1].to + 1n} is next. The dealer keeps dealing every hand it has the secret for`,
        }),
      );
    }
    if (house.dealer !== this.sender.address) {
      this.once("not-dealer", 300, () => this.log.error("not_the_dealer", { height, address: this.sender.address, advice: "this key isn't the table's dealer, so it can't commit: the owner names it with set_dealer. Hands it holds secrets for are still dealt" }));
      return;
    }
    const servable = Number(house.nextCommit - house.nextGame) - lost.length;
    // Under the low-water mark a top-up starts, and runs (a batch a look) until the target.
    if (servable < this.opts.lowWater) this.toppingUp = true;
    if (servable >= this.opts.target) this.toppingUp = false;
    if (!this.toppingUp) return;
    if ([...this.busy.keys()].some((k) => k.startsWith("commit:"))) return;
    const n = Math.min(this.opts.batch, this.opts.target - servable);
    if (n < 1) return;
    if (servable + n >= this.opts.target) this.toppingUp = false;
    const first = house.nextCommit;
    const key = `commit:${first}`;
    if (this.waiting(key)) return;

    // The secrets are kept by index: a batch planned again (after a restart, a lost
    // transaction, a refused one) commits the chains it would have the first time.
    const fresh = new Map<bigint, string>();
    const tops: string[] = [];
    let reused = 0;
    for (let i = 0; i < n; i++) {
      const k = first + BigInt(i);
      let top = this.store.top(k);
      if (top === undefined) {
        if (this.opts.dryRun) continue;
        top = this.random();
        fresh.set(k, top);
      } else reused++;
      tops.push(top);
    }
    const fields = { first, count: n, reused, read_at: height };
    if (this.opts.dryRun) {
      // A dry run makes no secrets: nothing it writes could be mistaken for a committed chain.
      this.once(`dry:${key}:${n}`, 60, () => this.log.info("would_commit", fields));
      started.push(key);
      return;
    }
    try {
      // On disk, and in the backup, before the commitments leave.
      this.store.put(fresh);
    } catch (e) {
      this.bump(key);
      this.log.error("store_write_failed", { first, count: n, error: errorText(e), advice: "nothing was committed: the secrets must be stored (and backed up) first. Check the store's and the backup's disks" });
      return;
    }
    this.begin(key, "commit", call.commitBatch(first, tops.map((top) => chainFromTop(top)[0])), { validForBlocks: this.opts.txValidBlocks, fuelLimit: FUEL.commit(n) }, fields, started);
  }

  private waiting(key: string): boolean {
    if (this.busy.has(key)) return true;
    const b = this.backoff.get(key);
    return b !== undefined && b.until > this.ticks;
  }

  private bump(key: string): void {
    const fails = (this.backoff.get(key)?.fails ?? 0) + 1;
    this.backoff.set(key, { fails, until: this.ticks + Math.min(2 ** fails, 64) });
  }

  /** Starts one call unless the same one is on its way or waiting out a failure. */
  private begin(key: string, action: string, c: TableCall, given: SendOptions, fields: LogFields, started: string[]): void {
    if (this.waiting(key)) return;
    const opts = this.opts.fullFuel || this.thirsty.has(key) ? { ...given, fuelLimit: undefined } : given;
    started.push(key);
    if (this.opts.dryRun) {
      this.once(`dry:${key}`, 60, () => this.log.info(`would_${action}`, fields));
      return;
    }
    const pending: Pending = { action, fields, sending: true, unknown: 0 };
    this.busy.set(key, pending);
    const run = (async () => {
      let sent: Sent;
      try {
        sent = await this.sender.send(c, opts);
      } catch (e) {
        // A sender shouldn't throw; one that does may have submitted. Nothing to follow by id,
        // and every call is safe to plan again from the chain.
        sent = { status: "unsent", reason: errorText(e) };
      }
      this.settle(key, pending, sent);
    })();
    this.inflight.add(run);
    void run.finally(() => this.inflight.delete(run));
  }

  /** Takes in what became of a call. */
  private settle(key: string, p: Pending, sent: Sent): void {
    switch (sent.status) {
      case "final":
        this.busy.delete(key);
        this.backoff.delete(key);
        this.thirsty.delete(key);
        this.log.info(p.action, { ...p.fields, tx: sent.id, height: sent.height, fuel: sent.fuel, ...eventFields(sent.events) });
        return;
      case "aborted": {
        this.busy.delete(key);
        if (sent.code === undefined && sent.trap !== undefined && /fuel/.test(sent.trap) && !this.thirsty.has(key)) {
          // It ran out of the fuel it reserved: the same call again, at once, with the most a transaction may have.
          this.thirsty.add(key);
          this.log.warn(`${p.action}_out_of_fuel`, { ...p.fields, tx: sent.id, height: sent.height, advice: "sent again with the chain's maximum fuel" });
          return;
        }
        this.bump(key);
        const moved = sent.code !== undefined && (MOVED[p.action] ?? []).includes(sent.code);
        // The table moved on between the read and the call: the next tick reads it again.
        const fields = { ...p.fields, tx: sent.id, height: sent.height, abort: sent.code, trap: sent.trap, why: (sent.code !== undefined && ABORTS[sent.code]) || "the transaction failed", moved };
        if (moved) this.log.info(`${p.action}_refused`, fields);
        else this.log.warn(`${p.action}_refused`, fields);
        return;
      }
      case "unknown":
        p.sending = false;
        p.id = sent.id;
        p.validUntil = sent.validUntil;
        p.unknown = 0;
        this.log.warn(`${p.action}_unconfirmed`, { ...p.fields, tx: sent.id, held: sent.held, valid_until: sent.validUntil });
        return;
      case "unsent":
        this.busy.delete(key);
        this.bump(key);
        this.log.warn(`${p.action}_not_sent`, { ...p.fields, reason: sent.reason });
        return;
    }
  }

  /** Asks after every transaction whose fate wasn't known, by its id. */
  private async followUp(height: number): Promise<void> {
    for (const [key, p] of [...this.busy]) {
      if (p.sending || p.id === undefined) continue;
      let now: Sent;
      try {
        now = await this.sender.check(p.id);
      } catch {
        continue; // The node can't be asked right now: ask again next tick.
      }
      if (now.status === "final" || now.status === "aborted") {
        this.settle(key, p, now);
        continue;
      }
      const expired = p.validUntil !== undefined && height > p.validUntil;
      const held = now.status === "unknown" && now.held;
      p.unknown = held ? 0 : p.unknown + 1;
      if (expired || (!held && p.unknown >= this.lostAfter)) {
        // It can't run any more, or it never reached the node: the next look at the table
        // plans whatever is still due.
        this.busy.delete(key);
        this.log.warn(`${p.action}_lost`, { ...p.fields, tx: p.id, expired, height });
      }
    }
  }

  /** Forgets the secrets of hands the chain shows settled, keeping the newest `keepSettled`. */
  private async prune(house: House, open: readonly bigint[]): Promise<void> {
    if (this.opts.dryRun) return;
    // Nothing at or after the oldest open hand, and nothing a bet could still take.
    let floor = house.nextGame;
    for (const id of open) if (id < floor) floor = id;
    const cutoff = floor - BigInt(this.opts.keepSettled);
    const old = this.store.indexes().filter((k) => k < cutoff);
    if (old.length < this.pruneBatch) return;
    const gone: bigint[] = [];
    for (const k of old.slice(0, 64)) {
      // Settled means the hand's record is gone, read from the chain, not assumed.
      if ((await this.reader.game(k)) !== null) break;
      gone.push(k);
    }
    if (!gone.length) return;
    try {
      this.store.remove(gone);
      for (const k of gone) this.bad.delete(k);
      this.log.info("pruned", { count: gone.length, below: cutoff, stored: this.store.size });
    } catch (e) {
      this.log.error("store_write_failed", { error: errorText(e), advice: "settled secrets couldn't be pruned: check the store's and the backup's disks" });
    }
  }

  private async checkBalance(): Promise<void> {
    let balance: bigint;
    try {
      balance = await this.sender.balance();
    } catch {
      return;
    }
    if (balance < this.opts.lowBalance) {
      this.once("low-balance", 300, () => this.log.warn("low_balance", { address: this.sender.address, balance: fang(balance), under: fang(this.opts.lowBalance), advice: "the dealer pays a fee for every reveal and bonds a deposit for every commitment: send it FANG, or it will stop dealing" }));
    }
  }
}
