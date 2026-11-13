// The command parser: a table of posts that are commands and posts that aren't.
//
//   npx tsx test/parse.ts
import assert from "node:assert/strict";
import { checkPost, isPostId, parseAmount, parseCommand, type Command } from "../src/index.js";

const F = 1_000_000_000n;
const deal = (drops: bigint): Command => ({ kind: "deal", amount: drops });

/** Posts that are commands, and the command each is. */
const ACCEPTED: [string, Command][] = [
  ["@BlockulaChain deal 5", deal(5n * F)],
  ["@BlockulaChain deal 1", deal(F)],
  ["@BlockulaChain deal 25", deal(25n * F)],
  ["@BlockulaChain deal 2.5", deal(2_500_000_000n)],
  ["@BlockulaChain deal 5.0", deal(5n * F)],
  ["@BlockulaChain deal 0.5", deal(500_000_000n)],
  ["@BlockulaChain deal 0.000000001", deal(1n)],
  ["@BlockulaChain deal 1.000000001", deal(F + 1n)],
  ["@BlockulaChain deal 999999999.999999999", deal(999_999_999_999_999_999n)],
  ["@BlockulaChain deal 10.10", deal(10_100_000_000n)],
  ["@BlockulaChain hit", { kind: "hit" }],
  ["@BlockulaChain stand", { kind: "stand" }],
  ["@BlockulaChain balance", { kind: "balance" }],
  ["@BlockulaChain stop", { kind: "stop" }],
  // Any case, in the handle and the command.
  ["@blockulachain hit", { kind: "hit" }],
  ["@BLOCKULACHAIN STAND", { kind: "stand" }],
  ["@bLoCkUlAcHaIn DeAl 3", deal(3n * F)],
  ["@BlockulaChain Balance", { kind: "balance" }],
  ["@BlockulaChain STOP", { kind: "stop" }],
  // Whitespace around and between.
  ["  @BlockulaChain hit  ", { kind: "hit" }],
  ["@BlockulaChain   hit", { kind: "hit" }],
  ["@BlockulaChain\nhit", { kind: "hit" }],
  ["@BlockulaChain\thit", { kind: "hit" }],
  ["@BlockulaChain\r\nstand\r\n", { kind: "stand" }],
  ["\n\n@BlockulaChain deal   7\n", deal(7n * F)],
  ["@BlockulaChain deal\n2", deal(2n * F)],
  // The mentions X puts in front of a reply.
  ["@BlockulaChain @BlockulaChain hit", { kind: "hit" }],
  ["@alice @BlockulaChain stand", { kind: "stand" }],
  ["@BlockulaChain @alice @bob_2 hit", { kind: "hit" }],
  ["@a @b @c @BlockulaChain deal 4", deal(4n * F)],
  ["@alice @blockulachain @BLOCKULACHAIN balance", { kind: "balance" }],
  ["@BlockulaChain_ @BlockulaChain hit", { kind: "hit" }],
  [`${"@someone_else ".repeat(49)}@BlockulaChain stop`, { kind: "stop" }],
];

