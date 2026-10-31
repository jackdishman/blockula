// Signing-bytes vectors: the SDK's encoder (src/tx.ts `signingBytes`) against the node's
// (node/src/types.rs `Tx::signing_bytes`), offline. tx-vectors.json is written and checked by
// the node's `types::tests::tx_vectors` (every action tag, and txs at the bounds of every integer
// width). Then the encoder must refuse what the node's JSON parser refuses, instead of wrapping
// it into a different, valid tx (audit F70).
// No node needed: `npm run test:vectors -w @blockula/sdk`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { bytesToHex } from "@noble/hashes/utils.js";
import { Wallet, actionDigest, beaconCommitment, describeAction, isProgramAction, keyPopMessage, proposalDigest, reviewAction, seanceCommitment, seancePairsDigest, signingBytes, txBytes, u64Field, type Action, type KeyRole, type ProposalKind, type SeanceClaimRef, type UnsignedTx } from "../src/index.js";

type Case = { label: string; tx: UnsignedTx; signing_bytes: string };
// `program_cases`: the program actions (tags 62–64 and 69, docs/PROGRAMS-V0.md §2), every option
// set and unset, and at their bounds (node/src/types.rs `program_vector_actions`). Their labels are
// distinct (the node asserts it), so a failure names its case; `cases`' labels repeat.
const V = JSON.parse(readFileSync(new URL("./tx-vectors.json", import.meta.url), "utf8")) as { cases: Case[]; program_cases: Case[] };
for (const c of V.cases) assert.equal(bytesToHex(signingBytes(c.tx)), c.signing_bytes, c.label);
for (const c of V.program_cases) assert.equal(bytesToHex(signingBytes(c.tx)), c.signing_bytes, c.label);
assert.equal(new Set(V.program_cases.map((c) => c.label)).size, V.program_cases.length, "program vector labels are distinct");
// Tags 55–59 are Ghost Mode's, unused; 60–61 are a small set's readiness and admission; 62–64 and
// 69 are programs' (65–68 and 70 are reserved for later program actions, and no node decodes them).
const all = [...V.cases, ...V.program_cases];
for (const tag of [...Array(55).keys(), 60, 61, 62, 63, 64, 69]) assert.ok(all.some((c) => c.label.endsWith(`(tag ${tag})`)), `tag ${tag} has a vector`);
for (const c of V.cases) assert.ok(!isProgramAction(c.tx.action), `${c.label} isn't a program action`);
for (const c of V.program_cases) assert.ok(isProgramAction(c.tx.action), `${c.label} is a program action`);
console.log(`✓ ${all.length} tx encodings match the node byte for byte (every action tag, the ${V.program_cases.length} program vectors among them, integers at their bounds, non-ASCII strings)`);

