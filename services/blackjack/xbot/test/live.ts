// The bot on a real local chain, through the real table driver and the real renderer: a table is
// created, players link X handles on chain, deposit and grant the relayer; a small dealer in
// this file deals; posts are fed to the bot and its replies land as files. Nothing is posted,
// and the link check is the local stand-in (no request leaves this machine).
//
//   services/blackjack/xbot/test/local-chain.sh      (starts a chain of its own on 38500, runs this, stops it)
// or, against a local chain that is already running:
//   BLOCKULA_RPC=http://127.0.0.1:38500 npm run test:live -w @blockula/blackjack-xbot
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { Client, Wallet, checkReceipt } from "@blockula/sdk";
import { DEFAULT_LIMITS, DROPS_PER_FANG, Outcome, Table, call, chainFromTop, createTable, dealers, eventsOf, fang, owedLinks, send, toHex, verifyHand, type Game, type TableCall } from "@blockula/blackjack";
import { renderTable } from "@blockula/blackjack-render";
import { Bot, DirSink, JsonStore, NOTE, SdkTable, X_LIMIT, bannedIn, keyPath, localRpc, localVerifier, readKey, storePath, systemClock, writeKey, xLength, type LocalPost, type Post, type Written } from "../src/index.js";

const RPC = process.env.BLOCKULA_RPC;
if (!RPC) {
  console.log("- BLOCKULA_RPC isn't set: the live bot test is skipped (see the file's header)");
  process.exit(0);
}
localRpc(RPC);
const client = new Client(RPC, { network: "local" });
await client.status();
const FANG = DROPS_PER_FANG;
const guest = (file: string) => new Uint8Array(readFileSync(new URL(`../../../../node/vm/tests/programs/${file}`, import.meta.url)));
const dir = mkdtempSync(join(tmpdir(), "xbot-live-"));
const home = join(dir, "home");
const out = join(dir, "out");

// Every request this process makes is counted: all of them must be to the local node.
const asked = new Set<string>();
const realFetch = globalThis.fetch;
globalThis.fetch = ((input: Parameters<typeof fetch>[0], init?: Parameters<typeof fetch>[1]) => {
  asked.add(new URL(input instanceof Request ? input.url : String(input)).origin);
  return realFetch(input, init);
}) as typeof fetch;

// ---- accounts: the relayer's key is made as `init` makes it, in a 0600 file ----
writeKey(home, Wallet.generate().seedHex);
assert.equal(statSync(keyPath(home)).mode & 0o777, 0o600);
const wallet = async (drips = 1) => {
  const w = Wallet.generate(client);
  for (let i = 0; i < drips; i++) await w.faucet();
  return w;
};
const [owner, dealer, alice, bob, carol] = await Promise.all([wallet(4), wallet(), wallet(), wallet(), wallet()]);
await client.addressPrefix();
const relayer = Wallet.fromSeed(readKey(home), client);
await relayer.faucet();

// ---- the table: quick deadlines, a bankroll, the relayer approved ----
const program = await createTable(owner, guest("blackjack.wasm"), guest("blackjack.iface"), dealer.address);
const table = Table.at(RPC, program);
console.log("table:", program);
const go = (w: Wallet, c: TableCall) => send(w, program, c);
await go(owner, call.setLimits({ ...DEFAULT_LIMITS, revealBlocks: 12n, idleBlocks: 600n }));
await go(owner, call.fund(300n * FANG));
await go(owner, call.approveRelayer(relayer.address, true));
const height = async () => BigInt((await client.status()).height as number);
const balanceOf = async (address: string) => BigInt((await client.account(address)).balance);

// ---- the players: an X handle linked on chain, a table balance, a grant ----
// The proof "posts" exist only in this list: the stand-in the bot checks links against.
const proofs: LocalPost[] = [];
async function join_(w: Wallet, handle: string, proofBy: string | null, deposit: bigint) {
  const postId = String(1_700_000_000_000_000_000n + BigInt(proofs.length + 1));
  const proof = await w.xProof(handle);
  await w.linkX(handle, postId);
  if (proofBy) proofs.push({ id: postId, handle: proofBy, text: proof.text });
  await go(w, call.deposit(deposit));
  await go(w, call.setRelayer(relayer.address, 5n * FANG, 20n * FANG, (await height()) + 2_000n));
}
await join_(alice, "Alice_Live", "Alice_Live", 20n * FANG);
await join_(bob, "Bob_Live", "Bob_Live", 12n * FANG);
// Carol names a handle, and the post under her record is someone else's.
await join_(carol, "Carol_Live", "SomeoneElse", 5n * FANG);
assert.equal((await client.xLink(alice.address))?.handle, "Alice_Live");

