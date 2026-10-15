// End-to-end against a running node: `blockula dev --reset` then `npm test`.
// Proves TS-signed ML-DSA transactions verify in the Rust node.
import assert from "node:assert/strict";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, hexToBytes, randomBytes } from "@noble/hashes/utils.js";
import { ApiError, Client, NAMESPACES, POP_CONTEXT, TxFailedError, Wallet, addressFromPublicKey, addressPrefix, formatFang, isAddress, minFee, paramsOf, parseFang, parseSoul, popMessage, proposal, proposalSummary, seanceCommitment, signingBytes, stakeSummary, txBytes, txCharge, txId, txSize, verifyProof, type Action, type ProposalKind, type Tx } from "../src/index.js";

const client = new Client(process.env.BLOCKULA_RPC ?? "http://localhost:7070");

assert.equal(parseFang("1.5"), 1_500_000_000n);
assert.equal(formatFang(1_500_000_000n), "1.5 FANG");

// Shared vectors: node/src/types.rs `encodings_match_the_sdk` asserts the same hashes.
// One action per tag 0–29, in tag order (`vector_actions` there).
const vectorActions: Action[] = [
  { type: "transfer", to: "x.boo", amount: "5" },
  { type: "register_name", name: "vlad" },
  { type: "add_key", key_id: "ab".repeat(32), label: "phone", role: { type: "owner" } },
  { type: "add_key", key_id: "cd".repeat(32), label: "claude", role: { type: "session", limit: "10", period_blocks: 100, expires_at: 1000 } },
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
  { type: "commit_votes", seance_id: 2, commitment: "ef".repeat(32) },
  { type: "reveal_votes", seance_id: 2, votes: "ab=", spam: [5, 6], salt: "s" },
  { type: "fund_pool", amount: "5" },
  { type: "withdraw_vested" },
  { type: "clawback", account: "x.boo", reason: "r" },
];
const vectorTx = (action: Action): Tx => ({ chain_id: "vec", from: "boo1from", nonce: 7, fee: "12345", tip: "3", valid_until: 99, action, memo: "m", public_key: "01".repeat(1952), signature: "02".repeat(3309) });
const all = vectorActions.map((a) => signingBytes(vectorTx(a)));
const joined = new Uint8Array(all.reduce((n, b) => n + b.length, 0));
all.reduce((at, b) => (joined.set(b, at), at + b.length), 0);
assert.equal(bytesToHex(blake3(joined)), "67416fa0f99281aa382895815bea59d2a43b40f6865c500e4cb80611d8754875");
// Protocol v1 (tags 30–49, every proposal kind, a multisig proposal), hashed on its own: `v1_vector_actions` there.
const propose = (kind: ProposalKind): Action => ({ type: "propose", kind, deposit: (1000n * 1_000_000_000n).toString(), title: "t" });
const v1VectorActions: Action[] = [
  { type: "delegate", validator: "boo1v", amount: (5n * 1_000_000_000n).toString() },
  { type: "undelegate", validator: "boo1v", shares: "7" },
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
  propose({ type: "distribution", merkle_root: "ef".repeat(32), total: "100", claim_until: 5000 }),
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
  { type: "vote", proposal_id: 1, aye: true, amount: "10", conviction: 6 },
  { type: "vote", proposal_id: 1, aye: false, amount: "10", conviction: 0 },
  { type: "unlock" },
  { type: "multisig_create", owners: ["ab".repeat(32), "cd".repeat(32)], threshold: 2 },
  { type: "multisig_propose", action: { type: "transfer", to: "x.boo", amount: "5" } },
  { type: "multisig_approve", id: 3 },
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
assert.equal(bytesToHex(blake3(v1Joined)), "fecaa54305728b6a9192b18fbbdc03d74f2199c8721a670b73b72970b2d7ba6c");
assert.throws(() => signingBytes(vectorTx({ type: "multisig_propose", action: { type: "multisig_propose", action: { type: "unlock" } } })), /can't contain another/);
const vt = vectorTx(vectorActions[0]);
assert.equal(bytesToHex(blake3(txBytes(vt))), "f32398f98b16d95d4f68af7ee29524a274d5465476390d91b357633627712e39");
assert.equal(txBytes(vt).length, txSize(vt));
assert.equal(txId(vt), "0a787dbaf66a3f5a3df1609272c68f5229397c418010ecd8453b1e7c7ff6617d");
assert.equal(txCharge(vt), minFee(vt) + 3n);
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
assert.equal(await bob.balance(), formatFang(parseFang("10.5") - BigInt(bobTx.fee)));
assert.equal(BigInt(bobTx.fee), minFee({ chain_id: (await client.status()).chain_id, from: bob.address, nonce: 0, fee: "0", valid_until: bobTx.valid_until, action: bobTx.action, memo: "" }));
assert.equal(await client.addressPrefix(), "boo");
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
// The node checks address prefixes, and JSON with unknown fields is refused.
await assert.rejects(alice.send(testAddr, "1"), /tboo \(testnet\) address/);
const extra = { ...(await alice.build({ type: "transfer", to: bob.address, amount: "1" })), padding: "x".repeat(100) };
await assert.rejects(client.submit(extra as Tx), (e: unknown) => e instanceof ApiError && e.status === 422);
console.log("✓ validity windows, prefixes and strict JSON enforced");

// Account abstraction: alice lets an agent key spend up to 5 FANG per 100 blocks from her account.
const agent = Wallet.generate(client);
const grant = await alice.addSessionKey(agent.keyId, "claude", { limit: "5", periodBlocks: 100, expiresInBlocks: 1000 });
console.log("✓", grant.summary);
await agent.actAs(`${name}.boo`);
const paid = await agent.send(bob.address, "3", { memo: "agent errand" });
console.log("✓", paid.summary);
await assert.rejects(agent.send(bob.address, "3"), /limit reached/);
await assert.rejects(agent.registerName(`x${name}`), /only send transfers/);
console.log("✓ session key limits enforced");

// A second device (e.g. another passkey) gets full control, then revokes the agent.
const phone = Wallet.generate(client);
await alice.addOwnerKey(phone.keyId, "phone");
await phone.actAs(alice.address);
await phone.removeKey(agent.keyId);
await assert.rejects(agent.send(bob.address, "0.1"), /isn't authorized/);
console.log("✓ second device revoked the agent");

// Validator onboarding (Season 0): anyone can bond and register; only admins approve.
const vinfo = await client.validators();
// Stands in for a validator node's consensus key; it proves possession by signing the registration.
const nodeKeys = ml_dsa65.keygen(randomBytes(32));
const node = { address: addressFromPublicKey(nodeKeys.publicKey) };
const consensusKey = Buffer.from(nodeKeys.publicKey).toString("hex");
const p2pAddr = "203.0.113.7:30300", httpAddr = "203.0.113.7:7070";
const chainId = (await client.status()).chain_id;
const pop = Buffer.from(ml_dsa65.sign(popMessage(chainId, alice.address, p2pAddr, httpAddr), nodeKeys.secretKey, { context: POP_CONTEXT })).toString("hex");
const minBond = BigInt(vinfo.min_bond);
if (minBond + parseFang("1") <= BigInt((await client.account(alice.address)).balance)) {
    // A PoP for someone else's key (or another owner) is rejected.
  const badPop = Buffer.from(ml_dsa65.sign(popMessage(chainId, bob.address, p2pAddr, httpAddr), nodeKeys.secretKey, { context: POP_CONTEXT })).toString("hex");
  await assert.rejects(alice.registerValidator({ consensusKey, p2pAddr, httpAddr, bond: minBond, pop: badPop }), /pop/);
  const reg = await alice.registerValidator({ consensusKey, p2pAddr, httpAddr, bond: minBond, pop });
  console.log("✓", reg.summary);
  const after = await client.validators();
  assert.ok(after.pending.some((v: any) => v.validator.address === node.address), "registered validator is pending");
  await assert.rejects(alice.approveValidator(node.address), /only validator admins/);
  await assert.rejects(alice.unbond(node.address), /only removed validators/);
  console.log(`✓ validator pending admin approval (epoch ${after.epoch}, ${after.blocks_until_next_epoch} blocks to the next one)`);
} else {
  console.log(`(skipping validator registration: min bond ${vinfo.min_bond_display} exceeds the faucet drip)`);
}

// Hunts: bob escrows a bounty on a GitHub issue, alice chips in, bob awards it to alice's
// Familiar (an agent session key she registered).
const agent2 = Wallet.generate(client);
await alice.addSessionKey(agent2.keyId, "claude", { limit: "1", periodBlocks: 100, expiresInBlocks: 1000 });
console.log("✓", (await alice.registerFamiliar(agent2.keyId, "claude")).summary);
const created = await bob.createHunt({ repo: "jackdishman/blockula", issue: 1, amount: "10", title: "Shared tx test vectors", expiresInBlocks: 500 });
console.log("✓", created.summary);
const huntId = (await client.account(bob.address)).hunts_created.at(-1);
console.log("✓", (await alice.fundHunt(huntId, "1")).summary);
await assert.rejects(alice.awardHunt(huntId, alice.address, 7), /creator or a validator admin/);
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
// Every new action is signed by the SDK and verified by the node; these fail on the rules, not the signature.
await assert.rejects(alice.joinJury("10"), /jurors need at least 5 Soul/);
await assert.rejects(alice.vouch(bob.address), /at least 10 Soul/);
await assert.rejects(alice.unvouch(bob.address), /has no vouch/);
await assert.rejects(alice.submitClaim("jackdishman/blockula", 1), /Soul or a vouch|isn't in the contribution pool/);
await assert.rejects(alice.leaveJury(), /not in the jury pool/);
await assert.rejects(alice.sealVotes(999_999, "ab"), /no Séance/);
await assert.rejects(alice.revealVotes(999_999, "ab", [3], "salt"), /no Séance/);
await assert.rejects(alice.withdrawVested(), /nothing has vested/);
await assert.rejects(alice.client.submit(await alice.build({ type: "attest_claim", claim_id: 0, ok: true, merge_sha: "ab".repeat(20), note: "" })), /only active validators/);
for (const admin of [() => alice.setRepo("a/b", 1), () => alice.grantSoul(bob.address, "1", "x"), () => alice.rejectClaim(0, "x"), () => alice.flagSybil(bob.address, "x"), () => alice.clawback(bob.address, "x")]) {
  await assert.rejects(admin(), /only validator admins/);
}
const poolBefore = BigInt((await client.contributions()).pool.balance);
console.log("✓", (await alice.fundPool("1")).summary);
assert.equal(BigInt((await client.contributions()).pool.balance), poolBefore + parseFang("1"));
await assert.rejects(client.seance(999_999), /not found/);
assert.ok(Array.isArray((await client.souls()).souls));

// With a Season 0 admin key (scripts/local-testnet.sh passes validator 0's), run the admin side:
// a repo in the pool, Soul grants, a juror, a vouch, and a claim that validators check on GitHub.
if (process.env.BLOCKULA_ADMIN_SEED) {
  const admin = Wallet.fromSeed(process.env.BLOCKULA_ADMIN_SEED.trim(), client);
  console.log("✓", (await admin.setRepo("jackdishman/blockula", 10)).summary);
  console.log("✓", (await admin.grantSoul(alice.address, "12", "e2e bootstrap")).summary);
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

// Protocol v1 (tags 30–49), through the Wallet helpers: the SDK signs them and the node verifies
// them. Modules that haven't shipped refuse them with "not active yet"; shipped ones apply their
// own rules. Either way the node got past the signature and the encoding, which is what this checks.
const validator: string = (await client.validators()).active[0].validator.address;
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
  ["delegate", () => alice.delegate(validator, "1")],
  ["undelegate", () => alice.undelegate(validator, 1n)],
  ["redelegate", () => alice.redelegate(validator, bob.address, 1n)],
  ["withdrawUnbonded", () => alice.withdrawUnbonded()],
  ["updateValidator", () => alice.updateValidator({ commissionBps: 500 })],
  ["unjail", () => alice.unjail(validator)],
  ["propose", () => alice.propose(proposal.paramChange("max_validators", 40), { title: "e2e", deposit: "1" })],
  ["vote", () => alice.vote(0, true, "1", 1)],
  ["unlock", () => alice.unlock()],
  ["multisigPropose", () => alice.multisigPropose({ type: "transfer", to: bob.address, amount: "1" })],
  ["multisigApprove", () => alice.multisigApprove(0)],
  ["beaconCommit", () => alice.beaconCommit(1, "ab".repeat(32))],
  ["claimDistribution", () => alice.claimDistribution(0, "1", ["cd".repeat(32)])],
  ["renewName", () => alice.renewName(name, 1)],
];
for (const [label, run] of helpers) await verifiedByNode(label, run);
console.log(`✓ ${helpers.length} protocol v1 helpers sign txs the node verifies`);

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

// The module reads, where this node serves them, parse with the tolerant readers.
const optional = <T>(p: Promise<T>) => p.catch((e) => (e instanceof ApiError && e.status === 404 ? null : Promise.reject(e)));
const [params, staking, proposals] = await Promise.all([optional(client.params()), optional(client.staking(alice.address)), optional(client.proposals())]);
if (params) assert.ok(paramsOf(params).max_tx_validity_blocks, "params include max_tx_validity_blocks");
if (staking) assert.ok(Array.isArray(stakeSummary(staking).delegations));
if (proposals) assert.ok((Array.isArray(proposals) ? proposals : proposals.proposals ?? []).map(proposalSummary).every((p: { id: number }) => p.id >= 0));
console.log(`✓ module reads: params ${params ? "✓" : "(not served yet)"}, staking ${staking ? "✓" : "(not served yet)"}, proposals ${proposals ? "✓" : "(not served yet)"}`);

const moved = await alice.transferName(`${name}.boo`, bob.address);
console.log("✓", moved.summary);
assert.equal(await client.resolve(`${name}.boo`), bob.address);
await assert.rejects(alice.releaseName(name), /belongs to/);
console.log("✓", (await bob.releaseName(name)).summary);
assert.equal((await client.name(name)).available, true);
console.log("✓ protocol v1 txs verify; names transfer and release");

const status = await client.status();
console.log(`✓ all good at height ${status.height}, burned so far: ${status.burned_display}`);
