/**
 * Calling a deployed program from the wallet (docs/PROGRAMS.md 11.3, docs/EXPLORER.md): the methods of
 * its interface as specs, `ActionSpec`s for its `call` methods and read forms for its `view` methods,
 * with typed fields for their arguments; what the fields hold, encoded by the SDK's ABI codec (Borsh by
 * convention, docs/PROGRAMS-V0.md §3.2: `encodeProgramInput`); and what a method returned, decoded by its
 * declared type and shown as the program's claim, never as markup or as the wallet's own words.
 *
 * Specs are made only from a record the SDK checked (`client.program`: every method name an identifier,
 * every selector its name's, every type in the interface's vocabulary, the version digest recomputed).
 * Fields are keyed by position (`arg0`, `arg1`, …), never by a name the deployer chose. Addresses are
 * `boo1…` only: a program reads 32 bytes, and a `.boo` name moves with its owner.
 *
 * Pure (the SDK, and modules with only type imports), so `node --test` runs it (lib/program-calls.test.mjs).
 */
import { encodeProgramInput, encodeProgramValue, decodeProgramInput, decodeProgramValue, parseFang, ProgramValueError, type Action, type ProgramInfo, type ProgramType, type ProgramValue } from "@blockula/sdk";
import { inertText } from "./inert.ts";
import { formatDrops } from "./ledger.ts";
import type { ActionSpec, Param, ParamType, Values } from "./programs";

/** An argument of a method, as the checked interface has it: the deployer's name for it, and its type. */
export interface ProgramArg {
  untrusted_name: string;
  type: ProgramType;
}

/** A field for an argument: a `Param`, with the interface type its value is encoded as. */
export type ProgramParam = Param & { abi: ProgramType };

/** What a method's spec shares, a call's or a view's. */
interface MethodSpecBase {
  /** The program's address. */
  program: string;
  /** The method's identifier: the deployer's, checked to be an identifier and to have its selector. */
  method: string;
  /** One field per argument, in order, keyed `arg0`, `arg1`, …; each carries its interface type in `abi`. */
  params: ProgramParam[];
  args: ProgramArg[];
  returns: ProgramType | null;
}

/**
 * A `call` method as an `ActionSpec` (11.3's `specFromInterface`): it sends `call_program`, only owner keys
 * sign it, and `build` makes the call from what `checkCall` read from its fields (`input`, `value` and
 * `storage_limit`, drops), bound to `versionDigest`, the digest of the record the page shows. Its
 * `fuel_limit` is the SDK's to fill (`Wallet.call`, 11.3's `prepare`): fuel is priced from the node's
 * prices, which a synchronous `build` doesn't have, so a spec's own action leaves it at 0.
 */
export interface ProgramCallSpec extends Omit<ActionSpec, "params" | "kind">, MethodSpecBase {
  kind: "call_program";
  mode: "call";
  payable: boolean;
  versionDigest: string;
}

/** A `view` method: a read form, run on the node (`client.view`), which signs and pays nothing. */
export interface ProgramViewSpec extends MethodSpecBase {
  mode: "view";
}

const INTEGER_BITS: Record<string, { bits: number; signed: boolean }> = { u8: { bits: 8, signed: false }, u16: { bits: 16, signed: false }, u32: { bits: 32, signed: false }, u64: { bits: 64, signed: false }, u128: { bits: 128, signed: false }, i64: { bits: 64, signed: true }, i128: { bits: 128, signed: true } };

const isOption = (t: ProgramType): t is { option: ProgramType } => typeof t === "object" && "option" in t;
const isVec = (t: ProgramType): t is { vec: [ProgramType, number] } => typeof t === "object" && "vec" in t;
const isLeaf = (t: ProgramType) => !isOption(t) && !isVec(t);

/** The field a leaf type is typed into (11.3's table; `address` takes addresses only, never names). */
export function paramTypeOf(t: ProgramType): ParamType {
  if (typeof t === "string") {
    if (t === "u8" || t === "u16" || t === "u32") return "int";
    if (t === "u64" || t === "i64" || t === "u128" || t === "i128" || t === "fang" || t === "bool" || t === "address" || t === "name") return t;
    if (t === "hash32") return "hex";
  } else if ("bytes" in t) return "hex";
  else if ("text" in t) return "text";
  return "json";
}

/**
 * The field for an argument of type `t`: a leaf is one field; an option of a leaf an optional one
 * (empty for none); a vec of leaves a list of them, up to its bound; anything deeper one JSON field the
 * codec holds to the type (11.3's fallback).
 */
