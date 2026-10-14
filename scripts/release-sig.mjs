#!/usr/bin/env node
// Offline ML-DSA-65 signatures on release checksums (docs/OPERATIONS.md "Releases").
// Run from a checkout after `npm ci` (it uses the SDK's @noble dependencies). The signing machine
// stays offline: copy the checkout over, sign, and carry only checksums.txt.sig back.
//
//   node scripts/release-sig.mjs keyid  --key release-key.json
//   node scripts/release-sig.mjs sign   --key release-key.json checksums.txt > checksums.txt.sig
//   node scripts/release-sig.mjs verify --key-id <hex> --sig checksums.txt.sig checksums.txt
//   node scripts/release-sig.mjs selftest
//
// The key file is `blockula keygen` output ({"address","seed"}). Format, shared with the planned
// `blockula release sign|verify` subcommand (which must pass the same selftest vector):
//   message   = "BLOCKULA_RELEASE_V1" ‖ file bytes
//   signature = ML-DSA-65 (FIPS 204, empty context, deterministic), as crypto::Keys::sign
//   key id    = hex BLAKE3(public key), as everywhere else in Blockula
//   .sig file = one JSON line: {"format","key_id","public_key","signature"} (hex)
import { readFileSync } from "node:fs";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, hexToBytes, utf8ToBytes, concatBytes } from "@noble/hashes/utils.js";
import { bech32m } from "@scure/base";

const FORMAT = "blockula-release-signature-v1";
const DOMAIN = utf8ToBytes("BLOCKULA_RELEASE_V1");
const PUBLIC_KEY_LEN = 1952;
const SIGNATURE_LEN = 3309;

const message = (file) => concatBytes(DOMAIN, file);
const keyId = (publicKey) => bytesToHex(blake3(publicKey));

/** Keys from `blockula keygen` JSON; the address must match, or the seed is corrupt. */
function loadKey(json) {
  const { seed, address } = JSON.parse(json);
  if (!/^[0-9a-f]{64}$/.test(seed ?? "")) throw new Error("key file needs a 32-byte hex `seed` (blockula keygen output)");
  const keys = ml_dsa65.keygen(hexToBytes(seed));
  if (address) {
    const { prefix } = bech32m.decode(address);
    const derived = bech32m.encode(prefix, bech32m.toWords(blake3(keys.publicKey)));
    if (derived !== address) throw new Error(`seed doesn't match address ${address}`);
  }
  return keys;
}

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
    case "keyid": {
      const keys = loadKey(readFileSync(opts.key, "utf8"));
      console.log(keyId(keys.publicKey));
      return;
    }
    case "sign": {
      if (files.length !== 1) throw new Error("usage: sign --key <key.json> <file>");
      const keys = loadKey(readFileSync(opts.key, "utf8"));
      const sig = sign(keys, readFileSync(files[0]));
      verify(sig, readFileSync(files[0]), sig.key_id);
      process.stdout.write(JSON.stringify(sig) + "\n");
      console.error(`signed ${files[0]} with key ${sig.key_id}`);
      return;
    }
    case "verify": {
      if (files.length !== 1 || !opts.sig) throw new Error("usage: verify --key-id <hex> --sig <file.sig> <file>");
      verify(JSON.parse(readFileSync(opts.sig, "utf8")), readFileSync(files[0]), opts["key-id"]);
      console.log(`✓ ${files[0]} is signed by release key ${opts["key-id"]}`);
      return;
    }
    case "selftest":
      return selftest();
    default:
      throw new Error("usage: release-sig.mjs keyid|sign|verify|selftest (see the header)");
  }
}

try {
  main(process.argv.slice(2));
} catch (e) {
  console.error(`✗ ${e.message}`);
  process.exit(1);
}
