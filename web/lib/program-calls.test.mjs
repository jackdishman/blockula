// lib/explorer/program-calls.ts: a deployed program's methods as forms. Specs come from a record the SDK
// checked (the recorded answers of a `blockula dev --programs` chain, sdk-ts/test/program-fixtures.json),
// fields encode with the SDK's ABI codec, whose bytes are the node's borsh crate's
// (sdk-ts/test/program-abi-vectors.json, written by node/src/programs/abi_vectors.rs), and what a method
// returned is decoded by its declared type and shown as text. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { checkProgram, checkReceipt, methodSelector } from "@blockula/sdk";
import { checkCall, encodeDraft, fieldProblem, initialDraft, readInput, readReturn, sameCall, shownText, shownValue, specAction, specsFromProgram, typeHint } from "./explorer/program-calls.ts";

const json = (rel) => JSON.parse(readFileSync(new URL(rel, import.meta.url), "utf8"));
const fx = json("../../sdk-ts/test/program-fixtures.json");
const abi = json("../../sdk-ts/test/program-abi-vectors.json");
const RPC = "http://fixture.test";
const escrow = checkProgram(RPC, fx.escrow.program.address, fx.escrow.program);
const counter = checkProgram(RPC, fx.counter.program.address, fx.counter.program);
const { bob } = fx.escrow.accounts;

/** A checked-looking record with the methods given (selectors computed, as `checkProgram` demands). */
const record = (methods) => ({ address: escrow.address, methods: methods.map((m) => ({ payable: false, returns: null, ...m, selector: methodSelector(m.untrusted_name) })) });

test("a checked record's call methods become ActionSpecs bound to the shown digest, its views read forms, its initializer neither", () => {
  const { calls, views } = specsFromProgram(escrow, escrow.version_digest);
  assert.deepEqual(calls.map((c) => c.method), ["open", "refund", "release"]);
  assert.deepEqual(views, []);
  const open = calls[0];
  assert.equal(open.kind, "call_program");
  assert.equal(open.ownerOnly, true, "a session key signs transfers only");
  assert.equal(open.payable, true);
  assert.equal(open.versionDigest, escrow.version_digest);
  assert.deepEqual(open.params.map((p) => [p.key, p.label, p.type]), [["arg0", "id", "u64"], ["arg1", "seller", "address"]]);
  const c = specsFromProgram(counter, counter.version_digest);
  assert.deepEqual(c.views.map((v) => [v.method, v.returns]), [["get", "u64"]]);
  assert.deepEqual(c.calls.map((v) => [v.method, v.returns]), [["increment", "u64"]]);
  const withInit = specsFromProgram(record([{ untrusted_name: "init", kind: "init", args: [] }, { untrusted_name: "peek", kind: "view", args: [], returns: "bool" }]), "ab".repeat(32));
  assert.deepEqual([withInit.calls.length, withInit.views.length], [0, 1]);
});

test("fields are keyed by position, never by the deployer's names", () => {
  const { calls } = specsFromProgram(record([{ untrusted_name: "set", kind: "call", args: [{ untrusted_name: "constructor", type: "u8" }, { untrusted_name: "value", type: "bool" }] }]), "ab".repeat(32));
  assert.deepEqual(calls[0].params.map((p) => p.key), ["arg0", "arg1"]);
  assert.deepEqual(initialDraft(calls[0]), { arg0: "", arg1: "false", storage_limit: "0" }, "a call's storage limit starts at 0, never guessed");
});

test("the escrow's open, typed into its form, is the input the recorded call carried", () => {
  const open = specsFromProgram(escrow, escrow.version_digest).calls[0];
  const c = checkCall(open, { arg0: "1", arg1: bob, value: "2.5", storage_limit: "0.03" }, "boo");
  assert.equal(c.input, fx.escrow.tx_open.action.calls[0].input);
  assert.equal(c.value, 2_500_000_000n);
  assert.equal(c.storageLimit, 30_000_000n);
  // What it signs is the spec's own call, bound to the digest it was made with; only the fuel limit is the SDK's.
  const action = specAction(open, c);
  assert.deepEqual(action.calls, [{ program: escrow.address, version_digest: escrow.version_digest, method: "open", input: c.input, value: "2500000000" }]);
  assert.ok(sameCall({ ...action, fuel_limit: 10_000_000 }, action));
  for (const [what, change] of [
    ["another digest", (a) => ({ ...a, calls: [{ ...a.calls[0], version_digest: "00".repeat(32) }] })],
    ["another input", (a) => ({ ...a, calls: [{ ...a.calls[0], input: "" }] })],
    ["another value", (a) => ({ ...a, calls: [{ ...a.calls[0], value: "1" }] })],
    ["another storage limit", (a) => ({ ...a, storage_limit: "0" })],
    ["a binding", (a) => ({ ...a, bindings: [{ program: bob, version_digest: "00".repeat(32) }] })],
    ["two calls", (a) => ({ ...a, calls: [a.calls[0], a.calls[0]] })],
  ]) assert.equal(sameCall(change(action), action), false, what);
});

