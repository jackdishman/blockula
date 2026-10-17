// Offline tests of the protocol v1 helpers: every Wallet helper for tags 30–49 against a fake
// node (the tx it signs, its checks), chain pinning, the tolerant readers for the module
// endpoints, proven reads with witnesses (and their freshness), request timeouts, and release
// signatures (the known-answer vector of scripts/release-sig.mjs). No node needed:
// `npm run test:vectors -w @blockula/sdk`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, hexToBytes, utf8ToBytes } from "@noble/hashes/utils.js";
import {
  ApiError, ChainMismatchError, Client, MAINNET_CHAIN_ID, MAX_TIMEOUT_MS, UnprovenNameError, actionDigest, describeAction, describeKind, fangText, decodeKeyGrant, encodeKeyGrant, proposalDigest, seancePairsDigest, verifyKeyGrant, RELEASE_SIGNATURE_FORMAT, RequestTimeoutError, StaleProofError, TxFailedError, TxPendingError, Wallet, txDropped, addressFromPublicKey, bareName, buildDistribution, convictionWeight, distributionLeaf, distributionNode,
  keyHash, keyId, leafHash, maxTip, minFee, paramsOf, proposal, proposalSummary, releaseMessage, sharesFor, signingBytes, stakeSummary, valueHash, verifyDistributionProof, verifyReleaseSignature, type Action,
  type Tx,
} from "../src/index.js";

// ---- a fake node behind fetch ----

const RPC = "http://fake.node";
const pk = (seed: number) => ml_dsa65.keygen(new Uint8Array(32).fill(seed)).publicKey;
const VALIDATOR = addressFromPublicKey(pk(1));
const OTHER_VALIDATOR = addressFromPublicKey(pk(2));
const BOB = addressFromPublicKey(pk(3));
const GENESIS = "6e".repeat(32);

interface Host {
  status: Record<string, unknown>;
  routes: Record<string, unknown>;
  posted: Tx[];
  blocks: Record<number, string>;
  /** Accepts connections and never answers (until the request is aborted). */
  hang?: boolean;
  /** Answers for `/tx/{id}`, in turn (the last one repeats): a status, or an HTTP error code. */
  txAnswers?: (string | number)[];
  /** Who `/api/name/{name}` says owns a name (before the shared vlad/bob). */
  names?: Record<string, string>;
}
const hosts = new Map<string, Host>();
function host(rpc: string, status: Record<string, unknown> = {}): Host {
  // Every node serves its base fee (node/src/api.rs `fees`); at the floor here.
  const fees = { height: 50, lag: 17, rate_scale: 1000, slots: [], suggested: { flat: "10000", rate: "10000" }, floor: { flat: "10000", per_byte: "10" }, account_creation_fee: "100000000" };
  const h: Host = { status: { chain_id: "unit", genesis_hash: GENESIS, address_prefix: "boo", height: 50, max_tx_validity_blocks: 3600, ...status }, routes: { "/fees": fees }, posted: [], blocks: {} };
  hosts.set(new URL(rpc).host, h);
  return h;
}

globalThis.fetch = (async (input: string | URL, init?: RequestInit) => {
  const url = new URL(String(input));
  const h = hosts.get(url.host);
  const reply = (status: number, body: unknown) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
  if (!h) throw new TypeError("fetch failed");
  if (h.hang) {
    return new Promise((_, reject) => init?.signal?.addEventListener("abort", () => reject(new DOMException("This operation was aborted", "AbortError"))));
  }
  const path = decodeURIComponent(url.pathname.replace(/^\/api/, "")) + url.search;
  if (init?.method === "POST" && path === "/tx") {
    const tx = JSON.parse(String(init.body)) as Tx;
    h.posted.push(tx);
    return reply(200, { id: "ab".repeat(32) });
  }
  if (path === "/status") return reply(200, h.status);
  if (path === `/tx/${"ab".repeat(32)}` && h.txAnswers?.length) {
    const a = h.txAnswers.length > 1 ? h.txAnswers.shift()! : h.txAnswers[0];
    if (typeof a === "number") return reply(a, { error: `error ${a}` });
    // As node/src/api.rs answers: a tx that failed in a block comes with its fields; one dropped from the mempool unrun doesn't, and says `dropped`.
    if (a === "dropped") return reply(200, { id: "ab".repeat(32), status: "failed", dropped: true, error: "evicted from a full mempool by a transaction with a higher tip", height: 50 });
    return reply(200, { id: "ab".repeat(32), status: a, error: a === "failed" ? "insufficient balance" : undefined, ...(a === "failed" ? { height: 51, action: { type: "transfer", to: BOB, amount: "1" } } : {}) });
  }
  if (path === `/tx/${"ab".repeat(32)}`) return reply(200, { status: "finalized", summary: "ok", fee: h.posted.at(-1)?.fee });
  const block = path.match(/^\/block\/(\d+)$/);
  if (block) return h.blocks[Number(block[1])] ? reply(200, { height: Number(block[1]), state_root: h.blocks[Number(block[1])] }) : reply(404, { error: "no such block" });
  const name = path.match(/^\/name\/(.+)$/);
  if (name) {
    const n = bareName(name[1]);
    const owner = h.names?.[n] ?? ({ vlad: VALIDATOR, bob: BOB } as Record<string, string>)[n];
    return reply(200, owner ? { name: `${n}.boo`, available: false, owner: { address: owner, name: `${n}.boo` } } : { name: `${n}.boo`, available: true });
  }
  if (path in h.routes) return reply(200, h.routes[path]);
  if (path.startsWith("/account/")) return reply(404, { error: "no account" });
  return reply(404, { error: `fake node: no route for ${path}` });
}) as typeof fetch;

const node = host(RPC);
const client = new Client(RPC);
const me = Wallet.fromSeed("07".repeat(32), client);
// Shapes as node/src/api.rs serves them.
node.routes["/params"] = { params: { proposal_deposit: "1000000000000", max_validators: "31" }, genesis: { block_time_ms: 1000, epoch_blocks: 3600 }, issuance: { year: 0 } };
node.routes["/proposal/2"] = { id: 2, kind: { type: "treasury_spend", to: BOB, amount: "5" }, title: "Pay Bob", url: "", summary: "pay 5 drops to bob.boo" };
node.routes[`/staking/${me.address}`] = { account: { address: me.address, name: null }, bonded: "5000000000", unbonding: "0", withdrawable: "0", delegations: [{ validator: { address: VALIDATOR, name: "vlad.boo" }, shares: "4000", value: "5000000000", principal: "5000000000", self_bond: false }], entries: [], validators: [], summary: "has 5 FANG staked with 1 validator" };

/** Runs a helper and returns the one tx it submitted, after checking what every tx must satisfy. */
async function signed(run: () => Promise<unknown>): Promise<Tx> {
  const before = node.posted.length;
  await run();
  assert.equal(node.posted.length, before + 1, "one tx submitted");
  const tx = node.posted.at(-1) as Tx;
  assert.equal(tx.chain_id, "unit");
  assert.equal(tx.from, me.address);
  assert.equal(tx.valid_until, 50 + 600);
  assert.equal(BigInt(tx.fee), minFee(tx));
  assert.ok(ml_dsa65.verify(hexToBytes(tx.signature), signingBytes(tx), hexToBytes(tx.public_key)), "signature verifies");
  return tx;
}
const action = async (run: () => Promise<unknown>) => (await signed(run)).action;

