// SEC-2 known-answer tests for ML-DSA-65 in @noble/post-quantum, called the way the SDK calls it
// (`ml_dsa65.keygen(seed)`, then `ml_dsa65.sign(msg, secretKey)`: external interface, hedged),
// plus the TS half of the Rust⇄TS cross-implementation vectors. Offline, no node needed:
//
//   npm run test:kat -w @blockula/sdk             check everything
//   npm run test:kat -w @blockula/sdk -- --write  regenerate node/testdata/cross/ts-signed.json
//
// Vectors live in node/testdata (see acvp/README.md and cross/README.md there); node/src/kat.rs
// runs the same files against the Rust node. A failure here is a critical finding.
import { readFileSync, writeFileSync } from "node:fs";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { blake3 } from "@noble/hashes/blake3.js";
import { sha256 } from "@noble/hashes/sha2.js";
import { bytesToHex, hexToBytes, utf8ToBytes } from "@noble/hashes/utils.js";
import { Client, KEY_POP_CONTEXT, POP_CONTEXT, Wallet, keyPopMessage, popMessage, signingBytes, txBytes, verifyKeyGrant, type Tx, type UnsignedTx } from "../src/index.js";

/** Vector files are plain JSON; fields are read by their upstream names. */
type Json = any;

const testdata = (rel: string) => new URL(`../../node/testdata/${rel}`, import.meta.url);
const load = (rel: string): Json => JSON.parse(readFileSync(testdata(rel), "utf8"));
const hx = (s?: string | null) => hexToBytes(s ?? "");
const sha = (b: Uint8Array) => bytesToHex(sha256(b));
const eq = (a: Uint8Array, b: Uint8Array) => bytesToHex(a) === bytesToHex(b);
/** noble throws on a malformed key or a context over 255 bytes where Rust returns false. */
const attempt = <T>(f: () => T, fallback: T) => { try { return f(); } catch { return fallback; } };

// ------------------------------------------------------------------------------------ report

const lines: string[] = [];
const failures: string[] = [];
let cases = 0;

/** Runs `check` on every case; it returns an error string (or throws) to fail. */
function group(label: string, items: Json[], check: (t: Json) => string | void, expect?: number) {
  let pass = 0;
  for (const t of items) {
    let err: string | void;
    try { err = check(t); } catch (e) { err = `threw ${(e as Error).message}`; }
    if (err) failures.push(`${label} tc ${t.tcId ?? t.label}: ${err}`);
    else pass++;
  }
  cases += items.length;
  lines.push(`${label}: ${pass}/${items.length} passed`);
  if (expect !== undefined && items.length !== expect) failures.push(`${label}: expected ${expect} cases, found ${items.length}`);
}

/** Asserts a vector file's case count, so a regenerated file can't silently drop cases. */
function expectCases(file: string, n: number, run: () => void) {
  const before = cases;
  run();
  if (cases - before !== n) failures.push(`${file}: expected ${n} cases, found ${cases - before}; update the count deliberately`);
}

/** Wycheproof has many one-test groups: flatten and report per upstream source. */
function bySource(file: Json): [string, Json[]][] {
  const out = new Map<string, Json[]>();
  for (const g of file.testGroups) {
    const list = out.get(g.source.name) ?? [];
    for (const t of g.tests) list.push({ ...t, group: g });
    out.set(g.source.name, list);
  }
  return [...out];
}

const keys = (seed: Uint8Array) => ml_dsa65.keygen(seed);

// ------------------------------------------------------------------------------------ ACVP

expectCases("acvp/ML-DSA-65-keyGen.json", 25, () => {
  for (const g of load("acvp/ML-DSA-65-keyGen.json").testGroups) {
    group(`ACVP keyGen tg${g.tgId}`, g.tests, (t) => {
      const { publicKey, secretKey } = keys(hx(t.seed));
      if (sha(publicKey) !== t.pkSha256) return "public key";
      if (sha(secretKey) !== t.skSha256) return "expanded secret key";
      if (sha(Wallet.fromSeed(t.seed).publicKey) !== t.pkSha256) return "Wallet.fromSeed public key";
    });
  }
});

