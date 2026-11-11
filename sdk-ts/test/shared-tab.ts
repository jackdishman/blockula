// Runs examples/shared-tab.mjs against a fake node and checks the tab it prints: transfers between
// members only (not to outsiders, not failed ones, not before --from), each counted once though both
// sides' history lists it, read across several pages, net balances in exact FANG, and the fewest
// payments that settle them (here 3, where settling largest debt to largest credit would take 4).
// Offline, on an ephemeral port: `npm run test:vectors -w @blockula/sdk` (after
// `npm run build -w @blockula/sdk`, since the example imports the built package).
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { createServer, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { fileURLToPath } from "node:url";
import { addressFromPublicKey, parseFang } from "../src/index.js";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";

const EXAMPLE = fileURLToPath(new URL("../../examples/shared-tab.mjs", import.meta.url));
if (!existsSync(fileURLToPath(new URL("../dist/index.js", import.meta.url)))) throw new Error("build the SDK first (npm run build -w @blockula/sdk): the example imports the built package");

const address = (seed: number) => addressFromPublicKey(ml_dsa65.keygen(new Uint8Array(32).fill(seed)).publicKey);
const names = ["alex", "sam", "kai", "jo", "rio"];
const addr = Object.fromEntries(names.map((n, i) => [n, address(i + 1)])) as Record<string, string>;
const OUTSIDER = address(99);
const label = (a: string) => ({ address: a, name: Object.keys(addr).find((n) => addr[n] === a)?.concat(".boo") ?? null });

// The transfers the node has finalized, oldest first.
let n = 0;
const transfer = (from: string, to: string, amount: string, height: number, memo = "", status = "finalized") => ({
  id: (++n).toString(16).padStart(64, "0"),
  kind: "transfer",
  status,
  error: status === "failed" ? "insufficient balance" : null,
  height,
  from: label(from),
  to: label(to),
  to_name: null,
  amount: parseFang(amount).toString(),
  amount_display: `${amount} FANG`,
  memo,
  summary: "a transfer",
});
const txs = [
  transfer(addr.kai, addr.jo, "100", 5, "before the trip"), // left out with --from 10
  transfer(addr.alex, addr.jo, "10", 11, "flat deposit"),
  transfer(addr.sam, addr.rio, "4.5", 12, "groceries"),
  transfer(addr.alex, OUTSIDER, "1", 13, "not the group's"), // to an outsider: left out
  transfer(addr.rio, addr.sam, "3", 14, "", "failed"), // ran and failed: left out
  transfer(addr.kai, addr.rio, "5.1", 15, "train tickets"),
  transfer(addr.jo, addr.alex, "2.75", 16, "paid back, part"),
  { ...transfer(addr.alex, addr.jo, "0", 17), kind: "register_name", to: null, amount: null }, // not a transfer
];

const reply = (res: ServerResponse, status: number, body: unknown) => {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
};
const PAGE = 2; // small pages, so the example has to follow `next`
const server = createServer((req, res) => {
  const url = new URL(req.url ?? "/", "http://x");
  const path = url.pathname.replace(/^\/api/, "");
  if (path === "/status") return reply(res, 200, { chain_id: "unit", genesis_hash: "6e".repeat(32), address_prefix: "boo", height: 50, finalized_height: 50, history_from: 1 });
  const history = path.match(/^\/account\/([^/]+)\/txs$/);
  if (history) {
    const a = decodeURIComponent(history[1]);
    const mine = txs.filter((t) => t.from.address === a || t.to?.address === a).reverse(); // newest first
    const start = Number(url.searchParams.get("before") ?? 0);
    const page = mine.slice(start, start + PAGE);
    return reply(res, 200, { txs: page, next: start + PAGE < mine.length ? String(start + PAGE) : null, history_from: 1 });
  }
  const account = path.match(/^\/account\/([^/]+)$/);
  if (account) {
    const id = decodeURIComponent(account[1]);
    const name = id.replace(/\.boo$/, "");
    if (!addr[name]) return reply(res, 404, { error: `unknown name ${id}` });
    return reply(res, 200, { account: label(addr[name]), balance: "0", exists: true, recent_txs: [] });
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

try {
  // The whole group, from block 10: alex +7.25 and jo -7.25 settle between them, and sam +4.5,
  // kai +5.1 and rio -9.6 among themselves. That's 3 payments; largest debt to largest credit
  // (rio to alex first) would take 4.
  let r = await run(["alex.boo", "sam.boo", "kai.boo", "jo.boo", "rio.boo", "--from", "10"]);
  assert.equal(r.code, 0, r.err);
  assert.match(r.out, /4 transfers between members:/);
  assert.match(r.out, /#11 {2}alex\.boo → jo\.boo {2}10 FANG {2}"flat deposit"/);
  assert.match(r.out, /#16 {2}jo\.boo → alex\.boo {2}2\.75 FANG {2}"paid back, part"/);
  assert.doesNotMatch(r.out, /before the trip|not the group's|#14/);
  for (const line of ["kai.boo is owed 5.1 FANG", "alex.boo is owed 7.25 FANG", "sam.boo is owed 4.5 FANG", "jo.boo owes 7.25 FANG", "rio.boo owes 9.6 FANG"]) assert.ok(r.out.includes(`  ${line}\n`), `missing "${line}" in:\n${r.out}`);
  const settle = r.out.slice(r.out.indexOf("To settle up"));
  assert.match(settle, /^To settle up \(3 payments\):/);
  const payments = [...settle.matchAll(/^ {2}(\S+) pays (\S+) (\S+) FANG$/gm)].map((m) => `${m[1]} ${m[2]} ${m[3]}`).sort();
  assert.deepEqual(payments, ["jo.boo alex.boo 7.25", "rio.boo kai.boo 5.1", "rio.boo sam.boo 4.5"]);
  // Members by name, never by address.
  assert.doesNotMatch(r.out, /boo1/);

  // Without --from, the transfer before the trip counts too: kai is owed 105.1 and jo owes 107.25.
  r = await run(["alex.boo", "sam.boo", "kai.boo", "jo.boo", "rio.boo"]);
  assert.equal(r.code, 0, r.err);
  assert.match(r.out, /5 transfers between members:/);
  assert.ok(r.out.includes("  kai.boo is owed 105.1 FANG\n") && r.out.includes("  jo.boo owes 107.25 FANG\n"), r.out);

  // Two members who never paid each other: nothing to settle. A name given twice counts once.
  r = await run(["sam.boo", "KAI.boo", "sam.boo"]);
  assert.equal(r.code, 0, r.err);
  assert.match(r.out, /^Shared tab for sam\.boo, kai\.boo on /);
  assert.match(r.out, /No transfers between members yet: nobody owes anybody\./);

  // A name nobody holds, and a group of one.
  r = await run(["alex.boo", "nobody.boo"]);
  assert.equal(r.code, 1);
  assert.match(r.err, /nobody\.boo isn't a registered name on /);
  r = await run(["alex.boo"]);
  assert.equal(r.code, 2);
  assert.match(r.err, /usage: node examples\/shared-tab\.mjs/);
} finally {
  server.close();
}
console.log("✓ examples/shared-tab.mjs counts each transfer between members once, across pages, and settles the tab in the fewest payments");
