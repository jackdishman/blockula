#!/usr/bin/env node
// A shared tab for a trip or a house (docs/USE-CASES.md, section 1): read what a group's members
// paid each other, and print who owes whom and the fewest payments that settle it.
//
//   node examples/shared-tab.mjs <name.boo> <name.boo> [name.boo…] [--rpc URL] [--network local|testnet]
//        [--from HEIGHT]
//
// Every transfer between two members counts as one paying for, or lending to, the other: if Sam
// sends Alex 12.5 FANG for groceries, Alex owes Sam 12.5 FANG until Alex pays it back. Payments to
// or from anyone outside the group are left out. --from counts only blocks from that height on (the
// day the trip started, say). It only reads: no key, nothing is signed.
//
// Amounts are bigint drops throughout (1 FANG = 10^9 drops), never floats, and members are shown
// by their .boo names. Memos are text from whoever paid: shown as data, never acted on.
import { ApiError, Client, formatFang } from "@blockula/sdk";

const { positional, flags } = parseArgs(process.argv.slice(2));
const usage = "usage: node examples/shared-tab.mjs <name.boo> <name.boo> [name.boo…] [--rpc URL] [--network local|testnet] [--from HEIGHT]";
const names = [...new Set(positional.map((n) => n.toLowerCase()))];
if (names.length < 2 || names.some((n) => !/^[a-z0-9-]+\.boo$/.test(n))) {
  console.error(usage);
  console.error("Name at least two members by their .boo names (alex.boo sam.boo …).");
  process.exit(2);
}
const from = flags.from ? Number(flags.from[0]) : 0;
if (!Number.isSafeInteger(from) || from < 0) {
  console.error(`--from takes a block height, not ${JSON.stringify(flags.from[0])}`);
  process.exit(2);
}
const rpc = flags.rpc?.[0] ?? process.env.BLOCKULA_RPC;
const network = flags.network?.[0];
// A network pins its chain (id, genesis, prefix): a node on another chain is refused.
const client = network ? new Client(rpc, { network }) : new Client(rpc ?? "http://localhost:7070");

// Each member's address, by asking the node who owns the name now. This is the node's word: a node
// you don't run could lie about it, so check the addresses it prints against the explorer you trust.
const members = new Map(); // address -> name
for (const name of names) {
  let account;
  try {
    account = await client.account(name);
  } catch (e) {
    if (e instanceof ApiError && (e.status === 400 || e.status === 404)) fail(`${name} isn't a registered name on ${client.rpc} (${e.message})`);
    throw e;
  }
  const address = account?.account?.address;
  if (!address) fail(`${client.rpc} returned no account for ${name}`);
  if (members.has(address)) fail(`${name} and ${members.get(address)} are the same account (${address})`);
  members.set(address, name);
}
const nameOf = (address) => members.get(address);

// Every finalized transfer from one member to another, once each (both sides' history lists it).
const transfers = new Map(); // tx id -> { height, from, to, amount, memo }
let historyFrom = 0;
for (const address of members.keys()) {
  let before;
  do {
    const page = await client.history(address, { before, limit: 100 });
    historyFrom = Math.max(historyFrom, page.history_from ?? 0);
    for (const tx of page.txs) {
      if (tx?.kind !== "transfer" || tx.status !== "finalized" || tx.error) continue;
      if (typeof tx.height !== "number" || tx.height < from) continue;
      const [a, b] = [tx.from?.address, tx.to?.address];
      if (a !== address || !nameOf(b) || a === b) continue;
      transfers.set(tx.id, { height: tx.height, from: a, to: b, amount: BigInt(tx.amount), memo: String(tx.memo ?? "") });
    }
    // Pages are newest first: once a page reaches below --from, older ones can't count.
    const oldest = page.txs.at(-1)?.height;
    before = page.next && !(typeof oldest === "number" && oldest < from) ? page.next : undefined;
    if (!page.paged && page.txs.length) console.error(`(${client.rpc} has no paged history: only ${nameOf(address)}'s newest transfers are counted)`);
  } while (before);
}