expectCases("acvp/ML-DSA-65-sigGen.json", 90, () => {
  for (const g of load("acvp/ML-DSA-65-sigGen.json").testGroups) {
    const { signatureInterface: iface, externalMu: mu, deterministic: det } = g;
    group(`ACVP sigGen tg${g.tgId} ${iface}${mu ? " µ" : ""} ${det ? "deterministic" : "hedged"}`, g.tests, (t) => {
      const { publicKey, secretKey } = keys(hx(t.seed));
      if (sha(publicKey) !== t.pkSha256) return "public key";
      const extraEntropy = det ? (false as const) : hx(t.rnd);
      let sig: Uint8Array;
      if (mu) sig = ml_dsa65.internal.sign(hx(t.mu), secretKey, { externalMu: true, extraEntropy });
      else if (iface === "internal") sig = ml_dsa65.internal.sign(hx(t.message), secretKey, { extraEntropy });
      else {
        sig = ml_dsa65.sign(hx(t.message), secretKey, { context: hx(t.context), extraEntropy });
        // `Wallet.sign` passes no options at all: the default context must be the empty one.
        if (!t.context && !eq(ml_dsa65.sign(hx(t.message), secretKey, { extraEntropy }), sig)) return "default context isn't empty";
      }
      if (sha(sig) !== t.signatureSha256) return "signature differs from NIST's";
    });
  }
});

expectCases("acvp/ML-DSA-65-sigVer.json", 45, () => {
  for (const g of load("acvp/ML-DSA-65-sigVer.json").testGroups) {
    const { signatureInterface: iface, externalMu: mu } = g;
    group(`ACVP sigVer tg${g.tgId} ${iface}${mu ? " µ" : ""}`, g.tests, (t) => {
      const [sig, pk] = [hx(t.signature), hx(t.pk)];
      const got = mu ? ml_dsa65.internal.verify(sig, hx(t.mu), pk, { externalMu: true })
        : iface === "internal" ? ml_dsa65.internal.verify(sig, hx(t.message), pk)
        : ml_dsa65.verify(sig, hx(t.message), pk, { context: hx(t.context) });
      if (got !== t.testPassed) return `got ${got}, NIST says ${t.testPassed} (${t.reason})`;
    });
  }
});

// ------------------------------------------------------------------------------------ Wycheproof

expectCases("wycheproof/mldsa_65_verify_test.json", 210, () => {
  for (const [source, tests] of bySource(load("wycheproof/mldsa_65_verify_test.json"))) {
    group(`Wycheproof verify ${source}`, tests, (t) => {
      if (t.result === "acceptable") return;
      const got = attempt(() => ml_dsa65.verify(hx(t.sig), hx(t.msg), hx(t.group.publicKey), { context: hx(t.ctx) }), false);
      if (got !== (t.result === "valid")) return `got ${got}, want ${t.result} (${t.comment})`;
    });
  }
});

expectCases("wycheproof/mldsa_65_sign_seed_test.json", 105, () => {
  for (const [source, tests] of bySource(load("wycheproof/mldsa_65_sign_seed_test.json"))) {
    group(`Wycheproof sign_seed ${source}`, tests, (t) => {
      const got = attempt(() => {
        const { publicKey, secretKey } = keys(hx(t.group.privateSeed)); // throws on a wrong seed length
        if (t.group.publicKeySha256 && sha(publicKey) !== t.group.publicKeySha256) return false;
        const sig = t.msg !== undefined
          ? ml_dsa65.sign(hx(t.msg), secretKey, { context: hx(t.ctx), extraEntropy: t.rnd ? hx(t.rnd) : false })
          : ml_dsa65.internal.sign(hx(t.mu), secretKey, { externalMu: true, extraEntropy: false });
        return sha(sig) === t.sigSha256;
      }, false);
      if (got !== (t.result === "valid")) return `got ${got}, want ${t.result} (${t.comment})`;
    });
  }
});

// ------------------------------------------------------------------------------------ Rust ⇄ TS

const rust = load("cross/rust-signed.json");
const CHAIN: string = rust.chain_id;
const GENESIS: string = rust.genesis_hash;
const WRITE = process.argv.includes("--write");

/** A client pinned to the testnet prefix (never contacted: `Wallet.address` reads its prefix). */
const testnet = new Client("http://127.0.0.1:9", { prefix: "tboo" });
const keyJson = (seed: string) => {
  const w = Wallet.fromSeed(seed);
  return { seed, public_key_sha256: sha(w.publicKey), key_id: w.keyId, address: w.address, testnet_address: Wallet.fromSeed(seed, testnet).address };
};
const stale = (label: string) => `${label}: encoding changed; regenerate node/testdata/cross (see its README.md)`;
/** pq.rs signs with the namespace as context, hashed with BLAKE3 when over 255 bytes. */
const contextFor = (ns: Uint8Array) => (ns.length <= 255 ? ns : blake3(ns));
/** A signed tx minus its key and signature: what `signingBytes` encodes. */
const unsigned = ({ public_key: _pk, signature: _sig, ...tx }: Tx): UnsignedTx => tx;

