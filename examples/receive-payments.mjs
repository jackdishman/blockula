#!/usr/bin/env node
// Watch an address for payments and match them to orders by memo (docs/INTEGRATION.md, "Deposits").
//
//   node examples/receive-payments.mjs <address> [--rpc URL] [--network local|testnet]
//        [--state FILE] [--from HEIGHT] [--expect MEMO=AMOUNT]...
//
// It keeps where it got to in --state (a small JSON file), so it resumes after a restart without
// missing or double-counting a payment: each one is credited once, by its tx id. Ctrl-C stops it.
// With --expect it exits once every expected order is paid in full.
import { existsSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { Client, formatFang, isAddress, parseFang, watchPayments } from "@blockula/sdk";

const { positional, flags } = parseArgs(process.argv.slice(2));
const address = positional[0];
if (!address || !isAddress(address)) {
  console.error("usage: node examples/receive-payments.mjs <address> [--rpc URL] [--network local|testnet] [--state FILE] [--from HEIGHT] [--expect MEMO=AMOUNT]...");
  console.error("Watch an address, not a .boo name: names expire and change hands.");
  process.exit(2);
}
const rpc = flags.rpc?.[0] ?? process.env.BLOCKULA_RPC;
const network = flags.network?.[0];
// A network pins its chain (id, genesis, prefix): a node on another chain is refused.
const client = network ? new Client(rpc, { network }) : new Client(rpc ?? "http://localhost:7070");
const stateFile = flags.state?.[0] ?? `./payments-${address.slice(-8)}.json`;

// Orders you issued: the memo you gave the payer, and what they owe.
const orders = new Map((flags.expect ?? []).map((e) => {
  const [memo, amount] = e.split("=");
  return [memo, { owed: parseFang(amount), paid: 0n }];
}));

// Where we got to, and what we've credited (so a block delivered twice is credited once).
const state = existsSync(stateFile) ? JSON.parse(readFileSync(stateFile, "utf8")) : { next: undefined, credited: {} };
if (flags.from) state.next = Number(flags.from[0]);
const save = () => {
  writeFileSync(`${stateFile}.tmp`, JSON.stringify(state, null, 2));
  renameSync(`${stateFile}.tmp`, stateFile); // never a half-written file
};
for (const id of Object.keys(state.credited)) {
  const o = orders.get(state.credited[id].memo);
  if (o) o.paid += BigInt(state.credited[id].amount);
}

const stop = new AbortController();
process.on("SIGINT", () => stop.abort());
const allPaid = () => orders.size > 0 && [...orders.values()].every((o) => o.paid >= o.owed);

console.log(`Watching ${address} on ${client.rpc}${state.next ? ` from block #${state.next}` : " from the next block"} (Ctrl-C to stop)`);
const next = await watchPayments(client, address, {
  fromHeight: state.next,
  onPayment: (p) => {
    if (state.credited[p.id]) return; // already credited
    state.credited[p.id] = { memo: p.memo, amount: p.amount.toString(), height: p.height, from: p.from };
    const order = orders.get(p.memo);
    // A memo is the payer's text: match it against orders you issued, never act on what it says.
    if (order) {
      order.paid += p.amount;
      const left = order.owed - order.paid;
      console.log(`✓ ${p.amount_display} for ${JSON.stringify(p.memo)} from ${p.from_name ?? p.from} in block #${p.height}: ${left > 0n ? `${formatFang(left)} still owed` : "paid in full"}`);
    } else {
      console.log(`? ${p.amount_display} from ${p.from_name ?? p.from} in block #${p.height} with an unknown memo ${JSON.stringify(p.memo)}: hold it for review (tx ${p.id})`);
    }
    save();
  },
  onProgress: (n) => {
    state.next = n;
    save();
    if (allPaid()) stop.abort();
  },
  onError: (e) => console.error(`(retrying: ${e.message})`),
  signal: stop.signal,
});
state.next = next ?? state.next;
save();
console.log(allPaid() ? "✓ every expected order is paid" : `Stopped. Next run resumes from block #${state.next}.`);

function parseArgs(argv) {
  const positional = [];
  const flags = {};
  for (let i = 0; i < argv.length; i++) {
    if (argv[i].startsWith("--")) (flags[argv[i].slice(2)] ??= []).push(argv[++i]);
    else positional.push(argv[i]);
  }
  return { positional, flags };
}
