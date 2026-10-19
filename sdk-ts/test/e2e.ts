// End-to-end against a running node: `blockula dev --reset` then `npm test`.
// Proves TS-signed ML-DSA transactions verify in the Rust node.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, hexToBytes, randomBytes } from "@noble/hashes/utils.js";
import { ApiError, Client, NAMESPACES, POP_CONTEXT, encodeKeyGrant, TxFailedError, Wallet, addressFromPublicKey, addressPrefix, baseFee, beaconCommitment, formatFang, ghostSighash, isAddress, minFee, paramsOf, parseFang, parseSoul, popMessage, proposal, proposalSummary, seanceCommitment, signingBytes, stakeSummary, txBytes, txCharge, txId, txSize, verifyProof, type Action, type GhostBundle, type ProposalKind, type Tx } from "../src/index.js";

const client = new Client(process.env.BLOCKULA_RPC ?? "http://localhost:7070");

assert.equal(parseFang("1.5"), 1_500_000_000n);
assert.equal(formatFang(1_500_000_000n), "1.5 FANG");

// Shared vectors: node/src/types.rs `encodings_match_the_sdk` asserts the same hashes.
// One action per tag 0–29, in tag order (`vector_actions` there).
const vectorActions: Action[] = [
  { type: "transfer", to: "x.boo", amount: "5" },
  { type: "register_name", name: "vlad" },
  { type: "add_key", key_id: "ab".repeat(32), label: "phone", role: { type: "owner" }, public_key: "0c", pop: "0d" },
  { type: "add_key", key_id: "cd".repeat(32), label: "claude", role: { type: "session", limit: "10", period_blocks: 100, expires_at: 1000 }, public_key: "0e", pop: "0f" },
  { type: "remove_key", key_id: "ab".repeat(32) },
  { type: "register_validator", consensus_key: "0a", p2p_addr: "1.2.3.4:5", http_addr: "https://v.example", bond: "50", pop: "0b" },
  { type: "approve_validator", validator: "boo1v" },
  { type: "remove_validator", validator: "boo1v" },
  { type: "unbond", validator: "boo1v" },
  { type: "link_github", handle: "vlad", gist_id: "abc123" },
  { type: "unlink_github", handle: "vlad" },
  { type: "create_hunt", repo: "a/b", issue: 3, amount: "10", expires_at: 500, title: "t" },
  { type: "fund_hunt", hunt_id: 1, amount: "2" },
  { type: "award_hunt", hunt_id: 1, to: "x.boo", pr: 4, familiar_key: "ef".repeat(32) },
  { type: "award_hunt", hunt_id: 1, to: "x.boo", pr: 4 },
  { type: "cancel_hunt", hunt_id: 1 },
  { type: "register_familiar", key_id: "cd".repeat(32), label: "claude" },
  { type: "set_repo", repo: "a/b", weight: 10 },
  { type: "grant_soul", to: "x.boo", amount: 2500, reason: "r" },
  { type: "vouch", account: "x.boo" },
  { type: "unvouch", account: "x.boo" },
  { type: "flag_sybil", account: "x.boo", reason: "r" },
  { type: "submit_claim", repo: "a/b", pr: 9, familiar_key: "ab".repeat(32) },
  { type: "submit_claim", repo: "a/b", pr: 9 },
  { type: "attest_claim", claim_id: 4, ok: true, merge_sha: "cd".repeat(20), note: "" },
  { type: "reject_claim", claim_id: 4, reason: "r" },
  { type: "join_jury", bond: (10n * 1_000_000_000n).toString() },
  { type: "leave_jury" },
  { type: "commit_votes", seance_id: 2, commitment: "ef".repeat(32), pairs_digest: "e0".repeat(32) },
  { type: "reveal_votes", seance_id: 2, votes: "ab=", spam: [5, 6], salt: "s" },
  { type: "fund_pool", amount: "5" },
  { type: "withdraw_vested" },
  { type: "clawback", account: "x.boo", reason: "r" },
];
const vectorTx = (action: Action): Tx => ({ chain_id: "vec", genesis_hash: "03".repeat(32), from: "boo1from", nonce: 7, fee: "12345", tip: "3", valid_until: 99, action, memo: "m", public_key: "01".repeat(1952), signature: "02".repeat(3309) });
const all = vectorActions.map((a) => signingBytes(vectorTx(a)));
const joined = new Uint8Array(all.reduce((n, b) => n + b.length, 0));
all.reduce((at, b) => (joined.set(b, at), at + b.length), 0);
assert.equal(bytesToHex(blake3(joined)), "1078aa2103641542458acf9a1928da918af6505c8cb3a3a1f7bbf7320bbe3c30");
// Protocol v1 (tags 30–49, every proposal kind, a multisig proposal), hashed on its own: `v1_vector_actions` there.
const propose = (kind: ProposalKind): Action => ({ type: "propose", kind, deposit: (1000n * 1_000_000_000n).toString(), title: "t" });
const v1VectorActions: Action[] = [
  { type: "delegate", validator: "boo1v", amount: (5n * 1_000_000_000n).toString() },
  { type: "undelegate", validator: "boo1v", shares: "7" },
  { type: "undelegate", validator: "boo1v", shares: "0", amount: (9n * 1_000_000_000n).toString() },
  { type: "redelegate", from_validator: "boo1v", to_validator: "boo1w", shares: "8" },
  { type: "withdraw_unbonded" },
  { type: "update_validator", commission_bps: 500, p2p_addr: "1.2.3.4:5", http_addr: "https://v.example" },
  { type: "update_validator", commission_bps: 0, p2p_addr: "", http_addr: "" },
  { type: "unjail", validator: "boo1v" },
  { type: "report_equivocation", evidence: "0a0b" },
  { type: "beacon_commit", epoch: 3, commitment: "ab".repeat(32) },
  { type: "beacon_reveal", epoch: 3, secret: "cd".repeat(32) },
  { type: "propose", kind: { type: "param_change", key: "max_validators", value: "40" }, deposit: (1000n * 1_000_000_000n).toString(), title: "More validators", url: "https://x.example/p/1" },
  propose({ type: "treasury_spend", to: "x.boo", amount: "9" }),
  propose({ type: "distribution", distribution_id: 6, merkle_root: "ef".repeat(32), total: "100", claim_until: 5000 }),
  propose({ type: "set_repo", repo: "a/b", weight: 10 }),
  propose({ type: "grant_soul", to: "x.boo", amount: 2500, reason: "r" }),
  propose({ type: "flag_sybil", account: "x.boo", reason: "r" }),
  propose({ type: "reject_claim", claim_id: 4, reason: "r" }),
  propose({ type: "clawback", account: "x.boo", reason: "r" }),
  propose({ type: "unlink_github", handle: "vlad" }),
  propose({ type: "refund_hunt", hunt_id: 1 }),
  propose({ type: "schedule_upgrade", version: 2, activation_height: 100_000 }),
  propose({ type: "activate_feature", feature: "ghost" }),
  propose({ type: "deactivate_feature", feature: "ghost" }),
  propose({ type: "text" }),
  { type: "vote", proposal_id: 1, aye: true, amount: "10", conviction: 6, proposal_digest: "d1".repeat(32) },
  { type: "vote", proposal_id: 1, aye: false, amount: "10", conviction: 0, proposal_digest: "d1".repeat(32) },
  { type: "unlock" },
  { type: "multisig_create", owners: ["ab".repeat(32), "cd".repeat(32)], threshold: 2 },
  { type: "multisig_propose", action: { type: "transfer", to: "x.boo", amount: "5" } },
  { type: "multisig_approve", id: 3, action_digest: "a3".repeat(32) },
  { type: "renew_name", name: "vlad", years: 2 },
  { type: "transfer_name", name: "vlad", to: "x.boo" },
  { type: "release_name", name: "vlad" },
  { type: "claim_distribution", distribution_id: 1, amount: "50", proof: ["ab".repeat(32), "cd".repeat(32)] },
  { type: "claim_distribution", distribution_id: 1, amount: "50", proof: [] },
  { type: "attest_revert", claim_id: 4, revert_sha: "ef".repeat(20) },
];
const v1 = v1VectorActions.map((a) => signingBytes(vectorTx(a)));
const v1Joined = new Uint8Array(v1.reduce((n, b) => n + b.length, 0));
v1.reduce((at, b) => (v1Joined.set(b, at), at + b.length), 0);
assert.equal(bytesToHex(blake3(v1Joined)), "a411ddbbcc010f275e102b3d2b2abec2b9e96c6c0e3f69cb50868a07678d4f6e");
// Proposal kinds added after that vector froze, each on its own: `unflag_sybil` (tag 14, audit F54) and `cancel_upgrade` (tag 15, audit F50).
assert.equal(bytesToHex(blake3(signingBytes(vectorTx(propose({ type: "unflag_sybil", account: "x.boo", reason: "r" }))))), "225aa5b8e4df8e8d27fe796992dfd02e5e402c14a9bbc3873d41d63dd07dc2cf");
assert.equal(bytesToHex(blake3(signingBytes(vectorTx(propose({ type: "cancel_upgrade", version: 2, activation_height: 100_000 }))))), "199d2455419f738528317187cdbfa72e5430308b096c2c8741f5801290d3d156");
assert.throws(() => signingBytes(vectorTx({ type: "multisig_propose", action: { type: "multisig_propose", action: { type: "unlock" } } })), /can't contain another/);
// A set of one or two's readiness and admission (tags 60–61, audit round 3), each on its own: `small_set_vector_actions` there.
const smallSetVectorActions: Action[] = [{ type: "validator_ready", epoch: 3 }, { type: "admit_validator", validator: "boo1v", epoch: 3 }];
assert.deepEqual(smallSetVectorActions.map((a) => bytesToHex(blake3(signingBytes(vectorTx(a))))), ["1276620e9888e634548d914538007b797a6b81281a89a27d3f88470804d19012", "75a6f69d5103b23658fc22a88a23a4ba1811d8c52d8a8043cd633b59a3dcf3c1"]);
// Ghost Mode (tags 50–54): signing bytes (the unsigned wire form for 51 and 52) and the sighashes of 50–52, hashed on their own:
// `ghost_vector_actions` there.
const fill = (n: number, len: number) => n.toString(16).padStart(2, "0").repeat(len);
const vectorBundle = (proof: number): GhostBundle => ({ instance: fill(1, 32), anchor: fill(2, 32), nullifiers: [fill(3, 32), fill(4, 32)], commitments: [fill(5, 32), fill(6, 32)], enc: [fill(7, 1401), fill(8, 1401)], out: [fill(9, 80), fill(10, 80)], proof: fill(proof, 300) });
const ghostVectorActions: Action[] = [
  { type: "shield", amount: (5n * 1_000_000_000n).toString(), bundle: vectorBundle(11) },
  { type: "private_transfer", bundle: vectorBundle(12) },
  { type: "unshield", to: "x.boo", amount: "7", bundle: vectorBundle(13) },
  { type: "set_ghost_address", address: fill(14, 1216) },
  { type: "ghost_pause" },
];
const isGhostTransfer = (a: Action) => a.type === "shield" || a.type === "private_transfer" || a.type === "unshield";
const ghostVectorTx = (action: Action): Tx =>
  action.type === "private_transfer" || action.type === "unshield" ? { chain_id: "vec", genesis_hash: "03".repeat(32), from: "", nonce: 0, fee: "12345", tip: "3", valid_until: 99, action, memo: "", public_key: "", signature: "" } : vectorTx(action);
const ghostParts = ghostVectorActions.flatMap((a) => (isGhostTransfer(a) ? [signingBytes(ghostVectorTx(a)), ghostSighash(ghostVectorTx(a))] : [signingBytes(ghostVectorTx(a))]));
const ghostJoined = new Uint8Array(ghostParts.reduce((n, b) => n + b.length, 0));
ghostParts.reduce((at, b) => (ghostJoined.set(b, at), at + b.length), 0);
assert.equal(bytesToHex(blake3(ghostJoined)), "a0673611a2f5de08736a58b4fa66413b17c98255c077dc15432383e68e20414b");
const pt = ghostVectorTx(ghostVectorActions[1]);
assert.equal(bytesToHex(ghostSighash(pt)), "c68439d83ad46343419a1645022271f6de9a343108ed610bbccb33e38a423378");
assert.equal(txId(pt), bytesToHex(blake3(txBytes(pt))));
assert.equal(txSize(pt), txBytes(pt).length);
const vt = vectorTx(vectorActions[0]);
assert.equal(bytesToHex(blake3(txBytes(vt))), "084ce40c9d406468f7373b0aced2b285ff77d353de986752050877e1953501e0");
assert.equal(txBytes(vt).length, txSize(vt));
assert.equal(txId(vt), "250e5fc58d322d13ba15e01991885090daa4e8742cc1f68336a2ca37c8539a22");
assert.equal(txCharge(vt), minFee(vt) + 3n);
// The genesis hash is 32 bytes of lowercase hex, or nothing gets signed.
for (const genesis_hash of ["", "03".repeat(31), "AB".repeat(32), "zz".repeat(32)]) assert.throws(() => signingBytes({ ...vt, genesis_hash }), /genesis_hash/);
// Same vector as node/src/econ.rs `fee_slots_match_the_sdk`: this tx at 12,656 milli-drops a byte.
assert.equal(txSize(vt), 5_373);
assert.equal(baseFee(vt, { flat: "10000", rate: "12656" }), 78_000n);
assert.equal(minFee(vt), 10_000n + 10n * 5_373n);
// Addresses: any known prefix is an address; `isAddress(s, prefix)` asks for one.
const someKey = ml_dsa65.keygen(new Uint8Array(32).fill(4)).publicKey;
const [mainAddr, testAddr] = [addressFromPublicKey(someKey), addressFromPublicKey(someKey, "tboo")];
assert.ok(testAddr.startsWith("tboo1") && isAddress(testAddr) && isAddress(mainAddr, "boo") && !isAddress(testAddr, "boo"));
assert.equal(addressPrefix(testAddr), "tboo");
// Same vectors as node/src/crypto.rs `prefixes_render_and_parse` (Keys::from_seed([4; 32])).
assert.equal(mainAddr, "boo15le8r6nlt2mpl4k9df9zd5ragyw35m9hm5spjr6td8h93kdkps7qaltngu");
assert.equal(testAddr, "tboo15le8r6nlt2mpl4k9df9zd5ragyw35m9hm5spjr6td8h93kdkps7qscv9f0");
assert.ok(!isAddress(testAddr.toUpperCase()));
console.log("✓ shared encoding vectors match the node");

const alice = Wallet.generate(client);
const bob = Wallet.generate(client);
const name = `alice${Date.now() % 100000}`;
console.log("alice:", alice.address);

await alice.faucet();
console.log("after faucet:", await alice.balance());

const reg = await alice.registerName(name);
console.log("✓", reg.summary);

const sent = await alice.send(bob.address, "12.5", { memo: "trick or treat" });
console.log("✓", sent.summary);

const viaName = await bob.client.account(`${name}.boo`);
assert.equal(viaName.account.address, alice.address);

const bobTx = await bob.send(`${name}.boo`, "2");
// Without witnesses the name itself is signed, and the chain pays its owner (audit gap-7-1).
assert.equal(bobTx.action.to, `${name}.boo`);
assert.equal(await bob.balance(), formatFang(parseFang("10.5") - BigInt(bobTx.fee)));
const { chain_id, genesis_hash } = await client.status();
assert.equal(BigInt(bobTx.fee), minFee({ chain_id, genesis_hash, from: bob.address, nonce: 0, fee: "0", valid_until: bobTx.valid_until, action: bobTx.action, memo: "" }));
// The chain's prefix: `boo` on a dev chain, `tboo` on a public testnet (DEC-7); addresses follow it.
const prefix = await client.addressPrefix();
assert.ok(prefix === "boo" || prefix === "tboo", prefix);
assert.equal(addressPrefix(alice.address), prefix);
console.log("✓", bobTx.summary);

// Tips: paid on top of the base fee (which is burned), and the fee covers exactly both.
const tipped = await alice.send(bob.address, "1", { tip: "0.001" });
assert.equal(tipped.tip, "1000000");
assert.equal(BigInt(tipped.fee), BigInt(tipped.max_fee));
console.log("✓ tipped 0.001 FANG:", tipped.fee_display, "charged");

// Expiry: a tx past `valid_until` can never execute, and one too far ahead isn't accepted.
const { public_key: _pk, signature: _sig, ...unsigned } = await alice.build({ type: "transfer", to: bob.address, amount: "1" });
const height = (await client.status()).height;
await assert.rejects(client.submit(alice.sign({ ...unsigned, valid_until: height })), /expired at block/);
await assert.rejects(client.submit(alice.sign({ ...unsigned, valid_until: height + 100_000 })), /too far ahead/);
await assert.rejects(alice.send(bob.address, "1", { validForBlocks: 100_000 }), /at most/);
// Signed for another instance of this chain id (a relaunch): refused, so old txs can't replay.
assert.equal(unsigned.genesis_hash, genesis_hash);
await assert.rejects(client.submit(alice.sign({ ...unsigned, genesis_hash: "00".repeat(32) })), /another instance/);
// The node checks address prefixes, and JSON with unknown fields is refused.
const foreign = prefix === "tboo" ? mainAddr : testAddr;
await assert.rejects(alice.send(foreign, "1"), prefix === "tboo" ? /boo \(mainnet\) address/ : /tboo \(testnet\) address/);
const extra = { ...(await alice.build({ type: "transfer", to: bob.address, amount: "1" })), padding: "x".repeat(100) };
await assert.rejects(client.submit(extra as Tx), (e: unknown) => e instanceof ApiError && e.status === 422);
console.log("✓ validity windows, the chain instance, prefixes and strict JSON enforced");

// Account abstraction: alice lets an agent key spend up to 5 FANG per 100 blocks from her account.
const agent = Wallet.generate(client);
// The agent consents to alice's account (its grant code); a key id alone, or a grant made for
// another account, can't be added (audit gap-7-2).
const agentGrant = await agent.keyGrant(alice.address);
await assert.rejects(alice.addSessionKey(agent.keyId, "claude", { limit: "5", periodBlocks: 100, expiresInBlocks: 1000 }), /that's a key id/);
await assert.rejects(bob.addSessionKey(agentGrant, "claude", { limit: "5", periodBlocks: 100, expiresInBlocks: 1000 }), /that grant is for/);
// Mallory (bob) signs alice's grant into his own AddKey anyway: the node refuses it.
const stolen = { type: "add_key", key_id: agentGrant.key_id, label: "x", role: { type: "owner" }, public_key: agentGrant.public_key, pop: agentGrant.pop } as const;
await assert.rejects(client.submit(await bob.build(stolen)), /proof of possession doesn't verify/);
const grant = await alice.addSessionKey(encodeKeyGrant(agentGrant), "claude", { limit: "5", periodBlocks: 100, expiresInBlocks: 1000 });
console.log("✓", grant.summary);
const listing = await client.keyAccounts(agent.keyId);
assert.deepEqual(listing.accounts.map((a: { account: { address: string } }) => a.account.address).filter((a: string) => a !== agent.address), [alice.address], "only the account it agreed to join lists the key");
assert.equal(listing.next, null);
await agent.actAs(`${name}.boo`);
const paid = await agent.send(bob.address, "3", { memo: "agent errand" });
console.log("✓", paid.summary);
await assert.rejects(agent.send(bob.address, "3"), /limit reached/);
await assert.rejects(agent.registerName(`x${name}`), /only send transfers/);
console.log("✓ session key limits enforced");

// A second device (e.g. another passkey) gets full control, then revokes the agent.
const phone = Wallet.generate(client);
await alice.addOwnerKey(await phone.keyGrant(alice.address), "phone");
await phone.actAs(alice.address);
await phone.removeKey(agent.keyId);
await assert.rejects(agent.send(bob.address, "0.1"), /isn't authorized/);
console.log("✓ second device revoked the agent");

// Validator onboarding: anyone can bond and register (a candidate, selected by stake at an epoch
// boundary); on dev and test chains only admins may pin or remove validators. The stand-in runs no
// node, so its owner (an operator account alice funds) retires it at once by unbonding its
// self-bond: it never joins a multi-validator set.
const vinfo = await client.validators();
// Stands in for a validator node's consensus key; it proves possession by signing the registration.
const nodeKeys = ml_dsa65.keygen(randomBytes(32));
const node = { address: addressFromPublicKey(nodeKeys.publicKey) };
const consensusKey = Buffer.from(nodeKeys.publicKey).toString("hex");
const p2pAddr = "203.0.113.7:30300", httpAddr = "203.0.113.7:7070";
const chainId = (await client.status()).chain_id;
const operator = Wallet.generate(client);
const pop = Buffer.from(ml_dsa65.sign(popMessage(chainId, operator.address, p2pAddr, httpAddr), nodeKeys.secretKey, { context: POP_CONTEXT })).toString("hex");
const minBond = BigInt(vinfo.min_bond);
if (minBond + parseFang("2") <= BigInt((await client.account(alice.address)).balance)) {
  await alice.send(operator.address, minBond + parseFang("1"));
  // A PoP for someone else's key (or another owner) is rejected.
  const badPop = Buffer.from(ml_dsa65.sign(popMessage(chainId, bob.address, p2pAddr, httpAddr), nodeKeys.secretKey, { context: POP_CONTEXT })).toString("hex");
  await assert.rejects(operator.registerValidator({ consensusKey, p2pAddr, httpAddr, bond: minBond, pop: badPop }), /pop/);
  const reg = await operator.registerValidator({ consensusKey, p2pAddr, httpAddr, bond: minBond, pop });
  console.log("✓", reg.summary);
  const after = await client.validators();
  // A candidate, or (if an epoch boundary passed since it registered, which on a multi-validator
  // chain with free seats selects it) already in the set: the retirement below covers both.
  const listed = (vs: any[]) => vs.some((v: any) => v.validator.address === node.address);
  assert.ok(listed(after.candidates) || listed(after.active), "a registered validator is a candidate (or was selected at a boundary since)");
  await assert.rejects(operator.approveValidator(node.address), /only validator admins/);
  await assert.rejects(operator.unbond(node.address), /Unbond is gone/);
  console.log(`✓ validator registered as a candidate (epoch ${after.epoch}, ${after.blocks_until_next_epoch} blocks to the next one)`);
  console.log("✓", (await operator.undelegate(node.address, "all")).summary);
  const retired = await client.validators();
  assert.ok(![...retired.candidates, ...retired.joining].some((v: any) => v.validator.address === node.address), "a validator below the minimum self-bond isn't a candidate");
  assert.equal(stakeSummary(await client.staking(operator.address)).unbonding.length, 1);
  console.log("✓ validator retired: under the minimum self-bond it can't be selected");
} else {
  console.log(`(skipping validator registration: min bond ${vinfo.min_bond_display} exceeds the faucet drip)`);
}

// Hunts: bob escrows a bounty on a GitHub issue, alice chips in, bob awards it to alice's
// Familiar (an agent session key she registered).
const agent2 = Wallet.generate(client);
await alice.addSessionKey(await agent2.keyGrant(alice.address), "claude", { limit: "1", periodBlocks: 100, expiresInBlocks: 1000 });
console.log("✓", (await alice.registerFamiliar(agent2.keyId, "claude")).summary);
const created = await bob.createHunt({ repo: "jackdishman/blockula", issue: 1, amount: "10", title: "Shared tx test vectors", expiresInBlocks: 500 });
console.log("✓", created.summary);
const huntId = (await client.account(bob.address)).hunts_created.at(-1);
console.log("✓", (await alice.fundHunt(huntId, "1")).summary);
await assert.rejects(alice.awardHunt(huntId, alice.address, 7), /only the hunt's creator/);
const awarded = await bob.awardHunt(huntId, `${name}.boo`, 42, agent2.keyId);
console.log("✓", awarded.summary);
const h = await client.hunt(huntId);
assert.equal(h.status, "awarded");
assert.equal(h.amount_display, "11 FANG");
assert.equal(h.familiar, "claude");
assert.equal(h.pr_url, "https://github.com/jackdishman/blockula/pull/42");
await assert.rejects(bob.cancelHunt(huntId), /already/);
const prof = await client.account(alice.address);
assert.equal(prof.hunts_won.length, 1);
assert.equal(prof.familiars[0].label, "claude");
// GitHub link: a claim on-chain, verified off-chain by the node (this gist id doesn't exist).
console.log("✓", (await alice.linkGithub(`alice${Date.now() % 100000}`, "0123456789abcdef")).summary);
assert.equal((await client.github((await client.account(alice.address)).github.handle)).verified, false);
console.log("✓ hunts, Familiars and GitHub links work");

// 🪦 Proof of Contribution. Same vector as node/src/types.rs `contribution_encodings_match_the_sdk`.
assert.equal(seanceCommitment(3, "boo1juror", "ab=", [1, 2], "pepper"), "1f3071e1123a713836e2c9c27798dc74ed1fd1f97de6b5fd985c0dba8f9f7eae");
assert.equal(parseSoul("2.5"), 2500);
// Same vector as node/src/beacon/tests.rs `hashes_are_domain_separated_and_stable`.
assert.equal(beaconCommitment("beacon", 2, "boo1v", "07".repeat(32)), "04a492304d681e4905279135f0a7120f14ef2ea203f0b0dfdfa289c12f2e2f8a");
// Every new action is signed by the SDK and verified by the node; these fail on the rules, not the signature.
await assert.rejects(alice.joinJury("10"), /jurors need at least 5 Soul/);
await assert.rejects(alice.vouch(bob.address), /at least 10 Soul/);
await assert.rejects(alice.unvouch(bob.address), /has no vouch/);
await assert.rejects(alice.submitClaim("jackdishman/blockula", 1), /Soul or a vouch|isn't in the contribution pool/);
await assert.rejects(alice.leaveJury(), /not in the jury pool/);
const ghostPairs = [[{ id: 1, repo: "a/b", pr: 1 }, { id: 2, repo: "a/b", pr: 2 }], [{ id: 2, repo: "a/b", pr: 2 }, { id: 1, repo: "a/b", pr: 1 }]] as [{ id: number; repo: string; pr: number }, { id: number; repo: string; pr: number }][];
await assert.rejects(alice.sealVotes(999_999, "ab", [], undefined, { pairs: ghostPairs }), /no Séance/);
await assert.rejects(alice.revealVotes(999_999, "ab", [3], "salt"), /no Séance/);
await assert.rejects(alice.withdrawVested(), /nothing has vested/);
await assert.rejects(alice.client.submit(await alice.build({ type: "attest_claim", claim_id: 0, ok: true, merge_sha: "ab".repeat(20), note: "" })), /only active validators/);
for (const admin of [() => alice.setRepo("a/b", 1), () => alice.grantSoul(bob.address, "1", "x"), () => alice.rejectClaim(0, "x"), () => alice.flagSybil(bob.address, "x"), () => alice.clawback(bob.address, "x")]) {
  await assert.rejects(admin(), /only validator admins/);
}
const poolBefore = BigInt((await client.contributions()).pool.balance);
console.log("✓", (await alice.fundPool("1")).summary);
// (Issuance also pays the pool its share at every epoch boundary.)
assert.ok(BigInt((await client.contributions()).pool.balance) >= poolBefore + parseFang("1"));
await assert.rejects(client.seance(999_999), /not found/);
assert.ok(Array.isArray((await client.souls()).souls));

// The admin key below (BLOCKULA_ADMIN_SEED: scripts/local-testnet.sh passes validator 0's) and
// the council key further down are a validator's own, and its node signs its duties (beacon
// commitments and reveals, claim attestations) from the same account (`Chain::system_fee_payer`),
// so what the e2e signs with them races those duties for the account's nonces. A duty signed at
// the same moment takes the nonce ("bad nonce"). A queued duty that stops applying refuses what's
// signed behind it ("no longer applies") until the next block drops it, along with what was
// already queued behind it ("an earlier transaction from the same account was dropped"): a local
// testnet's validators attest a claim at the same tick, and those whose attestation arrives after
// the two refutations that settle it have it dropped. None of these ran, so the tx is signed again
// (up to 5 tries, 1 s apart). A refused tx, or one dropped once another used its nonce, can never
// run. One dropped behind a gap could, if the gap fills (the node's next duty takes that nonce) and
// its bytes are resubmitted, so it's signed again only when running it twice is harmless
// (`idempotent`): a repo's weight is set, and an account's vote on a proposal replaces its earlier
// one (`governance::vote`).
async function asValidator<T>(what: string, send: () => Promise<T>, idempotent = false): Promise<T> {
  for (let attempt = 1; ; attempt++) {
    try {
      return await send();
    } catch (e) {
      const why = (e as Error).message;
      const dead = (e instanceof ApiError && e.status === 400 && /bad nonce|no longer applies/.test(why)) || (e instanceof TxFailedError && e.dropped && /bad nonce/.test(why));
      const orphaned = e instanceof TxFailedError && e.dropped && /earlier transaction from the same account was dropped/.test(why);
      if (attempt === 5 || !(dead || (orphaned && idempotent))) throw e;
      console.log(`  (${what} raced the validator's own duties for its account's nonce: ${why}; signing it again)`);
      await new Promise((r) => setTimeout(r, 1_000));
    }
  }
}

// With a Season 0 admin key, run the admin side: a repo in the pool, Soul grants, a juror, a
// vouch, and a claim that validators check on GitHub.
if (process.env.BLOCKULA_ADMIN_SEED) {
  const admin = Wallet.fromSeed(process.env.BLOCKULA_ADMIN_SEED.trim(), client);
  console.log("✓", (await asValidator("setRepo", () => admin.setRepo("jackdishman/blockula", 10), true)).summary);
  console.log("✓", (await asValidator("grantSoul", () => admin.grantSoul(alice.address, "12", "e2e bootstrap"))).summary);
  console.log("✓", (await alice.joinJury("10")).summary);
  console.log("✓", (await alice.vouch(bob.address)).summary);
  await bob.faucet();
  await bob.linkGithub(`bob${Date.now() % 100000}`, "0123456789abcdef");
  const pr = 1_000_000 + (Date.now() % 1_000_000);
  const claimed = await bob.submitClaim("jackdishman/blockula", pr);
  console.log("✓", claimed.summary);
  const mine = (await client.claims({ account: bob.address })).claims;
  assert.equal(mine[0].pr, pr);
  assert.equal(mine[0].status, "pending");
  const profile = (await client.account(alice.address)).contribution;
  assert.equal(profile.juror.active, true);
  assert.equal(profile.vouching_for[0].account.address, bob.address);
  console.log("✓", (await alice.leaveJury()).summary);
  console.log("✓ contribution pool admin flow (validators will check the claim on GitHub)");
}
console.log("✓ Proof of Contribution txs verify and enforce their rules");

// Protocol v1 (tags 30–49): every module is live, and each flow below runs to its end on the dev
// chain's short durations (`blockula dev`: 1-minute epochs, unbonding in ~2–3 minutes, votes in a
// minute, a 20-second timelock). Responses are read with the SDK's readers, so this also checks
// that they read what the node serves.
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function until<T>(what: string, f: () => Promise<T | undefined | null | false>, timeoutMs = 600_000): Promise<T> {
  const start = Date.now();
  for (;;) {
    const v = await f();
    if (v) return v;
    if (Date.now() - start > timeoutMs) throw new Error(`timed out waiting for ${what}`);
    await sleep(2_000);
  }
}
const validators = await client.validators();
const validator: string = validators.active[0].validator.address;
assert.equal(validators.active[0].status, "active");
assert.ok(/^\d+$/.test(validators.active[0].stake) && validators.active[0].eligible === true, "pools are served with stake and eligibility");

// The fee market: each upcoming block's base fee is fixed 17 blocks ahead; a tx is charged its
// block's base fee for its size (burned) plus its tip, never more than its fee.
const feeView = await client.fees();
assert.equal(feeView.lag, 17);
assert.equal(feeView.slots.length, 18);
const priced = await alice.send(bob.address, "1", { tip: "0.000002" });
const pricedBlock = await client.block(priced.height);
assert.ok(pricedBlock.base_fee, "executed blocks show the base fee they charged");
const slot = pricedBlock.base_fee;
assert.equal(BigInt(priced.fee), BigInt(slot.flat) + (BigInt(slot.rate) * BigInt(priced.size_bytes)) / 1000n + BigInt(priced.tip), "charged: the block's base fee for its size + the tip");
assert.ok(BigInt(priced.fee) <= BigInt(priced.max_fee));
console.log(`✓ fee market: charged ${priced.fee_display} (base ${pricedBlock.base_fee.flat} + ${pricedBlock.base_fee.rate}‰ per byte, plus the tip) of a ${formatFang(BigInt(priced.max_fee))} max fee`);

// Staking: delegate to the dev validator, unbond part of it, wait out the unbonding period, withdraw.
console.log("✓", (await alice.delegate(validator, "3")).summary);
const staked = stakeSummary(await client.staking(alice.address));
const pool = staked.delegations.find((d) => d.validator === validator);
if (!pool) throw new Error("the delegation isn't served");
// Worth at least what went in (share rounding is in the pool's favour by under a drop a share,
// and issuance may have paid the pool since: on a dev chain the pool is a sliver of the supply,
// so its share price climbs fast).
assert.ok(BigInt(pool.principal) === parseFang("3") && BigInt(pool.value) + 1_000n >= parseFang("3") && pool.self_bond === false, `the delegation is served with its value: ${JSON.stringify(pool)}`);
// By amount (audit gap-7-5): signed as 2 FANG, converted by the chain at the pool's price when it
// runs, rounded up, so at least 2 FANG unbonds (and under a share's worth more).
const byAmount = await alice.undelegate(validator, { amount: "2" });
assert.deepEqual([byAmount.action?.shares, byAmount.action?.amount], ["0", parseFang("2").toString()], JSON.stringify(byAmount.action));
console.log("✓", byAmount.summary);
const unbonding = stakeSummary(await client.staking(alice.address));
assert.equal(unbonding.unbonding.length, 1);
assert.equal(unbonding.unbonding[0].validator, validator);
assert.ok(BigInt(unbonding.unbonding_total) >= parseFang("2") && BigInt(unbonding.unbonding_total) < parseFang("2") + 1_000n && unbonding.unbonding_total === unbonding.unbonding[0].amount, JSON.stringify(unbonding));
// More than the rest is worth is refused by the chain (nothing is unbonded).
await assert.rejects(alice.undelegate(validator, { amount: "2" }), /less than the 2 FANG asked for/);
assert.ok(unbonding.delegations.some((d) => d.validator === validator && BigInt(d.principal) < parseFang("3")), "the sale took principal out");
await assert.rejects(alice.withdrawUnbonded(), /nothing has finished unbonding/);
await assert.rejects(alice.unjail(validator), /only validator .*'s owner|isn't jailed/);
console.log(`✓ unbonding until block #${unbonding.unbonding[0].completes_at} (withdrawn below)`);

// Names renew (from the old expiry), and the name endpoint says when they lapse.
const before = await client.name(name);
assert.equal(before.status, "active");
assert.ok(/^\d+$/.test(before.renewal_price));
const renewed = await alice.renewName(name, 1);
console.log("✓", renewed.summary);
assert.equal((await client.name(name)).expires_at, before.expires_at + before.registration_blocks);

// Governance: a ParamChange proposal, voted through and executed after its timelock. Nothing on
// a dev chain can reach the token quorum (the validator holds nearly all the supply), so the
// bootstrap council passes it: the dev validator's key (BLOCKULA_ADMIN_SEED, or the dev home's
// validator.key) votes for its validator.
const councilSeed = process.env.BLOCKULA_ADMIN_SEED ?? (process.env.BLOCKULA_HOME ? readFileSync(`${process.env.BLOCKULA_HOME}/dev/validator.key`, "utf8") : undefined);
const rules = (await client.proposals({ limit: 1 })).params;
assert.ok(/^\d+$/.test(rules.proposal_deposit) && rules.voting_blocks > 0, "the governance rules are served");
const paramsBefore = paramsOf(await client.params());
const target = Number(paramsBefore.max_validators) === 31 ? 30 : 31;
// (A chain without dev durations asks a deposit the faucet doesn't cover: governance's refusals
// below still run there.)
let proposalId: number | undefined;
let votingEnds = 0;
if (BigInt(rules.proposal_deposit) + parseFang("5") <= BigInt((await client.account(alice.address)).balance)) {
  await assert.rejects(alice.propose(proposal.paramChange("max_validators", 65), { title: "too many", deposit: BigInt(rules.proposal_deposit) }), /max_validators/);
  const proposed = await alice.propose(proposal.paramChange("max_validators", target), { title: `e2e: max_validators ${target}`, url: "https://example.com/e2e" });
  console.log("✓", proposed.summary);
  const id: number = (await client.proposals({ limit: 1 })).proposals[0].id;
  proposalId = id;
  const opened = proposalSummary(await client.proposal(id), rules);
  assert.deepEqual([opened.status, opened.title, opened.supermajority, opened.proposer], ["voting", `e2e: max_validators ${target}`, false, alice.address]);
  console.log("✓", (await alice.vote(id, true, "1", 1)).summary);
  await assert.rejects(alice.vote(id, true, "1000000", 1), /vote with up to/);
  // Voted tokens are locked: they can't be sent away (and voted again from another account) until
  // the lock lapses. Bob holds no stake, so the lock covers his liquid FANG.
  console.log("✓", (await bob.vote(id, true, "5", 1)).summary);
  await assert.rejects(bob.send(alice.address, BigInt((await client.account(bob.address)).balance) - parseFang("1")), /is locked \(vote:/);
  const voted = proposalSummary(await client.proposal(id), rules);
  assert.deepEqual([voted.aye_amount, voted.aye, voted.voters], [parseFang("6").toString(), parseFang("6").toString(), 2]);
  votingEnds = voted.voting_ends_at;
  if (councilSeed) {
    const council = Wallet.fromSeed(councilSeed.trim(), client);
    console.log("✓", (await asValidator("the council vote", () => council.vote(id, true, "1", 0), true)).summary);
    assert.equal(proposalSummary(await client.proposal(id)).council.ayes, 1, "the validator's vote counts in the council");
  }
} else {
  console.log(`(skipping a proposal: its ${formatFang(BigInt(rules.proposal_deposit))} deposit is more than the faucet gave)`);
}

// Staking, continued: once the unbonding period is over, withdraw (on chains with dev durations:
// elsewhere unbonding takes weeks).
const { height: nowHeight } = await client.status();
if (unbonding.unbonding[0].completes_at - nowHeight <= 900) {
  console.log(`  waiting for the unbonding entry (free at block #${unbonding.unbonding[0].completes_at})…`);
  await until("the unbonding entry", async () => stakeSummary(await client.staking(alice.address)).unbonding[0]?.ready);
  const beforeWithdraw = BigInt((await client.account(alice.address)).balance);
  const withdrawn = await alice.withdrawUnbonded();
  console.log("✓", withdrawn.summary);
  assert.equal(BigInt((await client.account(alice.address)).balance), beforeWithdraw + BigInt(unbonding.unbonding_total) - BigInt(withdrawn.fee));
  const afterWithdraw = stakeSummary(await client.staking(alice.address));
  assert.deepEqual([afterWithdraw.unbonding.length, afterWithdraw.withdrawable], [0, "0"]);
  console.log("✓ staking: delegated, unbonded, withdrew;", afterWithdraw.summary);
} else {
  console.log(`(not waiting ${unbonding.unbonding[0].completes_at - nowHeight} blocks for the unbonding to finish: this chain doesn't have dev durations)`);
}

// The council key passes the proposal alone only if its validator is the whole set (a dev chain).
if (proposalId !== undefined && councilSeed && validators.active.length === 1 && votingEnds - (await client.status()).height <= 900) {
  const id = proposalId;
  console.log(`  waiting for the vote to close at block #${votingEnds} and the timelock…`);
  const done = await until("the proposal to execute", async () => {
    const p = proposalSummary(await client.proposal(id), rules);
    return p.status === "executed" || p.status === "rejected" || p.status === "failed" ? p : undefined;
  });
  assert.equal(done.status, "executed", done.outcome);
  assert.equal(done.council.of, 1);
  assert.equal(paramsOf(await client.params()).max_validators, String(target));
  console.log(`✓ governance: proposal #${id} passed by the council and ran (${done.outcome}); max_validators is ${target}`);
  // Unlock once the vote's lock has run out.
  await until("the vote lock to expire", async () => {
    try {
      return await alice.unlock();
    } catch (e) {
      if (/no expired vote locks/.test((e as Error).message)) return undefined;
      throw e;
    }
  });
  console.log("✓ vote locks released");
} else if (proposalId !== undefined) {
  console.log("(not waiting for the proposal to run: that needs a dev chain's single validator as the council, via BLOCKULA_ADMIN_SEED or BLOCKULA_HOME)");
}

// Refused for their own reasons, not the encoding.
const governance: [Action, RegExp][] = [
  [{ type: "vote", proposal_id: 1_000_000, aye: true, amount: "1", conviction: 1, proposal_digest: "00".repeat(32) }, /no proposal #1000000/],
  [{ type: "multisig_propose", action: { type: "transfer", to: bob.address, amount: "1" } }, /isn't a multisig account/],
  [{ type: "claim_distribution", distribution_id: 1_000_000, amount: "1", proof: ["cd".repeat(32)] }, /no distribution #1000000/],
  // `cancel_upgrade` (F50): the node decodes the SDK's encoding and refuses what isn't scheduled.
  [{ type: "propose", kind: proposal.cancelUpgrade(2, 1_000_000), deposit: rules.proposal_deposit, title: "e2e: cancel an upgrade" }, /no upgrade to v2 at block 1000000 is scheduled/],
];
for (const [action, why] of governance) {
  await assert.rejects(client.submit(await alice.build(action)), why, action.type);
}
await assert.rejects(client.multisig(alice.address), (e: unknown) => e instanceof ApiError && e.status === 404);
await assert.rejects(client.distribution(1_000_000), (e: unknown) => e instanceof ApiError && e.status === 404);
// The randomness beacon and revert attestations are live, for validators only.
const epoch2 = (await client.status()).epoch + 2;
await assert.rejects(client.submit(await alice.build({ type: "beacon_commit", epoch: epoch2, commitment: beaconCommitment("blockula-dev", epoch2, alice.address, "07".repeat(32)) })), /commit to the beacon/);
await assert.rejects(client.submit(await alice.build({ type: "attest_revert", claim_id: 0, revert_sha: "cd".repeat(20) })), /only active validators attest reverts/);
// Ghost Mode ships off: its txs are refused until governance activates it.
assert.equal((await (await fetch(`${client.rpc}/api/ghost`)).json()).active, false);
await assert.rejects(alice.client.submit(await alice.build({ type: "set_ghost_address", address: "00".repeat(1216) })), /Ghost Mode is off|valid 1216-byte/);

// Every v1 helper signs a tx the node verifies (refusals come from the rules, not the signature).
async function verifiedByNode(label: string, run: () => Promise<unknown>) {
  try {
    await run();
  } catch (e) {
    const msg = (e as Error).message;
    assert.ok(e instanceof TxFailedError || (e instanceof ApiError && e.status === 400), `${label}: ${msg}`);
    assert.ok(!/signature|signing bytes|unknown field|wire form|decode/i.test(msg), `${label} was refused before the rules: ${msg}`);
  }
}
const helpers: [string, () => Promise<unknown>][] = [
  ["redelegate", () => alice.redelegate(validator, bob.address, 1n)],
  ["updateValidator", () => alice.updateValidator({ commissionBps: 500 })],
  ["multisigApprove", () => alice.multisigApprove(0, { action: { type: "unlock" } })],
  ["beaconCommit", () => alice.beaconCommit(1, "ab".repeat(32))],
  ["claimDistribution", () => alice.claimDistribution(0, "1", ["cd".repeat(32)])],
];
for (const [label, run] of helpers) await verifiedByNode(label, run);
console.log(`✓ ${helpers.length} more protocol v1 helpers sign txs the node verifies`);

// Proven reads: /api/proof answers check out against the state root, and the node agrees with
// itself as a witness. Names and accounts that don't exist are proven absent.
const provenAlice = await client.provenAccount(alice.address, { witnesses: [new Client(client.rpc)] });
assert.ok(provenAlice, "alice's account is proven");
assert.equal(provenAlice.balance.toString(), (await client.account(alice.address)).balance);
assert.equal(provenAlice.nonce, (await client.account(alice.address)).nonce);
assert.ok(provenAlice.keys.some((k) => k.key_id === agent2.keyId && k.role.type === "session"), "alice's session key is proven");
assert.equal((await client.provenName(name))?.owner, alice.address);
assert.equal(await client.provenAccount(Wallet.generate(client).address), null);
assert.equal(await client.provenName(`nobody${Date.now() % 100000}`), null);
const raw = await client.proof(NAMESPACES.ACCOUNTS, alice.address);
assert.ok(verifyProof(raw.root, NAMESPACES.ACCOUNTS, alice.address, hexToBytes(raw.value as string), raw.proof));
assert.ok(!verifyProof(raw.root, NAMESPACES.ACCOUNTS, bob.address, hexToBytes(raw.value as string), raw.proof));
console.log(`✓ proven reads: ${provenAlice.balance} drops at block #${provenAlice.height}, root ${provenAlice.root.slice(0, 12)}…`);

const offered = await alice.transferName(`${name}.boo`, bob.address);
console.log("✓", offered.summary);
assert.equal(await client.resolve(`${name}.boo`), alice.address, "an offer: still alice's");
assert.equal((await client.name(name)).offered_to?.address, bob.address);
const moved = await bob.acceptName(name);
console.log("✓", moved.summary);
assert.equal(await client.resolve(`${name}.boo`), bob.address);
await assert.rejects(alice.releaseName(name), /belongs to/);
console.log("✓", (await bob.releaseName(name)).summary);
assert.equal((await client.name(name)).available, true);
console.log("✓ protocol v1 txs verify; names renew, transfer and release");

const status = await client.status();
console.log(`✓ all good at height ${status.height}, burned so far: ${status.burned_display}`);
