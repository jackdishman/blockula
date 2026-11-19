/**
 * A table read through Merkle proofs: every record comes with its proof against a state root,
 * and that root is confirmed by other nodes of the same chain (witnesses) before anything is
 * made of the record. docs/proposals/blackjack-card-privacy.md has the why: the dealer reveals
 * the links a hand's record says it is owed, so a node that could lie about a record could have
 * unrevealed cards sent to it.
 *
 * `ProvenTable` is a `Table` with one thing replaced: where a stored value comes from. The
 * storage route is never used (there is none to fall back to), so no read is one node's word.
 *
 * What a read establishes: the record is in the state after block `height`, under a root that
 * `quorum` witnesses report for that block and none reports otherwise, and that block is at most
 * `maxLagBlocks` behind the newest head the witnesses report. What it doesn't: that the
 * witnesses are independent of the node. Choosing them is the operator's part.
 *
 * Program storage in the state tree (node/src/programs/state.rs): namespace 69, key = the
 * program's 32 address bytes ‖ the storage key, value = format byte 1 ‖ Borsh `Entry`
 * (value: u32 length + bytes, payer: 32, deposit: u64, capacity: u32).
 */
import { NAMESPACES, checkProofResponse, type ProofResponse } from "@blockula/sdk";
import { addressBytes } from "./codec.js";
import { hexBytes, toHex } from "./rules.js";
import { Table } from "./table.js";

/** The node the records are read from: the SDK's `Client` is one. */
export interface ProofNode {
  readonly rpc?: string;
  /** `GET /api/proof/{namespace}/{key as hex}`, as the node answered it. */
  proof(namespace: number, key: Uint8Array): Promise<ProofResponse>;
}

/** Another node of the same chain: the SDK's `Client` (pinned to the chain) is one. */
export interface WitnessNode {
  readonly rpc?: string;
  /** Its head: `height`. A pinned `Client` also refuses a node of another chain here. */
  status(): Promise<{ height?: unknown }>;
  /** Block `height`'s header: `state_root`. Rejects with `status: 404` while it hasn't that block. */
  block(height: number): Promise<{ state_root?: unknown }>;
}

/**
 * Why a proven read was refused.
 * - `bad_proof`: the node's answer isn't a valid proof of the record asked for.
 * - `disagreement`: a witness reports another state root for that block. Someone is lying or broken.
 * - `unconfirmed`: too few witnesses confirmed the root in time (down, or behind).
 * - `stale`: the proof is real and too old.
 */
export type ProofFailure = "bad_proof" | "disagreement" | "unconfirmed" | "stale";

export class ProofError extends Error {
  constructor(
    readonly kind: ProofFailure,
    message: string,
  ) {
    super(message);
    this.name = "ProofError";
  }
}

export interface ProvenOptions {
  /** How many witnesses must confirm a state root. Default: all of them. */
  quorum?: number;
  /** How far behind the witnesses' newest head a proof's block may be. Default 10. */
  maxLagBlocks?: number;
  /** How long a witness that hasn't reached a block (or doesn't answer) is waited for. Default 5,000 ms. */
  waitMs?: number;
  /** The pause between asks of a witness that isn't there yet. Default 200 ms. */
  retryMs?: number;
  /** How long one request to a witness may take. Default 4,000 ms. */
  attemptMs?: number;
  /** How long the witnesses' heads are remembered. Default 1,000 ms. */
  headTtlMs?: number;
}

export const DEFAULT_PROOF_MAX_LAG_BLOCKS = 10;
/** Confirmed roots remembered, by height. */
const ROOTS_KEPT = 128;

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
const short = (hex: unknown): string => `${String(hex).slice(0, 12)}…`;
const nameOf = (n: { rpc?: string }, i: number): string => n.rpc ?? `witness ${i + 1}`;

/** `p`, or a rejection after `ms`. */
function within<T>(p: Promise<T>, ms: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`no answer in ${ms} ms`)), ms);
    p.then(
      (v) => (clearTimeout(t), resolve(v)),
      (e) => (clearTimeout(t), reject(e)),
    );
  });
}

/** The tree key of a program's storage entry: its 32 address bytes ‖ the storage key (hex). */
export function storageRecordKey(program: string, key: string): Uint8Array {
  const k = hexBytes(key);
  if (k.length === 0) throw new Error("an empty storage key");
  const out = new Uint8Array(32 + k.length);
  out.set(addressBytes(program), 0);
  out.set(k, 32);
  return out;
}

