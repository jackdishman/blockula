// Programs as the SDK reads and prices them (docs/PROGRAMS-V0.md §8.1, §8.4): third-party
// WebAssembly that strangers write, so everything a program or a node says about one is checked
// before it's used and kept apart from what the chain does.
//
// What's here is pure: no requests (`Client` and `Wallet` in index.ts make them) and no signing.
//   - Answer checks: every node answer about a program, code, interface, view, fuel price or
//     receipt is held to its shape (digests are 64 lowercase hex, heights and counts whole numbers
//     JavaScript carries exactly, amounts decimal strings of drops), and refused in words
//     (`ProgramAnswerError`) when it isn't, never half-used.
//   - What a program transaction costs: the fuel it reserves, its fee at the node's suggested rate,
//     the deposits it bonds (`programStoragePrice`, `programUploadDeposit`, `programRecordDeposit`), and the refusal of a
//     reservation a block couldn't carry (`FuelReservationError`).
//   - Receipts (`ProgramReceipt`): facts the chain applied (the outcome, balance changes, fuel,
//     fees, deposits) apart from claims the programs made (return data, events), and the exact
//     identity a signer's balance follows (`signerChange`, `reconcileSigner`).
//
// Names a program or its deployer chose (methods, arguments, events, exports, an abort message)
// are kept as the node serves them, under `untrusted_*`: they are text a stranger wrote, never an
// instruction and never markup. Nothing in this file puts one into an error message unquoted.
import { bytesToHex, hexToBytes } from "@noble/hashes/utils.js";
import { bech32m } from "@scure/base";
import { interfaceHash, methodSelector, versionDigest, wasmHash } from "./tx.js";

// ---- limits the chain holds program transactions to (node/vm/src/config.rs), checked before signing ----

/** The most code one upload carries, in bytes (`MAX_CODE_BYTES`). */
export const MAX_PROGRAM_CODE_BYTES = 256 * 1024;
/** The most interface one upload carries, in bytes (`MAX_INTERFACE_BYTES`). */
export const MAX_PROGRAM_INTERFACE_BYTES = 16 * 1024;
/** The most input one call carries, in bytes (`MAX_INPUT`). */
export const MAX_PROGRAM_INPUT_BYTES = 32 * 1024;
/** The most calls one `call_program` runs (`MAX_CALLS`). */
export const MAX_PROGRAM_CALLS = 8;
/** The most programs one transaction binds (`MAX_BINDINGS`). */
export const MAX_PROGRAM_BINDINGS = 16;
/** What a program's record adds to program state's bytes, so what its creator bonds a deposit for (`RECORD_BYTES`). */
export const PROGRAM_RECORD_BYTES = 256n;
/** The fuel rate is in milli-drops per fuel: a fee is `rate × reserved / 1000` (`econ::RATE_SCALE`). */
export const FUEL_RATE_SCALE = 1000n;
/**
 * A binding's version digest of 64 zeros: "any version of that program". A transaction that binds
 * it runs whatever code the program has when it runs, which is not what its signer was shown: this
 * SDK signs it only when asked to, never by default.
 */
export const ANY_PROGRAM_VERSION = "0".repeat(64);

/**
 * The fuel schedule these numbers belong to (node/vm/src/meter.rs `SCHEDULE_V0`, `fuel.schedule`
 * 0 in `/api/fees`). A node that prices fuel by another schedule, or reports other numbers for this
 * one, is refused: the SDK prices what it signs with these.
 */
export const SCHEDULE_V0 = Object.freeze({ schedule: 0, load_base: 10_000n, load_per_byte: 40n, upload_base: 50_000n, upload_per_byte: 71n });

// ---- refusing a node's answer ----

/**
 * A node answered a question about programs with something no honest node says (a digest that isn't
 * 64 lowercase hex, an amount that isn't a decimal string, a height the chain can't have, …). Nothing
 * from the answer was used and nothing was signed on its word: ask another node.
 */
export class ProgramAnswerError extends Error {
  constructor(readonly rpc: string, readonly what: string, readonly field: string, readonly problem: string) {
    super(`${rpc} answered ${what} with a ${field} that ${problem}. Nothing from that answer was used: ask another node`);
    this.name = "ProgramAnswerError";
  }
}

/** The chain hasn't switched programs on (`/api/fees` has no `fuel` section): nothing was signed. */
export class ProgramsOffError extends Error {
  constructor(readonly rpc: string) {
    super(`programs aren't on at ${rpc}: this chain hasn't switched the \`programs\` feature on (a chain made with \`blockula dev --programs\` or \`blockula summon --programs\` has them from block 1). Nothing was signed`);
    this.name = "ProgramsOffError";
  }
}

/**
 * A transaction would reserve more fuel than one block counts (`program_max_block_fuel`), so no
 * block could include it: refused before it's signed. `reserved` is its fuel to run plus the load
 * of the code it declares.
 */
export class FuelReservationError extends Error {
  constructor(readonly reserved: bigint, readonly run: bigint, readonly load: bigint, readonly limit: bigint) {
    super(`this transaction would reserve ${reserved} fuel (${run} to run and ${load} to load its code), and a block counts at most ${limit} (program_max_block_fuel): lower the fuel limit or bind less. Nothing was signed`);
    this.name = "FuelReservationError";
  }
}

/** What can hide or reorder text: control, format (bidi, zero-width), private-use and surrogate characters, and line and paragraph separators. */
const HIDDEN = /[\p{Cc}\p{Cf}\p{Co}\p{Cs}\p{Zl}\p{Zp}]/gu;

/**
 * Text a node or a program chose, made inert before it's quoted anywhere (an error, a sentence):
 * stripped of control, format (bidi, zero-width), private-use, surrogate and line and paragraph
 * separator characters, as the MCP's `clean()` does. JSON quoting escapes control characters but
 * not the format ones (U+202E would still reorder the line), so every quote goes through this first.
 */
export const plainText = (s: string): string => s.replace(HIDDEN, "");

/** A node's text, stripped of hidden characters, cut and quoted so an error can show what it said without becoming it. */
function shown(v: unknown): string {
  const s = typeof v === "string" ? JSON.stringify([...plainText(v)].slice(0, 40).join("")) : plainText(JSON.stringify(v) ?? String(v));
  return s.length > 60 ? `${s.slice(0, 60)}…` : s;
}

/** The checks of one node answer, each refusing in the answer's own terms (`ProgramAnswerError`). */
class Answer {
  constructor(readonly rpc: string, readonly what: string) {}

  fail(field: string, problem: string): never {
    throw new ProgramAnswerError(this.rpc, this.what, field, problem);
  }

  /** A JSON object (not null, not an array). */
  obj(v: unknown, field: string): Record<string, unknown> {
    if (v === null || typeof v !== "object" || Array.isArray(v)) this.fail(field, `isn't an object (${shown(v)})`);
    return v as Record<string, unknown>;
  }

  /** A JSON array of at most `max` entries. */
  arr(v: unknown, field: string, max = 4096): unknown[] {
    if (!Array.isArray(v)) this.fail(field, `isn't a list (${shown(v)})`);
    if ((v as unknown[]).length > max) this.fail(field, `has ${(v as unknown[]).length} entries, past the most a node serves (${max})`);
    return v as unknown[];
  }

  /** A string of at most `max` characters (what a program or a deployer wrote is bounded too). */
  str(v: unknown, field: string, max = 1024): string {
    if (typeof v !== "string") this.fail(field, `isn't text (${shown(v)})`);
    if ((v as string).length > max) this.fail(field, `is ${(v as string).length} characters, past ${max}`);
    return v as string;
  }

