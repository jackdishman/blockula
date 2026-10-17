// Fill a local dev chain with a little of everything the explorer shows: names, payments with
// memos, a second owner device, an agent on a budget spending through its session key, hunts
// and their funders, and a donation to the contribution pool. Their (dev-chain) keys are written
// to `<out>/seed-wallets.json` (mode 600, your temp dir by default) so a browser-only wallet can
// be opened with one of them.
//
//   blockula dev --reset &
//   npm run build -w @blockula/sdk
//   node scripts/seed-dev.mjs [rpc=http://localhost:7070] [out=$TMPDIR]
import { writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { Client, Wallet } from "@blockula/sdk";

const rpc = process.argv[2] || "http://localhost:7070";
const out = process.argv[3] || os.tmpdir();
const client = new Client(rpc);

const status = await client.status();
if (!status.faucet) throw new Error(`${rpc} has no faucet; seed a dev chain (blockula dev)`);

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

// A second owner device on vlad's account (a phone), signing a payment of its own.
const phone = Wallet.generate(client);
await vlad.addOwnerKey(phone.keyId, "Vlad's phone");
await phone.actAs("vlad.boo");
await phone.send("mina.boo", "2", { memo: "sent from my phone" });

// An agent with a daily budget, spending through its session key.
const agent = Wallet.generate(client);
await vlad.addSessionKey(agent.keyId, "claude", { limit: "10", periodBlocks: 86_400, expiresInBlocks: 30 * 86_400 });
await agent.actAs("vlad.boo");
for (const [to, amount, memo] of [["mina.boo", "1", "api credits"], ["lucy.boo", "0.75", "dataset"], ["renfield.boo", "0.4", "a tip"], ["mina.boo", "1.5", "more api credits"]]) {
  await agent.send(to, amount, { memo });
}
await vlad.registerFamiliar(agent.keyId, "claude");

// Hunts, funded by several accounts.
const { height } = await client.status();
await vlad.createHunt({ repo: "jackdishman/blockula", issue: 14, amount: "25", title: "Balance journal endpoint for exact balance charts", expiresInBlocks: 200_000 });
await mina.createHunt({ repo: "jackdishman/blockula", issue: 15, amount: "12", title: "Decode certificate signers in /api/block", expiresInBlocks: 200_000 });
const hunts = (await client.hunts()).hunts;
const first = hunts.find((h) => h.issue === 14);
await lucy.fundHunt(first.id, "5");
await renfield.fundHunt(first.id, "2");
await mina.fundPool("4");

writeFileSync(
  path.join(out, "seed-wallets.json"),
  JSON.stringify({ rpc, from_height: height, wallets: { vlad: vlad.seedHex, mina: mina.seedHex, lucy: lucy.seedHex, renfield: renfield.seedHex, phone: phone.seedHex, agent: agent.seedHex } }, null, 2),
  { mode: 0o600 },
);
console.log(`\nSeeded ${rpc}. Keys in ${path.join(out, "seed-wallets.json")}.`);
