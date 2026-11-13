// The fixture: 100 scripted threads (test/fixtures/threads.json, written by test/gen-threads.ts)
// replayed through the bot over the in-memory table, a fake clock and the stand-in link check.
// Every reply must be exactly the one in test/fixtures/expected.json, and each thread must hold
// to what's true of any thread: every hand settles, nothing is answered that wasn't a command,
// nobody has two hands open, and the table balances add up.
//
//   npx tsx test/threads.ts
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { Outcome, altText, payout, relaySeed, verifyHand } from "@blockula/blackjack";
import { NOTE, X_LIMIT, bannedIn, parseCommand, xLength, type Post } from "../src/index.js";
import { expectedOf, runThread, type ExpectedReply, type Thread } from "./world.js";

interface ExpectedThread {
  name: string;
  results: [string, string][];
  replies: ExpectedReply[];
  balances: Record<string, string>;
}
const load = <T>(file: string): T => JSON.parse(readFileSync(new URL(`./fixtures/${file}`, import.meta.url), "utf8")) as T;
const threads = load<Thread[]>("threads.json");
const expected = load<ExpectedThread[]>("expected.json");
assert.equal(threads.length, 100);
assert.equal(expected.length, 100);

const seen = { posts: 0, replies: 0, hands: 0, results: new Map<string, number>(), actions: new Map<string, number>(), outcomes: new Set<Outcome>() };
const count = (m: Map<string, number>, k: string) => m.set(k, (m.get(k) ?? 0) + 1);