  bool(v: unknown, field: string): boolean {
    if (typeof v !== "boolean") this.fail(field, `isn't true or false (${shown(v)})`);
    return v as boolean;
  }

  /** A whole number JavaScript carries exactly (heights, counts, fuel): 0 to 2^53 − 1. */
  int(v: unknown, field: string): number {
    if (typeof v !== "number" || !Number.isSafeInteger(v) || v < 0) this.fail(field, `isn't a whole number from 0 to 2^53 − 1 (${shown(v)})`);
    return v as number;
  }

  /** An amount of drops as the node writes it: a decimal string without a sign or leading zeros, below 2^64. */
  amount(v: unknown, field: string): string {
    if (typeof v !== "string" || !/^(0|[1-9]\d*)$/.test(v) || BigInt(v) >= 1n << 64n) this.fail(field, `isn't an amount of drops (a decimal string below 2^64: ${shown(v)})`);
    return v as string;
  }

  /** A signed change of drops (a balance change): a decimal string, its magnitude below 2^64. */
  change(v: unknown, field: string): string {
    if (typeof v !== "string" || !/^(0|-?[1-9]\d*)$/.test(v) || BigInt(v) >= 1n << 64n || BigInt(v) <= -(1n << 64n)) this.fail(field, `isn't a change of drops (a signed decimal string: ${shown(v)})`);
    return v as string;
  }

  /** A digest or hash: 64 lowercase hex characters. */
  digest(v: unknown, field: string): string {
    if (typeof v !== "string" || !/^[0-9a-f]{64}$/.test(v)) this.fail(field, `isn't 64 lowercase hex characters (${shown(v)})`);
    return v as string;
  }

  /** Lowercase hex of whole bytes, at most `maxBytes` of them (empty allowed). */
  hex(v: unknown, field: string, maxBytes: number): string {
    if (typeof v !== "string" || v.length % 2 !== 0 || !/^[0-9a-f]*$/.test(v)) this.fail(field, `isn't lowercase hex of whole bytes (${shown(v)})`);
    if ((v as string).length > 2 * maxBytes) this.fail(field, `is ${(v as string).length / 2} bytes, past ${maxBytes}`);
    return v as string;
  }

  address(v: unknown, field: string): string {
    if (!isAddr(v)) this.fail(field, `isn't an address (${shown(v)})`);
    return v as string;
  }

  /** `{ address, name, short }`, as the node labels an account: the name is a stranger's text. */
  label(v: unknown, field: string): AccountLabel {
    const o = this.obj(v, field);
    return { address: this.address(o.address, `${field}.address`), name: o.name === null || o.name === undefined ? null : this.str(o.name, `${field}.name`, 128), short: this.str(o.short, `${field}.short`, 64) };
  }
}

/** Whether `v` is a canonical address under a known prefix (kept here so this module has no import cycle). */
function isAddr(v: unknown): v is string {
  if (typeof v !== "string" || v.length > 90 || v !== v.toLowerCase()) return false;
  try {
    const { prefix, words } = bech32m.decode(v as `${string}1${string}`);
    return (prefix === "boo" || prefix === "tboo") && bech32m.fromWords(words).length === 32;
  } catch {
    return false;
  }
}

/** `boo1ab…cdef`: the short form of an address for sentences. */
function shortAddress(a: string): string {
  return a.length > 18 ? `${a.slice(0, 10)}…${a.slice(-6)}` : a;
}

/** An account as the node labels it. */
export interface AccountLabel {
  address: string;
  /** A `.boo` name that account owns, or a system label: a stranger's text. */
  name: string | null;
  short: string;
}

// ---- arguments, refused before any request or signature ----

/** A program's address as a caller gives it: never a name, which moves with its owner (docs/PROGRAMS-V0.md §8.1). */
export function programAddressArg(s: unknown, what = "a program"): string {
  if (!isAddr(s)) throw new Error(`${what} is named by its address (boo1… or tboo1…), never by a .boo name: ${shown(s)} isn't one`);
  return s;
}

/** A code, interface or version digest as a caller gives it: 64 lowercase hex characters. */
export function programHashArg(s: unknown, what: string): string {
  if (typeof s !== "string" || !/^[0-9a-f]{64}$/.test(s)) throw new Error(`${what} is 64 lowercase hex characters, not ${shown(s)}`);
  return s;
}

/** A method name: an identifier, as the chain's interfaces name them (`[a-z][a-z0-9_]{0,31}`). */
export function programMethodArg(s: unknown): string {
  if (typeof s !== "string" || !IDENT.test(s)) throw new Error(`a method name is a lowercase letter, then lowercase letters, digits and underscores, at most 32: ${shown(s)} isn't one`);
  return s;
}

/** Bytes as a caller gives them (code, an interface, an input): raw, or lowercase hex of whole bytes; at most `max`. */
export function programBytesArg(v: unknown, what: string, max: number): Uint8Array {
  let bytes: Uint8Array;
  if (v instanceof Uint8Array) bytes = v;
  else if (typeof v === "string" && v.length % 2 === 0 && /^[0-9a-f]*$/.test(v)) bytes = hexToBytes(v);
  else throw new Error(`${what} is bytes, or lowercase hex of whole bytes, not ${shown(v)}`);
  if (bytes.length > max) throw new Error(`${what} is ${bytes.length} bytes, past the ${max} the chain takes`);
  return bytes;
}

/** A call's input as the request and the transaction carry it: lowercase hex, at most `MAX_PROGRAM_INPUT_BYTES`. */
export const programInputArg = (v: unknown): string => bytesToHex(programBytesArg(v, "an input", MAX_PROGRAM_INPUT_BYTES));

/** A fuel amount a caller gives: a whole number from 1 to 2^53 − 1 (the chain's own bound is lower). */
export function positiveFuel(n: unknown): number {
  if (typeof n !== "number" || !Number.isSafeInteger(n) || n < 1) throw new Error(`fuel is a whole number of at least 1, not ${shown(n)}`);
  return n;
}

// ---- what the node says about a program, its code and interface ----

/** A type in an interface (node/src/programs/interface_json.rs `TypeJson`): scalars as strings, the rest as one-key objects. */
export type ProgramType = string | { bytes: number } | { text: number } | { option: ProgramType } | { vec: [ProgramType, number] };

const SCALARS = new Set(["u8", "u16", "u32", "u64", "u128", "i64", "i128", "bool", "fang", "address", "name", "hash32"]);

/** An interface type as the node serves it, refusing anything outside the closed vocabulary (and deeper than the format allows: 4 layers). */
function checkType(a: Answer, v: unknown, field: string, depth = 0): ProgramType {
  // `depth` counts the option and vec layers around this type: an interface allows four (§3.2).
  if (depth > 4) a.fail(field, "nests deeper than an interface type can");
  if (typeof v === "string") {
    if (!SCALARS.has(v)) a.fail(field, `isn't a type an interface can hold (${shown(v)})`);
    return v;
  }
  const o = a.obj(v, field);
  const keys = Object.keys(o);
  if (keys.length !== 1) a.fail(field, `isn't one type (${shown(keys)})`);
  switch (keys[0]) {
    case "bytes": return { bytes: a.int(o.bytes, `${field}.bytes`) };
    case "text": return { text: a.int(o.text, `${field}.text`) };
    case "option": return { option: checkType(a, o.option, `${field}.option`, depth + 1) };
    case "vec": {
      const pair = a.arr(o.vec, `${field}.vec`, 2);
      if (pair.length !== 2) a.fail(`${field}.vec`, "isn't a type and a most");
      return { vec: [checkType(a, pair[0], `${field}.vec[0]`, depth + 1), a.int(pair[1], `${field}.vec[1]`)] };
    }
    default: return a.fail(field, `isn't a type an interface can hold (${shown(keys[0])})`);
  }
}

