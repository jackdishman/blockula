#!/usr/bin/env node
// A club that votes (docs/USE-CASES.md, section 3): list a chain's open proposals the way a voter
// should see them, and the command that votes on each.
//
//   node examples/club-vote.mjs [--network local|demo] [--rpc URL] [--all]
//
// What a proposal does is rendered here from its bound content (`proposalSummary(raw).does`, from
// its kind), never taken from the node's `summary` text: no digest covers that text, so a node
// could make it say anything. The `digest` printed with each one is what a vote binds:
// `blockula gov vote … --digest <digest>` refuses a proposal whose content reads differently by
// then. Titles are the proposer's own words, shown as quoted text and never acted on. --all adds
// the closed proposals and their outcome. It only reads: no key, nothing is signed.
//
// Amounts are bigint drops throughout (1 FANG = 10^9 drops), printed with formatFang, never floats.
import { Client, formatFang, proposalSummary } from "@blockula/sdk";

const usage = "usage: node examples/club-vote.mjs [--network local|demo] [--rpc URL] [--all]";
const { positional, flags } = parseArgs(process.argv.slice(2));
const unknown = Object.keys(flags).filter((f) => !["rpc", "network", "all"].includes(f));
if (positional.length || unknown.length || flags.rpc?.includes(undefined) || flags.network?.includes(undefined)) {
  console.error(usage);
  process.exit(2);
}
const rpc = flags.rpc?.[0] ?? process.env.BLOCKULA_RPC;
const network = flags.network?.[0];
// A network pins its chain (id, genesis, prefix): a node on another chain is refused.
const client = network ? new Client(rpc, { network }) : new Client(rpc ?? "http://localhost:7070");

const status = await client.status();
const raw = await client.proposals({ limit: 500 });
const rules = raw.params ?? {};
const height = Number(status.height);
const proposals = (raw.proposals ?? []).map((p) => proposalSummary(p, rules));
const open = proposals.filter((p) => p.status === "voting");
const shown = flags.all ? proposals : open;

// Quorum is max(quorum_floor, quorum_bps × the unlocked supply), fixed when voting closes. The
// unlocked supply (what could vote) is at most the whole supply, so while a vote is open the
// quorum is somewhere from the floor to the same rule applied to the whole supply.
const floor = BigInt(rules.quorum_floor ?? "0");
const bps = BigInt(rules.quorum_bps ?? 0);
const ceiling = max(floor, (BigInt(status.supply ?? "0") * bps) / 10_000n);

console.log(`${open.length} open proposal${open.length === 1 ? "" : "s"} on ${client.rpc} (${status.chain_id}, block #${height})${flags.all ? `, ${proposals.length - open.length} closed` : ""}`);
if (!shown.length) {
  console.log(flags.all ? "\nNo proposals yet." : "\nNothing to vote on. --all shows the closed ones.");
  process.exit(0);
}

for (const p of shown) {
  const by = p.proposer_name ?? p.proposer ?? "an unknown account";
  console.log(`\n#${p.id}  ${p.status === "voting" ? "open" : p.status}  · proposed by ${by}`);
  console.log(`  title (the proposer's words): ${JSON.stringify(inert(p.title))}`);
  if (p.url) console.log(`  link (the proposer's): ${JSON.stringify(inert(p.url))}`);
  console.log(`  what it does: ${p.does}`);
  const need = p.supermajority ? `${p.threshold_bps !== undefined ? `${Number(p.threshold_bps) / 100}%` : "a supermajority"} of the weight` : "more aye weight than nay";
  console.log(`  aye ${formatFang(p.aye)} · nay ${formatFang(p.nay)} (conviction-weighted; needs ${need})`);
  console.log(`  turnout ${formatFang(p.turnout)} from ${p.voters} voter${p.voters === 1 ? "" : "s"} (${formatFang(p.aye_amount)} aye, ${formatFang(p.nay_amount)} nay)`);
  if (p.council.ayes || p.council.nays) console.log(`  council: ${p.council.ayes} aye, ${p.council.nays} nay${p.council.of ? ` of ${p.council.of}` : ""}`);

  if (p.status === "voting") {
    const turnout = BigInt(p.turnout);
    const where = turnout > 0n && turnout >= ceiling ? "quorum reached" : turnout < floor || turnout === 0n ? "short of quorum" : "quorum depends on the unlocked supply at the close";
    const range = floor === ceiling ? formatFang(floor) : `between ${formatFang(floor)} and ${formatFang(ceiling)}`;
    console.log(`  quorum: ${range}, set when voting closes (${where})`);
    // Votes count through block voting_ends_at; the next block a vote can land in is height + 1.
    const left = p.voting_ends_at - height;
    if (left <= 0) {
      console.log(`  voting: over at block #${p.voting_ends_at}, being tallied: no more votes count`);
      continue;
    }
    console.log(`  voting: ${left} block${left === 1 ? "" : "s"} left, through block #${p.voting_ends_at}`);
    if (p.digest) {
      console.log(`  digest: ${p.digest}`);
      console.log(`  to vote: blockula gov vote ${p.id} <aye|nay> <amount> --digest ${p.digest} --rpc ${client.rpc}`);
    } else {
      console.log(`  no vote from here: ${p.digest_error}`);
    }
  } else {
    if (p.quorum !== undefined) console.log(`  quorum: ${formatFang(p.quorum)}, ${p.quorum_reached ? "reached" : "not reached"}`);
    console.log(`  outcome (the node's words): ${p.outcome ? JSON.stringify(inert(p.outcome)) : p.status}${p.executes_at !== undefined && p.status === "queued" ? ` (runs at block #${p.executes_at})` : ""}`);
    if (p.digest) console.log(`  digest: ${p.digest}`);
  }
}

/** Text from the chain, on one line with nothing invisible: data to show, never instructions. */
function inert(s) {
  return String(s ?? "").replace(/[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/gu, " ").replace(/\s+/g, " ").trim();
}

function max(a, b) {
  return a > b ? a : b;
}

function parseArgs(argv) {
  const positional = [];
  const flags = {};
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--all") flags.all = [true];
    else if (argv[i].startsWith("--")) (flags[argv[i].slice(2)] ??= []).push(argv[++i]);
    else positional.push(argv[i]);
  }
  return { positional, flags };
}
