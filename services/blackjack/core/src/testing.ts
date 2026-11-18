/**
 * For tests that need a table without a chain: the table's records encoded as the program stores
 * them (the package itself only ever decodes), and a state tree that makes the Merkle proofs a
 * node would serve. Exported as `@blockula/blackjack/testing`; nothing at run time uses it.
 *
 * The tree is written from its definition (node/src/state/tree.rs: a compact binary sparse
 * Merkle tree over BLAKE3), not from the verifier, which is the SDK's `verifyProof`: a proof made
 * here and checked there is two implementations agreeing.
 */
import { EMPTY_HASH, keyHash, leafHash, nodeHash, valueHash, type Proof, type ProofResponse } from "@blockula/sdk";
import { GAME_BYTES, HOUSE_BYTES, LIMITS_BYTES, MAX_OPEN_GAMES, addressBytes, storageKey, type Game, type House, type Limits } from "./codec.js";
import { Phase, hexBytes, toHex } from "./rules.js";

// ---------------------------------------------------------------------------------- records

class Writer {
  private readonly parts: Uint8Array[] = [];
  u8(n: number): this {
    this.parts.push(Uint8Array.of(n));
    return this;
  }
  u64(n: bigint): this {
    const b = new Uint8Array(8);
    new DataView(b.buffer).setBigUint64(0, n, true);
    this.parts.push(b);
    return this;
  }
  raw(b: Uint8Array): this {
    this.parts.push(b);
    return this;
  }
  hex(bytes: number): string {
    const out = toHex(Uint8Array.from(this.parts.flatMap((p) => [...p])));
    if (out.length !== 2 * bytes) throw new Error(`encoded ${out.length / 2} bytes, the record is ${bytes}`);
    return out;
  }
}

export function encodeHouse(h: House): string {
  return new Writer().raw(addressBytes(h.owner)).raw(addressBytes(h.dealer)).u64(h.bankroll).u64(h.reserved).u64(h.stakes).u64(h.table).u64(h.nextCommit).u64(h.nextGame).u8(h.paused ? 1 : 0).u8(h.open).hex(HOUSE_BYTES);
}

/** The limits, in the record's order: the order of `DEFAULT_LIMITS`' keys (and of anything spread from it). */
export function encodeLimits(l: Limits): string {
  const w = new Writer();
  for (const v of Object.values(l)) w.u64(v as bigint);
  return w.hex(LIMITS_BYTES);
}

export function encodeGame(g: Game): string {
  const padded = (cards: readonly number[], n: number) => Uint8Array.from({ length: n }, (_, i) => cards[i] ?? 0);
  // One field holds the seed's hash until the seed is revealed, and the seed after.
  const seedField = g.phase === Phase.Seed ? g.seedHash : g.clientSeed;
  if (seedField === null) throw new Error("a hand's record always holds a seed or its hash");
  return new Writer()
    .raw(addressBytes(g.player))
    .u64(g.stake)
    .raw(hexBytes(seedField, 32))
    .raw(hexBytes(g.link, 32))
    .u64(g.deadline)
    .u64(g.postId)
    .u8(g.phase)
    .u8(g.table ? 1 : 0)
    .u8(g.dealt)
    .u8(g.playerCards.length)
    .u8(g.dealerCards.length)
    .raw(padded(g.playerCards, 20))
    .raw(padded(g.dealerCards, 12))
    .hex(GAME_BYTES);
}

export function encodeOpenList(ids: readonly bigint[]): string {
  const w = new Writer();
  for (let i = 0; i < MAX_OPEN_GAMES; i++) w.u64(ids[i] ?? 0n);
  return w.hex(8 * MAX_OPEN_GAMES);
}

/** A table's whole storage, `key hex → value hex`, as the program would hold it. */
export function tableStorage(t: { house: House; limits: Limits; open: readonly bigint[]; games: readonly Game[]; commitments: ReadonlyMap<bigint, string> }): Map<string, string> {
  const out = new Map<string, string>();
  out.set(storageKey.house, encodeHouse(t.house));
  out.set(storageKey.limits, encodeLimits(t.limits));
  // The program keeps no list while no hand is open.
  if (t.open.length) out.set(storageKey.open, encodeOpenList(t.open));
  for (const g of t.games) out.set(storageKey.game(g.id), encodeGame(g));
  for (const [k, c] of t.commitments) out.set(storageKey.commitment(k), c);
  return out;
}

// ---------------------------------------------------------------------------------- the state tree

