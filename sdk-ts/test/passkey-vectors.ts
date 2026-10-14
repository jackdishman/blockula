// Passkey derivation vectors: fixed PRF outputs → seed → key id → address, shared with the web
// wallet's browser tests (web/e2e/passkey.spec.ts). Then createPasskeyWallet/unlockPasskeyWallet
// run against a simulated authenticator, to pin the PRF salt and the WebAuthn options they send.
// No node needed: `npm run test:vectors -w @blockula/sdk`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { bytesToHex, hexToBytes } from "@noble/hashes/utils.js";
import { Wallet } from "../src/index.js";
import { createPasskeyWallet, unlockPasskeyWallet } from "../src/passkey.js";

interface Vectors {
  prf_salt: string;
  context: string;
  prefix: string;
  vectors: { prf: string; seed: string; key_id: string; address: string }[];
}
const V = JSON.parse(readFileSync(new URL("./passkey-vectors.json", import.meta.url), "utf8")) as Vectors;

assert.ok(V.vectors.length >= 4);
for (const v of V.vectors) {
  const w = Wallet.fromPasskeyPrf(hexToBytes(v.prf));
  assert.equal(w.seedHex, v.seed, `seed for prf ${v.prf}`);
  assert.equal(w.keyId, v.key_id, `key id for prf ${v.prf}`);
  assert.equal(w.address, v.address, `address for prf ${v.prf}`);
  assert.ok(w.address.startsWith(`${V.prefix}1`));
  // The seed alone is the wallet: the same key from Wallet.fromSeed.
  assert.equal(Wallet.fromSeed(v.seed).keyId, v.key_id);
}
console.log(`✓ ${V.vectors.length} PRF → seed → key id → address vectors`);

// ---- A simulated authenticator ----

type Mode = "prf-on-create" | "prf-on-get-only" | "no-prf";
const SALT = new TextEncoder().encode(V.prf_salt);
const RAW_ID = new Uint8Array([0xb1, 0x0c, 0x01, 0xa0]);

function authenticator(mode: Mode, prf: Uint8Array) {
  const calls: { kind: "create" | "get"; publicKey: any }[] = [];
  const evalPrf = (publicKey: any) => {
    const salt = new Uint8Array(publicKey.extensions.prf.eval.first);
    // The wallet is bound to this salt: changing it would move every passkey wallet.
    assert.equal(bytesToHex(salt), bytesToHex(SALT), "PRF salt");
    return prf.slice().buffer;
  };
  const credential = (ext: object) => ({ rawId: RAW_ID.slice().buffer, getClientExtensionResults: () => ext });
  const credentials = {
    async create({ publicKey }: any) {
      calls.push({ kind: "create", publicKey });
      if (mode === "prf-on-create") return credential({ prf: { enabled: true, results: { first: evalPrf(publicKey) } } });
      if (mode === "prf-on-get-only") return credential({ prf: { enabled: true } });
      return credential({});
    },
    async get({ publicKey }: any) {
      calls.push({ kind: "get", publicKey });
      return credential(mode === "no-prf" ? {} : { prf: { results: { first: evalPrf(publicKey) } } });
    },
  };
  Object.defineProperty(globalThis, "navigator", { value: { credentials }, configurable: true, writable: true });
  return calls;
}

const v = V.vectors[3];
const prf = hexToBytes(v.prf);
const rpId = "wallet.blockula.test";

// Providers that return PRF output at creation: one prompt.
{
  const calls = authenticator("prf-on-create", prf);
  const w = await createPasskeyWallet({ username: "vlad", rpId });
  assert.equal(w.address, v.address);
  assert.deepEqual(w.credentialId, RAW_ID);
  assert.equal(calls.length, 1);
  const pk = calls[0].publicKey;
  assert.equal(pk.rp.id, rpId);
  assert.equal(pk.authenticatorSelection.residentKey, "required");
  assert.equal(pk.authenticatorSelection.userVerification, "required");
}

// Providers that only evaluate PRF on get() (prf.enabled at creation): a second prompt, same wallet.
{
  const calls = authenticator("prf-on-get-only", prf);
  const w = await createPasskeyWallet({ username: "vlad", rpId });
  assert.equal(w.address, v.address);
  assert.deepEqual(calls.map((c) => c.kind), ["create", "get"]);
  assert.equal(calls[1].publicKey.rpId, rpId);
  assert.deepEqual(new Uint8Array(calls[1].publicKey.allowCredentials[0].id), RAW_ID, "get() targets the new credential");
}

// Unlocking: with a remembered credential id it's targeted; without, the OS picker is shown.
{
  const calls = authenticator("prf-on-create", prf);
  assert.equal((await unlockPasskeyWallet({ rpId, credentialId: RAW_ID })).keyId, v.key_id);
  assert.equal((await unlockPasskeyWallet({ rpId })).keyId, v.key_id);
  assert.equal(calls[0].publicKey.userVerification, "required");
  assert.equal(calls[0].publicKey.allowCredentials.length, 1);
  assert.equal(calls[1].publicKey.allowCredentials.length, 0);
}

// Providers without PRF fail fast with an actionable message, after one prompt.
{
  const calls = authenticator("no-prf", prf);
  await assert.rejects(createPasskeyWallet({ username: "vlad", rpId }), /PRF extension/);
  assert.equal(calls.length, 1);
  await assert.rejects(unlockPasskeyWallet({ rpId }), /PRF extension/);
}
console.log("✓ passkey create/unlock: PRF salt, options, PRF-on-get fallback, no-PRF error");
