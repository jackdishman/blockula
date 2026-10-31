// End to end against a running `blockula dev --programs` (or BLOCKULA_RPC on any chain with programs
// on): the SDK uploads, creates, calls and views a real guest (the counter, node/vm/tests/programs),
// reads it back checked and proven, reconciles the signer's balance from each receipt by §8.1's exact
// identity, and refuses before signing what the chain would refuse or what a lying node shows.
// `npm run test:programs -w @blockula/sdk` runs it (CI does, against `blockula dev --programs`, with
// BLOCKULA_E2E_PROGRAMS=1); on a chain with programs off it says so and passes unless that variable is
// set, which makes it a failure.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { bytesToHex } from "@noble/hashes/utils.js";
import { bech32m } from "@scure/base";
import type { AddressInfo } from "node:net";
import { ApiError, Client, FuelReservationError, ProgramAnswerError, ProgramFailedError, Wallet, baseFee, chargedFee, checkFuel, parseFang, decodeProgram, formatFang, fuelFee, interfaceHash, methodSelector, programAddress, reconcileSigner, txId, versionDigest, wasmHash, type Tx } from "../src/index.js";

const RPC = process.env.BLOCKULA_RPC ?? "http://localhost:7070";
const client = new Client(RPC, { network: "local" });
await client.status();

const off = (await client.fees()).fuel === undefined;
if (off) {
  if (process.env.BLOCKULA_E2E_PROGRAMS === "1") assert.fail(`programs are off at ${RPC}: start it with \`blockula dev --programs\``);
  console.log(`- programs are off at ${RPC} (start \`blockula dev --programs\` to run the programs e2e): skipped`);
  process.exit(0);
}

const guest = (file: string) => new Uint8Array(readFileSync(new URL(`../../node/vm/tests/programs/${file}`, import.meta.url)));
const wasm = guest("counter.wasm");
const iface = guest("counter.iface");
const [wasm_hash, interface_hash] = [wasmHash(wasm), interfaceHash(iface)];

/** A node in front of `RPC` that answers as it does except where `rewrite` says otherwise: a node that lies (or prices differently). */
async function front(rewrite: (path: string, json: any) => any): Promise<{ url: string; close: () => void }> {
  const server: Server = createServer(async (req, res) => {
    const chunks: Buffer[] = [];
    for await (const c of req) chunks.push(c as Buffer);
    const body = Buffer.concat(chunks);
    const up = await fetch(RPC.replace(/\/$/, "") + req.url, { method: req.method, headers: { "content-type": "application/json" }, body: req.method === "GET" || req.method === "HEAD" ? undefined : body });
    const text = await up.text();
    let out = text;
    try {
      out = JSON.stringify(rewrite((req.url ?? "").replace(/^\/api/, "").split("?")[0], JSON.parse(text)));
    } catch {
      // Not JSON: passed on as it came.
    }
    res.writeHead(up.status, { "content-type": "application/json" }).end(out);
  });
  await new Promise<void>((ok) => server.listen(0, "127.0.0.1", ok));
  return { url: `http://127.0.0.1:${(server.address() as AddressInfo).port}`, close: () => server.close() };
}

const alice = Wallet.generate(client);
await alice.faucet();
const nonceOf = async (w: Wallet) => (await client.account(w.address)).next_nonce as number;
const balance = async (w: Wallet) => (await w.balances()).balance;
console.log("alice:", alice.address, (await alice.balances()).display.transferable, "transferable");