/** A method of a program's interface, as `/api/program` serves it. Its names are the deployer's. */
export interface ProgramMethod {
  /** The deployer's identifier for it: an untrusted claim. */
  untrusted_name: string;
  /** The method's selector, 16 hex: BLAKE3 of its name (`methodSelector`). */
  selector: string;
  /** What the chain enforces: `call` (a transaction), `view` (a read) or `init` (once, when the program is created). */
  kind: "call" | "view" | "init";
  payable: boolean;
  args: { untrusted_name: string; type: ProgramType }[];
  returns: ProgramType | null;
}

const IDENT = /^[a-z][a-z0-9_]{0,31}$/;

function checkMethod(a: Answer, v: unknown, field: string): ProgramMethod {
  const o = a.obj(v, field);
  const untrusted_name = a.str(o.untrusted_name, `${field}.untrusted_name`, 64);
  // The chain's identifiers are `[a-z][a-z0-9_]{0,31}`: any other text is no method of any program.
  if (!IDENT.test(untrusted_name)) a.fail(`${field}.untrusted_name`, `isn't an identifier a method can have (${shown(untrusted_name)})`);
  const selector = a.str(o.selector, `${field}.selector`, 16);
  if (selector !== methodSelector(untrusted_name)) a.fail(`${field}.selector`, `isn't the selector of ${shown(untrusted_name)}`);
  const kind = o.kind;
  if (kind !== "call" && kind !== "view" && kind !== "init") a.fail(`${field}.kind`, `isn't call, view or init (${shown(kind)})`);
  const args = a.arr(o.args, `${field}.args`, 16).map((x, i) => {
    const arg = a.obj(x, `${field}.args[${i}]`);
    const name = a.str(arg.untrusted_name, `${field}.args[${i}].untrusted_name`, 64);
    if (!IDENT.test(name)) a.fail(`${field}.args[${i}].untrusted_name`, `isn't an identifier an argument can have (${shown(name)})`);
    return { untrusted_name: name, type: checkType(a, arg.type, `${field}.args[${i}].type`) };
  });
  return { untrusted_name, selector, kind: kind as ProgramMethod["kind"], payable: a.bool(o.payable, `${field}.payable`), args, returns: o.returns === null || o.returns === undefined ? null : checkType(a, o.returns, `${field}.returns`) };
}

/**
 * A program as a node serves it (`GET /api/program/{address}`), checked: its `version_digest` is
 * the one its own hashes and version compute (so a node can't show one program's code under
 * another's digest), and its numbers and digests are well formed. `proven: false`: this is one
 * node's word (`Client.provenProgram` proves the record itself against witnesses).
 */
export interface ProgramInfo {
  address: string;
  creator: AccountLabel;
  created_at: number;
  code: { wasm_hash: string; size: number | null; interface_hash: string };
  version: number;
  /** What a call to this program binds (`versionDigest`), recomputed from `code` and `version`, never taken on trust. */
  version_digest: string;
  authority: AccountLabel | null;
  upgrade_delay_blocks: number;
  policy: "open" | "additive";
  /** The node's sentence about who may upgrade it: its words, not a rule. */
  upgradability: string;
  accepts_transfers: boolean;
  /** Drops it holds, as a decimal string. */
  balance: string;
  storage: { items: number; bytes: number; deposit: string; sponsored: string };
  record_deposit: string;
  record_payer: AccountLabel;
  /** Its interface's methods; `null` only if the node's state is broken. The names are the deployer's. */
  methods: ProgramMethod[] | null;
  /** Where `/api/proof` serves this record. */
  proof_path: string;
  /** The block the node read it at. */
  height: number;
  proven: false;
}

/** `GET /api/program/{address}` checked against what was asked (`address`) and the chain's clock (`clock`: refuses a height the chain can't have reached). */
export function checkProgram(rpc: string, address: string, raw: unknown, clock: (height: number) => void = () => {}): ProgramInfo {
  const a = new Answer(rpc, `GET /api/program/${isAddr(address) ? address : shown(address)}`);
  const o = a.obj(raw, "answer");
  if (a.address(o.address, "address") !== address) a.fail("address", `isn't the program that was asked for (${shown(o.address)})`);
  const code = a.obj(o.code, "code");
  const wasm_hash = a.digest(code.wasm_hash, "code.wasm_hash");
  const interface_hash = a.digest(code.interface_hash, "code.interface_hash");
  const version = a.int(o.version, "version");
  if (version < 1 || version > 0xffff_ffff) a.fail("version", `isn't a program version (${version})`);
  const reported = a.digest(o.version_digest, "version_digest");
  const digest = versionDigest(address, wasm_hash, interface_hash, version);
  if (reported !== digest) a.fail("version_digest", `isn't what its code, interface and version compute (${shown(reported)}, not ${digest.slice(0, 16)}…): the node is showing one thing and would have you sign another`);
  const policy = o.policy;
  if (policy !== "open" && policy !== "additive") a.fail("policy", `isn't open or additive (${shown(policy)})`);
  const storage = a.obj(o.storage, "storage");
  const height = a.int(o.height, "height");
  clock(height);
  return {
    address,
    creator: a.label(o.creator, "creator"),
    created_at: a.int(o.created_at, "created_at"),
    code: { wasm_hash, size: code.size === null || code.size === undefined ? null : a.int(code.size, "code.size"), interface_hash },
    version,
    version_digest: digest,
    authority: o.authority === null || o.authority === undefined ? null : a.label(o.authority, "authority"),
    upgrade_delay_blocks: a.int(o.upgrade_delay_blocks, "upgrade_delay_blocks"),
    policy: policy as "open" | "additive",
    upgradability: a.str(o.upgradability, "upgradability", 600),
    accepts_transfers: a.bool(o.accepts_transfers, "accepts_transfers"),
    balance: a.amount(o.balance, "balance"),
    storage: { items: a.int(storage.items, "storage.items"), bytes: a.int(storage.bytes, "storage.bytes"), deposit: a.amount(storage.deposit, "storage.deposit"), sponsored: a.amount(storage.sponsored, "storage.sponsored") },
    record_deposit: a.amount(o.record_deposit, "record_deposit"),
    record_payer: a.label(o.record_payer, "record_payer"),
    methods: o.methods === null || o.methods === undefined ? null : a.arr(o.methods, "methods", 64).map((m, i) => checkMethod(a, m, `methods[${i}]`)),
    proof_path: a.str(o.proof_path, "proof_path", 300),
    height,
    proven: false,
  };
}

/** A stored code as `GET /api/code/{hash}` serves it, checked. */
export interface ProgramCodeInfo {
  wasm_hash: string;
  size: number;
  /** The method names its exported functions carry (`bk_<name>`): the deployer's. */
  untrusted_exports: string[];
  uploader: AccountLabel | null;
  /** What was bonded for it, drops. */
  deposit: string | null;
  /** How many programs use it. */
  refs: number | null;
  /** The block that stored it: usable from the block after. */
  uploaded_at: number | null;
  last_used_at: number | null;
  /** The code itself, when it was asked for (`?wasm=1`) and its hash checked out. */
  wasm?: Uint8Array;
  height: number;
}

