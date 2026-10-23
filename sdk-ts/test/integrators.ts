// Offline tests of what integrators lean on (DEV-1–4, DEV-7, DEV-14, TR-3, TR-5, TR-6): network
// presets and the blockula-1 pin, honest balances, the per-wallet nonce queue and `sendMany`,
// `quote` and the opening burn, fee headroom, paged blocks and history, and `watchPayments`.
// Against a fake node behind fetch that keeps nonces, a mempool and blocks the way node/src
// does. No node needed: `npm run test:vectors -w @blockula/sdk`.
import assert from "node:assert/strict";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { hexToBytes } from "@noble/hashes/utils.js";
import {
  ApiError, ChainMismatchError, Client, HistoryPrunedError, MAINNET_CHAIN_ID, MIN_NEW_ACCOUNT, MainnetNotAllowedError, NETWORKS, Wallet,
  addressFromPublicKey, baseFee, formatFang, parseFang, paymentsIn, signingBytes, txBytes, txId, watchPayments, type Payment, type Tx,
} from "../src/index.js";
import { KNOWN_CLOCKS, KNOWN_GENESIS, KNOWN_NETWORKS, TESTNET_RPC } from "../../mcp/src/network.js";

const FANG = 1_000_000_000n;
const GENESIS = "6e".repeat(32);
const pk = (byte: number) => ml_dsa65.keygen(new Uint8Array(32).fill(byte)).publicKey;
const BOB = addressFromPublicKey(pk(3));
const NEWBIE = addressFromPublicKey(pk(4));

// ---- a fake node: accounts with nonces, a mempool that counts toward next_nonce, blocks ----

interface Acct { balance: bigint; nonce: number; locked?: bigint }
interface Seen { tx: Tx; status: "pending" | "finalized" | "failed"; dropped?: boolean; error?: string; height?: number; reads: number }
interface Node {
  status: Record<string, unknown>;
  accounts: Map<string, Acct>;
  /** Pending txs, per sender, in nonce order. */
  pool: Map<string, Tx[]>;
  seen: Map<string, Seen>;
  /** Every POST /tx body, in order (the same bytes twice when resubmitted). */
  posts: Tx[];
  /** What the next POSTs answer instead of admitting: an HTTP status (with an error), in turn. */
  script: { status: number; error: string; retryAfter?: string }[];
  /** `next_nonce` without the mempool (a node that hasn't seen the wallet's pending txs yet). */
  lagging?: boolean;
  /** How many reads of a pending tx before it finalizes. */
  finalizeAfter: number;
  /** Most txs pending at once. */
  maxPending: number;
  blocks: Map<number, Record<string, unknown>>;
  /** Paged history (node-relay's `/api/account/{id}/txs`): absent on older nodes. */
  pagedHistory: boolean;
  routes: Record<string, unknown>;
  requests: string[];
  contentTypes: string[];
  /** Answers every request with this HTTP status (a node that's down behind a proxy, rate limiting). */
  failWith?: number;
  /** Blocks answered 404 once (a machine behind a load balancer that isn't there yet). */
  notYet?: Set<number>;
  /** Heights left out of the next `/blocks` page (a machine behind a load balancer that lags the one that answered `/status`). */
  hideHeaders?: Set<number>;
}

const nodes = new Map<string, Node>();
function fakeNode(rpc: string, status: Record<string, unknown> = {}): Node {
  const n: Node = {
    status: { chain_id: "unit", genesis_hash: GENESIS, address_prefix: "boo", height: 50, finalized_height: 50, max_tx_validity_blocks: 3600, history_from: 1, ...status },
    accounts: new Map(), pool: new Map(), seen: new Map(), posts: [], script: [], finalizeAfter: 1, maxPending: 0, blocks: new Map(), pagedHistory: false, routes: {}, requests: [], contentTypes: [],
  };
  nodes.set(new URL(rpc).host, n);
  return n;
}
const pendingCount = (n: Node) => [...n.pool.values()].reduce((k, txs) => k + txs.length, 0);
const fees = { height: 50, lag: 17, slots: [], suggested: { flat: "10000", rate: "10000" }, lowest: { flat: "10000", rate: "10000" }, tip_cap_multiple: 20, account_creation_fee: "100000000" };
const reply = (status: number, body: unknown, headers: Record<string, string> = {}) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", ...headers } });
let fetchCalls = 0;

/** Finalizes `s` and every tx its sender queued before it, in nonce order, as blocks would. */
function finalize(n: Node, s: Seen) {
  for (const t of [...(n.pool.get(s.tx.from) ?? [])]) {
    if (t.nonce > s.tx.nonce) break;
    finalizeOne(n, n.seen.get(txId(t))!);
  }
}

function finalizeOne(n: Node, s: Seen) {
  const from = s.tx.from;
  const queue = n.pool.get(from) ?? [];
  n.pool.set(from, queue.filter((t) => txId(t) !== txId(s.tx)));
  const a = n.accounts.get(from) ?? { balance: 0n, nonce: 0 };
  a.nonce = s.tx.nonce + 1;
  n.status.height = (n.status.height as number) + 1;
  n.status.finalized_height = n.status.height;
  s.height = n.status.height as number;
  if (s.tx.action.type === "transfer") {
    const to = s.tx.action.to;
    const amount = BigInt(s.tx.action.amount);
    const opened = !n.accounts.has(to);
    a.balance -= amount + BigInt(s.tx.fee) + (opened ? BigInt(fees.account_creation_fee) : 0n);
    const r = n.accounts.get(to) ?? { balance: 0n, nonce: 0 };
    r.balance += amount;
    n.accounts.set(to, r);
  }
  n.accounts.set(from, a);
  s.status = "finalized";
}

