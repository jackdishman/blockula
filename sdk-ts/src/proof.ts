// Light-client checks of state proofs. A port of node/src/state/tree.rs (the tree's definition is
// a consensus rule): a compact binary sparse Merkle tree over BLAKE3, where a record
// `(namespace, key) → value` is the leaf `k = BLAKE3(namespace ‖ key)`, `v = BLAKE3(value)`.
// Shared vectors: sdk-ts/test/proof-vectors.json (written and checked by the node's
// `state::proof_vectors` tests).
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, hexToBytes } from "@noble/hashes/utils.js";

/** Record namespaces (node/src/state/mod.rs `ns`): the first byte of every record's tree key. */
export const NAMESPACES = {
  META: 0,
  ACCOUNTS: 1,
  NAMES: 2,
  VALIDATORS: 3,
  ACTIVE_SETS: 4,
  HUNTS: 5,
  GITHUB: 6,
  GITHUB_BY_ACCOUNT: 7,
  FAMILIARS: 8,
  REPOS: 16,
  SOULS: 17,
  VOUCHES: 18,
  SYBILS: 19,
  CLAIMS: 20,
  CLAIM_BY_PR: 21,
  HANDLES: 22,
  JURORS: 23,
  SEANCES: 24,
  GRANTS: 25,
  // 32–47 belong to the protocol v1 modules (staking 32–36, governance 37–42, econ 43, beacon
  // 44–46, upgrade 47); see node/src/state/mod.rs for what each holds.
  RESERVED_NAMES: 43,
  FEATURES: 47,
} as const;

/** Hash of an empty subtree. */
export const EMPTY_HASH = new Uint8Array(32);

const utf8 = new TextEncoder();

/** A record key as bytes: strings are UTF-8 (addresses, names), as the node's `RecordKey`. */
export function recordKey(key: string | Uint8Array): Uint8Array {
  return typeof key === "string" ? utf8.encode(key) : key;
}

/** Tree key of a record: BLAKE3(namespace ‖ key). */
export function keyHash(namespace: number, key: Uint8Array): Uint8Array {
  const b = new Uint8Array(1 + key.length);
  b[0] = namespace;
  b.set(key, 1);
  return blake3(b);
}

/** Hash of a record's encoded value. */
export function valueHash(value: Uint8Array): Uint8Array {
  return blake3(value);
}

const cat3 = (tag: number, a: Uint8Array, b: Uint8Array) => {
  const out = new Uint8Array(65);
  out[0] = tag;
  out.set(a, 1);
  out.set(b, 33);
  return out;
};

/** A lone leaf: BLAKE3(0x00 ‖ k ‖ v). */
export function leafHash(key: Uint8Array, value: Uint8Array): Uint8Array {
  return blake3(cat3(0, key, value));
}

/** An internal node: BLAKE3(0x01 ‖ left ‖ right). */
export function nodeHash(left: Uint8Array, right: Uint8Array): Uint8Array {
  return blake3(cat3(1, left, right));
}

/** Bit `i` of a key hash, most significant first. */
const bit = (h: Uint8Array, i: number) => (h[i >> 3] >> (7 - (i & 7))) & 1;

/** Whether two key hashes agree on their first `bits` bits. */
function samePrefix(a: Uint8Array, b: Uint8Array, bits: number): boolean {
  const whole = bits >> 3;
  const rest = bits & 7;
  for (let i = 0; i < whole; i++) if (a[i] !== b[i]) return false;
  return rest === 0 || ((a[whole] ^ b[whole]) >> (8 - rest)) === 0;
}

const equal = (a: Uint8Array, b: Uint8Array) => a.length === b.length && a.every((x, i) => x === b[i]);

const compare = (a: Uint8Array, b: Uint8Array) => {
  for (let i = 0; i < 32; i++) if (a[i] !== b[i]) return a[i] - b[i];
  return 0;
};

/**
 * The root of a set of leaves `[key hash, value hash]`, straight from the definition (tree.rs
 * `root_of`). Leaves are sorted here; duplicate keys are an error.
 */
