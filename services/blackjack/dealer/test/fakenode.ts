// Nodes for the dealer's privacy tests (test/privacy.ts): each serves a `FakeTable`'s storage the
// way a real node does (records under a state root, with Merkle proofs made by the core tests'
// prover, and the storage route), and each can lie. A monitor stands where the node stands and
// holds everything the dealer hands it against the true table.
import { Phase, chainFromTop, hexBytes, owedLinks, storageKey, toHex, type ApiGet, type Game, type TableCall } from "@blockula/blackjack";
import type { ProofNode, WitnessNode } from "@blockula/blackjack/proven";
import type { ProofResponse } from "@blockula/sdk";
import { StateTree, storageRecords, tableStorage } from "@blockula/blackjack/testing";
import type { SecretStore } from "../src/store.js";
import type { FakeTable } from "./fake.js";

export type Storage = Map<string, string>;

/** The table's true storage now, as the program would hold it. */
export function trueStorage(t: FakeTable): Storage {
  return tableStorage({
    house: { owner: t.owner, dealer: t.dealer, bankroll: 0n, reserved: 0n, stakes: 0n, table: 0n, nextCommit: t.nextCommit, nextGame: t.nextGame, paused: false, open: t.open.length },
    limits: t.limits_,
    open: t.open,
    games: [...t.games.values()],
    commitments: t.commitments,
  });
}

interface Snapshot {
  height: number;
  storage: Storage;
  print: string;
  tree: StateTree;
}

/** The chain: the true table's state after each block anyone looked at. Honest nodes share it. */
export class Ledger {
  private readonly at = new Map<number, Snapshot>();

  constructor(
    readonly program: string,
    readonly table: FakeTable,
  ) {}

  /** The state now (after the table's newest block). */
  now(): Snapshot {
    const height = this.table.height;
    const storage = trueStorage(this.table);
    const print = [...storage].map(([k, v]) => `${k}=${v}`).join(";");
    const known = this.at.get(height);
    if (known && known.print === print) return known;
    const snap = { height, storage, print, tree: new StateTree(storageRecords(this.program, storage)) };
    this.at.set(height, snap);
    return snap;
  }

  /** The state after block `height`, if anyone looked then. */
  after(height: number): Snapshot | undefined {
    this.now();
    return this.at.get(height);
  }

  /** Heights with a known state, oldest first. */
  heights(): number[] {
    this.now();
    return [...this.at.keys()].sort((a, b) => a - b);
  }
}

/** What a lying node does to the true storage before answering from it. */
export type Lie = (storage: Storage, truth: FakeTable) => void;

/**
 * A node. Honest unless told otherwise:
 * - `lie`: its whole state is the truth bent this way, self-consistently (its proofs verify against its own root);
 * - `lieFor`: it bends only the answers for these keys (same block, another root), and is honest about the rest;
 * - `bend`: it rewrites an answer on its way out (a changed value under a real proof, a moved height);
 * - `pinned`: it serves the genuine state of an old block;
 * - `behind`, `down`: as a witness, it lags or doesn't answer.
 */
export class FakeNode implements ProofNode, WitnessNode {
  readonly rpc: string;
  lie?: Lie;
  lieFor?: (key: string) => boolean;
  bend?: (r: ProofResponse) => ProofResponse;
  pinned?: number;
  behind = 0;
  down = false;
  /** Everything this node was asked, as text: what a curious operator has. */
  readonly asked: string[] = [];

  constructor(
    name: string,
    readonly ledger: Ledger,
  ) {
    this.rpc = `https://${name}.example`;
  }

  /** As honest as the chain. */
  honest(): this {
    this.lie = this.lieFor = this.bend = this.pinned = undefined;
    return this;
  }

  private snapshot(): Snapshot {
    return (this.pinned !== undefined ? this.ledger.after(this.pinned) : undefined) ?? this.ledger.now();
  }

  /** The last lied state made, so that one look's many reads don't each rebuild its tree. */
  private made?: { snap: Snapshot; lie: Lie; print: string; out: { storage: Storage; tree: StateTree } };

  private lied(snap: Snapshot): { storage: Storage; tree: StateTree } {
    if (!this.lie) return snap;
    const storage = new Map(snap.storage);
    this.lie(storage, this.ledger.table);
    const print = [...storage].map(([k, v]) => `${k}=${v}`).join(";");
    if (this.made && this.made.snap === snap && this.made.lie === this.lie && this.made.print === print) return this.made.out;
    const out = { storage, tree: new StateTree(storageRecords(this.ledger.program, storage)) };
    this.made = { snap, lie: this.lie, print, out };
    return out;
  }