for (const [i, thread] of threads.entries()) {
  const want = expected[i];
  assert.equal(want.name, thread.name);
  const w = await runThread(thread);
  const where = `${thread.name} (${thread.kind})`;

  // ---- exactly the expected replies, in order: text, alt text, meta ----
  const got = w.sink.replies.map(expectedOf);
  assert.equal(got.length, want.replies.length, `${where}: ${got.length} replies, expected ${want.replies.length}`);
  got.forEach((r, k) => assert.deepEqual(r, want.replies[k], `${where}: reply ${k} (${r.id})`));
  assert.deepEqual(w.results, want.results, `${where}: how each post ended`);

  // ---- every hand settles ----
  assert.equal(w.table.games.size, 0, `${where}: a hand is still open on the table`);
  assert.deepEqual(w.store.state.hands, {}, `${where}: the bot still holds a hand`);
  assert.deepEqual(Object.values(w.store.state.posts).filter((p) => p.state !== "done"), [], `${where}: a post is unfinished`);
  assert.equal(w.table.settledHands.length, Number(w.table.house_.nextGame), `${where}: every hand dealt was settled`);

  // ---- never two open hands for one player ----
  assert.ok(w.table.maxOpenPerPlayer <= 1, `${where}: two hands open at once`);

  // ---- no reply without a command before it, by the handle it answers ----
  const posts = new Map<string, { post: Post; at: number }>();
  thread.steps.forEach((s, at) => {
    if (s.t === "post" && !posts.has(s.post.id)) posts.set(s.post.id, { post: s.post, at });
  });
  for (const r of got) {
    const origin = posts.get(r.meta.inReplyTo);
    assert.ok(origin, `${where}: ${r.id} answers a post that was never handed over`);
    assert.ok(parseCommand(origin.post.text) !== null, `${where}: ${r.id} answers a post that isn't a command`);
    assert.equal(r.meta.handle, origin.post.authorHandle, `${where}: ${r.id} is for the post's author`);
  }
  // A post that isn't a command, a duplicate and a stranger's move get nothing; so does a result the bot marks ignored.
  const answered = new Set(got.map((r) => r.meta.inReplyTo));
  for (const [id, result] of w.results) if (result.startsWith("ignored:")) assert.ok(!answered.has(id) || w.results.some(([other, res]) => other === id && !res.startsWith("ignored:")), `${where}: post ${id} was ignored and answered`);

  // ---- every relayed action is one post's, by the account linked to its author, once ----
  const acted = new Set<bigint>();
  for (const e of w.table.relayed) {
    assert.ok(!acted.has(e.postId), `${where}: post ${e.postId} was acted on twice`);
    acted.add(e.postId);
    const origin = posts.get(String(e.postId));
    assert.ok(origin, `${where}: a relayed ${e.action} for a post nobody made`);
    const cmd = parseCommand(origin.post.text);
    assert.equal(cmd?.kind, e.action === "bet" ? "deal" : e.action === "withdraw" ? "stop" : e.action, `${where}: the post's command is what was relayed`);
    const owner = thread.setup.players.find((p) => p.link === "verified" && p.handle?.toLowerCase() === origin.post.authorHandle.toLowerCase());
    assert.equal(owner && w.table.account(owner.name), e.player, `${where}: the account played is the one the author's handle is linked to`);
  }
  // A handle with no checked link, or one two accounts hold, never moves anything.
  for (const p of thread.setup.players) {
    const twins = thread.setup.players.filter((q) => q.link === "verified" && q.handle?.toLowerCase() === p.handle?.toLowerCase()).length;
    if (p.link !== "verified" || twins > 1) assert.ok(!w.table.relayed.some((e) => e.player === w.table.account(p.name)), `${where}: ${p.name} was played without a single checked link`);
  }

  // ---- the money: every hand's cards and payout recompute, and every balance reconciles ----
  assert.ok(w.table.invariant(), `${where}: balance = bankroll + reserved + stakes + table`);
  let paid = 0n;
  for (const s of w.table.settledHands) {
    const bet = w.table.relayed.find((e) => e.action === "bet" && e.game === s.game);
    assert.ok(bet, `${where}: hand ${s.game} has a relayed bet`);
    assert.equal(s.clientSeed, relaySeed(s.player, bet.postId), `${where}: hand ${s.game}'s seed is its post's`);
    verifyHand(s);
    assert.equal(s.payout, payout(s.stake, s.outcome));
    paid += s.payout;
    seen.outcomes.add(s.outcome);
  }
  let ledgerPaid = 0n;
  for (const p of thread.setup.players) {
    const account = w.table.account(p.name);
    const l = w.table.ledger.get(account) ?? { deposited: 0n, withdrawn: 0n, staked: 0n, paid: 0n };
    const balance = w.table.players.get(account)?.balance ?? 0n;
    assert.equal(balance, l.deposited - l.staked + l.paid - l.withdrawn, `${where}: ${p.name}'s table balance is deposits − stakes + payouts − withdrawals`);
    assert.equal(String(balance), want.balances[p.name], `${where}: ${p.name}'s balance at the end`);
    ledgerPaid += l.paid;
  }
  assert.equal(ledgerPaid, paid, `${where}: what the table balances got is what the hands paid`);

  // ---- every reply: fits a post, no banned word, the note where a hand starts or ends, an alt text with every picture ----
  for (const r of w.sink.replies) {
    assert.ok(xLength(r.text) <= X_LIMIT, `${where}: ${r.id} is ${xLength(r.text)} long`);
    assert.deepEqual(bannedIn(`${r.text}\n${r.image?.alt ?? ""}`), [], `${where}: ${r.id}`);
    const opens = r.meta.action === "deal" && r.image !== null;
    const ends = /^Hand \d+: /.test(r.text) || r.meta.action === "stop";
    assert.equal(r.text.includes(NOTE), opens || ends, `${where}: ${r.id} and the note`);
    if (r.image) assert.ok(r.image.alt.length > 40 && r.image.alt.length <= 1_000, `${where}: ${r.id}'s alt text`);
    // A reply about a hand says its cards in the text too, and has a picture.
    assert.equal(r.image !== null, r.meta.action !== "refused" && r.meta.action !== "balance" && r.meta.action !== "stop", `${where}: ${r.id} has a picture exactly when it shows a hand`);
    // Nothing of the post is echoed: no mention, no link.
    assert.ok(!/@|https?:/.test(r.text), `${where}: ${r.id}`);
    count(seen.actions, r.meta.action + (r.meta.reason ? `:${r.meta.reason}` : ""));
  }
  // The alt texts are the real ones, of the snapshots the bot asked pictures of.
  const pictured = w.sink.replies.filter((r) => r.image);
  assert.equal(w.snapshots.length, pictured.length);
  for (const s of w.snapshots) assert.ok(pictured.some((r) => r.image?.alt === altText(s)));

  seen.posts += thread.steps.filter((s) => s.t === "post").length;
  seen.replies += got.length;
  seen.hands += w.table.settledHands.length;
  for (const [, result] of w.results) count(seen.results, result);
}

// ---- the fixture covers what it's meant to ----
for (const result of ["ok", "ignored:not a command", "ignored:duplicate", "ignored:not the hand's player", "ignored:stopped", "ignored:rate", "refused:no_link", "refused:unverified", "refused:conflict", "refused:no_grant", "refused:expired", "refused:paused", "refused:hand_open", "refused:limits", "refused:daily", "refused:balance", "refused:no_hand", "refused:thread", "refused:waiting", "refused:finish_first"]) assert.ok((seen.results.get(result) ?? 0) >= 3, `the fixture has posts that end ${result}`);
for (const action of ["deal", "hit", "stand", "balance", "stop", "update", "timeout", "settled"]) assert.ok((seen.actions.get(action) ?? 0) >= 3, `the fixture has ${action} replies`);
for (const o of Object.values(Outcome)) assert.ok(seen.outcomes.has(o), `the fixture has a hand that ended with outcome ${o}`);
assert.ok(seen.posts >= 1_000 && seen.hands >= 150);

console.log(`✓ threads: 100 threads, ${seen.posts} posts, ${seen.replies} replies matched exactly, ${seen.hands} hands settled and verified`);
