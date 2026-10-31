// Program vectors for the explorer's ledger (web/lib/program.test.mjs): real program transactions on a
// `blockula dev --programs` chain, what the node said about each once it ran (/api/tx, the receipt
// included), and what each did to the balances of the accounts it touched. The test holds the
// explorer's `effectsOf` to those balances, exactly, and its receipt reading to the receipts. The v1
// vectors (scripts/explorer-vectors.mjs, web/lib/v1-vectors.json) are a separate file and untouched:
// that script makes random wallets and names every run, so regenerating it would change every line.
//
//   blockula dev --programs --reset --port 7340 &
//   npm run build -w @blockula/sdk
//   node scripts/explorer-program-vectors.mjs http://localhost:7340 > web/lib/program-vectors.json
import { readFileSync } from "node:fs";
import { bytesToHex } from "@noble/hashes/utils.js";
import { bech32m } from "@scure/base";
import { Client, ProgramFailedError, Wallet } from "@blockula/sdk";

const rpc = process.argv[2] || "http://localhost:7070";
const client = new Client(rpc);
const status = await client.status();
if (!status.faucet) throw new Error(`${rpc} has no faucet; run this against a dev chain (blockula dev --programs)`);
if ((await client.fees()).fuel === undefined) throw new Error(`${rpc} has programs off: start it with \`blockula dev --programs\``);
await client.addressPrefix();

const guest = (file) => new Uint8Array(readFileSync(new URL(`../node/vm/tests/programs/${file}`, import.meta.url)));
const burned = async () => (await client.status()).burned;
const balance = async (a) => (await client.account(a).then((x) => BigInt(x.balance), () => 0n)).toString();
const log = (...a) => console.error(...a);
const addressBytes = (a) => bytesToHex(bech32m.fromWords(bech32m.decode(a).words));
const u64 = (n) => bytesToHex(new Uint8Array(new BigUint64Array([BigInt(n)]).buffer));

const vectors = [];
/** Runs `step` (a program helper of the SDK), keeping the node's answer for its tx and the balances of `watch` around it. */
async function vector(name, signer, watch, step, note) {
  const accounts = [...new Set([signer.address, ...watch])];
  const snapshot = async () => Object.fromEntries(await Promise.all(accounts.map(async (a) => [a, await balance(a)])));
  const before = await snapshot();
  const burnedBefore = await burned();
  let tx;
  try {
    tx = (await step()).tx;
  } catch (e) {
    if (!(e instanceof ProgramFailedError)) throw e;
    tx = e.tx;
  }
  const after = await snapshot();
  const burnedAfter = await burned();
  vectors.push({ name, note, signer: signer.address, accounts, tx, before, after, burned: (BigInt(burnedAfter) - BigInt(burnedBefore)).toString() });
  log(`✓ ${name}: ${tx.status}`);
  return tx;
}

const alice = Wallet.generate(client);
await alice.faucet();
const bob = Wallet.generate(client);
await bob.faucet();
const seller = Wallet.generate(client).address;

// The counter: a view, a call that writes, and what happens without a storage limit.
const counter = { wasm: guest("counter.wasm"), iface: guest("counter.iface") };
let up;
await vector("upload", alice, [], async () => (up = await alice.uploadCode(counter.wasm, counter.iface)));
let made;
await vector("create", alice, [], async () => (made = await alice.createProgram({ wasmHash: up.wasmHash, interfaceHash: up.interfaceHash })), "the program's own account is opened by the create: its creation fee burns from the signer");
const program = made.address;
await vector("a call that traps for want of a storage limit", alice, [program], () => alice.call(program, "increment"), "a failed call pays its fee and nothing else");
await vector("a call that writes", alice, [program], () => alice.call(program, "increment", "", { storageLimit: "1" }), "the signer's write bond is in the VM's diff, and in its balance");
await vector("a call that writes again", alice, [program], () => alice.call(program, "increment", "", { storageLimit: "1" }), "an overwrite bonds nothing more");
await vector("uploading it again", alice, [], () => alice.uploadCode(counter.wasm, counter.iface), "stored already: no deposit");

// The escrow: a payable call, a payout to an account that doesn't exist yet, and an abort.
const escrow = { wasm: guest("escrow.wasm"), iface: guest("escrow.iface") };
let up2;
await vector("upload another program", bob, [], async () => (up2 = await bob.uploadCode(escrow.wasm, escrow.iface)));
let made2;
await vector("create another program", bob, [], async () => (made2 = await bob.createProgram({ wasmHash: up2.wasmHash, interfaceHash: up2.interfaceHash })));
const deal = made2.address;
await vector("a payable call", bob, [deal, seller], () => bob.callProgram([{ program: deal, method: "open", input: u64(1) + addressBytes(seller), value: "1" }], { storageLimit: "1" }), "1 FANG goes to the program through the VM's diff, with the write bond");
await vector("a payout to a new account", bob, [deal, seller], () => bob.callProgram([{ program: deal, method: "release", input: u64(1) }]), "the seller's account is opened by the program: its creation fee comes out of the amount, and the bond comes back");
await vector("a call that aborts with a message", bob, [deal], () => bob.callProgram([{ program: deal, method: "release", input: u64(7) }]), "the program's own words, sanitized, in the receipt's outcome");

process.stdout.write(`{"chain_id": ${JSON.stringify(status.chain_id)}, "made_with": "scripts/explorer-program-vectors.mjs", "vectors": [\n${vectors.map((v) => JSON.stringify(v)).join(",\n")}\n]}\n`);