// ---- a small dealer: commits chains, and reveals whatever a hand is owed ----
const chains = new Map<bigint, string[]>();
async function commit(n: number) {
  const first = (await table.house()).nextCommit;
  const made = Array.from({ length: n }, () => chainFromTop(toHex(randomBytes(32))));
  made.forEach((c, i) => chains.set(first + BigInt(i), c));
  await go(dealer, call.commitBatch(first, made.map((c) => c[0])));
}
await commit(16);
let dealing = true;
let dealerStopped = false;
const dealerLoop = (async () => {
  while (!dealerStopped) {
    if (dealing) {
      for (const id of await table.openGames()) {
        const g = await table.game(id);
        const chain = chains.get(id);
        if (!dealing || !g || !chain || !dealers(g.phase)) continue;
        await go(dealer, call.reveal(id, owedLinks(g, chain))).catch(() => undefined);
      }
    }
    await new Promise((r) => setTimeout(r, 250));
  }
})();

// ---- the bot: the real driver, the real renderer, a store and replies in the temp dir ----
const config = { cooldownMs: 0, revealWaitMs: 30_000, pollMs: 300 };
const driver = await SdkTable.open({ rpc: RPC, program, seed: readKey(home) });
assert.equal(driver.relayer, relayer.address);
const newBot = () => new Bot({ table: driver, store: JsonStore.open(storePath(home)), verifyLink: localVerifier(proofs), render: (s) => renderTable(s), sink: new DirSink(out), clock: systemClock, config });
let bot = newBot();
assert.deepEqual(await bot.recover(), []);

let n = 0;
const all: Written[] = [];
async function say(handle: string, text: string, inReplyToId: string | null = null) {
  const post: Post = { id: String(1_860_000_000_000_000_000n + BigInt(Date.now()) * 100n + BigInt(n++)), authorHandle: handle, text, inReplyToId, createdAt: new Date().toISOString() };
  const r = await bot.handle(post);
  all.push(...r.replies);
  return { post, result: r.result, replies: r.replies };
}

/** A reply's files: the text, and for a hand a 1200 × 675 PNG and its alt text. */
function files(r: Written) {
  const folder = join(out, `${r.meta.inReplyTo}-${r.seq}`);
  const text = readFileSync(join(folder, "reply.txt"), "utf8").trimEnd();
  assert.equal(text, r.text);
  assert.ok(xLength(text) <= X_LIMIT);
  const meta = JSON.parse(readFileSync(join(folder, "meta.json"), "utf8"));
  assert.deepEqual([meta.id, meta.inReplyTo, meta.handle, meta.action, meta.posted], [r.id, r.meta.inReplyTo, r.meta.handle, r.meta.action, false]);
  if (!r.image) {
    assert.deepEqual(readdirSync(folder).sort(), ["meta.json", "reply.txt"]);
    return { text, meta, alt: null };
  }
  assert.deepEqual(readdirSync(folder).sort(), ["image.alt.txt", "image.png", "meta.json", "reply.txt"]);
  const png = readFileSync(join(folder, "image.png"));
  assert.deepEqual([...png.subarray(0, 8)], [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a], "a PNG");
  assert.deepEqual([png.readUInt32BE(16), png.readUInt32BE(20)], [1200, 675], "1200 × 675");
  assert.ok(png.length > 10_000);
  const alt = readFileSync(join(folder, "image.alt.txt"), "utf8").trimEnd();
  assert.ok(alt.startsWith(`A blackjack table on Blockula, hand ${meta.gameId}. @${r.meta.handle}'s cards`) || alt.includes("aren't dealt yet"), alt);
  assert.deepEqual(bannedIn(`${text}\n${alt}`), []);
  return { text, meta, alt };
}

/** The table's own `relayed` event in a reply's transaction. */
async function relayedIn(txId: string) {
  const receipt = checkReceipt(RPC as string, await client.tx(txId));
  assert.ok(receipt?.outcome.ok);
  return eventsOf(receipt, program).filter((e) => e.name === "relayed");
}