/** `GET /api/code/{hash}[?wasm=1]` checked: it's the code asked for, and when the bytes came they hash to it. */
export function checkCode(rpc: string, hash: string, raw: unknown, wantWasm: boolean, clock: (height: number) => void = () => {}): ProgramCodeInfo {
  const a = new Answer(rpc, `GET /api/code/${hash}`);
  const o = a.obj(raw, "answer");
  if (a.digest(o.wasm_hash, "wasm_hash") !== hash) a.fail("wasm_hash", `isn't the code that was asked for (${shown(o.wasm_hash)})`);
  const size = a.int(o.size, "size");
  // Exports are the `bk_<name>` functions of validated code, so each is an identifier: any other text is no export of any code.
  const exports = a.arr(o.untrusted_exports, "untrusted_exports", 64).map((e, i) => {
    const name = a.str(e, `untrusted_exports[${i}]`, 64);
    if (!IDENT.test(name)) a.fail(`untrusted_exports[${i}]`, `isn't an identifier a method can have (${shown(name)})`);
    return name;
  });
  const height = a.int(o.height, "height");
  clock(height);
  const out: ProgramCodeInfo = {
    wasm_hash: hash,
    size,
    untrusted_exports: exports,
    uploader: o.uploader === null || o.uploader === undefined ? null : a.label(o.uploader, "uploader"),
    deposit: o.deposit === null || o.deposit === undefined ? null : a.amount(o.deposit, "deposit"),
    refs: o.refs === null || o.refs === undefined ? null : a.int(o.refs, "refs"),
    uploaded_at: o.uploaded_at === null || o.uploaded_at === undefined ? null : a.int(o.uploaded_at, "uploaded_at"),
    last_used_at: o.last_used_at === null || o.last_used_at === undefined ? null : a.int(o.last_used_at, "last_used_at"),
    height,
  };
  if (wantWasm) {
    const bytes = hexToBytes(a.hex(o.wasm, "wasm", MAX_PROGRAM_CODE_BYTES));
    if (wasmHash(bytes) !== hash) a.fail("wasm", "doesn't hash to the code that was asked for: a different program's bytes");
    if (bytes.length !== size) a.fail("wasm", `is ${bytes.length} bytes, not the ${size} the answer says`);
    out.wasm = bytes;
  }
  return out;
}

/** A stored interface as `GET /api/interface/{hash}` serves it, checked. */
export interface ProgramInterfaceInfo {
  interface_hash: string;
  size: number;
  /** The interface's format-1 bytes (docs/PROGRAMS-V0.md §3.2), which hash to `interface_hash`. */
  bytes: Uint8Array;
  hex: string;
  /**
   * The node's reading of those bytes as JSON (§3.4): every name in it is the deployer's. It isn't
   * checked against the bytes (this SDK has no interface decoder yet): the bytes are what `interface_hash` names.
   */
  untrusted_interface: unknown;
  uploader: AccountLabel | null;
  deposit: string | null;
  refs: number | null;
  uploaded_at: number | null;
  last_used_at: number | null;
  height: number;
}

/** `GET /api/interface/{hash}` checked: it's the interface asked for, and its bytes hash to it. */
export function checkInterface(rpc: string, hash: string, raw: unknown, clock: (height: number) => void = () => {}): ProgramInterfaceInfo {
  const a = new Answer(rpc, `GET /api/interface/${hash}`);
  const o = a.obj(raw, "answer");
  if (a.digest(o.interface_hash, "interface_hash") !== hash) a.fail("interface_hash", `isn't the interface that was asked for (${shown(o.interface_hash)})`);
  const size = a.int(o.size, "size");
  const hex = a.hex(o.hex, "hex", MAX_PROGRAM_INTERFACE_BYTES);
  const bytes = hexToBytes(hex);
  if (interfaceHash(bytes) !== hash) a.fail("hex", "doesn't hash to the interface that was asked for: a different interface's bytes");
  if (bytes.length !== size) a.fail("size", `isn't the ${bytes.length} bytes the answer holds`);
  const height = a.int(o.height, "height");
  clock(height);
  const opt = <T>(v: unknown, f: (x: unknown) => T): T | null => (v === null || v === undefined ? null : f(v));
  return {
    interface_hash: hash,
    size,
    bytes,
    hex,
    untrusted_interface: o.untrusted_interface ?? null,
    uploader: opt(o.uploader, (x) => a.label(x, "uploader")),
    deposit: opt(o.deposit, (x) => a.amount(x, "deposit")),
    refs: opt(o.refs, (x) => a.int(x, "refs")),
    uploaded_at: opt(o.uploaded_at, (x) => a.int(x, "uploaded_at")),
    last_used_at: opt(o.last_used_at, (x) => a.int(x, "last_used_at")),
    height,
  };
}

/** How a failed program run ended (docs/PROGRAMS-V0.md §8.1's table): numbers, never engine text. */
export interface ProgramOutcome {
  ok: boolean;
  /** The outcome's number in §8.1's table (0 for ok). */
  number: number;
  /** `ok`, or the table's code: `trapped`, `aborted`, `commit`, `value_cap`, `exit_only`, `storage_limit`, `state_full`, `invalid_code`, `translation_refused`, `interface_mismatch`, `native`. */
  code: string;
  /** Where a trap or an abort began. */
  program?: string;
  trap?: { number: number; name: string };
  abort_code?: number;
  /** An abort's own message (at most 256 bytes, sanitized by the VM): the program's words. */
  untrusted_message?: string;
  commit_error?: number;
  /** A value cap failure's cap and the control it would have left, drops. */
  cap?: string;
  control?: string;
  rule?: { number: number; name: string };
  func?: number;
  offset?: number;
}

/** §8.1's table: each code's number. */
const OUTCOME_NUMBERS: Record<string, number> = { trapped: 1, aborted: 2, commit: 3, value_cap: 10, exit_only: 11, storage_limit: 12, state_full: 13, invalid_code: 14, translation_refused: 15, interface_mismatch: 16, native: 20 };

function checkOutcome(a: Answer, v: unknown, field: string): ProgramOutcome {
  const o = a.obj(v, field);
  const ok = a.bool(o.ok, `${field}.ok`);
  if (ok) return { ok: true, number: 0, code: "ok" };
  const code = a.str(o.code, `${field}.code`, 40);
  const number = a.int(o.number, `${field}.number`);
  // A code this SDK doesn't know may be a later protocol version's: shown as such. A known code must carry its own number.
  // (`hasOwn`: a code named `constructor` or `__proto__` is no code of §8.1's table, and is shown as one this SDK doesn't know.)
  if (Object.hasOwn(OUTCOME_NUMBERS, code) && OUTCOME_NUMBERS[code] !== number) a.fail(`${field}.number`, `isn't ${code}'s (${number}, not ${OUTCOME_NUMBERS[code]})`);
  const out: ProgramOutcome = { ok: false, number, code };
  const numbered = (x: unknown, f: string) => {
    const n = a.obj(x, f);
    return { number: a.int(n.number, `${f}.number`), name: a.str(n.name, `${f}.name`, 64) };
  };
  if (o.program !== undefined) out.program = a.address(o.program, `${field}.program`);
  if (o.trap !== undefined) out.trap = numbered(o.trap, `${field}.trap`);
  if (o.abort_code !== undefined) out.abort_code = a.int(o.abort_code, `${field}.abort_code`);
  if (o.untrusted_message !== undefined) out.untrusted_message = a.str(o.untrusted_message, `${field}.untrusted_message`, 1024);
  if (o.commit_error !== undefined) out.commit_error = a.int(o.commit_error, `${field}.commit_error`);
  if (o.cap !== undefined) out.cap = a.amount(o.cap, `${field}.cap`);
  if (o.control !== undefined) out.control = a.str(o.control, `${field}.control`, 40);
  if (o.rule !== undefined) out.rule = numbered(o.rule, `${field}.rule`);
  if (o.func !== undefined) out.func = a.int(o.func, `${field}.func`);
  if (o.offset !== undefined) out.offset = a.int(o.offset, `${field}.offset`);
  return out;
}