// ---- upload: the code and interface are stored, checked to hash to what was asked for, and the balance follows the receipt ----
// (A chain this guest was uploaded to before has it stored already: then there's no deposit to bond.)
const storedBefore = await client.code(wasm_hash).then(() => true, () => false);
let nonce = await nonceOf(alice);
let before = await balance(alice);
const up = await alice.uploadCode(wasm, iface);
let after = await balance(alice);
assert.equal(up.wasmHash, wasm_hash);
assert.equal(up.interfaceHash, interface_hash);
assert.equal(up.receipt.kind, "upload");
assert.equal(up.receipt.facts.upload?.new_code, !storedBefore);
assert.equal(up.deposit > 0n, !storedBefore);
assert.equal(up.receipt.facts.outside_vm.upload_deposit, up.deposit, "the deposit is outside the VM's diff");
assert.deepEqual(up.receipt.facts.balance_changes, [], "an upload moves nothing in the VM");
let r = reconcileSigner(up.receipt, alice.address, before, after);
assert.ok(r.exact && r.conserved, `upload: the balance moved ${r.actual}, the receipt says ${r.expected}`);
assert.equal(before - after, up.cost.fee + up.deposit, "the whole cost an upload debits: its fee and its deposit");
assert.equal(up.cost.total, up.cost.fee + up.cost.outside_vm.total);
assert.equal(up.receipt.facts.burned_by_programs, 0n, "an upload runs no program: nothing for programs to burn");
assert.ok(up.receipt.facts.fee.fuel > 0n, "an upload pays for the fuel it reserves");
assert.equal(await nonceOf(alice), nonce + 1);
const stored = await client.code(wasm_hash, { wasm: true });
assert.deepEqual(stored.wasm, wasm);
assert.equal(stored.size, wasm.length);
assert.deepEqual(stored.untrusted_exports, ["get", "increment"]);
const storedInterface = await client.interface(interface_hash);
assert.deepEqual(storedInterface.bytes, iface);
// The interface's JSON reading is the node's, with the deployer's names marked.
assert.deepEqual((storedInterface.untrusted_interface as { methods: { name: string }[] }).methods.map((m) => m.name), ["get", "increment"]);
console.log(`✓ upload: ${up.cost.fee} drops of fee and ${formatFang(up.deposit)} of deposit left alice's balance, as the receipt says`);

// Uploading the same bytes again only refreshes them: no new deposit.
const again = await alice.uploadCode(wasm, iface);
assert.equal(again.deposit, 0n);
assert.equal(again.receipt.facts.upload?.new_code, false);
console.log("✓ the same upload again bonds nothing");

// ---- create: the address is the sender's and its nonce's, returned once the create has applied ----
nonce = await nonceOf(alice);
before = await balance(alice);
const created = await alice.createProgram({ wasmHash: wasm_hash, interfaceHash: interface_hash });
after = await balance(alice);
const address = created.address;
assert.equal(address, programAddress(alice.address, nonce, await client.addressPrefix()));
assert.equal(created.receipt.facts.created, address);
assert.equal(created.tx.nonce, nonce);
const fee = BigInt((await client.fees()).account_creation_fee);
assert.equal(created.receipt.facts.outside_vm.account_creation_fee, fee, "a create burns the creation fee of the program's account");
assert.ok(created.receipt.facts.outside_vm.record_deposit > 0n);
r = reconcileSigner(created.receipt, alice.address, before, after);
assert.ok(r.exact && r.conserved, `create: the balance moved ${r.actual}, the receipt says ${r.expected}`);
assert.equal(before - after, created.cost.fee + created.cost.outside_vm.record_deposit + created.cost.outside_vm.account_creation_fee);
console.log(`✓ create: ${address} (${formatFang(created.cost.total)} in all, ${formatFang(fee)} of it burned to open its account)`);

// ---- reads: checked, and proven against a witness ----
const program = await client.program(address);
assert.equal(program.version_digest, versionDigest(address, wasm_hash, interface_hash, 1), "recomputed, not taken on trust");
assert.deepEqual(program.methods?.map((m) => [m.untrusted_name, m.kind, m.selector]), [["get", "view", methodSelector("get")], ["increment", "call", methodSelector("increment")]]);
assert.equal(program.proven, false);
assert.equal(program.authority, null);
assert.equal(program.upgradability, "immutable");
const witness = new Client(client.rpc);
const proven = await client.provenProgram(address, { witnesses: [witness] });
assert.ok(proven, "the program's record is proven");
assert.equal(proven.version_digest, program.version_digest);
assert.equal(proven.wasm_hash, wasm_hash);
assert.equal(proven.interface_hash, interface_hash);
assert.equal(proven.creator, alice.address);
assert.equal(proven.created_at, program.created_at);
assert.equal(proven.record_deposit, program.record_deposit);
assert.equal(proven.witnesses, 1);
assert.equal(await client.provenProgram(programAddress(alice.address, 99_999), { witnesses: [witness] }), null, "a program that isn't there is proven absent");
const raw = await client.proof(66, address);
assert.equal(decodeProgram(Uint8Array.from(Buffer.from(raw.value as string, "hex"))).wasm_hash, wasm_hash);
await assert.rejects(client.program("alice.boo"), /never by a \.boo name/);
await assert.rejects(client.program(programAddress(alice.address, 99_998)), (e) => e instanceof ApiError && e.status === 404);
console.log(`✓ the record reads back checked (digest ${program.version_digest.slice(0, 12)}…) and proven (block #${proven.height})`);

