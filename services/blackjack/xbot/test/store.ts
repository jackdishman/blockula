// A post is handled exactly once, across crashes and restarts. The process is "killed" at every
// point where a transaction could be half-done (before it's signed, after it's signed and before
// it's submitted, after it ran and before the bot heard, after it ran and before the reply was
// written, after the reply and before that was recorded), the store is read back from its file,
// and the new bot must finish the post: one relayed action at most, one reply, no second hand.
//
//   npx tsx test/store.ts
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Phase, verifyHand } from "@blockula/blackjack";
import { Crash, JsonStore, MemorySink, type Fault, type Post, type Reply, type State } from "../src/index.js";
import { World, type Setup } from "./world.js";

const dir = mkdtempSync(join(tmpdir(), "xbot-store-"));
const F = 1_000_000_000n;
const setup = (o: Partial<Setup> = {}): Setup => ({ dealerDelay: 0, players: [{ name: "p0", handle: "Mina_H", link: "verified", deposit: String(100n * F), grant: { maxBet: String(25n * F), dailyCap: String(200n * F), expiresIn: "40000" } }], ...o });
let n = 0;
let postN = 0;
const post = (text: string, inReplyToId: string | null = null): Post => ({ id: String(1_800_000_000_000_000_000n + BigInt(postN++)), authorHandle: "Mina_H", text, inReplyToId, createdAt: "2026-01-01T00:00:00Z" });
const onDisk = (path: string) => JSON.parse(readFileSync(path, "utf8")) as State;

/** A world whose store is a file, with a hand dealt and waiting on the player when `open` is set. */
async function world(open: boolean, o: Partial<Setup> = {}) {
  for (;;) {
    const path = join(dir, `store-${n}.json`);
    const w = new World(`crash-${n++}`, setup(o), JsonStore.open(path));
    const account = w.table.account("p0");
    if (!open) return { w, path, account, deal: null };
    const deal = post("@BlockulaChain deal 5");
    assert.equal((await w.bot.handle(deal)).result, "ok");
    const hand = w.store.state.hands.mina_h;
    // A hand that needs a move, and that one hit can't end (so what a hit did is plain to see).
    if (hand && (w.table.games.get(BigInt(hand.gameId))?.playerTotal ?? 99) <= 11) return { w, path, account, deal };
  }
}

const relayedFor = (w: World, id: string) => w.table.relayed.filter((e) => e.postId === BigInt(id));
const repliesTo = (w: World, id: string) => w.sink.replies.filter((r) => r.meta.inReplyTo === id);
const settle = async (w: World) => {
  // Play whatever is open to its end, so the books can be checked.
  for (let i = 0; i < 30 && w.store.state.hands.mina_h; i++) {
    const hand = w.store.state.hands.mina_h;
    w.clock.advance(5_000);
    await w.bot.handle(post("@BlockulaChain stand", hand.thread[0]));
  }
  assert.deepEqual(w.store.state.hands, {});
  assert.equal(w.table.games.size, 0);
  assert.ok(w.table.invariant());
  assert.ok(w.table.maxOpenPerPlayer <= 1);
  for (const s of w.table.settledHands) verifyHand(s);
};

