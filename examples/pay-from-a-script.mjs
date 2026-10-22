#!/usr/bin/env node
// Pay an address from a script, the way a withdrawal service should (docs/INTEGRATION.md,
// "Withdrawals"): quote the full cost, sign, store the signed tx before it leaves this machine,
// submit it, wait for it, keep the receipt. If anything is unclear afterwards, rebroadcast the
// same signed bytes; never sign the payment again.
//
//   node examples/pay-from-a-script.mjs <to> <amount> [--memo TEXT] [--rpc URL] [--network local|testnet|mainnet]
//        (--key FILE | --faucet) [--outbox FILE] [--dry-run]
//   node examples/pay-from-a-script.mjs --rebroadcast <tx id> [--rpc URL] [--network …] [--outbox FILE]
//
// --key FILE: a key file written by `blockula wallet new` ($BLOCKULA_HOME/wallet.json).
// --faucet:   a new key, funded by the chain's faucet (blockula dev and test chains only).
import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { ApiError, Client, MainnetNotAllowedError, TxFailedError, TxPendingError, Wallet, formatFang, txId } from "@blockula/sdk";

const { positional, flags } = parseArgs(process.argv.slice(2));
const rpc = flags.rpc ?? process.env.BLOCKULA_RPC;
// A network pins its chain (id, genesis, prefix); mainnet also needs it to sign at all.
const client = flags.network ? new Client(rpc, { network: flags.network }) : new Client(rpc ?? "http://localhost:7070");
const outbox = flags.outbox ?? "./outbox.jsonl";

if (flags.rebroadcast) {
  // The same bytes again: a node answers with the same id, and the tx runs at most once.
  const entry = readOutbox().find((e) => e.id === flags.rebroadcast);
  if (!entry) fail(`${flags.rebroadcast} isn't in ${outbox}`);
  await settle(entry.tx, entry.id, { rebroadcast: true });
  process.exit(0);
}

const [to, amount] = positional;
if (!to || !amount || (!flags.key && !flags.faucet)) {
  console.error("usage: node examples/pay-from-a-script.mjs <to> <amount> [--memo TEXT] [--rpc URL] [--network local|testnet|mainnet] (--key FILE | --faucet) [--outbox FILE] [--dry-run]");
  process.exit(2);
}

let wallet;
if (flags.key) {
  const file = JSON.parse(readFileSync(flags.key, "utf8"));
  wallet = Wallet.fromSeed(file.seed, client);
} else {
  wallet = Wallet.generate(client);
  await wallet.faucet();
  console.log(`A new key, funded by the faucet: ${wallet.address}`);
}
// Withdrawals get room for the base fee to rise while they wait. Only what's due is charged.
wallet.feeHeadroom = 2;

// 1. What it costs, before anything is signed: the fee, and the 0.1 FANG burned when it opens an account.
const memo = flags.memo ?? "";
const q = await wallet.quote(to, amount, { memo });
console.log(`Paying ${q.display.amount} to ${to}${q.recipient.byChain ? ` (${q.recipient.how})` : ""}`);
console.log(`  fee           ${q.display.fee}${q.maxFee > q.fee ? ` (at most ${q.display.maxFee})` : ""}`);
if (q.opensAccount) console.log(`  opening burn  ${q.display.creationFee} (${to} has no account yet)`);
console.log(`  total         ${q.display.total}; ${q.enough ? `${q.display.transferableAfter} left to send` : "MORE THAN THIS ACCOUNT CAN SEND"}`);
if (!q.enough) process.exit(1);
if (flags["dry-run"]) process.exit(0);

// 2. Sign, and store the signed tx and its id before it leaves this machine. One payment per run:
// build() doesn't take the nonce it signs with, so a service paying several at once from one key
// uses wallet.send(to, amount, { onSigned }) instead, which stores each one in the wallet's queue.
let tx;
try {
  tx = await wallet.build({ type: "transfer", to: q.recipient.signed, amount: q.amount.toString() }, memo);
} catch (e) {
  if (e instanceof MainnetNotAllowedError) fail(`${e.message}\n(pass --network mainnet to pay on mainnet)`);
  throw e;
}
const id = txId(tx);
appendFileSync(outbox, JSON.stringify({ id, tx, at: new Date().toISOString() }) + "\n");