const txView = (n: Node, s: Seen) => ({
  id: txId(s.tx), kind: s.tx.action.type, status: s.status, dropped: s.dropped, error: s.error ?? null, height: s.height ?? null,
  from: { address: s.tx.from, name: null }, to: s.tx.action.type === "transfer" ? { address: s.tx.action.to, name: null } : null,
  amount: s.tx.action.type === "transfer" ? s.tx.action.amount : null, fee: s.tx.fee, max_fee: s.tx.fee, nonce: s.tx.nonce, memo: s.tx.memo, summary: "ok", valid_until: s.tx.valid_until,
});

globalThis.fetch = (async (input: string | URL, init?: RequestInit) => {
  fetchCalls++;
  const url = new URL(String(input));
  const n = nodes.get(url.host);
  if (!n) throw new TypeError("fetch failed");
  const path = decodeURIComponent(url.pathname.replace(/^\/api/, ""));
  n.requests.push(path + url.search);
  if (n.failWith) return reply(n.failWith, { error: `error ${n.failWith}` }, n.failWith === 429 ? { "retry-after": "1" } : {});
  if (init?.method === "POST" && path === "/tx") {
    const type = new Headers(init.headers).get("content-type") ?? "";
    n.contentTypes.push(type);
    const tx: Tx = type === "application/octet-stream" ? (n.routes.wireDecoded as Tx) : JSON.parse(String(init.body));
    n.posts.push(tx);
    const scripted = n.script.shift();
    if (scripted) return reply(scripted.status, { error: scripted.error }, scripted.retryAfter ? { "retry-after": scripted.retryAfter } : {});
    const id = txId(tx);
    if (n.seen.has(id)) return reply(200, { id, status: n.seen.get(id)!.status }); // resubmitting the same bytes is idempotent
    const a = n.accounts.get(tx.from) ?? { balance: 0n, nonce: 0 };
    const queue = n.pool.get(tx.from) ?? [];
    const expected = a.nonce + queue.length;
    if (tx.nonce !== expected) return reply(400, { error: `bad nonce: expected ${expected}, got ${tx.nonce}` });
    if (tx.action.type === "transfer" && a.balance < BigInt(tx.action.amount) + BigInt(tx.fee)) return reply(400, { error: "insufficient balance" });
    if (queue.length >= 64) return reply(400, { error: "too many pending transactions from this account: wait for some to finalize" });
    if (!ml_dsa65.verify(hexToBytes(tx.signature), signingBytes(tx), hexToBytes(tx.public_key))) return reply(400, { error: "bad signature" });
    n.pool.set(tx.from, [...queue, tx]);
    n.maxPending = Math.max(n.maxPending, pendingCount(n));
    n.seen.set(id, { tx, status: "pending", reads: 0 });
    return reply(200, { id, status: "pending" });
  }
  if (path === "/status") return reply(200, n.status);
  if (path === "/fees") return reply(200, fees);
  const tx = path.match(/^\/tx\/([0-9a-f]{64})$/);
  if (tx) {
    const s = n.seen.get(tx[1]);
    if (!s) return reply(404, { error: "transaction not found" });
    if (s.status === "pending" && ++s.reads >= n.finalizeAfter) finalize(n, s);
    return reply(200, txView(n, s));
  }
  const paged = path.match(/^\/account\/([^/]+)\/txs$/);
  if (paged) {
    if (!n.pagedHistory) return new Response("", { status: 404 });
    const all = [...n.seen.values()].filter((s) => s.tx.from === paged[1] || (s.tx.action.type === "transfer" && s.tx.action.to === paged[1])).reverse();
    const limit = Number(url.searchParams.get("limit") ?? 25);
    const start = url.searchParams.has("before") ? Number(url.searchParams.get("before")) : 0;
    const page = all.slice(start, start + limit).map((s) => txView(n, s));
    return reply(200, { txs: page, next: start + limit < all.length ? String(start + limit) : null, history_from: n.status.history_from });
  }
  const account = path.match(/^\/account\/([^/]+)$/);
  if (account) {
    const id = account[1];
    const a = n.accounts.get(id);
    const balance = a?.balance ?? 0n;
    const locked = a?.locked ?? 0n;
    const recent = [...n.seen.values()].filter((s) => s.tx.from === id || (s.tx.action.type === "transfer" && s.tx.action.to === id)).reverse().map((s) => txView(n, s));
    return reply(200, {
      account: { address: id, name: null }, balance: balance.toString(), balance_display: formatFang(balance), transferable: (balance - locked).toString(), locked: locked.toString(),
      exists: !!a, creation_fee: a ? "0" : fees.account_creation_fee, nonce: a?.nonce ?? 0, next_nonce: (a?.nonce ?? 0) + (n.lagging ? 0 : (n.pool.get(id) ?? []).length), recent_txs: recent,
    });
  }
  const blocks = path === "/blocks";
  if (blocks) {
    const limit = Math.min(Number(url.searchParams.get("limit") ?? 20), 100);
    const before = url.searchParams.has("before") ? Number(url.searchParams.get("before")) : Infinity;
    const top = Math.min(before - 1, n.status.height as number);
    const from = Number(n.status.history_from ?? 1);
    const hidden = n.hideHeaders;
    n.hideHeaders = undefined;
    const out = [];
    for (let h = top; h >= from && out.length < limit; h--) if (!hidden?.has(h)) out.push({ height: h, hash: h.toString(16).padStart(64, "0"), tx_count: (n.blocks.get(h)?.txs as unknown[] | undefined)?.length ?? 0, timestamp_ms: 1_000 * h });
    return reply(200, out);
  }
  const block = path.match(/^\/block\/(\d+)$/);
  if (block) {
    const h = Number(block[1]);
    if (n.notYet?.delete(h)) return reply(404, { error: "block not found" });
    if (h < Number(n.status.history_from ?? 1)) return reply(410, { error: `block #${h} is pruned on this node`, pruned: true });
    const b = n.blocks.get(h);
    return b ? reply(200, { height: h, timestamp_ms: 1_000 * h, ...b }) : h <= (n.status.height as number) ? reply(200, { height: h, txs: [] }) : reply(404, { error: "block not found" });
  }
  if (path in n.routes) return reply(200, n.routes[path]);
  return reply(404, { error: `fake node: no route for ${path}` });
}) as typeof fetch;