/** A number of an outcome, for a sentence: itself when it's a whole number (an outcome that wasn't checked may hold anything), else `?`. */
const figure = (v: unknown): string => (typeof v === "number" && Number.isSafeInteger(v) ? String(v) : "?");

/** A name from an outcome (a trap's or a rule's), for a sentence: stripped of hidden characters and cut, `?` when it isn't text. */
const outcomeName = (v: unknown): string => (typeof v === "string" ? [...plainText(v)].slice(0, 40).join("").replace(/_/g, " ") : "?");

/**
 * A failed outcome in words, from its numbers and the chain's own names for them (docs/PROGRAMS-V0.md
 * §8.1): never engine text, and **never what the program wrote**: an abort's own message stays in
 * `outcome.untrusted_message`, for the caller to show as data. These words end up in exceptions that
 * agent frameworks hand to models, and a stranger's text doesn't belong in them. Tolerant of an
 * outcome that wasn't checked (a field of another type reads as `?`): it never throws on a node's
 * odd answer. `storageLimit` is the limit the failed transaction signed, drops: a call that traps
 * `storage_limit` was refused its writes' deposit by it (the default for a call is 0, which consents
 * to no new storage), and says so.
 */
export function outcomeWords(o: ProgramOutcome, storageLimit?: bigint): string {
  if (o === null || typeof o !== "object") return "it failed, and the node's outcome for it can't be read";
  const program = typeof o.program === "string" && isAddr(o.program) ? shortAddress(o.program) : "?";
  switch (o.code) {
    case "ok": return "it applied";
    case "trapped": {
      const hint = o.trap?.name === "storage_limit" ? `: the call's writes bond a deposit, and its storage limit (${storageLimit === undefined ? "0 unless given" : `${storageLimit} drops`}) was too small for it: pass a larger \`storageLimit\` for any method that writes` : "";
      return `program ${program} trapped: ${outcomeName(o.trap?.name)} (trap ${figure(o.trap?.number)})${hint}`;
    }
    case "aborted": return `program ${program} aborted with code ${figure(o.abort_code)}${o.untrusted_message ? " (its message is in the receipt's untrusted_message)" : ""}`;
    case "commit": return `the changes failed the chain's own commit checks (commit error ${figure(o.commit_error)})`;
    case "value_cap": return `programs would hold more than the value cap (${typeof o.cap === "string" && /^\d{1,20}$/.test(o.cap) ? o.cap : "?"} drops)`;
    case "exit_only": return "programs are exit-only: a call may not put more FANG under programs' control";
    case "storage_limit": return "the deposits passed the transaction's storage limit";
    case "state_full": return "program state is full";
    case "invalid_code": return `the code was refused: validation rule ${figure(o.rule?.number)} (${outcomeName(o.rule?.name)})`;
    case "translation_refused": return "the engine refused to translate the code";
    case "interface_mismatch": return "the interface's methods aren't the code's exports";
    case "native": return "a native check failed (funds, locks or the creation fee)";
    default: return `it failed with code ${shown(o.code)} (number ${figure(o.number)}), which this SDK doesn't know`;
  }
}

// ---- what the node says about a view ----

/** What a `View` method returned (`POST /api/program/{address}/view`), checked. */
export interface ProgramViewResult {
  ok: boolean;
  outcome: ProgramOutcome;
  /**
   * What the method returned, lowercase hex: `""` when it returns nothing, `null` when it failed.
   * It's the program's own claim: the chain checks none of it.
   */
  return_data: string | null;
  fuel_used: number;
  /** The newest block this node executed: the view read the state after it, and ran as of the block after. */
  height: number;
}

export function checkView(rpc: string, address: string, raw: unknown, clock: (height: number) => void = () => {}): ProgramViewResult {
  const a = new Answer(rpc, `POST /api/program/${address}/view`);
  const o = a.obj(raw, "answer");
  const ok = a.bool(o.ok, "ok");
  const outcome = checkOutcome(a, o.outcome, "outcome");
  if (outcome.ok !== ok) a.fail("outcome", "disagrees with ok");
  const return_data = ok ? a.hex(o.return_data, "return_data", 4 * 1024) : null;
  if (!ok && o.return_data !== null && o.return_data !== undefined) a.fail("return_data", "is there for a view that failed");
  const height = a.int(o.height, "height");
  clock(height);
  return { ok, outcome, return_data, fuel_used: a.int(o.fuel_used, "fuel_used"), height };
}

// ---- fuel and what it costs ----

/** The chain's fuel prices and program bounds now (`/api/fees`' `fuel` section), checked. Rates are milli-drops per fuel. */
export interface ProgramFuel {
  /** `on`; `exit_only` (switched off by governance: only calls that send no FANG); `paused` (nothing but a pause). */
  status: "on" | "exit_only" | "paused";
  /** What the next block charges. */
  rate: bigint;
  floor_rate: bigint;
  /** The most the rate can reach in the next 17 blocks of full fuel: a fee priced with it covers any of them. */
  suggested_rate: bigint;
  max_tx_fuel: bigint;
  max_block_fuel: bigint;
  max_tx_code_bytes: number;
  /** Program state's accounted bytes, and the soft cap its price curve doubles around. */
  state_bytes: bigint;
  state_soft_bytes: bigint;
  /** What a byte of storage costs now, drops; `null` past the ceiling. */
  storage_price: bigint | null;
  paused_until: number | null;
}