// 3. Submit and wait.
await settle(tx, id);

async function settle(tx, id, { rebroadcast = false } = {}) {
  try {
    await client.post(tx);
  } catch (e) {
    // A refusal (4xx) means this node didn't admit it. Anything else (a timeout, a 5xx) is unclear.
    if (e instanceof ApiError && e.status >= 400 && e.status < 500 && e.status !== 408) fail(await refusal(tx, id, e.message, rebroadcast));
    fail(`unclear (${e.message}). Tx ${id} may have arrived: rebroadcast it, never re-sign:\n  node examples/pay-from-a-script.mjs --rebroadcast ${id}`);
  }
  try {
    const done = await client.waitForTx(id, 30_000, tx.valid_until);
    console.log(`✓ ${done.summary}`);
    console.log(`  tx ${id}, block #${done.height}, fee charged ${done.fee_display}`);
    console.log(`  explorer: ${client.rpc}/#/tx/${id}`);
  } catch (e) {
    if (e instanceof TxFailedError && !e.dropped) fail(`failed in a block: ${e.reason}. Its fee was charged; nothing else moved.`);
    if (e instanceof TxFailedError || e instanceof TxPendingError) {
      fail(`${e.message}\nIt can still run until block ${tx.valid_until}. Check it, or rebroadcast the same bytes:\n  node examples/pay-from-a-script.mjs --rebroadcast ${id}`);
    }
    throw e;
  }
}

/**
 * What a refusal means for this payment. A node that no longer holds a tx that ran (it pruned the
 * block, or synced past it) refuses it again as "bad nonce", so the account's nonce decides: once
 * used, the tx can never run again; while unused, it can until its valid_until.
 */
async function refusal(tx, id, why, rebroadcast) {
  let used, height;
  try {
    const [account, status] = await Promise.all([client.account(tx.from), client.status()]);
    if (!Number.isSafeInteger(account?.nonce) || !Number.isSafeInteger(status?.height)) throw new Error("no nonce or height in its answer");
    used = account.nonce > tx.nonce;
    height = status.height;
  } catch (e) {
    return `refused: ${why}. ${tx.from}'s nonce couldn't be read (${e.message}), so whether tx ${id} ran is unknown. Don't sign this payment again until you know: rebroadcast it later, or look it up.`;
  }
  if (used && rebroadcast) {
    return `refused: ${why}. But nonce ${tx.nonce} of ${tx.from} has been used: tx ${id} ran, or another tx took its nonce. Either way it can never run again. Don't sign this payment again until you know which: look the tx up on a node that keeps every block (/api/tx/${id}).`;
  }
  if (used) return `refused: ${why}. Another tx from ${tx.from} has used nonce ${tx.nonce}, so tx ${id} can never run. Nothing was paid: run this again to pay with the next nonce.`;
  if (!rebroadcast) return `refused: ${why}. Nothing was sent; tx ${id} can't run from this node, and the next payment takes its nonce.`;
  if (height > tx.valid_until) {
    return `refused: ${why}. Nonce ${tx.nonce} is still unused and the chain is past block ${tx.valid_until}, its valid_until (by this node's account), so tx ${id} never ran and never will. You can pay again.`;
  }
  return `refused: ${why}. Nonce ${tx.nonce} is still unused, so tx ${id} hasn't run, and this node won't run it. A node that took it earlier still could, until block ${tx.valid_until}: pay again only after that, if the nonce is still unused.`;
}

function readOutbox() {
  if (!existsSync(outbox)) return [];
  return readFileSync(outbox, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

function parseArgs(argv) {
  const positional = [];
  const flags = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--faucet" || a === "--dry-run") flags[a.slice(2)] = true;
    else if (a.startsWith("--")) flags[a.slice(2)] = argv[++i];
    else positional.push(a);
  }
  return { positional, flags };
}
