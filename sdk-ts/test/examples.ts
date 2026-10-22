// Runs examples/pay-from-a-script.mjs against a fake node and checks what it tells the operator
// when a node refuses the payment. A node that no longer holds a tx that ran (it pruned the block,
// or synced past it) refuses it again as "bad nonce", so the account's nonce decides what the
// refusal means. Offline, on an ephemeral port: `npm run test:vectors -w @blockula/sdk` (after
// `npm run build -w @blockula/sdk`, since the example imports the built package).
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { Client, Wallet, addressFromPublicKey, txId, type Tx } from "../src/index.js";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";

const EXAMPLE = fileURLToPath(new URL("../../examples/pay-from-a-script.mjs", import.meta.url));
if (!existsSync(fileURLToPath(new URL("../dist/index.js", import.meta.url)))) throw new Error("build the SDK first (npm run build -w @blockula/sdk): the example imports the built package");

const FANG = 1_000_000_000n;
const SHOP = addressFromPublicKey(ml_dsa65.keygen(new Uint8Array(32).fill(7)).publicKey);
const fees = { height: 50, lag: 17, slots: [], suggested: { flat: "10000", rate: "10000" }, lowest: { flat: "10000", rate: "10000" }, tip_cap_multiple: 20, account_creation_fee: "100000000" };

/** The fake node's state; each case sets what it needs. */
const node = {
  height: 50,
  accounts: new Map<string, { balance: bigint; nonce: number }>(),
  /** What the next POST /tx answers: a refusal (and the nonce the sender's account has after it), or admission. */
  answer: { status: 400, error: "insufficient balance" } as { status: number; error?: string; nonceAfter?: number },
  /** Txs this node holds (admitted or finalized), by id. */
  held: new Map<string, Tx>(),
  accountsDown: false,
};

function reply(res: ServerResponse, status: number, body: unknown) {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
}

async function body(req: IncomingMessage): Promise<any> {
  let text = "";
  for await (const chunk of req) text += chunk;
  return JSON.parse(text);
}

const server = createServer(async (req, res) => {
  const path = new URL(req.url ?? "/", "http://x").pathname.replace(/^\/api/, "");
  if (path === "/status") return reply(res, 200, { chain_id: "unit", genesis_hash: "6e".repeat(32), address_prefix: "boo", height: node.height, finalized_height: node.height, max_tx_validity_blocks: 3600, history_from: 1 });
  if (path === "/fees") return reply(res, 200, fees);
  if (req.method === "POST" && path === "/faucet") {
    const { to } = await body(req);
    node.accounts.set(to, { balance: 100n * FANG, nonce: 0 });
    return reply(res, 200, { id: "fa".repeat(32) });
  }
  if (req.method === "POST" && path === "/tx") {
    const tx: Tx = await body(req);
    const { status, error, nonceAfter } = node.answer;
    if (nonceAfter !== undefined) node.accounts.get(tx.from)!.nonce = nonceAfter;
    if (status !== 200) return reply(res, status, { error });
    node.held.set(txId(tx), tx);
    return reply(res, 200, { id: txId(tx), status: "pending" });
  }
  const tx = path.match(/^\/tx\/([0-9a-f]{64})$/);
  if (tx) {
    if (tx[1] === "fa".repeat(32)) return reply(res, 200, { id: tx[1], status: "finalized", summary: "faucet", height: node.height });
    const held = node.held.get(tx[1]);
    if (!held) return reply(res, 404, { error: "transaction not found" });
    return reply(res, 200, { id: tx[1], status: "finalized", summary: "paid the shop", height: node.height, fee: held.fee, fee_display: "0.00006 FANG", action: held.action });
  }
  const account = path.match(/^\/account\/([^/]+)$/);
  if (account) {
    if (node.accountsDown) return reply(res, 500, { error: "storage error" });
    const a = node.accounts.get(account[1]);
    const balance = (a?.balance ?? 0n).toString();
    return reply(res, 200, { account: { address: account[1], name: null }, balance, transferable: balance, locked: "0", exists: !!a, creation_fee: a ? "0" : fees.account_creation_fee, nonce: a?.nonce ?? 0, next_nonce: a?.nonce ?? 0, recent_txs: [] });
  }
  return reply(res, 404, { error: `fake node: no route for ${path}` });
});
await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
const rpc = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
const dir = mkdtempSync(join(tmpdir(), "blockula-example-"));
const outbox = join(dir, "outbox.jsonl");