function paramOf(arg: ProgramArg, i: number): ProgramParam {
  const base = { key: `arg${i}`, label: inertText(arg.untrusted_name, 32), abi: arg.type, help: typeHint(arg.type) };
  const t = arg.type;
  const bound = (x: ProgramType) => (typeof x === "object" && "text" in x ? x.text : typeof x === "object" && "bytes" in x ? x.bytes : x === "hash32" ? 32 : undefined);
  if (isLeaf(t)) return { ...base, type: paramTypeOf(t), max: bound(t) };
  if (isOption(t) && isLeaf(t.option)) return { ...base, type: paramTypeOf(t.option), optional: true, max: bound(t.option) };
  if (isVec(t) && isLeaf(t.vec[0])) return { ...base, type: "list", of: paramTypeOf(t.vec[0]), max: t.vec[1] };
  return { ...base, type: "json" };
}

/** The type a field's items (a list's) or its value (an optional field's) are of. */
export const leafOf = (p: { abi: ProgramType }): ProgramType => (isOption(p.abi) ? p.abi.option : isVec(p.abi) ? p.abi.vec[0] : p.abi);

const range = (t: string) => {
  const { bits, signed } = INTEGER_BITS[t];
  const max = signed ? (1n << BigInt(bits - 1)) - 1n : (1n << BigInt(bits)) - 1n;
  return `${signed ? `${(-max - 1n).toLocaleString("en-US")}` : "0"} to ${max.toLocaleString("en-US")}`;
};

/** What a field of type `t` takes, in plain words (for its help line). */
export function typeHint(t: ProgramType, prefix = "boo"): string {
  if (typeof t === "string") {
    if (t in INTEGER_BITS) return `A whole number, ${range(t)} (${t}).`;
    if (t === "fang") return "An amount of FANG, like 1.5 (up to 9 decimals).";
    if (t === "bool") return "Yes or no (bool).";
    if (t === "address") return `An address (${prefix}1…), never a .boo name: a program keeps the address, and a name moves with its owner.`;
    if (t === "name") return "A .boo name's label, like alice: the program gets the name, and decides itself what it means.";
    if (t === "hash32") return "32 bytes, as 64 hex characters (hash32).";
    return "?";
  }
  if ("bytes" in t) return `Bytes as hex, at most ${t.bytes.toLocaleString("en-US")} bytes (${t.bytes * 2} hex characters).`;
  if ("text" in t) return `Text, at most ${t.text.toLocaleString("en-US")} bytes of UTF-8 (an accented letter takes 2, an emoji 4).`;
  if (isOption(t) && isLeaf(t.option)) return `${typeHint(t.option, prefix).replace(/\.$/, "")}, or empty for none.`;
  if (isVec(t) && isLeaf(t.vec[0])) return `A list of up to ${t.vec[1].toLocaleString("en-US")}. Each: ${typeHint(t.vec[0], prefix).replace(/^./, (c) => c.toLowerCase())}`;
  return 'JSON for this type: whole numbers as strings ("5"), amounts of FANG as "1.5", addresses as strings, bytes as hex, none as null, some as {"some": …}, a list as [ … ].';
}

/** What a form holds while it's typed: a string per field, a list of strings for a list. */
export type Draft = Record<string, string | string[]>;

/** A spec's fields as an empty form (yes-or-no fields start at no; a call's value and storage limit at 0). */
export function initialDraft(spec: MethodSpecBase & { mode: "call" | "view"; payable?: boolean }): Draft {
  const d: Draft = {};
  for (const p of spec.params) d[p.key] = p.type === "list" ? [] : p.type === "bool" && !p.optional ? "false" : "";
  if (spec.mode === "call") {
    if (spec.payable) d.value = "";
    d.storage_limit = "0";
  }
  return d;
}

/** A field's problem that isn't the codec's (a required field left empty). */
class FieldError extends Error {}

/** A leaf as typed, as the codec takes it. Text is taken exactly as typed (it's signed byte for byte); the rest trimmed. */
function readLeaf(t: ProgramType, raw: string): unknown {
  if (typeof t === "object" && "text" in t) return raw;
  const v = raw.trim();
  if (!v) throw new FieldError("Required.");
  if (t === "bool") {
    if (v === "true" || v === "false") return v === "true";
    throw new FieldError("Yes or no.");
  }
  if (t === "hash32" || (typeof t === "object" && "bytes" in t)) return v.replace(/^0x/i, "").toLowerCase();
  return v;
}

/** What a field holds, as the codec takes it (`encodeProgramValue`), or a `FieldError`. */
function readField(p: ProgramParam, raw: string | string[] | undefined): unknown {
  const t = p.abi;
  if (p.type === "json") {
    const v = typeof raw === "string" ? raw.trim() : "";
    if (!v) throw new FieldError("Required: JSON for this type (null for none).");
    try {
      return JSON.parse(v);
    } catch {
      throw new FieldError("That isn't JSON.");
    }
  }
  if (p.type === "list") return (Array.isArray(raw) ? raw : []).map((item) => readLeaf(leafOf(p), item));
  const v = typeof raw === "string" ? raw : "";
  if (p.optional) return v === "" ? null : { some: readLeaf(leafOf(p), v) };
  return readLeaf(t, v);
}

