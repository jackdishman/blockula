// From a handle to an account: only a link whose proof post checks out counts, exactly one
// account may hold a handle, a check is kept for a while and then made again. The check is the
// local stand-in: nothing here (or anywhere in the package) asks X.
//
//   npx tsx test/links.ts
import assert from "node:assert/strict";
import { DEFAULT_CONFIG, FakeClock, FakeTable, Links, MemoryStore, localVerifier, type LinkClaim, type Post, type VerifyLink } from "../src/index.js";
import { World } from "./world.js";

const F = 1_000_000_000n;
const TTL = DEFAULT_CONFIG.linkTtlMs;
const EVERY = DEFAULT_CONFIG.listEveryMs;

function setup() {
  const clock = new FakeClock();
  const table = new FakeTable({ seed: "links", clock });
  const store = new MemoryStore();
  let checks = 0;
  let broken = false;
  const inner = table.verifier();
  const verify: VerifyLink = async (claim: LinkClaim) => {
    checks++;
    if (broken) throw new Error("the check can't be made right now");
    return inner(claim);
  };
  const links = new Links(table, store, verify, clock, DEFAULT_CONFIG);
  const join = (name: string) => {
    const account = table.account(name);
    table.deposit(account, 10n * F);
    table.grant(account, 5n * F, 10n * F, 1_000n);
    return account;
  };
  return { clock, table, store, links, join, checks: () => checks, breakCheck: (on: boolean) => (broken = on) };
}

// ---- one account, a proof by the handle: it resolves, whatever the handle's case ----
{
  const { clock, table, links, join, checks } = setup();
  const mina = join("mina");
  table.linkWithProof(mina, "Mina_H", "111");
  assert.deepEqual(await links.resolve("Mina_H"), { kind: "ok", account: mina });
  assert.deepEqual(await links.resolve("mina_h"), { kind: "ok", account: mina });
  assert.deepEqual(await links.resolve("MINA_H"), { kind: "ok", account: mina });
  assert.equal(checks(), 1, "checked once, then kept");
  assert.deepEqual(await links.resolve("Nobody_Here"), { kind: "none" });
  // A look-alike handle is another handle.
  assert.deepEqual(await links.resolve("Mina_H_"), { kind: "none" });
  clock.advance(TTL - 1);
  assert.deepEqual(await links.resolve("Mina_H"), { kind: "ok", account: mina });
  assert.equal(checks(), 1);
  // Past its time the link is read and checked again.
  clock.advance(1);
  assert.deepEqual(await links.resolve("Mina_H"), { kind: "ok", account: mina });
  assert.equal(checks(), 2);
  // The proof post is gone (deleted): at the next check the link no longer holds.
  table.posts.delete("111");
  assert.deepEqual(await links.resolve("Mina_H"), { kind: "ok", account: mina }, "still within the kept check");
  clock.advance(TTL);
  assert.deepEqual(await links.resolve("Mina_H"), { kind: "unverified" });
  // Linked again with a new post: found at the next listing.
  table.linkWithProof(mina, "Mina_H", "112");
  clock.advance(EVERY);
  assert.deepEqual(await links.resolve("Mina_H"), { kind: "ok", account: mina });
  // Unlinked: gone when the kept check runs out.
  table.unlink(mina);
  clock.advance(TTL);
  assert.deepEqual(await links.resolve("Mina_H"), { kind: "none" });
}

// ---- a claim without a good proof is not a link ----
{
  const { clock, table, links, join } = setup();
  const a = join("a");
  const b = join("b");
  const c = join("c");
  table.linkWithProof(a, "Lucy_W", "201", "SomeoneElse"); // the post is by another handle
  table.setLink(b, "quincey_m", "202"); // no such post
  table.setLink(c, "Renfield77", "203");
  table.posts.set("203", { handle: "Renfield77", text: "a post without the code" });
  for (const h of ["Lucy_W", "quincey_m", "Renfield77"]) assert.deepEqual(await links.resolve(h), { kind: "unverified" }, h);
  // The code is the account's own: another account's proof post doesn't serve.
  const d = join("d");
  const proof = table.setLink(a, "VanHelsing", "204");
  table.posts.set("204", { handle: "VanHelsing", text: proof.code });
  table.setLink(d, "ArthurHolm", "204");
  clock.advance(EVERY);
  assert.deepEqual(await links.resolve("ArthurHolm"), { kind: "unverified" });
}

// ---- one handle, two accounts ----
{
  const { clock, table, links, join } = setup();
  const [a, b, squatter] = [join("a"), join("b"), join("squatter")];
  table.linkWithProof(a, "Twin_Acct", "301");
  assert.deepEqual(await links.resolve("Twin_Acct"), { kind: "ok", account: a });
  // A second account proves the same handle: neither is played.
  table.linkWithProof(b, "twin_acct", "302");
  clock.advance(TTL);
  assert.deepEqual(await links.resolve("Twin_Acct"), { kind: "conflict" });
  assert.deepEqual(await links.resolve("TWIN_ACCT"), { kind: "conflict" });
  // One unlinks: the other is played again.
  table.unlink(a);
  clock.advance(TTL);
  assert.deepEqual(await links.resolve("Twin_Acct"), { kind: "ok", account: b });
  // An account that only claims the handle (its proof is someone else's post) locks nobody out.
  table.linkWithProof(squatter, "Twin_Acct", "303", "SomeoneElse");
  clock.advance(TTL);
  assert.deepEqual(await links.resolve("Twin_Acct"), { kind: "ok", account: b });
}