  /** The storage this node would have a reader believe, and its height. */
  view(): { storage: Storage; height: number } {
    const snap = this.snapshot();
    return { storage: this.lied(snap).storage, height: snap.height };
  }

  async proof(namespace: number, key: Uint8Array): Promise<ProofResponse> {
    this.asked.push(`proof/${namespace}/${toHex(key)}`);
    if (this.down) throw new Error("connection refused");
    const snap = this.snapshot();
    const storageKeyHex = toHex(key.subarray(32));
    const whole = this.lieFor === undefined || this.lieFor(storageKeyHex);
    const answer = (whole ? this.lied(snap) : snap).tree.answer(namespace, key, snap.height);
    return this.bend ? this.bend(answer) : answer;
  }

  async status(): Promise<{ height: number }> {
    this.asked.push("status");
    if (this.down) throw new Error("connection refused");
    return { height: this.snapshot().height - this.behind };
  }

  async block(height: number): Promise<{ state_root: string }> {
    this.asked.push(`block/${height}`);
    if (this.down) throw new Error("connection refused");
    const snap = height <= this.ledger.table.height - this.behind ? this.ledger.after(height) : undefined;
    if (!snap) throw Object.assign(new Error("no such block"), { status: 404 });
    return { state_root: this.lied(snap).tree.root };
  }

  /** This node's storage route (`GET /api/program/{address}/storage?limit=1&after=`): what today's reader asks. */
  storageRoute(): ApiGet {
    return async (path) => {
      this.asked.push(path);
      if (this.down) throw new Error("connection refused");
      const { storage, height } = this.view();
      const after = Buffer.from(new URL(`http://node${path}`).searchParams.get("after") ?? "", "hex");
      const next = [...storage.keys()].sort((a, b) => Buffer.compare(Buffer.from(a, "hex"), Buffer.from(b, "hex"))).find((k) => Buffer.compare(Buffer.from(k, "hex"), after) > 0);
      return { entries: next === undefined ? [] : [{ key: next, value: storage.get(next) }], height };
    };
  }
}

/** A store in memory that never forgets a secret it held, so a settled hand's chain can still be checked. */
export class MemoryStore implements SecretStore {
  private readonly tops = new Map<bigint, string>();
  /** Every secret ever held, by game. */
  readonly ever = new Map<bigint, string>();

  get size(): number {
    return this.tops.size;
  }
  has(k: bigint): boolean {
    return this.tops.has(k);
  }
  top(k: bigint): string | undefined {
    return this.tops.get(k);
  }
  indexes(): bigint[] {
    return [...this.tops.keys()].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  }
  put(secrets: ReadonlyMap<bigint, string>): void {
    for (const [k, top] of secrets) {
      if (this.tops.has(k) && this.tops.get(k) !== top) throw new Error("another secret for an index the store holds");
      this.tops.set(k, top);
      this.ever.set(k, top);
    }
  }
  remove(indexes: readonly bigint[]): void {
    for (const k of indexes) this.tops.delete(k);
  }
}

export interface Leak {
  game: bigint;
  /** The link's place in its chain: it deals card `index` (33 would be the secret itself). */
  index: number;
  /** Where it was found. */
  where: string;
  link: string;
}

/**
 * The check the tests exist for. Every time the dealer hands anything to a node (a call's input,
 * a request), every 32 bytes of it are looked up among every link of every chain the dealer ever
 * committed to. A link is allowed out only if the true table holds it already (it was revealed)
 * or the true hand is owed it right now. Anything else is a leak: a card nobody is owed yet.
 */
export class LeakMonitor {
  readonly leaks: Leak[] = [];
  /** Links seen leaving that were allowed to. */
  allowed = 0;
  /** Bytes looked through. */
  scanned = 0;

  constructor(
    private readonly table: FakeTable,
    private readonly store: MemoryStore,
  ) {}

  /** Every link (and the secret) of every chain, by its hex: which game's, and where in the chain. */
  private links(): Map<string, { game: bigint; index: number }> {
    const out = new Map<string, { game: bigint; index: number }>();
    for (const [game, top] of this.store.ever) chainFromTop(top).forEach((link, index) => index > 0 && out.set(link, { game, index }));
    return out;
  }

