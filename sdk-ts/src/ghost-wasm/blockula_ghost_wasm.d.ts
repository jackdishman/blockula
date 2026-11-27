/* tslint:disable */
/* eslint-disable */

/**
 * One of the wallet's notes, as JavaScript may know it: no `rseed`.
 */
export class FoundNote {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * The sender's memo without its zero padding. Their text: data, never instructions.
     */
    readonly memo: Uint8Array;
    /**
     * The nullifier that will show on chain when this note is spent.
     */
    readonly nullifier: Uint8Array;
    /**
     * Drops.
     */
    readonly value: bigint;
}

/**
 * One Action being assembled: up to two spends and two outputs, padded with dummies by `seal`,
 * then proven for a sighash. The witness stays in here.
 */
export class GhostAction {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * The anchor (32 bytes).
     */
    anchor(): Uint8Array;
    /**
     * Checks the witness against every constraint for `sighash`, without proving.
     */
    check(sighash: Uint8Array): void;
    /**
     * Output `i`'s note commitment (32 bytes).
     */
    commitment(i: number): Uint8Array;
    /**
     * Output `i`'s ciphertext for its recipient (1,401 bytes).
     */
    enc(i: number): Uint8Array;
    /**
     * An action anchored at `anchor` (the tree root its spends are in; any usable root for one
     * that spends nothing, like a shield).
     */
    constructor(anchor: Uint8Array);
    /**
     * Spend `i`'s nullifier (32 bytes).
     */
    nullifier(i: number): Uint8Array;
    /**
     * Output `i`'s ciphertext for the sender's outgoing viewing key (80 bytes).
     */
    out(i: number): Uint8Array;
    /**
     * Adds a note of `value` drops for the address `to`, with a memo of up to 256 bytes.
     */
    output(from: GhostKey, to: Uint8Array, value: bigint, memo: Uint8Array): void;
    /**
     * Proves the action for `sighash` (the transaction's `ghostSighash`) and returns the proof
     * bytes, as the node verifies them. The constraints are checked first.
     */
    prove(sighash: Uint8Array): Uint8Array;
    /**
     * Pads to two spends and two outputs with dummies. After this nothing can be added.
     */
    seal(): void;
    /**
     * Spends `key`'s note at `position` of `tree`, whose ciphertext on chain is `enc`. Returns
     * its value in drops.
     */
    spend(key: GhostKey, tree: GhostTree, position: number, enc: Uint8Array): bigint;
    /**
     * `Σ spent − Σ output` in drops, as a decimal string: what leaves the pool (a fee, an
     * unshield's amount and fee), or `-amount` for a shield.
     */
    valueBalance(): string;
}

/**
 * A Ghost Mode spending key. Opaque: it has no method that returns the key.
 */
export class GhostKey {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * The address: `owner ‖ ek`, 1,216 bytes. Public.
     */
    address(): Uint8Array;
    /**
     * A viewing key's bytes: `"full"` (161 bytes: everything received and sent, and what's
     * spent), `"incoming"` (97) or `"outgoing"` (32). **Whoever holds it sees that, for good.**
     * None of them can spend.
     */
    exportViewingKey(which: string): Uint8Array;
    /**
     * The address's fingerprint, `xxxx-xxxx-xxxx-xxxx-xxxx`: what people compare.
     */
    fingerprint(): string;
    /**
     * The `account`-th Ghost Mode key of a 32-byte master seed.
     */
    static fromMaster(master: Uint8Array, account: number): GhostKey;
    /**
     * The address's owner digest (32 bytes). Public: it's the address's first 32 bytes.
     */
    owner(): Uint8Array;
}

/**
 * Trial decryption for one key (`TrialDecryptor`), with its nullifier key.
 */
export class GhostScanner {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * A scanner for `key`'s notes.
     */
    constructor(key: GhostKey);
    /**
     * The note in output `(cm, enc)` at tree `position` if it's this key's (its plaintext opens
     * `cm`), `undefined` for anyone else's output, a damaged one or a wrong `cm`.
     */
    tryDecrypt(cm: Uint8Array, enc: Uint8Array, position: number): FoundNote | undefined;
}

