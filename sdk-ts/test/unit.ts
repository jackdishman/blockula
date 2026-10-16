// Offline tests of the protocol v1 helpers: every Wallet helper for tags 30–49 against a fake
// node (the tx it signs, its checks), chain pinning, the tolerant readers for the module
// endpoints, proven reads with witnesses, and release signatures (the known-answer vector of
// scripts/release-sig.mjs). No node needed: `npm run test:vectors -w @blockula/sdk`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, hexToBytes, utf8ToBytes } from "@noble/hashes/utils.js";
import {
  ChainMismatchError, Client, RELEASE_SIGNATURE_FORMAT, Wallet, addressFromPublicKey, bareName, buildDistribution, convictionWeight, distributionLeaf, distributionNode,
  keyId, minFee, paramsOf, proposal, proposalSummary, releaseMessage, sharesFor, signingBytes, stakeSummary, verifyDistributionProof, verifyReleaseSignature, type Action,
  type Tx,
} from "../src/index.js";

// ---- a fake node behind fetch ----

const RPC = "http://fake.node";
const pk = (seed: number) => ml_dsa65.keygen(new Uint8Array(32).fill(seed)).publicKey;
const VALIDATOR = addressFromPublicKey(pk(1));
const OTHER_VALIDATOR = addressFromPublicKey(pk(2));
const BOB = addressFromPublicKey(pk(3));
const GENESIS = "6e".repeat(32);

interface Host { status: Record<string, unknown>; routes: Record<string, unknown>; posted: Tx[]; blocks: Record<number, string> }
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
  const path = decodeURIComponent(url.pathname.replace(/^\/api/, "")) + url.search;
  if (init?.method === "POST" && path === "/tx") {
    const tx = JSON.parse(String(init.body)) as Tx;
    h.posted.push(tx);
    return reply(200, { id: "ab".repeat(32) });
  }
  if (path === "/status") return reply(200, h.status);
  if (path === `/tx/${"ab".repeat(32)}`) return reply(200, { status: "finalized", summary: "ok", fee: h.posted.at(-1)?.fee });
  const block = path.match(/^\/block\/(\d+)$/);
  if (block) return h.blocks[Number(block[1])] ? reply(200, { height: Number(block[1]), state_root: h.blocks[Number(block[1])] }) : reply(404, { error: "no such block" });
  const name = path.match(/^\/name\/(.+)$/);
  if (name) {
    const n = bareName(name[1]);
    const owner = ({ vlad: VALIDATOR, bob: BOB } as Record<string, string>)[n];
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
  ["redelegate all", () => me.redelegate(VALIDATOR, OTHER_VALIDATOR, "all"), { type: "redelegate", from_validator: VALIDATOR, to_validator: OTHER_VALIDATOR, shares: "4000" }],
  ["withdraw unbonded", () => me.withdrawUnbonded(), { type: "withdraw_unbonded" }],
  ["update validator", () => me.updateValidator({ commissionBps: 500 }), { type: "update_validator", commission_bps: 500, p2p_addr: "", http_addr: "" }],
  ["unjail", () => me.unjail(VALIDATOR), { type: "unjail", validator: VALIDATOR }],
  ["report equivocation", () => me.reportEquivocation("0a0b"), { type: "report_equivocation", evidence: "0a0b" }],
  ["beacon commit", () => me.beaconCommit(3, "AB".repeat(32)), { type: "beacon_commit", epoch: 3, commitment: "ab".repeat(32) }],
  ["beacon reveal", () => me.beaconReveal(3, "cd".repeat(32)), { type: "beacon_reveal", epoch: 3, secret: "cd".repeat(32) }],
  [
    "propose a treasury spend (deposit from /api/params, name resolved)",
    () => me.propose(proposal.treasurySpend("bob.boo", "9"), { title: " Pay Bob ", url: "https://forum.example/p/1" }),
    { type: "propose", kind: { type: "treasury_spend", to: BOB, amount: (9n * FANG).toString() }, deposit: "1000000000000", title: "Pay Bob", url: "https://forum.example/p/1" },
  ],
  ["propose a param change", () => me.propose(proposal.paramChange("max_validators", 40), { title: "More validators", deposit: "2000" }), { type: "propose", kind: { type: "param_change", key: "max_validators", value: "40" }, deposit: (2000n * FANG).toString(), title: "More validators", url: "" }],
  ["vote", () => me.vote(1, true, "10", 6), { type: "vote", proposal_id: 1, aye: true, amount: (10n * FANG).toString(), conviction: 6 }],
  ["vote without conviction", () => me.vote(2, false, 1n, 0), { type: "vote", proposal_id: 2, aye: false, amount: "1", conviction: 0 }],
  ["unlock", () => me.unlock(), { type: "unlock" }],
  ["multisig create", () => me.multisigCreate(["AB".repeat(32), "cd".repeat(32)], 2), { type: "multisig_create", owners: ["ab".repeat(32), "cd".repeat(32)], threshold: 2 }],
  ["multisig propose", () => me.multisigPropose({ type: "transfer", to: BOB, amount: "5" }), { type: "multisig_propose", action: { type: "transfer", to: BOB, amount: "5" } }],
  ["multisig approve", () => me.multisigApprove(3), { type: "multisig_approve", id: 3 }],
  ["renew a name", () => me.renewName("Vlad.boo", 2), { type: "renew_name", name: "vlad", years: 2 }],
  ["transfer a name", () => me.transferName("vlad", "bob.boo"), { type: "transfer_name", name: "vlad", to: BOB }],
  ["accept a name", () => me.acceptName("Vlad.boo"), { type: "transfer_name", name: "vlad", to: me.account }],
  ["withdraw a name offer", () => me.withdrawNameOffer("vlad"), { type: "transfer_name", name: "vlad", to: me.account }],
  ["release a name", () => me.releaseName("vlad.boo"), { type: "release_name", name: "vlad" }],
  ["claim a distribution", () => me.claimDistribution(1, "50", ["ab".repeat(32)]), { type: "claim_distribution", distribution_id: 1, amount: (50n * FANG).toString(), proof: ["ab".repeat(32)] }],
  ["attest a revert", () => me.attestRevert(4, "ef".repeat(20)), { type: "attest_revert", claim_id: 4, revert_sha: "ef".repeat(20) }],
];
for (const [label, run, want] of cases) assert.deepEqual(await action(run), want, label);
console.log(`✓ ${cases.length} protocol v1 helpers sign the expected txs`);