const sentence = (s: string) => `${s.charAt(0).toUpperCase()}${s.slice(1)}${/[.!?]$/.test(s) ? "" : "."}`;

/** A codec error in a field's words: the item it's about (a list's), and the SDK's sentence. */
function problemOf(e: unknown): string {
  if (e instanceof FieldError) return e.message;
  if (e instanceof ProgramValueError) {
    const where = e.where.replace(/^the value,?\s*/, "");
    return sentence(where ? `${where}: ${e.problem}` : e.problem);
  }
  return sentence(e instanceof Error ? e.message : String(e));
}

/** Why a field's value isn't one of its type, or `null` when it is (checked by the codec itself, so a field shows what encoding would refuse). */
export function fieldProblem(p: ProgramParam, raw: string | string[] | undefined, prefix: string): string | null {
  try {
    encodeProgramValue(p.abi, readField(p, raw), { prefix });
    return null;
  } catch (e) {
    return problemOf(e);
  }
}

/** A form's input: every field read and encoded in order (`encodeProgramInput`), lowercase hex; throws on the first field that isn't one of its type, naming it by position. */
export function encodeDraft(spec: MethodSpecBase, draft: Draft, prefix: string): string {
  const values = spec.params.map((p, i) => {
    try {
      return readField(p, draft[p.key]);
    } catch (e) {
      throw new Error(`Argument ${i + 1} (${p.label}): ${problemOf(e)}`);
    }
  });
  try {
    return encodeProgramInput(spec.args, values, { prefix });
  } catch (e) {
    if (!(e instanceof ProgramValueError)) throw new Error(problemOf(e));
    // `argument 2, item 1`: named by position, with the field's label (the deployer's name for it, made inert).
    const where = e.where.replace(/^argument (\d+)/, (_, n: string) => `Argument ${n} (${spec.params[Number(n) - 1]?.label ?? "?"})`);
    throw new Error(sentence(`${where}: ${e.problem}`));
  }
}

/** An amount of FANG as typed in a call's own fields (its value, its storage limit): drops, or why not. */
export function fangField(raw: string | string[] | undefined, what: string): bigint {
  const v = typeof raw === "string" ? raw.trim() : "";
  if (!v) return 0n;
  if (!/^\d{1,11}(\.\d{1,9})?$/.test(v)) throw new Error(`${what}: an amount of FANG, like 1.5 (up to 9 decimals).`);
  return parseFang(v);
}

/** What a call's form says, read: its input (hex), the FANG it sends (only to a payable method) and its storage limit, drops. */
export function checkCall(spec: ProgramCallSpec, draft: Draft, prefix: string): { input: string; value: bigint; storageLimit: bigint } {
  const input = encodeDraft(spec, draft, prefix);
  const value = spec.payable ? fangField(draft.value, "Value") : 0n;
  const storageLimit = fangField(draft.storage_limit, "Storage limit");
  return { input, value, storageLimit };
}

/** A call's action as its spec makes it from `checkCall`'s answer (`Values`: `input` hex, `value` and `storage_limit` drops). */
function callAction(spec: Pick<ProgramCallSpec, "program" | "method" | "versionDigest">, v: Values): Action {
  return {
    type: "call_program",
    calls: [{ program: spec.program, version_digest: spec.versionDigest, method: spec.method, input: String(v.input ?? ""), value: String(v.value ?? "0") }],
    fuel_limit: 0,
    storage_limit: String(v.storage_limit ?? "0"),
    bindings: [],
  };
}

/**
 * Whether `signed` (the transaction the SDK built) is the call `spec` describes: the same program,
 * digest, method, input and value, the same storage limit and no bindings. Only `fuel_limit` may
 * differ (the SDK fills it). The review refuses a transaction that isn't.
 */
export function sameCall(signed: Action, spec: Action): boolean {
  if (signed.type !== "call_program" || spec.type !== "call_program") return false;
  if (signed.calls.length !== 1 || spec.calls.length !== 1 || signed.bindings.length !== 0 || spec.bindings.length !== 0) return false;
  const [a, b] = [signed.calls[0], spec.calls[0]];
  return a.program === b.program && a.version_digest === b.version_digest && a.method === b.method && a.input === b.input && String(a.value) === String(b.value) && String(signed.storage_limit) === String(spec.storage_limit);
}

