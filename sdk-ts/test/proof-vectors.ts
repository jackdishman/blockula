// State proof vectors: the SDK's light-client checks (src/proof.ts) against roots, proofs and
// records the node made (proof-vectors.json, written and checked by node/src/state/proof_vectors.rs).
// No node needed: `npm run test:vectors -w @blockula/sdk`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { bytesToHex, hexToBytes } from "@noble/hashes/utils.js";
import { EMPTY_HASH, NAMESPACES, checkProofResponse, decodeAccount, decodeName, keyHash, leafHash, rootOf, valueHash, verifyProof, type Proof, type ProofResponse } from "../src/index.js";

type Rec = [number, string, string];
interface Case { label: string; namespace: number; key: string; value: string | null; proof: Proof; valid: boolean }
interface Section { records: Rec[]; root: string; proofs: Case[] }
const V = JSON.parse(readFileSync(new URL("./proof-vectors.json", import.meta.url), "utf8")) as {
  tree_golden: { leaves: [string, string][]; root: string };
  tree: Section;
  state: Section & { alice: { balance: string; nonce: number } };
  records: { account: { value: string; balance: string; nonce: number; keys: unknown[] }; name: { value: string; name: string; owner: string; registered_at: number; expires_at: number; offered_to: string; grace_blocks: number } };
};

const be32 = (i: number) => new Uint8Array([i >>> 24, (i >>> 16) & 255, (i >>> 8) & 255, i & 255]);
const le32 = (i: number) => be32(i).reverse();
const hex = bytesToHex;

// tree::tests::golden_roots: the leaves derive the same way here, and their root is the node's.
const golden = [0, 1, 2, 3, 4].map((i) => [keyHash(1, be32(i)), valueHash(le32(i))] as [Uint8Array, Uint8Array]);
assert.deepEqual(golden.map(([k, v]) => [hex(k), hex(v)]).sort(), V.tree_golden.leaves);
assert.equal(hex(rootOf(golden)), "2daa89cfecc65603ee75d4fbdbad2f57d267e26634a7d8d957f39812c786a6b8");
assert.equal(hex(rootOf(golden)), V.tree_golden.root);
assert.deepEqual(rootOf([]), EMPTY_HASH);
assert.equal(hex(rootOf([golden[1]])), hex(leafHash(...golden[1])));
assert.throws(() => rootOf([golden[0], golden[0]]), /duplicate/);

/** A section's root from its records, then every proof as the node judged it. */
function checkSection(name: string, s: Section) {
  const leaves = s.records.map(([n, k, v]) => [keyHash(n, hexToBytes(k)), valueHash(hexToBytes(v))] as [Uint8Array, Uint8Array]);
  assert.equal(hex(rootOf(leaves)), s.root, `${name}: root from records`);
  for (const c of s.proofs) {
    const value = c.value === null ? null : hexToBytes(c.value);
    assert.equal(verifyProof(s.root, c.namespace, hexToBytes(c.key), value, c.proof), c.valid, `${name}: ${c.label}`);
  }
  return s.proofs.length;
}
let cases = checkSection("tree", V.tree) + checkSection("state", V.state);

// Tampering: every valid proof stops verifying if any part of it changes.
for (const c of V.tree.proofs.filter((c) => c.valid)) {
  const key = hexToBytes(c.key);
  const value = c.value === null ? null : hexToBytes(c.value);
  const ok = (p: Proof, root = V.tree.root) => verifyProof(root, c.namespace, key, value, p);
  assert.ok(ok(c.proof), c.label);
  const flipped = structuredClone(c.proof);
  flipped.siblings[0] = (flipped.siblings[0][0] === "0" ? "1" : "0") + flipped.siblings[0].slice(1);
  assert.ok(!ok(flipped), `${c.label}: tampered sibling`);
  assert.ok(!ok({ ...c.proof, siblings: c.proof.siblings.slice(1) }), `${c.label}: missing sibling`);
  assert.ok(!ok({ ...c.proof, siblings: [...c.proof.siblings, "00".repeat(32)] }), `${c.label}: extra sibling`);
  assert.ok(!ok({ ...c.proof, siblings: c.proof.siblings.map((s) => s.toUpperCase()) }), `${c.label}: non-canonical hex`);
  assert.ok(!ok({ ...c.proof, siblings: [...c.proof.siblings.slice(0, -1), "zz"] }), `${c.label}: malformed hex`);
  assert.ok(!ok({ ...c.proof, siblings: Array(257).fill("00".repeat(32)) }), `${c.label}: deeper than 256`);
  assert.ok(!ok(c.proof, "00".repeat(32)), `${c.label}: another root`);
  assert.ok(!ok(c.proof, "nothex"), `${c.label}: malformed root`);
  if (c.proof.leaf) {
    assert.ok(!ok({ ...c.proof, leaf: null }), `${c.label}: leaf dropped`);
    assert.ok(!ok({ ...c.proof, leaf: { ...c.proof.leaf, value: "00".repeat(32) } }), `${c.label}: leaf value swapped`);
  }
  cases += 1;
}
// Exclusion proofs never prove a value, and an empty tree proves everything absent.
assert.ok(verifyProof(EMPTY_HASH, 1, "anything", null, { siblings: [], leaf: null }));
assert.ok(!verifyProof(EMPTY_HASH, 1, "anything", new Uint8Array(), { siblings: [], leaf: null }));
assert.ok(!verifyProof(EMPTY_HASH, 256, "anything", null, { siblings: [], leaf: null }), "namespaces are bytes");
assert.ok(!verifyProof(EMPTY_HASH, 1, "anything", null, null as unknown as Proof), "no proof");
console.log(`✓ ${cases} state proof vectors (tree root 2daa89cf…, state root ${V.state.root.slice(0, 8)}…) match the node`);