export function rootOf(leaves: [Uint8Array, Uint8Array][]): Uint8Array {
  const sorted = [...leaves].sort((x, y) => compare(x[0], y[0]));
  for (let i = 1; i < sorted.length; i++) if (compare(sorted[i - 1][0], sorted[i][0]) === 0) throw new Error("duplicate leaf key");
  const sub = (from: number, to: number, depth: number): Uint8Array => {
    if (to === from) return EMPTY_HASH;
    if (to - from === 1) return leafHash(sorted[from][0], sorted[from][1]);
    let split = from;
    while (split < to && bit(sorted[split][0], depth) === 0) split++;
    return nodeHash(sub(from, split, depth + 1), sub(split, to, depth + 1));
  };
  return sub(0, sorted.length, 0);
}

/** The leaf a proof's path ends at (hex), as the node serves it. */
export interface ProofLeaf {
  key: string;
  value: string;
}

/** A Merkle proof (node/src/state/tree.rs `Proof`, JSON): sibling hashes from the root down, and where the path ends. */
export interface Proof {
  siblings: string[];
  /** `null`: an empty subtree (the key is absent). Otherwise a lone leaf: the key's own, or another key with the same prefix (absent). */
  leaf: ProofLeaf | null;
}

const hex32 = (s: unknown): Uint8Array | null => (typeof s === "string" && /^[0-9a-f]{64}$/.test(s) ? hexToBytes(s) : null);

/**
 * Checks that `(namespace, key)` holds `value` (`null`: is absent) under `root` (tree.rs
 * `verify`). Never throws: malformed input is `false`.
 */
export function verifyProof(root: Uint8Array | string, namespace: number, key: string | Uint8Array, value: Uint8Array | null, proof: Proof): boolean {
  const r = typeof root === "string" ? hex32(root) : root;
  if (!r || r.length !== 32 || !Number.isInteger(namespace) || namespace < 0 || namespace > 255) return false;
  if (!proof || !Array.isArray(proof.siblings) || proof.siblings.length > 256) return false;
  const siblings = proof.siblings.map(hex32);
  if (siblings.some((s) => s === null)) return false;
  const k = keyHash(namespace, recordKey(key));
  const depth = siblings.length;
  let h: Uint8Array;
  if (proof.leaf) {
    const lk = hex32(proof.leaf.key);
    const lv = hex32(proof.leaf.value);
    if (!lk || !lv) return false;
    if (value !== null && equal(lk, k) && equal(lv, valueHash(value))) h = leafHash(lk, lv);
    else if (value === null && !equal(lk, k) && samePrefix(lk, k, depth)) h = leafHash(lk, lv);
    else return false;
  } else if (value === null) {
    h = EMPTY_HASH;
  } else {
    return false;
  }
  for (let i = depth - 1; i >= 0; i--) {
    const s = siblings[i] as Uint8Array;
    h = bit(k, i) === 0 ? nodeHash(h, s) : nodeHash(s, h);
  }
  return equal(h, r);
}

/** `/api/proof/{namespace}/{key}`: a record's value and its proof against the state root after block `height`. */
export interface ProofResponse {
  namespace: number;
  /** The record key, hex. */
  key: string;
  /** The encoded value, hex; `null` if the record doesn't exist. */
  value: string | null;
  proof: Proof;
  root: string;
  height: number;
  block_hash?: string;
}

/** A record checked against a state root. */
export interface ProvenRecord {
  /** The record's encoded value, or `null` if it provably doesn't exist. */
  value: Uint8Array | null;
  /** The state root it was proven against, and the block it's the root after. */
  root: string;
  height: number;
}

/**
 * Checks a `/api/proof` response for the record you asked for: the node must answer for exactly
 * `(namespace, key)` and its proof must verify against the root it reports. Throws otherwise.
 * This proves the record is in the state with that root; whether the root is the chain's is a
 * separate question (see `Client.provenRecord`'s `witnesses`).
 */
export function checkProofResponse(r: ProofResponse, namespace: number, key: string | Uint8Array): ProvenRecord {
  const want = bytesToHex(recordKey(key));
  if (!r || r.namespace !== namespace || typeof r.key !== "string" || r.key.toLowerCase() !== want) throw new Error("the node answered for a different record than the one asked for");
  if (!Number.isSafeInteger(r.height) || r.height < 0) throw new Error("the proof has no valid height");
  let value: Uint8Array | null = null;
  if (r.value !== null && r.value !== undefined) {
    if (typeof r.value !== "string" || !/^([0-9a-f]{2})*$/.test(r.value)) throw new Error("the proof's value isn't hex");
    value = hexToBytes(r.value);
  }
  if (!verifyProof(r.root, namespace, key, value, r.proof)) throw new Error(`the node's proof for this record doesn't verify against state root ${String(r.root).slice(0, 16)}…`);
  return { value, root: r.root, height: r.height };
}