/** `/api/fees`' `fuel` section: the chain's, or `ProgramsOffError` when it has none (programs never switched on). */
export function checkFuel(rpc: string, raw: unknown): ProgramFuel {
  if (raw === undefined || raw === null) throw new ProgramsOffError(rpc);
  const a = new Answer(rpc, "GET /api/fees");
  const o = a.obj(raw, "fuel");
  const status = o.status;
  if (status !== "on" && status !== "exit_only" && status !== "paused") a.fail("fuel.status", `isn't on, exit_only or paused (${shown(status)})`);
  // The SDK prices with schedule 0's numbers: another schedule, or other numbers under that name, aren't priced here.
  const schedule = a.int(o.schedule, "fuel.schedule");
  if (schedule !== SCHEDULE_V0.schedule) a.fail("fuel.schedule", `is ${schedule}, a fuel schedule this SDK doesn't know: update @blockula/sdk`);
  for (const [key, want] of [["load_base", SCHEDULE_V0.load_base], ["load_per_byte", SCHEDULE_V0.load_per_byte], ["upload_base", SCHEDULE_V0.upload_base], ["upload_per_byte", SCHEDULE_V0.upload_per_byte]] as const) {
    if (BigInt(a.int(o[key], `fuel.${key}`)) !== want) a.fail(`fuel.${key}`, `is ${o[key]}, not schedule 0's ${want}`);
  }
  const rate = BigInt(a.int(o.rate, "fuel.rate"));
  const floor_rate = BigInt(a.int(o.floor_rate, "fuel.floor_rate"));
  const suggested_rate = BigInt(a.int(o.suggested_rate, "fuel.suggested_rate"));
  if (suggested_rate < rate || rate < floor_rate) a.fail("fuel.suggested_rate", "isn't at least the rate, which isn't at least the floor");
  return {
    status: status as ProgramFuel["status"],
    rate,
    floor_rate,
    suggested_rate,
    max_tx_fuel: BigInt(a.int(o.max_tx_fuel, "fuel.max_tx_fuel")),
    max_block_fuel: BigInt(a.int(o.max_block_fuel, "fuel.max_block_fuel")),
    max_tx_code_bytes: a.int(o.max_tx_code_bytes, "fuel.max_tx_code_bytes"),
    state_bytes: BigInt(a.int(o.state_bytes, "fuel.state_bytes")),
    state_soft_bytes: BigInt(a.int(o.state_soft_bytes, "fuel.state_soft_bytes")),
    storage_price: o.storage_price === null ? null : BigInt(a.amount(o.storage_price, "fuel.storage_price")),
    paused_until: o.paused_until === null || o.paused_until === undefined ? null : a.int(o.paused_until, "fuel.paused_until"),
  };
}

/** The programs parameters a client prices with (`/api/params`' `programs.params`), checked. */
export interface ProgramParams {
  /** Drops per accounted byte at an empty state (`program_storage_price`): the price curve doubles it. */
  storage_price: bigint;
  /** `program_min_upgrade_delay_blocks`: the notice an upgrade authority must give. */
  min_upgrade_delay_blocks: number;
}

export function checkParams(rpc: string, raw: unknown): ProgramParams {
  if (raw === undefined || raw === null) throw new ProgramsOffError(rpc);
  const a = new Answer(rpc, "GET /api/params");
  const params = a.obj(a.obj(raw, "programs").params, "programs.params");
  const value = (key: string): string => a.amount(a.obj(params[key], `programs.params.${key}`).value, `programs.params.${key}.value`);
  const storage_price = BigInt(value("program_storage_price"));
  if (storage_price === 0n) a.fail("programs.params.program_storage_price.value", "is 0: a deposit of nothing");
  const delay = BigInt(value("program_min_upgrade_delay_blocks"));
  if (delay > BigInt(Number.MAX_SAFE_INTEGER)) a.fail("programs.params.program_min_upgrade_delay_blocks.value", "is past 2^53 − 1");
  return { storage_price, min_upgrade_delay_blocks: Number(delay) };
}

/** The fuel to load one distinct code of `size` bytes (schedule 0: `load_base + load_per_byte × size`, §5.1). */
export const loadFuel = (size: number | bigint): bigint => SCHEDULE_V0.load_base + SCHEDULE_V0.load_per_byte * BigInt(size);

/** The fuel an upload of `code` bytes of WebAssembly and `iface` bytes of interface reserves (§5.1's `upload_fuel`: its whole `exec`). */
export const uploadFuel = (code: number | bigint, iface: number | bigint): bigint => SCHEDULE_V0.upload_base + SCHEDULE_V0.upload_per_byte * (BigInt(code) + BigInt(iface));

/** The fuel fee of `reserved` fuel at `rate` milli-drops per fuel: `rate × reserved / 1000`, rounded down (§5.1). Burned, never refunded. */
export const fuelFee = (rate: bigint, reserved: bigint): bigint => (rate * reserved) / FUEL_RATE_SCALE;

/** What a transaction reserves: its fuel to run, the load of the distinct code it declares, and the two together. */
export interface FuelReservation {
  run: bigint;
  load: bigint;
  reserved: bigint;
}

/** `run + load`, refused (`FuelReservationError`) when no block could carry it: past `max_block_fuel`, which admission holds every transaction to. */
function checkBlockFuel(fuel: ProgramFuel, run: bigint, load: bigint): bigint {
  const reserved = run + load;
  if (reserved > fuel.max_block_fuel) throw new FuelReservationError(reserved, run, load, fuel.max_block_fuel);
  return reserved;
}

/**
 * Whether a block could carry `run` fuel and the load of the `declared` code (each distinct code
 * hash once, with its size): refused as admission refuses it, before anything is signed. A
 * reservation past `max_block_fuel` is a `FuelReservationError`; code past what one transaction may
 * load, or a fuel limit past `max_tx_fuel` (`execFuelLimited`: a create's or a call's own limit, not
 * an upload's), is an `Error` that says so.
 */
export function reserveFuel(fuel: ProgramFuel, run: bigint, declared: Map<string, number>, execFuelLimited = true): FuelReservation {
  if (execFuelLimited && run > fuel.max_tx_fuel) throw new Error(`a fuel limit of ${run} is above program_max_tx_fuel (${fuel.max_tx_fuel}). Nothing was signed`);
  let bytes = 0;
  let load = 0n;
  for (const size of declared.values()) {
    bytes += size;
    load += loadFuel(size);
  }
  if (bytes > fuel.max_tx_code_bytes) throw new Error(`the code this transaction declares is ${bytes} bytes, past the ${fuel.max_tx_code_bytes} one transaction may load: bind fewer or smaller programs. Nothing was signed`);
  return { run, load, reserved: checkBlockFuel(fuel, run, load) };
}

/**
 * The fuel limit a call or an initializer is signed with when the caller gives none (docs/PROGRAMS-V0.md
 * §8.4): `min(program_max_tx_fuel, program_max_block_fuel − load)`, the most one transaction may run
 * that a block can still carry beside the load of the `declared` code. With the parameters at their
 * defaults that's `program_max_tx_fuel`; with `program_max_tx_fuel` = `program_max_block_fuel` (both
 * legal) a bare `program_max_tx_fuel` would itself trip the refusal of a reservation past the block.
 * When the load alone fills the block there is no room: 1, which `reserveFuel` then refuses in words.
 */
export function defaultFuelLimit(fuel: ProgramFuel, declared: Map<string, number>): bigint {
  let load = 0n;
  for (const size of declared.values()) load += loadFuel(size);
  const room = fuel.max_block_fuel - load;
  if (room < 1n) return 1n;
  return fuel.max_tx_fuel < room ? fuel.max_tx_fuel : room;
}

/**
 * The fuel fee of a reservation (drops) at the rate the next 17 blocks can reach (`suggested_rate`:
 * what a transaction's `fee` is priced at, §5.5) and at the floor rate (what the tip cap is counted
 * on: a block's rate is never below it), after holding it to `max_block_fuel` once more against
 * these very prices. Burned, and never refunded: a transaction pays for what it reserves.
 */
export function priceFuel(fuel: ProgramFuel, r: FuelReservation): { suggested: bigint; floor: bigint } {
  const reserved = checkBlockFuel(fuel, r.run, r.load);
  return { suggested: fuelFee(fuel.suggested_rate, reserved), floor: fuelFee(fuel.floor_rate, reserved) };
}

/**
 * Whether the chain's programs status admits this kind of transaction now: `on` admits all;
 * `exit_only` only a call that sends no FANG (an exit); `paused` none. The node refuses the rest at
 * submission, so this only says so before a signature (`sendsValue`: the transaction attaches FANG).
 */
