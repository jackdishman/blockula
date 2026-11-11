#!/usr/bin/env node
// A guestbook on a chain: anyone signs it by sending a payment with a memo to its name.
//
//   node examples/guestbook.mjs serve [--name guestbook] [--rpc URL] [--network local|demo|testnet]
//        [--key FILE] [--faucet] [--state FILE] [--from HEIGHT]
//   node examples/guestbook.mjs read [--name guestbook] [--limit 20] [--rpc URL] [--network local|demo|testnet]
//
// serve: owns the name (registering it if it's free) and logs each new signature once, by tx id,
//        resuming from the stored height after a restart. It signs nothing and pays nothing.
// read:  prints the latest signatures for a guestbook name from any node. It needs no key.
//
// Signatures are strangers' text. They're printed as inert text only: control characters
// (including escape sequences and newlines) are replaced and the length is capped. Nothing here
// reads a memo for meaning.
import { existsSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { randomBytes } from "node:crypto";
import { ApiError, Client, Wallet, formatFang, parseFang, watchPayments } from "@blockula/sdk";

const { positional, flags } = parseArgs(process.argv.slice(2));
const mode = positional[0];
if (mode !== "serve" && mode !== "read") {
  console.error("usage: node examples/guestbook.mjs serve|read [--name guestbook] [--rpc URL] [--network local|demo|testnet] [--key FILE] [--faucet] [--state FILE] [--limit N]");
  process.exit(2);
}
const name = (flags.name ?? "guestbook").replace(/\.boo$/, "");
const nameFull = `${name}.boo`;
const rpc = flags.rpc ?? process.env.BLOCKULA_RPC;
// A network pins its chain (id, genesis, prefix): a node on another chain is refused.
const client = flags.network ? new Client(rpc, { network: flags.network }) : new Client(rpc ?? "http://localhost:7070");

const MAX_SHOWN = 120;
/** Text from a stranger, made safe to print: control characters become spaces, and it's cut short. */
const inert = (s) => {
  const t = String(s ?? "").replace(/[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/gu, " ").replace(/\s+/g, " ").trim();
  const chars = [...t];
  return chars.length > MAX_SHOWN ? `${chars.slice(0, MAX_SHOWN).join("")}…` : t;
};
const line = (s) => `${inert(s.from_name ?? s.from)} signed with ${s.amount_display}: ${JSON.stringify(inert(s.memo))}`;

if (mode === "read") {
  const limit = Number(flags.limit ?? 20);
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 200) {
    console.error("--limit must be from 1 to 200");
    process.exit(2);
  }
  const info = await client.name(nameFull).catch((e) => (e instanceof ApiError && e.status === 404 ? null : Promise.reject(e)));
  const owner = typeof info?.owner === "string" ? info.owner : info?.owner?.address;
  if (!owner) {
    console.error(`${nameFull} has no owner on ${client.rpc} (${info?.status ?? "unknown"}).`);
    process.exit(1);
  }
  // Newest first, from the account's history; only payments that paid it count as signatures.
  const signatures = [];
  let before;
  for (let pages = 0; signatures.length < limit && pages < 50; pages++) {
    const page = await client.history(owner, { before, limit: 100 });
    for (const t of page.txs ?? page.items ?? []) {
      if (t.kind === "transfer" && t.status === "finalized" && !t.error && (typeof t.to === "string" ? t.to : t.to?.address) === owner) {
        signatures.push({ from: typeof t.from === "string" ? t.from : t.from?.address, from_name: t.from?.name ?? null, amount_display: t.amount_display ?? formatFang(t.amount ?? "0"), memo: t.memo ?? "", height: t.height });
      }
    }
    before = page.next;
    if (!before) break;
  }
  console.log(`${nameFull} (${owner}) on ${client.rpc}: ${Math.min(limit, signatures.length)} latest signature${signatures.length === 1 ? "" : "s"}`);
  for (const s of signatures.slice(0, limit)) console.log(`  #${s.height} ${line(s)}`);
  process.exit(0);
}

// serve
const keyFile = flags.key ?? "./guestbook-key.json";
const stateFile = flags.state ?? `./guestbook-${name}.json`;
let keyHex;
if (existsSync(keyFile)) keyHex = JSON.parse(readFileSync(keyFile, "utf8")).seed;
else {
  keyHex = randomBytes(32).toString("hex");
  writeFileSync(keyFile, JSON.stringify({ ["seed"]: keyHex }), { mode: 0o600 });
  console.log(`A new key, written to ${keyFile}`);
}
const wallet = Wallet.fromSeed(keyHex, client);
await client.addressPrefix();
const me = wallet.address;

let info = await client.name(nameFull).catch((e) => (e instanceof ApiError && e.status === 404 ? null : Promise.reject(e)));
if (!info || info.status === "available") {
  if (flags.faucet) {
    await wallet.faucet();
    console.log("Funded by the faucet");
  }
  const done = await wallet.registerName(nameFull);
  console.log(`Registered ${nameFull}: ${done.summary}`);
  info = await client.name(nameFull);
}
const owner = typeof info.owner === "string" ? info.owner : info.owner?.address;
if (owner !== me) {
  console.error(`${nameFull} belongs to ${owner ?? "someone else"} (${info.status}), not to this key. Pick another --name or use that key.`);
  process.exit(1);
}

const state = existsSync(stateFile) ? JSON.parse(readFileSync(stateFile, "utf8")) : { next: undefined, seen: {} };
if (flags.from) state.next = Number(flags.from);
const save = () => {
  writeFileSync(`${stateFile}.tmp`, JSON.stringify(state, null, 2));
  renameSync(`${stateFile}.tmp`, stateFile);
};
const stop = new AbortController();
process.on("SIGINT", () => stop.abort());

console.log(`The guestbook ${nameFull} is at ${me} on ${client.rpc}${state.next ? `, from block #${state.next}` : ", from the next block"} (Ctrl-C to stop)`);
const next = await watchPayments(client, me, {
  fromHeight: state.next,
  onPayment: (p) => {
    if (state.seen[p.id]) return; // each signature is logged once
    state.seen[p.id] = p.height;
    console.log(`#${p.height} ${line(p)}`);
    save();
  },
  onProgress: (n) => {
    state.next = n;
    save();
  },
  onError: (e) => console.error(`(retrying: ${e.message})`),
  signal: stop.signal,
}).catch((e) => {
  console.error(`Stopped: ${e.message}`);
  process.exitCode = 1;
});
if (next !== undefined) state.next = next;
save();
console.log(`Stopped. Next run resumes from block #${state.next}.`);

function parseArgs(argv) {
  const positional = [];
  const flags = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--faucet") flags[a.slice(2)] = true;
    else if (a.startsWith("--")) flags[a.slice(2)] = argv[++i];
    else positional.push(a);
  }
  return { positional, flags };
}