// The golden state's account decodes as the node wrote it, and /api/proof responses are checked
// for the record that was asked for.
const alice = V.state.proofs.find((c) => c.label === "account boo1alice") as Case;
const resp: ProofResponse = { namespace: NAMESPACES.ACCOUNTS, key: hex(new TextEncoder().encode("boo1alice")), value: alice.value, proof: alice.proof, root: V.state.root, height: 9 };
const proven = checkProofResponse(resp, NAMESPACES.ACCOUNTS, "boo1alice");
assert.equal(proven.height, 9);
const acct = decodeAccount(proven.value as Uint8Array);
assert.deepEqual([acct.balance.toString(), acct.nonce, acct.keys], [V.state.alice.balance, V.state.alice.nonce, []]);
assert.throws(() => checkProofResponse(resp, NAMESPACES.ACCOUNTS, "boo1bob"), /different record/);
assert.throws(() => checkProofResponse(resp, NAMESPACES.NAMES, "boo1alice"), /different record/);
assert.throws(() => checkProofResponse({ ...resp, value: "00" }, NAMESPACES.ACCOUNTS, "boo1alice"), /doesn't verify/);
assert.throws(() => checkProofResponse({ ...resp, value: null }, NAMESPACES.ACCOUNTS, "boo1alice"), /doesn't verify/);
assert.throws(() => checkProofResponse({ ...resp, root: "11".repeat(32) }, NAMESPACES.ACCOUNTS, "boo1alice"), /doesn't verify/);
assert.throws(() => checkProofResponse({ ...resp, value: "xyz" }, NAMESPACES.ACCOUNTS, "boo1alice"), /isn't hex/);
assert.throws(() => checkProofResponse({ ...resp, height: -1 }, NAMESPACES.ACCOUNTS, "boo1alice"), /height/);
const nobody = V.state.proofs.find((c) => c.label === "boo1nobody has no account") as Case;
assert.equal(checkProofResponse({ ...resp, key: nobody.key, value: null, proof: nobody.proof }, NAMESPACES.ACCOUNTS, "boo1nobody").value, null);

// Record decoders: an account with an owner and a session key, and a name record with its expiry.
const a = decodeAccount(hexToBytes(V.records.account.value));
assert.deepEqual({ balance: a.balance.toString(), nonce: a.nonce, keys: a.keys }, { balance: V.records.account.balance, nonce: V.records.account.nonce, keys: V.records.account.keys });
assert.throws(() => decodeAccount(hexToBytes(V.records.account.value).slice(0, 20)), /ends early/);
const n = decodeName(hexToBytes(V.records.name.value));
assert.deepEqual(n, { name: V.records.name.name, owner: V.records.name.owner, registered_at: V.records.name.registered_at, expires_at: V.records.name.expires_at, offered_to: V.records.name.offered_to, grace_blocks: V.records.name.grace_blocks });
// Older records end after the expiry (before schema 6) or after the offer (schema 6, before the
// grace was recorded in 7); one cut inside the expiry, the offer or the grace is refused.
const nameBytes = hexToBytes(V.records.name.value);
const graceLen = 8;
const offerLen = 1 + 4 + V.records.name.offered_to.length;
const head = nameBytes.slice(0, -offerLen - graceLen);
assert.deepEqual(decodeName(head), { name: V.records.name.name, owner: V.records.name.owner, registered_at: V.records.name.registered_at, expires_at: V.records.name.expires_at });
assert.deepEqual(decodeName(nameBytes.slice(0, -graceLen)), { name: V.records.name.name, owner: V.records.name.owner, registered_at: V.records.name.registered_at, expires_at: V.records.name.expires_at, offered_to: V.records.name.offered_to });
assert.equal(decodeName(Uint8Array.from([...head, 0])).offered_to, undefined);
assert.deepEqual(decodeName(Uint8Array.from([...head, 0, ...nameBytes.slice(-graceLen)])), { name: V.records.name.name, owner: V.records.name.owner, registered_at: V.records.name.registered_at, expires_at: V.records.name.expires_at, grace_blocks: V.records.name.grace_blocks });
assert.throws(() => decodeName(head.slice(0, -1)), /ends early/, "a name record without its expiry isn't read as one that never expires");
assert.throws(() => decodeName(nameBytes.slice(0, -graceLen - 1)), /ends early/);
assert.throws(() => decodeName(nameBytes.slice(0, -1)), /ends early/);
assert.throws(() => decodeName(Uint8Array.from([...head, 2])), /bad offered_to tag/);
console.log("✓ proof responses and record decoders");
