#!/usr/bin/env node
// Verifies offline ML-DSA-65 signatures on release checksums (docs/OPERATIONS.md "Releases"), from
// a checkout after `npm ci` (it uses the SDK's @noble dependencies). It never loads a key and has no
// `sign`: the release key is used only by a verified `blockula release sign` on the offline
// machine, which holds no checkout and no npm tree (audit supply-chain-B-4).
//
//   node scripts/release-sig.mjs verify --key-id <hex> [--tag vX.Y.Z] --sig checksums.txt.sig checksums.txt
//   node scripts/release-sig.mjs selftest
//
// Format, shared with `blockula release sign|verify` (node/src/release_cli.rs), which passes the
// same selftest vector:
//   message   = "BLOCKULA_RELEASE_V1" ‖ file bytes
//   signature = ML-DSA-65 (FIPS 204, empty context, deterministic), as crypto::Keys::sign
//   key id    = hex BLAKE3(public key), as everywhere else in Blockula
//   .sig file = one JSON line: {"format","key_id","public_key","signature"} (hex)
//   --tag     = the file's first line must be "# blockula-release <tag>" (checksums.txt's, written
//               by scripts/release-checksums.sh), so another release's signed checksums can't pass
import { readFileSync } from "node:fs";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, hexToBytes, utf8ToBytes, concatBytes } from "@noble/hashes/utils.js";

const FORMAT = "blockula-release-signature-v1";
const DOMAIN = utf8ToBytes("BLOCKULA_RELEASE_V1");
const PUBLIC_KEY_LEN = 1952;
const SIGNATURE_LEN = 3309;

const TAG_LINE_PREFIX = "# blockula-release ";

const message = (file) => concatBytes(DOMAIN, file);
const keyId = (publicKey) => bytesToHex(blake3(publicKey));

/** Only the selftest signs, with the vector's public seed. */
function sign(keys, file) {
  const signature = ml_dsa65.sign(message(file), keys.secretKey, { extraEntropy: false });
  return { format: FORMAT, key_id: keyId(keys.publicKey), public_key: bytesToHex(keys.publicKey), signature: bytesToHex(signature) };
}

/** Throws unless `sig` is a valid signature on `file` by the key with id `expectedKeyId`. */
function verify(sig, file, expectedKeyId) {
  if (sig.format !== FORMAT) throw new Error(`unknown signature format ${sig.format}`);
  const publicKey = hexToBytes(sig.public_key ?? "");
  const signature = hexToBytes(sig.signature ?? "");
  if (publicKey.length !== PUBLIC_KEY_LEN || signature.length !== SIGNATURE_LEN) throw new Error("malformed ML-DSA-65 key or signature");
  const id = keyId(publicKey);
  if (sig.key_id !== id) throw new Error("key_id doesn't match the public key");
  if (!/^[0-9a-f]{64}$/.test(expectedKeyId ?? "")) throw new Error("pass the release key id to trust (--key-id, 64 hex chars)");
  if (id !== expectedKeyId) throw new Error(`signed by key ${id}, not the release key ${expectedKeyId}`);
  if (!ml_dsa65.verify(signature, message(file), publicKey)) throw new Error("INVALID signature");
}

/** Throws unless the file's first line names release `tag`. */
function checkTag(file, tag) {
  if (!/^v\d+\.\d+\.\d+$/.test(tag ?? "")) throw new Error(`a release tag is vX.Y.Z, got ${tag}`);
  const first = new TextDecoder().decode(file).split("\n", 1)[0];
  if (first === TAG_LINE_PREFIX + tag) return;
  if (first.startsWith(TAG_LINE_PREFIX)) throw new Error(`it's for release ${first.slice(TAG_LINE_PREFIX.length)}, not ${tag}: another release's signed checksums`);
  throw new Error(`it names no release (no "${TAG_LINE_PREFIX}vX.Y.Z" first line), so it can't be bound to ${tag}`);
}