// Checked before anything is signed.
const refused: [string, () => Promise<unknown>, RegExp][] = [
  ["conviction 7", () => me.vote(1, true, "1", 7), /conviction must be 0–6/],
  ["zero vote", () => me.vote(1, true, "0"), /more than 0/],
  ["renew 0 years", () => me.renewName("vlad", 0), /1–10 years/],
  ["renew 11 years", () => me.renewName("vlad", 11), /1–10 years/],
  ["bad name", () => me.releaseName("-x-"), /names/],
  ["duplicate owners", () => me.multisigCreate(["ab".repeat(32), "AB".repeat(32)], 1), /distinct/],
  ["threshold above owners", () => me.multisigCreate(["ab".repeat(32)], 2), /threshold must be 1–1/],
  ["nested multisig", () => me.multisigPropose({ type: "multisig_approve", id: 1 }), /can't hold another/],
  ["bad commitment", () => me.beaconCommit(1, "ab"), /32 bytes of hex/],
  ["bad evidence", () => me.reportEquivocation("0A"), /lowercase hex/],
  ["bad commission", () => me.updateValidator({ commissionBps: 10_001 }), /0 to 10000/],
  ["empty title", () => me.propose(proposal.text(), { title: "  ", deposit: "1" }), /title/],
  ["http url", () => me.propose(proposal.text(), { title: "t", url: "http://x.example", deposit: "1" }), /https/],
  ["redelegate to itself", () => me.redelegate(VALIDATOR, "vlad.boo", 1n), /different validator/],
  ["no stake to undelegate", () => me.undelegate(OTHER_VALIDATOR, "all"), /no stake/],
  ["zero shares", () => me.undelegate(VALIDATOR, "0"), /positive whole number/],
  ["unregistered validator name", () => me.delegate("nobody.boo", "1"), /isn't registered/],
  ["bad revert sha", () => me.attestRevert(1, "ab"), /40-hex/],
];
const posted = node.posted.length;
for (const [label, run, re] of refused) await assert.rejects(run(), re, label);
assert.equal(node.posted.length, posted, "nothing submitted");
assert.throws(() => proposal.setRepo("not a repo", 1), /owner\/name/);
assert.throws(() => proposal.paramChange("Bad Key", 1), /parameter keys/);
assert.throws(() => proposal.grantSoul(BOB, "1.2345", "r"), /Soul/);
assert.throws(() => proposal.scheduleUpgrade(0, 10), /version/);
assert.deepEqual(proposal.grantSoul("bob.boo", "2.5", "r"), { type: "grant_soul", to: "bob.boo", amount: 2500, reason: "r" });
console.log(`✓ ${refused.length} bad calls refused before signing`);

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

// ---- proven reads: the proof checks out, and witnesses must agree on the root ----

const V = JSON.parse(readFileSync(new URL("./proof-vectors.json", import.meta.url), "utf8"));
const alice = V.state.proofs.find((c: { label: string }) => c.label === "account boo1alice");
const aliceAddr = "boo1alice";
node.routes[`/proof/1/${bytesToHex(utf8ToBytes(aliceAddr))}`] = { namespace: 1, key: alice.key, value: alice.value, proof: alice.proof, root: V.state.root, height: 9 };
// provenAccount wants a real address; the vector's account is keyed "boo1alice", so read the record directly.
const rec = await client.provenRecord(1, aliceAddr);
assert.equal(rec.root, V.state.root);
const agrees = host("http://witness-a.node");
agrees.blocks[9] = V.state.root;
const disagrees = host("http://witness-b.node");
disagrees.blocks[9] = "22".repeat(32);
const lagging = host("http://witness-c.node");
await client.provenRecord(1, aliceAddr, { witnesses: [new Client("http://witness-a.node")] });
await assert.rejects(client.provenRecord(1, aliceAddr, { witnesses: [new Client("http://witness-a.node"), new Client("http://witness-b.node")] }), /disagrees about the state at block #9/);
setTimeout(() => (lagging.blocks[9] = V.state.root), 300);
await client.provenRecord(1, aliceAddr, { witnesses: [new Client("http://witness-c.node")], waitMs: 3000 });
await assert.rejects(client.provenRecord(1, aliceAddr, { witnesses: [new Client("http://witness-d.node")] }), /fetch failed/);
node.routes[`/proof/1/${bytesToHex(utf8ToBytes(aliceAddr))}`] = { namespace: 1, key: alice.key, value: "00", proof: alice.proof, root: V.state.root, height: 9 };
await assert.rejects(client.provenRecord(1, aliceAddr), /doesn't verify/);
await assert.rejects(client.provenAccount("boo1alice"), /isn't an address/);
console.log("✓ proven reads check proofs and witnesses");

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