/** Plays `handle`'s open hand to its end: hits below 15, replying under the bot's last reply. Returns the last reply. */
async function playOut(handle: string, account: string, last: Written): Promise<Written> {
  for (let i = 0; i < 12; i++) {
    const open = (await table.player(account)).openGame;
    if (open === null) return last;
    const g = (await table.game(open)) as Game;
    const move = g.playerTotal < 15 ? "hit" : "stand";
    const r = await say(handle, `@BlockulaChain ${move}`, last.id);
    assert.equal(r.result, "ok", `${handle}'s ${move}`);
    assert.equal(r.replies.length, 1);
    const f = files(r.replies[0]);
    assert.equal(f.meta.action, move);
    assert.deepEqual(await relayedIn(f.meta.txId), [{ name: "relayed", player: account, relayer: relayer.address, postId: BigInt(r.post.id), action: move, game: open }], `the ${move}'s relayed event carries the post's id`);
    last = r.replies[0];
  }
  throw new Error("the hand didn't end");
}

// ---- alice: a whole hand ----
const aliceBefore = await balanceOf(alice.address);
const d1 = await say("Alice_Live", "@BlockulaChain deal 2");
assert.equal(d1.result, "ok");
const f1 = files(d1.replies[0]);
assert.match(f1.text, /^Hand 0/);
assert.ok(f1.text.endsWith(NOTE), "the reply that starts a hand carries the note");
assert.deepEqual(await relayedIn(f1.meta.txId), [{ name: "relayed", player: alice.address, relayer: relayer.address, postId: BigInt(d1.post.id), action: "bet", game: 0n }], "the bet's relayed event carries the post's id");
const end1 = await playOut("Alice_Live", alice.address, d1.replies[0]);
const fe1 = files(end1);
assert.match(fe1.text, /^Hand 0: /);
assert.ok(fe1.text.endsWith(NOTE), "the reply that ends a hand carries the note");
let rec = await table.player(alice.address);
assert.equal(rec.last?.game, 0n);
verifyHand({ ...rec.last!, game: 0n });
assert.equal(rec.balance, 18n * FANG + rec.last!.payout, "the table balance is the deposit less the stake plus what came back");
assert.ok(fe1.text.includes(rec.last!.payout > 0n ? `${fang(rec.last!.payout)} FANG came back to your table balance, now ${fang(rec.balance)} FANG.` : `Nothing came back. Table balance: ${fang(rec.balance)} FANG.`), fe1.text);
assert.equal(await balanceOf(alice.address), aliceBefore, "a relayed hand moves nothing in the player's account");
console.log(`✓ a hand for @Alice_Live: ${all.length} replies, each with a 1200 × 675 PNG and alt text; outcome ${rec.last!.outcome}`);

// ---- two players at once, a stranger in the thread, a second deal, a balance ----
const d2 = await say("Alice_Live", "@blockulachain  DEAL 3.5");
const d3 = await say("Bob_Live", "@BlockulaChain deal 1");
assert.deepEqual([d2.result, d3.result], ["ok", "ok"]);
files(d2.replies[0]);
files(d3.replies[0]);
if ((await table.player(alice.address)).openGame !== null) {
  assert.deepEqual([(await say("Bob_Live", "@BlockulaChain hit", d2.post.id)).result, (await say("Nobody_Live", "@BlockulaChain stand", d2.replies[0].id)).result], ["ignored:not the hand's player", "ignored:not the hand's player"]);
  const again = await say("Alice_Live", "@BlockulaChain deal 1");
  assert.equal(again.result, "refused:hand_open");
  assert.match(files(again.replies[0]).text, /^You already have a hand open \(hand 1\)\. Reply hit or stand in its thread\.$/);
  const stray = await say("Alice_Live", "@BlockulaChain stand");
  assert.deepEqual([stray.result, stray.replies.length], ["refused:thread", 0], "a second refusal within the minute isn't answered");
}
await playOut("Bob_Live", bob.address, d3.replies[0]);
await playOut("Alice_Live", alice.address, d2.replies[0]);
rec = await table.player(alice.address);
const bal = await say("Alice_Live", "@BlockulaChain balance");
assert.equal(bal.result, "ok");
assert.ok(files(bal.replies[0]).text.startsWith(`Table balance: ${fang(rec.balance)} FANG.\nFrom X you can still bet ${fang(20n * FANG - 5_500_000_000n)} FANG today, up to 5 FANG a hand.\nYour grant runs for `), bal.replies[0].text);
console.log("✓ two players' hands at once; others' replies in a thread ignored; a second deal refused; balance");