// Known-answer vector, cross-checked against the node's ml-dsa 0.1.1 (crypto::Keys::from_seed, then
// sign over the domain-prefixed file): a Rust `blockula release` must reproduce it.
const VECTOR = {
  seed: "07".repeat(32),
  file: "0123abcd  blockula-x86_64-unknown-linux-gnu.tar.gz\n",
  key_id: "b470b86b94891081659c41ca43129ca4fd136cd247ecbd0b9d3f037f5215fedd",
  signature_blake3: "390d0f3d0c35684e1f9bf14044eaa58edb00ea8849c6eaecc14d782f2338b579",
};

function selftest() {
  const keys = ml_dsa65.keygen(hexToBytes(VECTOR.seed));
  const file = utf8ToBytes(VECTOR.file);
  const sig = sign(keys, file);
  const got = { key_id: sig.key_id, signature_blake3: bytesToHex(blake3(hexToBytes(sig.signature))) };
  if (got.key_id !== VECTOR.key_id || got.signature_blake3 !== VECTOR.signature_blake3) {
    throw new Error(`known-answer mismatch: ${JSON.stringify(got)}`);
  }
  verify(sig, file, VECTOR.key_id);
  const rejects = (what, f) => {
    try { f(); } catch { return; }
    throw new Error(`accepted ${what}`);
  };
  rejects("a modified file", () => verify(sig, utf8ToBytes(VECTOR.file.replace("0123", "0124")), VECTOR.key_id));
  rejects("another key", () => verify(sign(ml_dsa65.keygen(new Uint8Array(32)), file), file, VECTOR.key_id));
  rejects("a swapped public key", () => verify({ ...sig, public_key: bytesToHex(ml_dsa65.keygen(new Uint8Array(32)).publicKey) }, file, VECTOR.key_id));
  const flipped = hexToBytes(sig.signature);
  flipped[100] ^= 1;
  rejects("a modified signature", () => verify({ ...sig, signature: bytesToHex(flipped) }, file, VECTOR.key_id));
  rejects("a tx-domain signature", () => verify({ ...sig, signature: bytesToHex(ml_dsa65.sign(file, keys.secretKey, { extraEntropy: false })) }, file, VECTOR.key_id));
  const tagged = utf8ToBytes(`${TAG_LINE_PREFIX}v0.5.0\n${VECTOR.file}`);
  checkTag(tagged, "v0.5.0");
  rejects("another release's checksums", () => checkTag(tagged, "v0.5.1"));
  rejects("untagged checksums", () => checkTag(file, "v0.5.0"));
  rejects("a malformed tag", () => checkTag(tagged, "0.5.0"));
  console.log("✓ release signature selftest passed");
}

function main(argv) {
  const [cmd, ...rest] = argv;
  const opts = {};
  const files = [];
  for (let i = 0; i < rest.length; i++) {
    if (rest[i].startsWith("--")) opts[rest[i].slice(2)] = rest[++i];
    else files.push(rest[i]);
  }
  switch (cmd) {
    case "keyid":
    case "sign":
      throw new Error("release-sig.mjs doesn't load keys or sign: use a verified `blockula release keyid|sign` on the offline machine (docs/OPERATIONS.md \"Releases\")");
    case "verify": {
      if (files.length !== 1 || !opts.sig) throw new Error("usage: verify --key-id <hex> [--tag vX.Y.Z] --sig <file.sig> <file>");
      const file = readFileSync(files[0]);
      verify(JSON.parse(readFileSync(opts.sig, "utf8")), file, opts["key-id"]);
      if (opts.tag !== undefined) checkTag(file, opts.tag);
      console.log(`✓ ${files[0]} is signed by release key ${opts["key-id"]}${opts.tag !== undefined ? ` and is release ${opts.tag}'s` : ""}`);
      return;
    }
    case "selftest":
      return selftest();
    default:
      throw new Error("usage: release-sig.mjs verify|selftest (see the header)");
  }
}

try {
  main(process.argv.slice(2));
} catch (e) {
  console.error(`✗ ${e.message}`);
  process.exit(1);
}