test("forms encode what the borsh crate writes", () => {
  const vec = abi.inputs.find((v) => v.label.startsWith("register("));
  const register = specsFromProgram(record([{ untrusted_name: "register", kind: "call", args: vec.args.map((type, i) => ({ untrusted_name: `a${i}`, type })) }]), "ab".repeat(32)).calls[0];
  assert.deepEqual(register.params.map((p) => [p.type, !!p.optional]), [["text", false], ["text", true], ["text", true]]);
  assert.equal(encodeDraft(register, { arg0: "Darkoh", arg1: "darkoh_x", arg2: "" }, "boo"), vec.hex, "an empty optional field is none");
  const every = abi.inputs.find((v) => v.label === "every kind once");
  const spec = specsFromProgram(record([{ untrusted_name: "every", kind: "call", args: every.args.map((type, i) => ({ untrusted_name: `a${i}`, type })) }]), "ab".repeat(32)).calls[0];
  // fang is typed as FANG: 5 drops is 0.000000005; hex is read in either case, with or without 0x.
  assert.equal(encodeDraft(spec, { arg0: "true", arg1: "0.000000005", arg2: "Bat.boo", arg3: `0x${"07".repeat(32).toUpperCase()}`, arg4: "09", arg5: "-3" }, "boo"), every.hex);
  const transfer = abi.inputs.find((v) => v.label.startsWith("token's transfer"));
  const t = specsFromProgram(record([{ untrusted_name: "transfer", kind: "call", args: transfer.args.map((type, i) => ({ untrusted_name: `a${i}`, type })) }]), "ab".repeat(32)).calls[0];
  assert.equal(encodeDraft(t, { arg0: `  ${transfer.values[0]} `, arg1: "250" }, "boo"), transfer.hex);
});

test("lists are repeatable fields up to their bound; deeper types are one JSON field the codec holds to the type", () => {
  const { calls } = specsFromProgram(record([{ untrusted_name: "send", kind: "call", args: [{ untrusted_name: "to", type: { vec: ["address", 2] } }, { untrusted_name: "deep", type: { option: { vec: [{ option: { vec: ["u8", 2] } }, 2] } } }] }]), "ab".repeat(32));
  const [to, deep] = calls[0].params;
  assert.deepEqual([to.type, to.of, to.max, deep.type], ["list", "address", 2, "json"]);
  const four = abi.values.find((v) => v.label === "four layers");
  const addresses = abi.values.find((v) => v.label === "vec of addresses");
  assert.equal(encodeDraft(calls[0], { arg0: addresses.value, arg1: JSON.stringify(four.value) }, "boo"), addresses.hex + four.hex);
  assert.match(fieldProblem(to, [bob, bob, bob], "boo"), /at most 2/);
  assert.match(fieldProblem(to, [bob, "alice.boo"], "boo"), /^Item 2: .*never a \.boo name/);
  assert.equal(fieldProblem(deep, "null", "boo"), null);
  assert.equal(fieldProblem(deep, "", "boo"), "Required: JSON for this type (null for none).");
  assert.equal(fieldProblem(deep, "{some", "boo"), "That isn't JSON.");
  assert.ok(fieldProblem(deep, '{"some": [{"some": ["1", "2", "3"]}]}', "boo"));
});