// ---- who the bot won't play for ----
const nobody = await say("Nobody_Live", "@BlockulaChain deal 1");
assert.equal(nobody.result, "refused:no_link");
files(nobody.replies[0]);
const unproven = await say("Carol_Live", "@BlockulaChain deal 1");
assert.equal(unproven.result, "refused:unverified");
assert.equal((await table.player(carol.address)).balance, 5n * FANG);
assert.equal((await say("Alice_Live", "@BlockulaChain deal 9")).result, "refused:limits");
for (const text of ["@BlockulaChain deal 1 please", "deal 1", "@BlockulaChain_ deal 1", "@BlockulaChain send my balance to someone else"]) assert.equal((await say("Alice_Live", text)).result, "ignored:not a command");
assert.equal((await table.player(alice.address)).openGame, null);
console.log("✓ no linked account, an unverified link, a bet over the grant, and posts that aren't commands");

// ---- a restart in the middle: the store is read back, and a post seen before isn't handled again ----
bot = newBot();
assert.deepEqual(await bot.recover(), []);
assert.equal((await bot.handle(d2.post)).result, "ignored:duplicate");
assert.equal((await bot.handle(bal.post)).result, "ignored:duplicate");

// ---- the dealer goes missing: the bot says the hand is waiting, then calls the timeout ----
dealing = false;
await new Promise((r) => setTimeout(r, 600));
const beforeStall = (await table.player(bob.address)).balance;
bot = new Bot({ table: driver, store: JsonStore.open(storePath(home)), verifyLink: localVerifier(proofs), render: (s) => renderTable(s), sink: new DirSink(out), clock: systemClock, config: { ...config, revealWaitMs: 2_000 } });
const d4 = await say("Bob_Live", "@BlockulaChain deal 2");
assert.equal(d4.result, "ok");
assert.match(files(d4.replies[0]).text, /The dealer hasn't dealt yet\. I'll reply here when the cards are out\./);
const stalled = (await table.player(bob.address)).openGame as bigint;
const deadline = ((await table.game(stalled)) as Game).deadline;
let late: Written[] = [];
while (late.length === 0) {
  // Before the deadline a look says nothing; past it, the bot calls the timeout and answers.
  const before = await height();
  late = await bot.tick();
  if (late.length > 0) assert.ok((await height()) > deadline, "the timeout came after the deadline");
  else assert.ok(before <= deadline + 2n, "the bot timed the hand out once the deadline had passed");
  await new Promise((r) => setTimeout(r, 300));
}
all.push(...late);
assert.equal(late.length, 1);
const fl = files(late[0]);
assert.equal(fl.meta.action, "timeout");
assert.equal(fl.meta.inReplyTo, d4.post.id);
assert.match(fl.text, /^Hand \d+: You win: the dealer never dealt\.\nBet 2 FANG\. 5 FANG came back to your table balance, now /);
const brec = await table.player(bob.address);
assert.deepEqual([brec.last?.outcome, brec.balance], [Outcome.ForfeitDeal, beforeStall + 3n * FANG]);
dealing = true;
console.log("✓ a dealer that never dealt: a waiting reply, then the bot's timeout paid the player");

// ---- stop: the whole table balance goes back to the player's own account ----
const onTable = (await table.player(alice.address)).balance;
const [accountBefore, relayerBefore] = [await balanceOf(alice.address), await balanceOf(relayer.address)];
const stop = await say("Alice_Live", "@BlockulaChain stop");
assert.equal(stop.result, "ok");
const fs = files(stop.replies[0]);
assert.equal(fs.text, `Sent your table balance of ${fang(onTable)} FANG back to your own account.\nI won't play for you until you post deal again.\n${NOTE}`);
assert.deepEqual(await relayedIn(fs.meta.txId), [{ name: "relayed", player: alice.address, relayer: relayer.address, postId: BigInt(stop.post.id), action: "withdraw", game: 0n }]);
assert.equal((await table.player(alice.address)).balance, 0n);
assert.equal(await balanceOf(alice.address), accountBefore + onTable, "the player's own account got exactly the table balance");
assert.ok((await balanceOf(relayer.address)) < relayerBefore, "the relayer paid the fee and got nothing");
for (const text of ["@BlockulaChain balance", "@BlockulaChain stop", "@BlockulaChain hit"]) assert.equal((await say("Alice_Live", text)).result, "ignored:stopped");
const quiet = await say("Alice_Live", "@BlockulaChain deal 1");
assert.deepEqual([quiet.result, quiet.replies.length], ["refused:balance", 0]);
// Back in: a deposit with her own key, then a deal.
await go(alice, call.deposit(3n * FANG));
const d5 = await say("Alice_Live", "@BlockulaChain deal 1");
assert.equal(d5.result, "ok");
await playOut("Alice_Live", alice.address, d5.replies[0]);
console.log("✓ stop sent the balance to the player's own account; the bot stayed quiet until a deposit and a deal");