// Every action the chain takes (and every proposal kind, through propose) has a client-rendered
// description: a multisig owner approving one is never left with "can't read" (audit gap-7-6).
for (const c of all) assert.doesNotMatch(describeAction(c.tx.action), /can't read/, c.label);
console.log("✓ describeAction renders every action tag and proposal kind in the vectors");

// What approvals, votes and sealed judgments bind (audit gap-7-6), and a key's consent (gap-7-2).
const D = V as unknown as {
  action_digests: { action: Action; digest: string }[];
  proposal_digests: { kind: ProposalKind; title: string; url: string; digest: string }[];
  seance_pairs_digest: { seance_id: number; juror: string; pairs: [SeanceClaimRef, SeanceClaimRef][]; digest: string };
  key_pop_message: { chain_id: string; genesis_hash: string; account: string; key_id: string; message: string };
};
for (const c of D.action_digests) assert.equal(actionDigest(c.action), c.digest, `action digest of ${c.action.type}`);
for (const c of D.proposal_digests) assert.equal(proposalDigest(c.kind, c.title, c.url), c.digest, `proposal digest of ${c.kind.type}`);
const sp = D.seance_pairs_digest;
assert.equal(seancePairsDigest(sp.seance_id, sp.juror, sp.pairs), sp.digest, "Séance pairs digest");
const kp = D.key_pop_message;
assert.equal(bytesToHex(keyPopMessage(kp.chain_id, kp.genesis_hash, kp.account, kp.key_id)), kp.message, "key consent message");
console.log(`✓ ${D.action_digests.length + D.proposal_digests.length + 1} approval, vote and judgment digests and the key consent message match the node`);

// ---- out-of-range integers are refused, never wrapped ----

const base = V.cases.find((c) => c.label === "tx integers at zero")!.tx;
const tx = (patch: Partial<UnsignedTx>): UnsignedTx => ({ ...base, ...patch });
const act = (action: Action) => tx({ action });
const session = (limit: unknown, extra: Partial<Extract<KeyRole, { type: "session" }>> = {}): Action =>
  ({ type: "add_key", key_id: "cd".repeat(32), label: "agent", role: { type: "session", limit, period_blocks: 10, expires_at: 10, ...extra }, public_key: "0e", pop: "0f" }) as Action;
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
  ["nonce 2^53 (not exact as a number)", tx({ nonce: 2 ** 53 }), /nonce must be 0 to 9007199254740991, got 9007199254740992 \(past 2\^53 − 1/],
  // review-r2-clients-1: whole-number fields the node's JSON carries as numbers are at most
  // 2^53 − 1 (its format rules refuse more), even as strings of digits.
  ["nonce 2^53 as digits", tx({ nonce: "9007199254740992" as unknown as number }), /nonce must be 0 to 9007199254740991, got 9007199254740992/],
  ["distribution id 2^53", act({ type: "propose", kind: { type: "distribution", distribution_id: "9007199254740992" as unknown as number, merkle_root: "ef".repeat(32), total: "1", claim_until: 1 }, deposit: "1", title: "t" }), /distribution_id must be 0 to 9007199254740991/],
  ["claim deadline 2^53", act({ type: "propose", kind: { type: "distribution", distribution_id: 1, merkle_root: "ef".repeat(32), total: "1", claim_until: 2 ** 53 }, deposit: "1", title: "t" }), /claim_until must be 0 to 9007199254740991/],
  ["activation 2^53", act({ type: "propose", kind: { type: "schedule_upgrade", version: 2, activation_height: 2 ** 53 }, deposit: "1", title: "t" }), /activation_height must be 0 to 9007199254740991/],
  ["hunt issue 2^60", act({ type: "create_hunt", repo: "a/b", issue: 2 ** 60, amount: "1", expires_at: 1, title: "t" }), /issue must be 0 to 9007199254740991/],
  ["claim id 2^53 in a claim", act({ type: "claim_distribution", distribution_id: 2 ** 53, amount: "1", proof: [] }), /distribution_id must be 0 to 9007199254740991/],
  ["conviction 256", act({ type: "vote", proposal_id: 1, aye: true, amount: "1", conviction: 256, proposal_digest: "d1".repeat(32) }), /conviction must be 0 to 255/],
  ["conviction -1", act({ type: "vote", proposal_id: 1, aye: true, amount: "1", conviction: -1, proposal_digest: "d1".repeat(32) }), /conviction must be 0 to 255/],
  ["aye not a bool", act({ type: "vote", proposal_id: 1, aye: "yes" as unknown as boolean, amount: "1", conviction: 1, proposal_digest: "d1".repeat(32) }), /aye must be true or false/],
  ["years 256", act({ type: "renew_name", name: "vlad", years: 256 }), /years must be 0 to 255/],
  ["threshold 1.5", act({ type: "multisig_create", owners: [], threshold: 1.5 }), /threshold must be a whole number/],
  ["upgrade version 2^32", act({ type: "propose", kind: { type: "schedule_upgrade", version: 2 ** 32, activation_height: 1 }, deposit: "1", title: "t" }), /version must be 0 to 4294967295/],
  ["negative hunt id", act({ type: "fund_hunt", hunt_id: -1, amount: "1" }), /0 to/],
  ["negative spam id", act({ type: "reveal_votes", seance_id: 1, votes: "", spam: [-1], salt: "" }), /0 to/],
  ["param value 2^64", act({ type: "propose", kind: { type: "param_change", key: "k", value: (MAX + 1n).toString() }, deposit: "1", title: "t" }), /0 to/],
  ["label not a string", act({ type: "add_key", key_id: "ab".repeat(32), label: 5 as unknown as string, role: { type: "owner" }, public_key: "0c", pop: "0d" }), /must be a string/],
];
for (const [label, t, re] of refused) assert.throws(() => signingBytes(t), re, label);

// review-r2-clients-1: what a vote or an approval binds is computed from the node's JSON. The
// largest values the chain allows (2^53 − 1) read back exactly (the vectors above include a
// distribution, upgrades, a claim and a hunt at that bound); a larger number arrives rounded, so
// the digest fails with a message that says so instead of a bare encoder error.
const atBound = D.proposal_digests.find((c) => c.kind.type === "distribution" && c.title === "At the bounds")!;
assert.equal((atBound.kind as { distribution_id: number }).distribution_id, Number.MAX_SAFE_INTEGER, "the vector is at the bound");
const served = (id: string) => JSON.parse(`{"type":"distribution","distribution_id":${id},"merkle_root":"${"ef".repeat(32)}","total":"18446744073709551615","claim_until":9007199254740991}`) as ProposalKind;
assert.equal(proposalDigest(served("9007199254740991"), "At the bounds", ""), atBound.digest, "parsed from the node's JSON text, the largest id gives the node's digest");
for (const id of ["9007199254740992", "9007199254740993", "1152921504606846977"]) {
  assert.throws(() => proposalDigest(served(id), "At the bounds", ""), /^RangeError: can't compute this proposal's digest: distribution_id must be 0 to 9007199254740991, got \d+ \(past 2\^53 − 1: not even exact as a JavaScript number\)\. The chain stores no proposal with a whole number past 2\^53 − 1 .* may be misreporting it/, id);
}
const storedAction = JSON.parse(`{"type":"fund_hunt","hunt_id":9007199254740993,"amount":"1"}`) as Action;
assert.throws(() => actionDigest(storedAction), /can't compute this multisig action's digest: hunt_id must be 0 to 9007199254740991.*misreporting/);
console.log("✓ digests at the JSON-number bound match the node; past it they fail with a clear message");

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

// ---- program actions (docs/PROGRAMS-V0.md §2) ----

type Upload = Extract<Action, { type: "upload_code" }>;
type Create = Extract<Action, { type: "create_program" }>;
type Call = Extract<Action, { type: "call_program" }>;
const program = (label: string) => V.program_cases.find((c) => c.label === label)!.tx.action;
const upload = program("upload_code (tag 62)") as Upload;
const immutable = program("create_program, immutable (tag 63)") as Create;
const create = program("create_program, upgradable, with init and bindings (tag 63)") as Create;
const call = program("call_program, a batch (tag 64)") as Call;
const one = program("call_program, one call (tag 64)") as Call;
const bytesOf = (a: Action) => bytesToHex(signingBytes(act(a)));

// A create writes three one-byte fields in a row: the policy, accepts_transfers and init's presence.
// The create vectors set every two of them apart in at least one vector, so an encoder that writes
// one in another's place can't produce the node's bytes above (review S7a-R1).
const flags = V.program_cases.flatMap((c) => (c.tx.action.type === "create_program" ? [[c.tx.action.policy === "additive", c.tx.action.accepts_transfers, c.tx.action.init != null]] : []));
for (const [i, j] of [[0, 1], [1, 2], [0, 2]]) assert.ok(flags.some((f) => f[i] !== f[j]), `a create vector sets its policy, accepts_transfers and init's presence apart (${i}, ${j})`);

// An option that's absent reads as null, as the node reads the JSON (`Option` fields).
assert.equal(immutable.authority, null);
assert.equal(immutable.init, null);
const { authority: _authority, init: _init, ...bare } = immutable;
assert.equal(bytesOf(bare as Action), bytesOf(immutable), "an absent authority and init encode as null");

// What the node's JSON parser refuses is refused here too, never signed as something else.
const programRefused: [string, Action, RegExp][] = [
  ["wasm in uppercase hex", { ...upload, wasm: upload.wasm.toUpperCase() }, /wasm must be lowercase hex/],
  ["wasm of an odd length", { ...upload, wasm: upload.wasm + "0" }, /wasm must be lowercase hex/],
  ["wasm not a string", { ...upload, wasm: [0, 97] as unknown as string }, /wasm must be a string of lowercase hex/],
  ["interface with a 0x prefix", { ...upload, interface: "0x" + upload.interface }, /interface must be lowercase hex/],
  ["upload storage limit -1", { ...upload, storage_limit: "-1" }, /storage_limit must be a decimal string/],
  ["upload storage limit 2^64", { ...upload, storage_limit: (MAX + 1n).toString() }, /storage_limit must be 0 to 18446744073709551615/],
  // fuel_limit is a JSON number (2^53 − 1 at most, as for the nonce), never a u64, as digits too (review S7a-R2).
  ["create fuel limit 2^53", { ...create, fuel_limit: 2 ** 53 }, /fuel_limit must be 0 to 9007199254740991, got 9007199254740992 \(past 2\^53 − 1/],
  ["create fuel limit 2^53 as digits", { ...create, fuel_limit: "9007199254740992" as unknown as number }, /fuel_limit must be 0 to 9007199254740991, got 9007199254740992$/],
  ["create delay 2^53 as digits", { ...create, upgrade_delay_blocks: "9007199254740992" as unknown as number }, /upgrade_delay_blocks must be 0 to 9007199254740991/],
  ["create delay -1", { ...create, upgrade_delay_blocks: -1 }, /upgrade_delay_blocks must be 0 to/],
  ["an unknown policy", { ...create, policy: "closed" as "open" }, /policy must be "open" or "additive", not "closed"/],
  ["accepts_transfers not a bool", { ...create, accepts_transfers: 1 as unknown as boolean }, /accepts_transfers must be true or false/],
  // "" has an immutable program's bytes, but the node reads it as an authority that isn't an address.
  ["an empty authority", { ...create, authority: "" }, /authority must be an address, or null/],
  ["an authority not a string", { ...create, authority: 5 as unknown as string }, /authority must be a string/],
  ["init not an object", { ...create, init: "init" as unknown as null }, /init must be an object/],
  ["init value -1", { ...create, init: { ...create.init!, value: "-1" } }, /init\.value must be a decimal string/],
  ["init input in uppercase", { ...create, init: { ...create.init!, input: "AB" } }, /init\.input must be lowercase hex/],
  ["bindings missing", { ...create, bindings: undefined as unknown as [] }, /bindings must be an array/],
  ["a binding not an object", { ...create, bindings: [null as unknown as Create["bindings"][number]] }, /each of bindings must be an object/],
  ["256 bindings (past a u8 count)", { ...call, bindings: Array(256).fill(call.bindings[0]) }, /bindings count must be 0 to 255, got 256/],
  ["calls not an array", { ...call, calls: {} as unknown as [] }, /calls must be an array/],
  ["256 calls (past a u8 count)", { ...call, calls: Array(256).fill(call.calls[0]) }, /calls count must be 0 to 255, got 256/],
  ["a call's value 2^64", { ...call, calls: [{ ...call.calls[0], value: (MAX + 1n).toString() }] }, /calls\.value must be 0 to 18446744073709551615/],
  ["a call's input of an odd length", { ...call, calls: [{ ...call.calls[0], input: "abc" }] }, /calls\.input must be lowercase hex/],
  ["a call's program not a string", { ...call, calls: [{ ...call.calls[0], program: null as unknown as string }] }, /calls\.program must be a string/],
  ["a call's fuel limit as a fraction", { ...call, fuel_limit: 1.5 }, /fuel_limit must be a whole number/],
  ["a call's fuel limit 2^53", { ...call, fuel_limit: 2 ** 53 }, /fuel_limit must be 0 to 9007199254740991, got 9007199254740992 \(past 2\^53 − 1/],
  ["a call's fuel limit 2^53 as digits", { ...call, fuel_limit: "9007199254740992" as unknown as number }, /fuel_limit must be 0 to 9007199254740991, got 9007199254740992$/],
  // No program action nests: the node doesn't decode one inside a multisig proposal (§2.1).
  ["a program action in a multisig proposal", { type: "multisig_propose", action: call }, /a multisig proposal can't hold a program action/],
  ["the pause in a multisig proposal", { type: "multisig_propose", action: { type: "programs_pause" } }, /a multisig proposal can't hold a program action/],
];
for (const [label, a, re] of programRefused) assert.throws(() => signingBytes(act(a)), re, label);
// Tags 65–68 and 70 are reserved for later program actions (§2.6): no encoder, no description.
for (const type of ["upgrade_program", "program_admin", "close_program", "remove_code", "add_scoped_key"]) {
  assert.throws(() => signingBytes(act({ type } as unknown as Action)), /unknown action type/, type);
  assert.match(describeAction({ type }), /can't read/, type);
}
console.log(`✓ ${programRefused.length + 5} malformed program actions and reserved program tags refused before encoding`);

// Summaries from structure only (PRG 9.7, node/src/programs/describe.rs): programs by address in
// full, code by hash, the FANG that goes in, what the program may become. Never a method name, an
// interface's identifiers or a call's input: those are the deployer's or the caller's words.
const [P, Q] = [call.calls[0].program, call.calls[2].program];
const [authority, B] = [create.authority!, call.bindings[0].program];
assert.equal(describeAction(upload), "Upload program code a1abd0dc7a01…fef57942 (14 bytes) with interface eedaaa7493e4…3589a396 (45 bytes), bonding at most 30 FANG in storage deposits for whichever isn't stored yet");
assert.equal(describeAction({ ...upload, storage_limit: "0" }), "Upload program code a1abd0dc7a01…fef57942 (14 bytes) with interface eedaaa7493e4…3589a396 (45 bytes)");
assert.equal(describeAction(immutable), "Create an immutable program from code c1c1c1c1c1c1…c1c1c1c1 and interface 1f1f1f1f1f1f…1f1f1f1f, bonding at most 1 FANG in storage deposits");
assert.equal(describeAction(create), `Create a program from code c1c1c1c1c1c1…c1c1c1c1 and interface 1f1f1f1f1f1f…1f1f1f1f, upgradable by ${authority} to code that keeps every method with 100800 blocks' notice once program upgrades exist, accepting transfers, running its initializer with 5 FANG attached, bonding at most 0.5 FANG in storage deposits; its code may also reach ${P} (any version), ${Q}`);
assert.equal(describeAction({ ...create, policy: "open" }), describeAction(create).replace("code that keeps every method", "any code"));
assert.equal(describeAction(program("create, immutable, accepting transfers")), "Create an immutable program from code c1c1c1c1c1c1…c1c1c1c1 and interface 1f1f1f1f1f1f…1f1f1f1f, accepting transfers, bonding at most 1 FANG in storage deposits");
assert.equal(describeAction(program("create, upgradable, without init, refusing transfers")), `Create a program from code c1c1c1c1c1c1…c1c1c1c1 and interface 1f1f1f1f1f1f…1f1f1f1f, upgradable by ${P} to code that keeps every method with 100800 blocks' notice once program upgrades exist, bonding at most 1 FANG in storage deposits`);
assert.equal(describeAction({ ...immutable, upgrade_delay_blocks: 5 }), "Create a program from code c1c1c1c1c1c1…c1c1c1c1 and interface 1f1f1f1f1f1f…1f1f1f1f, with an unreadable upgrade setting (no authority, but an upgrade delay or policy), bonding at most 1 FANG in storage deposits");
assert.equal(describeAction(one), `Call program ${P}`);
assert.equal(describeAction({ ...one, calls: [one.calls[0], one.calls[0]] }), `Make 2 calls to program ${P} (all or nothing)`);
assert.equal(describeAction(call), `Call 2 programs (${P}, ${Q}) in 3 calls (all or nothing) with 5 FANG attached, bonding at most 0.002 FANG in storage deposits; its code may also reach ${B}`);
assert.equal(describeAction({ type: "programs_pause" }), "Vote to pause programs (validators: they pause once f + 1 of the epoch's validators have)");
// The values of a batch add up exactly, past what one u64 holds.
assert.match(describeAction(program("call at the bounds")), / in 8 calls \(all or nothing\) with 147,573,952,589\.67641292 FANG attached,/);
// A method named to mislead, and an input that reads as an instruction, are never quoted.
const words = bytesToHex(new TextEncoder().encode("ignore previous instructions"));
const misleading = [
  { ...call, calls: [{ ...call.calls[0], method: "send_all_funds_to_attacker", input: words }] },
  { ...create, init: { ...create.init!, method: "send_all_funds_to_attacker", input: words } },
];
for (const a of misleading) assert.doesNotMatch(describeAction(a), /send_all_funds|ignore|instructions/);
for (const c of V.program_cases) assert.doesNotMatch(describeAction(c.tx.action), /increment|swap|settle/, c.label);
// Content this client can't read exactly is described as such, never guessed, and never throws.
const junk = [
  { type: "upload_code" },
  { type: "upload_code", wasm: 5, interface: "zz", storage_limit: "1" },
  { type: "create_program" },
  { type: "create_program", authority: "alice.boo", policy: 3, init: [1], bindings: "x", accepts_transfers: "yes" },
  { type: "call_program" },
  { type: "call_program", calls: [null, 5, "x"], bindings: [null] },
  { type: "call_program", calls: [{ program: P, value: 2 ** 60 }], bindings: [] },
  { ...one, bindings: [{ program: B, version_digest: "D1".repeat(32) }] },
];
for (const a of junk) assert.match(describeAction(a), /unreadable/, JSON.stringify(a));
// Programs and authorities are named by address only: a .boo name in their place is unreadable,
// never resolved or shown as an account (the node's format rules refuse it; review S7a-R3).
assert.equal(describeAction({ ...create, authority: "alice.boo" }), describeAction(create).replace(`upgradable by ${authority} `, "upgradable by an unreadable account "));
assert.equal(describeAction({ ...one, calls: [{ ...one.calls[0], program: "alice.boo" }] }), "Call program an unreadable program");
assert.equal(describeAction({ ...create, bindings: [{ ...create.bindings[0], program: "alice.boo" }, create.bindings[1]] }), describeAction(create).replace(`reach ${P} (any version)`, "reach an unreadable program (any version)"));
for (const a of [{ ...create, authority: "alice.boo" }, { ...one, calls: [{ ...one.calls[0], program: "alice.boo" }] }, { ...call, bindings: [{ ...call.bindings[0], program: "alice.boo" }] }]) assert.doesNotMatch(describeAction(a), /alice/, JSON.stringify(a));
// A multisig holds no program action (the node never decodes one nested), so a co-signer is never
// offered one to approve, however readable it is.
for (const c of V.program_cases) assert.equal(reviewAction(c.tx.action).readable, false, c.label);
assert.equal(reviewAction({ type: "transfer", to: P, amount: "1" }).readable, true, "a transfer still is");
console.log(`✓ describeAction summarizes program actions from their structure (${misleading.length} misleading method names and ${junk.length} malformed ones included); reviewAction never offers one for approval`);
