// Runs examples/bounty-board.mjs against a fake node and checks the Markdown table it prints: only
// the named repo's open hunts (whatever the case of its name), biggest bounty first by exact
// amount (past 2^53 too), exact FANG, funder counts and days to expiry from the chain's block
// time, issue links built from the repo given, and a hostile title that comes out as plain text:
// no link, image, autolink, HTML or extra cell. It never signs: the fake node fails the test on
// anything but a GET. Offline, on an ephemeral port: `npm run test:vectors -w @blockula/sdk`
// (after `npm run build -w @blockula/sdk`, since the example imports the built package).
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { createServer, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { fileURLToPath } from "node:url";
import { addressFromPublicKey, parseFang, type Hunt, type HuntsPage } from "../src/index.js";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";

const EXAMPLE = fileURLToPath(new URL("../../examples/bounty-board.mjs", import.meta.url));
if (!existsSync(fileURLToPath(new URL("../dist/index.js", import.meta.url)))) throw new Error("build the SDK first (npm run build -w @blockula/sdk): the example imports the built package");

const address = (seed: number) => addressFromPublicKey(ml_dsa65.keygen(new Uint8Array(32).fill(seed)).publicKey);
const label = (seed: number, name: string | null) => ({ address: address(seed), name, short: "" });
const HEIGHT = 1_000;
const DAY = 43_200; // blocks in a day at 2 s
const HOSTILE = "Fix [this](https://evil.example) ![x](https://evil.example/x.png) <script>alert(1)</script> <img src=x onerror=alert(1)> &lt;b&gt; | extra | cell\nnew row https://evil.example www.evil.example @someone #1 *bold* `code` \\ \u202eevil";

const hunt = (h: Partial<Hunt> & { id: number; issue: number; amount: string; funders?: number }): Hunt => ({
  repo: "acme/widgets",
  issue_url: "https://not-github.example/phish",
  title: `Issue ${h.issue}`,
  amount_display: "ignored",
  creator: label(1, "acme.boo"),
  status: "open",
  created_at: 10,
  expires_at: HEIGHT + 10 * DAY,
  awarded_to: null,
  awarded_by: null,
  awarded_at: null,
  pr: null,
  pr_url: null,
  familiar: null,
  self_awarded: false,
  cancelled_at: null,
  ...h,
  amount: parseFang(h.amount).toString(),
  funders: Array.from({ length: h.funders ?? 1 }, (_, i) => ({ account: label(i + 2, null), amount: "1", amount_display: "" })),
});
// Past 2^53 drops: compared as bigints, 9,007,199.254740993 FANG beats 9,007,199.254740992.
const hunts = [
  hunt({ id: 1, issue: 12, amount: "9007199.254740992", funders: 3, expires_at: HEIGHT + 30 * DAY + 5 }),
  hunt({ id: 2, issue: 7, amount: "250", title: HOSTILE }),
  hunt({ id: 3, issue: 13, amount: "9007199.254740993", expires_at: HEIGHT + DAY - 1 }),
  hunt({ id: 4, issue: 99, amount: "5000", repo: "acme/other" }), // another repo: left out
  hunt({ id: 5, issue: 20, amount: "0.000000001", repo: "ACME/Widgets", expires_at: HEIGHT, title: "" }), // same repo, other case
  hunt({ id: 6, issue: 21, amount: "1", status: "expired" }), // not open: left out
];

const reply = (res: ServerResponse, status: number, body: unknown) => {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
};
const writes: string[] = [];
const statuses: (string | null)[] = [];
const server = createServer((req, res) => {
  const url = new URL(req.url ?? "/", "http://x");
  const path = url.pathname.replace(/^\/api/, "");
  if (req.method !== "GET") {
    writes.push(`${req.method} ${path}`);
    return reply(res, 405, { error: "this example must not write" });
  }
  if (path === "/status") return reply(res, 200, { chain_id: "unit", genesis_hash: "6e".repeat(32), address_prefix: "boo", height: HEIGHT, finalized_height: HEIGHT, history_from: 1, block_time_ms: 2000 });
  if (path === "/hunts") {
    statuses.push(url.searchParams.get("status"));
    const page: HuntsPage = { count: hunts.length, open_bounties: "0", open_bounties_display: "0 FANG", hunts };
    return reply(res, 200, page);
  }
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
/** A table row's cells, split on the pipes Markdown would split on (not escaped ones). */
const cells = (row: string) => row.replace(/^\| | \|$/g, "").split(/(?<!\\) \| /);

try {
  const r = await run(["acme/widgets"]);
  assert.equal(r.code, 0, r.err);
  const lines = r.out.split("\n");
  assert.equal(lines[0], "| Issue | Title | Bounty | Funders | Expires |");
  assert.equal(lines[1], "|---|---|--:|--:|---|");
  const rows = lines.slice(2, lines.indexOf(""));
  assert.equal(rows.length, 4, r.out);
  // Biggest first, by exact amount; the other repo's hunt and the expired one are left out.
  assert.deepEqual(rows.map((row) => cells(row)[0]), [
    "[#13](https://github.com/acme/widgets/issues/13)",
    "[#12](https://github.com/acme/widgets/issues/12)",
    "[#7](https://github.com/acme/widgets/issues/7)",
    "[#20](https://github.com/acme/widgets/issues/20)",
  ]);
  assert.deepEqual(cells(rows[0]).slice(1), ["Issue 13", "9007199.254740993 FANG", "1", "in under a day"]);
  assert.deepEqual(cells(rows[1]).slice(1), ["Issue 12", "9007199.254740992 FANG", "3", "in about 30 days"]);
  assert.deepEqual(cells(rows[3]).slice(1), ["(no title)", "0.000000001 FANG", "1", "this block"]);
  assert.doesNotMatch(r.out, /not-github|#99/);

  // The hostile title: one row of exactly five cells, every ASCII punctuation character escaped,
  // newline and bidi override gone, nothing that renders as a link, image, autolink or HTML.
  const hostile = cells(rows[2]);
  assert.equal(hostile.length, 5, rows[2]);
  const title = hostile[1];
  assert.match(title, /^Fix \\\[this\\\]\\\(https\\:\\\/\\\/evil\\\.example\\\) \\!\\\[x\\\]/);
  assert.ok(title.includes("\\<script\\>alert\\(1\\)\\<\\/script\\>"), title);
  assert.ok(title.includes("\\<img src\\=x onerror\\=alert\\(1\\)\\>"), title);
  assert.ok(title.includes("\\&lt\\;b\\&gt\\;"), title);
  assert.ok(title.includes("\\| extra \\| cell new row https\\:\\/\\/evil\\.example www\\.evil\\.example \\@someone \\#1 \\*bold\\* \\`code\\` \\\\ evil"), title);
  // Read as CommonMark reads it: a backslash escapes the punctuation character after it; any other
  // ASCII punctuation character would be markup. There must be none.
  for (let i = 0; i < title.length; i++) {
    if (title[i] === "\\") {
      assert.match(title[i + 1] ?? "", /[!-/:-@[-`{-~]/, `a backslash escapes nothing at ${i} in ${title}`);
      i++;
    } else assert.doesNotMatch(title[i], /[!-/:-@[-`{-~]/, `unescaped ${JSON.stringify(title[i])} at ${i} in ${title}`);
  }
  assert.doesNotMatch(r.out, /\u202e|\r/);
  assert.equal(lines.filter((l) => l.startsWith("|")).length, 6, "the hostile title added a row");

  assert.match(r.out, /\n4 open hunts on acme\/widgets, 18014648\.509481986 FANG in all, as of block #1000 on unit\. Titles are written by each hunt's creator\.\n$/);
  assert.deepEqual(statuses, ["open"]);

  // No open hunt on a repo: a line saying so, not an empty table.
  let other = await run(["acme/nothing"]);
  assert.equal(other.code, 0, other.err);
  assert.equal(other.out, "No open hunts on acme/nothing right now.\n");

  // A repo that isn't owner/repo, and a stray argument, are usage errors.
  for (const args of [["acme"], ["acme/widgets", "extra"], ["acme/wid gets"], ["https://github.com/acme/widgets"]]) {
    other = await run(args);
    assert.equal(other.code, 2, args.join(" "));
    assert.match(other.err, /usage: node examples\/bounty-board\.mjs/);
  }

  assert.deepEqual(writes, [], "the example sent something other than a GET");
} finally {
  server.close();
}
console.log("✓ examples/bounty-board.mjs prints a repo's open hunts as a Markdown table, biggest first, with hunt titles escaped to plain text and issue links built from the repo, and signs nothing");