// ---- Decoding records (node/src/state/codec.rs: borsh, little-endian, u32 lengths) ----

/** A strict little-endian reader over a record's bytes. */
class Reader {
  at = 0;
  private readonly view: DataView;
  constructor(readonly b: Uint8Array) {
    this.view = new DataView(b.buffer, b.byteOffset, b.byteLength);
  }
  private need(n: number) {
    if (this.at + n > this.b.length) throw new Error("record ends early");
  }
  u8() {
    this.need(1);
    return this.b[this.at++];
  }
  u32() {
    this.need(4);
    const v = this.view.getUint32(this.at, true);
    this.at += 4;
    return v;
  }
  u64() {
    this.need(8);
    const v = this.view.getBigUint64(this.at, true);
    this.at += 8;
    return v;
  }
  str() {
    const n = this.u32();
    this.need(n);
    const s = new TextDecoder("utf-8", { fatal: true }).decode(this.b.subarray(this.at, this.at + n));
    this.at += n;
    return s;
  }
}

/** One key on an account, as proven. */
export interface ProvenKey {
  key_id: string;
  label: string;
  role: { type: "owner" } | { type: "session"; limit: string; period_blocks: number; expires_at: number };
  added_at: number;
}

/** The parts of an account record (chain.rs `Account`) a wallet needs: balance, nonce and keys. Later fields (locks) aren't read. */
export interface AccountRecord {
  balance: bigint;
  nonce: number;
  /** Authorized keys. Empty means only the key the address was derived from. */
  keys: ProvenKey[];
}

export function decodeAccount(value: Uint8Array): AccountRecord {
  const r = new Reader(value);
  const balance = r.u64();
  const nonce = Number(r.u64());
  const n = r.u32();
  if (n > 64) throw new Error("account record lists too many keys");
  const keys: ProvenKey[] = [];
  for (let i = 0; i < n; i++) {
    const key_id = r.str();
    const label = r.str();
    const tag = r.u8();
    let role: ProvenKey["role"];
    if (tag === 0) role = { type: "owner" };
    else if (tag === 1) role = { type: "session", limit: r.u64().toString(), period_blocks: Number(r.u64()), expires_at: Number(r.u64()) };
    else throw new Error(`bad key role tag ${tag}`);
    const added_at = Number(r.u64());
    r.u64(); // spent in the current window
    r.u64(); // window start
    keys.push({ key_id, label, role, added_at });
  }
  return { balance, nonce, keys };
}

/**
 * A name record (chain.rs `NameRecord`): the bare name, its owner, when it was registered, and
 * the last height it resolves at (DEC-11; after it, a grace period in which only the owner can
 * renew it, then anyone can register it). Later fields, if a version adds them, aren't read.
 */
export interface NameRecord {
  name: string;
  owner: string;
  registered_at: number;
  expires_at: number;
  /** The account the owner offered the name to (`TransferName`); it's still the owner's until they accept it. */
  offered_to?: string;
  /**
   * The grace (`name_grace_blocks`) it was registered or last renewed with. Its grace runs until
   * `expires_at` plus the longer of this and the chain's `name_grace_blocks` now (`/api/name`'s
   * `grace_until`), so lowering the parameter doesn't shorten it.
   */
  grace_blocks?: number;
}

export function decodeName(value: Uint8Array): NameRecord {
  const r = new Reader(value);
  const name = r.str();
  const owner = r.str();
  const registered_at = Number(r.u64());
  // Saturated, so a never-expiring record (u64::MAX) compares as "later than any height".
  const sat = (v: bigint) => (v > BigInt(Number.MAX_SAFE_INTEGER) ? Number.MAX_SAFE_INTEGER : Number(v));
  const expires_at = sat(r.u64());
  const rec: NameRecord = { name, owner, registered_at, expires_at };
  // State schema 6 appended `offered_to` (an Option: 0, or 1 and the address), schema 7
  // `grace_blocks`; older records end earlier.
  if (r.at === value.length) return rec;
  const tag = r.u8();
  if (tag > 1) throw new Error(`name record: bad offered_to tag ${tag}`);
  if (tag === 1) rec.offered_to = r.str();
  if (r.at === value.length) return rec;
  rec.grace_blocks = sat(r.u64());
  return rec;
}