test("each field refuses what its type can't hold, in the codec's words", () => {
  const { calls } = specsFromProgram(record([{ untrusted_name: "f", kind: "call", args: [{ untrusted_name: "who", type: "address" }, { untrusted_name: "note", type: { text: 4 } }, { untrusted_name: "n", type: "u64" }, { untrusted_name: "h", type: "hash32" }, { untrusted_name: "maybe", type: { option: "u8" } }] }]), "ab".repeat(32));
  const [who, note, n, h, maybe] = calls[0].params;
  assert.equal(fieldProblem(who, bob, "boo"), null);
  assert.match(fieldProblem(who, "bob.boo", "boo"), /never a \.boo name/);
  assert.match(fieldProblem(who, bob.replace("boo1", "tboo1"), "boo"), /./, "another network's address");
  assert.match(fieldProblem(who, `${bob.slice(0, -1)}q`, "boo"), /./, "a checksum that doesn't hold");
  assert.equal(fieldProblem(who, "", "boo"), "Required.");
  assert.equal(fieldProblem(note, "abcd", "boo"), null);
  assert.match(fieldProblem(note, "日本", "boo"), /6 bytes|at most 4/, "text is bounded in bytes, not characters");
  assert.equal(fieldProblem(note, "", "boo"), null, "empty text is text");
  assert.equal(fieldProblem(n, "18446744073709551615", "boo"), null);
  assert.match(fieldProblem(n, "18446744073709551616", "boo"), /outside u64's range/);
  assert.match(fieldProblem(n, "1.5", "boo"), /whole number/);
  assert.match(fieldProblem(h, "ab", "boo"), /./);
  assert.equal(fieldProblem(maybe, "", "boo"), null, "empty is none");
  assert.match(fieldProblem(maybe, "256", "boo"), /outside u8's range/);
  assert.throws(() => encodeDraft(calls[0], { arg0: "bob.boo", arg1: "", arg2: "1", arg3: "ab".repeat(32), arg4: "" }, "boo"), /^Error: Argument 1 \(who\): .*never a \.boo name/);
  assert.throws(() => checkCall(calls[0], { arg0: bob, arg1: "", arg2: "1", arg3: "ab".repeat(32), arg4: "", storage_limit: "lots" }, "boo"), /Storage limit: an amount of FANG/);
  // Not payable: whatever the form holds for a value, none is sent.
  assert.equal(checkCall(calls[0], { arg0: bob, arg1: "", arg2: "1", arg3: "ab".repeat(32), arg4: "", value: "5" }, "boo").value, 0n);
});

test("what a method returned is decoded by its declared type, and refused when it isn't one", () => {
  assert.deepEqual(readReturn("u64", fx.counter.view_ok.return_data, "boo"), { ok: true, value: 1n });
  // The counter's increment, as it ran: its receipt's return data is the program's claim, decoded by the interface.
  const receipt = checkReceipt(RPC, fx.counter.tx_call_ok);
  const r = readReturn("u64", receipt.claims.return_data[0].data, "boo");
  assert.equal(r.ok, true);
  assert.equal(readReturn(null, "", "boo").ok, true, "a method that returns nothing");
  assert.equal(readReturn(null, "00", "boo").ok, false);
  assert.equal(readReturn("u64", "01000000000000", "boo").ok, false, "seven bytes aren't a u64");
  assert.equal(readReturn("u8", "0100", "boo").ok, false, "a byte left over");
  assert.equal(readReturn("bool", "02", "boo").ok, false);
  assert.equal(readReturn({ text: 32 }, "02000000c328", "boo").ok, false, "text that isn't UTF-8");
  const input = readInput(escrow.methods[0].args, fx.escrow.tx_open.action.calls[0].input, "boo");
  assert.deepEqual(input, { ok: true, values: [1n, bob] });
});

test("a returned value shows as text: hidden characters out (and said), amounts with their drops, none and lists plain", () => {
  const hostile = '<img src=x onerror="window.__pwned=1">‮gnp.exe\u0007';
  const s = shownValue({ text: 200 }, hostile);
  assert.equal(s.leaf.altered, true);
  assert.doesNotMatch(s.leaf.text, /[‮\u0007]/);
  assert.ok(s.leaf.text.startsWith("<img"), "markup stays text: React escapes it");
  assert.equal(shownValue({ text: 8 }, "plain").leaf.altered, false);
  assert.equal(shownText(shownValue("fang", 1_500_000_000n)), "1.5 FANG (1500000000 drops)");
  assert.equal(shownText(shownValue("fang", 5n, "BAT")), "0.000000005 BAT (5 drops)");
  assert.equal(shownText(shownValue({ option: "u64" }, null)), "none");
  assert.equal(shownText(shownValue({ option: "u64" }, { some: 7n })), "7");
  assert.equal(shownText(shownValue({ vec: [{ option: { text: 8 } }, 3] }, [{ some: "a" }, null])), "[a, none]");
  assert.equal(shownValue("address", bob).leaf.mono, true);
  assert.equal(shownText(shownValue({ bytes: 4 }, "")), "(no bytes)");
  assert.equal(shownText(shownValue({ text: 4 }, "")), "(empty text)");
});

test("each field says what it takes, in plain words", () => {
  assert.equal(typeHint("u8"), "A whole number, 0 to 255 (u8).");
  assert.match(typeHint("i64"), /-9,223,372,036,854,775,808 to 9,223,372,036,854,775,807/);
  assert.match(typeHint("address", "tboo"), /tboo1…\), never a \.boo name/);
  assert.match(typeHint({ text: 32 }), /32 bytes of UTF-8/);
  assert.match(typeHint({ option: "u64" }), /or empty for none\.$/);
  assert.match(typeHint({ vec: ["address", 8] }), /^A list of up to 8\. Each: an address/);
  assert.match(typeHint({ option: { option: "bool" } }), /^JSON for this type/);
});