// ---- view: a read, nothing signed ----
const v0 = await client.view(address, "get");
assert.deepEqual([v0.ok, v0.return_data, v0.outcome.ok], [true, "0000000000000000", true]);
assert.ok(v0.fuel_used > 0 && v0.height > 0);
await assert.rejects(client.view(address, "increment"), (e) => e instanceof ApiError && e.status === 400 && /which only a transaction runs/.test(e.message));
await assert.rejects(client.view(address, "Get"), /method name/);
await assert.rejects(client.view("alice.boo", "get"), /never by a \.boo name/);
await assert.rejects(client.view(address, "get", "0g"), /lowercase hex/);
console.log("✓ view: get → 0 (the program's claim); a call method, a bad name and a name for an address are refused");

// ---- a call that can't bond storage traps, pays its fee and says why ----
nonce = await nonceOf(alice);
before = await balance(alice);
let signed: Tx | undefined;
const failed = await alice.call(address, "increment", "", { onSigned: (tx) => void (signed = tx) }).then(
  () => assert.fail("the call should trap storage_limit"),
  (e) => e,
);
after = await balance(alice);
assert.ok(failed instanceof ProgramFailedError, String(failed));
assert.equal(failed.receipt.outcome.code, "trapped");
assert.equal(failed.receipt.outcome.trap?.name, "storage_limit");
assert.match(failed.message, /storage limit \(0 unless given\) was too small|storage limit \(0 drops\) was too small/);
assert.match(failed.message, /it ran in block|failed in block/);
r = reconcileSigner(failed.receipt, alice.address, before, after);
assert.ok(r.exact && r.conserved, `failed call: the balance moved ${r.actual}, the receipt says ${r.expected}`);
assert.equal(before - after, failed.receipt.facts.fee.bytes + failed.receipt.facts.fee.fuel + failed.receipt.facts.fee.tip, "a failed call pays its fee, whole reservation included, and nothing else");
assert.ok(failed.receipt.facts.fee.fuel > 0n);
assert.equal(failed.receipt.facts.fuel.reserved, failed.receipt.facts.fuel.limit + failed.receipt.facts.fuel.load);
assert.equal(await nonceOf(alice), nonce + 1);
console.log(`✓ a call with no storage limit trapped storage_limit and was charged ${failed.receipt.facts.fee.bytes + failed.receipt.facts.fee.fuel} drops: ${failed.words}`);

