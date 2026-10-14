// End-to-end against a running node: `blockula dev --reset` then `npm test`.
// Proves TS-signed ML-DSA transactions verify in the Rust node.
import assert from "node:assert/strict";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { randomBytes } from "@noble/hashes/utils.js";
import { Client, POP_CONTEXT, Wallet, addressFromPublicKey, formatFang, minFee, parseFang, parseSoul, popMessage, seanceCommitment } from "../src/index.js";

const client = new Client(process.env.BLOCKULA_RPC ?? "http://localhost:7070");

assert.equal(parseFang("1.5"), 1_500_000_000n);
assert.equal(formatFang(1_500_000_000n), "1.5 FANG");

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
assert.equal(BigInt(bobTx.fee), minFee({ chain_id: (await client.status()).chain_id, from: bob.address, nonce: 0, fee: "0", action: bobTx.action, memo: "" }));
console.log("✓", bobTx.summary);

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

const status = await client.status();
console.log(`✓ all good at height ${status.height}, burned so far: ${status.burned_display}`);