const bit = (h: Uint8Array, i: number): number => (h[i >> 3] >> (7 - (i & 7))) & 1;
const order = (a: Uint8Array, b: Uint8Array): number => {
  for (let i = 0; i < 32; i++) if (a[i] !== b[i]) return a[i] - b[i];
  return 0;
};

/** A program's stored entry as the node keeps it: format 1 ‖ value (u32 length) ‖ payer 32 ‖ deposit 8 ‖ capacity 4. */
export function storedEntry(valueHex: string, payer: Uint8Array = new Uint8Array(32).fill(7), deposit = 20_000_000n): Uint8Array {
  const value = hexBytes(valueHex);
  const out = new Uint8Array(1 + 4 + value.length + 32 + 8 + 4);
  const view = new DataView(out.buffer);
  out[0] = 1;
  view.setUint32(1, value.length, true);
  out.set(value, 5);
  out.set(payer, 5 + value.length);
  view.setBigUint64(5 + value.length + 32, deposit, true);
  view.setUint32(5 + value.length + 40, value.length + 64, true);
  return out;
}

export interface StateRecord {
  namespace: number;
  key: Uint8Array;
  value: Uint8Array;
}

/** One state: records by `(namespace, key)`, its root, and a proof for any key (there or not). */
export class StateTree {
  private readonly leaves: [Uint8Array, Uint8Array][];
  private readonly values = new Map<string, Uint8Array>();
  private readonly hashes = new Map<string, Uint8Array>();
  readonly root: string;

  constructor(records: readonly StateRecord[]) {
    this.leaves = records.map((r): [Uint8Array, Uint8Array] => [keyHash(r.namespace, r.key), valueHash(r.value)]).sort((x, y) => order(x[0], y[0]));
    for (const r of records) this.values.set(`${r.namespace}:${toHex(r.key)}`, r.value);
    this.root = toHex(this.sub(0, this.leaves.length, 0));
  }

  /** The hash of the subtree holding leaves `[from, to)`, all of which share their first `depth` bits. */
  private sub(from: number, to: number, depth: number): Uint8Array {
    if (to === from) return EMPTY_HASH;
    if (to - from === 1) return leafHash(this.leaves[from][0], this.leaves[from][1]);
    const at = `${from}:${to}:${depth}`;
    let h = this.hashes.get(at);
    if (!h) {
      const split = this.split(from, to, depth);
      h = nodeHash(this.sub(from, split, depth + 1), this.sub(split, to, depth + 1));
      this.hashes.set(at, h);
    }
    return h;
  }

  private split(from: number, to: number, depth: number): number {
    let s = from;
    while (s < to && bit(this.leaves[s][0], depth) === 0) s++;
    return s;
  }

  value(namespace: number, key: Uint8Array): Uint8Array | null {
    return this.values.get(`${namespace}:${toHex(key)}`) ?? null;
  }

  /** The path from the root to where `(namespace, key)` is, or would be. */
  prove(namespace: number, key: Uint8Array): Proof {
    const k = keyHash(namespace, key);
    const siblings: string[] = [];
    let [from, to, depth] = [0, this.leaves.length, 0];
    while (to - from > 1) {
      const split = this.split(from, to, depth);
      if (bit(k, depth) === 0) {
        siblings.push(toHex(this.sub(split, to, depth + 1)));
        to = split;
      } else {
        siblings.push(toHex(this.sub(from, split, depth + 1)));
        from = split;
      }
      depth++;
    }
    return { siblings, leaf: to === from ? null : { key: toHex(this.leaves[from][0]), value: toHex(this.leaves[from][1]) } };
  }

  /** What `/api/proof/{namespace}/{key}` answers for this state after block `height`. */
  answer(namespace: number, key: Uint8Array, height: number): ProofResponse {
    const value = this.value(namespace, key);
    return { namespace, key: toHex(key), value: value === null ? null : toHex(value), proof: this.prove(namespace, key), root: this.root, height };
  }
}

/** A program's storage (`key hex → value hex`) as state records, with some unrelated records around them. */
export function storageRecords(program: string, entries: ReadonlyMap<string, string>, filler = 24): StateRecord[] {
  const at = addressBytes(program);
  const out: StateRecord[] = [...entries].map(([key, value]) => ({ namespace: 69, key: Uint8Array.from([...at, ...hexBytes(key)]), value: storedEntry(value) }));
  // Other programs' entries and other namespaces: the tree is never this table's alone.
  for (let i = 0; i < filler; i++) out.push({ namespace: i % 3 === 0 ? 1 : 69, key: Uint8Array.from([...new Uint8Array(32).fill(200 + (i % 7)), i, 0x67]), value: storedEntry(toHex(new Uint8Array(8).fill(i))) });
  return out;
}