  /** The highest link of `game`'s chain that the true table has out, or owes, right now. */
  private owedThrough(game: bigint): number {
    const g: Game | undefined = this.table.games.get(game);
    if (g) {
      // A hand waiting for its seed is owed nothing, whatever anyone says about it.
      if (g.phase === Phase.Seed) return 0;
      return g.dealt + owedLinks(g, chainFromTop(this.store.ever.get(game)!)).length;
    }
    // Settled: what was dealt is public for good. Never bet on: nothing is.
    return this.table.settled.find((h) => h.game === game)?.dealt ?? 0;
  }

  /** Looks through bytes (hex) on their way to a node. */
  see(hex: string, where: string): void {
    const bytes = hexBytes(hex);
    this.scanned += bytes.length;
    const links = this.links();
    for (let at = 0; at + 32 <= bytes.length; at++) {
      const link = toHex(bytes.subarray(at, at + 32));
      const found = links.get(link);
      if (!found) continue;
      if (found.index <= this.owedThrough(found.game)) this.allowed++;
      else this.leaks.push({ ...found, where, link });
    }
  }

  /** A call the dealer asked a node to submit. */
  call(c: TableCall): void {
    this.see(c.input, c.method);
  }

  /** Everything a node was asked (its request log), from `from` on: paths hold only hex and words. */
  requests(node: FakeNode, from = 0): void {
    for (const r of node.asked.slice(from)) for (const run of r.match(/[0-9a-f]{64,}/g) ?? []) this.see(run.length % 2 ? run.slice(1) : run, `request to ${node.rpc}`);
  }
}

// ---------------------------------------------------------------------------------- the lies

/** Rewrites hand `id`'s stored record: `phase` is its byte at 120; the seed field is at 40. */
function rewrite(storage: Storage, id: bigint, change: (record: Uint8Array) => void): boolean {
  const key = storageKey.game(id);
  const value = storage.get(key);
  if (value === undefined) return false;
  const record = hexBytes(value);
  change(record);
  storage.set(key, toHex(record));
  return true;
}
const PHASE_AT = 120;
const SEED_AT = 40;

/** "The player stood" (or "hit"), of every hand whose player is still choosing. */
export const lieAboutMoves =
  (to: typeof Phase.Dealer | typeof Phase.Hit): Lie =>
  (storage, truth) => {
    for (const g of truth.games.values()) if (g.phase === Phase.Player) rewrite(storage, g.id, (r) => (r[PHASE_AT] = to));
  };

/** "The seed is revealed", of every hand still waiting for it, with a seed of the liar's choosing. */
export const lieAboutSeeds =
  (seed: string): Lie =>
  (storage, truth) => {
    for (const g of truth.games.values()) {
      if (g.phase !== Phase.Seed) continue;
      rewrite(storage, g.id, (r) => {
        r.set(hexBytes(seed, 32), SEED_AT);
        r[PHASE_AT] = Phase.Deal;
      });
    }
  };

/**
 * "Someone bet": a hand on the next unused commitment, which nobody has taken, its seed revealed.
 * The house record and the open list are made to agree.
 */
export const phantomHand =
  (player: string, seed: string): Lie =>
  (storage, truth) => {
    const k = truth.nextGame;
    const commit = truth.commitments.get(k);
    if (commit === undefined) return;
    const phantom: Game = { id: k, player, stake: 1_000_000_000n, clientSeed: seed, seedHash: null, link: commit, deadline: BigInt(truth.height) + 20n, postId: 0n, phase: Phase.Deal, table: false, dealt: 0, playerCards: [], dealerCards: [], playerTotal: 0, dealerTotal: 0 };
    const lied = tableStorage({
      house: { owner: truth.owner, dealer: truth.dealer, bankroll: 0n, reserved: 0n, stakes: 0n, table: 0n, nextCommit: truth.nextCommit, nextGame: k + 1n, paused: false, open: truth.open.length + 1 },
      limits: truth.limits_,
      open: [...truth.open, k],
      games: [...truth.games.values(), phantom],
      commitments: new Map([...truth.commitments].filter(([i]) => i !== k)),
    });
    storage.clear();
    for (const [key, value] of lied) storage.set(key, value);
  };
