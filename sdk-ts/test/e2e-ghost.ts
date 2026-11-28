// Ghost Mode end to end with `@blockula/sdk/ghost` against a real node: this starts
// `blockula dev --ghost` (node/target/release/blockula) in a temporary BLOCKULA_HOME on a port of
// its own, and then
//
//   1. checks the SDK derives the key the CLI does: `blockula ghost address` prints the same
//      fingerprint for the same seed;
//   2. bob registers a name and publishes his Ghost Mode address;
//   3. alice shields 40, 30 and 20 FANG (three notes);
//   4. alice sends 80 FANG privately to bob's name: refused without his fingerprint and with a
//      wrong one, then sent with it (three notes, so one merge first);
//   5. bob's sync finds the note and its memo;
//   6. bob unshields 25.5 FANG to carol, who has no account yet;
//   7. the two wallets' shielded balances, carol's account and `/api/ghost`'s pool add up;
//   8. the note store (a file, mode 600) holds no seed and no note secret, restores, and a wallet
//      that scans from nothing finds the same notes; a node that lists the outputs wrongly is refused.
//
// Every proof is made by the WebAssembly prover and accepted by the node's verifier: that's the
// check. `npm run test:ghost -w @blockula/sdk` runs it (about 30 s). Nothing here touches
// ~/.blockula or any other chain, and no seed, key or note secret is printed.
//
//   BLOCKULA_BIN=…        the node binary (default node/target/release/blockula)
//   GHOST_E2E_PORT=7272   the dev chain's port (never 7070)
import assert from "node:assert/strict";
import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { connect, type AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { Client, Wallet, formatFang, parseFang } from "../src/index.js";
import { GhostWallet, MemoryNoteStore, mergesNeeded, pickNotes, type GhostPlan, type GhostSent } from "../src/ghost.js";
import { FileNoteStore } from "../src/ghost-node.js";

const BIN = process.env.BLOCKULA_BIN ?? fileURLToPath(new URL("../../node/target/release/blockula", import.meta.url));
const PORT = Number(process.env.GHOST_E2E_PORT ?? 7272);
const RPC = `http://127.0.0.1:${PORT}`;
assert.ok(existsSync(BIN), `${BIN} isn't built: cargo build --release --manifest-path node/Cargo.toml`);
assert.ok(Number.isSafeInteger(PORT) && ![7070, 30300, 3000, 4040, 4777, 4780, 5000, 5432, 7000].includes(PORT), `port ${PORT} is left alone: pick another (GHOST_E2E_PORT)`);

const listening = (port: number) =>
  new Promise<boolean>((ok) => {
    const s = connect(port, "127.0.0.1");
    s.once("connect", () => (s.destroy(), ok(true)));
    s.once("error", () => ok(false));
  });
assert.ok(!(await listening(PORT)), `something is already listening on port ${PORT}: pick another (GHOST_E2E_PORT)`);

const dir = mkdtempSync(join(tmpdir(), "blockula-ghost-sdk-"));
let node: ChildProcess | undefined;
const servers: Server[] = [];
const cleanup = () => {
  for (const s of servers) s.close();
  node?.kill();
  rmSync(dir, { recursive: true, force: true });
};
process.on("exit", cleanup);
for (const sig of ["SIGINT", "SIGTERM"] as const) process.on(sig, () => process.exit(1));

mkdirSync(join(dir, "node"));
node = spawn(BIN, ["dev", "--ghost", "--port", String(PORT)], { env: { ...process.env, BLOCKULA_HOME: join(dir, "node") }, stdio: "ignore" });
const client = new Client(RPC, { network: "local" });
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
for (let i = 0; ; i++) {
  const s = await client.status().catch(() => null);
  if (s && s.height >= 2) break;
  assert.ok(node.exitCode === null, "the dev node exited");
  assert.ok(i < 240, "the dev chain made no blocks");
  await sleep(250);
}
const status = await client.status();
assert.equal(status.chain_id, "blockula-dev");
const ghostStatus = async () => client.ghost() as Promise<{ active: boolean; pool: string; frontier_size: number; nullifiers: number }>;
assert.equal((await ghostStatus()).active, true, "Ghost Mode isn't on in the dev chain");
console.log(`✓ blockula dev --ghost at ${RPC}: Ghost Mode on`);

const fang = (drops: bigint) => formatFang(drops);
const rejects = async (run: () => Promise<unknown>, pattern: RegExp, what: string) => {
  await assert.rejects(run, (e: Error) => (assert.match(e.message, pattern, what), true), what);
};
const line = (s: GhostSent) => `   ${s.summary} (tx ${s.id}${s.height === undefined ? "" : `, block #${s.height}`}) · proved in ${s.proveMs} ms · ${s.bytes} bytes · fee ${s.feeDisplay}`;
const proveMs: number[] = [];
const done = (sent: GhostSent | GhostSent[]) => {
  for (const s of [sent].flat()) {
    console.log(line(s));
    proveMs.push(s.proveMs);
    assert.ok(s.height !== undefined && /^[0-9a-f]{64}$/.test(s.id) && s.bytes > 120_000 && s.bytes < 200_000, "a final ghost tx of a proof's size");
  }
};

// ---- the wallets: alice and bob have accounts (the dev faucet), carol has none ----
const alice = Wallet.generate(client);
const bob = Wallet.generate(client);
const carol = Wallet.generate(client);
await alice.faucet();
await bob.faucet();
const aliceStore = new FileNoteStore(join(dir, "alice-notes"));
const ghostA = await GhostWallet.fromWallet(alice, { store: aliceStore });
const ghostB = await GhostWallet.fromWallet(bob);
console.log(`alice ${alice.address}\nbob   ${bob.address} (ghost address fingerprint ${ghostB.fingerprint})\ncarol ${carol.address} (no account yet)`);
assert.match(ghostA.fingerprint, /^([0-9a-f]{4}-){4}[0-9a-f]{4}$/);
assert.equal(ghostA.address.length, 1216);
assert.notEqual(ghostA.fingerprint, ghostB.fingerprint);

// ---- 1. the CLI derives the same key from the same seed ----
{
  // The seed goes to a mode-600 file the CLI reads (`--key`), in the temporary directory; it's never printed.
  const keyFile = join(dir, "alice.key");
  writeFileSync(keyFile, alice.seedHex, { mode: 0o600 });
  mkdirSync(join(dir, "cli"));
  const out = spawnSync(BIN, ["ghost", "address", "--key", keyFile, "--rpc", RPC], { env: { ...process.env, BLOCKULA_HOME: join(dir, "cli") }, encoding: "utf8" });
  rmSync(keyFile);
  assert.equal(out.status, 0, out.stderr);
  const printed = /fingerprint: ((?:[0-9a-f]{4}-){4}[0-9a-f]{4})/.exec(out.stdout)?.[1];
  assert.equal(printed, ghostA.fingerprint, "the SDK and `blockula ghost address` derive the same Ghost Mode address from one seed");
  assert.ok(!out.stdout.includes(alice.seedHex));
  console.log(`✓ \`blockula ghost address\` prints the SDK's fingerprint for the same seed (${printed})`);
}

// A passkey's ghost key has a context of its own: not the account seed's key, and the same every time.
{
  const prf = crypto.getRandomValues(new Uint8Array(32));
  const [viaPrf, again, viaSeed] = [await GhostWallet.fromPasskeyPrf(prf, { client }), await GhostWallet.fromPasskeyPrf(prf, { client }), await GhostWallet.fromWallet(Wallet.fromPasskeyPrf(prf, client))];
  assert.equal(viaPrf.fingerprint, again.fingerprint);
  assert.notEqual(viaPrf.fingerprint, viaSeed.fingerprint);
  assert.equal(viaPrf.wallet?.address, viaSeed.wallet?.address, "the same passkey account signs its shields");
  for (const w of [viaPrf, again, viaSeed]) w.close();
  assert.throws(() => viaPrf.fingerprint, /closed/);
}

// The CLI's coin selection, offline.
assert.equal(mergesNeeded([40n, 30n, 20n], 80n, 1n), 1);
assert.equal(mergesNeeded([40n, 30n], 70n, 1n), 0);
assert.equal(mergesNeeded([40n, 30n, 20n], 91n, 1n), null);
assert.equal(mergesNeeded([], 1n, 1n), null);
assert.equal(mergesNeeded([5n, 5n, 1n], 11n, 1n), null, "a note worth no more than the fee isn't merged in");
assert.deepEqual(pickNotes([40n, 30n, 20n], 25n), [1], "the smallest single note that covers it");
assert.deepEqual(pickNotes([40n, 30n, 20n], 65n), [0, 1]);
assert.equal(pickNotes([40n, 30n, 20n], 80n), null);

// ---- 2. bob registers a name and publishes his address ----
const name = `ghostbob${Math.floor(Math.random() * 9000) + 1000}.boo`;
await bob.registerName(name);
assert.equal(await ghostA.published(name), null, "nothing published yet");
await rejects(() => ghostA.send(name, "1", { fingerprint: ghostB.fingerprint }), /hasn't published a Ghost Mode address/, "no address to pay");
const published = await ghostB.publishAddress();
assert.equal(published.status, "finalized");
const found = await ghostA.published(name);
assert.deepEqual(found, { recipient: name, address: ghostB.addressHex, fingerprint: ghostB.fingerprint });
console.log(`✓ ${name} published its Ghost Mode address (${published.summary})`);

// ---- 3. alice shields three notes ----
const pool0 = await ghostA.pool();
assert.ok(pool0.active && !pool0.paused && pool0.fee > 0n && pool0.feeNewAccount > pool0.fee && pool0.size === 0);
const publicBefore = BigInt((await alice.balances()).balance);
let shieldFees = 0n;
for (const amount of ["40", "30", "20"]) {
  const s = await ghostA.shield(amount);
  assert.equal(s.kind, "shield");
  shieldFees += s.fee;
  done(s);
}
let a = await ghostA.balance();
assert.equal(a.balance, parseFang("90"));
assert.deepEqual(a.notes.map((n) => n.display).sort(), ["20 FANG", "30 FANG", "40 FANG"]);
assert.equal(BigInt((await ghostStatus()).pool), parseFang("90"));
// The account paid the amounts, each tx's fee and the chain's verify fee, and nothing else.
assert.equal(publicBefore - BigInt((await alice.balances()).balance), parseFang("90") + shieldFees + 3n * pool0.verifyFee);
assert.equal((await ghostB.sync()).found, 0, "nothing of alice's is bob's");
console.log(`✓ alice shielded 90 FANG in three notes (pool ${fang(BigInt((await ghostStatus()).pool))})`);

// ---- 4. the private send: the fingerprint rule, then a merge and the payment ----
const before = await ghostStatus();
await rejects(() => ghostA.send(name, "80", { memo: "for the garlic" }), /needs the recipient's fingerprint/, "no fingerprint, no confirm");
await rejects(() => ghostA.send(name, "80", { fingerprint: ghostA.fingerprint }), /has the fingerprint .* not /, "another fingerprint");
await rejects(() => ghostA.send(name, "80", { confirm: () => false }), /wasn't confirmed/, "a person said no");
await rejects(() => ghostA.send(name, "80", { fingerprint: ghostB.fingerprint, memo: "x".repeat(257) }), /at most 256/, "a memo too long");
await rejects(() => ghostA.send(name, "90", { fingerprint: ghostB.fingerprint }), /This wallet has 90 FANG shielded in 3 notes, and this takes/, "more than it holds with the fee");
await rejects(() => ghostA.send(name, "80", { fingerprint: ghostB.fingerprint, maxFee: 1n }), /above the most this pays/, "the fee cap");
assert.deepEqual(await ghostStatus(), before, "a refused send sends nothing");
let plan: GhostPlan | undefined;
await rejects(() => ghostA.send(name, "80", { fingerprint: ghostB.fingerprint, onPlan: () => { throw new Error("stopped by the plan"); } }), /stopped by the plan/, "onPlan can stop it");
assert.deepEqual(await ghostStatus(), before, "a plan that's refused sends nothing");
const proven: string[] = [];
const sent = await ghostA.send(name, "80", { memo: "for the garlic", fingerprint: ghostB.fingerprint.toUpperCase(), onPlan: (p) => void (plan = p), onProven: (p) => void proven.push(`${p.kind}:${p.id}`) });
assert.deepEqual(sent.map((s) => s.kind), ["merge", "send"]);
assert.deepEqual(plan, { kind: "send", amount: parseFang("80"), merges: 1, mergeFee: pool0.fee, fee: pool0.fee, totalFees: 2n * pool0.fee });
assert.deepEqual(proven, sent.map((s) => `${s.kind}:${s.id}`), "each tx was handed over before it was posted");
done(sent);
// An unsigned tx: nobody's account, and its summary says nothing.
const onChain = await client.tx(sent[1].id);
assert.equal(onChain.status, "finalized");
assert.ok(!JSON.stringify(onChain).includes(alice.address) && !JSON.stringify(onChain).includes(bob.address), "a private transfer names no account");
console.log(`✓ alice sent 80 FANG privately to ${name}: one merge, then the payment`);

// ---- 5. bob finds it ----
let b = await ghostB.sync();
assert.equal(b.found, 1);
assert.equal(b.balance, parseFang("80"));
assert.equal(b.notes[0].memo, "for the garlic");
a = await ghostA.balance();
assert.equal(a.balance, parseFang("10") - 2n * pool0.fee, "alice: 90 − 80 − two fees");
console.log(`✓ bob's sync found ${b.display} with its memo; alice has ${a.display}`);

// ---- 6. bob unshields to carol, opening her account ----
assert.equal((await client.account(carol.address)).exists, false);
await rejects(() => ghostB.unshield(carol.address, "0.001"), /no account yet, and opening one takes at least/, "too little to open an account");
plan = undefined;
const out = await ghostB.unshield(carol.address, "25.5", { onPlan: (p) => void (plan = p) });
assert.deepEqual(out.map((s) => s.kind), ["unshield"]);
assert.equal(out[0].fee, pool0.feeNewAccount, "an unshield that opens an account pays the higher canonical fee");
assert.deepEqual(plan, { kind: "unshield", amount: parseFang("25.5"), merges: 0, mergeFee: pool0.fee, fee: pool0.feeNewAccount, totalFees: pool0.feeNewAccount });
done(out);
const carolNow = await client.account(carol.address);
assert.equal(BigInt(carolNow.balance), parseFang("25.5"));
b = await ghostB.balance();
assert.equal(b.balance, parseFang("80") - parseFang("25.5") - pool0.feeNewAccount);
console.log(`✓ bob unshielded 25.5 FANG to carol (${carolNow.balance_display}); bob has ${b.display}`);

// To an account that exists, by name, with the lower fee; then a transfer back with a confirm callback.
const back = await ghostA.unshield(name, "1");
assert.equal(back[back.length - 1].fee, pool0.fee);
done(back);
await ghostA.publishAddress();
let shown: string | undefined;
const reply = await ghostB.send(alice.address, "2", { confirm: (r) => ((shown = r.fingerprint), true) });
assert.equal(shown, ghostA.fingerprint, "confirm is shown the fingerprint of the address the node reported");
done(reply);
// A merge on request: alice's change and bob's 2 FANG become one note.
a = await ghostA.balance();
assert.equal(a.notes.length, 2);
const merged = await ghostA.merge();
assert.equal(merged.kind, "merge");
done(merged);
a = await ghostA.balance();
assert.equal(a.notes.length, 1);
await rejects(() => ghostA.merge(), /nothing to merge/, "one note left");

// ---- 7. everything adds up ----
b = await ghostB.balance();
const aliceExpected = parseFang("90") - parseFang("80") - 2n * pool0.fee - parseFang("1") - pool0.fee + parseFang("2") - pool0.fee;
const bobExpected = parseFang("80") - parseFang("25.5") - pool0.feeNewAccount - parseFang("2") - pool0.fee;
assert.equal(a.balance, aliceExpected);
assert.equal(b.balance, bobExpected);
const end = await ghostStatus();
assert.equal(BigInt(end.pool), a.balance + b.balance, "the pool is exactly the two wallets' notes");
assert.equal((await ghostA.pool()).pool, a.balance + b.balance);
console.log(`✓ it adds up: alice ${a.display} + bob ${b.display} = pool ${fang(BigInt(end.pool))}; carol ${carolNow.balance_display} (public)`);

// ---- 8. the note store, and a node that lies ----
const files = readdirSync(aliceStore.dir);
assert.equal(files.length, 1);
const storeFile = join(aliceStore.dir, files[0]);
assert.equal(statSync(storeFile).mode & 0o777, 0o600, "the note store is mode 600");
assert.equal(statSync(aliceStore.dir).mode & 0o777, 0o700);
const stored = readFileSync(storeFile, "utf8");
assert.ok(!stored.includes(alice.seedHex) && !/rseed|seed|"sk"|viewing/i.test(stored), "no seed, key or note secret in the note store");
assert.deepEqual(Object.keys(JSON.parse(stored).notes[0]).sort(), ["enc", "height", "memo", "nullifier", "position", "spentAt", "tx", "value"]);
// The same key and store: nothing to read again. The same key from nothing: the same notes.
const again = await GhostWallet.fromSeed(alice.seed, { client, store: new FileNoteStore(aliceStore.dir) });
const restored = await again.sync();
assert.deepEqual([restored.newLeaves, restored.balance, restored.leaves], [0, a.balance, end.frontier_size]);
const fresh = await GhostWallet.fromSeed(alice.seed, { client, store: new MemoryNoteStore() });
const rescanned = await fresh.sync();
assert.deepEqual([rescanned.newLeaves, rescanned.balance, rescanned.notes], [end.frontier_size, a.balance, a.notes]);
// A store that was tampered with is a cache gone bad: scanned again, same answer.
const bad = JSON.parse(stored);
bad.notes[bad.notes.length - 1].value = "999000000000";
writeFileSync(storeFile, JSON.stringify(bad), { mode: 0o600 });
const healed = await (await GhostWallet.fromSeed(alice.seed, { client, store: new FileNoteStore(aliceStore.dir) })).sync();
assert.deepEqual([healed.newLeaves, healed.balance], [end.frontier_size, a.balance]);

// A node that lists the pool's outputs wrongly (here: another commitment for a leaf) is refused, and nothing it said is kept.
const liar = createServer(async (req, res) => {
  const up = await fetch(RPC + req.url);
  let text = await up.text();
  if (req.url?.startsWith("/api/ghost/commitments")) {
    const page = JSON.parse(text);
    if (page.commitments.length > 1) page.commitments[0].cm = page.commitments[1].cm;
    text = JSON.stringify(page);
  }
  res.writeHead(up.status, { "content-type": "application/json" }).end(text);
});
servers.push(liar);
await new Promise<void>((ok) => liar.listen(0, "127.0.0.1", ok));
const lying = new Client(`http://127.0.0.1:${(liar.address() as AddressInfo).port}`, { network: "local" });
const fooled = await GhostWallet.fromSeed(alice.seed, { client: lying });
await rejects(() => fooled.sync(), /lists the pool's outputs wrongly \(they don't hash to its anchor\)/, "leaves that don't hash to the node's anchor");
await rejects(() => fooled.unshield(carol.address, "1"), /lists the pool's outputs wrongly/, "and nothing is spent on its word");
console.log("✓ the note store is mode 600 with no secrets, restores and rebuilds; a node whose leaves don't hash to its anchor is refused");

const sorted = [...proveMs].sort((x, y) => x - y);
console.log(`\n${proveMs.length} proofs by the WebAssembly prover, all accepted by the node: ${sorted[0]}–${sorted[sorted.length - 1]} ms, median ${sorted[Math.floor(sorted.length / 2)]} ms`);
console.log("✅ Ghost Mode SDK e2e passed");
for (const w of [ghostA, ghostB, again, fresh, fooled]) w.close();
process.exit(0);
