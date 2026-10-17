// Fill a local dev chain with a little of everything the explorer shows: names, payments with
// memos (one tipped, one opening a new account), a second owner device and an agent on a budget
// (each joining with its grant code), hunts and their funders, a donation to the contribution
// pool, and protocol v1: a delegation, a proposal and a vote on it (its lock), a renewal, a name
// offered and accepted and another still on offer, and a 2-of-2 multisig with a pending action.
// Their (dev-chain) keys are written to `<out>/seed-wallets.json` (mode 600, your temp dir by
// default) so a browser-only wallet can be opened with one of them.
//
//   blockula dev --reset &
//   npm run build -w @blockula/sdk
//   node scripts/seed-dev.mjs [rpc=http://localhost:7070] [out=$TMPDIR]
import { writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { Client, proposal, Wallet } from "@blockula/sdk";

const rpc = process.argv[2] || "http://localhost:7070";
const out = process.argv[3] || os.tmpdir();
const client = new Client(rpc);

const status = await client.status();
if (!status.faucet) throw new Error(`${rpc} has no faucet; seed a dev chain (blockula dev)`);
await client.addressPrefix();

const wallet = async (name) => {
  const w = Wallet.generate(client);
  await w.faucet();
  if (name) await w.registerName(name);
  console.log(`✓ ${name ?? w.address}`);
  return w;
};

const vlad = await wallet("vlad");
const mina = await wallet("mina");
const lucy = await wallet("lucy");
const renfield = await wallet("renfield");
await vlad.faucet();

await vlad.send("mina.boo", "12.5", { memo: "for the castle repairs" });
await mina.send("vlad.boo", "3", { memo: "garlic, returned" });
await vlad.send("lucy.boo", "7");
await lucy.send("renfield.boo", "1.25", { memo: "flies" });
await renfield.send("vlad.boo", "0.5", { memo: "tribute" });
// A tip to the block's proposer, on top of the base fee that burns.
await mina.send("lucy.boo", "0.25", { memo: "for the proposer too", tip: "0.0001" });
// A first payment to a fresh address opens its account, which burns the account-creation fee.
await vlad.send(Wallet.generate(client).address, "1", { memo: "a new account" });
console.log("✓ payments");

// A second owner device on vlad's account (a phone), signing a payment of its own. Keys join an
// account only with their consent: the grant code the key's holder makes for that account.
const phone = Wallet.generate(client);
await vlad.addOwnerKey(await phone.keyGrant(vlad.address), "Vlad's phone");
await phone.actAs(vlad.address);
await phone.send("mina.boo", "2", { memo: "sent from my phone" });

// An agent with a daily budget, spending through its session key.
const agent = Wallet.generate(client);
await vlad.addSessionKey(await agent.keyGrant(vlad.address), "claude", { limit: "10", periodBlocks: 86_400, expiresInBlocks: 30 * 86_400 });
await agent.actAs(vlad.address);
for (const [to, amount, memo] of [["mina.boo", "1", "api credits"], ["lucy.boo", "0.75", "dataset"], ["renfield.boo", "0.4", "a tip"], ["mina.boo", "1.5", "more api credits"]]) {
  await agent.send(to, amount, { memo });
}
await vlad.registerFamiliar(agent.keyId, "claude");
console.log("✓ a phone and an agent on vlad.boo");

// Hunts, funded by several accounts; one awarded.
const { height } = await client.status();
await vlad.createHunt({ repo: "jackdishman/blockula", issue: 14, amount: "25", title: "Balance journal endpoint for exact balance charts", expiresInBlocks: 200_000 });
await mina.createHunt({ repo: "jackdishman/blockula", issue: 15, amount: "12", title: "Decode certificate signers in /api/block", expiresInBlocks: 200_000 });
const hunts = (await client.hunts()).hunts;
const first = hunts.find((h) => h.issue === 14);
const second = hunts.find((h) => h.issue === 15);
await lucy.fundHunt(first.id, "5");
await renfield.fundHunt(first.id, "2");
await mina.awardHunt(second.id, "lucy.boo", 7);
await mina.fundPool("4");
console.log("✓ hunts and the pool");

// Protocol v1: stake, a proposal and a vote (which locks), names that lapse and change hands.
const validator = (await client.validators()).active[0]?.validator.address;
if (validator) await renfield.delegate(validator, "20");
await vlad.faucet();
const deposit = (await client.params()).params?.proposal_deposit;
const made = await vlad.propose(proposal.paramChange("max_validators", 40), { title: "Room for more validators", url: "https://github.com/jackdishman/blockula", deposit: deposit ? BigInt(deposit) : "10" });
const proposals = (await client.proposals()).proposals;
const prop = proposals.find((p) => p.title === "Room for more validators") ?? proposals[0];
await lucy.vote(prop.id, true, "5", 1, { proposal: { kind: prop.kind, title: prop.title, url: prop.url } });
await lucy.renewName("lucy", 1);
await vlad.registerName("castle");
await vlad.transferName("castle", "mina.boo");
await mina.acceptName("castle");
await vlad.registerName("tomb");
await vlad.transferName("tomb", "lucy.boo");
console.log(`✓ a delegation, proposal #${prop.id} (${made.id.slice(0, 8)}…) with a vote, a renewal, castle.boo accepted by mina, tomb.boo offered to lucy`);

// A 2-of-2 multisig: both owner keys must approve what it does.
const coven = await wallet("coven");
const second_owner = Wallet.generate(client);
await coven.addOwnerKey(await second_owner.keyGrant(coven.address), "the other witch");
await coven.multisigCreate([coven.keyId, second_owner.keyId], 2);
await coven.multisigPropose({ type: "transfer", to: "mina.boo", amount: "2000000000" });
console.log("✓ coven.boo, a 2-of-2 multisig with one action waiting");

writeFileSync(
  path.join(out, "seed-wallets.json"),
  JSON.stringify({ rpc, from_height: height, wallets: { vlad: vlad.seedHex, mina: mina.seedHex, lucy: lucy.seedHex, renfield: renfield.seedHex, phone: phone.seedHex, agent: agent.seedHex, coven: coven.seedHex, coven_second: second_owner.seedHex } }, null, 2),
  { mode: 0o600 },
);
console.log(`\nSeeded ${rpc}. Keys in ${path.join(out, "seed-wallets.json")}.`);
