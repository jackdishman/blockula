// Rate limits and cooldowns, on a clock the test moves: one command a handle every few seconds,
// so many an hour, so many a minute over everyone; and refusal replies rationed apart from that,
// so the bot can't be made to post over and over.
//
//   npx tsx test/limits.ts
import assert from "node:assert/strict";
import { MemoryStore, type BotConfig, type Post } from "../src/index.js";
import { World, type PlayerSetup } from "./world.js";

const F = 1_000_000_000n;
const player = (name: string, handle: string): PlayerSetup => ({ name, handle, link: "verified", deposit: String(100n * F), grant: { maxBet: String(25n * F), dailyCap: String(200n * F), expiresIn: "40000" } });
const HANDLES = ["Mina_H", "jonathan_h", "VanHelsing", "Lucy_W", "Renfield77", "quincey_m"];
let seed = 0;
const world = (config: Partial<BotConfig> = {}) => new World(`limits-${seed++}`, { dealerDelay: 0, players: HANDLES.map((h, i) => player(`p${i}`, h)), config });
let n = 0;
const post = (handle: string, text: string): Post => ({ id: String(1_810_000_000_000_000_000n + BigInt(n++)), authorHandle: handle, text, inReplyToId: null, createdAt: "" });
const say = async (w: World, handle: string, text = "@BlockulaChain balance") => {
  const r = await w.bot.handle(post(handle, text));
  return `${r.result}/${r.replies.length}`;
};

// ---- the cooldown: one command a handle every 3 seconds ----
{
  const w = world();
  assert.equal(await say(w, "Mina_H"), "ok/1");
  w.clock.advance(1_000);
  assert.equal(await say(w, "Mina_H"), "ignored:rate/0", "1 s later");
  assert.equal(await say(w, "MINA_H"), "ignored:rate/0", "the same handle in another case");
  assert.equal(await say(w, "Lucy_W"), "ok/1", "another handle isn't held back");
  w.clock.advance(1_999);
  assert.equal(await say(w, "Mina_H"), "ignored:rate/0", "2.999 s after the last one handled");
  w.clock.advance(1);
  assert.equal(await say(w, "Mina_H"), "ok/1", "3 s after");
  // A post held back is recorded: delivered again later, it still isn't handled.
  const held = post("Mina_H", "@BlockulaChain deal 5");
  assert.equal((await w.bot.handle(held)).result, "ignored:rate");
  w.clock.advance(60_000);
  assert.equal((await w.bot.handle(held)).result, "ignored:duplicate");
  assert.equal(w.table.house_.nextGame, 0n, "and no hand was dealt for it");
  // Posts that aren't commands don't count.
  for (let i = 0; i < 50; i++) assert.equal(await say(w, "Mina_H", "@BlockulaChain hello"), "ignored:not a command/0");
  assert.equal(await say(w, "Mina_H"), "ok/1");
}

// ---- so many an hour ----
{
  const w = world({ perHour: 5 });
  for (let i = 0; i < 5; i++) {
    assert.equal(await say(w, "Mina_H"), "ok/1", `command ${i + 1} of the hour`);
    w.clock.advance(10_000);
  }
  assert.equal(await say(w, "Mina_H"), "ignored:rate/0", "the sixth");
  assert.equal(await say(w, "Lucy_W"), "ok/1");
  // An hour after the first, one more fits.
  w.clock.advance(3_600_000 - 50_000);
  assert.equal(await say(w, "Mina_H"), "ok/1");
  assert.equal(await say(w, "Mina_H"), "ignored:rate/0");
}

// ---- so many a minute, over everyone ----
{
  const w = world({ globalPerMinute: 4 });
  for (const h of HANDLES.slice(0, 4)) assert.equal(await say(w, h), "ok/1");
  assert.equal(await say(w, HANDLES[4]), "ignored:rate/0", "the fifth command of the minute");
  w.clock.advance(59_999);
  assert.equal(await say(w, HANDLES[5]), "ignored:rate/0");
  w.clock.advance(1);
  assert.equal(await say(w, HANDLES[5]), "ok/1", "a minute later");
}

// ---- the limits live in the store: a restart doesn't reset them ----
{
  const w = world({ perHour: 2 });
  assert.equal(await say(w, "Mina_H"), "ok/1");
  w.restart(new MemoryStore(JSON.parse(JSON.stringify(w.store.state))));
  w.clock.advance(1_000);
  assert.equal(await say(w, "Mina_H"), "ignored:rate/0", "the cooldown, after a restart");
  w.clock.advance(5_000);
  assert.equal(await say(w, "Mina_H"), "ok/1");
  w.restart(new MemoryStore(JSON.parse(JSON.stringify(w.store.state))));
  w.clock.advance(5_000);
  assert.equal(await say(w, "Mina_H"), "ignored:rate/0", "the hour's count, after a restart");
}

// ---- refusals: a reply at most once a minute a handle, six an hour; the rest are silent ----
{
  const w = world({ cooldownMs: 0 });
  assert.equal(await say(w, "Nobody_Here", "@BlockulaChain deal 5"), "refused:no_link/1");
  for (let i = 0; i < 20; i++) {
    w.clock.advance(2_000);
    assert.equal(await say(w, "Nobody_Here", "@BlockulaChain deal 5"), "refused:no_link/0", "within the minute: refused, not answered");
  }
  // Another handle's refusal is its own.
  assert.equal(await say(w, "Nobody_Else", "@BlockulaChain hit"), "refused:no_link/1");
  w.clock.advance(20_000);
  assert.equal(await say(w, "Nobody_Here", "@BlockulaChain stop"), "refused:no_link/1", "a minute after the last reply");
  let replies = 2;
  for (let i = 0; i < 10; i++) {
    w.clock.advance(60_000);
    const got = await say(w, "Nobody_Here");
    if (got === "refused:no_link/1") replies++;
    else assert.equal(got, "refused:no_link/0");
  }
  assert.equal(replies, 6, "six refusal replies in the hour, however many posts");
  w.clock.advance(3_600_000);
  assert.equal(await say(w, "Nobody_Here"), "refused:no_link/1", "the next hour");
  // A linked player's refusals are rationed the same way, and their good commands still answer.
  assert.equal(await say(w, "Mina_H", "@BlockulaChain deal 26"), "refused:limits/1");
  w.clock.advance(1_000);
  assert.equal(await say(w, "Mina_H", "@BlockulaChain deal 27"), "refused:limits/0");
  assert.equal(await say(w, "Mina_H"), "ok/1");
  assert.equal(w.sink.replies.filter((r) => r.meta.handle === "Nobody_Here").length, 7);
}

// ---- a handle the operator blocked: nothing, not even a record ----
{
  const w = world({ blocked: ["mina_h"] });
  const p = post("Mina_H", "@BlockulaChain deal 5");
  assert.equal((await w.bot.handle(p)).result, "ignored:blocked");
  assert.equal(w.store.state.posts[p.id], undefined);
  assert.equal(await say(w, "Lucy_W"), "ok/1");
}

console.log("✓ limits: the cooldown, the hour, the minute over everyone, refusal replies rationed, kept across a restart");