export function checkProgramsStatus(fuel: ProgramFuel, kind: "upload" | "create" | "call", sendsValue: boolean): void {
  if (fuel.status === "on") return;
  if (fuel.status === "paused") throw new Error(`programs are paused until block ${fuel.paused_until ?? "?"}: the chain refuses every program transaction until then. Nothing was signed`);
  if (kind !== "call" || sendsValue) throw new Error(`programs are exit-only (governance switched them off): the chain refuses ${kind === "call" ? "a call that sends FANG" : `an ${kind === "upload" ? "upload" : "create"}`}. Nothing was signed`);
}

/**
 * The price of a byte of program state when it holds `used` bytes (node/vm/src/storage.rs `price`):
 * `base × 2^⌊8 × used / soft⌋`. `null` past the ceiling (`2 × soft`) or if the price wouldn't fit a u64.
 */
export function programStoragePrice(base: bigint, soft: bigint, used: bigint): bigint | null {
  if (soft === 0n || used > 2n * soft) return null;
  const price = base << ((8n * used) / soft);
  return price >= 1n << 64n ? null : price;
}

/**
 * What an upload bonds (node/src/program_cli.rs `Bounds::upload_deposit`, §2.2 step 5): each blob that
 * isn't stored yet, code first, at the price once it lands. `null` past program state's ceiling.
 */
export function programUploadDeposit(fuel: ProgramFuel, params: ProgramParams, blobs: { size: number; isNew: boolean }[]): bigint | null {
  let used = fuel.state_bytes;
  let total = 0n;
  for (const { size, isNew } of blobs) {
    if (!isNew) continue;
    used += BigInt(size);
    const price = programStoragePrice(params.storage_price, fuel.state_soft_bytes, used);
    if (price === null) return null;
    total += price * BigInt(size);
  }
  return total;
}

/** What a create bonds for its record (§2.3 step 2): `PROGRAM_RECORD_BYTES` at the price once it lands. `null` past the ceiling. */
export function programRecordDeposit(fuel: ProgramFuel, params: ProgramParams): bigint | null {
  const price = programStoragePrice(params.storage_price, fuel.state_soft_bytes, fuel.state_bytes + PROGRAM_RECORD_BYTES);
  return price === null ? null : price * PROGRAM_RECORD_BYTES;
}

/** A deposit and ten percent more: the default `storage_limit` of an upload or a create (§8.3), room for the price to move. */
export const depositWithMargin = (deposit: bigint): bigint => deposit + deposit / 10n;

// ---- receipts ----

/** What a program transaction's fee was made of, drops (§8.1): their sum is what it was charged. */
export interface ReceiptFee {
  bytes: bigint;
  fuel: bigint;
  tip: bigint;
}

/** What a signer was debited outside the VM's own balance changes (§8.1), drops: `total` is the three parts' sum. */
export interface OutsideVm {
  record_deposit: bigint;
  upload_deposit: bigint;
  account_creation_fee: bigint;
  total: bigint;
}

/** The facts of a program transaction: what the chain did, which the state holds too. */
export interface ProgramFacts {
  /** The VM's diff, net per account, drops (negative: a debit): a program's own sends and the signer's write bonds. Not the fee, not `outside_vm`. */
  balance_changes: { account: string; change: bigint }[];
  storage: { bytes_delta: number; bonded: bigint; refunded: bigint };
  outside_vm: OutsideVm;
  /** Drops the VM's diff burned: the creation fees of the accounts a program's own sends opened. Taken from the programs' balances inside the diff, never the signer's. */
  burned_by_programs: bigint;
  fuel: { limit: bigint; load: bigint; used: bigint; reserved: bigint; rate: bigint };
  fee: ReceiptFee;
  /** The program a create made. */
  created: string | null;
  upload: { wasm_hash: string; interface_hash: string; new_code: boolean; new_interface: boolean; deposit: bigint } | null;
}

/** What the programs said happened: claims, which the chain checks nothing of. Method and event names are the deployer's. */
export interface ProgramClaims {
  return_data: { program: string; untrusted_method: string; value: bigint; data: string }[];
  events: { program: string; untrusted_name: string; data: string }[];
  /** Set when the block's claims budget left this receipt's claims out (its facts are all kept). */
  omitted: { events: number; event_bytes: number; return_bytes: number } | null;
}

/** A program transaction's receipt as `/api/tx/{id}` serves it, checked: facts apart from claims. */
export interface ProgramReceipt {
  version: number;
  kind: "upload" | "create" | "call";
  outcome: ProgramOutcome;
  facts: ProgramFacts;
  claims: ProgramClaims;
}

/**
 * The `program` section of a `/api/tx` answer, checked: `null` for a program transaction with no
 * receipt to show (still pending, or its block pruned from this node). Every amount is a decimal
 * string, every digest 64 lowercase hex, and the sums the node states are recomputed: the fee parts
 * equal the transaction's `fee`, `outside_vm.total` is its parts' sum, `reserved` is `limit + load`.
 * A transaction that isn't a program's carries no `program` key, and `ProgramAnswerError` says so.
 */