/** A wallet with `balance` FANG on node `n`. */
function funded(n: Node, rpc: string, byte: number, balance = 1_000n * FANG, opts: ConstructorParameters<typeof Client>[1] = {}) {
  const w = Wallet.fromSeed(new Uint8Array(32).fill(byte), new Client(rpc, opts));
  n.accounts.set(w.address, { balance, nonce: 0 });
  return w;
}

// ---- network presets (DEV-1) ----

{
  // The prefix is known before any request: a testnet wallet renders tboo1… at once.
  const calls = fetchCalls;
  const w = Wallet.generate(Client.testnet());
  assert.ok(w.address.startsWith("tboo1"), w.address);
  assert.ok(Wallet.generate(new Client({ network: "testnet" })).address.startsWith("tboo1"));
  assert.equal(fetchCalls, calls, "no request needed to render the address");
  const t = new Client({ network: "testnet" });
  assert.deepEqual([t.rpc, t.network, t.prefix, t.genesisClock], [NETWORKS.testnet.rpc, "testnet", "tboo", NETWORKS.testnet.genesisClock]);
  // Your own node, held to the testnet's pins.
  const own = new Client("http://own.node", { network: "testnet" });
  assert.equal(own.rpc, "http://own.node");
  fakeNode("http://own.node", { chain_id: "blockula-testnet-2", genesis_hash: "00".repeat(32), address_prefix: "tboo" });
  await assert.rejects(own.status(), (e: unknown) => e instanceof ChainMismatchError && e.field === "genesis_hash");
  // Pins that contradict the network are errors, not guesses.
  assert.throws(() => new Client({ network: "testnet", chainId: "blockula-testnet-1" }), /contradicts the testnet network's/);
  assert.throws(() => new Client({ network: "testnet", prefix: "boo" }), /contradicts/);
  assert.throws(() => new Client({ network: "moon" as never }), /unknown network/);
  assert.doesNotThrow(() => new Client({ network: "testnet", genesisHash: NETWORKS.testnet.genesisHash!.toUpperCase() }));
  // No mainnet preset: the SDK knows the public testnet and a local dev chain.
  assert.deepEqual(Object.keys(NETWORKS), ["testnet", "local"]);
  assert.throws(() => new Client("https://rpc.example", { network: "mainnet" as never }), /unknown network "mainnet" \(testnet or local\)/);
  assert.equal((Client as unknown as Record<string, unknown>).mainnet, undefined, "no Client.mainnet()");
  assert.equal(Client.local().rpc, "http://localhost:7070");
  assert.equal(new Client("http://x.example", 5 as never).rpc, "http://x.example", "a bad options value is ignored as before");
  assert.ok(Object.isFrozen(NETWORKS) && Object.isFrozen(NETWORKS.testnet) && Object.isFrozen(NETWORKS.testnet.genesisClock));
}
{
  // The SDK's presets match the MCP's built-in networks (mcp/src/network.ts), which scripts/pin-network.sh writes: the pin PR copies them into networks.ts.
  assert.equal(NETWORKS.testnet.rpc, TESTNET_RPC);
  assert.equal(NETWORKS.testnet.chainId, KNOWN_NETWORKS.testnet.pin.chainId);
  assert.equal(NETWORKS.testnet.prefix, KNOWN_NETWORKS.testnet.pin.prefix);
  assert.equal(NETWORKS.testnet.genesisHash, KNOWN_GENESIS[NETWORKS.testnet.chainId!]);
  assert.deepEqual(NETWORKS.testnet.genesisClock, KNOWN_CLOCKS[NETWORKS.testnet.chainId!]);
  assert.equal(NETWORKS.local.rpc, KNOWN_NETWORKS.local.rpc);
}
console.log("✓ network presets pin everything up front (tboo1… before any request) and match the MCP's networks");

// ---- blockula-1 only through a client pinned to it (DEV-1) ----

{
  const main = fakeNode("http://main.node", { chain_id: MAINNET_CHAIN_ID });
  const lost = funded(main, "http://main.node", 0x21);
  // A client that isn't pinned to blockula-1 signs nothing for a node that says it's on it.
  for (const run of [() => lost.send(BOB, "1"), () => lost.build({ type: "unlock" }), () => lost.keyGrant(BOB), () => lost.registerName("ghoul")]) {
    await assert.rejects(run(), (e: unknown) => e instanceof MainnetNotAllowedError && /chainId: "blockula-1"/.test((e as Error).message) && /nothing was signed/.test((e as Error).message));
  }
  // The old opt-in is gone: allowMainnet is no option.
  await assert.rejects(funded(main, "http://main.node", 0x22, 1_000n * FANG, { allowMainnet: true } as never).send(BOB, "1"), MainnetNotAllowedError);
  assert.equal(main.posts.length, 0, "nothing submitted");
  // A key's consent made offline names the chain itself.
  assert.equal((await lost.keyGrant(BOB, { chainId: MAINNET_CHAIN_ID, genesisHash: GENESIS })).account, BOB);
  // The pin, and a client that pins the chain itself (as the web wallet's does) and says so.
  await funded(main, "http://main.node", 0x23, 1_000n * FANG, { chainId: MAINNET_CHAIN_ID }).send(BOB, "1");
  class SelfPinned extends Client {
    protected override chainPin() {
      return MAINNET_CHAIN_ID;
    }
  }
  const self = Wallet.fromSeed(new Uint8Array(32).fill(0x24), new SelfPinned("http://main.node"));
  main.accounts.set(self.address, { balance: 1_000n * FANG, nonce: 0 });
  await self.send(BOB, "1");
  assert.equal(main.posts.length, 2, "pinned clients sign there");
  // Other chains need no pin.
  const test = fakeNode("http://test.node");
  await funded(test, "http://test.node", 0x25).send(BOB, "1");
  assert.equal(test.posts.length, 1);
}
console.log("✓ blockula-1 only through a client pinned to it: MainnetNotAllowedError otherwise, nothing signed; no mainnet preset or opt-in");

// ---- honest balances (DEV-2) ----

{
  const n = fakeNode("http://bal.node");
  const w = funded(n, "http://bal.node", 0x30, 12n * FANG + FANG / 2n);
  n.accounts.get(w.address)!.locked = 2n * FANG;
  assert.equal(await w.balance(), "12.5 FANG");
  const b = await w.balances();
  assert.deepEqual([b.balance, b.transferable, b.locked, b.exists], [12n * FANG + FANG / 2n, 10n * FANG + FANG / 2n, 2n * FANG, true]);
  assert.deepEqual(b.display, { balance: "12.5 FANG", transferable: "10.5 FANG", locked: "2 FANG" });
  // An account that doesn't exist yet is empty, not an error.
  const fresh = Wallet.fromSeed(new Uint8Array(32).fill(0x31), new Client("http://bal.node"));
  assert.equal(await fresh.balance(), "0 FANG");
  assert.equal((await fresh.balances()).exists, false);
  // The node down, rate limited or failing: an error, never "0 FANG".
  await assert.rejects(Wallet.fromSeed(new Uint8Array(32).fill(0x32), new Client("http://down.node")).balance(), /fetch failed/);
  n.failWith = 429;
  await assert.rejects(w.balance(), (e: unknown) => e instanceof ApiError && e.status === 429 && e.retryAfterMs === 1_000);
  n.failWith = 502;
  await assert.rejects(w.balances(), (e: unknown) => e instanceof ApiError && e.status === 502);
  n.failWith = undefined;
}
console.log("✓ balance() throws when the node can't answer (\"0 FANG\" only for an account that doesn't exist); balances() has transferable and locked");

// ---- the nonce queue (DEV-3, TR-5) ----

{
  const n = fakeNode("http://q.node");
  const w = funded(n, "http://q.node", 0x40);
  // Ten parallel sends from one wallet: every one admitted, nonces 0–9, none refused.
  const sent = await Promise.all(Array.from({ length: 10 }, (_, i) => w.send(BOB, `${i + 1}`)));
  assert.ok(sent.every((t) => t.status === "finalized"));
  assert.deepEqual(n.posts.map((t) => t.nonce), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
  // A node whose next_nonce doesn't count this wallet's pending txs yet (another node, a block being written): the queue keeps counting.
  const lag = fakeNode("http://lag.node");
  lag.lagging = true;
  lag.finalizeAfter = 3;
  const lw = funded(lag, "http://lag.node", 0x41);
  await Promise.all(Array.from({ length: 5 }, () => lw.send(BOB, "1", { wait: false })));
  assert.deepEqual(lag.posts.map((t) => t.nonce), [0, 1, 2, 3, 4]);
  // A refused tx (the node said no) used no nonce: the next one takes it.
  const r = fakeNode("http://refuse.node");
  const rw = funded(r, "http://refuse.node", 0x42, 5n * FANG);
  r.finalizeAfter = 100;
  await rw.send(BOB, "1", { wait: false });
  await assert.rejects(rw.send(BOB, "100", { wait: false }), /insufficient balance/);
  await rw.send(BOB, "1", { wait: false });
  assert.deepEqual(r.posts.map((t) => t.nonce), [0, 1, 1]);
  // An answer that doesn't say (a 5xx: it may have been admitted): its nonce isn't handed out again.
  r.script.push({ status: 502, error: "couldn't reach a validator" });
  await assert.rejects(rw.send(BOB, "1", { wait: false }), (e: unknown) => e instanceof ApiError && e.status === 502);
  r.lagging = true;
  const after = await rw.build({ type: "unlock" });
  assert.equal(after.nonce, 3, "the nonce of a tx that may have arrived isn't reused");
  r.lagging = false;
  // 408: the node's timeout layer answers it for a request it may already have handled, so that nonce isn't reused either.
  const slow = fakeNode("http://timeout.node");
  const sw = funded(slow, "http://timeout.node", 0x46);
  slow.script.push({ status: 408, error: "request timed out" });
  await assert.rejects(sw.send(BOB, "1", { wait: false }), (e: unknown) => e instanceof ApiError && e.status === 408);
  slow.lagging = true;
  assert.equal((await sw.build({ type: "unlock" })).nonce, 1, "the nonce of a tx whose submission timed out isn't reused");
  // A node writing a block can misjudge a correct nonce for a moment (TR-1): the same bytes go again, once.
  const t1 = fakeNode("http://race.node");
  const tw = funded(t1, "http://race.node", 0x43);
  t1.script.push({ status: 400, error: "bad nonce: expected 5, got 0" });
  await tw.send(BOB, "1");
  assert.equal(t1.posts.length, 2);
  assert.equal(txId(t1.posts[0]), txId(t1.posts[1]), "resubmitted as the same signed bytes, never re-signed");
  // A real nonce gap: refused twice, then the wallet goes back to the node's count.
  t1.script.push({ status: 400, error: "bad nonce: expected 9, got 1" }, { status: 400, error: "bad nonce: expected 9, got 1" });
  await assert.rejects(tw.send(BOB, "1"), /bad nonce/);
  await tw.send(BOB, "1");
  assert.equal(t1.posts.at(-1)!.nonce, 1);
  // A tx this node dropped gives its nonce back.
  const d = fakeNode("http://drop.node");
  d.finalizeAfter = 1_000;
  const dw = funded(d, "http://drop.node", 0x44);
  await dw.send(BOB, "1", { wait: false });
  const dropped = d.seen.get(txId(d.posts[0]))!;
  Object.assign(dropped, { status: "failed", dropped: true, error: "evicted" });
  d.pool.set(dw.address, []);
  d.lagging = true;
  assert.equal((await dw.build({ type: "unlock" })).nonce, 0, "dropped: the node's next nonce again");
  // The public build() reads the queue's count but doesn't take a nonce.
  const b = fakeNode("http://build.node");
  const bw = funded(b, "http://build.node", 0x45);
  assert.equal((await bw.build({ type: "unlock" })).nonce, 0);
  assert.equal((await bw.build({ type: "unlock" })).nonce, 0, "building without submitting takes no nonce");
}
console.log("✓ parallel sends from one wallet get consecutive nonces; refusals give theirs back, unclear answers don't; a misjudged nonce is resubmitted as the same bytes");

// ---- store first, then send: onSigned hands over each signed tx before it's submitted, in the queue ----

{
  const n = fakeNode("http://outbox.node");
  const w = funded(n, "http://outbox.node", 0x48);
  const outbox: { id: string; tx: Tx; submittedYet: boolean }[] = [];
  const onSigned = (tx: Tx, id: string) => void outbox.push({ id, tx, submittedYet: n.posts.some((p) => txId(p) === id) });
  // Parallel withdrawals from one key, each stored before it leaves: one nonce each.
  await Promise.all(Array.from({ length: 5 }, (_, i) => w.send(BOB, `${i + 1}`, { onSigned })));
  assert.deepEqual(outbox.map((e) => e.tx.nonce), [0, 1, 2, 3, 4]);
  assert.ok(outbox.every((e) => e.id === txId(e.tx) && !e.submittedYet), "stored with its id, before it was submitted");
  assert.deepEqual(n.posts.map(txId), outbox.map((e) => e.id), "what was stored is what was submitted");
  // A store that fails submits nothing, and the next tx takes the nonce.
  const full = new Error("the outbox's disk is full");
  await assert.rejects(w.send(BOB, "1", { onSigned: () => { throw full; } }), (e: unknown) => e === full);
  assert.equal(n.posts.length, 5, "nothing submitted");
  await w.send(BOB, "1", { onSigned });
  assert.equal(outbox.at(-1)!.tx.nonce, 5);
  // sendMany stores each one too.
  const batch = await w.sendMany([{ to: BOB, amount: "1" }, { to: BOB, amount: "2" }], { onSigned });
  assert.ok(batch.every((r) => r.ok));
  assert.deepEqual(outbox.slice(-2).map((e) => [e.tx.nonce, e.submittedYet]), [[6, false], [7, false]]);
}
console.log("✓ onSigned stores each signed tx before it's submitted, in the nonce queue (parallel sends, sendMany); a failed store submits nothing");

// ---- sendMany (TR-5): a batch, at most `concurrency` pending, 429s and the per-sender cap waited out ----

{
  const n = fakeNode("http://batch.node");
  n.finalizeAfter = 2;
  const w = funded(n, "http://batch.node", 0x50);
  n.script.push({ status: 429, error: "too many transactions from your IP: try again in 1 minute", retryAfter: "1" });
  const payments = Array.from({ length: 9 }, (_, i) => ({ to: BOB, amount: `${i + 1}`, memo: `order-${i}` }));
  payments.splice(4, 0, { to: BOB, amount: "5000", memo: "too much" });
  const started = Date.now();
  const results = await w.sendMany(payments, { concurrency: 3 });
  assert.ok(Date.now() - started >= 900, "waited out Retry-After");
  assert.equal(results.length, 10);
  assert.deepEqual(results.map((r) => r.ok), [true, true, true, true, false, true, true, true, true, true]);
  const failed = results[4];
  assert.ok(!failed.ok && /insufficient balance/.test(String((failed.error as Error).message)));
  assert.ok(n.maxPending <= 3, `at most 3 pending at once (saw ${n.maxPending})`);
  const admitted = [...n.seen.values()].map((s) => s.tx.nonce).sort((a, b) => a - b);
  assert.deepEqual(admitted, [0, 1, 2, 3, 4, 5, 6, 7, 8], "nine admitted, one nonce each, no gaps");
  assert.deepEqual(new Set(n.posts.slice(0, 2).map(txId)).size, 1, "the rate-limited tx was resubmitted as the same bytes");
  await assert.rejects(w.sendMany([{ to: BOB, amount: "1" }, { to: BOB, amount: "0" }]), /payment 2: send more than 0/);
  await assert.rejects(w.sendMany([], { concurrency: 65 }), /concurrency/);
  // Without patience a 429 is the caller's to handle.
  n.script.push({ status: 429, error: "slow down", retryAfter: "1" });
  await assert.rejects(w.send(BOB, "1"), (e: unknown) => e instanceof ApiError && e.status === 429);
}
console.log("✓ sendMany keeps at most `concurrency` pending, resubmits the same bytes after a 429, and reports each payment");

// ---- costs: quote, the opening burn, zero amounts, fee headroom (DEV-7, DEV-14, TR-6) ----

{
  const n = fakeNode("http://cost.node");
  const w = funded(n, "http://cost.node", 0x60, 10n * FANG);
  // Paying an account that doesn't exist yet: the 0.1 FANG opening burn is in the quote.
  const q = await w.quote(NEWBIE, "0.02");
  const probe = await w.build({ type: "transfer", to: NEWBIE, amount: parseFang("0.02").toString() });
  assert.equal(q.fee, BigInt(probe.fee));
  assert.equal(q.fee, baseFee(probe, fees.suggested));
  assert.deepEqual([q.amount, q.creationFee, q.total, q.opensAccount, q.enough], [parseFang("0.02"), parseFang("0.1"), parseFang("0.12") + q.fee, true, true]);
  assert.equal(q.transferableAfter, 10n * FANG - q.total);
  assert.equal(q.display.creationFee, "0.1 FANG");
  assert.equal(q.recipient.address, NEWBIE);
  // An account that exists costs only the fee.
  n.accounts.set(BOB, { balance: 1n, nonce: 0 });
  const known = await w.quote(BOB, "0.02");
  assert.deepEqual([known.creationFee, known.opensAccount, known.total], [0n, false, parseFang("0.02") + known.fee]);
  // Refused before signing: 0, and less than 0.01 FANG to a new account.
  const before = n.posts.length;
  await assert.rejects(w.send(BOB, "0"), /send more than 0/);
  await assert.rejects(w.send(BOB, 0n), /send more than 0/);
  await assert.rejects(w.quote(BOB, "0"), /send more than 0/);
  await assert.rejects(w.send(NEWBIE, "0.001"), /sending to a new account needs at least 0\.01 FANG/);
  await assert.rejects(w.quote(NEWBIE, "0.009"), /new account needs at least/);
  assert.equal(n.posts.length, before, "nothing signed");
  assert.equal(MIN_NEW_ACCOUNT, parseFang("0.01"));
  // The receipt of a payment that opened an account says what it burned.
  const paid = await w.send(NEWBIE, "0.02");
  assert.deepEqual([paid.opened_account, paid.creation_fee, paid.creation_fee_display], [true, "100000000", "0.1 FANG"]);
  const spent = 10n * FANG - n.accounts.get(w.address)!.balance;
  assert.equal(spent, q.total, "the quote matches what left the account");
  const again = await w.send(NEWBIE, "0.02");
  assert.equal(again.opened_account, undefined, "a second payment opens nothing");
  // Fee headroom: only the base fee and the tip are charged, so room to rise is free.
  const tx = await w.build({ type: "unlock" });
  const base = baseFee(tx, fees.suggested);
  assert.equal(BigInt(tx.fee), base);
  assert.equal(BigInt((await w.build({ type: "unlock" }, "", { feeHeadroom: 3 })).fee), 3n * base);
  assert.equal(BigInt((await w.build({ type: "unlock" }, "", { feeHeadroom: 1.5, tip: 7n })).fee), (base * 1500n + 999n) / 1000n + 7n);
  assert.equal(BigInt((await w.build({ type: "unlock" }, "", { maxFee: "0.01" })).fee), parseFang("0.01"));
  await assert.rejects(w.build({ type: "unlock" }, "", { maxFee: 1n }), /maxFee 0\.000000001 FANG is below what this transaction costs now/);
  for (const feeHeadroom of [0.5, 101, Number.NaN]) await assert.rejects(w.build({ type: "unlock" }, "", { feeHeadroom }), /feeHeadroom must be a number from 1 to 100/);
  w.feeHeadroom = 2;
  assert.equal(BigInt((await w.build({ type: "unlock" })).fee), 2n * base, "the wallet's default");
  const hq = await w.quote(BOB, "1");
  assert.equal(hq.maxFee, 2n * hq.fee, "the quote's maxFee counts the headroom");
  assert.equal(hq.transferableAfter, BigInt((await w.balances()).transferable) - (hq.amount + hq.maxFee));
}
console.log("✓ quote() counts the opening burn, 0 and sub-minimum payments are refused before signing, receipts say what opening an account burned, fee headroom is free");

// ---- paged blocks and history (DEV-4, TR-3) ----

{
  const n = fakeNode("http://pages.node", { height: 300, finalized_height: 300 });
  const c = new Client("http://pages.node");
  const page = await c.blocks({ limit: 5, before: 101 });
  assert.deepEqual(page.map((b) => b.height), [100, 99, 98, 97, 96]);
  assert.deepEqual((await c.blocks(3)).map((b) => b.height), [300, 299, 298], "blocks(limit) still works");
  assert.ok(n.requests.includes("/blocks?limit=5&before=101"));
  await assert.rejects(c.blocks({ before: -1 }), /before must be a whole number/);
  await assert.rejects(c.blocks({ limit: 1.5 }), /limit must be a whole number/);
  // History: paged on nodes that have it…
  const w = funded(n, "http://pages.node", 0x70);
  for (let i = 0; i < 5; i++) await w.send(BOB, "1");
  n.pagedHistory = true;
  const first = await c.history(w.address, { limit: 2 });
  assert.deepEqual([first.txs.length, first.next, first.paged], [2, "2", true]);
  const rest = await c.history(w.address, { limit: 10, before: first.next! });
  assert.deepEqual([rest.txs.length, rest.next], [3, null]);
  // …and the newest ones on nodes from before it.
  n.pagedHistory = false;
  const old = await c.history(w.address);
  assert.deepEqual([old.txs.length, old.next, old.paged], [5, null, false]);
  assert.deepEqual((await c.history(w.address, { before: "2" })).txs, []);
}
console.log("✓ blocks({ limit, before }) and history(account, { before, limit }) page back (recent_txs on older nodes)");

// ---- watchPayments (DEV-4) ----

const ME = addressFromPublicKey(pk(5));
const transfer = (id: string, to: string, amount: bigint, extra: Record<string, unknown> = {}) => ({ id, kind: "transfer", status: "finalized", error: null, from: { address: BOB, name: "bob.boo" }, to: { address: to, name: null }, to_name: null, amount: amount.toString(), amount_display: formatFang(amount), memo: "", action: { type: "transfer", to, amount: amount.toString() }, ...extra });
{
  // paymentsIn: transfers that paid the account and didn't fail. Never the action's `to` (a name).
  const block = { height: 7, timestamp_ms: 7_000, txs: [
    transfer("a", ME, 5n * FANG, { memo: "order-1", to_name: "me.boo", action: { type: "transfer", to: "me.boo", amount: "5000000000" } }),
    transfer("b", BOB, FANG),
    transfer("c", ME, FANG, { status: "failed", error: "insufficient balance" }),
    { id: "d", kind: "award_hunt", status: "finalized", error: null, to: { address: ME }, amount: null },
    transfer("e", ME, 2n * FANG),
  ] };
  const found = paymentsIn(block, ME);
  assert.deepEqual(found.map((p) => [p.id, p.index, p.amount, p.memo, p.to_name, p.from, p.from_name, p.height, p.timestamp_ms]), [
    ["a", 0, 5n * FANG, "order-1", "me.boo", BOB, "bob.boo", 7, 7_000],
    ["e", 4, 2n * FANG, "", null, BOB, "bob.boo", 7, 7_000],
  ]);
}
{
  const n = fakeNode("http://watch.node", { height: 250, finalized_height: 250 });
  n.blocks.set(5, { txs: [transfer("p1", ME, FANG, { memo: "order-1" }), transfer("x", BOB, FANG)] });
  n.blocks.set(150, { txs: [transfer("p2", ME, 2n * FANG)] });
  n.blocks.set(240, { txs: [transfer("p3", ME, 3n * FANG, { memo: "order-3" })] });
  const c = new Client("http://watch.node");
  /** Watches until `next` passes `stopAt` (default 250), or for `forMs`. */
  const run = async (opts: Partial<Parameters<typeof watchPayments>[2]> & { stopAt?: number; forMs?: number }) => {
    const got: Payment[] = [];
    const progress: number[] = [];
    const stop = new AbortController();
    if (opts.forMs) setTimeout(() => stop.abort(), opts.forMs);
    const result = await watchPayments(c, ME, {
      pollMs: 10,
      ...opts,
      onPayment: (p) => void got.push(p),
      onProgress: (next) => {
        progress.push(next);
        if (next > (opts.stopAt ?? 250)) stop.abort();
      },
      signal: stop.signal,
    });
    return { got, progress, result };
  };
  const all = await run({ fromHeight: 1 });
  assert.deepEqual(all.got.map((p) => [p.id, p.height, p.amount]), [["p1", 5, FANG], ["p2", 150, 2n * FANG], ["p3", 240, 3n * FANG]]);
  assert.equal(all.progress.at(-1), 251);
  assert.equal(all.result, 251);
  const fetched = n.requests.filter((r) => /^\/block\/\d+$/.test(r));
  assert.deepEqual(fetched, ["/block/5", "/block/150", "/block/240"], "only blocks with txs are fetched");
  // Resuming from a stored height delivers only what came after.
  const resumed = await run({ fromHeight: 151 });
  assert.deepEqual(resumed.got.map((p) => p.id), ["p3"]);
  // Following the head: new blocks are picked up as they finalize.
  const live = (async () => run({ stopAt: 252 }))();
  await new Promise((r) => setTimeout(r, 50));
  n.blocks.set(252, { txs: [transfer("p4", ME, 4n * FANG)] });
  n.status.height = n.status.finalized_height = 252;
  assert.deepEqual((await live).got.map((p) => p.id), ["p4"]);
  // Only finalized blocks: a head past finalized_height waits.
  n.status.height = 253;
  n.blocks.set(253, { txs: [transfer("p5", ME, FANG)] });
  const capped = await run({ fromHeight: 253, stopAt: Infinity, forMs: 100 });
  assert.deepEqual([capped.got, capped.result], [[], 253], "an unfinalized block isn't read");
  n.status.finalized_height = 253;
  // A transient error is retried (and reported); the caller's own error stops the watch.
  const errors: unknown[] = [];
  let fails = 1;
  const real = globalThis.fetch;
  globalThis.fetch = (async (input: string | URL, init?: RequestInit) => (fails-- > 0 ? new Response(JSON.stringify({ error: "busy" }), { status: 503 }) : real(input, init))) as typeof fetch;
  const retried = await run({ fromHeight: 240, onError: (e) => void errors.push(e), stopAt: 252 });
  globalThis.fetch = real;
  assert.deepEqual(retried.got.map((p) => p.id), ["p3", "p4"]);
  assert.ok(errors.length === 1 && errors[0] instanceof ApiError);
  // A block this machine doesn't serve yet is retried, never skipped.
  n.notYet = new Set([150]);
  const late: unknown[] = [];
  const waited = await run({ fromHeight: 140, onError: (e) => void late.push(e), stopAt: 200 });
  assert.deepEqual(waited.got.map((p) => p.id), ["p2", "p3"]);
  assert.ok(late.length === 1 && (late[0] as ApiError).status === 404);
  // A header missing from the page of headers (at or below the head the status reported): the watch waits for it, never passes over it.
  n.hideHeaders = new Set([150]);
  const gap: unknown[] = [];
  const stalled = await run({ fromHeight: 140, onError: (e) => void gap.push(e), stopAt: 200 });
  assert.deepEqual(stalled.got.map((p) => p.id), ["p2", "p3"], "the payment in the block whose header was missing is delivered");
  assert.ok(gap.length === 1 && /didn't serve block #150's header yet/.test(String((gap[0] as Error).message)), "the stall is reported");
  const boom = new TypeError("my database is down");
  await assert.rejects(watchPayments(c, ME, { fromHeight: 1, pollMs: 10, onPayment: () => { throw boom; } }), (e: unknown) => e === boom, "never retried, never swallowed");
  // A pruned node can't show the gap: refused unless the caller skips it.
  n.status.history_from = 100;
  await assert.rejects(run({ fromHeight: 1 }), (e: unknown) => e instanceof HistoryPrunedError && e.from === 1 && e.historyFrom === 100);
  const skipped = await run({ fromHeight: 1, skipPruned: true, stopAt: 252 });
  assert.deepEqual(skipped.got.map((p) => p.id), ["p2", "p3", "p4"]);
  // Watch an address, not a name.
  await assert.rejects(watchPayments(c, "me.boo", { onPayment: () => undefined }), /isn't an address/);
  await assert.rejects(watchPayments(c, addressFromPublicKey(pk(5), "tboo"), { onPayment: () => undefined }), /tboo1… address, but .* uses boo1… addresses/);
}
console.log("✓ watchPayments delivers finalized transfers to the account in order, fetches only blocks with txs, resumes from a stored height, follows the head, retries the node's errors and refuses a pruned gap");

// ---- the wire form (DEV-16) ----

{
  const n = fakeNode("http://wire.node");
  const w = funded(n, "http://wire.node", 0x80, 1_000n * FANG, { wireTxs: true });
  const tx = await w.build({ type: "transfer", to: BOB, amount: FANG.toString() });
  n.routes.wireDecoded = tx;
  await w.client.submit(tx);
  assert.equal(n.contentTypes.at(-1), "application/octet-stream");
  assert.ok(txBytes(tx).length < JSON.stringify(tx).length / 1.8, "about half the bytes of JSON");
  const plain = fakeNode("http://json.node");
  await funded(plain, "http://json.node", 0x81).send(BOB, "1");
  assert.equal(plain.contentTypes.at(-1), "application/json");
}
console.log("✓ wireTxs posts txs in their wire form");

// A failure in a block (not dropped) keeps its nonce used.
{
  const n = fakeNode("http://ran.node");
  n.finalizeAfter = 1_000;
  const w = funded(n, "http://ran.node", 0x90);
  await w.send(BOB, "1", { wait: false });
  Object.assign(n.seen.get(txId(n.posts[0]))!, { status: "failed", dropped: false, error: "x", height: 51 });
  n.lagging = true;
  assert.equal((await w.build({ type: "unlock" })).nonce, 1, "a tx that ran used its nonce");
}
console.log("✓ a tx that ran and failed keeps its nonce used");
