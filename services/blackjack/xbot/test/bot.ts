// The bot's rules that aren't a limit, a link or a crash: whose move a hand takes and where,
// one hand a player, stop and coming back, what a balance says, posts that aren't posts; the
// dry run's loop and the files a reply becomes; the key file.
//
//   npx tsx test/bot.ts
import assert from "node:assert/strict";
import { chmodSync, existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { altText } from "@blockula/blackjack";
import { Bot, DirSink, MemoryStore, NOTE, keyPath, loadSettings, pinStore, readKey, runPosts, writeKey, type Post } from "../src/index.js";
import { World, type PlayerSetup } from "./world.js";

const F = 1_000_000_000n;
const grant = { maxBet: String(25n * F), dailyCap: String(200n * F), expiresIn: "40000" };
const player = (name: string, handle: string, deposit = 100n): PlayerSetup => ({ name, handle, link: "verified", deposit: String(deposit * F), grant });
let n = 0;
const mk = (handle: string, text: string, inReplyToId: string | null = null): Post => ({ id: String(1_840_000_000_000_000_000n + BigInt(n++)), authorHandle: handle, text, inReplyToId, createdAt: "" });

/** A world where Mina has a hand open that needs a move. */
async function withHand() {
  for (let i = 0; ; i++) {
    const w = new World(`bot-${i}`, { dealerDelay: 0, players: [player("mina", "Mina_H"), player("lucy", "Lucy_W")] });
    const say = async (handle: string, text: string, to: string | null = null) => {
      w.clock.advance(5_000);
      const post = mk(handle, text, to);
      const r = await w.bot.handle(post);
      return { post, result: r.result, replies: r.replies };
    };
    const deal = await say("Mina_H", "@BlockulaChain deal 5");
    const hand = w.store.state.hands.mina_h;
    if (hand && (w.table.games.get(BigInt(hand.gameId))?.playerTotal ?? 99) <= 10) return { w, say, deal };
  }
}

// ---- a move: by the hand's player, as a reply in the hand's thread ----
{
  const { w, say, deal } = await withHand();
  const first = deal.replies[0];
  assert.equal(first.meta.action, "deal");
  assert.ok(first.text.includes("Reply hit or stand.") && first.text.endsWith(NOTE));
  assert.equal(first.meta.gameId, "0");
  assert.equal(first.meta.handle, "Mina_H");
  assert.equal(first.meta.account, w.table.account("mina"));
  assert.match(first.meta.txId as string, /^[0-9a-f]{64}$/);
  // Not a reply at all, and a reply to something else: refused (once; then silently).
  assert.equal((await say("Mina_H", "@BlockulaChain hit")).result, "refused:thread");
  assert.equal((await say("Mina_H", "@BlockulaChain hit", "12345")).result, "refused:thread");
  // Someone else under the hand's deal, and under the bot's reply: no answer, linked or not.
  for (const to of [deal.post.id, first.id]) {
    for (const stranger of ["Lucy_W", "Nobody_Here"]) {
      const r = await say(stranger, "@BlockulaChain hit", to);
      assert.deepEqual([r.result, r.replies.length], ["ignored:not the hand's player", 0]);
    }
  }
  assert.equal(w.table.relayed.length, 1, "nothing but the deal was relayed");
  // The bot's replies that aren't about the hand (a refusal) aren't the hand's thread.
  w.clock.advance(120_000);
  const refusal = await say("Mina_H", "@BlockulaChain hit", "12345");
  assert.deepEqual([refusal.result, refusal.replies.length], ["refused:thread", 1]);
  assert.equal((await say("Mina_H", "@BlockulaChain hit", refusal.replies[0].id)).result, "refused:thread");
  assert.equal(w.table.relayed.length, 1);
  // The player, under the bot's reply: a card.
  const hit = await say("Mina_H", "@BlockulaChain hit", first.id);
  assert.equal(hit.result, "ok");
  assert.equal(hit.replies[0].meta.action, "hit");
  assert.ok(!hit.replies[0].text.includes(NOTE), "a reply in the middle of a hand doesn't repeat the note");
  assert.deepEqual(w.table.relayed.map((e) => [e.action, String(e.postId)]), [["bet", deal.post.id], ["hit", hit.post.id]], "the table's events carry the posts' ids");
  // Under their own last move, and under the deal: both are the thread.
  const stand = await say("Mina_H", "@BlockulaChain stand", w.store.state.hands.mina_h ? hit.post.id : deal.post.id);
  if (stand.result === "ok") {
    assert.match(stand.replies[0].text, /^Hand 0: /);
    assert.ok(stand.replies[0].text.endsWith(NOTE));
    assert.equal(stand.replies[0].image?.alt, altText(w.snapshots[w.snapshots.length - 1]));
  }
  // Over: a move now has no hand to take.
  for (let i = 0; i < 10 && w.store.state.hands.mina_h; i++) await say("Mina_H", "@BlockulaChain stand", deal.post.id);
  assert.equal((await say("Mina_H", "@BlockulaChain hit", deal.post.id)).result, "refused:no_hand");
}

// ---- one hand a player; two players' hands don't mix ----
{
  const { w, say, deal } = await withHand();
  const second = await say("Mina_H", "@BlockulaChain deal 2");
  assert.equal(second.result, "refused:hand_open");
  assert.match(second.replies[0].text, /^You already have a hand open \(hand 0\)\. Reply hit or stand in its thread\.$/);
  assert.equal(w.table.house_.nextGame, 1n);
  const lucy = await say("Lucy_W", "@BlockulaChain deal 3");
  assert.equal(lucy.result, "ok");
  assert.equal(w.table.house_.nextGame, 2n);
  // Each player's move lands on their own hand only.
  if (w.store.state.hands.lucy_w) {
    assert.equal((await say("Mina_H", "@BlockulaChain stand", lucy.post.id)).result, "ignored:not the hand's player");
    assert.equal((await say("Lucy_W", "@BlockulaChain stand", deal.post.id)).result, "ignored:not the hand's player");
    assert.equal((await say("Lucy_W", "@BlockulaChain stand", lucy.post.id)).result, "ok");
  }
  assert.equal((await say("Mina_H", "@BlockulaChain stand", deal.post.id)).result, "ok");
  assert.ok(w.table.maxOpenPerPlayer === 1 && w.table.invariant());
}

// ---- stop: the balance goes to the player's own account, and the bot goes quiet until a deal goes through ----
{
  const { w, say, deal } = await withHand();
  const account = w.table.account("mina");
  const early = await say("Mina_H", "@BlockulaChain stop");
  assert.deepEqual([early.result, early.replies[0].text], ["refused:finish_first", "Finish your open hand first, then post stop."]);
  for (let i = 0; i < 10 && w.store.state.hands.mina_h; i++) await say("Mina_H", "@BlockulaChain stand", deal.post.id);
  const before = (await w.table.player(account)).balance;
  w.clock.advance(60_000);
  const stop = await say("Mina_H", "@BlockulaChain stop");
  assert.equal(stop.result, "ok");
  assert.match(stop.replies[0].text, /^Sent your table balance of [0-9.]+ FANG back to your own account\.\nI won't play for you until you post deal again\.\n/);
  assert.equal(w.table.ledger.get(account)?.withdrawn, before, "the whole balance, to its own account");
  assert.equal((await w.table.player(account)).balance, 0n);
  assert.deepEqual(w.table.relayed[w.table.relayed.length - 1], { name: "relayed", player: account, relayer: w.table.relayer, postId: BigInt(stop.post.id), action: "withdraw", game: 0n });
  // Stopped: no command is answered, and a deal the balance can't cover is refused without a word.
  for (const text of ["@BlockulaChain balance", "@BlockulaChain hit", "@BlockulaChain stop"]) assert.deepEqual([(await say("Mina_H", text)).result], ["ignored:stopped"]);
  w.clock.advance(120_000);
  const broke = await say("Mina_H", "@BlockulaChain deal 5");
  assert.deepEqual([broke.result, broke.replies.length], ["refused:balance", 0]);
  assert.equal(w.store.state.stopped.mina_h, true);
  // The way back is the player's own: a deposit with their key, then a deal.
  w.table.deposit(account, 10n * F);
  const back = await say("Mina_H", "@BlockulaChain deal 5");
  assert.equal(back.result, "ok");
  assert.equal(w.store.state.stopped.mina_h, undefined);
  assert.equal((await say("Mina_H", "@BlockulaChain balance")).result, "ok");
  // Another player is never stopped by it.
  assert.equal((await say("Lucy_W", "@BlockulaChain balance")).result, "ok");
}

// ---- stop with nothing to send, and without a grant ----
{
  const w = new World("bot-stop", { dealerDelay: 0, players: [player("empty", "Mina_H", 0n), { ...player("nogrant", "Lucy_W"), grant: null }] });
  w.store.state.enrolled.push(w.table.account("nogrant"));
  const a = await w.bot.handle(mk("Mina_H", "@BlockulaChain stop"));
  assert.deepEqual([a.result, a.replies[0].text.split("\n")[0]], ["refused:empty", "Your table balance is already empty, so there's nothing to send back."]);
  const b = await w.bot.handle(mk("Lucy_W", "@BlockulaChain stop"));
  assert.deepEqual([b.result, b.replies[0].text.split("\n")[0]], ["refused:stop_no_grant", "I can't move your table balance without a live grant. Withdraw it yourself on the table page."]);
  assert.deepEqual(w.store.state.stopped, { mina_h: true, lucy_w: true });
  assert.equal(w.table.relayed.length, 0);
  assert.equal((await w.table.player(w.table.account("nogrant"))).balance, 100n * F);
}

// ---- balance: the table balance and what's left of today ----
{
  const w = new World("bot-balance", { dealerDelay: 0, players: [{ ...player("mina", "Mina_H", 40n), grant: { maxBet: String(10n * F), dailyCap: String(30n * F), expiresIn: "500" } }] });
  const r = await w.bot.handle(mk("Mina_H", "@BlockulaChain balance"));
  assert.equal(r.replies[0].text, "Table balance: 40 FANG.\nFrom X you can still bet 30 FANG today, up to 10 FANG a hand.\nYour grant runs for 500 more blocks.");
  assert.equal(r.replies[0].image, null);
  w.clock.advance(5_000);
  await w.bot.handle(mk("Mina_H", "@BlockulaChain deal 10"));
  w.clock.advance(5_000);
  const during = await w.bot.handle(mk("Mina_H", "@BlockulaChain balance"));
  if (w.store.state.hands.mina_h) assert.match(during.replies[0].text, /^Table balance: 30 FANG\.\nYou have a hand open\.\nFrom X you can still bet 20 FANG today/);
  w.table.revoke(w.table.account("mina"));
  w.clock.advance(5_000);
  const after = await w.bot.handle(mk("Mina_H", "@BlockulaChain balance"));
  assert.match(after.replies[0].text, /This bot has no grant from your account right now, so it can't play for you\.$/);
}

// ---- things that aren't posts, the bot's own posts, old posts ----
{
  const w = new World("bot-posts", { dealerDelay: 0, players: [player("mina", "Mina_H")], config: { maxPostAgeMs: 60_000 } });
  const now = () => new Date(w.clock.now()).toISOString();
  for (const bad of [null, 5, "post", [], {}, { id: "1", authorHandle: "Mina_H" }, { id: "x", authorHandle: "Mina_H", text: "@BlockulaChain hit" }, { id: "1", authorHandle: "Mina H", text: "@BlockulaChain hit" }]) assert.equal((await w.bot.handle(bad)).result, "ignored:not a post");
  assert.equal((await w.bot.handle({ ...mk("BlockulaChain", "@BlockulaChain balance"), createdAt: now() })).result, "ignored:own post");
  assert.equal((await w.bot.handle({ ...mk("blockulachain", "@BlockulaChain deal 5"), createdAt: now() })).result, "ignored:own post");
  assert.equal((await w.bot.handle({ ...mk("Mina_H", "@BlockulaChain balance"), createdAt: now() })).result, "ok");
  w.clock.advance(5_000);
  assert.equal((await w.bot.handle({ ...mk("Mina_H", "@BlockulaChain deal 5"), createdAt: new Date(w.clock.now() - 61_000).toISOString() })).result, "ignored:old");
  assert.equal((await w.bot.handle({ ...mk("Mina_H", "@BlockulaChain deal 5"), createdAt: "yesterday" })).result, "ignored:old");
  assert.equal(w.table.relayed.length, 0);
  assert.equal(Object.keys(w.store.state.posts).length, 1, "only the handled command is recorded");
}

// ---- the dry run: JSON lines in, a folder a reply out ----
const dir = mkdtempSync(join(tmpdir(), "xbot-run-"));
{
  const w = new World("bot-run", { dealerDelay: 15, players: [player("mina", "Mina_H")] });
  const out = join(dir, "out");
  const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47]);
  const bot = new Bot({ table: w.table, store: new MemoryStore(), verifyLink: w.table.verifier(), render: (s) => ({ png, alt: altText(s) }), sink: new DirSink(out), clock: w.clock });
  const deal = mk("Mina_H", "@BlockulaChain deal 5");
  const unlinked = mk("Nobody_Here", "@BlockulaChain balance");
  const lines = [JSON.stringify(deal), "", "this line isn't JSON", JSON.stringify({ no: "post" }), JSON.stringify(mk("Passerby_1", "what a game")), JSON.stringify(deal), JSON.stringify(unlinked)];
  const log: string[] = [];
  const sum = await runPosts(bot, lines, { clock: w.clock, drainMs: 120_000, log: (l) => log.push(l) });
  // The dealer here takes 30 s: the deal's reply says the hand is waiting, and the run stays to see the cards.
  assert.deepEqual([sum.posts, sum.unreadable, sum.waiting], [6, 1, 0]);
  assert.deepEqual(sum.results, { ok: 1, "ignored:not a post": 1, "ignored:not a command": 1, "ignored:duplicate": 1, "refused:no_link": 1 });
  assert.equal(sum.replies, 3);
  assert.deepEqual(readdirSync(out).sort(), [`${deal.id}-0`, `${deal.id}-1`, `${unlinked.id}-0`].sort());
  const first = join(out, `${deal.id}-0`);
  assert.deepEqual(readdirSync(first).sort(), ["image.alt.txt", "image.png", "meta.json", "reply.txt"]);
  assert.match(readFileSync(join(first, "reply.txt"), "utf8"), /^Hand 0\. Bet 5 FANG\.\nThe dealer hasn't dealt yet\./);
  assert.deepEqual([...readFileSync(join(first, "image.png"))], [...png]);
  assert.match(readFileSync(join(first, "image.alt.txt"), "utf8"), /^A blackjack table on Blockula, hand 0\./);
  const meta = JSON.parse(readFileSync(join(first, "meta.json"), "utf8"));
  assert.deepEqual([meta.id, meta.inReplyTo, meta.handle, meta.action, meta.gameId, meta.posted, meta.image, meta.account], [`dry-${deal.id}-0`, deal.id, "Mina_H", "deal", "0", false, true, w.table.account("mina")]);
  assert.match(meta.txId, /^[0-9a-f]{64}$/);
  assert.match(meta.height, /^[0-9]+$/);
  const update = JSON.parse(readFileSync(join(out, `${deal.id}-1`, "meta.json"), "utf8"));
  assert.equal(update.action, "update");
  // A refusal is text and meta: no picture.
  assert.deepEqual(readdirSync(join(out, `${unlinked.id}-0`)).sort(), ["meta.json", "reply.txt"]);
  // The log names post ids and results, never a post's text.
  assert.ok(log.length === 5 && log.every((l) => /^([0-9]+|\?) (ok|ignored:[a-z' ]+|refused:[a-z_]+) \(\d (reply|replies)\)$/.test(l)), log.join("\n"));
  // Written again (a restart re-answering), a reply's folder is replaced, not doubled.
  new DirSink(out).write({ seq: 0, text: "again", image: null, meta: { ...meta, inReplyTo: deal.id } });
  assert.deepEqual(readdirSync(first).sort(), ["meta.json", "reply.txt"]);
  assert.equal(readdirSync(out).filter((f) => f.startsWith(".")).length, 0, "no half-written folder is left");
}

// ---- the key file and the settings ----
{
  const home = join(dir, "home");
  const seed = "ab".repeat(32);
  const path = writeKey(home, seed);
  assert.equal(path, keyPath(home));
  assert.equal(statSync(path).mode & 0o777, 0o600);
  assert.equal(readKey(home), seed);
  assert.throws(() => writeKey(home, "cd".repeat(32)), /EEXIST/, "a key is never overwritten");
  assert.equal(readKey(home), seed);
  const loose = join(dir, "loose");
  writeFileSync(writeKey(loose, seed), JSON.stringify({ seed }), { mode: 0o644 });
  chmodSync(keyPath(loose), 0o644);
  assert.throws(() => readKey(loose), /mode 0600/);
  assert.throws(() => readKey(join(dir, "nowhere")), /run "blackjack-xbot init"/);
  assert.ok(!existsSync(join(dir, "nowhere")));

  const env = { ...process.env };
  delete process.env.BLOCKULA_RPC;
  delete process.env.BLACKJACK_TABLE;
  assert.deepEqual(loadSettings(home), { rpc: null, table: null, bot: loadSettings(home).bot });
  writeFileSync(join(home, "config.json"), JSON.stringify({ rpc: "http://127.0.0.1:38501", table: "boo1file", cooldownMs: 5_000, blocked: ["Spammer_1"] }));
  let s = loadSettings(home);
  assert.deepEqual([s.rpc, s.table, s.bot.cooldownMs, s.bot.blocked, s.bot.perHour], ["http://127.0.0.1:38501", "boo1file", 5_000, ["spammer_1"], 60]);
  process.env.BLOCKULA_RPC = "http://127.0.0.1:38502";
  s = loadSettings(home, { table: "boo1flag" });
  assert.deepEqual([s.rpc, s.table], ["http://127.0.0.1:38502", "boo1flag"], "flags, then the environment, then the file");
  writeFileSync(join(home, "config.json"), JSON.stringify({ cooldownMs: -1 }));
  assert.throws(() => loadSettings(home), /cooldownMs/);
  writeFileSync(join(home, "config.json"), JSON.stringify({ blocked: ["not a handle"] }));
  assert.throws(() => loadSettings(home), /blocked/);
  process.env = env;
}
// ---- a store is for one chain and one table ----
{
  const store = new MemoryStore();
  const pin = { chainId: "xbot-live", genesisHash: "11".repeat(32), table: "boo1table" };
  pinStore(store, pin);
  pinStore(store, { ...pin });
  assert.deepEqual(store.state.chain, pin);
  assert.throws(() => pinStore(store, { ...pin, table: "boo1other" }), /another home/);
  assert.throws(() => pinStore(store, { ...pin, genesisHash: "22".repeat(32) }), /another home/);
}
rmSync(dir, { recursive: true, force: true });

console.log("✓ bot: moves in the hand's thread by its player, one hand a player, stop and coming back, balance, the dry run's files, the key file");