/**
 * The specs of a checked program record (`client.program`): an `ActionSpec` for each `call` method,
 * bound to `versionDigest` (the digest of the record the page shows), and a read form for each `view`.
 * `init` methods run only when a program is created, so they get neither.
 */
export function specsFromProgram(info: Pick<ProgramInfo, "address" | "methods">, versionDigest: string): { calls: ProgramCallSpec[]; views: ProgramViewSpec[] } {
  const calls: ProgramCallSpec[] = [];
  const views: ProgramViewSpec[] = [];
  for (const m of info.methods ?? []) {
    const args = m.args.map((a) => ({ untrusted_name: a.untrusted_name, type: a.type }));
    const params = args.map(paramOf);
    const base = { program: info.address, method: m.untrusted_name, params, args, returns: m.returns };
    if (m.kind === "view") views.push({ ...base, mode: "view" });
    if (m.kind !== "call") continue;
    const spec: ProgramCallSpec = {
      ...base,
      mode: "call",
      id: `call:${m.untrusted_name}`,
      kind: "call_program",
      payable: m.payable,
      versionDigest,
      title: m.untrusted_name,
      blurb: "",
      who: `owner keys of a personal account${m.payable ? "; it takes FANG" : ""} (a session key signs transfers only, and a shared account can't propose a program call yet)`,
      ownerOnly: true,
      build: (v) => callAction(spec, v),
      describe: () => `Call ${m.untrusted_name} on program ${info.address}`,
    };
    calls.push(spec);
  }
  return { calls, views };
}

/** A spec's own action for what `checkCall` read. */
export const specAction = (spec: ProgramCallSpec, c: { input: string; value: bigint; storageLimit: bigint }): Action => spec.build({ input: c.input, value: c.value.toString(), storage_limit: c.storageLimit.toString() }, { height: 0, account: "" });

/** What a method returned (lowercase hex), decoded by its declared type: the value, or why the bytes aren't one. */
export function readReturn(type: ProgramType | null, hex: string, prefix: string): { ok: true; value: ProgramValue } | { ok: false; problem: string } {
  try {
    return { ok: true, value: decodeProgramValue(type, hex, { prefix }) };
  } catch (e) {
    return { ok: false, problem: problemOf(e) };
  }
}

/** A call's input decoded back by its arguments' types (what the review shows beside the hex), or why it can't be. */
export function readInput(args: readonly ProgramArg[], hex: string, prefix: string): { ok: true; values: ProgramValue[] } | { ok: false; problem: string } {
  try {
    return { ok: true, values: decodeProgramInput(args, hex, { prefix }) };
  } catch (e) {
    return { ok: false, problem: problemOf(e) };
  }
}

/** A value to show: a leaf's text (and whether it's an address or bytes, set in monospace; whether hidden characters were taken out), none, some, or a list. */
export type Shown = { leaf: { text: string; mono: boolean; altered: boolean } } | { none: true } | { some: Shown } | { items: Shown[] };

/** Characters that hide or reorder text: a program's text that has them is shown without them, and says so. */
const HIDDEN = /[\p{Cc}\p{Cf}\p{Co}\p{Cs}\p{Zl}\p{Zp}]/u;

/**
 * A decoded value (`decodeProgramValue`'s form) to show, by its type. Text is the program's: shown
 * through `inertText` (hidden characters out, cut to `max` characters), marked `altered` when that took
 * something out. Amounts are drops and their FANG; nothing is looked up or labeled as an account.
 */
export function shownValue(type: ProgramType, v: ProgramValue, unit = "FANG", max = 2000): Shown {
  const leaf = (text: string, mono = false, altered = false): Shown => ({ leaf: { text, mono, altered } });
  if (isOption(type)) return v === null ? { none: true } : { some: shownValue(type.option, (v as { some: ProgramValue }).some, unit, max) };
  if (isVec(type)) return { items: (v as ProgramValue[]).map((x) => shownValue(type.vec[0], x, unit, max)) };
  if (typeof type === "object" && "text" in type) {
    const s = String(v);
    return s === "" ? leaf("(empty text)") : leaf(inertText(s, max), false, HIDDEN.test(s) || [...s].length > max);
  }
  if (typeof type === "object" && "bytes" in type) return v === "" ? leaf("(no bytes)") : leaf(String(v), true);
  if (type === "fang") return leaf(`${formatDrops(v as bigint, unit)} (${(v as bigint).toString()} drops)`);
  if (type === "address" || type === "hash32") return leaf(String(v), true);
  return leaf(String(v));
}

/** A shown value on one line (for a list of arguments, a test, a copy): nested values in brackets. */
export function shownText(s: Shown): string {
  if ("leaf" in s) return s.leaf.text;
  if ("none" in s) return "none";
  if ("some" in s) return shownText(s.some);
  return `[${s.items.map(shownText).join(", ")}]`;
}