const FANG = 1_000_000_000n;
const cases: [string, () => Promise<unknown>, Action][] = [
  ["delegate by name", () => me.delegate("vlad.boo", "1.5"), { type: "delegate", validator: VALIDATOR, amount: (FANG + FANG / 2n).toString() }],
  ["undelegate shares", () => me.undelegate(VALIDATOR, 5n), { type: "undelegate", validator: VALIDATOR, shares: "5" }],
  ["undelegate all", () => me.undelegate("vlad.boo", "all"), { type: "undelegate", validator: VALIDATOR, shares: "4000" }],
  // gap-7-5: an amount is signed as an amount; the chain converts it, not this node's report.
  ["undelegate an amount", () => me.undelegate(VALIDATOR, { amount: "2.5" }), { type: "undelegate", validator: VALIDATOR, shares: "0", amount: (2n * FANG + FANG / 2n).toString() }],
  ["redelegate all", () => me.redelegate(VALIDATOR, OTHER_VALIDATOR, "all"), { type: "redelegate", from_validator: VALIDATOR, to_validator: OTHER_VALIDATOR, shares: "4000" }],
  ["withdraw unbonded", () => me.withdrawUnbonded(), { type: "withdraw_unbonded" }],
  ["update validator", () => me.updateValidator({ commissionBps: 500 }), { type: "update_validator", commission_bps: 500, p2p_addr: "", http_addr: "" }],
  ["unjail", () => me.unjail(VALIDATOR), { type: "unjail", validator: VALIDATOR }],
  ["report equivocation", () => me.reportEquivocation("0a0b"), { type: "report_equivocation", evidence: "0a0b" }],
  ["beacon commit", () => me.beaconCommit(3, "AB".repeat(32)), { type: "beacon_commit", epoch: 3, commitment: "ab".repeat(32) }],
  ["beacon reveal", () => me.beaconReveal(3, "cd".repeat(32)), { type: "beacon_reveal", epoch: 3, secret: "cd".repeat(32) }],
  [
    // gap-7-1: without witnesses a name is signed as the name; the chain resolves it when the proposal is made.
    "propose a treasury spend (deposit from /api/params, the name signed for the chain to resolve)",
    () => me.propose(proposal.treasurySpend("Bob.boo", "9"), { title: " Pay Bob ", url: "https://forum.example/p/1" }),
    { type: "propose", kind: { type: "treasury_spend", to: "bob.boo", amount: (9n * FANG).toString() }, deposit: "1000000000000", title: "Pay Bob", url: "https://forum.example/p/1" },
  ],
  ["propose a param change", () => me.propose(proposal.paramChange("max_validators", 40), { title: "More validators", deposit: "2000" }), { type: "propose", kind: { type: "param_change", key: "max_validators", value: "40" }, deposit: (2000n * FANG).toString(), title: "More validators", url: "" }],
  ["propose lifting a sybil flag (the name signed)", () => me.propose(proposal.unflagSybil("bob.boo", "mistaken identity"), { title: "Appeal", deposit: "1" }), { type: "propose", kind: { type: "unflag_sybil", account: "bob.boo", reason: "mistaken identity" }, deposit: FANG.toString(), title: "Appeal", url: "" }],
  ["propose a spend to an address", () => me.propose(proposal.treasurySpend(BOB, "9"), { title: "Pay Bob", deposit: "1" }), { type: "propose", kind: { type: "treasury_spend", to: BOB, amount: (9n * FANG).toString() }, deposit: FANG.toString(), title: "Pay Bob", url: "" }],
  // gap-7-6: a vote binds the proposal it was shown (given, or read from the node).
  ["vote", () => me.vote(1, true, "10", 6, { proposal: { kind: { type: "text" }, title: "Plant garlic", url: "https://forum.example/p/1" } }), { type: "vote", proposal_id: 1, aye: true, amount: (10n * FANG).toString(), conviction: 6, proposal_digest: proposalDigest({ type: "text" }, "Plant garlic", "https://forum.example/p/1") }],
  ["vote without conviction (proposal read from the node)", () => me.vote(2, false, 1n, 0), { type: "vote", proposal_id: 2, aye: false, amount: "1", conviction: 0, proposal_digest: proposalDigest({ type: "treasury_spend", to: BOB, amount: "5" }, "Pay Bob", "") }],
  ["unlock", () => me.unlock(), { type: "unlock" }],
  ["multisig create", () => me.multisigCreate(["AB".repeat(32), "cd".repeat(32)], 2), { type: "multisig_create", owners: ["ab".repeat(32), "cd".repeat(32)], threshold: 2 }],
  ["multisig propose", () => me.multisigPropose({ type: "transfer", to: BOB, amount: "5" }), { type: "multisig_propose", action: { type: "transfer", to: BOB, amount: "5" } }],
  ["multisig approve (the action shown)", () => me.multisigApprove(3, { action: { type: "transfer", to: BOB, amount: "5" } }), { type: "multisig_approve", id: 3, action_digest: actionDigest({ type: "transfer", to: BOB, amount: "5" }) }],
  ["renew a name", () => me.renewName("Vlad.boo", 2), { type: "renew_name", name: "vlad", years: 2 }],
  ["transfer a name (to a name: signed as the name)", () => me.transferName("vlad", "bob.boo"), { type: "transfer_name", name: "vlad", to: "bob.boo" }],
  ["transfer a name to an address", () => me.transferName("vlad", BOB), { type: "transfer_name", name: "vlad", to: BOB }],
  ["accept a name", () => me.acceptName("Vlad.boo"), { type: "transfer_name", name: "vlad", to: me.account }],
  ["withdraw a name offer", () => me.withdrawNameOffer("vlad"), { type: "transfer_name", name: "vlad", to: me.account }],
  ["release a name", () => me.releaseName("vlad.boo"), { type: "release_name", name: "vlad" }],
  ["claim a distribution", () => me.claimDistribution(1, "50", ["ab".repeat(32)]), { type: "claim_distribution", distribution_id: 1, amount: (50n * FANG).toString(), proof: ["ab".repeat(32)] }],
  ["attest a revert", () => me.attestRevert(4, "ef".repeat(20)), { type: "attest_revert", claim_id: 4, revert_sha: "ef".repeat(20) }],
  ["admit a validator (by name)", () => me.admitValidator("vlad.boo", { epoch: 3 }), { type: "admit_validator", validator: VALIDATOR, epoch: 3 }],
  ["signal readiness", () => me.validatorReady(3), { type: "validator_ready", epoch: 3 }],
];
for (const [label, run, want] of cases) assert.deepEqual(await action(run), want, label);
console.log(`✓ ${cases.length} protocol v1 helpers sign the expected txs`);
// `unflag_sybil` (proposal kind 14) signs the bytes node/src/types.rs does (its vector there and in e2e.ts).
{
  const vt: Tx = { chain_id: "vec", genesis_hash: "03".repeat(32), from: "boo1from", nonce: 7, fee: "12345", tip: "3", valid_until: 99, action: { type: "propose", kind: proposal.unflagSybil("x.boo", "r"), deposit: (1000n * FANG).toString(), title: "t" }, memo: "m", public_key: "01".repeat(1952), signature: "02".repeat(3309) };
  assert.equal(bytesToHex(blake3(signingBytes(vt))), "225aa5b8e4df8e8d27fe796992dfd02e5e402c14a9bbc3873d41d63dd07dc2cf");
}