/** The program's bytes inside a stored entry (format 1). Throws on anything else: a format this code doesn't know is never guessed at. */
export function storedValue(record: Uint8Array): Uint8Array {
  if (record.length < 1 + 4 + 32 + 8 + 4) throw new ProofError("bad_proof", "the stored entry is too short to be one");
  if (record[0] !== 1) throw new ProofError("bad_proof", `the stored entry has format ${record[0]}; this reader knows format 1`);
  const n = new DataView(record.buffer, record.byteOffset, record.byteLength).getUint32(1, true);
  if (record.length !== 1 + 4 + n + 32 + 8 + 4) throw new ProofError("bad_proof", "the stored entry's length isn't what its value's length says");
  return record.subarray(5, 5 + n);
}

/** State roots confirmed by witnesses, and how fresh a proof is against their heads. */
export class RootWitnesses {
  readonly quorum: number;
  readonly maxLagBlocks: number;
  private readonly waitMs: number;
  private readonly retryMs: number;
  private readonly attemptMs: number;
  private readonly headTtlMs: number;
  /** Roots confirmed (or being confirmed), by height. A failed confirmation is forgotten, and asked again. */
  private readonly roots = new Map<number, { root: string; done: Promise<void> }>();
  private head?: { at: number; height: Promise<number> };
  /** Rounds of witness requests made: one for each block a proof came from. */
  rounds = 0;

  constructor(
    readonly witnesses: readonly WitnessNode[],
    opts: ProvenOptions = {},
  ) {
    if (witnesses.length === 0) throw new Error("proven reads need at least one witness: a proof against a root only the node vouches for proves nothing");
    this.quorum = opts.quorum ?? witnesses.length;
    if (!Number.isSafeInteger(this.quorum) || this.quorum < 1 || this.quorum > witnesses.length) throw new Error(`the witness quorum is 1 to ${witnesses.length} (the number of witnesses)`);
    this.maxLagBlocks = opts.maxLagBlocks ?? DEFAULT_PROOF_MAX_LAG_BLOCKS;
    if (!Number.isSafeInteger(this.maxLagBlocks) || this.maxLagBlocks < 0) throw new Error("maxLagBlocks is a whole number of blocks");
    this.waitMs = opts.waitMs ?? 5_000;
    this.retryMs = opts.retryMs ?? 200;
    this.attemptMs = opts.attemptMs ?? 4_000;
    this.headTtlMs = opts.headTtlMs ?? 1_000;
  }

  /** Resolves once `root` is confirmed as the state root after block `height`; throws a `ProofError` otherwise. */
  confirm(height: number, root: string): Promise<void> {
    const known = this.roots.get(height);
    if (known) {
      // The witnesses were asked about this block already. Another root for it is the node contradicting itself.
      if (known.root !== root) return Promise.reject(new ProofError("disagreement", `the node gave two state roots for block #${height}: ${short(known.root)} and ${short(root)}`));
      return known.done;
    }
    const done = this.ask(height, root);
    this.roots.set(height, { root, done });
    done.catch(() => {
      if (this.roots.get(height)?.done === done) this.roots.delete(height);
    });
    if (this.roots.size > ROOTS_KEPT) {
      const oldest = Math.min(...this.roots.keys());
      this.roots.delete(oldest);
    }
    return done;
  }

