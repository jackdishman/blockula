#!/usr/bin/env node
// A team's bounty board (docs/USE-CASES.md, section 4): a repo's open Hunts as a Markdown table to
// paste into a README or append to a GitHub Actions job summary.
//
//   node examples/bounty-board.mjs <owner/repo> [--network local|demo] [--rpc URL]
//
// One row per open hunt on that repo, biggest bounty first: the issue (linked), its title, the
// bounty, how many funded it, and about how long until it expires (from the chain's block time).
// It only reads: no key, nothing is signed.
//
// Hunt titles are chain text anyone who creates a hunt writes. Every ASCII punctuation character in
// one is backslash-escaped, which CommonMark renders as that plain character, so a title can't make
// a link, an image, an autolink, HTML or an extra table cell. The issue link is built here from the
// repo you named and the hunt's issue number, not taken from the node. Amounts are bigint drops
// (1 FANG = 10^9 drops), printed with formatFang, never floats.
import { Client, formatFang } from "@blockula/sdk";

const usage = "usage: node examples/bounty-board.mjs <owner/repo> [--network local|demo] [--rpc URL]";
const { positional, flags } = parseArgs(process.argv.slice(2));
const unknown = Object.keys(flags).filter((f) => !["rpc", "network"].includes(f));
const repo = positional[0];
if (positional.length !== 1 || unknown.length || Object.values(flags).flat().includes(undefined) || !/^[A-Za-z0-9-]{1,39}\/[A-Za-z0-9._-]{1,100}$/.test(repo ?? "")) {
  console.error(usage);
  console.error("Name one GitHub repo as owner/repo (jackdishman/blockula).");
  process.exit(2);
}
const rpc = flags.rpc?.[0] ?? process.env.BLOCKULA_RPC;
const network = flags.network?.[0];
// A network pins its chain (id, genesis, prefix): a node on another chain is refused.
const client = network ? new Client(rpc, { network }) : new Client(rpc ?? "http://localhost:7070");

const status = await client.status();
const page = await client.hunts("open");
const height = BigInt(status.height);
const blockMs = BigInt(status.block_time_ms ?? 0);
const NODE_LIMIT = 100; // /api/hunts returns at most this many

const mine = page.hunts
  .filter((h) => h.status === "open" && typeof h.repo === "string" && h.repo.toLowerCase() === repo.toLowerCase())
  .filter((h) => {
    if (Number.isSafeInteger(h.issue) && h.issue > 0) return true;
    console.error(`(left out hunt #${h.id}: the node gave no usable issue number)`);
    return false;
  })
  .sort((a, b) => (BigInt(b.amount) > BigInt(a.amount) ? 1 : BigInt(b.amount) < BigInt(a.amount) ? -1 : a.id - b.id));
if (page.hunts.length >= NODE_LIMIT) console.error(`(${client.rpc} lists at most ${NODE_LIMIT} open hunts, the biggest first: smaller ones on ${repo} may be missing)`);

if (!mine.length) {
  console.log(`No open hunts on ${repo} right now.`);
  process.exit(0);
}

console.log("| Issue | Title | Bounty | Funders | Expires |");
console.log("|---|---|--:|--:|---|");
let total = 0n;
for (const h of mine) {
  total += BigInt(h.amount);
  const issue = `[#${h.issue}](https://github.com/${repo}/issues/${h.issue})`;
  console.log(`| ${issue} | ${markdownText(h.title) || "(no title)"} | ${formatFang(BigInt(h.amount))} | ${h.funders.length} | ${expiresIn(h.expires_at)} |`);
}
console.log("");
console.log(`${mine.length} open hunt${mine.length === 1 ? "" : "s"} on ${repo}, ${formatFang(total)} in all, as of block #${height} on ${markdownText(status.chain_id)}. Titles are written by each hunt's creator.`);

/** About how long until a hunt expires: it can be awarded through block `expires_at`. */
function expiresIn(expiresAt) {
  const blocks = BigInt(expiresAt) - height;
  if (blocks <= 0n) return "this block";
  if (blockMs <= 0n) return `in ${blocks} blocks`;
  const days = (blocks * blockMs) / 86_400_000n;
  return days === 0n ? "in under a day" : `in about ${days} day${days === 1n ? "" : "s"}`;
}

/**
 * Chain text as plain Markdown text on one line: control and invisible characters (newlines, bidi
 * overrides) become spaces, then every ASCII punctuation character is backslash-escaped.
 */
function markdownText(s) {
  return String(s ?? "")
    .replace(/[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/gu, " ")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/[!-/:-@[-`{-~]/g, "\\$&");
}

function parseArgs(argv) {
  const positional = [];
  const flags = {};
  for (let i = 0; i < argv.length; i++) {
    if (argv[i].startsWith("--")) (flags[argv[i].slice(2)] ??= []).push(argv[++i]);
    else positional.push(argv[i]);
  }
  return { positional, flags };
}
