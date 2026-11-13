// The ABI codec (src/programs.ts `encodeProgramInput`, `decodeProgramValue`, docs/PROGRAMS-V0.md §3.2),
// offline. It's held to the borsh crate the node pins (test/program-abi-vectors.json, written by
// node/src/programs/abi_vectors.rs): every value and input encodes to the crate's bytes and decodes back,
// and bytes the crate refuses are refused. It's held to a real call too (the escrow guest's recorded
// `open` in test/program-fixtures.json), and to the interface's bounds, which Borsh doesn't know: text is
// bounded in bytes, not characters; a vec in items. `npm run test:vectors -w @blockula/sdk` runs it.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { bech32m } from "@scure/base";
import { MAX_PROGRAM_INPUT_BYTES, ProgramValueError, checkProgram, decodeProgramInput, decodeProgramValue, encodeProgramInput, encodeProgramValue, sameProgramValue, type ProgramType, type ProgramValue } from "../src/index.js";

const vectors = JSON.parse(readFileSync(new URL("./program-abi-vectors.json", import.meta.url), "utf8"));
const fx = JSON.parse(readFileSync(new URL("./program-fixtures.json", import.meta.url), "utf8"));
const hex = (b: Uint8Array) => Buffer.from(b).toString("hex");

/** A vector's JSON value as the SDK holds it: integers and drops as bigints, the rest as written. */
function fromJson(type: ProgramType, v: unknown): ProgramValue {
  if (typeof type === "string") return ["u8", "u16", "u32", "u64", "u128", "i64", "i128", "fang"].includes(type) ? BigInt(v as string) : (v as ProgramValue);
  if ("option" in type) return v === null ? null : { some: fromJson(type.option, (v as { some: unknown }).some) };
  if ("vec" in type) return (v as unknown[]).map((x) => fromJson(type.vec[0], x));
  return v as ProgramValue;
}
const refusedAs = (what: string, run: () => unknown, match: RegExp) => assert.throws(run, (e: unknown) => e instanceof ProgramValueError && match.test(e.message), what);

// ---- every value: the borsh crate's bytes, both ways ----
{
  assert.ok(vectors.values.length >= 30);
  for (const v of vectors.values) {
    const value = fromJson(v.type, v.value);
    assert.equal(hex(encodeProgramValue(v.type, value)), v.hex, `encode ${v.label}`);
    const back = decodeProgramValue(v.type, v.hex);
    assert.ok(sameProgramValue(back, value), `decode ${v.label}: ${String(back)}`);
    assert.ok(sameProgramValue(decodeProgramValue(v.type, Buffer.from(v.hex, "hex")), value), `decode ${v.label} from bytes`);
  }
  // A text's bound is its bytes: the vector at its bound is 7 characters and 11 bytes.
  const atBound = vectors.values.find((v: { label: string }) => v.label.startsWith("text of 7 characters"));
  assert.equal([...atBound.value].length, 7);
  assert.equal(new TextEncoder().encode(atBound.value).length, atBound.type.text);
  // A byte-order mark is the program's text, kept (a decoder that strips it would show other text than was sent).
  assert.equal(decodeProgramValue({ text: 8 }, "05000000efbbbf6869"), "﻿hi");
  console.log(`✓ ${vectors.values.length} values: the borsh crate's bytes, encoded and decoded back`);
}

// ---- inputs: arguments back to back, as a Borsh tuple; and the call the escrow guest really took ----
{
  for (const v of vectors.inputs) {
    const args = v.args.map((type: ProgramType) => ({ type }));
    const values = v.values.map((x: unknown, i: number) => fromJson(v.args[i], x));
    assert.equal(encodeProgramInput(args, values), v.hex, `encode ${v.label}`);
    const back = decodeProgramInput(args, v.hex);
    assert.ok(back.length === values.length && back.every((x, i) => sameProgramValue(x, values[i])), `decode ${v.label}`);
  }
  // The recorded `open(id: 1, seller: bob)` of `blockula dev --programs`: the interface it was signed under, the input it carried.
  const escrow = checkProgram("http://node.test", fx.escrow.program.address, fx.escrow.program);
  const open = escrow.methods?.find((m) => m.untrusted_name === "open");
  assert.ok(open);
  const recorded = fx.escrow.tx_open.action.calls[0];
  assert.equal(encodeProgramInput(open.args, [1n, fx.escrow.accounts.bob]), recorded.input, "the escrow's open, as the chain ran it");
  assert.deepEqual(decodeProgramInput(open.args, recorded.input), [1n, fx.escrow.accounts.bob]);
  // The counter's `get() → u64` view, as the dev node answered it: 1.
  const counter = checkProgram("http://node.test", fx.counter.program.address, fx.counter.program);
  const get = counter.methods?.find((m) => m.untrusted_name === "get");
  assert.equal(decodeProgramValue(get?.returns ?? null, fx.counter.view_ok.return_data), 1n);
  // A method that returns nothing returns no bytes.
  assert.equal(decodeProgramValue(null, ""), null);
  refusedAs("a method that returns nothing, with bytes", () => decodeProgramValue(null, "00"), /1 byte left over/);
  console.log(`✓ ${vectors.inputs.length} inputs, and the escrow's recorded open and the counter's view, both ways`);
}

