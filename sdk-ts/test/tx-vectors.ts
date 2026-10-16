// Signing-bytes vectors: the SDK's encoder (src/tx.ts `signingBytes`) against the node's
// (node/src/types.rs `Tx::signing_bytes`), offline. tx-vectors.json is written and checked by
// the node's `types::tests::tx_vectors` (every action tag, and txs at the bounds of every integer
// width). Then the encoder must refuse what the node's JSON parser refuses, instead of wrapping
// it into a different, valid tx (audit F70).
// No node needed: `npm run test:vectors -w @blockula/sdk`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { bytesToHex } from "@noble/hashes/utils.js";
import { Wallet, beaconCommitment, seanceCommitment, signingBytes, txBytes, u64Field, type Action, type KeyRole, type UnsignedTx } from "../src/index.js";

const V = JSON.parse(readFileSync(new URL("./tx-vectors.json", import.meta.url), "utf8")) as { cases: { label: string; tx: UnsignedTx; signing_bytes: string }[] };
for (const c of V.cases) assert.equal(bytesToHex(signingBytes(c.tx)), c.signing_bytes, c.label);
for (let tag = 0; tag <= 54; tag++) assert.ok(V.cases.some((c) => c.label.endsWith(`(tag ${tag})`)), `tag ${tag} has a vector`);
console.log(`✓ ${V.cases.length} tx encodings match the node byte for byte (every action tag, integers at their bounds, non-ASCII strings)`);

// ---- out-of-range integers are refused, never wrapped ----

const base = V.cases.find((c) => c.label === "tx integers at zero")!.tx;
const tx = (patch: Partial<UnsignedTx>): UnsignedTx => ({ ...base, ...patch });
const act = (action: Action) => tx({ action });
const session = (limit: unknown, extra: Partial<Extract<KeyRole, { type: "session" }>> = {}): Action =>
  ({ type: "add_key", key_id: "cd".repeat(32), label: "agent", role: { type: "session", limit, period_blocks: 10, expires_at: 10, ...extra } }) as Action;
const MAX = (1n << 64n) - 1n;

// The finding's examples: a "-1" limit would have become u64::MAX (an unlimited session key), and
// 2^64 + 1 drops would have become 1.
assert.throws(() => signingBytes(act(session("-1"))), /must be a decimal string/);
assert.throws(() => signingBytes(act({ type: "transfer", to: "x.boo", amount: (MAX + 2n).toString() })), /must be 0 to 18446744073709551615/);
// The bounds themselves encode.
signingBytes(act(session(MAX.toString())));
signingBytes(act({ type: "transfer", to: "x.boo", amount: MAX.toString() }));

const refused: [string, UnsignedTx, RegExp][] = [
  ["amount 2^64", act({ type: "transfer", to: "x.boo", amount: (MAX + 1n).toString() }), /0 to 18446744073709551615/],
  ["negative amount", act({ type: "transfer", to: "x.boo", amount: "-5" }), /decimal string/],
  ["empty amount", act({ type: "transfer", to: "x.boo", amount: "" }), /decimal string/],
  ["hex amount", act({ type: "transfer", to: "x.boo", amount: "0x10" }), /decimal string/],
  ["padded amount", act({ type: "transfer", to: "x.boo", amount: " 1" }), /decimal string/],
  ["fractional amount", act({ type: "transfer", to: "x.boo", amount: "1.5" }), /decimal string/],
  ["amount as a fractional number", act({ type: "transfer", to: "x.boo", amount: 1.5 as unknown as string }), /whole number/],
  ["amount as a negative number", act({ type: "transfer", to: "x.boo", amount: -1 as unknown as string }), /0 to/],
  ["amount missing", act({ type: "transfer", to: "x.boo" } as Action), /number or a decimal string/],
  ["negative nonce", tx({ nonce: -1 }), /nonce must be 0 to/],
  ["fractional nonce", tx({ nonce: 1.5 }), /nonce must be a whole number/],
  ["NaN valid_until", tx({ valid_until: NaN }), /valid_until must be a whole number/],
  ["valid_until 2^64", tx({ valid_until: 2 ** 64 }), /valid_until must be 0 to/],
  ["negative fee", tx({ fee: "-1" }), /fee must be a decimal string/],
  ["fee 2^64", tx({ fee: (MAX + 1n).toString() }), /fee must be 0 to/],
  ["tip null", tx({ tip: null as unknown as string }), /tip must be a number or a decimal string/],
  ["memo null", tx({ memo: null as unknown as string }), /memo must be a string/],
  ["session period -1", act(session("1", { period_blocks: -1 })), /0 to/],
  ["session expiry 2^64", act(session("1", { expires_at: 2 ** 64 })), /0 to/],
  ["nonce 2^53 (not exact as a number)", tx({ nonce: 2 ** 53 }), /nonce must be 0 to 9007199254740991 as a number/],
  ["conviction 256", act({ type: "vote", proposal_id: 1, aye: true, amount: "1", conviction: 256 }), /conviction must be 0 to 255/],
  ["conviction -1", act({ type: "vote", proposal_id: 1, aye: true, amount: "1", conviction: -1 }), /conviction must be 0 to 255/],
  ["aye not a bool", act({ type: "vote", proposal_id: 1, aye: "yes" as unknown as boolean, amount: "1", conviction: 1 }), /aye must be true or false/],
  ["years 256", act({ type: "renew_name", name: "vlad", years: 256 }), /years must be 0 to 255/],
  ["threshold 1.5", act({ type: "multisig_create", owners: [], threshold: 1.5 }), /threshold must be a whole number/],
  ["upgrade version 2^32", act({ type: "propose", kind: { type: "schedule_upgrade", version: 2 ** 32, activation_height: 1 }, deposit: "1", title: "t" }), /version must be 0 to 4294967295/],
  ["negative hunt id", act({ type: "fund_hunt", hunt_id: -1, amount: "1" }), /0 to/],
  ["negative spam id", act({ type: "reveal_votes", seance_id: 1, votes: "", spam: [-1], salt: "" }), /0 to/],
  ["param value 2^64", act({ type: "propose", kind: { type: "param_change", key: "k", value: (MAX + 1n).toString() }, deposit: "1", title: "t" }), /0 to/],
  ["label not a string", act({ type: "add_key", key_id: "ab".repeat(32), label: 5 as unknown as string, role: { type: "owner" } }), /must be a string/],
];
for (const [label, t, re] of refused) assert.throws(() => signingBytes(t), re, label);

// Wallet.sign and txBytes go through the same encoder: nothing out of range gets signed.
const w = Wallet.fromSeed("07".repeat(32));
assert.throws(() => w.sign(act(session("-1"))), /decimal string/);
const signed = w.sign(act(session("5")));
assert.throws(() => txBytes({ ...signed, action: session("-1") }), /decimal string/);
// The other encoders too.
assert.throws(() => seanceCommitment(-1, "boo1juror", "a", [], "s"), /0 to/);
assert.throws(() => seanceCommitment(1, "boo1juror", "a", [2 ** 64], "s"), /0 to/);
assert.throws(() => beaconCommitment("c", -1, "boo1v", "ab".repeat(32)), /epoch must be 0 to/);
assert.equal(u64Field("18446744073709551615"), MAX);
assert.equal(u64Field(0), 0n);
assert.throws(() => u64Field(1n << 64n), /0 to/);
console.log(`✓ ${refused.length + 8} out-of-range or mistyped fields refused before encoding (nothing wraps)`);