export function checkReceipt(rpc: string, tx: unknown): ProgramReceipt | null {
  const a = new Answer(rpc, "GET /api/tx/{id}");
  const t = a.obj(tx, "answer");
  if (!("program" in t)) a.fail("program", "is missing: that isn't a program transaction");
  if (t.program === null) return null;
  const p = a.obj(t.program, "program");
  const kind = p.kind;
  if (kind !== "upload" && kind !== "create" && kind !== "call") a.fail("program.kind", `isn't upload, create or call (${shown(kind)})`);
  const version = a.int(p.version, "program.version");
  const outcome = checkOutcome(a, p.outcome, "program.outcome");
  const facts = a.obj(p.facts, "program.facts");
  const big = (v: unknown, f: string) => BigInt(a.amount(v, f));
  const fee = a.obj(facts.fee, "program.facts.fee");
  const parts: ReceiptFee = { bytes: big(fee.bytes, "program.facts.fee.bytes"), fuel: big(fee.fuel, "program.facts.fee.fuel"), tip: big(fee.tip, "program.facts.fee.tip") };
  if (t.fee !== undefined && BigInt(a.amount(t.fee, "fee")) !== parts.bytes + parts.fuel + parts.tip) a.fail("fee", "isn't the sum of the receipt's fee parts");
  const out = a.obj(facts.outside_vm, "program.facts.outside_vm");
  const outside: OutsideVm = { record_deposit: big(out.record_deposit, "outside_vm.record_deposit"), upload_deposit: big(out.upload_deposit, "outside_vm.upload_deposit"), account_creation_fee: big(out.account_creation_fee, "outside_vm.account_creation_fee"), total: big(out.total, "outside_vm.total") };
  if (outside.total !== outside.record_deposit + outside.upload_deposit + outside.account_creation_fee) a.fail("program.facts.outside_vm.total", "isn't the sum of its parts");
  const f = a.obj(facts.fuel, "program.facts.fuel");
  const fuel = { limit: BigInt(a.int(f.limit, "fuel.limit")), load: BigInt(a.int(f.load, "fuel.load")), used: BigInt(a.int(f.used, "fuel.used")), reserved: BigInt(a.int(f.reserved, "fuel.reserved")), rate: BigInt(a.int(f.rate, "fuel.rate")) };
  if (fuel.reserved !== fuel.limit + fuel.load) a.fail("program.facts.fuel.reserved", "isn't the fuel limit and the load together");
  const s = a.obj(facts.storage, "program.facts.storage");
  if (typeof s.bytes_delta !== "number" || !Number.isSafeInteger(s.bytes_delta)) a.fail("program.facts.storage.bytes_delta", `isn't a whole number (${shown(s.bytes_delta)})`);
  const burned = big(facts.burned_by_programs, "program.facts.burned_by_programs");
  const changes = a.arr(facts.balance_changes, "program.facts.balance_changes").map((c, i) => {
    const o = a.obj(c, `balance_changes[${i}]`);
    return { account: a.address(o.account, `balance_changes[${i}].account`), change: BigInt(a.change(o.change, `balance_changes[${i}].change`)) };
  });
  if (new Set(changes.map((c) => c.account)).size !== changes.length) a.fail("program.facts.balance_changes", "lists an account twice");
  let upload: ProgramFacts["upload"] = null;
  if (facts.upload !== null && facts.upload !== undefined) {
    const u = a.obj(facts.upload, "program.facts.upload");
    upload = { wasm_hash: a.digest(u.wasm_hash, "upload.wasm_hash"), interface_hash: a.digest(u.interface_hash, "upload.interface_hash"), new_code: a.bool(u.new_code, "upload.new_code"), new_interface: a.bool(u.new_interface, "upload.new_interface"), deposit: big(u.deposit, "upload.deposit") };
  }
  const storage = { bytes_delta: s.bytes_delta as number, bonded: big(s.bonded, "storage.bonded"), refunded: big(s.refunded, "storage.refunded") };
  const gap = conservationGap({ balance_changes: changes, storage, outside_vm: outside, burned_by_programs: burned });
  if (gap !== 0n) a.fail("program.facts", `don't conserve FANG: the balance changes, what was bonded and refunded and what the programs burned are ${gap} drops from adding up to nothing`);
  const claims = a.obj(p.claims, "program.claims");
  const omitted = claims.omitted === null || claims.omitted === undefined ? null : (() => {
    const o = a.obj(claims.omitted, "claims.omitted");
    return { events: a.int(o.events, "omitted.events"), event_bytes: a.int(o.event_bytes, "omitted.event_bytes"), return_bytes: a.int(o.return_bytes, "omitted.return_bytes") };
  })();
  return {
    version,
    kind: kind as ProgramReceipt["kind"],
    outcome,
    facts: {
      balance_changes: changes,
      storage,
      outside_vm: outside,
      burned_by_programs: burned,
      fuel,
      fee: parts,
      created: facts.created === null || facts.created === undefined ? null : a.address(facts.created, "program.facts.created"),
      upload,
    },
    claims: {
      return_data: a.arr(claims.return_data, "claims.return_data", 64).map((r, i) => {
        const o = a.obj(r, `return_data[${i}]`);
        return { program: a.address(o.program, `return_data[${i}].program`), untrusted_method: a.str(o.untrusted_method, `return_data[${i}].untrusted_method`, 64), value: big(o.value, `return_data[${i}].value`), data: a.hex(o.data, `return_data[${i}].data`, 4 * 1024) };
      }),
      events: a.arr(claims.events, "claims.events", 64).map((e, i) => {
        const o = a.obj(e, `events[${i}]`);
        return { program: a.address(o.program, `events[${i}].program`), untrusted_name: a.str(o.untrusted_name, `events[${i}].untrusted_name`, 64), data: a.hex(o.data, `events[${i}].data`, 16 * 1024) };
      }),
      omitted,
    },
  };
}

/** What a program transaction's fee came to, drops: bytes, fuel and tip (§8.1's `Σ`). */
export const chargedFee = (r: ProgramReceipt): bigint => r.facts.fee.bytes + r.facts.fee.fuel + r.facts.fee.tip;

/** What the VM's diff did to `account`'s balance, drops (0 for an account it doesn't list). */
export const vmChange = (r: ProgramReceipt, account: string): bigint => r.facts.balance_changes.find((c) => c.account === account)?.change ?? 0n;

/**
 * How far a receipt's facts are from conserving FANG (docs/PROGRAMS-V0.md §8.1): Σ `balance_changes` +
 * (`storage.bonded` − `outside_vm.record_deposit` − `outside_vm.upload_deposit` − `storage.refunded`) +
 * `burned_by_programs`. The VM's diff conserves (what its programs moved and the deposits it bonded
 * or freed, plus what it burned, sum to nothing), and a create's record deposit and an upload's are
 * bonded outside it: so a receipt the chain wrote is 0 here, and `checkReceipt` refuses one that
 * isn't. (The account creation fee a create burns from its signer is the signer's, not a program's:
 * it's in `outside_vm`, not here.)
 */
export const conservationGap = (f: Pick<ProgramFacts, "balance_changes" | "storage" | "outside_vm" | "burned_by_programs">): bigint =>
  f.balance_changes.reduce((sum, c) => sum + c.change, 0n) + (f.storage.bonded - f.outside_vm.record_deposit - f.outside_vm.upload_deposit - f.storage.refunded) + f.burned_by_programs;

/**
 * How the signer's balance moved, exactly (docs/PROGRAMS-V0.md §8.1, S6B-R1): Δbalance(signer) =
 * −(fee total + `outside_vm.total`) + `balance_changes[signer]`. The fee is burned (and the tip goes
 * to the proposer), the deposits and the account creation fee leave outside the VM, and the VM's diff
 * holds what the programs moved, the signer's own write bonds among it. A failed transaction's receipt
 * has no balance changes and nothing outside the VM: only the fee.
 */
export const signerChange = (r: ProgramReceipt, signer: string): bigint => vmChange(r, signer) - chargedFee(r) - r.facts.outside_vm.total;

/** `reconcileSigner`'s answer: what the receipt says the signer's balance did, what it did, and whether they're the same. */
export interface Reconciliation {
  expected: bigint;
  actual: bigint;
  /** `actual − expected`: 0 when the balance moved exactly as the receipt says (nothing else touched the account between the two reads). */
  difference: bigint;
  exact: boolean;
  /** Whether the receipt's own facts conserve FANG (`conservationGap` is 0): a receipt that doesn't can't be what the chain wrote. */
  conserved: boolean;
}

/**
 * The signer's balance before and after its program transaction (both in drops, read around it with
 * nothing else touching the account), held to `signerChange`.
 */
export function reconcileSigner(r: ProgramReceipt, signer: string, before: bigint, after: bigint): Reconciliation {
  const expected = signerChange(r, signer);
  const actual = after - before;
  return { expected, actual, difference: actual - expected, exact: actual === expected, conserved: conservationGap(r.facts) === 0n };
}

/** What a program transaction cost its signer, drops, from its receipt: the fee, what left outside the VM, what the VM took (the write bonds), and all of it. */
export interface ProgramCost {
  /** Bytes, fuel and tip. */
  fee: bigint;
  /** Deposits and the account creation fee that left outside the VM's diff. */
  outside_vm: OutsideVm;
  /** What the VM's diff took from the signer (positive: a debit): its write bonds, and FANG it sent. 0 or less when the programs paid it. */
  vm: bigint;
  /** `fee + outside_vm.total + vm`: the signer's whole debit. */
  total: bigint;
}

export function programCost(r: ProgramReceipt, signer: string): ProgramCost {
  const fee = chargedFee(r);
  const vm = -vmChange(r, signer);
  return { fee, outside_vm: r.facts.outside_vm, vm, total: fee + r.facts.outside_vm.total + vm };
}