/** The SDK's own signatures over its own and the node's messages (`ts-signed.json`). */
async function tsSigned(): Promise<Json> {
  const out: Json[] = [];
  const push = (label: string, seed: string, ns: Uint8Array, msg: Uint8Array, sig: Uint8Array, extra: Json = {}) =>
    out.push({ label, seed, namespace: bytesToHex(ns), ctx: bytesToHex(contextFor(ns)), msg: bytesToHex(msg), sig: bytesToHex(sig), ...extra });
  const det = (seed: string, ns: Uint8Array, msg: Uint8Array) =>
    bytesToHex(ml_dsa65.sign(msg, keys(hx(seed)).secretKey, { context: contextFor(ns), extraEntropy: false }));
  // Txs (v3) through `Wallet.sign` (hedged, fresh randomness, empty context), with their wire
  // form; one from the same key's testnet (`tboo`) address.
  const owner = "07".repeat(32);
  const w = Wallet.fromSeed(owner);
  const wt = Wallet.fromSeed(owner, testnet);
  for (const [label, signer, action] of [
    ["tx/transfer", w, { type: "transfer", to: "bob.boo", amount: "2500000000" }],
    ["tx/register_name", w, { type: "register_name", name: "vampire" }],
    ["tx/transfer-tboo", wt, { type: "transfer", to: Wallet.fromSeed("ff".repeat(32), testnet).address, amount: "7" }],
  ] as const) {
    const tx = signer.sign({ chain_id: CHAIN, genesis_hash: GENESIS, from: signer.address, nonce: 3, fee: "200000", tip: "1000", valid_until: 777, action, memo: "from the SDK" });
    const msg = signingBytes(tx);
    push(label, owner, new Uint8Array(), msg, hx(tx.signature), { sig_det: det(owner, new Uint8Array(), msg), tx, wire: bytesToHex(txBytes(tx)) });
  }
  // A validator PoP built by the SDK.
  const v = rust.cases.find((c: Json) => c.label === "validator-pop").seed as string;
  const pop = { chain_id: CHAIN, owner: w.address, p2p_addr: "198.51.100.4:7071", http_addr: "https://sdk-validator.example" };
  const popMsg = popMessage(pop.chain_id, pop.owner, pop.p2p_addr, pop.http_addr);
  push("validator-pop", v, POP_CONTEXT, popMsg, ml_dsa65.sign(popMsg, keys(hx(v)).secretKey, { context: POP_CONTEXT }), { sig_det: det(v, POP_CONTEXT, popMsg), pop });
  // A key's consent to being added to the owner's account (AddKey's pop), made by `Wallet.keyGrant`.
  const added = "ff".repeat(32);
  const grant = await Wallet.fromSeed(added).keyGrant(w.address, { chainId: CHAIN, genesisHash: GENESIS });
  const key_pop = { chain_id: CHAIN, genesis_hash: GENESIS, account: grant.account, key_id: grant.key_id };
  const keyPopMsg = keyPopMessage(CHAIN, GENESIS, grant.account, grant.key_id);
  push("key-pop", added, KEY_POP_CONTEXT, keyPopMsg, hx(grant.pop), { sig_det: det(added, KEY_POP_CONTEXT, keyPopMsg), key_pop });
  // The node's vote, handoff and edge-case messages, signed hedged; all four validators sign
  // the finalize vote so the Rust side can assemble a certificate from SDK signatures.
  const validators = ["0b", "0c", "0d", "0e"].map((b) => b.repeat(32));
  for (const c of rust.cases.filter((c: Json) => !c.tx && !c.pop && !c.key_pop)) {
    const signers = c.label === "consensus/finalize" ? validators
      : c.label === "consensus/notarize" ? [validators[1]]
      : c.label === "consensus/nullify" ? [validators[2]]
      : [c.seed as string];
    for (const seed of signers) {
      push(c.label, seed, hx(c.namespace), hx(c.msg), ml_dsa65.sign(hx(c.msg), keys(hx(seed)).secretKey, { context: hx(c.ctx) }));
    }
  }
  return {
    generator: "sdk-ts/test/kat.ts --write (@noble/post-quantum 0.7.1); see README.md",
    chain_id: CHAIN,
    genesis_hash: GENESIS,
    keys: rust.keys.map((k: Json) => keyJson(k.seed)),
    cases: out,
  };
}

if (WRITE) {
  writeFileSync(testdata("cross/ts-signed.json"), JSON.stringify(await tsSigned(), null, 2) + "\n");
  console.log("wrote node/testdata/cross/ts-signed.json");
}
const ts = load("cross/ts-signed.json");

