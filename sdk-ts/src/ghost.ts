// 👻 `@blockula/sdk/ghost`: Ghost Mode (shielded transfers) for wallets. NOT AUDITED, and off on
// every chain but a local test chain made with `blockula dev --ghost` (docs/GHOST-MODE.md).
//
// A `GhostWallet` holds a Ghost Mode key, finds its notes on the chain and makes the three ghost
// transactions: `shield` (tag 50), `send` (a private transfer, tag 51) and `unshield` (tag 52).
// It's the CLI's wallet (`blockula ghost`, node/src/ghost_cli.rs) in TypeScript: the same key for
// the same seed, the same sync, coin selection, merges and fees.
//
// The cryptography runs in WebAssembly (node/ghost-wasm, the `blockula-ghost` crate itself), and
// the secrets stay there: this module holds opaque handles, never a spending key, a viewing key or
// a note's `rseed`. The note store keeps, for each of the wallet's notes, where it is and its
// ciphertext from the chain (public); the note is decrypted again inside the WebAssembly when it's
// spent. A note's value and memo are in the store, so it's still private data: a file store is
// mode 600.
//
// What a node is trusted for, as in the CLI: its leaves must hash to its own anchor, a note counts
// only if its plaintext opens the commitment on chain, and a proof against a wrong anchor or
// instance is refused by every honest node, so a lying node can waste a fee's worth of work but
// can't make the wallet spend what it doesn't mean to. Two things are the node's word: the
// recipient's address (so `send` needs the fingerprint its owner read out, or a `confirm` callback
// that's shown the one the node reported) and the fee it quotes (refused above `maxFee`).
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, hexToBytes, utf8ToBytes } from "@noble/hashes/utils.js";
import initWasm, * as wasm from "./ghost-wasm/blockula_ghost_wasm.js";
import { ApiError, Client, MIN_NEW_ACCOUNT, Wallet, bareName, formatFang, ghostSighash, isAddress, parseFang, txBytes, txId, type Action, type Amount, type FinishAction, type GhostBundle, type Tx, type TxOptions, type UnsignedTx } from "./index.js";

// ------------------------------------------------------------------------------- the WebAssembly

/** Where the prover's `.wasm` comes from, when it isn't next to this module: its bytes, a compiled module, or a URL to fetch. */
export type GhostWasmSource = BufferSource | WebAssembly.Module | URL | string | Response;

let loading: Promise<void> | undefined;

/**
 * Loads the prover (once). In Node it reads the `.wasm` shipped next to this module; in a browser
 * it fetches it from there (`new URL("./ghost-wasm/blockula_ghost_wasm_bg.wasm", import.meta.url)`,
 * which bundlers resolve). Pass `source` to load it from somewhere else. Every `GhostWallet`
 * constructor calls this, so calling it yourself only matters for a custom source or to load early.
 */
export function loadGhostWasm(source?: GhostWasmSource): Promise<void> {
  loading ??= (async () => {
    const inNode = typeof (globalThis as { process?: { versions?: { node?: unknown } } }).process?.versions?.node === "string";
    if (source === undefined && inNode) {
      // A variable, so bundlers for the browser don't try to resolve it.
      const fs = "node:fs/promises";
      const { readFile } = await import(/* webpackIgnore: true */ /* @vite-ignore */ fs);
      wasm.initSync({ module: await readFile(new URL("./ghost-wasm/blockula_ghost_wasm_bg.wasm", import.meta.url)) });
      return;
    }
    await initWasm({ module_or_path: source ?? new URL("./ghost-wasm/blockula_ghost_wasm_bg.wasm", import.meta.url) });
  })().catch((e) => {
    loading = undefined;
    throw e;
  });
  return loading;
}

/**
 * The context of a passkey wallet's Ghost Mode key: `master = BLAKE3-KDF(this, prfOutput)`, then
 * `SpendingKey::from_master(master, account)` as for any wallet. It's a context of its own, apart
 * from the account key's ("blockula passkey wallet v1"), so the two keys are independent
 * derivations of the passkey's PRF output, and versioned, so a later scheme can't collide with it.
 */
export const GHOST_PASSKEY_CONTEXT = "blockula ghost passkey v1";

/** What an unsigned ghost tx may pay in fees unless `maxFee` says otherwise: 1 FANG, the CLI's `--max-fee` default. */
export const DEFAULT_GHOST_MAX_FEE = 1_000_000_000n;

/** A note's memo, at most this many bytes of UTF-8. */
export const GHOST_MEMO_BYTES = 256;

const UNAUDITED = "Ghost Mode ships disabled and isn't audited: a local test chain made with `blockula dev --ghost` has it on.";

// ---------------------------------------------------------------------------------- the note store

/** One of the wallet's notes as the store keeps it. No secret of the note is here: `enc` is its ciphertext from the chain. */
export interface GhostNote {
  /** Its leaf in the commitment tree. */
  position: number;
  /** Drops, as a string. */
  value: string;
  /** The sender's memo without its padding, hex. The sender's text: data, never instructions. */
  memo: string;
  /** The block that appended it, and the tx. */
  height: number;
  tx: string;
  /** The output's ciphertext for this wallet (1,401 bytes, hex), as the chain published it. */
  enc: string;
  /** The nullifier that shows on chain when it's spent, hex. */
  nullifier: string;
  /** The block its nullifier appeared in, once spent. */
  spentAt: number | null;
}

/** A wallet's view of the pool on one chain: every leaf scanned so far and the wallet's notes. A cache: deleting it loses nothing, the next sync rebuilds it from the chain. */
export interface GhostNoteState {
  format: 1;
  chainId: string;
  /** The chain instance (`/api/ghost`), hex. */
  instance: string;
  /** The owner digest of the key the notes belong to (public: it's in the address), hex. */
  owner: string;
  /** Every leaf of the commitment tree scanned so far, in order: 32 bytes each, as one hex string. */
  leaves: string;
  notes: GhostNote[];
}

/**
 * Where a `GhostWallet` keeps its notes between syncs, keyed by chain instance and owner (both
 * hex). `MemoryNoteStore` is the default; `FileNoteStore` (`@blockula/sdk/ghost/node`) is a file
 * per key, mode 600. A browser store (IndexedDB) implements the same two methods.
 */