const COMMANDS = ["deal", "hit", "stand", "stop"] as const;
const FAULTS: Fault[] = ["before-sign", "before-submit", "after-submit"];
let cases = 0;
for (const command of COMMANDS) {
  for (const fault of FAULTS) {
    for (const forgotten of fault === "after-submit" ? [false, true] : [false]) {
      const what = `${command}, killed ${fault}${forgotten ? ", and the node forgot the transaction" : ""}`;
      const { w, path, account, deal } = await world(command === "hit" || command === "stand");
      const before = await w.table.player(account);
      w.clock.advance(5_000);
      const p = command === "deal" ? post("@BlockulaChain deal 5") : command === "stop" ? post("@BlockulaChain stop") : post(`@BlockulaChain ${command}`, deal!.id);
      w.table.fault = fault;
      await assert.rejects(w.bot.handle(p), Crash, what);
      // What the file holds is what the next process knows.
      const rec = onDisk(path).posts[p.id];
      assert.equal(rec.state, fault === "before-sign" ? "intent" : "sent", `${what}: the record on disk`);
      assert.equal(rec.txId === null, fault === "before-sign", `${what}: the transaction's id is on disk once it's signed`);
      assert.equal(repliesTo(w, p.id).length, 0);
      assert.equal(relayedFor(w, p.id).length, fault === "after-submit" ? 1 : 0, `${what}: what the table saw`);

      if (forgotten) w.table.forgetTxs();
      const bot = w.restart(JsonStore.open(path));
      const written = await bot.recover();
      const done = w.store.state.posts[p.id];
      assert.equal(done.state, "done", what);
      // Signed but never submitted: it is given up only once it can't run any more, and says so.
      const ran = fault !== "before-submit";
      assert.equal(done.result, ran ? "ok" : "refused:not_sent", what);
      assert.equal(relayedFor(w, p.id).length, ran ? 1 : 0, `${what}: the action ran ${ran ? "once" : "never"}`);
      assert.equal(repliesTo(w, p.id).length, 1, `${what}: one reply`);
      assert.equal(written.length, 1);
      if (!ran) {
        assert.ok((await w.table.height()) > BigInt(rec.validUntil as string), `${what}: given up only past its last valid block`);
        assert.deepEqual(await w.table.player(account), before, `${what}: nothing moved`);
      } else if (command === "deal") {
        assert.equal(w.table.house_.nextGame, 1n, `${what}: one hand was opened`);
      } else if (command === "hit") {
        const g = w.table.games.get(BigInt(done.gameId as string));
        assert.equal(g?.playerCards.length, 3, `${what}: one card was taken`);
      } else if (command === "stand") {
        assert.equal(w.table.games.size, 0, `${what}: the hand was played out`);
      } else {
        assert.equal((await w.table.player(account)).balance, 0n);
        assert.equal(w.table.ledger.get(account)?.withdrawn, before.balance, `${what}: the balance went back once`);
        assert.equal(w.store.state.stopped.mina_h, true);
      }
      // The same post again, to the new process and after another restart: seen, never handled again.
      assert.equal((await bot.handle(p)).result, "ignored:duplicate");
      assert.equal((await w.restart(JsonStore.open(path)).handle(p)).result, "ignored:duplicate");
      assert.deepEqual(await w.bot.recover(), []);
      assert.equal(relayedFor(w, p.id).length, ran ? 1 : 0);
      assert.equal(repliesTo(w, p.id).length, 1);
      if (command !== "stop") await settle(w);
      cases++;
    }
  }
}

// ---- killed between the transaction and the reply, and between the reply and its record ----
class DyingSink extends MemorySink {
  /** `before`: dies instead of writing. `after`: writes, then dies before the bot records it. */
  die: "before" | "after" | null = null;
  /** The process is gone: the table stops answering it too. */
  onDeath: () => void = () => undefined;
  gone = false;
  override write(reply: Reply): string {
    if (this.gone) throw new Crash("the process is gone");
    const how = this.die;
    this.die = null;
    if (how === "before") this.dead("before the reply was written");
    const id = super.write(reply);
    if (how === "after") this.dead("after the reply was written");
    return id;
  }
  private dead(where: string): never {
    this.gone = true;
    this.onDeath();
    throw new Crash(where);
  }
}
for (const command of COMMANDS) {
  for (const die of ["before", "after"] as const) {
    const what = `${command}, killed ${die} its reply was written`;
    const { w, path, deal } = await world(command === "hit" || command === "stand");
    const sink = new DyingSink();
    sink.replies.push(...w.sink.replies);
    w.sink = sink;
    sink.onDeath = () => {
      w.table.dead = true;
    };
    w.restart(w.store);
    w.clock.advance(5_000);
    const p = command === "deal" ? post("@BlockulaChain deal 5") : command === "stop" ? post("@BlockulaChain stop") : post(`@BlockulaChain ${command}`, deal!.id);
    sink.die = die;
    await assert.rejects(w.bot.handle(p), Crash, what);
    assert.equal(onDisk(path).posts[p.id].state, "acted", `${what}: the action is on disk as done`);
    assert.equal(relayedFor(w, p.id).length, 1);
    sink.gone = false;
    await w.restart(JsonStore.open(path)).recover();
    assert.equal(w.store.state.posts[p.id].result, "ok", what);
    assert.equal(relayedFor(w, p.id).length, 1, `${what}: not sent again`);
    assert.deepEqual(repliesTo(w, p.id).map((r) => r.id), [`dry-${p.id}-0`], `${what}: one reply, under the same id`);
    assert.equal(w.store.state.posts[p.id].replies, 1);
    if (command !== "stop") await settle(w);
    cases++;
  }
}

