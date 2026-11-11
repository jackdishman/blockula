#!/usr/bin/env node
// For deploy/host-bots.sh: for each house name, says whether its house key owns it, and accepts it
// when it's been offered to that key's account (`acceptName`). Signs nothing else.
//
//   node deploy/bots/accept-names.mjs <hosted dir> [--network demo | --rpc URL]
//
// <hosted dir> holds trickortreat-key.json and guestbook-key.json. Exits 0 when it has looked at
// both (whether or not they're the house's yet) and prints one line per name.
import { readFileSync } from "node:fs";
import { Client, Wallet } from "@blockula/sdk";

const [dir, ...rest] = process.argv.slice(2);
if (!dir) {
  console.error("usage: node deploy/bots/accept-names.mjs <hosted dir> [--network demo | --rpc URL]");
  process.exit(2);
}
const flag = (f) => (rest.indexOf(f) >= 0 ? rest[rest.indexOf(f) + 1] : undefined);
const rpc = flag("--rpc");
const client = rpc ? new Client(rpc) : new Client({ network: flag("--network") ?? "demo" });
await client.addressPrefix();

for (const name of ["trickortreat", "guestbook"]) {
  const wallet = Wallet.fromSeed(JSON.parse(readFileSync(`${dir}/${name}-key.json`, "utf8")).seed, client);
  const me = wallet.address;
  const info = await client.name(name);
  const owner = info.owner?.address ?? info.owner;
  const offered = info.offered_to?.address ?? info.offered_to;
  if (owner === me) {
    console.log(`✓ ${name}.boo is the house's (${me})`);
  } else if (offered === me) {
    const r = await wallet.acceptName(name);
    console.log(`✓ ${name}.boo accepted by ${me}: ${r.summary ?? r.id}`);
  } else if (info.status === "available") {
    console.log(`… ${name}.boo is free: its bot registers it when it starts (${me})`);
  } else {
    console.log(`… ${name}.boo belongs to ${owner} and isn't offered to the house (${me}) yet: its bot waits`);
  }
}