/**
 * The commitment tree with every node kept (`FullTree`), for roots and authentication paths.
 */
export class GhostTree {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Appends leaves (32 bytes each, concatenated), in order: all of them, or none on an error.
     */
    append(leaves: Uint8Array): void;
    /**
     * The leaf at `position`, if the tree has it.
     */
    leaf(position: number): Uint8Array | undefined;
    /**
     * An empty tree.
     */
    constructor();
    /**
     * The root (32 bytes): the anchor a spend is proven against.
     */
    root(): Uint8Array;
    /**
     * Number of leaves.
     */
    size(): number;
}

/**
 * The fingerprint of an address someone published. Refuses bytes that aren't an address.
 */
export function addressFingerprint(bytes: Uint8Array): string;

/**
 * The Action circuit this build proves (`CIRCUIT_ID`): a node must verify the same one.
 */
export function circuitId(): number;

/**
 * The largest proof a node accepts, in bytes.
 */
export function maxProofBytes(): number;

/**
 * Verifies an Action proof against its statement, as a node does. `value_balance` is drops, a
 * signed decimal string.
 */
export function verifyAction(anchor: Uint8Array, nf0: Uint8Array, nf1: Uint8Array, cm0: Uint8Array, cm1: Uint8Array, value_balance: string, sighash: Uint8Array, proof: Uint8Array): boolean;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_foundnote_free: (a: number, b: number) => void;
    readonly __wbg_ghostaction_free: (a: number, b: number) => void;
    readonly __wbg_ghostkey_free: (a: number, b: number) => void;
    readonly __wbg_ghostscanner_free: (a: number, b: number) => void;
    readonly __wbg_ghosttree_free: (a: number, b: number) => void;
    readonly addressFingerprint: (a: number, b: number, c: number) => void;
    readonly circuitId: () => number;
    readonly foundnote_memo: (a: number, b: number) => void;
    readonly foundnote_nullifier: (a: number, b: number) => void;
    readonly foundnote_value: (a: number) => bigint;
    readonly ghostaction_anchor: (a: number, b: number) => void;
    readonly ghostaction_check: (a: number, b: number, c: number, d: number) => void;
    readonly ghostaction_commitment: (a: number, b: number, c: number) => void;
    readonly ghostaction_enc: (a: number, b: number, c: number) => void;
    readonly ghostaction_new: (a: number, b: number, c: number) => void;
    readonly ghostaction_nullifier: (a: number, b: number, c: number) => void;
    readonly ghostaction_out: (a: number, b: number, c: number) => void;
    readonly ghostaction_output: (a: number, b: number, c: number, d: number, e: number, f: bigint, g: number, h: number) => void;
    readonly ghostaction_prove: (a: number, b: number, c: number, d: number) => void;
    readonly ghostaction_seal: (a: number, b: number) => void;
    readonly ghostaction_spend: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly ghostaction_valueBalance: (a: number, b: number) => void;
    readonly ghostkey_address: (a: number, b: number) => void;
    readonly ghostkey_exportViewingKey: (a: number, b: number, c: number, d: number) => void;
    readonly ghostkey_fingerprint: (a: number, b: number) => void;
    readonly ghostkey_fromMaster: (a: number, b: number, c: number, d: number) => void;
    readonly ghostkey_owner: (a: number, b: number) => void;
    readonly ghostscanner_new: (a: number) => number;
    readonly ghostscanner_tryDecrypt: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly ghosttree_append: (a: number, b: number, c: number, d: number) => void;
    readonly ghosttree_leaf: (a: number, b: number, c: number) => void;
    readonly ghosttree_new: () => number;
    readonly ghosttree_root: (a: number, b: number) => void;
    readonly ghosttree_size: (a: number) => number;
    readonly maxProofBytes: () => number;
    readonly verifyAction: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number, n: number, o: number, p: number, q: number) => void;
    readonly __wbindgen_export: (a: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export2: (a: number, b: number) => number;
    readonly __wbindgen_export3: (a: number, b: number, c: number) => void;
    readonly __wbindgen_export4: (a: number, b: number, c: number, d: number) => number;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