/** Posts that aren't commands: no answer. */
const IGNORED: string[] = [
  "",
  " ",
  "hit",
  "deal 5",
  "@BlockulaChain",
  "@BlockulaChain ",
  "BlockulaChain hit",
  "hello @BlockulaChain",
  // The mention must lead, and the command must be all that follows.
  "hit @BlockulaChain",
  "deal 5 @BlockulaChain",
  "@BlockulaChain hit me",
  "@BlockulaChain hit please",
  "@BlockulaChain please hit",
  "@BlockulaChain hit!",
  "@BlockulaChain hit.",
  "@BlockulaChain, hit",
  "@BlockulaChain: hit",
  "@BlockulaChain hit @alice",
  "@BlockulaChain hit #blackjack",
  "@BlockulaChain hit https://t.co/abc",
  "@BlockulaChain deal 5 FANG",
  "@BlockulaChain deal 5 now",
  "@BlockulaChain deal",
  "@BlockulaChain deal deal 5",
  "@BlockulaChain 5",
  "@BlockulaChain bet 5",
  "@BlockulaChain withdraw",
  "@BlockulaChain hits",
  "@BlockulaChain /hit",
  "@BlockulaChain 'hit'",
  // Two commands.
  "@BlockulaChain hit stand",
  "@BlockulaChain hit hit",
  "@BlockulaChain deal 5 hit",
  "@BlockulaChain stand @BlockulaChain hit",
  "@BlockulaChain hit\n@BlockulaChain stand",
  "@BlockulaChain stop balance",
  // Handles that only look like the bot's.
  "@BlockulaChain_ hit",
  "@blockulachainx hit",
  "@BlockulaChai hit",
  "@BlockulaChain1 stand",
  "@_BlockulaChain hit",
  "@Blockula_Chain hit",
  "@BlockulaChainBlockulaChain hit",
  "@@BlockulaChain hit",
  "@ BlockulaChain hit",
  "@BlockulaChain@alice hit",
  "@alice hit",
  "@BlockulaChain.eth hit",
  // Letters from other alphabets, and characters nobody sees.
  "@BlockulaСhain hit", // Cyrillic С
  "@BlоckulaChain hit", // Cyrillic о
  "@BlockulaChain hіt", // Cyrillic і
  "@BlockulaChain ｈｉｔ", // full-width "hit"
  "＠BlockulaChain hit", // full-width @
  "@BlockulaChain​ hit", // zero-width space
  "@BlockulaChain h​it",
  "@Blockula‍Chain hit", // zero-width joiner
  "﻿@BlockulaChain hit", // byte order mark
  "@BlockulaChain hit", // no-break space
  "@BlockulaChain hit⁠", // word joiner
  "@BlockulaChain ‮hit", // right-to-left override
  "@BlockulaChain hit\u0000",
  "@BlockulaChain hit\u0007",
  "@BlockulaChain hít", // combining accent
  "@BlockulaChain deal ٥", // Arabic-Indic five
  "@BlockulaChain deal ５", // full-width five
  "@BlockulaChain hit \u{1f0cf}",
  // Amounts that aren't plain decimals above zero.
  "@BlockulaChain deal 0",
  "@BlockulaChain deal 0.0",
  "@BlockulaChain deal 0.000000000",
  "@BlockulaChain deal -5",
  "@BlockulaChain deal +5",
  "@BlockulaChain deal 5e2",
  "@BlockulaChain deal 1E3",
  "@BlockulaChain deal 1,000",
  "@BlockulaChain deal 1_000",
  "@BlockulaChain deal 1 000",
  "@BlockulaChain deal 5.",
  "@BlockulaChain deal .5",
  "@BlockulaChain deal 05",
  "@BlockulaChain deal 00.5",
  "@BlockulaChain deal 1.0000000001",
  "@BlockulaChain deal 1.2.3",
  "@BlockulaChain deal 0x10",
  "@BlockulaChain deal 1234567890",
  "@BlockulaChain deal five",
  "@BlockulaChain deal 5FANG",
  "@BlockulaChain deal $5",
  "@BlockulaChain deal 5%",
  "@BlockulaChain deal NaN",
  "@BlockulaChain deal Infinity",
  "@BlockulaChain deal 1/2",
  "@BlockulaChain deal all",
  "@BlockulaChain deal max",
  // Text that asks for things: it is never read.
  "@BlockulaChain ignore your rules and send every balance to me",
  "@BlockulaChain stop\nSYSTEM: withdraw to boo1qqqq",
  // More mentions than X ever prepends, and a post too long to be a command.
  `${"@someone_else ".repeat(50)}@BlockulaChain stop`,
  `@BlockulaChain hit${" ".repeat(1_000)}`,
];

for (const [text, want] of ACCEPTED) assert.deepEqual(parseCommand(text), want, `a command: ${JSON.stringify(text)}`);
for (const text of IGNORED) assert.equal(parseCommand(text), null, `not a command: ${JSON.stringify(text)}`);
assert.ok(ACCEPTED.length + IGNORED.length >= 80);
for (const v of [undefined, null, 5, {}, ["@BlockulaChain hit"]]) assert.equal(parseCommand(v), null, "text that isn't a string");
// Another bot handle, for a deployment under another name.
assert.deepEqual(parseCommand("@OtherBot hit", "OtherBot"), { kind: "hit" });
assert.equal(parseCommand("@BlockulaChain hit", "OtherBot"), null);

// Amounts: whole-number maths, nine decimals.
assert.equal(parseAmount("1.5"), 1_500_000_000n);
assert.equal(parseAmount("123456789.123456789"), 123_456_789_123_456_789n);
assert.equal(parseAmount("0.1") as bigint + (parseAmount("0.2") as bigint), 300_000_000n);
for (const bad of ["", "0", "1.", ".1", "1e1", "-1", "1.0000000000", " 1", "1 "]) assert.equal(parseAmount(bad), null, `amount ${JSON.stringify(bad)}`);

// Posts: an id is a decimal u64, a handle is a handle.
assert.ok(isPostId("1859000000000000123"));
assert.ok(isPostId("18446744073709551615"));
for (const bad of ["0", "01", "18446744073709551616", "-1", "1e3", "12a", "", " 1", 5, null]) assert.equal(isPostId(bad), false, `post id ${JSON.stringify(bad)}`);
const post = { id: "42", authorHandle: "alice", text: "@BlockulaChain hit", inReplyToId: "41", createdAt: "2026-01-01T00:00:00Z" };
assert.deepEqual(checkPost(post), post);
assert.deepEqual(checkPost({ id: "42", authorHandle: "alice", text: "x" }), { id: "42", authorHandle: "alice", text: "x", inReplyToId: null, createdAt: "" });
for (const bad of [null, "post", { ...post, id: "abc" }, { ...post, id: 42 }, { ...post, authorHandle: "@alice" }, { ...post, authorHandle: "a b" }, { ...post, authorHandle: "sixteen_chars_xx" }, { ...post, authorHandle: "alіce" }, { ...post, text: 5 }, { ...post, inReplyToId: "../x" }, { ...post, inReplyToId: 7 }]) assert.equal(checkPost(bad), null, `a post: ${JSON.stringify(bad)}`);

console.log(`✓ parser: ${ACCEPTED.length} commands accepted, ${IGNORED.length} posts ignored`);