/** Runs the example (async: this process serves the fake node meanwhile). */
const run = (args: string[]) =>
  new Promise<{ code: number; out: string; err: string }>((resolve) => {
    execFile(process.execPath, [EXAMPLE, ...args, "--rpc", rpc, "--outbox", outbox], { cwd: dir, timeout: 60_000, env: { ...process.env, BLOCKULA_RPC: "" } }, (error, out, err) => {
      resolve({ code: error ? (typeof error.code === "number" ? error.code : -1) : 0, out, err });
    });
  });

/** A payment from a funded wallet, signed with nonce 0 and stored in the outbox, as the example's first run leaves it. */
async function stored(): Promise<Tx> {
  const payer = Wallet.generate(new Client(rpc));
  node.accounts.set(payer.address, { balance: 100n * FANG, nonce: 0 });
  const tx = await payer.build({ type: "transfer", to: SHOP, amount: (FANG / 2n).toString() }, "order-7");
  writeFileSync(outbox, JSON.stringify({ id: txId(tx), tx, at: new Date().toISOString() }) + "\n");
  return tx;
}

try {
  // The first post, refused with the nonce unused: nothing was sent.
  node.answer = { status: 400, error: "insufficient balance" };
  let r = await run([SHOP, "1", "--faucet"]);
  assert.equal(r.code, 1, r.err);
  assert.match(r.err, /refused: insufficient balance\. Nothing was sent; tx [0-9a-f]{64} can't run from this node, and the next payment takes its nonce\./);

  // The first post, refused because another tx from the account used its nonce: this one can never run.
  node.answer = { status: 400, error: "bad nonce: expected 1, got 0", nonceAfter: 1 };
  r = await run([SHOP, "1", "--faucet"]);
  assert.equal(r.code, 1, r.err);
  assert.match(r.err, /Another tx from boo1\w+ has used nonce 0, so tx [0-9a-f]{64} can never run\. Nothing was paid/);

  // A rebroadcast to a node that no longer holds the tx, whose nonce has been used: it may have
  // run, so the operator is told never to sign it again before finding out, not that nothing was sent.
  let tx = await stored();
  node.answer = { status: 400, error: "bad nonce: expected 1, got 0", nonceAfter: 1 };
  r = await run(["--rebroadcast", txId(tx)]);
  assert.equal(r.code, 1, r.err);
  assert.match(r.err, new RegExp(`But nonce 0 of ${tx.from} has been used: tx ${txId(tx)} ran, or another tx took its nonce\\. Either way it can never run again\\. Don't sign this payment again until you know which`));
  assert.doesNotMatch(r.err, /Nothing was (sent|paid)|pay again/);

  // A rebroadcast refused with the nonce unused, within its validity window: another node may still run it.
  tx = await stored();
  node.answer = { status: 400, error: "fee below the base fee" };
  r = await run(["--rebroadcast", txId(tx)]);
  assert.equal(r.code, 1, r.err);
  assert.match(r.err, new RegExp(`Nonce 0 is still unused, so tx ${txId(tx)} hasn't run, and this node won't run it\\. A node that took it earlier still could, until block ${tx.valid_until}: pay again only after that`));

  // The same, past its valid_until: it never ran and never will.
  node.height = tx.valid_until + 1;
  r = await run(["--rebroadcast", txId(tx)]);
  assert.equal(r.code, 1, r.err);
  assert.match(r.err, new RegExp(`the chain is past block ${tx.valid_until}, its valid_until \\(by this node's account\\), so tx ${txId(tx)} never ran and never will\\. You can pay again\\.`));
  node.height = 50;

  // The nonce can't be read: unknown, so no advice to pay again.
  node.accountsDown = true;
  r = await run(["--rebroadcast", txId(tx)]);
  node.accountsDown = false;
  assert.equal(r.code, 1, r.err);
  assert.match(r.err, /nonce couldn't be read \(storage error\), so whether tx [0-9a-f]{64} ran is unknown\. Don't sign this payment again until you know/);

  // A node that still holds it answers with the same id: the receipt, nothing paid twice.
  tx = await stored();
  node.held.set(txId(tx), tx);
  node.answer = { status: 200 };
  r = await run(["--rebroadcast", txId(tx)]);
  assert.equal(r.code, 0, r.err);
  assert.match(r.out, new RegExp(`✓ paid the shop\\n  tx ${txId(tx)}, block #50`));
} finally {
  server.close();
  rmSync(dir, { recursive: true, force: true });
}
console.log("✓ examples/pay-from-a-script.mjs reads the account's nonce on a refusal: a used nonce on a rebroadcast is 'it may have run, never re-sign', an unused one says until when it could still run");
