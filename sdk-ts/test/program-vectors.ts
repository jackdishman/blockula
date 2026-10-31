// Programs' digest vectors: the SDK's ports (src/tx.ts `wasmHash`, `interfaceHash`,
// `versionDigest`, `programAddress`, `methodSelector`) against the node's (node/src/programs/digest.rs
// and the VM's `interface::selector`, docs/PROGRAMS-V0.md §2.1), offline. program-vectors.json is
// written and checked by the node's `programs::digest::tests::program_vectors`.
// No node needed: `npm run test:vectors -w @blockula/sdk`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { bech32, bech32m } from "@scure/base";
import { hexToBytes } from "@noble/hashes/utils.js";
import { FIRST_PROGRAM_VERSION, PROGRAM_DIGEST_CONTEXTS, interfaceHash, methodSelector, programAddress, versionDigest, wasmHash, type Action, type UnsignedTx } from "../src/index.js";

interface Blob { label: string; input: string; hash: string }
const V = JSON.parse(readFileSync(new URL("./program-vectors.json", import.meta.url), "utf8")) as {
  contexts: Record<string, string>;
  wasm_hashes: Blob[];
  interface_hashes: Blob[];
  program_addresses: { creator: string; nonce: string; bytes: string; boo: string; tboo: string }[];
  version_digests: { address: string; wasm_hash: string; interface_hash: string; version: number; digest: string }[];
  selectors: { method: string; selector: string }[];
};

// The five contexts, each digest's own (so no two digests of different things meet).
assert.deepEqual({ ...PROGRAM_DIGEST_CONTEXTS }, V.contexts);
assert.equal(new Set(Object.values(PROGRAM_DIGEST_CONTEXTS)).size, 5, "five distinct contexts");
assert.equal(FIRST_PROGRAM_VERSION, 1);

// Code and interfaces, as lowercase hex (how `upload_code` carries them) and as bytes.
for (const [name, hash, cases] of [["wasm", wasmHash, V.wasm_hashes], ["interface", interfaceHash, V.interface_hashes]] as const) {
  for (const c of cases) {
    assert.equal(hash(c.input), c.hash, `${name} hash of ${c.label || "nothing"}`);
    assert.equal(hash(hexToBytes(c.input)), c.hash, `${name} hash of ${c.label || "nothing"}, as bytes`);
  }
}
// The same bytes hash apart as code and as an interface.
for (const c of V.wasm_hashes) assert.notEqual(wasmHash(c.input), interfaceHash(c.input));

// Program addresses: from the creator's address and the creating transaction's nonce, in the
// creator's prefix unless another is given (the prefix only renders the same 32 bytes).
for (const c of V.program_addresses) {
  const creator = { boo: bech32m.encode("boo", bech32m.toWords(hexToBytes(c.creator))), tboo: bech32m.encode("tboo", bech32m.toWords(hexToBytes(c.creator))) };
  assert.equal(programAddress(creator.boo, c.nonce), c.boo, `${c.creator.slice(0, 8)}… nonce ${c.nonce}`);
  assert.equal(programAddress(creator.tboo, c.nonce), c.tboo, `${c.creator.slice(0, 8)}… nonce ${c.nonce} on a testnet`);
  assert.equal(programAddress(creator.boo, c.nonce, "tboo"), c.tboo, "an explicit prefix");
  assert.equal(programAddress(creator.tboo, BigInt(c.nonce), "boo"), c.boo, "a bigint nonce");
  assert.deepEqual(bech32m.fromWords(bech32m.decode(c.boo as `${string}1${string}`).words), hexToBytes(c.bytes), "the address carries the KDF's 32 bytes");
}
const nonceZero = V.program_addresses.find((c) => c.nonce === "0")!;
const creator0 = bech32m.encode("boo", bech32m.toWords(hexToBytes(nonceZero.creator)));
assert.equal(programAddress(creator0, 0), nonceZero.boo, "a number nonce");

// Version digests: the address (as the tx codec writes a string), the two hashes and the version.
for (const c of V.version_digests) assert.equal(versionDigest(c.address, c.wasm_hash, c.interface_hash, c.version), c.digest, `${c.address.slice(0, 8)}… v${c.version}`);
const v1 = V.version_digests.find((c) => c.version === 1 && c.address.startsWith("boo1"))!;
assert.equal(versionDigest(v1.address, v1.wasm_hash, v1.interface_hash), v1.digest, "version 1 by default");