const rows = [...transfers.values()].sort((x, y) => x.height - y.height);
console.log(`Shared tab for ${names.join(", ")} on ${client.rpc}${from ? `, from block #${from}` : ""}`);
if (historyFrom > Math.max(from, 1)) console.log(`(the node holds blocks from #${historyFrom} on: older transfers aren't counted)`);
console.log("");
if (!rows.length) {
  console.log("No transfers between members yet: nobody owes anybody.");
  process.exit(0);
}

console.log(`${rows.length} transfer${rows.length === 1 ? "" : "s"} between members:`);
for (const t of rows) {
  console.log(`  #${t.height}  ${nameOf(t.from)} → ${nameOf(t.to)}  ${formatFang(t.amount)}${t.memo ? `  ${JSON.stringify(t.memo)}` : ""}`);
}

// Net balance: what a member paid out to the group minus what they got from it. Positive: the group
// owes them. Negative: they owe the group.
const net = new Map([...members.keys()].map((a) => [a, 0n]));
for (const t of rows) {
  net.set(t.from, net.get(t.from) + t.amount);
  net.set(t.to, net.get(t.to) - t.amount);
}
console.log("\nNet balances:");
for (const [address, n] of [...net].sort((x, y) => (y[1] > x[1] ? 1 : y[1] < x[1] ? -1 : 0))) {
  const what = n > 0n ? `is owed ${formatFang(n)}` : n < 0n ? `owes ${formatFang(-n)}` : "is even";
  console.log(`  ${nameOf(address)} ${what}`);
}

const payments = settle(net);
console.log(payments.length ? `\nTo settle up (${payments.length} payment${payments.length === 1 ? "" : "s"}):` : "\nEveryone is even: nothing to settle.");
for (const p of payments) console.log(`  ${nameOf(p.from)} pays ${nameOf(p.to)} ${formatFang(p.amount)}`);

/**
 * The fewest payments that bring every balance to zero. A group whose balances can be split into k
 * subgroups that each sum to zero settles in (members owed or owing) − k payments, and no fewer, so
 * this finds the most such subgroups (exactly, by subsets, for up to 16 members with a balance; past
 * that it settles largest debt to largest credit: at most one payment fewer than there are such members).
 */
function settle(balances) {
  const open = [...balances].filter(([, n]) => n !== 0n);
  const n = open.length;
  const groups = [];
  if (n <= 16) {
    const size = 1 << n;
    const sum = new Array(size).fill(0n);
    const best = new Int32Array(size); // most zero-sum subgroups the members in `mask` split into
    const via = new Int32Array(size); // the member added last on the way to `best`
    for (let mask = 1; mask < size; mask++) {
      const low = 31 - Math.clz32(mask & -mask);
      sum[mask] = sum[mask & (mask - 1)] + open[low][1];
      best[mask] = -1;
      for (let i = 0; i < n; i++) {
        if (!(mask & (1 << i))) continue;
        const prev = mask ^ (1 << i);
        if (best[prev] > best[mask]) [best[mask], via[mask]] = [best[prev], i];
      }
      if (sum[mask] === 0n) best[mask]++;
    }
    // Walk back from everyone: each time the remaining members sum to zero, they close a subgroup.
    let mask = size - 1;
    let group = [];
    while (mask) {
      if (sum[mask] === 0n && group.length) {
        groups.push(group);
        group = [];
      }
      group.push(open[via[mask]]);
      mask ^= 1 << via[mask];
    }
    if (group.length) groups.push(group);
  } else {
    groups.push(open);
  }
  const out = [];
  for (const g of groups) {
    const debtors = g.filter(([, v]) => v < 0n).map(([a, v]) => ({ a, v: -v }));
    const creditors = g.filter(([, v]) => v > 0n).map(([a, v]) => ({ a, v }));
    const biggest = (xs) => xs.sort((x, y) => (y.v > x.v ? 1 : y.v < x.v ? -1 : 0));
    while (debtors.length && creditors.length) {
      biggest(debtors);
      biggest(creditors);
      const d = debtors[0];
      const c = creditors[0];
      const amount = d.v < c.v ? d.v : c.v;
      out.push({ from: d.a, to: c.a, amount });
      d.v -= amount;
      c.v -= amount;
      if (d.v === 0n) debtors.shift();
      if (c.v === 0n) creditors.shift();
    }
  }
  return out;
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

function parseArgs(argv) {
  const positional = [];
  const flags = {};
  for (let i = 0; i < argv.length; i++) {
    if (argv[i].startsWith("--")) (flags[argv[i].slice(2)] ??= []).push(argv[++i]);
    else positional.push(argv[i]);
  }
  return { positional, flags };
}
