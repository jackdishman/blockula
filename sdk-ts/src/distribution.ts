// Community distribution Merkle trees (docs/GOVERNANCE.md "Merkle tree format"): a port of
// node/src/governance.rs `distribution_leaf`, `merkle_parent`, `build_distribution` and
// `root_from_proof`. Claims are checked on chain against the proposal's root, so these must match
// the node byte for byte; test/unit.ts checks the shared vectors (distribution 7).
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, hexToBytes } from "@noble/hashes/utils.js";

const LEAF_DOMAIN = new TextEncoder().encode("BLOCKULA_DISTRIBUTION_LEAF_V1");
const NODE_DOMAIN = new TextEncoder().encode("BLOCKULA_DISTRIBUTION_NODE_V1");
/** Most hashes a claim's proof may carry (the node's `MAX_PROOF`). */
export const MAX_DISTRIBUTION_PROOF = 64;

const U64_MAX = (1n << 64n) - 1n;

function u64le(v: bigint | number, what: string): Uint8Array {
  const n = BigInt(v);
  if (n < 0n || n > U64_MAX) throw new Error(`${what} must fit in a u64`);
  const out = new Uint8Array(8);
  new DataView(out.buffer).setBigUint64(0, n, true);
  return out;
}

function concat(...parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

function compare(a: Uint8Array, b: Uint8Array): number {
  for (let i = 0; i < Math.min(a.length, b.length); i++) if (a[i] !== b[i]) return a[i] - b[i];
  return a.length - b.length;
}

/**
 * The leaf of `address`'s `amount` (drops) in distribution `id`:
 * BLAKE3("BLOCKULA_DISTRIBUTION_LEAF_V1" ‖ id u64 LE ‖ len(address) u16 LE ‖ address ‖ amount u64 LE).
 * `address` is the bech32m string exactly as the claimant's tx spells it.
 */
export function distributionLeaf(id: bigint | number, address: string, amount: bigint): Uint8Array {
  const addr = new TextEncoder().encode(address);
  if (addr.length > 0xffff) throw new Error("address too long");
  const len = new Uint8Array([addr.length & 0xff, addr.length >> 8]);
  return blake3(concat(LEAF_DOMAIN, u64le(id, "distribution id"), len, addr, u64le(amount, "amount")));
}

/** Sorted-pair inner node: BLAKE3("BLOCKULA_DISTRIBUTION_NODE_V1" ‖ min(a, b) ‖ max(a, b)). */
export function distributionNode(a: Uint8Array, b: Uint8Array): Uint8Array {
  const [lo, hi] = compare(a, b) <= 0 ? [a, b] : [b, a];
  return blake3(concat(NODE_DOMAIN, lo, hi));
}

/**
 * Root and per-entry proofs (hex sibling hashes from the leaf up), entries in input order, in
 * the reference shape: leaves sorted, paired left to right level by level, an odd last node
 * carried up unchanged. Addresses must be distinct and amounts non-zero.
 */
export function buildDistribution(id: bigint | number, entries: { address: string; amount: bigint }[]): { root: string; proofs: string[][] } {
  if (entries.length === 0) throw new Error("a distribution needs at least one entry");
  const seen = new Set<string>();
  for (const e of entries) {
    if (seen.has(e.address)) throw new Error(`${e.address} is listed twice`);
    seen.add(e.address);
    if (e.amount <= 0n) throw new Error(`${e.address} gets 0`);
  }
  const leaves = entries.map((e, i) => ({ hash: distributionLeaf(id, e.address, e.amount), i }));
  // Like the node's sort of (hash, index) pairs: by hash, then input order.
  leaves.sort((x, y) => compare(x.hash, y.hash) || x.i - y.i);
  let level = leaves.map((l) => l.hash);
  const pos = new Array<number>(entries.length);
  leaves.forEach((l, p) => (pos[l.i] = p));
  const proofs: Uint8Array[][] = entries.map(() => []);
  while (level.length > 1) {
    for (let i = 0; i < pos.length; i++) {
      const sibling = pos[i] ^ 1;
      if (sibling < level.length) proofs[i].push(level[sibling]);
      pos[i] = Math.floor(pos[i] / 2);
    }
    const next: Uint8Array[] = [];
    for (let k = 0; k < level.length; k += 2) next.push(k + 1 < level.length ? distributionNode(level[k], level[k + 1]) : level[k]);
    level = next;
  }
  return { root: bytesToHex(level[0]), proofs: proofs.map((p) => p.map(bytesToHex)) };
}

/** The root a leaf and its proof (hex sibling hashes from the leaf up) lead to (hex). */
export function distributionRoot(id: bigint | number, address: string, amount: bigint, proof: string[]): string {
  if (proof.length > MAX_DISTRIBUTION_PROOF) throw new Error(`a proof has at most ${MAX_DISTRIBUTION_PROOF} hashes`);
  let h = distributionLeaf(id, address, amount);
  for (const s of proof) {
    const sibling = hexToBytes(s);
    if (sibling.length !== 32) throw new Error("proof hashes are 32 bytes of hex");
    h = distributionNode(h, sibling);
  }
  return bytesToHex(h);
}

/** Whether `proof` leads from `address`'s leaf for `amount` (drops) in distribution `id` to `root` (hex). */
export function verifyDistributionProof(root: string, id: bigint | number, address: string, amount: bigint, proof: string[]): boolean {
  try {
    return distributionRoot(id, address, amount, proof) === root.toLowerCase();
  } catch {
    return false;
  }
}