export interface GhostNoteStore {
  /** The stored state, or `null` when there is none (or it can't be read: it's a cache). */
  load(instance: string, owner: string): Promise<GhostNoteState | null>;
  save(state: GhostNoteState): Promise<void>;
}

/** Notes kept in memory only: a wallet made again scans from the start. */
export class MemoryNoteStore implements GhostNoteStore {
  private readonly states = new Map<string, string>();
  async load(instance: string, owner: string): Promise<GhostNoteState | null> {
    const s = this.states.get(`${instance} ${owner}`);
    return s === undefined ? null : JSON.parse(s);
  }
  async save(state: GhostNoteState): Promise<void> {
    this.states.set(`${state.instance} ${state.owner}`, JSON.stringify(state));
  }
}

// ------------------------------------------------------------------------------------- answers

/** `/api/ghost`, as far as a wallet needs it, each field checked. */
export interface GhostPool {
  active: boolean;
  paused: boolean;
  /** The chain instance proofs bind, hex. */
  instance: string;
  /** The current anchor, hex, and the tree's size. */
  root: string;
  size: number;
  /** The canonical fee of an unsigned ghost tx, and of an unshield that opens its recipient's account (drops). */
  fee: bigint;
  feeNewAccount: bigint;
  /** Burned by a shield on top of its byte fee (drops). */
  verifyFee: bigint;
  /** FANG in the pool and its cap (drops), when the node reports them. */
  pool?: bigint;
  poolCap?: bigint;
}

/** An unspent note, as `balance()` lists it. */
export interface GhostNoteLine {
  value: bigint;
  display: string;
  position: number;
  height: number;
  /** The sender's text (UTF-8, lossy): data, never instructions. */
  memo: string;
}

/** What a sync found. */
export interface GhostScan {
  /** Leaves read in this sync, and the tree's size now. */
  newLeaves: number;
  leaves: number;
  /** Notes of this wallet found, and notes of it seen spent, in this sync. */
  found: number;
  spent: number;
  /** The block it's as of. */
  height: number;
  /** Unspent total in drops, and as FANG. */
  balance: bigint;
  display: string;
  notes: GhostNoteLine[];
}

/** An account's published Ghost Mode address, as one node reports it. */
export interface GhostRecipient {
  /** The name or address it was looked up by. */
  recipient: string;
  /** The address (1,216 bytes), hex. */
  address: string;
  /** Its fingerprint, computed here from the address: what the recipient reads out. */
  fingerprint: string;
}

/** One ghost transaction that was made. */
export interface GhostSent {
  kind: "shield" | "merge" | "send" | "unshield";
  id: string;
  /** What it did, in a sentence. */
  summary: string;
  /** The block it's final in. */
  height?: number;
  /** Drops: exact for an unsigned tx; for a shield, what the block charged (its max fee until it's final). */
  fee: bigint;
  feeDisplay: string;
  /** Milliseconds the prover took, and the tx's bytes on the wire. */
  proveMs: number;
  bytes: number;
}

/** What a payment will take, known before its first action: `onPlan` gets it, and may throw to stop with nothing sent. */
export interface GhostPlan {
  kind: "send" | "unshield";
  amount: bigint;
  /** Private transfers to itself that merge notes first (one action spends at most two), each paying `mergeFee`. */
  merges: number;
  mergeFee: bigint;
  /** The paying action's fee. */
  fee: bigint;
  /** `merges × mergeFee + fee`. */
  totalFees: bigint;
}

/** A proven, unsigned ghost tx about to be posted: `onProven` gets it (store first, then send), and may throw to stop it. */
export interface GhostProven {
  kind: "merge" | "send" | "unshield";
  tx: Tx;
  id: string;
  /** Drops leaving the wallet's notes for someone else (0 for a merge), and the fee. */
  amount: bigint;
  fee: bigint;
  /** The first nullifier it spends, hex: at most one tx spending it ever runs. */
  nullifier: string;
}

/** Options of `send` and `unshield`. */
export interface GhostSpendOptions {
  /** The most one action may pay in fees: FANG ("1") or bigint drops. Default 1 FANG. The node quotes the fee. */
  maxFee?: Amount;
  /** How long to wait for each tx to be final (default 60 s). After it, `TxPendingError`: check it by id, never send it again. */
  waitMs?: number;
  onPlan?: (plan: GhostPlan) => void | Promise<void>;
  onProven?: (proven: GhostProven) => void | Promise<void>;
}

/** Options of `send`. */
export interface GhostSendOptions extends GhostSpendOptions {
  /** A note for the recipient, encrypted with the payment: up to 256 bytes of UTF-8. */
  memo?: string;
  /** The fingerprint of the recipient's address, as they read it out. Another one is refused. */
  fingerprint?: string;
  /** Shown the address the node reported and its fingerprint; resolve `true` only if a person checked it. Used when there's no `fingerprint`. */
  confirm?: (recipient: GhostRecipient) => boolean | Promise<boolean>;
}

// ------------------------------------------------------------------------------------- helpers

const HEX32 = /^[0-9a-f]{64}$/;
const ENC_HEX = /^[0-9a-f]{2802}$/;

const lied = (rpc: string, what: string) => new Error(`${rpc} lists the pool's outputs wrongly (${what}). Nothing was stored: scan from another node`);

function dropsField(g: any, key: string, rpc: string): bigint {
  const v = g?.[key];
  if (typeof v !== "string" || !/^\d{1,20}$/.test(v) || BigInt(v) >= 1n << 64n) throw new Error(`${rpc}/api/ghost has no ${key}`);
  return BigInt(v);
}

function hexField(g: any, key: string, rpc: string): string {
  const v = g?.[key];
  if (typeof v !== "string" || !HEX32.test(v)) throw new Error(`${rpc}/api/ghost has no ${key}`);
  return v;
}

/** A recipient that's only looked up: an address as given, or the normalized `.boo` name. */
function accountRef(who: string): string {
  const w = who.trim();
  if (isAddress(w)) return w;
  if (/^t?boo1/.test(w)) throw new Error(`${w} isn't a valid address (check for typos; addresses are lowercase)`);
  const n = bareName(w);
  if (!/^[a-z0-9-]{3,32}$/.test(n)) throw new Error(`${w} is neither an address nor a .boo name`);
  return `${n}.boo`;
}