// ---- what Borsh refuses is refused ----
{
  for (const v of vectors.refused) refusedAs(v.label, () => decodeProgramValue(v.type, v.hex), /./);
  console.log(`✓ ${vectors.refused.length} encodings the borsh crate refuses are refused`);
}

// ---- the interface's bounds, which Borsh doesn't know, and values of the wrong kind ----
{
  // Text: at most `max` bytes of UTF-8, whatever its characters.
  assert.equal(hex(encodeProgramValue({ text: 4 }, "abcd")), "0400000061626364");
  assert.equal(hex(encodeProgramValue({ text: 4 }, "日")), "03000000e697a5");
  refusedAs("two characters past four bytes", () => encodeProgramValue({ text: 4 }, "日本"), /at most 4 bytes of UTF-8, and this is 6/);
  refusedAs("a lone surrogate", () => encodeProgramValue({ text: 8 }, "a\ud800b"), /lone surrogate/);
  refusedAs("text that isn't a string", () => encodeProgramValue({ text: 8 }, 5), /text is a string/);
  refusedAs("decoded text past its bound", () => decodeProgramValue({ text: 2 }, "03000000616263"), /says 3 bytes of text, past its type's 2/);
  // Bytes and vecs: their counts.
  refusedAs("bytes past their bound", () => encodeProgramValue({ bytes: 2 }, "000102"), /at most 2 bytes, and this is 3/);
  refusedAs("bytes in uppercase hex", () => encodeProgramValue({ bytes: 2 }, "AB"), /lowercase hex/);
  refusedAs("a vec past its bound", () => encodeProgramValue({ vec: ["u8", 2] }, [1n, 2n, 3n]), /at most 2 items, and this has 3/);
  refusedAs("a decoded vec past its bound", () => decodeProgramValue({ vec: ["u8", 2] }, "03000000010203"), /says 3 items, past its type's 2/);
  refusedAs("decoded bytes past their bound", () => decodeProgramValue({ bytes: 1 }, "020000000102"), /says 2 bytes, past its type's 1/);
  // Integers: whole, in range, never a float.
  assert.equal(hex(encodeProgramValue("u64", "18446744073709551615")), "ffffffffffffffff");
  assert.equal(hex(encodeProgramValue("u64", 5)), "0500000000000000", "a safe integer");
  assert.equal(hex(encodeProgramValue("i64", "-1")), "ffffffffffffffff");
  for (const [type, v, re] of [
    ["u64", -1n, /outside u64's range/],
    ["u64", 1n << 64n, /outside u64's range/],
    ["u8", 256, /outside u8's range/],
    ["i64", 1n << 63n, /outside i64's range/],
    ["i128", -(1n << 127n) - 1n, /outside i128's range/],
    ["u64", 1.5, /isn't a whole number JavaScript holds exactly/],
    ["u64", 2 ** 53, /isn't a whole number JavaScript holds exactly/],
    ["u64", "01", /whole number/],
    ["u64", "-0", /whole number/],
    ["u64", " 1", /whole number/],
    ["u64", "1e3", /whole number/],
    ["u32", true, /whole number/],
  ] as const) refusedAs(`${type} ${String(v)}`, () => encodeProgramValue(type, v), re);
  // FANG: a bigint of drops, or a decimal string of FANG; never a number, which would have to be guessed.
  assert.equal(hex(encodeProgramValue("fang", "1.5")), "002f685900000000");
  assert.equal(hex(encodeProgramValue("fang", 1_500_000_000n)), "002f685900000000");
  refusedAs("FANG as a number", () => encodeProgramValue("fang", 5), /not the number 5/);
  refusedAs("ten decimals", () => encodeProgramValue("fang", "1.0000000001"), /at most 9 decimals/);
  refusedAs("past a u64 of drops", () => encodeProgramValue("fang", "18446744074"), /outside an amount of drops/);
  // Bools: true or false.
  refusedAs("a bool as 1", () => encodeProgramValue("bool", 1), /a bool is true or false/);
  console.log("✓ the interface's bounds: text in bytes, bytes and vecs in counts; integers whole and in range; FANG never a number");
}

// ---- addresses only, never names; this chain's network only ----
{
  const key = new Uint8Array(32).fill(7);
  const boo = bech32m.encode("boo", bech32m.toWords(key));
  const tboo = bech32m.encode("tboo", bech32m.toWords(key));
  assert.equal(hex(encodeProgramValue("address", boo)), "07".repeat(32));
  assert.equal(hex(encodeProgramValue("address", tboo, { prefix: "tboo" })), "07".repeat(32));
  refusedAs("a .boo name", () => encodeProgramValue("address", "alice.boo"), /never a \.boo name/);
  refusedAs("a bare name", () => encodeProgramValue("address", "alice"), /never a \.boo name/);
  refusedAs("another network's address", () => encodeProgramValue("address", tboo, { prefix: "boo" }), /another network \(tboo1…\): addresses here start with boo1/);
  refusedAs("a typo in the checksum", () => encodeProgramValue("address", `${boo.slice(0, -1)}${boo.endsWith("q") ? "p" : "q"}`), /isn't an address/);
  refusedAs("uppercase", () => encodeProgramValue("address", boo.toUpperCase()), /isn't an address/);
  refusedAs("bytes where an address goes", () => encodeProgramValue("address", key), /isn't an address/);
  // Decoded with the chain's prefix.
  assert.equal(decodeProgramValue("address", "07".repeat(32), { prefix: "tboo" }), tboo);
  assert.equal(decodeProgramValue("address", "07".repeat(32)), boo);
  refusedAs("an unknown prefix", () => decodeProgramValue("address", "07".repeat(32), { prefix: "cosmos" }), /isn't an address prefix/);
  // A name type is a label, written without its suffix.
  assert.equal(hex(encodeProgramValue("name", "Alice.boo")), hex(encodeProgramValue("name", "alice")));
  refusedAs("a label too short", () => encodeProgramValue("name", "al"), /3 to 32/);
  refusedAs("a label ending in a dash", () => encodeProgramValue("name", "bat-"), /3 to 32/);
  refusedAs("a decoded name that isn't a label", () => decodeProgramValue("name", "03000000414243"), /isn't a \.boo label/);
  // A hash32: exactly 32 bytes.
  refusedAs("a short hash", () => encodeProgramValue("hash32", "ab".repeat(31)), /32 bytes/);
  console.log("✓ addresses: boo1… or tboo1… of this chain's network, never a .boo name; names are labels; hashes 32 bytes");
}

// ---- options: none and some apart, nested ones unambiguous ----
{
  assert.equal(hex(encodeProgramValue({ option: "u8" }, null)), "00");
  assert.equal(hex(encodeProgramValue({ option: "u8" }, undefined)), "00");
  assert.equal(hex(encodeProgramValue({ option: "u8" }, 3n)), "0103", "a bare value is some");
  assert.equal(hex(encodeProgramValue({ option: "u8" }, { some: 3n })), "0103");
  assert.equal(hex(encodeProgramValue({ option: { option: "u8" } }, { some: null })), "0100");
  refusedAs("a bare value under two options", () => encodeProgramValue({ option: { option: "u8" } }, 3n), /an option of an option is null \(none\) or \{ some: … \}/);
  assert.deepEqual(decodeProgramValue({ option: { option: "u8" } }, "0100"), { some: null });
  assert.ok(!sameProgramValue(null, { some: null }), "none isn't some(none)");
  assert.ok(!sameProgramValue([1n], [1n, 2n]));
  assert.ok(sameProgramValue([{ some: "a" }, null], [{ some: "a" }, null]));
  console.log("✓ options: none, some, and some(none) apart");
}

// ---- inputs as a whole: one value per argument, at most 32 KiB; types as an interface holds them ----
{
  refusedAs("too few values", () => encodeProgramInput([{ type: "u8" }, { type: "u8" }], [1n]), /takes 2 arguments, and 1 was given/);
  refusedAs("a value of the wrong kind names its argument by position", () => encodeProgramInput([{ type: "u8" }, { type: { text: 4 } }], [1n, 5]), /^argument 2: text is a string/);
  refusedAs("an item names its place", () => encodeProgramInput([{ type: { vec: [{ option: "u8" }, 3] } }], [[1n, { some: 300n }]]), /^argument 1, item 2, inside some: 300 is outside u8's range/);
  const big = "a".repeat(20_000);
  refusedAs("past 32 KiB", () => encodeProgramInput([{ type: { text: 32_768 } }, { type: { text: 32_768 } }], [big, big]), new RegExp(`past the ${MAX_PROGRAM_INPUT_BYTES} a call may carry`));
  refusedAs("input bytes left over", () => decodeProgramInput([{ type: "u8" }], "0102"), /^the input: 1 byte left over/);
  refusedAs("input cut short", () => decodeProgramInput([{ type: "u8" }, { type: "u8" }], "01"), /^argument 2: the bytes end 1 short/);
  for (const [type, re] of [
    ["float", /isn't one an interface can hold/],
    [{ text: 0 }, /text's bound 0 isn't 1 to 32768/],
    [{ bytes: 32_769 }, /bytes's bound 32769 isn't 1 to 32768/],
    [{ vec: ["u8", 1025] }, /vec's bound 1025 isn't 1 to 1024/],
    [{ option: { option: { option: { option: { option: "u8" } } } } }, /nests deeper/],
    [{ text: 4, bytes: 4 }, /isn't one an interface can hold/],
  ] as const) refusedAs(`the type ${JSON.stringify(type)}`, () => encodeProgramValue(type as ProgramType, null), re);
  console.log("✓ inputs: one value per argument, at most 32 KiB, places named by position; types bounded as an interface's are");
}

console.log("✓ program ABI tests done");
