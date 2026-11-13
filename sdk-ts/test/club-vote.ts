// Runs examples/club-vote.mjs against a fake node and checks what it shows a voter: what each open
// proposal does rendered from its kind (never the node's summary, which this node lies in), the
// title as quoted text, exact tallies and turnout, quorum's range while open, blocks left, the
// digest a vote binds (checked against proposalDigest here) and the command that votes with it;
// no command for one this client can't read or whose voting is over; --all adds closed ones with
// their outcome. It never signs: the fake node fails the test on anything but a GET.
// Offline, on an ephemeral port: `npm run test:vectors -w @blockula/sdk` (after
// `npm run build -w @blockula/sdk`, since the example imports the built package).
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { createServer, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { fileURLToPath } from "node:url";
import { addressFromPublicKey, parseFang, proposalDigest, type ProposalKind } from "../src/index.js";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";

const EXAMPLE = fileURLToPath(new URL("../../examples/club-vote.mjs", import.meta.url));
if (!existsSync(fileURLToPath(new URL("../dist/index.js", import.meta.url)))) throw new Error("build the SDK first (npm run build -w @blockula/sdk): the example imports the built package");

const address = (seed: number) => addressFromPublicKey(ml_dsa65.keygen(new Uint8Array(32).fill(seed)).publicKey);
const [MINA, LUCY, VLAD] = [address(1), address(2), address(3)];
const LIE = "Nothing to see here, vote aye";
const HEIGHT = 100;

const side = (amount: string, weight: string) => ({ amount: { drops: parseFang(amount).toString() }, weight: { drops: parseFang(weight).toString() } });
const proposal = (p: Record<string, unknown>) => ({
  url: "",
  summary: LIE,
  deposit: parseFang("1000").toString(),
  status: "voting",
  created_at: 10,
  execute_at: null,
  supermajority: false,
  council: [],
  tally: null,
  outcome: "",
  ayes: side("0", "0"),
  nays: side("0", "0"),
  voters: 0,
  ...p,
});

const thursdays = { kind: { type: "text" } as ProposalKind, title: "Move meetings\nto Thursdays?‮", url: "https://example.com/club/42" };
const spend = { kind: { type: "treasury_spend", to: LUCY, amount: parseFang("2.5").toString() } as ProposalKind, title: "Pay for the hall" };
const proposals = [
  // Newest first, as the node serves them.
  proposal({ id: 4, kind: { type: "text" }, title: "Too late", proposer: { address: VLAD, name: "vlad.boo" }, voting_end: HEIGHT }),
  proposal({ id: 3, kind: { type: "mystery_kind" }, title: "Unreadable", proposer: { address: VLAD, name: "vlad.boo" }, voting_end: 300 }),
  proposal({ id: 2, ...thursdays, proposer: { address: MINA, name: "mina.boo" }, voting_end: 150, ayes: side("30.5", "61"), nays: side("9.5", "0.95"), voters: 3 }),
  proposal({ id: 1, ...spend, proposer: { address: LUCY, name: null }, voting_end: 400, supermajority: true }),
  proposal({
    id: 0,
    kind: { type: "param_change", key: "max_validators", value: 40 },
    title: "Room for more validators",
    proposer: { address: VLAD, name: "vlad.boo" },
    status: "executed",
    voting_end: 60,
    execute_at: 70,
    ayes: side("50", "50"),
    voters: 2,
    tally: { quorum: { drops: parseFang("20").toString() }, quorum_reached: true },
    outcome: "ran: max_validators is now 40",
  }),
];
const params = { quorum_floor: parseFang("20").toString(), quorum_bps: 400, supermajority_bps: 6667 };

const reply = (res: ServerResponse, status: number, body: unknown) => {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
};
const writes: string[] = [];
let served = proposals;
const server = createServer((req, res) => {
  const path = new URL(req.url ?? "/", "http://x").pathname.replace(/^\/api/, "");
  if (req.method !== "GET") {
    writes.push(`${req.method} ${path}`);
    return reply(res, 405, { error: "this example must not write" });
  }
  // 1,000 FANG supplied: 4% of it is 40 FANG, so an open vote's quorum is between 20 and 40 FANG.
  if (path === "/status") return reply(res, 200, { chain_id: "unit", genesis_hash: "6e".repeat(32), address_prefix: "boo", height: HEIGHT, finalized_height: HEIGHT, history_from: 1, supply: parseFang("1000").toString() });
  if (path === "/proposals") return reply(res, 200, { count: served.length, params, proposals: served });
  return reply(res, 404, { error: `fake node: no route for ${path}` });
});
await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
const rpc = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;

const run = (args: string[]) =>
  new Promise<{ code: number; out: string; err: string }>((resolve) => {
    execFile(process.execPath, [EXAMPLE, ...args, "--rpc", rpc], { timeout: 60_000, env: { ...process.env, BLOCKULA_RPC: "" } }, (error, out, err) => {
      resolve({ code: error ? (typeof error.code === "number" ? error.code : -1) : 0, out, err });
    });
  });
/** The block of output for proposal #id. */
const block = (out: string, id: number) => {
  const start = out.indexOf(`\n#${id} `);
  assert.ok(start >= 0, `no proposal #${id} in:\n${out}`);
  const end = out.indexOf("\n#", start + 1);
  return out.slice(start + 1, end < 0 ? undefined : end);
};

try {
  let r = await run([]);
  assert.equal(r.code, 0, r.err);
  assert.match(r.out, /^4 open proposals on http:\/\/127\.0\.0\.1:\d+ \(unit, block #100\)\n/);
  // The node's summary is never shown: what each does comes from its kind.
  assert.ok(!r.out.includes(LIE), `the node's summary leaked into:\n${r.out}`);
  assert.doesNotMatch(r.out, /#0 /); // closed: only with --all

  const two = block(r.out, 2);
  const digest2 = proposalDigest(thursdays.kind, thursdays.title, thursdays.url);
  assert.match(two, /^#2 {2}open {2}· proposed by mina\.boo\n/);
  // The title is the proposer's text, on one line and quoted, with nothing invisible left in it.
  assert.ok(two.includes(`  title (the proposer's words): "Move meetings to Thursdays?"\n`), two);
  assert.ok(two.includes(`  link (the proposer's): "https://example.com/club/42"\n`), two);
  assert.ok(two.includes("  what it does: Nothing on chain: a text proposal (a signal; read its title and link)\n"), two);
  assert.ok(two.includes("  aye 61 FANG · nay 0.95 FANG (conviction-weighted; needs more aye weight than nay)\n"), two);
  assert.ok(two.includes("  turnout 40 FANG from 3 voters (30.5 FANG aye, 9.5 FANG nay)\n"), two);
  assert.ok(two.includes("  quorum: between 20 FANG and 40 FANG, set when voting closes (quorum reached)\n"), two);
  assert.ok(two.includes("  voting: 50 blocks left, through block #150\n"), two);
  assert.ok(two.includes(`  digest: ${digest2}\n`), two);
  assert.ok(two.includes(`  to vote: blockula gov vote 2 <aye|nay> <amount> --digest ${digest2} --rpc ${rpc}\n`), two);

  // A spend: rendered in full from the kind (amount and recipient), the proposer by address since
  // it has no name, and a supermajority's threshold from the rules.
  const one = block(r.out, 1);
  const digest1 = proposalDigest(spend.kind, spend.title, "");
  assert.ok(one.startsWith(`#1  open  · proposed by ${LUCY}\n`), one);
  assert.ok(one.includes(`  what it does: Pay 2.5 FANG from the treasury to ${LUCY}\n`), one);
  assert.ok(one.includes("(conviction-weighted; needs 66.67% of the weight)"), one);
  assert.ok(one.includes("  quorum: between 20 FANG and 40 FANG, set when voting closes (short of quorum)\n"), one);
  assert.ok(one.includes(`--digest ${digest1} `), one);
  assert.doesNotMatch(one, /link \(/);

  // One this client can't read: no digest, so no command to vote on it.
  const three = block(r.out, 3);
  assert.match(three, /what it does: Something this client can't read/);
  assert.match(three, /no vote from here: /);
  assert.doesNotMatch(three, /to vote:|digest: [0-9a-f]{64}/);

  // Voting over (its last block is the head): tallied next, nothing to vote with.
  const four = block(r.out, 4);
  assert.match(four, /voting: over at block #100, being tallied: no more votes count/);
  assert.doesNotMatch(four, /to vote:/);

  // --all adds the closed one, with the quorum it had and its outcome.
  r = await run(["--all"]);
  assert.equal(r.code, 0, r.err);
  assert.match(r.out, /^4 open proposals on .*, 1 closed\n/);
  const zero = block(r.out, 0);
  assert.ok(zero.startsWith("#0  executed  · proposed by vlad.boo\n"), zero);
  assert.ok(zero.includes("  what it does: Set the chain parameter max_validators to 40"), zero);
  assert.ok(zero.includes("  quorum: 20 FANG, reached\n"), zero);
  assert.ok(zero.includes(`  outcome (the node's words): "ran: max_validators is now 40"\n`), zero);
  assert.doesNotMatch(zero, /to vote:/);
  assert.ok(!r.out.includes(LIE), r.out);

  // Nothing open.
  served = proposals.slice(-1);
  r = await run([]);
  assert.equal(r.code, 0, r.err);
  assert.match(r.out, /^0 open proposals on .*\n\nNothing to vote on\. --all shows the closed ones\.\n$/);

  // A stray argument is a usage error, not ignored.
  r = await run(["0"]);
  assert.equal(r.code, 2);
  assert.match(r.err, /usage: node examples\/club-vote\.mjs/);

  assert.deepEqual(writes, [], "the example sent something other than a GET");
} finally {
  server.close();
}
console.log("✓ examples/club-vote.mjs shows each open proposal from its bound content (never the node's summary), with exact tallies, quorum's range, blocks left and the digest a vote binds, and signs nothing");