const sameFingerprint = (a: string, b: string) => a.trim().toLowerCase() === b.trim().toLowerCase();

const plural = (n: number) => (n === 1 ? "" : "s");

/**
 * How many merges (private transfers to itself, spending two notes into one) a wallet holding
 * notes of `values` (largest first) makes before one action can pay `need`, when a merge pays
 * `mergeFee`: 0 if its two largest notes cover it, `null` if nothing it holds does.
 * node/src/ghost_cli.rs `merges_needed`.
 */
export function mergesNeeded(values: bigint[], need: bigint, mergeFee: bigint): number | null {
  if (values.length === 0) return null;
  let have = values[0] + (values[1] ?? 0n);
  let merges = 0;
  for (const v of values.slice(2)) {
    if (have >= need) break;
    // Merging in a note worth no more than the fee only loses FANG.
    if (v <= mergeFee || have < mergeFee) return null;
    have = have - mergeFee + v;
    merges += 1;
  }
  return have >= need ? merges : null;
}

/**
 * The notes one action spends to cover `need`, from `values` (largest first), as indexes into it:
 * the smallest single note that covers it, else the two largest if they do. `null`: merge first.
 * node/src/ghost_cli.rs `pick`.
 */
export function pickNotes(values: bigint[], need: bigint): number[] | null {
  for (let i = values.length - 1; i >= 0; i--) if (values[i] >= need) return [i];
  if (values.length >= 2 && values[0] + values[1] >= need) return [0, 1];
  return null;
}

/** What a spend of shielded notes pays. */
type Pay =
  | { kind: "send"; to: Uint8Array; label: string; amount: bigint; memo: Uint8Array }
  | { kind: "unshield"; to: string; amount: bigint; opensAccount: boolean };

/** The wallet's view of the pool on one chain, in memory: the tree (in WebAssembly) and its notes. */
interface View {
  chainId: string;
  instance: string;
  tree: wasm.GhostTree;
  leaves: string[];
  notes: GhostNote[];
}

// -------------------------------------------------------------------------------- the wallet

/**
 * A Ghost Mode wallet: one key, on the chain its `client` is on.
 *
 * ```ts
 * const ghost = await GhostWallet.fromWallet(wallet);              // the key `blockula ghost` derives from the same seed
 * await ghost.publishAddress();                                    // tag 53: others can send to the account's name
 * await ghost.shield("40");                                        // tag 50
 * await ghost.send("vlad.boo", "12.5", { memo: "for the garlic", fingerprint: "0da2-6e0c-41c7-e1f6-b5d9" });
 * await ghost.unshield("mina.boo", "3");                           // tag 52: amount and recipient are public
 * (await ghost.balance()).display;                                 // "24.492 FANG"
 * ```
 *
 * Amounts are FANG strings ("1.5") or bigint drops. Everything that sends waits for its tx to be
 * final and syncs again. One wallet does one thing at a time: calls queue behind each other.
 */
export class GhostWallet {
  private view?: View;
  private queue: Promise<unknown> = Promise.resolve();
  private closed = false;

  private constructor(
    private readonly key: wasm.GhostKey,
    /** The node it reads and submits to (and its chain pin). */
    readonly client: Client,
    /** The account wallet: signs `shield` and `publishAddress`. Without one, the wallet can still receive, send and unshield (those txs are unsigned). */
    readonly wallet: Wallet | undefined,
    private readonly store: GhostNoteStore,
  ) {}

  /**
   * The Ghost Mode wallet of an account wallet: `SpendingKey::from_master(wallet.seed, account)`,
   * the key the CLI derives from the same seed (`blockula ghost`, account 0), so the two see the
   * same notes. The seed is the master seed of the ghost keys through
   * BLAKE3-KDF("blockula ghost v1 account", seed ‖ account), a derivation apart from the ML-DSA
   * key's, so neither key says anything about the other.
   */
  static async fromWallet(wallet: Wallet, opts: { store?: GhostNoteStore; account?: number; wasm?: GhostWasmSource } = {}): Promise<GhostWallet> {
    await loadGhostWasm(opts.wasm);
    // A copy: the WebAssembly side wipes what it's handed.
    return new GhostWallet(wasm.GhostKey.fromMaster(wallet.seed.slice(), opts.account ?? 0), wallet.client, wallet, opts.store ?? new MemoryNoteStore());
  }

  /** `fromWallet(Wallet.fromSeed(seed, client))`: a 32-byte seed, hex or raw. */
  static async fromSeed(seed: string | Uint8Array, opts: { client?: Client; store?: GhostNoteStore; account?: number; wasm?: GhostWasmSource } = {}): Promise<GhostWallet> {
    return GhostWallet.fromWallet(Wallet.fromSeed(seed, opts.client), opts);
  }

  /**
   * The Ghost Mode wallet of a passkey: its key comes from the passkey's PRF output under a
   * context of its own, `master = BLAKE3-KDF("blockula ghost passkey v1", prfOutput)`
   * (`GHOST_PASSKEY_CONTEXT`), then `SpendingKey::from_master(master, account)`. So a passkey's
   * ghost key doesn't pass through its account key's seed, and the same passkey gives the same
   * ghost wallet on every device.
   *
   * **A passkey wallet always uses this, never `fromWallet`**: the two give different keys (and
   * addresses) for the same passkey. `wallet` is the account wallet it signs shields with
   * (default: `Wallet.fromPasskeyPrf(prfOutput, client)`, the same passkey's account).
   */
  static async fromPasskeyPrf(prfOutput: Uint8Array, opts: { client?: Client; wallet?: Wallet; store?: GhostNoteStore; account?: number; wasm?: GhostWasmSource } = {}): Promise<GhostWallet> {
    if (prfOutput.length < 32) throw new Error("a passkey's PRF output is at least 32 bytes");
    await loadGhostWasm(opts.wasm);
    const wallet = opts.wallet ?? Wallet.fromPasskeyPrf(prfOutput, opts.client);
    const master = blake3(prfOutput, { context: utf8ToBytes(GHOST_PASSKEY_CONTEXT) });
    const key = wasm.GhostKey.fromMaster(master, opts.account ?? 0);
    master.fill(0);
    return new GhostWallet(key, opts.client ?? wallet.client, wallet, opts.store ?? new MemoryNoteStore());
  }

