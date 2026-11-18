// A state tree for tests: it makes the proofs a node would serve. Written from the tree's
// definition (node/src/state/tree.rs: a compact binary sparse Merkle tree over BLAKE3), not from
// the verifier, which is the SDK's `verifyProof`: a proof made here and checked there is two
// implementations agreeing.
import { EMPTY_HASH, keyHash, leafHash, nodeHash, valueHash, type Proof, type ProofResponse } from "@blockula/sdk";
import { addressBytes, hexBytes, toHex } from "../src/index.js";

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

/** One state: records by `(namespace, key)`, its root, and a proof for any key (there or not). */
export class StateTree {
  private readonly leaves: [Uint8Array, Uint8Array][];
  private readonly values = new Map<string, Uint8Array>();
  private readonly hashes = new Map<string, Uint8Array>();
  readonly root: string;

  constructor(records: readonly { namespace: number; key: Uint8Array; value: Uint8Array }[]) {
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
export function storageRecords(program: string, entries: ReadonlyMap<string, string>, filler = 24): { namespace: number; key: Uint8Array; value: Uint8Array }[] {
  const at = addressBytes(program);
  const out = [...entries].map(([key, value]) => ({ namespace: 69, key: Uint8Array.from([...at, ...hexBytes(key)]), value: storedEntry(value) }));
  // Other programs' entries and other namespaces: the tree is never this table's alone.
  for (let i = 0; i < filler; i++) out.push({ namespace: i % 3 === 0 ? 1 : 69, key: Uint8Array.from([...new Uint8Array(32).fill(200 + (i % 7)), i, 0x67]), value: storedEntry(toHex(new Uint8Array(8).fill(i))) });
  return out;
}