// ---- a check that can't be made is "not verified", and is made again ----
{
  const { clock, table, links, join, checks, breakCheck } = setup();
  const a = join("a");
  table.linkWithProof(a, "Mina_H", "401");
  breakCheck(true);
  assert.deepEqual(await links.resolve("Mina_H"), { kind: "unverified" });
  breakCheck(false);
  assert.deepEqual(await links.resolve("Mina_H"), { kind: "unverified" }, "not listed again at once");
  assert.equal(checks(), 1);
  clock.advance(EVERY);
  assert.deepEqual(await links.resolve("Mina_H"), { kind: "ok", account: a });
}

// ---- who is in the registry: accounts that granted this relayer, and accounts enrolled by hand ----
{
  const { clock, table, store, links, join, checks } = setup();
  const late = table.account("late");
  table.linkWithProof(late, "Lucy_W", "501");
  assert.deepEqual(await links.resolve("Lucy_W"), { kind: "none" }, "linked, but never granted this relayer");
  join("late");
  assert.deepEqual(await links.resolve("Lucy_W"), { kind: "none" }, "the table isn't listed again for every post");
  clock.advance(EVERY);
  assert.deepEqual(await links.resolve("Lucy_W"), { kind: "ok", account: late });
  // A grant to another relayer doesn't enroll an account here.
  const other = table.account("other");
  table.linkWithProof(other, "quincey_m", "502");
  table.grant(other, 5n * F, 10n * F, 1_000n, table.account("another relayer"));
  clock.advance(TTL);
  assert.deepEqual(await links.resolve("quincey_m"), { kind: "none" });
  // An operator can enroll one by hand.
  store.state.enrolled.push(other);
  clock.advance(EVERY);
  assert.deepEqual(await links.resolve("quincey_m"), { kind: "ok", account: other });
  // Unknown handles can't make the bot list and check over and over.
  const before = checks();
  for (let i = 0; i < 100; i++) assert.deepEqual(await links.resolve(`stranger_${i}`), { kind: "none" });
  assert.equal(checks(), before);
}

// ---- through the bot: each case's reply, and no play without a checked link ----
{
  const grant = { maxBet: String(25n * F), dailyCap: String(200n * F), expiresIn: "40000" };
  const w = new World("links-bot", {
    dealerDelay: 0,
    players: [
      { name: "good", handle: "Mina_H", link: "verified", deposit: String(50n * F), grant },
      { name: "bad", handle: "Lucy_W", link: "wrong-author", deposit: String(50n * F), grant },
      { name: "t1", handle: "Twin_Acct", link: "verified", deposit: String(50n * F), grant },
      { name: "t2", handle: "Twin_Acct", link: "verified", deposit: String(50n * F), grant },
      { name: "enrolled", handle: "quincey_m", link: "verified", deposit: String(50n * F), grant: null },
    ],
  });
  w.store.state.enrolled.push(w.table.account("enrolled"));
  let n = 0;
  const say = async (handle: string, text: string) => {
    w.clock.advance(5_000);
    const post: Post = { id: String(1_820_000_000_000_000_000n + BigInt(n++)), authorHandle: handle, text, inReplyToId: null, createdAt: "" };
    const r = await w.bot.handle(post);
    return [r.result, r.replies[0]?.text ?? null];
  };
  assert.deepEqual((await say("Mina_H", "@BlockulaChain deal 5"))[0], "ok");
  assert.match((await say("Lucy_W", "@BlockulaChain deal 5")).join(" "), /^refused:unverified An account names this X handle, but its proof post didn't check out/);
  assert.match((await say("Twin_Acct", "@BlockulaChain deal 5")).join(" "), /^refused:conflict Two accounts have linked this X handle/);
  assert.match((await say("Nobody_Here", "@BlockulaChain deal 5")).join(" "), /^refused:no_link I can't find a Blockula account/);
  // Linked and checked, but no grant: found (enrolled by hand), and refused for the grant.
  assert.match((await say("quincey_m", "@BlockulaChain deal 5")).join(" "), /^refused:no_grant Your account hasn't granted this bot/);
  // Only the one good link was ever played.
  assert.deepEqual([...new Set(w.table.relayed.map((e) => e.player))], [w.table.account("good")]);
  for (const name of ["bad", "t1", "t2", "enrolled"]) assert.equal((await w.table.player(w.table.account(name))).balance, 50n * F, `${name}'s balance is untouched`);
}

// ---- the dry run's stand-in over local posts ----
{
  const claim = { handle: "Mina_H", post_id: "9", code: "blockula-x-" + "ab".repeat(20) };
  const verify = localVerifier([{ id: "9", handle: "mina_h", text: `Linking: ${claim.code}` }, { id: "10", handle: "Mina_H", text: "no code here" }]);
  assert.deepEqual(await verify(claim), { verified: true });
  assert.deepEqual(await verify({ ...claim, post_id: "10" }), { verified: false });
  assert.deepEqual(await verify({ ...claim, post_id: "11" }), { verified: false });
  assert.deepEqual(await verify({ ...claim, handle: "Lucy_W" }), { verified: false });
}

console.log("✓ links: checked, kept and checked again; unverified claims, two accounts on one handle, enrollment");