// ---- the command line, on the same home: status, and a run from a file of posts ----
const cli = fileURLToPath(new URL("../src/cli.ts", import.meta.url));
const tsx = fileURLToPath(new URL("../../../../node_modules/.bin/tsx", import.meta.url));
const env = { ...process.env, BLACKJACK_XBOT_HOME: home, BLOCKULA_RPC: RPC, BLACKJACK_TABLE: program };
const status = execFileSync(tsx, [cli, "status"], { env, encoding: "utf8" });
assert.match(status, /Dry run build: no X client, nothing is posted\./);
assert.ok(status.includes(`Relayer: ${relayer.address}`) && status.includes(`Table: ${program}`), status);
assert.ok(!status.includes(readKey(home)), "the key is never printed");
const postsFile = join(dir, "posts.jsonl");
const proofsFile = join(dir, "proofs.json");
const cliPost: Post = { id: String(1_870_000_000_000_000_000n + BigInt(Date.now())), authorHandle: "Bob_Live", text: "@BlockulaChain balance", inReplyToId: null, createdAt: new Date().toISOString() };
writeFileSync(postsFile, [JSON.stringify(cliPost), "not json", JSON.stringify(d4.post)].join("\n") + "\n");
writeFileSync(proofsFile, JSON.stringify(proofs));
const cliOut = join(dir, "cli-out");
const ran = execFileSync(tsx, [cli, "run", "--posts", postsFile, "--out", cliOut, "--proofs", proofsFile, "--drain-ms", "0"], { env, encoding: "utf8" });
assert.match(ran, /3 posts read, 1 replies written, 1 unreadable lines, 0 hands still waiting on the dealer\./);
assert.ok(readFileSync(join(cliOut, `${cliPost.id}-0`, "reply.txt"), "utf8").startsWith(`Table balance: ${fang((await table.player(bob.address)).balance)} FANG.`));
assert.throws(() => execFileSync(tsx, [cli, "status", "--rpc", "https://demo.blockula.app"], { env, encoding: "utf8", stdio: "pipe" }), /local chain only/);
assert.throws(() => execFileSync(tsx, [cli, "init"], { env, encoding: "utf8", stdio: "pipe" }), /EEXIST/, "init never replaces a key");
console.log("✓ the command line: status, a run from a file of posts, a non-local node refused, a key never replaced");

// ---- the end: every hand settled and verified, every drop accounted for, nothing asked but the local node ----
dealerStopped = true;
await dealerLoop;
const h = await table.house();
assert.deepEqual([h.open, h.reserved, h.stakes], [0, 0n, 0n]);
assert.equal(await balanceOf(program), h.bankroll + h.reserved + h.stakes + h.table, "balance = bankroll + reserved + stakes + table");
const [a, b, c] = [await table.player(alice.address), await table.player(bob.address), await table.player(carol.address)];
assert.equal(h.table, a.balance + b.balance + c.balance, "the table balances are the players'");
for (const p of [a, b]) verifyHand({ ...p.last!, game: p.last!.game });
const store = JsonStore.open(storePath(home)).state;
assert.deepEqual(store.hands, {});
assert.deepEqual(Object.values(store.posts).filter((p) => p.state !== "done"), []);
for (const r of all) {
  assert.ok(xLength(r.text) <= X_LIMIT);
  assert.deepEqual(bannedIn(`${r.text}\n${r.image?.alt ?? ""}`), []);
}
assert.deepEqual([...asked], [new URL(RPC).origin], "every request went to the local node");
rmSync(dir, { recursive: true, force: true });
console.log(`✅ the live bot test passed: ${all.length} replies written as files, ${Number(h.nextGame)} hands, nothing posted`);