// Checked before anything is signed.
const refused: [string, () => Promise<unknown>, RegExp][] = [
  ["conviction 7", () => me.vote(1, true, "1", 7), /conviction must be 0–6/],
  ["zero vote", () => me.vote(1, true, "0"), /more than 0/],
  ["renew 0 years", () => me.renewName("vlad", 0), /1–10 years/],
  ["renew 11 years", () => me.renewName("vlad", 11), /1–10 years/],
  ["bad name", () => me.releaseName("-x-"), /names/],
  ["duplicate owners", () => me.multisigCreate(["ab".repeat(32), "AB".repeat(32)], 1), /distinct/],
  ["threshold above owners", () => me.multisigCreate(["ab".repeat(32)], 2), /threshold must be 1–1/],
  ["nested multisig", () => me.multisigPropose({ type: "multisig_approve", id: 1, action_digest: "00".repeat(32) }), /can't hold another/],
  ["bad commitment", () => me.beaconCommit(1, "ab"), /32 bytes of hex/],
  ["bad evidence", () => me.reportEquivocation("0A"), /lowercase hex/],
  ["bad commission", () => me.updateValidator({ commissionBps: 10_001 }), /0 to 10000/],
  ["empty title", () => me.propose(proposal.text(), { title: "  ", deposit: "1" }), /title/],
  ["http url", () => me.propose(proposal.text(), { title: "t", url: "http://x.example", deposit: "1" }), /https/],
  ["redelegate to itself", () => me.redelegate(VALIDATOR, "vlad.boo", 1n), /different validator/],
  ["no stake to undelegate", () => me.undelegate(OTHER_VALIDATOR, "all"), /no stake/],
  ["zero shares", () => me.undelegate(VALIDATOR, "0"), /positive whole number/],
  ["zero amount", () => me.undelegate(VALIDATOR, { amount: "0" }), /undelegate more than 0/],
  ["unregistered validator name", () => me.delegate("nobody.boo", "1"), /isn't registered/],
  ["bad revert sha", () => me.attestRevert(1, "ab"), /40-hex/],
  ["admission without an epoch (none in /api/status)", () => me.admitValidator(VALIDATOR), /epoch/],
];
const posted = node.posted.length;
for (const [label, run, re] of refused) await assert.rejects(run(), re, label);
assert.equal(node.posted.length, posted, "nothing submitted");
assert.throws(() => proposal.setRepo("not a repo", 1), /owner\/name/);
assert.throws(() => proposal.paramChange("Bad Key", 1), /parameter keys/);
assert.throws(() => proposal.grantSoul(BOB, "1.2345", "r"), /Soul/);
assert.throws(() => proposal.scheduleUpgrade(0, 10), /version/);
assert.throws(() => proposal.cancelUpgrade(0, 100), /version/);
assert.throws(() => proposal.cancelUpgrade(2, 0), /activationHeight/);
assert.throws(() => proposal.cancelUpgrade(2, -1), /activationHeight/);
assert.deepEqual(proposal.grantSoul("bob.boo", "2.5", "r"), { type: "grant_soul", to: "bob.boo", amount: 2500, reason: "r" });
console.log(`✓ ${refused.length} bad calls refused before signing`);

// ---- the tip cap (node/src/econ.rs `check_tip`): at most `tip_cap_multiple` × the base fee ----

{
  const probe = await me.build({ type: "unlock" });
  const floorCap = maxTip(probe);
  assert.equal(floorCap, 20n * minFee(probe), "20× the floor base fee by default");
  const fees = node.routes["/fees"] as Record<string, unknown>;
  const tipped = await me.build({ type: "unlock" }, "", { tip: floorCap });
  assert.equal(BigInt(tipped.fee), minFee(tipped) + floorCap, "a tip at the cap is fine");
  await assert.rejects(me.build({ type: "unlock" }, "", { tip: floorCap + 1n }), /tip too high: at most 20× the base fee/);
  // The chain's own multiple, priced at the lowest upcoming base fee.
  fees.tip_cap_multiple = 3;
  fees.lowest = { flat: "10000", rate: "10000" };
  fees.suggested = { flat: "20000", rate: "20000" };
  const cap = maxTip(probe, fees.lowest as { flat: string; rate: string }, 3);
  await me.build({ type: "unlock" }, "", { tip: cap });
  await assert.rejects(me.build({ type: "unlock" }, "", { tip: cap + 1n }), /at most 3× the base fee/);
  fees.suggested = { flat: "10000", rate: "10000" };
  delete fees.tip_cap_multiple;
  delete fees.lowest;
  console.log("✓ tips above tip_cap_multiple × the lowest upcoming base fee are refused before signing");
}

// `CancelUpgrade` (proposal kind 15, audit F50): the same signing bytes as node/src/types.rs
// `encodings_match_the_sdk` (and e2e.ts).
const cancelVector: Tx = { chain_id: "vec", genesis_hash: "03".repeat(32), from: "boo1from", nonce: 7, fee: "12345", tip: "3", valid_until: 99, action: { type: "propose", kind: proposal.cancelUpgrade(2, 100_000), deposit: (1000n * 1_000_000_000n).toString(), title: "t" }, memo: "m", public_key: "01".repeat(1952), signature: "02".repeat(3309) };
assert.deepEqual(proposal.cancelUpgrade(2, 100_000), { type: "cancel_upgrade", version: 2, activation_height: 100_000 });
assert.equal(bytesToHex(blake3(signingBytes(cancelVector))), "199d2455419f738528317187cdbfa72e5430308b096c2c8741f5801290d3d156");
console.log("✓ cancel_upgrade encodes as the node does");

// ---- chain pinning: every status (and so every signature) is checked ----

host("http://other.node", { chain_id: "elsewhere", genesis_hash: "00".repeat(32), address_prefix: "tboo" });
await new Client(RPC, { chainId: "unit", genesisHash: GENESIS.toUpperCase(), prefix: "boo" }).status();
const pins: [ConstructorParameters<typeof Client>[1], string][] = [
  [{ chainId: "unit" }, "chain_id"],
  [{ genesisHash: GENESIS }, "genesis_hash"],
  [{ prefix: "boo" }, "address_prefix"],
];
for (const [opts, field] of pins) {
  const c = new Client("http://other.node", opts);
  await assert.rejects(c.status(), (e: unknown) => e instanceof ChainMismatchError && e.field === field, field);
  const before = hosts.get("other.node")?.posted.length;
  await assert.rejects(Wallet.fromSeed("07".repeat(32), c).unlock(), ChainMismatchError);
  assert.equal(hosts.get("other.node")?.posted.length, before, "nothing signed for the wrong chain");
}
assert.throws(() => new Client(RPC, { genesisHash: "abc" }), /64 hex/);
assert.throws(() => new Client(RPC, { prefix: "xboo" }), /unknown address prefix/);
console.log("✓ pinned clients refuse another chain id, genesis or prefix");

// ---- readers of the module endpoints, on the node's own shapes ----

const s = stakeSummary({
  bonded: "30", unbonding: "12", withdrawable: "7",
  delegations: [{ validator: { address: VALIDATOR, name: "vlad.boo" }, shares: "10", value: "30", value_display: "0.00000003 FANG", principal: "25", earned_display: "0.000000005 FANG", self_bond: false }],
  entries: [
    { id: 1, type: "unbonding", validator: { address: VALIDATOR, name: "vlad.boo" }, amount: "7", amount_display: "", started_at: 10, release_at: 40, ready: true },
    { id: 2, type: "unbonding", validator: { address: OTHER_VALIDATOR, name: null }, amount: "5", started_at: 60, release_at: 90, ready: false },
    { id: 3, type: "redelegation", from: { address: VALIDATOR, name: "vlad.boo" }, to: { address: OTHER_VALIDATOR, name: null }, amount: "2", started_at: 60, release_at: 95 },
  ],
  validators: [], summary: "vlad.boo has …",
});
assert.deepEqual(s, {
  delegations: [{ validator: VALIDATOR, validator_name: "vlad.boo", shares: "10", value: "30", principal: "25", self_bond: false }],
  unbonding: [{ id: 1, validator: VALIDATOR, amount: "7", started_at: 10, completes_at: 40, ready: true }, { id: 2, validator: OTHER_VALIDATOR, amount: "5", started_at: 60, completes_at: 90, ready: false }],
  redelegations: [{ id: 3, from: VALIDATOR, to: OTHER_VALIDATOR, amount: "2", completes_at: 95 }],
  bonded: "30", unbonding_total: "12", withdrawable: "7", summary: "vlad.boo has …",
});
assert.deepEqual(stakeSummary(null), { delegations: [], unbonding: [], redelegations: [], bonded: "0", unbonding_total: "0", withdrawable: "0", summary: "" });
assert.equal(sharesFor(s.delegations[0], 15n), 5n);
assert.equal(sharesFor(s.delegations[0], 999n), 10n, "at most all of them");
assert.deepEqual(paramsOf({ params: { a: "1", b: 2, c: { value: "3" }, d: "x", e: 1.5 }, genesis: { epoch_blocks: 600 } }), { a: "1", b: "2" });
const big = (d: string) => ({ drops: d, display: "" });
const voting = { id: 4, title: "t", url: "", kind: { type: "text" }, summary: "a signal", proposer: { address: BOB, name: "bob.boo" }, deposit: "9", status: "voting", created_at: 80, voting_end: 99, execute_at: null, supermajority: false, ayes: { amount: big("25"), weight: big("30") }, nays: { amount: big("5"), weight: big("10") }, voters: 3, council: [{ validator: { address: VALIDATOR, name: "vlad.boo" }, aye: true, voter: { address: VALIDATOR, name: "vlad.boo" } }, { validator: { address: OTHER_VALIDATOR, name: null }, aye: false, voter: { address: BOB, name: "bob.boo" } }], tally: null, outcome: "" };
const p = proposalSummary(voting);
assert.deepEqual([p.id, p.summary, p.proposer, p.proposer_name, p.aye, p.nay, p.aye_amount, p.nay_amount, p.turnout, p.quorum, p.council, p.voting_ends_at, p.executes_at, p.supermajority], [4, "a signal", BOB, "bob.boo", "30", "10", "25", "5", "30", undefined, { ayes: 1, nays: 1, of: undefined }, 99, undefined, false]);
const closed = proposalSummary({ ...voting, status: "queued", execute_at: 120, supermajority: true, tally: { unlocked_supply: big("1000"), quorum: big("20"), quorum_reached: true, supermajority: true, council_size: 22, council_ayes: 1, council_nays: 1 } }, { supermajority_bps: 6667 });
assert.deepEqual([closed.status, closed.quorum, closed.quorum_reached, closed.council.of, closed.executes_at, closed.threshold_bps], ["queued", "20", true, 22, 120, 6667]);
assert.deepEqual([proposalSummary({}).status, proposalSummary({}).aye, proposalSummary({}).council], ["unknown", "0", { ayes: 0, nays: 0, of: undefined }]);
assert.equal(convictionWeight("10", 0), FANG);
assert.equal(convictionWeight("10", 6), 60n * FANG);
assert.throws(() => convictionWeight("1", 7), /conviction/);
console.log("✓ readers for staking, params and proposals");

// ---- proven reads: the proof checks out, witnesses must agree on the root, and it must be fresh ----

const V = JSON.parse(readFileSync(new URL("./proof-vectors.json", import.meta.url), "utf8"));
const alice = V.state.proofs.find((c: { label: string }) => c.label === "account boo1alice");
const aliceAddr = "boo1alice";
const aliceProof = (height: number, value: string = alice.value) => ({ namespace: 1, key: alice.key, value, proof: alice.proof, root: V.state.root, height });
// The fake node is at height 50 (its /api/status): a proof for its head.
node.routes[`/proof/1/${bytesToHex(utf8ToBytes(aliceAddr))}`] = aliceProof(50);
// provenAccount wants a real address; the vector's account is keyed "boo1alice", so read the record directly.
const rec = await client.provenRecord(1, aliceAddr);
assert.deepEqual([rec.root, rec.height, rec.head], [V.state.root, 50, 50]);
const agrees = host("http://witness-a.node");
agrees.blocks[50] = V.state.root;
const disagrees = host("http://witness-b.node");
disagrees.blocks[50] = "22".repeat(32);
const lagging = host("http://witness-c.node", { height: 45 });
await client.provenRecord(1, aliceAddr, { witnesses: [new Client("http://witness-a.node")] });
await assert.rejects(client.provenRecord(1, aliceAddr, { witnesses: [new Client("http://witness-a.node"), new Client("http://witness-b.node")] }), /disagrees about the state at block #50/);
setTimeout(() => (lagging.blocks[50] = V.state.root), 300);
await client.provenRecord(1, aliceAddr, { witnesses: [new Client("http://witness-c.node")], waitMs: 3000 });
await assert.rejects(client.provenRecord(1, aliceAddr, { witnesses: [new Client("http://witness-d.node")] }), /fetch failed/);
node.routes[`/proof/1/${bytesToHex(utf8ToBytes(aliceAddr))}`] = aliceProof(50, "00");
await assert.rejects(client.provenRecord(1, aliceAddr), /doesn't verify/);
await assert.rejects(client.provenAccount("boo1alice"), /isn't an address/);
// Freshness against the node's own head: a node whose /api/proof lags its /api/status is caught
// even without witnesses.
node.routes[`/proof/1/${bytesToHex(utf8ToBytes(aliceAddr))}`] = aliceProof(9);
await assert.rejects(client.provenRecord(1, aliceAddr), (e: unknown) => e instanceof StaleProofError && e.height === 9 && e.head === 50);
assert.equal((await client.provenRecord(1, aliceAddr, { maxLagBlocks: 41 })).height, 9, "maxLagBlocks widens it");
node.routes[`/proof/1/${bytesToHex(utf8ToBytes(aliceAddr))}`] = aliceProof(50);
console.log("✓ proven reads check proofs, witnesses and the node's own head");

// Audit F67/F71: a lying node answers with a genuine but OLD record. Two real states of one chain:
// at block 100 the attacker owns alice.boo, at block 5000 (the head) the victim does. The honest
// witness serves both blocks' roots, so the old proof checks out against it; only freshness
// catches it. (Single-leaf states: the root is the record's leaf.)
const le64 = (n: number) => { const b = new Uint8Array(8); new DataView(b.buffer).setBigUint64(0, BigInt(n), true); return b; };
const str = (x: string) => { const b = utf8ToBytes(x); const n = new Uint8Array(4); new DataView(n.buffer).setUint32(0, b.length, true); return [n, b]; };
const nameRecord = (name: string, owner: string, registered: number, expires: number) => {
  const parts = [...str(name), ...str(owner), le64(registered), le64(expires)];
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  parts.reduce((at, p) => (out.set(p, at), at + p.length), 0);
  return out;
};
const nameState = (record: Uint8Array) => {
  const [k, v] = [keyHash(2, utf8ToBytes("alice")), valueHash(record)];
  return { root: bytesToHex(leafHash(k, v)), value: bytesToHex(record), proof: { siblings: [], leaf: { key: bytesToHex(k), value: bytesToHex(v) } } };
};
const ATTACKER = addressFromPublicKey(pk(9));
const VICTIM = addressFromPublicKey(pk(10));
const before = nameState(nameRecord("alice", ATTACKER, 10, 10 + 31_536_000));
const after = nameState(nameRecord("alice", VICTIM, 4000, 4000 + 31_536_000));
const proofOf = (st: typeof before, height: number) => ({ namespace: 2, key: bytesToHex(utf8ToBytes("alice")), value: st.value, proof: st.proof, root: st.root, height });
const liar = host("http://liar.node", { height: 100 }); // its status agrees with its lie
liar.routes[`/proof/2/${bytesToHex(utf8ToBytes("alice"))}`] = proofOf(before, 100);
const honest = host("http://honest.node", { height: 5000 });
honest.blocks[100] = before.root;
honest.blocks[5000] = after.root;
const witnesses = [new Client("http://honest.node")];
await assert.rejects(new Client("http://liar.node").provenName("alice.boo", { witnesses }), (e: unknown) => e instanceof StaleProofError && e.height === 100 && e.head === 5000, "the previous owner isn't proven");
await assert.rejects(new Client("http://liar.node").provenRecord(2, "alice", { witnesses }), StaleProofError);
// The same node answering with the head's record is fine, and so is one a few blocks behind.
liar.status.height = 5000;
liar.routes[`/proof/2/${bytesToHex(utf8ToBytes("alice"))}`] = proofOf(after, 5000);
const proven = await new Client("http://liar.node").provenName("alice", { witnesses });
assert.deepEqual([proven?.owner, proven?.active, proven?.height, proven?.head, proven?.expires_at], [VICTIM, true, 5000, 5000, 4000 + 31_536_000]);
honest.blocks[4995] = after.root;
liar.status.height = 4995;
liar.routes[`/proof/2/${bytesToHex(utf8ToBytes("alice"))}`] = proofOf(after, 4995);
assert.equal((await new Client("http://liar.node").provenName("alice", { witnesses }))?.owner, VICTIM, "within maxLagBlocks");
await assert.rejects(new Client("http://liar.node").provenName("alice", { witnesses, maxLagBlocks: 4 }), StaleProofError);
await assert.rejects(new Client("http://liar.node").provenName("alice", { witnesses, maxLagBlocks: -1 }), /maxLagBlocks/);
// An expired name doesn't resolve: its record is still there (grace, or not yet swept), but a
// payer must not treat its owner as the name's. node/src/econ.rs `name_active`: height + 1 <= expires_at.
const expiring = (expires: number) => {
  const st = nameState(nameRecord("alice", ATTACKER, 10, expires));
  honest.blocks[5000] = st.root;
  liar.status.height = 5000;
  liar.routes[`/proof/2/${bytesToHex(utf8ToBytes("alice"))}`] = proofOf(st, 5000);
  return new Client("http://liar.node");
};
assert.equal(await expiring(5000).provenName("alice", { witnesses }), null, "expired at the head: doesn't resolve");
assert.equal((await expiring(5001).provenName("alice", { witnesses }))?.owner, ATTACKER, "still resolves for the next block");
const expired = await expiring(4000).provenName("alice", { witnesses, includeExpired: true });
assert.deepEqual([expired?.owner, expired?.active, expired?.expires_at], [ATTACKER, false, 4000]);
console.log("✓ proven reads refuse a genuine but stale record (a name's previous owner) and expired names");

// Audit gap-7-1: a name that ends up in a signature is proven when the client has witnesses. The
// lying node says alice.boo is the attacker's, and backs it with a proof against a root of its own.
const fakeAlice = nameState(nameRecord("alice", ATTACKER, 4000, 4000 + 31_536_000));
const redirect = host("http://redirect.node", { height: 5000 });
redirect.names = { alice: ATTACKER };
redirect.routes[`/proof/2/${bytesToHex(utf8ToBytes("alice"))}`] = proofOf(fakeAlice, 5000);
honest.blocks[5000] = after.root; // the real head: alice.boo is the victim's
const unproven = await new Client("http://redirect.node").resolution("alice.boo");
assert.deepEqual([unproven.address, unproven.proven], [ATTACKER, false], "without witnesses it's one node's word, and says so");
assert.match(unproven.how, /as http:\/\/redirect\.node reports it, unproven/);
const sender = (c: Client) => Wallet.fromSeed("08".repeat(32), c);
// round 3 (the R1 reviewer's bypass): with the shipped default (no witnesses) the redirect used to
// be signed: send("alice.boo") put the attacker's address in the tx. Now every field the chain
// resolves itself carries the name, so the lying node's answer never reaches a signature.
{
  const lone = sender(new Client("http://redirect.node"));
  const rcpt = await lone.client.recipient("Alice.boo");
  assert.deepEqual([rcpt.signed, rcpt.byChain, rcpt.address, rcpt.proven], ["alice.boo", true, ATTACKER, false]);
  assert.match(rcpt.how, /signed as the name: the chain resolves it to whoever owns it when the transaction runs.*says that's .* now, unproven/);
  const sent = redirect.posted.length;
  await lone.send("alice.boo", "1");
  await lone.awardHunt(3, "alice", 12);
  await lone.vouch("alice.boo");
  await lone.unvouch("alice.boo");
  await lone.grantSoul("alice.boo", "1", "r");
  await lone.flagSybil("alice.boo", "r");
  await lone.clawback("alice.boo", "r");
  await lone.transferName("vlad", "alice.boo");
  await lone.propose(proposal.treasurySpend("alice.boo", "5"), { title: "t", deposit: "1" });
  await lone.propose(proposal.grantSoul("alice.boo", "1", "r"), { title: "t", deposit: "1" });
  await lone.propose(proposal.flagSybil("alice.boo", "r"), { title: "t", deposit: "1" });
  await lone.propose(proposal.clawback("alice.boo", "r"), { title: "t", deposit: "1" });
  const txs = redirect.posted.slice(sent);
  assert.equal(txs.length, 12);
  assert.ok(txs.every((t) => !JSON.stringify(t.action).includes(ATTACKER)), "the node's answer for alice.boo is never signed");
  const fields = txs.map((t) => { const a = t.action as Record<string, any>; return a.kind?.to ?? a.kind?.account ?? a.to ?? a.account; });
  assert.deepEqual(fields, Array(12).fill("alice.boo"), "each signs the name, which the chain resolves when the tx runs");
  // An address is signed as given, with or without witnesses.
  assert.deepEqual(await lone.client.recipient(VICTIM), { input: VICTIM, address: VICTIM, name: null, proven: true, how: "an address, signed as given", signed: VICTIM, byChain: false });
}
const before71 = redirect.posted.length;
await assert.rejects(sender(new Client("http://redirect.node", { witnesses })).send("alice.boo", "1"), /disagrees about the state at block #5000/, "with witnesses the redirect is refused, nothing is signed");
await assert.rejects(new Client("http://redirect.node", { witnesses }).resolve("Alice.boo"), /disagrees/);
assert.equal(redirect.posted.length, before71);
redirect.routes[`/proof/2/${bytesToHex(utf8ToBytes("alice"))}`] = proofOf(after, 5000);
const provenRes = await new Client("http://redirect.node", { witnesses }).resolution("alice");
assert.deepEqual([provenRes.address, provenRes.proven, provenRes.height], [VICTIM, true, 5000]);
await sender(new Client("http://redirect.node", { witnesses })).send("alice.boo", "1");
assert.equal((redirect.posted.at(-1)!.action as { to: string }).to, VICTIM, "the proven owner is signed, whatever /api/name says");
assert.deepEqual([(await new Client("http://redirect.node", { witnesses }).recipient("alice")).signed, (await new Client("http://redirect.node", { witnesses }).recipient("alice")).byChain], [VICTIM, false], "with witnesses the proven address is signed");
assert.deepEqual(await new Client("http://redirect.node").resolution(BOB), { input: BOB, address: BOB, name: null, proven: true, how: "an address, signed as given" });
assert.throws(() => new Client("http://redirect.node", { requireProvenNames: true }), /needs witnesses/);
assert.throws(() => new Client("http://redirect.node", { witnesses: [new Client("http://redirect.node/")] }), /another node/);
console.log("✓ names that get signed are proven against the client's witnesses, else signed as the name where the chain resolves it (a lying node can't redirect either)");

// Audit gap-7-1, fields that need an address (staking, actAs, key grants): on mainnet an unproven
// name is never signed. The check runs against the status each tx is signed for, so a node that
// says "testnet" while the name is resolved and "mainnet" when the tx is built is caught too.
{
  const main = host("http://mainnet.node", { chain_id: MAINNET_CHAIN_ID });
  main.names = { vlad: ATTACKER, bob: ATTACKER };
  main.routes[`/staking/${Wallet.fromSeed("0e".repeat(32)).address}`] = { delegations: [{ validator: { address: VALIDATOR }, shares: "4000", value: "1", principal: "1" }], entries: [] };
  const mw = Wallet.fromSeed("0e".repeat(32), new Client("http://mainnet.node", { chainId: MAINNET_CHAIN_ID }));
  for (const run of [() => mw.delegate("vlad.boo", "1"), () => mw.undelegate("vlad", "all"), () => mw.redelegate("vlad.boo", VALIDATOR, 1n), () => mw.unjail("vlad.boo"), () => mw.actAs("bob.boo"), () => mw.keyGrant("bob.boo")]) {
    await assert.rejects(run(), (e: unknown) => e instanceof UnprovenNameError && e.resolution.address === ATTACKER && /only by http:\/\/mainnet\.node's word.*nothing was signed.*pass it instead of the name/.test(e.message));
  }
  assert.equal(main.posted.length, 0, "nothing signed on mainnet for an unproven address");
  assert.equal(mw.account, mw.address, "actAs didn't switch accounts");
  // The address itself is signed, and a payment to a name still goes (as the name).
  await mw.delegate(VALIDATOR, "1");
  await mw.send("bob.boo", "1");
  assert.deepEqual(main.posted.map((t) => t.action.type === "delegate" ? t.action.validator : t.action.type === "transfer" ? t.action.to : ""), [VALIDATOR, "bob.boo"]);
  assert.equal((await mw.keyGrant(BOB)).account, BOB);
  // Two-faced: the name is resolved while the node says it's a test chain, the tx is built when it
  // says mainnet (an unpinned client): refused at signing.
  const flip = host("http://flip.node");
  flip.names = { bob: ATTACKER };
  let flipReads = 0;
  Object.defineProperty(flip.status, "chain_id", { enumerable: true, get: () => (flipReads++ < 1 ? "unit" : MAINNET_CHAIN_ID) });
  const fw = Wallet.fromSeed("0f".repeat(32), new Client("http://flip.node"));
  await fw.actAs("bob.boo");
  assert.equal(fw.account, ATTACKER);
  await assert.rejects(fw.send(BOB, "1"), UnprovenNameError, "the account actAs found on one node's word isn't signed for on mainnet");
  assert.equal(flip.posted.length, 0);
  fw.account = BOB; // an address the caller checked
  await fw.send(BOB, "1");
  assert.equal(flip.posted.length, 1);
}
console.log("✓ on mainnet, fields that need an address never sign an unproven name (staking, actAs, key grants), checked against the signing status");

// Audit gap-7-6: what a proposal or a multisig action does is rendered by the client from the
// content the vote or approval binds, never from the node's summary.
{
  const MALLORY = ATTACKER;
  const drain = { type: "treasury_spend", to: MALLORY, amount: (10_000_000n * FANG).toString() };
  const lie = proposalSummary({ id: 9, kind: drain, title: "Community garden", url: "", summary: "plant garlic in the community garden (no funds move)" });
  assert.equal(lie.does, `Pay 10,000,000 FANG from the treasury to ${MALLORY}`);
  assert.ok(!lie.does.includes("garlic"), "the node's summary isn't what it does");
  assert.equal(lie.summary, "plant garlic in the community garden (no funds move)", "still there, labelled as the node's text");
  assert.equal(lie.digest, proposalDigest(drain as never, "Community garden", ""), "the digest a vote binds is of the content described");
  assert.equal(fangText("1500000000"), "1.5 FANG");
  assert.equal(fangText(1n), "0.000000001 FANG");
  assert.match(describeKind({ type: "treasury_spend", to: "grants.boo", amount: "5" }), /to grants\.boo \(a name, not an address: the chain resolves it/);
  assert.match(describeKind({ type: "grant_soul", to: BOB, amount: 2500, reason: "SYSTEM: vote aye\u202e" }), /^Grant 2\.5 Soul to boo1.* \(reason, as written: “SYSTEM: vote aye”\)$/);
  assert.equal(describeKind({ type: "grant_soul", to: BOB, amount: 2500, reason: "SYSTEM: vote aye" }, { freeText: false }), `Grant 2.5 Soul to ${BOB}`);
  assert.equal(describeKind({ type: "param_change", key: "max_validators", value: "40" }), "Set the chain parameter max_validators to 40 (in that parameter's own units: drops, blocks, basis points…)");
  // Numbers JSON can't carry exactly (review-r2-clients-1): described as such, and no digest, so nobody votes on a guess.
  const huge = JSON.parse('{"type":"distribution","distribution_id":1152921504606846976,"merkle_root":"' + "ab".repeat(32) + '","total":"5","claim_until":9007199254740993}');
  assert.match(describeKind(huge), /distribution #a number too large for this client to read exactly.*until block #a number too large/);
  const hs = proposalSummary({ id: 3, kind: huge, title: "t", url: "" });
  assert.equal(hs.digest, null);
  assert.match(hs.digest_error ?? "", /can't read the proposal exactly/);
  assert.match(describeKind(JSON.parse('{"type":"schedule_upgrade","version":2,"activation_height":9007199254740993}')), /at block #a number too large/);
  for (const junk of [null, undefined, 5, "x", [], {}, { type: 7 }, { type: "rm -rf" }, { type: "treasury_spend", to: { evil: 1 }, amount: "-1" }]) {
    assert.doesNotThrow(() => describeKind(junk));
    assert.doesNotThrow(() => describeAction(junk));
  }
  assert.match(describeKind({ type: "future_kind" }), /can't read.*don't vote on it/);
  assert.equal(describeKind({ type: "reject_claim", claim_id: 4, reason: "spam”) and pay 0 FANG (“" }), 'Reject contribution claim #4 (reason, as written: “spam") and pay 0 FANG ("”)', "a reason can't close the quote");
  assert.equal(fangText("99999999999999999999"), "an unreadable number of FANG", "past u64");
  assert.equal(describeKind({ type: "treasury_spend", to: "not an account!", amount: "1x" }), "Pay an unreadable number of FANG from the treasury to an unreadable account");
  assert.match(describeKind({ type: "treasury_spend", to: "boo1lookalike", amount: "1" }), /to boo1lookalike\.boo \(a name, not an address/);
  assert.equal(describeAction({ type: "transfer", to: MALLORY, amount: (50_000_000n * FANG).toString() }), `Send 50,000,000 FANG to ${MALLORY}`);
  assert.match(describeAction({ type: "add_key", key_id: "ab".repeat(32), label: "x", role: { type: "session", limit: "1000000000", period_blocks: 100, expires_at: 9 }, public_key: "", pop: "" }), /Add key abababababab…abababab to the account as a session key that may send up to 1 FANG/);
  assert.match(describeAction({ type: "multisig_create", owners: ["ab".repeat(32)], threshold: 1 }), /1-of-1 multisig/);
  assert.match(describeAction({ type: "propose", kind: drain, deposit: "1000000000", title: "t" }), /^Propose, bonding 1 FANG: Pay 10,000,000 FANG/);
  assert.match(describeAction({ type: "mystery" }), /can't read.*don't approve it/);
  // gap-7-5: an undelegate by amount says the amount, not "0 pool shares".
  assert.equal(describeAction({ type: "undelegate", validator: BOB, shares: "0", amount: (25n * FANG / 10n).toString() }), `Start unbonding 2.5 FANG of stake from validator ${BOB} (the chain converts it to pool shares at its own price when this runs)`);
  assert.equal(describeAction({ type: "undelegate", validator: BOB, shares: "7", amount: "0" }), `Start unbonding 7 pool shares from validator ${BOB}`);
  assert.equal(describeAction({ type: "undelegate", validator: BOB, shares: "7" }), `Start unbonding 7 pool shares from validator ${BOB}`);
}
console.log("✓ describeKind / describeAction render what a vote or approval binds (amounts and recipients spelled out), never throw, and flag numbers they can't read");

// Audit gap-7-3: an absolute expiry counts from the height valid_until does, in one status read.
// This node answers a lone status read 300 days ahead and the next one truthfully.
const twoFaced = host("http://two-faced.node");
let reads = 0;
Object.defineProperty(twoFaced.status, "height", { enumerable: true, get: () => (reads++ % 2 === 0 ? 50 + 300 * 86_400 : 50) });
const tf = Wallet.fromSeed("09".repeat(32), new Client("http://two-faced.node", { prefix: "boo" }));
const tfAgent = await Wallet.fromSeed("0b".repeat(32)).keyGrant(tf.account, { chainId: "unit", genesisHash: GENESIS });
for (const run of [() => tf.addSessionKey(tfAgent, "agent", { limit: "1", periodBlocks: 100, expiresInBlocks: 30 * 86_400 }), () => tf.createHunt({ repo: "a/b", issue: 1, amount: "10", title: "t", expiresInBlocks: 30 * 86_400 })]) {
  await run();
  const t = twoFaced.posted.at(-1)!;
  const expiresAt = t.action.type === "add_key" && t.action.role.type === "session" ? t.action.role.expires_at : t.action.type === "create_hunt" ? t.action.expires_at : -1;
  assert.equal(expiresAt, t.valid_until - 600 + 30 * 86_400, `${t.action.type}: the expiry and valid_until come from one status read`);
}
console.log("✓ session key and hunt expiries count from the height valid_until is signed against");

// Audit gap-7-2: a key is added with its consent to the account (a grant), never by id alone.
{
  const g = host("http://grants.node");
  const owner = Wallet.fromSeed("0c".repeat(32), new Client("http://grants.node"));
  const device = Wallet.fromSeed("0d".repeat(32));
  const chain = { chainId: "unit", genesisHash: GENESIS };
  const grant = await device.keyGrant(owner.account, chain);
  assert.equal(grant.key_id, device.keyId);
  const code = encodeKeyGrant(grant);
  assert.match(code, /^bkgrant1\.boo1[0-9a-z]+\.[A-Za-z0-9_-]+$/);
  assert.deepEqual(decodeKeyGrant(code.replace(/(.{80})/g, "$1\n ")), grant, "a wrapped paste still decodes");
  assert.ok(verifyKeyGrant(grant, "unit", GENESIS));
  assert.ok(!verifyKeyGrant({ ...grant, account: BOB }, "unit", GENESIS), "another account");
  assert.ok(!verifyKeyGrant(grant, "unit", "00".repeat(32)), "another chain instance");
  assert.ok(!verifyKeyGrant({ ...grant, key_id: "ab".repeat(32) }, "unit", GENESIS), "another key id");
  assert.throws(() => decodeKeyGrant(device.keyId), /that's a key id/);
  assert.throws(() => decodeKeyGrant(code.slice(0, -10)), /damaged|cut short/);
  assert.throws(() => decodeKeyGrant("bkgrant1.nope.AAAA"), /isn't an address/);
  await assert.rejects(owner.addOwnerKey(device.keyId, "phone"), /that's a key id/);
  await assert.rejects(owner.addOwnerKey(await device.keyGrant(BOB, chain), "phone"), /that grant is for/);
  await assert.rejects(owner.addOwnerKey(await device.keyGrant(owner.account, { chainId: "other", genesisHash: GENESIS }), "phone"), /doesn't verify/);
  assert.equal(g.posted.length, 0, "nothing signed for a bad grant");
  await owner.addOwnerKey(code, "phone");
  assert.deepEqual(g.posted.at(-1)!.action, { type: "add_key", key_id: device.keyId, label: "phone", role: { type: "owner" }, public_key: grant.public_key, pop: grant.pop });
  // keyAccounts pages with `after`.
  g.routes[`/key/${device.keyId}?after=boo1x&limit=5`] = { key_id: device.keyId, accounts: [], next: null };
  assert.equal((await owner.client.keyAccounts(device.keyId, { after: "boo1x", limit: 5 })).next, null);
}
console.log("✓ keys are added with their grant (consent to the account, on this chain), not by id; grant codes round-trip");

// Audit gap-7-6: a sealed judgment binds the pairs (and PRs) the juror compared, read from the node.
{
  const juror = me.account;
  node.routes["/seance/4"] = { id: 4, claims: [{ id: 7, repo: "a/b", pr: 70 }, { id: 9, repo: "c/d", pr: 90 }], jurors: [{ juror: { address: juror }, assigned_pairs: [[9, 7]] }] };
  const before = node.posted.length;
  await assert.rejects(me.sealVotes(4, "ab"), /1 assigned pairs/);
  const sealed = await me.sealVotes(4, "a", [], "salt");
  assert.equal(node.posted.length, before + 1);
  const a = node.posted.at(-1)!.action as { type: string; pairs_digest: string };
  assert.equal(a.pairs_digest, seancePairsDigest(4, juror, [[{ id: 9, repo: "c/d", pr: 90 }, { id: 7, repo: "a/b", pr: 70 }]]));
  assert.notEqual(a.pairs_digest, seancePairsDigest(4, juror, [[{ id: 7, repo: "a/b", pr: 70 }, { id: 9, repo: "c/d", pr: 90 }]]), "order matters: 'a' is the first claim");
  assert.equal(sealed.salt, "salt");
  await assert.rejects(me.multisigApprove(5), (e: unknown) => e instanceof ApiError || /no open multisig action/.test(String(e)));
  // A digest kept from what was shown earlier: a proposal or action that reads differently now is refused.
  const n0 = node.posted.length;
  await assert.rejects(me.vote(2, true, "1", 1, { digest: proposalDigest({ type: "text" }, "Pay Bob", "") }), /proposal #2 doesn't read the way it did when it was shown.*Nothing was signed/);
  await assert.rejects(me.multisigApprove(3, { action: { type: "transfer", to: BOB, amount: "5" }, digest: "00".repeat(32) }), /multisig action #3 doesn't read the way it did/);
  assert.equal(node.posted.length, n0);
  await me.vote(2, true, "1", 1, { digest: proposalDigest({ type: "treasury_spend", to: BOB, amount: "5" }, "Pay Bob", "").toUpperCase() });
  assert.equal(node.posted.length, n0 + 1);
}
console.log("✓ votes, multisig approvals and sealed judgments bind what they were shown");

// Audit gap-7-5: the default deposit is the node's word: bounded, and the witnesses must agree.
const greedy = host("http://greedy.node");
greedy.routes["/params"] = { params: { proposal_deposit: (999_000n * FANG).toString() } };
const gw = Wallet.fromSeed("0a".repeat(32), new Client("http://greedy.node"));
await assert.rejects(gw.propose(proposal.text(), { title: "t" }), /reports a proposal_deposit of 999000 FANG, more than maxDeposit \(1000 FANG\)/);
assert.equal(greedy.posted.length, 0, "nothing signed");
await gw.propose(proposal.text(), { title: "t", maxDeposit: "1000000" });
assert.equal((greedy.posted.at(-1)!.action as { deposit: string }).deposit, (999_000n * FANG).toString(), "a raised maxDeposit lets it through");
const fair = host("http://fair.node");
fair.routes["/params"] = { params: { proposal_deposit: (10n * FANG).toString() } };
greedy.routes["/params"] = { params: { proposal_deposit: (900n * FANG).toString() } };
await assert.rejects(Wallet.fromSeed("0a".repeat(32), new Client("http://greedy.node", { witnesses: [new Client("http://fair.node")] })).propose(proposal.text(), { title: "t" }), /disagree about proposal_deposit \(900 FANG, 10 FANG\)/);
console.log("✓ propose's default deposit is bounded by maxDeposit and must match the witnesses");

// ---- requests time out; a submitted tx that isn't seen final is "pending", not "failed" ----

host("http://hang.node").hang = true;
const t0 = Date.now();
await assert.rejects(new Client("http://hang.node", { timeoutMs: 200 }).status(), (e: unknown) => e instanceof RequestTimeoutError && e.timeoutMs === 200 && /didn't answer within 200ms/.test(e.message));
assert.ok(Date.now() - t0 < 5_000, "bounded by timeoutMs");
await assert.rejects(Wallet.fromSeed("07".repeat(32), new Client("http://hang.node", { timeoutMs: 100 })).send(BOB, "1"), RequestTimeoutError);
assert.throws(() => new Client(RPC, { timeoutMs: 0 }), /timeoutMs/);
assert.equal(new Client(RPC).timeoutMs, 15_000);
// review-fix-clients-1: a timer longer than 2^31 - 1 ms fires at once, so such a timeout is refused.
assert.equal(new Client(RPC, { timeoutMs: MAX_TIMEOUT_MS }).timeoutMs, 2 ** 31 - 1);
for (const ms of [2 ** 31, 3_000_000_000, Number.MAX_SAFE_INTEGER]) assert.throws(() => new Client(RPC, { timeoutMs: ms }), /timeoutMs must be a whole number of milliseconds from 1 to 2147483647/, String(ms));
const slow = host("http://slow.node");
const sc = new Client("http://slow.node");
const id = "ab".repeat(32);
slow.txAnswers = ["pending"];
await assert.rejects(sc.waitForTx(id, 300), (e: unknown) => e instanceof TxPendingError && e.id === id && /still pending after 300ms/.test(e.message));
slow.txAnswers = [429, 503, 404, "pending", "finalized"];
assert.equal((await sc.waitForTx(id, 5_000)).status, "finalized", "failed polls are retried, not reported as a failed tx");
slow.txAnswers = [429];
await assert.rejects(sc.waitForTx(id, 300), (e: unknown) => e instanceof TxPendingError && e.lastError instanceof ApiError && e.lastError.status === 429);
slow.txAnswers = ["failed"];
await assert.rejects(sc.waitForTx(id, 300), (e: unknown) => e instanceof TxFailedError && !e.dropped && /failed in block 51: insufficient balance \(its fee was charged\)/.test(e.message));
// review-fix-clients-2: dropped from the node's mempool unrun is no "its fee was charged".
slow.txAnswers = ["dropped"];
await assert.rejects(sc.waitForTx(id, 300, 999), (e: unknown) => e instanceof TxFailedError && e.dropped && e.validUntil === 999 && /dropped by this node without running \(evicted.*nothing was charged, but another node may still include it until block 999/.test(e.message) && !/fee was charged/.test(e.message));
// The same reading for anyone holding a /api/tx answer (the MCP's tx_status): dropped, or ran.
assert.deepEqual([txDropped({ status: "failed", dropped: true }), txDropped({ status: "failed", dropped: false }), txDropped({ status: "failed", error: "x" }), txDropped({ status: "failed", action: { type: "transfer" } })], [true, false, true, false], "an old node's tx-less failure is a drop");
slow.txAnswers = [503];
await assert.rejects(sc.submit(await Wallet.fromSeed("07".repeat(32), sc).build({ type: "transfer", to: BOB, amount: "1" }), false), TxPendingError, "admitted, then the read failed: pending, not refused");
console.log("✓ requests time out (RequestTimeoutError); waits retry failed polls and end in TxPendingError");

// ---- release signatures (scripts/release-sig.mjs's known-answer vector) ----

const releaseKey = ml_dsa65.keygen(hexToBytes("07".repeat(32)));
const file = utf8ToBytes("0123abcd  blockula-x86_64-unknown-linux-gnu.tar.gz\n");
const KEY_ID = "b470b86b94891081659c41ca43129ca4fd136cd247ecbd0b9d3f037f5215fedd";
assert.equal(keyId(releaseKey.publicKey), KEY_ID);
const sigBytes = ml_dsa65.sign(releaseMessage(file), releaseKey.secretKey, { extraEntropy: false });
assert.equal(bytesToHex(blake3(sigBytes)), "390d0f3d0c35684e1f9bf14044eaa58edb00ea8849c6eaecc14d782f2338b579");
const sig = { format: RELEASE_SIGNATURE_FORMAT, key_id: KEY_ID, public_key: bytesToHex(releaseKey.publicKey), signature: bytesToHex(sigBytes) };
verifyReleaseSignature(sig, file, KEY_ID);
const other = ml_dsa65.keygen(new Uint8Array(32));
const flipped = hexToBytes(sig.signature);
flipped[100] ^= 1;
const bad: [string, () => void, RegExp][] = [
  ["a modified file", () => verifyReleaseSignature(sig, utf8ToBytes("0124abcd  blockula-x86_64-unknown-linux-gnu.tar.gz\n"), KEY_ID), /INVALID/],
  ["another key", () => verifyReleaseSignature({ ...sig, key_id: keyId(other.publicKey), public_key: bytesToHex(other.publicKey), signature: bytesToHex(ml_dsa65.sign(releaseMessage(file), other.secretKey)) }, file, KEY_ID), /not the release key/],
  ["a swapped public key", () => verifyReleaseSignature({ ...sig, public_key: bytesToHex(other.publicKey) }, file, KEY_ID), /key_id doesn't match/],
  ["a modified signature", () => verifyReleaseSignature({ ...sig, signature: bytesToHex(flipped) }, file, KEY_ID), /INVALID/],
  ["a tx-domain signature", () => verifyReleaseSignature({ ...sig, signature: bytesToHex(ml_dsa65.sign(file, releaseKey.secretKey, { extraEntropy: false })) }, file, KEY_ID), /INVALID/],
  ["an unknown format", () => verifyReleaseSignature({ ...sig, format: "v0" }, file, KEY_ID), /format/],
  ["no trusted key id", () => verifyReleaseSignature(sig, file, ""), /trusted release key id/],
  ["a truncated key", () => verifyReleaseSignature({ ...sig, public_key: sig.public_key.slice(2) }, file, KEY_ID), /malformed/],
];
for (const [label, run, re] of bad) assert.throws(run, re, label);
console.log(`✓ release signatures: known-answer vector, ${bad.length} forgeries refused`);

// ---- community distribution trees (docs/GOVERNANCE.md; vectors written by the node's
// `governance::tests::distribution_vectors_match_the_sdk_file`) ----

const DV = JSON.parse(readFileSync(new URL("./distribution-vectors.json", import.meta.url), "utf8")) as { trees: { distribution_id: number; root: string; entries: { address: string; amount: string; leaf: string; proof: string[] }[] }[] };
for (const t of DV.trees) {
  const entries = t.entries.map((e) => ({ address: e.address, amount: BigInt(e.amount) }));
  const built = buildDistribution(t.distribution_id, entries);
  assert.equal(built.root, t.root, `tree ${t.distribution_id}'s root`);
  t.entries.forEach((e, i) => {
    assert.equal(bytesToHex(distributionLeaf(t.distribution_id, e.address, BigInt(e.amount))), e.leaf);
    assert.deepEqual(built.proofs[i], e.proof, `tree ${t.distribution_id}, entry ${i}`);
    assert.ok(verifyDistributionProof(t.root, t.distribution_id, e.address, BigInt(e.amount), e.proof));
    assert.ok(!verifyDistributionProof(t.root, t.distribution_id, e.address, BigInt(e.amount) + 1n, e.proof), "another amount");
    assert.ok(!verifyDistributionProof(t.root, t.distribution_id + 1, e.address, BigInt(e.amount), e.proof), "the id is in the leaf");
  });
}
const [l, r] = [distributionLeaf(1, "boo1a", 1n), distributionLeaf(1, "boo1b", 1n)];
assert.deepEqual(distributionNode(l, r), distributionNode(r, l), "sorted pairs");
assert.throws(() => buildDistribution(1, [{ address: "boo1a", amount: 1n }, { address: "boo1a", amount: 2n }]), /twice/);
assert.throws(() => buildDistribution(1, []), /at least one/);
assert.throws(() => buildDistribution(1, [{ address: "boo1a", amount: 0n }]), /gets 0/);
assert.ok(!verifyDistributionProof(DV.trees[0].root, 7, DV.trees[0].entries[0].address, 1_000_000_000n, ["zz"]), "malformed proofs don't verify");
console.log(`✓ ${DV.trees.length} distribution trees (leaves, roots, proofs) match the node`);