// ---- a call that writes, with a storage limit: the bond is the signer's, the fee prices the fuel ----
const fees = await client.fees();
const fuel = checkFuel(client.rpc, fees.fuel);
nonce = await nonceOf(alice);
before = await balance(alice);
const inc = await alice.call(address, "increment", "", { storageLimit: "1", onSigned: (tx) => void (signed = tx) });
after = await balance(alice);
assert.equal(inc.receipt.kind, "call");
assert.deepEqual(inc.receipt.claims.return_data.map((d) => [d.untrusted_method, d.data]), [["increment", "0100000000000000"]]);
assert.ok(inc.receipt.facts.storage.bonded > 0n);
assert.equal(inc.receipt.facts.balance_changes.length, 1);
assert.equal(inc.receipt.facts.balance_changes[0].account, alice.address, "the signer's own write bond is in the VM's diff");
assert.equal(inc.receipt.facts.balance_changes[0].change, -inc.receipt.facts.storage.bonded);
r = reconcileSigner(inc.receipt, alice.address, before, after);
assert.ok(r.exact && r.conserved, `call: the balance moved ${r.actual}, the receipt says ${r.expected}`);
assert.equal(inc.cost.total, inc.cost.fee + inc.receipt.facts.storage.bonded);
// The fee it signed covers the bytes' base fee and the fuel it reserved at the suggested rate.
const tx = signed as Tx;
assert.equal(tx.nonce, nonce);
const reserved = inc.receipt.facts.fuel.reserved;
const priced = baseFee(tx, fees.suggested) + fuelFee(fuel.suggested_rate, reserved);
assert.equal(BigInt(tx.fee), priced, "fee = base fee + suggested rate × reserved fuel / 1000");
assert.ok(BigInt(tx.fee) > baseFee(tx, fees.suggested), "the fee includes fuel");
assert.ok(inc.receipt.facts.fee.fuel <= fuelFee(fuel.suggested_rate, reserved), "charged no more than the suggested rate's");
assert.equal(txId(tx), inc.tx.id);
assert.deepEqual((await client.view(address, "get")).return_data, "0100000000000000");
console.log(`✓ a call that writes: return data ${inc.receipt.claims.return_data[0].data} (the program's claim); fee ${tx.fee} prices ${reserved} reserved fuel at ${fuel.suggested_rate}`);