  /** Drops the key and the tree (the WebAssembly side wipes the key bytes it can). The wallet can't be used after. */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.dropView();
    this.key.free();
  }

  private open(): void {
    if (this.closed) throw new Error("this GhostWallet was closed");
  }

  /** The Ghost Mode address: `owner ‖ ek`, 1,216 bytes. Public; nobody types it. */
  get address(): Uint8Array {
    this.open();
    return this.key.address();
  }

  get addressHex(): string {
    return bytesToHex(this.address);
  }

  /** The address's fingerprint (`xxxx-xxxx-xxxx-xxxx-xxxx`): read it to whoever pays you, so they can check the address their wallet found is yours. */
  get fingerprint(): string {
    this.open();
    return this.key.fingerprint();
  }

  /**
   * A viewing key's bytes: `"full"` (everything received and sent, and what's spent), `"incoming"`
   * or `"outgoing"`. **Whoever holds it sees that for as long as the wallet is used: it can't be
   * revoked.** None can spend. Never log it.
   */
  exportViewingKey(which: "full" | "incoming" | "outgoing"): Uint8Array {
    this.open();
    return this.key.exportViewingKey(which);
  }

  /** Runs `job` after every job queued before it. */
  private serial<T>(job: () => Promise<T>): Promise<T> {
    this.open();
    const run = this.queue.then(job, job);
    this.queue = run.catch(() => undefined);
    return run;
  }

  // ------------------------------------------------------------------------------ the node's word

  /** `/api/ghost`, checked: the circuit this build proves, and fees on the node's own grid. */
  async pool(): Promise<GhostPool> {
    const rpc = this.client.rpc;
    let g: any;
    try {
      g = await this.client.ghost();
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) throw new Error(`${rpc} doesn't serve /api/ghost: it runs a blockula without Ghost Mode`);
      throw e;
    }
    if (!Number.isSafeInteger(g?.circuit_id)) throw new Error(`${rpc}/api/ghost has no circuit_id`);
    if (g.circuit_id !== wasm.circuitId()) throw new Error(`${rpc} verifies Ghost Mode circuit ${g.circuit_id}, and this SDK proves circuit ${wasm.circuitId()}: update @blockula/sdk`);
    const [fee, feeNewAccount, grid] = [dropsField(g, "canonical_fee", rpc), dropsField(g, "canonical_fee_new_account", rpc), dropsField(g, "fee_grid", rpc)];
    // The fee rule (docs/GHOST-MODE.md section 8, "Fees"): whole multiples of the grid, or every node refuses the tx.
    if (grid === 0n || fee % grid !== 0n || feeNewAccount % grid !== 0n || feeNewAccount < fee) throw new Error(`${rpc} quotes ghost fees that aren't on its own fee grid. Nothing was signed: use another node`);
    if (!Number.isSafeInteger(g.frontier_size) || g.frontier_size < 0) throw new Error(`${rpc}/api/ghost has no frontier_size`);
    const optional = (k: string) => (typeof g[k] === "string" && /^\d{1,20}$/.test(g[k]) ? BigInt(g[k]) : undefined);
    return { active: g.active === true, paused: g.paused === true, instance: hexField(g, "instance", rpc), root: hexField(g, "root", rpc), size: g.frontier_size, fee, feeNewAccount, verifyFee: dropsField(g, "verify_fee", rpc), pool: optional("pool"), poolCap: optional("pool_cap") };
  }

  private on(pool: GhostPool): void {
    if (!pool.active) throw new Error(`Ghost Mode is off on the chain at ${this.client.rpc}. ${UNAUDITED}`);
  }

  private takes(pool: GhostPool): void {
    this.on(pool);
    if (pool.paused) throw new Error(`Ghost Mode is paused on the chain at ${this.client.rpc}: its validators stopped the pool, and only governance resumes it. Nothing was sent.`);
  }

  /**
   * The Ghost Mode address an account (a `.boo` name or an address) published, as this node
   * reports it: **the node's word**. Its fingerprint is computed here from the address; show it,
   * and pay only once the recipient confirmed it's theirs. `null`: they've published none.
   */
  async published(account: string): Promise<GhostRecipient | null> {
    this.open();
    const recipient = accountRef(account);
    let found: { address?: unknown };
    try {
      found = await this.client.ghostAddress(recipient);
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) {
        if (e.message === "account not found") throw new Error(`${recipient} isn't an account on this chain (a .boo name nobody holds, or a mistyped address)`);
        return null;
      }
      throw e;
    }
    const hex = typeof found?.address === "string" ? found.address.toLowerCase() : "";
    let fingerprint: string;
    try {
      fingerprint = wasm.addressFingerprint(hexToBytes(hex));
    } catch {
      throw new Error(`${this.client.rpc} reports a Ghost Mode address for ${recipient} that isn't one. Nothing was sent: use another node`);
    }
    return { recipient, address: hex, fingerprint };
  }

  // ------------------------------------------------------------------------------------- sync

  private dropView(): void {
    this.view?.tree.free();
    this.view = undefined;
  }

  /** The view of `pool`'s chain: the one in memory, else the store's (if every note in it opens the leaf it names), else an empty one. */
  private async viewOf(pool: GhostPool): Promise<View> {
    if (this.view?.instance === pool.instance) return this.view;
    this.dropView();
    const chainId = String((await this.client.status()).chain_id ?? "");
    const owner = bytesToHex(this.key.owner());
    const view: View = { chainId, instance: pool.instance, tree: new wasm.GhostTree(), leaves: [], notes: [] };
    const stored = await this.store.load(pool.instance, owner).catch(() => null);
    if (stored && this.restore(view, stored, owner)) return (this.view = view);
    // A store that isn't this chain's and key's, or doesn't add up, is a cache gone bad: scan again.
    view.tree.free();
    return (this.view = { ...view, tree: new wasm.GhostTree(), leaves: [], notes: [] });
  }

  private restore(view: View, s: GhostNoteState, owner: string): boolean {
    try {
      if (s?.format !== 1 || s.instance !== view.instance || s.owner !== owner || typeof s.leaves !== "string" || !Array.isArray(s.notes)) return false;
      if (s.leaves.length % 64 !== 0 || !/^[0-9a-f]*$/.test(s.leaves)) return false;
      view.tree.append(hexToBytes(s.leaves));
      const scanner = new wasm.GhostScanner(this.key);
      try {
        for (const n of s.notes) {
          if (!Number.isSafeInteger(n?.position) || typeof n.enc !== "string" || !ENC_HEX.test(n.enc) || typeof n.value !== "string") return false;
          const leaf = view.tree.leaf(n.position);
          // A note counts only if its ciphertext opens the leaf at its position, for this key.
          const found = leaf && scanner.tryDecrypt(leaf, hexToBytes(n.enc), n.position);
          if (!found) return false;
          const same = found.value.toString() === n.value && bytesToHex(found.nullifier) === n.nullifier;
          found.free();
          if (!same) return false;
          view.notes.push({ position: n.position, value: n.value, memo: typeof n.memo === "string" && /^[0-9a-f]*$/.test(n.memo) ? n.memo : "", height: Number.isSafeInteger(n.height) ? n.height : 0, tx: typeof n.tx === "string" ? n.tx : "", enc: n.enc, nullifier: n.nullifier, spentAt: Number.isSafeInteger(n.spentAt) ? n.spentAt : null });
        }
      } finally {
        scanner.free();
      }
      view.leaves = s.leaves ? [s.leaves] : [];
      return true;
    } catch {
      return false;
    }
  }

  /**
   * Brings the view up to the node's tree: appends every new leaf, keeps the outputs that decrypt
   * to this wallet's key, and marks its notes spent where their nullifier shows up. Nothing is
   * kept unless the leaves hash to the node's anchor. node/src/ghost_cli.rs `sync`.
   */
  private async syncNow(pool: GhostPool): Promise<{ view: View; newLeaves: number; found: number; spent: number; height: number }> {
    this.on(pool);
    const rpc = this.client.rpc;
    const view = await this.viewOf(pool);
    const scanner = new wasm.GhostScanner(this.key);
    // The wallet's unspent notes by the nullifier that spends them.
    const mine = new Map<string, number>();
    view.notes.forEach((n, i) => n.spentAt === null && mine.set(n.nullifier, i));
    let [newLeaves, found, spent] = [0, 0, 0];
    try {
      for (;;) {
        const from = view.tree.size();
        let page: any;
        try {
          page = await this.client.ghostCommitments(from, 256);
        } catch (e) {
          if (e instanceof ApiError && e.status === 404) throw new Error(`${rpc} doesn't serve /api/ghost/commitments, which wallets sync from: it runs an older blockula. Use a node on this release.`);
          if (e instanceof ApiError && e.status === 410) throw new Error(`${rpc} no longer holds the blocks this scan needs (${e.message}). Scan from an archive node (\`blockula run --archive\`), or a node that still holds them.`);
          throw e;
        }
        const size = page?.frontier_size;
        if (!Number.isSafeInteger(size)) throw lied(rpc, "no frontier_size");
        if (size < from) throw new Error(`The chain at ${rpc} has ${size} shielded outputs and this wallet's note store has ${from}: the node is behind, or the store is from another chain. Wait for the node to sync, or clear the note store and scan again.`);
        const list = page.commitments;
        if (!Array.isArray(list)) throw lied(rpc, "no commitments");
        const cms: string[] = [];
        for (const e of list) {
          const position = from + cms.length;
          if (e?.position !== position) throw lied(rpc, "a leaf out of order");
          if (typeof e.cm !== "string" || !HEX32.test(e.cm)) throw lied(rpc, "a commitment that isn't a digest");
          if (typeof e.enc !== "string" || !/^[0-9a-f]*$/.test(e.enc) || e.enc.length % 2) throw lied(rpc, "a ciphertext that isn't hex");
          if (!Number.isSafeInteger(e.height) || e.height < 0) throw lied(rpc, "a leaf without its block");
          if (position >= 2 ** 32) throw lied(rpc, "more leaves than the tree holds");
          cms.push(e.cm);
          // A spend is listed with the action it's in, which comes after the note it spends.
          const i = typeof e.nullifier === "string" ? mine.get(e.nullifier) : undefined;
          if (i !== undefined) {
            mine.delete(e.nullifier);
            view.notes[i].spentAt = e.height;
            spent += 1;
          }
          const note = e.enc.length === 2802 ? scanner.tryDecrypt(hexToBytes(e.cm), hexToBytes(e.enc), position) : undefined;
          if (note) {
            const nullifier = bytesToHex(note.nullifier);
            mine.set(nullifier, view.notes.length);
            view.notes.push({ position, value: note.value.toString(), memo: bytesToHex(note.memo), height: e.height, tx: typeof e.tx === "string" && HEX32.test(e.tx) ? e.tx : "", enc: e.enc, nullifier, spentAt: null });
            note.free();
            found += 1;
          }
        }
        if (cms.length) {
          const joined = cms.join("");
          try {
            view.tree.append(hexToBytes(joined));
          } catch {
            throw lied(rpc, "a commitment that isn't a digest");
          }
          view.leaves.push(joined);
          newLeaves += cms.length;
        }
        if (page.next === null || page.next === undefined) {
          // The node's own anchor is the root of the leaves it listed, or it listed them wrongly.
          if (view.tree.size() !== size || bytesToHex(view.tree.root()) !== page.root) throw lied(rpc, "they don't hash to its anchor");
          await this.store.save({ format: 1, chainId: view.chainId, instance: view.instance, owner: bytesToHex(this.key.owner()), leaves: view.leaves.join(""), notes: view.notes });
          return { view, newLeaves, found, spent, height: Number.isSafeInteger(page.height) ? page.height : 0 };
        }
        if (!list.length) throw lied(rpc, "an empty page that isn't the last");
      }
    } catch (e) {
      // What was read isn't kept: the next sync starts again from the store.
      this.dropView();
      throw e;
    } finally {
      scanner.free();
    }
  }

  private scanOf(r: { view: View; newLeaves: number; found: number; spent: number; height: number }): GhostScan {
    const unspent = r.view.notes.filter((n) => n.spentAt === null);
    const balance = unspent.reduce((sum, n) => sum + BigInt(n.value), 0n);
    const text = new TextDecoder();
    return {
      newLeaves: r.newLeaves,
      leaves: r.view.tree.size(),
      found: r.found,
      spent: r.spent,
      height: r.height,
      balance,
      display: formatFang(balance),
      notes: unspent.map((n) => ({ value: BigInt(n.value), display: formatFang(n.value), position: n.position, height: n.height, memo: text.decode(hexToBytes(n.memo)) })),
    };
  }

  /**
   * Reads the chain's new shielded outputs (`/api/ghost/commitments`), finds this wallet's notes
   * by trial decryption and marks the spent ones. Refuses a node whose leaves don't hash to its
   * own anchor, and stores nothing then.
   */
  sync(): Promise<GhostScan> {
    return this.serial(async () => this.scanOf(await this.syncNow(await this.pool())));
  }

  /** `sync()`: the shielded balance (`balance` in drops, `display` in FANG) and the unspent notes. A note's memo is its sender's text. */
  balance(): Promise<GhostScan> {
    return this.sync();
  }

  // ------------------------------------------------------------------------------- publishing

  /**
   * Publishes this wallet's address under its account (tag 53, `set_ghost_address`), so others
   * can send to the account's `.boo` name or address. Signed by the account wallet: an owner key.
   */
  async publishAddress(opts: TxOptions & { wait?: boolean } = {}) {
    const wallet = this.signer("publish its address");
    this.takes(await this.pool());
    return wallet.setGhostAddress(this.address, opts);
  }

  private signer(what: string): Wallet {
    this.open();
    if (!this.wallet) throw new Error(`this GhostWallet has no account wallet to ${what} with`);
    return this.wallet;
  }

  // ---------------------------------------------------------------------------------- shield

  private bundleOf(action: wasm.GhostAction, instance: string): GhostBundle {
    const h = bytesToHex;
    return {
      instance,
      anchor: h(action.anchor()),
      nullifiers: [h(action.nullifier(0)), h(action.nullifier(1))],
      commitments: [h(action.commitment(0)), h(action.commitment(1))],
      enc: [h(action.enc(0)), h(action.enc(1))],
      out: [h(action.out(0)), h(action.out(1))],
      proof: "",
    };
  }

  /**
   * A shield of `amount`, ready to be signed: the `draft` action (its proof a placeholder of the
   * largest size, so a fee priced on it covers the real one) and `finish`, which proves for the
   * tx it's given (`BuildOptions.finish`). `shield()` is `wallet.submitFinished(draft, finish)`;
   * this is for callers with a submit path of their own (the MCP's capped one). Call `done()`
   * when finished with it.
   */
  async prepareShield(amount: Amount): Promise<{ amount: bigint; verifyFee: bigint; draft: Action; finish: FinishAction; proveMs: () => number; done: () => void }> {
    this.open();
    const drops = parseFang(amount);
    if (drops <= 0n) throw new Error("shield an amount above 0");
    if (drops >= 1n << 64n) throw new Error("that amount is more FANG than exists");
    const pool = await this.pool();
    this.takes(pool);
    // A shield spends nothing shielded: two dummy spends, a note of the amount for this wallet and
    // a dummy output, anchored at the pool's current root.
    const action = new wasm.GhostAction(hexToBytes(pool.root));
    try {
      action.output(this.key, this.key.address(), drops, new Uint8Array());
      action.seal();
      if (action.valueBalance() !== (-drops).toString()) throw new Error("the shield doesn't add up. Nothing was signed");
      const bundle = this.bundleOf(action, pool.instance);
      let proveMs = 0;
      const shield = (proof: string): Action => ({ type: "shield", amount: drops.toString(), bundle: { ...bundle, proof } });
      const finish: FinishAction = (tx) => {
        // The proof is bound to the tx's nonce, fee and validity (and not to the placeholder).
        const sighash = ghostSighash({ ...tx, action: shield("") });
        const started = performance.now();
        const proof = action.prove(sighash);
        proveMs = performance.now() - started;
        return shield(bytesToHex(proof));
      };
      return { amount: drops, verifyFee: pool.verifyFee, draft: shield("00".repeat(wasm.maxProofBytes())), finish, proveMs: () => Math.round(proveMs), done: () => action.free() };
    } catch (e) {
      action.free();
      throw e;
    }
  }

  /**
   * Moves `amount` from the account into Ghost Mode (tag 50): a note of this wallet's own. Signed
   * by the account wallet (an owner key: session keys can't shield); the account also pays the
   * usual fee and the chain's `ghost_verify_fee`. Waits for it to be final, then syncs.
   */
  shield(amount: Amount, opts: TxOptions & { waitMs?: number } = {}): Promise<GhostSent> {
    return this.serial(async () => {
      const wallet = this.signer("shield");
      const prepared = await this.prepareShield(amount);
      try {
        let signed: Tx | undefined;
        const { waitMs, ...txOpts } = opts;
        const sent = await wallet.submitFinished(prepared.draft, prepared.finish, { ...txOpts, wait: false, onSigned: (tx) => void (signed = tx) });
        const id: string = sent?.id ?? txId(signed!);
        const done = await this.client.waitForTx(id, waitMs ?? 60_000, signed?.valid_until);
        await this.syncNow(await this.pool());
        const fee = /^\d+$/.test(String(done?.fee ?? "")) ? BigInt(done.fee) : BigInt(signed?.fee ?? 0);
        return { kind: "shield", id, summary: `👻 shielded ${formatFang(prepared.amount)} from ${wallet.account}`, height: Number.isSafeInteger(done?.height) ? done.height : undefined, fee, feeDisplay: formatFang(fee), proveMs: prepared.proveMs(), bytes: signed ? txBytes(signed).length : 0 };
      } finally {
        prepared.done();
      }
    });
  }

  // --------------------------------------------------------------------------- send, unshield

  /** The height a tx made now counts from, checked like any signing height, and the last block it's valid in: the chain's whole window. */
  private async validUntil(): Promise<{ chainId: string; genesisHash: string; validUntil: number }> {
    const status = await this.client.status();
    if (!Number.isSafeInteger(status.height) || status.height < 0) throw new Error(`${this.client.rpc} doesn't report its height`);
    this.client.checkMainnetPin(status.chain_id);
    await this.client.checkSigningHeight(status.height);
    const window = status.max_tx_validity_blocks;
    if (!Number.isSafeInteger(window) || window < 1) throw new Error(`${this.client.rpc} doesn't report max_tx_validity_blocks`);
    if (typeof status.chain_id !== "string" || typeof status.genesis_hash !== "string") throw new Error(`${this.client.rpc} doesn't report its chain`);
    return { chainId: status.chain_id, genesisHash: status.genesis_hash, validUntil: status.height + window };
  }

  /**
   * Builds, proves and submits one unsigned action spending `inputs` (one or two of the wallet's
   * notes): it pays `pay` (`null`: a merge), the `fee`, and the rest back to the wallet.
   */
  private async act(pool: GhostPool, view: View, inputs: GhostNote[], pay: Pay | null, fee: bigint, opts: GhostSpendOptions): Promise<GhostSent> {
    const total = inputs.reduce((sum, n) => sum + BigInt(n.value), 0n);
    const toRecipient = pay?.kind === "send" ? pay.amount : 0n;
    const leavingPublic = pay?.kind === "unshield" ? pay.amount : 0n;
    const change = total - toRecipient - leavingPublic - fee;
    if (change < 0n || change >= 1n << 64n) throw new Error("those notes don't cover the amount and the fee");
    const { chainId, genesisHash, validUntil } = await this.validUntil();
    const me = this.key.address();
    const none = new Uint8Array();
    const action = new wasm.GhostAction(view.tree.root());
    try {
      for (const n of inputs) {
        const value = action.spend(this.key, view.tree, n.position, hexToBytes(n.enc));
        if (value.toString() !== n.value) throw new Error("a note in the note store doesn't match the chain: clear the note store and scan again. Nothing was sent");
      }
      if (pay?.kind === "send") action.output(this.key, pay.to, pay.amount, pay.memo);
      if (change > 0n) action.output(this.key, me, change, none);
      action.seal();
      // What leaves the pool is exactly the public amount and the fee: checked on the witness itself.
      if (action.valueBalance() !== (leavingPublic + fee).toString()) throw new Error("the action doesn't add up. Nothing was sent");
      const bundle = this.bundleOf(action, pool.instance);
      const act: Action = pay?.kind === "unshield" ? { type: "unshield", to: pay.to, amount: pay.amount.toString(), bundle } : { type: "private_transfer", bundle };
      // An unsigned tx: no sender, nonce, key or signature. The proof authorizes it, the fee is
      // exact (it's in the value balance), and the tip is 0.
      const tx: Tx = { chain_id: chainId, genesis_hash: genesisHash, from: "", nonce: 0, fee: fee.toString(), tip: "0", valid_until: validUntil, action: act, memo: "", public_key: "", signature: "" };
      const started = performance.now();
      // `prove` checks every constraint first: a witness that doesn't add up is an error here,
      // never an invalid proof on the wire.
      bundle.proof = bytesToHex(action.prove(ghostSighash(tx)));
      const proveMs = Math.round(performance.now() - started);
      const kind = pay?.kind ?? "merge";
      const id = txId(tx);
      if (opts.onProven) await opts.onProven({ kind, tx, id, amount: toRecipient + leavingPublic, fee, nullifier: bundle.nullifiers[0] });
      const posted = await this.client.post(tx);
      if (posted.id !== id) throw new Error(`${this.client.rpc} took the transaction under another id (${posted.id}, not ${id}): check both, and don't send it again`);
      const done = await this.client.waitForTx(id, opts.waitMs ?? 60_000, validUntil);
      const summary = pay?.kind === "send" ? `👻 sent ${formatFang(pay.amount)} privately to ${pay.label}` : pay?.kind === "unshield" ? `👻 unshielded ${formatFang(pay.amount)} to ${pay.to}` : `👻 merged ${inputs.length} notes into one of ${formatFang(change)}`;
      return { kind, id, summary, height: Number.isSafeInteger(done?.height) ? done.height : undefined, fee, feeDisplay: formatFang(fee), proveMs, bytes: txBytes(tx).length };
    } finally {
      action.free();
    }
  }

  /**
   * Pays `pay` from the wallet's notes: syncs, merges notes while the two largest don't cover the
   * amount and the fee, then the payment. Every action waits for the one before it to be final,
   * and the wallet syncs in between (its change is a new note). node/src/ghost_cli.rs `spend`.
   */
  private async spend(pay: Pay, opts: GhostSpendOptions): Promise<GhostSent[]> {
    const rpc = this.client.rpc;
    const maxFee = opts.maxFee === undefined ? DEFAULT_GHOST_MAX_FEE : parseFang(opts.maxFee);
    const sent: GhostSent[] = [];
    let planned = false;
    // Each merge leaves one note fewer; the bound only stops a loop no honest node causes.
    for (let round = 0; round < 256; round++) {
      const pool = await this.pool();
      this.takes(pool);
      const { view } = await this.syncNow(pool);
      // A block landed between the two reads: read both again.
      if (view.tree.size() !== pool.size || bytesToHex(view.tree.root()) !== pool.root) continue;
      const fee = pay.kind === "unshield" && pay.opensAccount ? pool.feeNewAccount : pool.fee;
      if (fee > maxFee || pool.fee > maxFee) throw new Error(`${rpc} quotes a fee of ${formatFang(fee)} for this, above the most this pays (${formatFang(maxFee)}, maxFee). Nothing was sent.`);
      const need = pay.amount + fee;
      if (need >= 1n << 64n) throw new Error("that amount is more FANG than exists");
      // The unspent notes worth spending, largest first.
      const spendable = view.notes.filter((n) => n.spentAt === null && BigInt(n.value) > 0n).sort((a, b) => (BigInt(b.value) > BigInt(a.value) ? 1 : BigInt(b.value) < BigInt(a.value) ? -1 : a.position - b.position));
      const values = spendable.map((n) => BigInt(n.value));
      const balance = view.notes.reduce((sum, n) => (n.spentAt === null ? sum + BigInt(n.value) : sum), 0n);
      const merges = mergesNeeded(values, need, pool.fee);
      if (merges === null) {
        const why = balance >= need ? ` (its notes are too small to gather: each merge pays the ${formatFang(pool.fee)} fee)` : "";
        throw new Error(`This wallet has ${formatFang(balance)} shielded in ${values.length} note${plural(values.length)}, and this takes ${formatFang(need)} (${formatFang(pay.amount)} and the ${formatFang(fee)} fee)${why}. Nothing was sent.`);
      }
      if (!planned) {
        planned = true;
        if (opts.onPlan) await opts.onPlan({ kind: pay.kind, amount: pay.amount, merges, mergeFee: pool.fee, fee, totalFees: BigInt(merges) * pool.fee + fee });
      }
      const picked = pickNotes(values, need);
      if (picked) {
        sent.push(await this.act(pool, view, picked.map((i) => spendable[i]), pay, fee, opts));
        await this.syncNow(await this.pool());
        return sent;
      }
      sent.push(await this.act(pool, view, spendable.slice(0, 2), null, pool.fee, opts));
    }
    throw new Error(`The pool kept changing under this payment. ${sent.length} action${plural(sent.length)} went through; run it again for the rest.`);
  }

  /**
   * Sends `amount` privately (tag 51) to the Ghost Mode address `to` (a `.boo` name or an address)
   * published. That address is the node's word and a private transfer can't be undone, so this
   * needs the recipient's `fingerprint` (as they read it out: another one is refused) or a
   * `confirm` callback, which is shown the address and fingerprint the node reported. With
   * neither it's refused before anything is read. A payment that needs more than two notes merges
   * them first (`onPlan` says how many actions that is); the result lists every action, the
   * payment last.
   */
  send(to: string, amount: Amount, opts: GhostSendOptions = {}): Promise<GhostSent[]> {
    return this.serial(async () => {
      const drops = parseFang(amount);
      if (drops <= 0n) throw new Error("send an amount above 0");
      if (drops >= 1n << 64n) throw new Error("that amount is more FANG than exists");
      // Unattended, the fingerprint the recipient read out is the only check there is.
      if (opts.fingerprint === undefined && !opts.confirm) throw new Error(`send needs the recipient's fingerprint, because the Ghost Mode address this pays is only the node's word: pass the fingerprint ${to} read to you (their wallet shows it), or a confirm callback that shows it to a person. Nothing was sent.`);
      const memo = utf8ToBytes(opts.memo ?? "");
      if (memo.length > GHOST_MEMO_BYTES) throw new Error(`the memo is ${memo.length} bytes, and a note carries at most ${GHOST_MEMO_BYTES}`);
      this.takes(await this.pool());
      const recipient = await this.published(to);
      if (!recipient) throw new Error(`${accountRef(to)} hasn't published a Ghost Mode address on this chain (they publish it from their wallet first). Nothing was sent.`);
      if (opts.fingerprint !== undefined) {
        if (!sameFingerprint(opts.fingerprint, recipient.fingerprint)) throw new Error(`${recipient.recipient}'s Ghost Mode address at ${this.client.rpc} has the fingerprint ${recipient.fingerprint}, not ${opts.fingerprint.trim()}. Nothing was sent.`);
      } else if ((await opts.confirm!(recipient)) !== true) {
        throw new Error(`The fingerprint ${recipient.fingerprint} wasn't confirmed. Nothing was sent.`);
      }
      return this.spend({ kind: "send", to: hexToBytes(recipient.address), label: recipient.recipient, amount: drops, memo }, opts);
    });
  }

  /**
   * Moves `amount` out of Ghost Mode (tag 52) to a public account, a `.boo` name or an address:
   * **the amount and the recipient are public**. A name is signed as the name (the chain pays
   * whoever owns it when the tx runs) unless the client's witnesses proved its owner. Paying an
   * address with no account yet opens it: at least 0.01 FANG, and the fee is higher by the
   * account creation fee.
   */
  unshield(to: string, amount: Amount, opts: GhostSpendOptions = {}): Promise<GhostSent[]> {
    return this.serial(async () => {
      const drops = parseFang(amount);
      if (drops <= 0n) throw new Error("unshield an amount above 0");
      if (drops >= 1n << 64n) throw new Error("that amount is more FANG than exists");
      this.takes(await this.pool());
      const recipient = await this.client.recipient(to);
      const account = await this.client.account(recipient.address);
      // Opening an account burns the creation fee out of the tx's fee, so the fee is higher.
      const opensAccount = account?.exists === false;
      if (opensAccount && drops < MIN_NEW_ACCOUNT) throw new Error(`${recipient.signed} has no account yet, and opening one takes at least ${formatFang(MIN_NEW_ACCOUNT)}. Nothing was sent.`);
      return this.spend({ kind: "unshield", to: recipient.signed, amount: drops, opensAccount }, opts);
    });
  }

  /**
   * Merges the wallet's two largest notes into one (a private transfer to itself, paying the
   * fee), so a later payment needs fewer actions. `send` and `unshield` merge by themselves when
   * they must; this is for doing it ahead of time.
   */
  merge(opts: GhostSpendOptions = {}): Promise<GhostSent> {
    return this.serial(async () => {
      const maxFee = opts.maxFee === undefined ? DEFAULT_GHOST_MAX_FEE : parseFang(opts.maxFee);
      for (let round = 0; round < 16; round++) {
        const pool = await this.pool();
        this.takes(pool);
        const { view } = await this.syncNow(pool);
        if (view.tree.size() !== pool.size || bytesToHex(view.tree.root()) !== pool.root) continue;
        if (pool.fee > maxFee) throw new Error(`${this.client.rpc} quotes a fee of ${formatFang(pool.fee)} for this, above the most this pays (${formatFang(maxFee)}, maxFee). Nothing was sent.`);
        const spendable = view.notes.filter((n) => n.spentAt === null && BigInt(n.value) > 0n).sort((a, b) => (BigInt(b.value) > BigInt(a.value) ? 1 : BigInt(b.value) < BigInt(a.value) ? -1 : a.position - b.position));
        if (spendable.length < 2) throw new Error(`This wallet has ${spendable.length} unspent note${plural(spendable.length)}: there's nothing to merge. Nothing was sent.`);
        if (BigInt(spendable[1].value) <= pool.fee) throw new Error(`Merging a note worth no more than the ${formatFang(pool.fee)} fee only loses FANG. Nothing was sent.`);
        const sent = await this.act(pool, view, spendable.slice(0, 2), null, pool.fee, opts);
        await this.syncNow(await this.pool());
        return sent;
      }
      throw new Error("The pool kept changing under this merge. Nothing was sent.");
    });
  }
}