// Selectors: the first 8 bytes under the method context (the VM's, the node's only one).
for (const c of V.selectors) assert.equal(methodSelector(c.method), c.selector, c.method);
console.log(`✓ ${V.wasm_hashes.length + V.interface_hashes.length} code and interface hashes, ${V.program_addresses.length} program addresses (both prefixes), ${V.version_digests.length} version digests and ${V.selectors.length} method selectors match the node`);

// The digests chain the way a client uses them: the program the vectors' first creator makes with
// nonce 0, from the code and interface the vectors upload, is the program whose version digest
// they give. And the upload vector's code is the code these hash.
assert.equal(v1.address, programAddress(creator0, 0));
const code = V.wasm_hashes.find((c) => c.label.startsWith("the Wasm header"))!;
const iface = V.interface_hashes.find((c) => c.label.startsWith("an interface"))!;
assert.equal(v1.wasm_hash, wasmHash(code.input));
assert.equal(v1.interface_hash, interfaceHash(iface.input));
const T = JSON.parse(readFileSync(new URL("./tx-vectors.json", import.meta.url), "utf8")) as { program_cases: { label: string; tx: UnsignedTx }[] };
const upload = T.program_cases.find((c) => c.label === "upload_code (tag 62)")!.tx.action as Extract<Action, { type: "upload_code" }>;
assert.equal(wasmHash(upload.wasm), code.hash, "the upload vector's code");
assert.equal(interfaceHash(upload.interface), iface.hash, "the upload vector's interface");
console.log("✓ the address, the hashes and the version digest chain as a create and a call use them");

/** The same 32 bytes and prefix with a bech32 checksum: an alias the chain refuses (crypto.rs `parse_address`). */
function bech32Of(addr: string): string {
  const { prefix, words } = bech32m.decode(addr as `${string}1${string}`);
  return bech32.encode(prefix, words);
}

// What isn't a program's part is refused, never hashed as something else.
const [address, w, i] = [v1.address, v1.wasm_hash, v1.interface_hash];
const refused: [string, () => unknown, RegExp][] = [
  ["code in uppercase hex", () => wasmHash("0061736D"), /wasm must be lowercase hex/],
  ["code of an odd length", () => wasmHash("006"), /wasm must be lowercase hex/],
  ["an interface not hex", () => interfaceHash("0x01"), /interface must be lowercase hex/],
  ["a version digest of a name", () => versionDigest("counter.boo", w, i), /program must be an address/],
  ["a version digest of an uppercase address", () => versionDigest(address.toUpperCase(), w, i), /program must be an address/],
  ["a code hash in uppercase", () => versionDigest(address, w.toUpperCase(), i), /wasm_hash must be 32 bytes of lowercase hex/],
  ["a short interface hash", () => versionDigest(address, w, i.slice(2)), /interface_hash must be 32 bytes of lowercase hex/],
  ["version 2^32", () => versionDigest(address, w, i, 2 ** 32), /version must be 0 to 4294967295/],
  ["version 1.5", () => versionDigest(address, w, i, 1.5), /version must be a whole number/],
  ["a creator that's a name", () => programAddress("alice.boo", 0), /creator must be an address/],
  ["a creator with a bech32 (not bech32m) checksum", () => programAddress(bech32Of(creator0), 0), /creator must be an address/],
  ["a nonce of -1", () => programAddress(creator0, -1), /nonce must be 0 to/],
  ["a nonce of 2^64", () => programAddress(creator0, (1n << 64n).toString()), /nonce must be 0 to 18446744073709551615/],
  ["an unknown prefix", () => programAddress(creator0, 0, "evil"), /unknown address prefix evil/],
  ["a selector of a capitalized name", () => methodSelector("Init"), /a method name is an identifier/],
  ["a selector of nothing", () => methodSelector(""), /a method name is an identifier/],
  ["a selector of a 33-character name", () => methodSelector("m".repeat(33)), /a method name is an identifier/],
  ["a selector of a non-ASCII name", () => methodSelector("ïnit"), /a method name is an identifier/],
];
for (const [label, f, re] of refused) assert.throws(f, re, label);
console.log(`✓ ${refused.length} malformed inputs refused before hashing`);