// ---- refused before anything is signed ----
nonce = await nonceOf(alice);
const refused = async (what: string, run: () => Promise<unknown>, match: RegExp | ((e: unknown) => boolean)) => {
  await assert.rejects(run(), typeof match === "function" ? match : (e) => match.test((e as Error).message), what);
  assert.equal(await nonceOf(alice), nonce, `${what}: nothing was submitted`);
};
await refused("fuel above program_max_tx_fuel", () => alice.call(address, "increment", "", { fuelLimit: Number(fuel.max_tx_fuel) + 1 }), /above program_max_tx_fuel/);
await refused("a view is no call", () => alice.call(address, "get"), /is a view: read it with client\.view/);
await refused("no such method", () => alice.call(address, "nothing"), /has no method "nothing"/);
await refused("a method that isn't payable takes no FANG", () => alice.call(address, "increment", "", { value: "1" }), /isn't payable/);
await refused("a digest other than the one shown", () => alice.call(address, "increment", "", { versionDigest: "0".repeat(63) + "1" }), /doesn't read the way it did when it was shown/);
await refused("a name for a program", () => alice.call("counter.boo", "increment"), /never by a \.boo name/);
await refused("a binding twice", () => alice.call(address, "increment", "", { bindings: [{ program: address }, { program: address }] }), /bound twice/);
// (Code stored already has no deposit to fall short of: on a chain this guest was uploaded to before, there's nothing to refuse.)
if (!(await client.code(wasmHash(guest("token.wasm"))).then(() => true, () => false))) await refused("a storage limit below the upload's deposit", () => alice.uploadCode(guest("token.wasm"), guest("token.iface"), { storageLimit: "0.000001" }), /below the deposit/);
await refused("a create that names nothing stored", () => alice.createProgram({ wasmHash: "ab".repeat(32), interfaceHash: interface_hash, waitMs: 1_500 }), /the code isn't usable after 1500 ms/);
await refused("a create's delay without an authority", () => alice.createProgram({ wasmHash: wasm_hash, interfaceHash: interface_hash, delayBlocks: 5 }), /an upgrade authority's/);
await refused("an initializer the interface doesn't have", () => alice.createProgram({ wasmHash: wasm_hash, interfaceHash: interface_hash, init: { method: "init" } }), /declares no initializer/);
console.log("✓ refused before signing: too much fuel, a view, an unknown or unpayable method, another version, a name, a double binding, a low storage limit, a create with nothing stored, an upgrade delay with no authority");

// ---- a node that prices a block small: a reservation no block could carry is refused before signing ----
const tight = await front((path, json) => (path === "/fees" && json.fuel ? { ...json, fuel: { ...json.fuel, max_tx_fuel: 20_000_000, max_block_fuel: 20_000_000 } } : json));
try {
  const lowBlock = Wallet.fromSeed(alice.seedHex, new Client(tight.url));
  // A limit of max_tx_fuel (20M here) and the code's load on top is past what one block counts. (The default limit leaves room for the
  // load, min(max_tx_fuel, max_block_fuel − load), so it's an explicit limit that is refused: the unit tests cover the default.)
  await assert.rejects(lowBlock.call(address, "increment", "", { storageLimit: "1", fuelLimit: 20_000_000 }), (e) => e instanceof FuelReservationError && e.limit === 20_000_000n && e.run === 20_000_000n && e.load > 0n && e.reserved === e.run + e.load);
  assert.equal(await nonceOf(alice), nonce, "nothing was signed");
  // A limit that fits goes through, priced at the rate this node reports.
  const fits = await lowBlock.call(address, "increment", "", { storageLimit: "1", fuelLimit: 1_000_000 });
  assert.equal(fits.receipt.facts.fuel.limit, 1_000_000n);
  nonce += 1;
} finally {
  tight.close();
}
console.log("✓ a reservation above the block's fuel is refused before signing (a node pricing blocks at 20M fuel)");

// ---- a node that shows a digest it shouldn't: refused, nothing signed ----
for (const [what, rewrite, match] of [
  ["an uppercased digest", (j: any) => ({ ...j, version_digest: String(j.version_digest).toUpperCase() }), /isn't 64 lowercase hex/],
  ["a short digest", (j: any) => ({ ...j, version_digest: String(j.version_digest).slice(0, 40) }), /isn't 64 lowercase hex/],
  ["another program's digest", (j: any) => ({ ...j, version_digest: "ab".repeat(32) }), /isn't what its code, interface and version compute/],
  ["a different code hash", (j: any) => ({ ...j, code: { ...j.code, wasm_hash: "cd".repeat(32) } }), /isn't what its code, interface and version compute/],
] as const) {
  const liar = await front((path, json) => (path === `/program/${address}` ? rewrite(json) : json));
  try {
    const victim = Wallet.fromSeed(alice.seedHex, new Client(liar.url));
    await assert.rejects(victim.call(address, "increment", "", { storageLimit: "1" }), (e) => e instanceof ProgramAnswerError && match.test(e.message), what);
    await assert.rejects(new Client(liar.url).program(address), ProgramAnswerError, what);
    assert.equal(await nonceOf(alice), nonce, `${what}: nothing was signed`);
  } finally {
    liar.close();
  }
}
console.log("✓ a node that shows a malformed or inconsistent version digest gets nothing signed on its word");

// ---- an initializer, token movements and a payable program, each reconciled from its receipt ----
const addressBytes = (a: string) => bytesToHex(bech32m.fromWords(bech32m.decode(a as `${string}1${string}`).words));
const u64le = (n: bigint) => bytesToHex(new Uint8Array(new BigUint64Array([n]).buffer));
const bob = Wallet.generate(client);
await bob.faucet();
{
  const tokenUp = await alice.uploadCode(guest("token.wasm"), guest("token.iface"));
  // The token's interface declares an initializer, so a create must run it (and says so before signing when it doesn't).
  await assert.rejects(alice.createProgram({ wasmHash: tokenUp.wasmHash, interfaceHash: tokenUp.interfaceHash }), /declares an initializer, so the create must run it/);
  before = await balance(alice);
  const token = await alice.createProgram({ wasmHash: tokenUp.wasmHash, interfaceHash: tokenUp.interfaceHash, init: { method: "init", input: addressBytes(alice.address) + u64le(1000n) }, storageLimit: "1" });
  after = await balance(alice);
  r = reconcileSigner(token.receipt, alice.address, before, after);
  assert.ok(r.exact && r.conserved, `create with init: the balance moved ${r.actual}, the receipt says ${r.expected}`);
  assert.equal(token.receipt.facts.fuel.limit, fuel.max_tx_fuel, "an initializer runs under program_max_tx_fuel by default");
  assert.ok(token.receipt.facts.fuel.load > 0n, "and loads its code");
  assert.ok(token.receipt.facts.storage.bonded > token.receipt.facts.outside_vm.record_deposit, "its writes bonded more than the record");
  assert.equal(token.cost.vm, token.receipt.facts.storage.bonded - token.receipt.facts.outside_vm.record_deposit, "the signer's write bonds are in the VM's diff, the record's deposit outside it");
  assert.equal((await client.view(token.address, "balance_of", addressBytes(alice.address))).return_data, u64le(1000n), "the initializer minted the supply");
  const sent = await alice.call(token.address, "transfer", addressBytes(bob.address) + u64le(250n), { storageLimit: "1" });
  assert.equal((await client.view(token.address, "balance_of", addressBytes(bob.address))).return_data, u64le(250n));
  assert.ok(sent.receipt.facts.storage.bonded > 0n, "a first balance is a new entry: it bonds a deposit");
  console.log(`✓ create with init: the supply is minted by the program's own initializer (its claim, read back by a view); ${formatFang(token.cost.total)} in all`);
}
{
  const escrowUp = await alice.uploadCode(guest("escrow.wasm"), guest("escrow.iface"));
  const escrow = await alice.createProgram({ wasmHash: escrowUp.wasmHash, interfaceHash: escrowUp.interfaceHash });
  const seller = Wallet.generate(client).address;
  before = await balance(alice);
  const open = await alice.call(escrow.address, "open", u64le(1n) + addressBytes(seller), { value: "1", storageLimit: "1" });
  after = await balance(alice);
  r = reconcileSigner(open.receipt, alice.address, before, after);
  assert.ok(r.exact && r.conserved, `a payable call: the balance moved ${r.actual}, the receipt says ${r.expected}`);
  assert.equal(open.receipt.facts.balance_changes.find((c) => c.account === escrow.address)?.change, parseFang("1"), "the program holds the FANG sent with the call");
  assert.equal(open.receipt.claims.events[0].untrusted_name, "open");
  assert.equal((await client.program(escrow.address)).balance, parseFang("1").toString());
  await assert.rejects(alice.call(escrow.address, "release", u64le(1n), { value: "1" }), /isn't payable/);
  before = await balance(alice);
  const paid = await alice.call(escrow.address, "release", u64le(1n));
  after = await balance(alice);
  r = reconcileSigner(paid.receipt, alice.address, before, after);
  assert.ok(r.exact && r.conserved, `a payout: the balance moved ${r.actual}, the receipt says ${r.expected}`);
  const payout = paid.receipt.facts.balance_changes.find((c) => c.account === seller)?.change;
  assert.equal(payout, parseFang("1") - fee, "a payout to an account that doesn't exist opens it: its creation fee comes out of the amount");
  assert.equal(paid.receipt.facts.burned_by_programs, fee, "...and the receipt says the program's send burned it: conservation holds with it");
  assert.equal(open.receipt.facts.burned_by_programs, 0n);
  assert.equal(BigInt((await client.account(seller)).balance), payout);
  // The deal is settled: releasing it again aborts with the program's own message, and the fee is all it costs.
  before = await balance(alice);
  const again = await alice.call(escrow.address, "release", u64le(1n)).then(() => assert.fail("it should abort"), (e) => e);
  after = await balance(alice);
  assert.ok(again instanceof ProgramFailedError && again.receipt.outcome.code === "aborted" && again.receipt.outcome.untrusted_message === "no deal", String(again));
  assert.match(again.message, /aborted with code 22/);
  assert.ok(!again.message.includes("no deal"), "the program's message isn't in the error's message");
  assert.equal(again.untrusted_message, "no deal", "it's on the error, as the program's claim");
  assert.equal(before - after, chargedFee(again.receipt));
  console.log(`✓ payable call and payout: ${formatFang(parseFang("1") - fee)} reached a new account through the program, and every balance followed its receipt`);
}

// ---- a validator's pause is for validators: a stranger's is refused by the chain, not signed into a halt ----
await assert.rejects(alice.programsPause(), (e) => e instanceof ApiError && e.status >= 400 && e.status < 500);
console.log("✓ programsPause from a non-validator is refused by the node");

const status = await client.status();
console.log(`✓ programs e2e done at height ${status.height}`);