  private ask(height: number, root: string): Promise<void> {
    this.rounds++;
    const until = Date.now() + this.waitMs;
    const n = this.witnesses.length;
    return new Promise<void>((resolve, reject) => {
      let confirmed = 0;
      /** Witnesses that answered once (a root, "not yet", or an error) or gave up. */
      let heard = 0;
      let gaveUp = 0;
      let over = false;
      const settle = () => {
        if (over) return;
        // A quorum, and every other witness has had its say once: one that would disagree isn't outrun.
        if (confirmed >= this.quorum && heard === n) {
          over = true;
          resolve();
        } else if (n - gaveUp < this.quorum) {
          // Too few are still trying for a quorum to be reached.
          over = true;
          reject(new ProofError("unconfirmed", `block #${height}'s state root was confirmed by ${confirmed} of ${n} witness(es) within ${this.waitMs} ms; ${this.quorum} must (a witness is down, or behind). Nothing was made of the record`));
        }
      };
      this.witnesses.forEach((w, i) => {
        void (async () => {
          let first = true;
          const once = () => {
            if (first) {
              first = false;
              heard++;
            }
          };
          for (;;) {
            if (over) return;
            try {
              const b = await within(w.block(height), this.attemptMs);
              if (typeof b?.state_root !== "string") throw new Error("no state root in the block");
              once();
              if (b.state_root.toLowerCase() !== root.toLowerCase()) {
                if (!over) {
                  over = true;
                  reject(new ProofError("disagreement", `${nameOf(w, i)} has state root ${short(b.state_root)} for block #${height}, the node's proof is against ${short(root)}: one of them is lying or broken. Nothing was made of the record`));
                }
                return;
              }
              confirmed++;
              return settle();
            } catch {
              // Not there yet (404), down, slow, or of another chain: asked again until the wait is over.
              once();
              settle();
            }
            if (Date.now() + this.retryMs > until) {
              gaveUp++;
              return settle();
            }
            await sleep(this.retryMs);
          }
        })();
      });
    });
  }

  /** The newest head the witnesses report (remembered for a moment). Throws `unconfirmed` when none answers. */
  newestHead(): Promise<number> {
    const now = Date.now();
    if (this.head && now - this.head.at < this.headTtlMs) return this.head.height;
    const height = (async () => {
      const heads = await Promise.all(
        this.witnesses.map((w) =>
          within(w.status(), this.attemptMs).then(
            (s) => (Number.isSafeInteger(s?.height) && (s.height as number) >= 0 ? (s.height as number) : undefined),
            () => undefined,
          ),
        ),
      );
      const known = heads.filter((h): h is number => h !== undefined);
      if (known.length === 0) throw new ProofError("unconfirmed", "no witness reports its height, so a proof's age can't be checked. Nothing was made of the record");
      return Math.max(...known);
    })();
    const entry = { at: now, height };
    this.head = entry;
    height.catch(() => {
      if (this.head === entry) this.head = undefined;
    });
    return height;
  }

  /** Throws `stale` when block `height` is more than `maxLagBlocks` behind the witnesses' newest head. */
  async fresh(height: number): Promise<void> {
    const head = await this.newestHead();
    if (height + this.maxLagBlocks < head) throw new ProofError("stale", `the node's proof is for block #${height} and the witnesses are at #${head} (at most ${this.maxLagBlocks} blocks behind is accepted): the node is serving old state, or is far behind`);
  }
}

/** One of a program's stored values, proven: `null` when the entry provably isn't there. */
export async function provenEntry(node: ProofNode, roots: RootWitnesses, program: string, key: string): Promise<{ value: string | null; height: number; root: string }> {
  const recordKey = storageRecordKey(program, key);
  const answer = await node.proof(NAMESPACES.PROGRAM_STORAGE, recordKey);
  let proven: { value: Uint8Array | null; root: string; height: number };
  try {
    proven = checkProofResponse(answer, NAMESPACES.PROGRAM_STORAGE, recordKey);
  } catch (e) {
    throw new ProofError("bad_proof", (e as Error).message);
  }
  // The head first: witnesses that are honest can then only be behind the proof, never far ahead of it by the time they're asked.
  await roots.fresh(proven.height);
  await roots.confirm(proven.height, proven.root);
  return { value: proven.value === null ? null : toHex(storedValue(proven.value)), height: proven.height, root: proven.root };
}

/**
 * A table whose every read is proven (see the top of this file). Use it wherever what is read
 * decides something that can't be taken back: the dealer's reveals.
 */
export class ProvenTable extends Table {
  readonly roots: RootWitnesses;
  /** Records read. */
  proofs = 0;

  constructor(
    program: string,
    private readonly node: ProofNode,
    witnesses: readonly WitnessNode[],
    opts: ProvenOptions = {},
  ) {
    // No storage route: a read that isn't a proof has nowhere to come from.
    super(program, () => Promise.reject(new Error("a proven table reads proofs only")));
    this.roots = new RootWitnesses(witnesses, opts);
  }

  /** One stored value by its key (hex), or `null`, proven; and the height of the state it was proven in. */
  override async entry(key: string): Promise<{ value: string | null; height: number }> {
    const { value, height } = await provenEntry(this.node, this.roots, this.program, key);
    this.proofs++;
    return { value, height };
  }
}