group("cross seed → key id / address (Rust-derived)", rust.keys, (k) => {
  const want = keyJson(k.seed);
  if (JSON.stringify(want) !== JSON.stringify({ seed: k.seed, public_key_sha256: k.public_key_sha256, key_id: k.key_id, address: k.address, testnet_address: k.testnet_address })) return `TS derives ${JSON.stringify(want)}`;
}, 5);

// Rust signed (deterministically, through the node's own paths): noble accepts every signature,
// rejects it under another context, and reproduces it byte for byte.
group("cross rust-signed", rust.cases, (c) => {
  const { publicKey, secretKey } = keys(hx(c.seed));
  const [ns, ctx, msg, sig] = [hx(c.namespace), hx(c.ctx), hx(c.msg), hx(c.sig)];
  if (!eq(contextFor(ns), ctx)) return "context isn't the namespace (or BLAKE3 of it)";
  if (!ml_dsa65.verify(sig, msg, publicKey, { context: ctx })) return "noble rejects the node's signature";
  if (ml_dsa65.verify(sig, msg, publicKey, { context: utf8ToBytes("blockula/other") })) return "verifies under another context";
  if (!eq(ml_dsa65.sign(msg, secretKey, { context: ctx, extraEntropy: false }), sig)) return "deterministic signatures differ between Rust and TS";
  if (c.tx && !eq(hx(c.tx.public_key), publicKey)) return "tx public key";
}, 12);

// The SDK-signed file still verifies and reproduces (the node checks it in node/src/kat.rs).
group("cross ts-signed", ts.cases, (c) => {
  const { publicKey, secretKey } = keys(hx(c.seed));
  const [ctx, msg, sig] = [hx(c.ctx), hx(c.msg), hx(c.sig)];
  if (!eq(contextFor(hx(c.namespace)), ctx)) return "context";
  if (!ml_dsa65.verify(sig, msg, publicKey, { context: ctx })) return "signature";
  if (c.sig_det && !eq(ml_dsa65.sign(msg, secretKey, { context: ctx, extraEntropy: false }), hx(c.sig_det))) return "deterministic signature";
}, 15);
group("cross seed → key id / address (TS-derived)", ts.keys, (k) => {
  if (JSON.stringify(keyJson(k.seed)) !== JSON.stringify(k)) return "derivation changed";
}, 5);

// Message formats: the SDK encodes the node's txs and PoPs to the same bytes (for both files),
// and signed the node's current vote and handoff messages.
group("cross formats", [...rust.cases, ...ts.cases], (c) => {
  if (c.tx && bytesToHex(signingBytes(unsigned(c.tx))) !== c.msg) return stale(c.label);
  if (c.tx && bytesToHex(txBytes(c.tx)) !== c.wire) return `${stale(c.label)} (wire form)`;
  if (c.pop) {
    if (bytesToHex(popMessage(c.pop.chain_id, c.pop.owner, c.pop.p2p_addr, c.pop.http_addr)) !== c.msg) return stale(c.label);
    if (bytesToHex(POP_CONTEXT) !== c.namespace) return stale(c.label);
  }
  // A key's consent (AddKey's pop): the node's message and context, and the SDK accepts it.
  if (c.key_pop) {
    const k = c.key_pop;
    if (bytesToHex(keyPopMessage(k.chain_id, k.genesis_hash, k.account, k.key_id)) !== c.msg || bytesToHex(KEY_POP_CONTEXT) !== c.namespace) return stale(c.label);
    if (!verifyKeyGrant({ account: k.account, key_id: k.key_id, public_key: bytesToHex(keys(hx(c.seed)).publicKey), pop: c.sig }, k.chain_id, k.genesis_hash)) return "verifyKeyGrant rejects it";
  }
  if (c.tx?.action?.type === "add_key") {
    const a = c.tx.action;
    if (!verifyKeyGrant({ account: c.tx.from, key_id: a.key_id, public_key: a.public_key, pop: a.pop }, c.tx.chain_id, c.tx.genesis_hash)) return `${c.label}: verifyKeyGrant rejects the AddKey's pop`;
  }
  if (!c.tx && !c.pop) {
    const r = rust.cases.find((x: Json) => x.label === c.label);
    if (!r || r.msg !== c.msg || r.namespace !== c.namespace) return stale(c.label);
  }
});

// ------------------------------------------------------------------------------------ result

for (const l of lines) console.log(l);
if (failures.length) {
  console.error(`\n✗ ${failures.length} of ${cases} ML-DSA-65 cases failed (critical, see SEC-2):\n${failures.join("\n")}`);
  process.exit(1);
}
console.log(`✓ ML-DSA-65 known answers: ${cases} cases (@noble/post-quantum)`);