// ---- a transaction the table refuses, with the process killed before it heard ----
{
  // A bankroll of 60 FANG takes bets of 3 at most; the bot can't know that before it sends.
  const { w, path, account } = await world(false, { bankroll: String(60n * F) });
  const p = post("@BlockulaChain deal 5");
  w.table.fault = "after-submit";
  await assert.rejects(w.bot.handle(p), Crash);
  assert.equal(onDisk(path).posts[p.id].state, "sent");
  await w.restart(JsonStore.open(path)).recover();
  assert.equal(w.store.state.posts[p.id].result, "refused:table:48");
  assert.deepEqual(repliesTo(w, p.id).map((r) => r.text), ["That's above the maximum bet right now."]);
  assert.equal((await w.table.player(account)).balance, 100n * F);
  assert.equal(w.table.house_.nextGame, 0n);
  cases++;
}

// ---- a hand that settles on the deal (a natural), with the transaction forgotten: the last hand's seed is the post's ----
{
  let found = false;
  for (let i = 0; i < 400 && !found; i++) {
    const { w, path } = await world(false);
    const p = post("@BlockulaChain deal 5");
    w.table.fault = "after-submit";
    await assert.rejects(w.bot.handle(p), Crash);
    w.table.forgetTxs();
    await w.restart(JsonStore.open(path)).recover();
    assert.equal(w.store.state.posts[p.id].result, "ok");
    assert.equal(repliesTo(w, p.id).length, 1);
    assert.equal(w.table.house_.nextGame, 1n);
    if (w.table.games.size === 0) {
      found = true;
      assert.match(repliesTo(w, p.id)[0].text, /^Hand 0: /);
      assert.deepEqual(w.store.state.hands, {});
    }
  }
  assert.ok(found, "a hand that settled on its deal");
  cases++;
}

// ---- the dealer's timeout, killed after it ran ----
{
  const { w, path, account } = await world(false, { dealerDelay: null });
  const p = post("@BlockulaChain deal 5");
  assert.equal((await w.bot.handle(p)).result, "ok");
  assert.match(repliesTo(w, p.id)[0].text, /hasn't dealt yet/);
  w.clock.advance(w.table.msPastDeadline(0n));
  w.table.fault = "after-submit";
  await assert.rejects(w.bot.tick(), Crash);
  assert.equal(onDisk(path).posts["timeout-0"].state, "sent");
  const bot = w.restart(JsonStore.open(path));
  await bot.recover();
  await bot.tick();
  await bot.tick();
  assert.equal(w.table.settledHands.length, 1, "the hand timed out once");
  assert.deepEqual(repliesTo(w, p.id).map((r) => r.meta.action), ["deal", "timeout"]);
  assert.equal((await w.table.player(account)).balance, 95n * F + 12_500_000_000n, "the stake back and 1.5 × it");
  assert.equal(w.store.state.posts["timeout-0"].state, "done");
  cases++;
}

// ---- the file itself ----
{
  const path = join(dir, "file.json");
  const a = JsonStore.open(path);
  a.state.enrolled.push("boo1test");
  a.commit();
  a.state.enrolled.push("never-committed");
  assert.deepEqual(JsonStore.open(path).state.enrolled, ["boo1test"], "what wasn't committed isn't there");
  // A commit that died half-way leaves its temporary file and the old state whole.
  writeFileSync(`${path}.tmp`, '{"version":1,"posts":{"half');
  assert.deepEqual(JsonStore.open(path).state.enrolled, ["boo1test"]);
  a.commit();
  assert.deepEqual(JsonStore.open(path).state.enrolled, ["boo1test", "never-committed"]);
  writeFileSync(path, "not json");
  assert.throws(() => JsonStore.open(path), /isn't JSON/);
  writeFileSync(path, '{"version":2}');
  assert.throws(() => JsonStore.open(path), /version 1/);
  // No key, seed or secret is ever in it.
  assert.ok(!/seed|secret|"key"/i.test(readFileSync(join(dir, "store-0.json"), "utf8")));
}

rmSync(dir, { recursive: true, force: true });
console.log(`✓ store: ${cases} crashes, each post handled exactly once after the restart`);
