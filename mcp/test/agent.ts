// The MCP as an agent's wallet, on any chain and on your own node, offline (npm test -w
// @blockula/mcp builds first; the server runs over stdio against fake nodes):
// 1. Units: caps (defaults, bad values), amounts, durations, session allowances, the checks before
//    signing, what can be spent now, the key file and accounts per chain, the ledger per chain
//    (and a 0.5.1 ledger), the account lock and the nonce tracker.
// 2. A bad cap is a configuration error every tool reports; the server stays up (AGENT-15).
// 3. Accounts and ledgers per chain: a 0.5.1 key file and ledger migrate, key and spend intact.
// 4. The true cost of a payment, and what wallet_info says can be spent.
// 5. Refused before signing: a session key's limits, locked FANG, a key that isn't on the account.
// 6. Nonces: parallel sends in one process and from two processes all go through; a node whose
//    view lags its mempool doesn't get a nonce reused; the node's mempool race.
// 7. Your own node: the network's public RPC as a witness by default; a chain without a faucet;
//    copy and ergonomics.
import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Client as McpClient } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { bech32m } from "@scure/base";
import { addressFromPublicKey, heightLimit, keyId, parseFang, txId, type Tx } from "@blockula/sdk";
import { actingFor, checkCode, createWalletFile, newWalletFile, readWalletFile, secretOf, writeWalletFile } from "../src/accounts.js";
import { aboutBlocks, allowanceOf, amountArg, capsFrom, configError, preflight, spendableNow, type Preflight } from "../src/budget.js";
import { AccountLocks, NonceTracker } from "../src/locks.js";
import { KNOWN_CLOCKS, KNOWN_GENESIS, TESTNET_RPC } from "../src/network.js";
import { SpendLedger, chainKey } from "../src/spend.js";

const FANG = 1_000_000_000n;
const dir = mkdtempSync(join(tmpdir(), "blockula-mcp-agent-"));
const G = "11".repeat(32);
const G2 = "22".repeat(32);
/** The key file's secret field, as 0.5.1 wrote it. */
const SECRET = "seed";
const today = () => new Date().toISOString().slice(0, 10);

// ---- 1. units ----

{
  // Caps: 100 FANG a call and 101 a day, unless set; the daily one follows the per-call one.
  const t = capsFrom({});
  assert.deepEqual([t.maxSend, t.maxDaily], [100n * FANG, 101n * FANG]);
  assert.match(t.dailyText, /by default BLOCKULA_MAX_SEND \+ 1 FANG for fees = 101 FANG/);
  assert.deepEqual([capsFrom({ BLOCKULA_MAX_SEND: "50" }).maxSend, capsFrom({ BLOCKULA_MAX_SEND: "50" }).maxDaily], [50n * FANG, 51n * FANG]);
  assert.deepEqual([capsFrom({ BLOCKULA_MAX_DAILY: "5" }).maxSend, capsFrom({ BLOCKULA_MAX_DAILY: "5" }).maxDaily], [100n * FANG, 5n * FANG]);
  assert.throws(() => capsFrom({ BLOCKULA_MAX_SEND: "100 FANG" }), /BLOCKULA_MAX_SEND must be an amount of FANG.*"100 FANG"/);
  assert.match(configError({ BLOCKULA_MAX_SEND: "100 FANG" })?.message ?? "", /BLOCKULA_MAX_SEND/);
  assert.match(configError({ BLOCKULA_MAX_DAILY: "-1" })?.message ?? "", /BLOCKULA_MAX_DAILY/);
  assert.match(configError({ BLOCKULA_TX_WAIT_MS: "soon" })?.message ?? "", /BLOCKULA_TX_WAIT_MS/);
  assert.equal(configError({ BLOCKULA_MAX_SEND: "1.5", BLOCKULA_TIMEOUT_MS: "300" }), null);
  // Amounts: a trailing " FANG" is fine; anything else says what's wanted.
  assert.equal(amountArg("1.5 FANG"), parseFang("1.5"));
  assert.equal(amountArg(" 2fang "), 2n * FANG);
  assert.throws(() => amountArg("1,5"), /a number in FANG with up to 9 decimals, like "1.5"/);
  assert.deepEqual([aboutBlocks(30), aboutBlocks(600), aboutBlocks(82_800), aboutBlocks(604_800)], ["about 30 s", "about 10 min", "about 23 h", "about 7 days"]);
  // A session key's allowance as the node counts it: the window restarts period_blocks after it began.
  const key = { key_id: "aa".repeat(32), role: { type: "session", limit: (3n * FANG).toString(), period_blocks: 100, expires_at: 1000 }, spent_this_period: (2n * FANG).toString(), window_start: 50 };
  assert.deepEqual(allowanceOf(key, 120), { limit: 3n * FANG, spent: 2n * FANG, left: FANG, periodBlocks: 100, resetsAt: 150, expiresAt: 1000, expired: false });
  assert.equal(allowanceOf(key, 150)?.left, 3n * FANG, "a new window");
  assert.equal(allowanceOf(key, 1001)?.expired, true);
  assert.equal(allowanceOf({ ...key, role: { type: "owner" } }, 120), null);
  // The checks before signing.
  const base: Preflight = { account: { keys: [key], transferable: (50n * FANG).toString() }, keyId: key.key_id, actingFor: "boo1user", action: "transfer", outflow: FANG / 2n, opening: 0n, fee: 1000n, height: 120 };
  preflight(base);
  assert.throws(() => preflight({ ...base, action: "register_name", outflow: 5n * FANG }), /Refused before signing.*session keys can only send transfers/);
  assert.throws(() => preflight({ ...base, action: "link_github", outflow: 0n }), /needs an owner key: ask the user to link GitHub in their wallet/);
  assert.throws(() => preflight({ ...base, action: "submit_claim", outflow: FANG }), /register it in their wallet/);
  assert.throws(() => preflight({ ...base, action: "submit_claim", outflow: FANG / 2n, account: { ...base.account, familiars: [{ key_id: "bb".repeat(32) }] } }), /Familiar/, "another key's Familiar registration doesn't count");
  preflight({ ...base, action: "submit_claim", outflow: FANG / 2n, account: { ...base.account, familiars: [{ key_id: key.key_id }] } });
  assert.throws(() => preflight({ ...base, outflow: FANG }), /needs 1\.000001 FANG with the fee, but the session key has 1 FANG of its 3 FANG allowance left this window\. It resets at block 150 \(about 30 s\)/);
  assert.throws(() => preflight({ ...base, outflow: FANG / 2n, opening: FANG / 10n * 6n }), /account-opening burn \(0\.6 FANG\)/, "a transfer's opening burn counts against the session key too");
  assert.throws(() => preflight({ ...base, height: 1001 }), /expired at block 1000/);
  assert.throws(() => preflight({ ...base, keyId: "cc".repeat(32) }), /This agent's key \(CCCC-CCCC-CCCC\) isn't on boo1user/);
  const owner = { ...key, role: { type: "owner" } };
  assert.throws(() => preflight({ ...base, account: { keys: [owner], transferable: (4n * FANG).toString(), locked: (90n * FANG).toString() }, outflow: 4n * FANG }), /can move 4 FANG now \(90 FANG more is locked/);
  preflight({ ...base, account: { keys: [owner], transferable: (4n * FANG).toString() }, action: "delegate", outflow: 0n });
  preflight({ ...base, account: { keys: [owner] }, outflow: 1_000n * FANG }); // the node doesn't say: the chain checks
  preflight({ ...base, account: null }); // no answer: nothing to refuse on
  // What can be spent now: the smallest limit, and which it is.
  const caps = capsFrom({});
  const sp = spendableNow({ transferable: 95n * FANG, allowance: allowanceOf(key, 120), height: 120, dailyLeft: 74n * FANG, caps });
  assert.deepEqual([sp.amount, sp.limitedBy], [FANG, "the session key has 1 FANG of 3 FANG left until block 150 (about 30 s)"]);
  assert.equal(sp.limits.length, 4);
  assert.match(spendableNow({ transferable: 4n * FANG, height: 1, dailyLeft: 101n * FANG, caps }).limitedBy, /the account can move 4 FANG now/);
  console.log("✓ caps (defaults, and bad values are errors), amounts, durations, session allowances, the checks before signing, what can be spent now");
}

{
  // The key file: writes keep the key and every other field; a new file never replaces one.
  const file = join(dir, "units", "agent-wallet.json");
  const secret = Buffer.from(ml_dsa65.keygen(new Uint8Array(32).fill(5)).publicKey.slice(0, 32)).toString("hex");
  const tboo = addressFromPublicKey(ml_dsa65.keygen(new Uint8Array(32).fill(6)).publicKey, "tboo");
  const legacy = { address: "boo1me", key_id: "ab".repeat(32), [SECRET]: secret, account: tboo, extra: { kept: true } };
  createWalletFile(file, legacy as never);
  createWalletFile(file, newWalletFile("boo1other", "cd".repeat(32), "ef".repeat(32)));
  assert.equal(secretOf(readWalletFile(file)!), secret, "a second create doesn't replace the key");
  assert.equal(statSync(file).mode & 0o777, 0o600);
  const f = readWalletFile(file)!;
  // A testnet account isn't used on a boo1… chain, and stays for its own chain.
  const ck = chainKey("fake-chain", G);
  assert.deepEqual(actingFor(f, ck, "boo"), { ignored: tboo, changed: false });
  // On a tboo1… chain it moves to that chain.
  const tk = chainKey("blockula-testnet-2", G2);
  assert.deepEqual(actingFor(f, tk, "tboo"), { account: tboo, changed: true });
  assert.deepEqual([f.account, f.accounts], [undefined, { [tk]: tboo }]);
  writeWalletFile(file, f);
  const back = readWalletFile(file)!;
  assert.deepEqual([secretOf(back), back.extra, back.accounts], [secret, { kept: true }, { [tk]: tboo }], "rewritten whole: the key and unknown fields are kept");
  assert.deepEqual(actingFor(back, ck, "boo"), { changed: false }, "other chains act for the key itself");
  assert.equal(checkCode("ab12cd34" + "0".repeat(52) + "9f8e"), "AB12-CD34-9F8E");
  assert.throws(() => writeWalletFile(file, { address: "x", key_id: "y" }), /no valid key/, "a file without its key is never written");
  assert.equal(secretOf(readWalletFile(file)!), secret);
  console.log("✓ the key file: written whole (key and unknown fields kept), never replaced by a second create; 0.5.1's account moves to the chain whose prefix it matches");
}

{
  // The ledger per chain, and a 0.5.1 ledger: what it counted counts toward every chain today.
  const file = join(dir, "units", "agent-spend.json");
  writeFileSync(file, JSON.stringify({ day: today(), spent: (7n * FANG).toString(), slots: { "boo1me#3": (7n * FANG).toString() } }));
  const l = new SpendLedger(file, 20n * FANG, "x");
  const [a, b] = [chainKey("chain-a", G), chainKey("chain-b", G2)];
  assert.deepEqual([l.spent(a), l.spent(b), l.spent()], [7n * FANG, 7n * FANG, 7n * FANG], "0.5.1's spend counts toward each chain");
  l.reserve(5n * FANG, { account: "boo1me", nonce: 3, chain: a });
  assert.deepEqual([l.spent(a), l.spent(b), l.spent()], [12n * FANG, 7n * FANG, 12n * FANG], "a chain's own spend counts only toward it (the 0.5.1 slot isn't deduped: errs high)");
  assert.throws(() => l.reserve(9n * FANG, { account: "boo1me", nonce: 4, chain: a }), /daily limit/);
  const r = l.reserve(9n * FANG, { account: "boo1me", nonce: 4, chain: b });
  assert.deepEqual([l.spent(b), l.spent()], [16n * FANG, 21n * FANG]);
  l.release(r, 2n * FANG);
  assert.deepEqual([l.spent(a), l.spent(b), l.spent()], [12n * FANG, 14n * FANG, 19n * FANG]);
  const raw = JSON.parse(readFileSync(file, "utf8"));
  assert.deepEqual([raw.spent, raw.chains], [(19n * FANG).toString(), { [a]: (5n * FANG).toString(), [b]: (7n * FANG).toString() }], "`spent` stays every chain's total, so 0.5.1 still reads a whole day");
  // 0.5.1 writes it back without `chains`: everything counts toward every chain again.
  writeFileSync(file, JSON.stringify({ day: raw.day, spent: raw.spent, slots: raw.slots }));
  assert.deepEqual([l.spent(a), l.spent(b)], [19n * FANG, 19n * FANG]);
  writeFileSync(file, JSON.stringify({ ...raw, chains: { [a]: "-1" } }));
  assert.throws(() => l.spent(a), /malformed/);
  console.log("✓ the ledger keeps a total per chain; a 0.5.1 ledger's spend counts toward every chain (never under-counted)");
}

{
  // The account lock: one at a time per key in this process, and across processes by its file.
  const locks = new AccountLocks(join(dir, "units", "nonces"), { staleMs: 60_000, waitMs: 2_000, pollMs: 5 });
  let inside = 0;
  let most = 0;
  const order: number[] = [];
  await Promise.all(
    [1, 2, 3].map((i) =>
      locks.run("k", async () => {
        inside++;
        most = Math.max(most, inside);
        await new Promise((r) => setTimeout(r, 20));
        order.push(i);
        inside--;
      }),
    ),
  );
  assert.deepEqual([most, order], [1, [1, 2, 3]], "serialized, in order");
  await Promise.all([locks.run("k1", () => new Promise((r) => setTimeout(r, 30))), locks.run("k2", async () => assert.ok(existsSync(locks.file("k1")), "another key isn't blocked"))]);
  // A lock file left by a process that's gone is taken over; one held by a live process waits it out.
  writeFileSync(locks.file("k"), "2147483646 0 x");
  assert.equal(await locks.run("k", async () => "ran"), "ran");
  assert.ok(!existsSync(locks.file("k")), "released");
  writeFileSync(locks.file("k"), `${process.ppid} 0 x`);
  await assert.rejects(locks.run("k", async () => "ran"), /sending from this account for over 2 s/);
  rmSync(locks.file("k"));
  // The nonce tracker: last pending + 1 while it can still run.
  const t = new NonceTracker(join(dir, "units", "nonces"));
  assert.equal(t.floor("c a", 10), undefined);
  t.pending("c a", 4, 600);
  assert.deepEqual([t.floor("c a", 10), t.floor("c a", 601), t.floor("c b", 10)], [5, undefined, undefined]);
  t.settled("c a", 3);
  assert.equal(t.floor("c a", 10), 5, "settling another nonce changes nothing");
  t.settled("c a", 4);
  assert.equal(t.floor("c a", 10), undefined, "a dropped or refused tx holds its nonce no longer");
  // One that ran keeps its nonce spent for good (a node's account read can trail a block that just ran).
  t.pending("c a", 6, 600);
  t.ran("c a", 6);
  assert.deepEqual([t.floor("c a", 10), t.floor("c a", 10_000)], [7, 7]);
  t.pending("c a", 7, 600);
  t.settled("c a", 7);
  assert.equal(t.floor("c a", 10), 7, "settling a pending tx doesn't forget one that ran");
  t.ran("c a", 3);
  assert.equal(t.floor("c a", 10), 7, "never goes down");
  t.forget("c a");
  assert.equal(t.floor("c a", 10), undefined);
  console.log("✓ the account lock serializes per account (in process and by its file, taking over a dead holder's); the nonce tracker");
}

// ---- a fake node that enforces nonces like the real mempool ----

const kp = (n: number) => ml_dsa65.keygen(new Uint8Array(32).fill(n)).publicKey;
const originalKey = (address: string) => Buffer.from(bech32m.fromWords(bech32m.decode(address as `${string}1${string}`, 200).words)).toString("hex");
const BOB = addressFromPublicKey(kp(3));
const FRESH = addressFromPublicKey(kp(4));
const TBOO_USER = addressFromPublicKey(kp(5), "tboo");

interface Acct {
  balance: bigint;
  nonce: number;
  exists: boolean;
  transferable?: bigint;
  locked?: bigint;
  locks?: unknown[];
  keys?: unknown[];
  familiars?: unknown[];
  github?: unknown;
}
const fake = {
  status: {} as Record<string, unknown>,
  /** `now`: a posted tx runs at once; `never`: it stays pending. */
  run: "now" as "now" | "never",
  /** /api/account's next_nonce leaves out the mempool (a node whose view lags it), while admission counts it. */
  lag: false,
  /** /api/account trails the last block that ran: one nonce behind (seen on a dev chain right after a block). */
  trail: false,
  /** Every /api/account answer waits this long (so builds made in parallel would overlap). */
  accountDelayMs: 0,
  accounts: new Map<string, Acct>(),
  pending: [] as Tx[],
  done: new Map<string, Tx>(),
  posted: [] as Tx[],
  refused: [] as string[],
  faucetHits: 0,
  hunts: [] as unknown[],
  githubVerified: false,
};
const acct = (a: string): Acct => fake.accounts.get(a) ?? { balance: 0n, nonce: 0, exists: false };
const pendingOf = (a: string) => fake.pending.filter((t) => t.from === a).length;
const height = () => (typeof fake.status.height === "number" ? fake.status.height : 10);
const node: Server = createServer((req, res) => {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", async () => {
    const reply = (code: number, v: unknown) => res.writeHead(code, { "content-type": "application/json" }).end(JSON.stringify(v, (_, x) => (typeof x === "bigint" ? x.toString() : x)));
    const path = decodeURIComponent((req.url ?? "").replace(/^\/api/, ""));
    if (path === "/status") return reply(200, { chain_id: "fake-chain", genesis_hash: G, address_prefix: "boo", height: 10, max_tx_validity_blocks: 3600, ...fake.status });
    if (req.method === "POST" && path === "/faucet") {
      fake.faucetHits++;
      return reply(400, { error: "this chain has no faucet" });
    }
    if (req.method === "POST" && path === "/tx") {
      const tx = JSON.parse(body) as Tx;
      const from = acct(tx.from);
      const expected = from.nonce + pendingOf(tx.from);
      if (tx.nonce !== expected) {
        fake.refused.push(`bad nonce: expected ${expected}, got ${tx.nonce}`);
        return reply(400, { error: `bad nonce: expected ${expected}, got ${tx.nonce}` });
      }
      fake.posted.push(tx);
      if (fake.run === "never") fake.pending.push(tx);
      else {
        fake.accounts.set(tx.from, { ...from, nonce: from.nonce + 1, exists: true });
        fake.done.set(txId(tx), tx);
      }
      return reply(200, { id: txId(tx), status: "pending" });
    }
    const t = path.match(/^\/tx\/([0-9a-f]{64})$/);
    if (t) {
      const tx = fake.done.get(t[1]);
      if (tx) return reply(200, { id: t[1], status: "finalized", summary: `${tx.action.type}`, fee: (BigInt(tx.fee) / 2n).toString(), fee_display: `${BigInt(tx.fee) / 2n} drops`, max_fee: tx.fee, height: 11, valid_until: tx.valid_until });
      const p = fake.pending.find((x) => txId(x) === t[1]);
      if (p) return reply(200, { id: t[1], status: "pending", valid_until: p.valid_until });
      return reply(404, { error: "tx not found" });
    }
    if (path.startsWith("/account/")) {
      const id = path.slice(9);
      const address = id === "bob.boo" || id === "bob" ? BOB : id;
      const a = acct(address);
      const keys = a.keys ?? [{ key_id: originalKey(address), label: "original key", role: { type: "owner" }, added_at: 0, spent_this_period: "0", window_start: 0 }];
      // Read now, answered later: what a client builds on can be stale by the time it signs.
      const answer = {
        account: { address, name: address === BOB ? "bob.boo" : null },
        balance: a.balance.toString(),
        balance_display: `${a.balance} drops`,
        ...(a.transferable !== undefined ? { transferable: a.transferable.toString(), locked: (a.locked ?? 0n).toString(), locks: a.locks ?? [] } : {}),
        exists: a.exists,
        creation_fee: a.exists ? "0" : "100000000",
        nonce: a.nonce,
        next_nonce: (fake.trail && a.nonce > 0 ? a.nonce - 1 : a.nonce) + (fake.lag ? 0 : pendingOf(address)),
        keys,
        familiars: a.familiars ?? [],
        github: a.github ?? null,
        recent_txs: [],
      };
      if (fake.accountDelayMs) await new Promise((r) => setTimeout(r, fake.accountDelayMs));
      return reply(200, answer);
    }
    if (path === "/fees") return reply(200, { height: height(), suggested: { flat: "10000", rate: "10000" }, account_creation_fee: "100000000" });
    if (path === "/params") return reply(200, { params: { name_price_3: "100000000000", name_price_4: "25000000000", name_price_5: "5000000000", name_renewal_bps: "2000" } });
    if (path === "/name/bob" || path === "/name/bob.boo") return reply(200, { name: "bob.boo", status: "active", available: false, owner: { address: BOB, name: "bob.boo" }, price: "5000000000", renewal_price: "1000000000" });
    if (path.startsWith("/name/")) return reply(200, { name: `${path.slice(6)}.boo`, status: "available", available: true, price: "5000000000", renewal_price: "1000000000" });
    if (path.startsWith("/hunts")) return reply(200, fake.hunts);
    if (path.startsWith("/github/")) return reply(200, { handle: path.slice(8), verified: fake.githubVerified, reason: fake.githubVerified ? "ok" : "gist doesn't contain the account's boo1 address" });
    if (path.startsWith("/claims")) return reply(200, { claims: [] });
    return reply(404, { error: `fake node: no route for ${path}` });
  });
});
await new Promise<void>((r) => node.listen(0, "127.0.0.1", r));
const rpc = `http://127.0.0.1:${(node.address() as AddressInfo).port}`;

async function server(home: string, env: Record<string, string> = {}, argv: string[] = []) {
  const transport = new StdioClientTransport({ command: process.execPath, args: [new URL("../dist/index.js", import.meta.url).pathname, ...argv], env: { PATH: process.env.PATH ?? "", BLOCKULA_HOME: home, BLOCKULA_RPC: rpc, BLOCKULA_TX_WAIT_MS: "300", ...env }, stderr: "ignore" });
  const c = new McpClient({ name: "test", version: "0" });
  await c.connect(transport);
  const call = async (name: string, args: Record<string, unknown> = {}) => {
    const r = (await c.callTool({ name, arguments: args })) as { content: { text: string }[]; isError?: boolean };
    return { error: !!r.isError, text: r.content[0].text, json: () => JSON.parse(r.content[0].text) };
  };
  return { call, tools: async () => (await c.listTools()).tools as { name: string; title?: string; annotations?: Record<string, unknown> }[], instructions: () => c.getInstructions() ?? "", close: () => c.close() };
}
const spentOn = (home: string) => {
  const f = join(home, "agent-spend.json");
  return existsSync(f) ? BigInt(JSON.parse(readFileSync(f, "utf8")).spent) : 0n;
};

// ---- 2. a bad cap is reported by every tool ----

{
  const home = join(dir, "badcap");
  const s = await server(home, { BLOCKULA_MAX_SEND: "100 FANG" });
  for (const [tool, args] of [["status", {}], ["create_wallet", {}], ["wallet_info", {}], ["send", { to: BOB, amount: "1" }], ["install_node", {}], ["list_hunts", {}]] as const) {
    const r = await s.call(tool, args);
    assert.ok(r.error && /isn't configured correctly: BLOCKULA_MAX_SEND must be an amount of FANG.*"100 FANG"/.test(r.text), `${tool}: ${r.text}`);
  }
  await s.close();
  assert.equal(fake.posted.length, 0);
  console.log("✓ BLOCKULA_MAX_SEND=\"100 FANG\": the server stays up and every tool answers with the configuration error");
}

// ---- 3. 0.5.1's files migrate ----

{
  const home = join(dir, "migrate");
  const secret = Buffer.from(kp(9).slice(0, 32)).toString("hex");
  const own = addressFromPublicKey(kp(10), "tboo");
  // 0.5.1's key file: one account for every network (here a testnet account).
  createWalletFile(join(home, "agent-wallet.json"), { address: own, key_id: "ef".repeat(32), [SECRET]: secret, account: TBOO_USER } as never);
  // 0.5.1's ledger: 3 FANG spent today, one slot.
  writeFileSync(join(home, "agent-spend.json"), JSON.stringify({ day: today(), spent: (3n * FANG).toString(), slots: { [`${TBOO_USER}#0`]: (3n * FANG).toString() } }));
  // On a boo1… chain the testnet account is ignored, and said so; the key and the ledger are intact.
  let s = await server(home, { BLOCKULA_MAX_DAILY: "4" });
  let r = await s.call("wallet_info");
  let w = r.json();
  assert.ok(!r.error && w.address !== TBOO_USER && w.address.startsWith("boo1") && new RegExp(`used to act for ${TBOO_USER}, the public testnet's account`).test(w.previous_account_ignored), r.text);
  assert.match(w.spent_today, /^3 FANG of 4 FANG/, "0.5.1's spend counts here");
  r = await s.call("send", { to: BOB, amount: "1" });
  assert.ok(r.error && /daily limit/.test(r.text) && /Nothing was signed/.test(r.text), `3 + 1 + the fee doesn't fit 4: ${r.text}`);
  await s.close();
  let f = readWalletFile(join(home, "agent-wallet.json"))!;
  assert.deepEqual([secretOf(f), f.account], [secret, TBOO_USER], "kept for its own chain");
  // On a tboo1… chain it's that chain's account from now on.
  fake.status = { chain_id: "fake-testnet", genesis_hash: G2, address_prefix: "tboo" };
  fake.accounts.set(TBOO_USER, { balance: 9n * FANG, nonce: 0, exists: true, keys: [{ key_id: keyId(ml_dsa65.keygen(Buffer.from(secret, "hex")).publicKey), label: "agent", role: { type: "owner" }, added_at: 0, spent_this_period: "0", window_start: 0 }] });
  s = await server(home, { BLOCKULA_MAX_DAILY: "5" });
  r = await s.call("wallet_info");
  w = r.json();
  assert.ok(!r.error && w.address === TBOO_USER && w.previous_account_ignored === undefined && /^3 FANG of 5 FANG/.test(w.spent_today), r.text);
  r = await s.call("send", { to: addressFromPublicKey(kp(3), "tboo"), amount: "1" });
  assert.ok(!r.error, r.text);
  assert.ok(spentOn(home) > 4n * FANG, "today's 0.5.1 spend and the new one, together");
  await s.close();
  f = readWalletFile(join(home, "agent-wallet.json"))!;
  assert.deepEqual([secretOf(f), f.account, f.accounts], [secret, undefined, { [chainKey("fake-testnet", G2)]: TBOO_USER }], "moved to its chain, the key intact");
  const ledgerFile = JSON.parse(readFileSync(join(home, "agent-spend.json"), "utf8"));
  assert.ok(BigInt(ledgerFile.spent) > 4n * FANG && ledgerFile.slots[`${TBOO_USER}#0`] === (3n * FANG).toString() && Object.keys(ledgerFile.chains).length === 1, JSON.stringify(ledgerFile));
  fake.status = {};
  console.log("✓ a 0.5.1 key file and ledger migrate: the key, the account (to the chain whose prefix it matches) and today's spend intact");
}

// ---- 4. the true cost, and what can be spent ----

const home = join(dir, "home");
let s = await server(home);
const me = (await s.call("create_wallet")).json();
const AGENT = me.own_address as string;
// The key file: unencrypted, readable by this user only.
assert.deepEqual([me.stored_at, statSync(me.stored_at).mode & 0o777, me.existed, me.check_code], [join(home, "agent-wallet.json"), 0o600, false, checkCode(me.key_id)]);
{
  fake.accounts.set(AGENT, { balance: 100n * FANG, nonce: 0, exists: true, transferable: 100n * FANG });
  fake.accounts.set(BOB, { balance: 5n * FANG, nonce: 0, exists: true });
  let r = await s.call("send", { to: FRESH, amount: "0.01 FANG" });
  const sent = fake.posted.at(-1)!;
  assert.ok(!r.error, r.text);
  // The fake node charges half the max fee.
  const fang = (v: string) => parseFang(v.replace(/ FANG$/, ""));
  assert.equal(r.json().account_opening_fee_burned, "0.1 FANG", r.text);
  assert.equal(fang(r.json().total_cost), parseFang("0.11") + BigInt(sent.fee) / 2n, "amount + the fee charged + the opening burn");
  r = await s.call("send", { to: BOB, amount: "1" });
  assert.ok(!r.error && r.json().account_opening_fee_burned === undefined && /^1\.\d+ FANG$/.test(r.json().total_cost), "an existing account: no burn");
  // In flight: the most it can cost.
  fake.run = "never";
  r = await s.call("send", { to: addressFromPublicKey(kp(11)), amount: "0.5" });
  const pend = fake.posted.at(-1)!;
  assert.ok(!r.error && r.json().status === "pending" && r.json().account_opening_fee_burned === "0.1 FANG" && /at most, if it runs/.test(r.json().total_cost_is), r.text);
  assert.equal(fang(r.json().total_cost), parseFang("0.6") + BigInt(pend.fee), "in flight: with the max fee");
  // It runs.
  for (const tx of fake.pending) fake.done.set(txId(tx), tx);
  fake.accounts.set(AGENT, { ...acct(AGENT), nonce: acct(AGENT).nonce + fake.pending.length });
  fake.pending = [];
  fake.run = "now";
  // wallet_info after a conviction vote: 90 of 94.6 FANG locked until block 1121.
  fake.accounts.set(AGENT, { balance: parseFang("94.6"), nonce: acct(AGENT).nonce, exists: true, transferable: parseFang("4.6"), locked: 90n * FANG, locks: [{ id: "vote:1", reason: "conviction 6 vote", amount: (90n * FANG).toString(), amount_display: "90 FANG", until: 1121, vesting: false }] });
  r = await s.call("wallet_info");
  const w = r.json();
  assert.deepEqual([w.transferable, w.locked, w.locked_until_block, w.spendable_now], ["4.6 FANG", "90 FANG", 1121, "4.6 FANG"], r.text);
  assert.match(w.locked_until, /^about 19 min from now$/);
  assert.match(w.spendable_now_limited_by, /the account can move 4\.6 FANG now/);
  assert.deepEqual(w.locks[0], { id: "vote:1", amount: "90 FANG", until_block: 1121, ends_in: "about 19 min", vesting: false });
  // More than it can move: refused before signing, nothing counted.
  const [before, counted] = [fake.posted.length, spentOn(home)];
  r = await s.call("send", { to: BOB, amount: "10" });
  assert.ok(r.error && /Refused before signing.*can move 4\.6 FANG now \(90 FANG more is locked/.test(r.text), r.text);
  assert.deepEqual([fake.posted.length, spentOn(home)], [before, counted]);
  console.log("✓ a payment that opens an account reports account_opening_fee_burned and total_cost (final and in flight); wallet_info shows transferable, locked (until when) and spendable_now");
}

// ---- 5. refused before signing ----

{
  // The agent acts for Bob through a session key: 3 FANG per 100 blocks, 2 spent, expiring at block 1000.
  const session = { key_id: me.key_id, label: "claude", role: { type: "session", limit: (3n * FANG).toString(), period_blocks: 100, expires_at: 1000 }, added_at: 0, spent_this_period: (2n * FANG).toString(), window_start: 5 };
  fake.accounts.set(BOB, { balance: 50n * FANG, nonce: 0, exists: true, transferable: 50n * FANG, keys: [session] });
  let r = await s.call("use_account", { account: BOB });
  let u = r.json();
  assert.ok(!r.error && u.address === BOB && u.session_key.left === "1 FANG" && u.session_key.resets_at_block === 105 && u.spendable_now === "1 FANG" && /session key has 1 FANG of 3 FANG left until block 105/.test(u.spendable_now_limited_by), r.text);
  assert.match(u.session_key.expires_in, /^about 17 min$/);
  const [before, counted] = [fake.posted.length, spentOn(home)];
  for (const [tool, args, why] of [
    ["register_name", { name: "ghoul" }, /session keys can only send transfers/],
    ["link_github", { handle: "vlad", gist_id: "ab".repeat(16) }, /needs an owner key: ask the user to link GitHub in their wallet/],
    ["stake", { validator: BOB, amount: "1" }, /session keys can only send transfers/],
    ["send", { to: addressFromPublicKey(kp(12)), amount: "2" }, /the session key has 1 FANG of its 3 FANG allowance left this window\. It resets at block 105/],
    ["send", { to: addressFromPublicKey(kp(12)), amount: "0.95" }, /account-opening burn \(0\.1 FANG\)/],
  ] as const) {
    r = await s.call(tool, args);
    assert.ok(r.error && /Refused before signing: nothing was signed or sent/.test(r.text) && why.test(r.text), `${tool}: ${r.text}`);
  }
  assert.deepEqual([fake.posted.length, spentOn(home)], [before, counted], "nothing signed or sent, spent_today unchanged");
  r = await s.call("send", { to: AGENT, amount: "0.5" });
  assert.ok(!r.error && fake.posted.at(-1)!.from === BOB, r.text);
  // Expired, or removed: refused too.
  fake.accounts.set(BOB, { ...acct(BOB), keys: [{ ...session, role: { ...session.role, expires_at: 9 } }] });
  r = await s.call("send", { to: AGENT, amount: "0.1" });
  assert.ok(r.error && /expired at block 9/.test(r.text), r.text);
  fake.accounts.set(BOB, { ...acct(BOB), keys: [] });
  r = await s.call("send", { to: AGENT, amount: "0.1" });
  assert.ok(r.error && new RegExp(`This agent's key \\(${checkCode(me.key_id)}\\) isn't on ${BOB}`).test(r.text), r.text);
  // Not authorized: the grant code comes with the check code to compare (and the wallet's address on known networks).
  r = await s.call("use_account", { account: BOB });
  u = r.json();
  assert.ok(u.status === "not_authorized_yet" && u.check_code === checkCode(me.key_id) && u.next.includes(checkCode(me.key_id)) && typeof u.grant_code === "string", r.text);
  assert.equal(readFileSync(u.grant_file, "utf8").trim(), u.grant_code, "the grant code in a file, to copy whole");
  assert.equal(statSync(u.grant_file).mode & 0o777, 0o600);
  assert.ok(u.copy_it.endsWith(` < ${u.grant_file}`), u.copy_it);
  // No account at the address: no grant code at all.
  r = await s.call("use_account", { account: addressFromPublicKey(kp(13)) });
  assert.ok(r.json().status === "no_account" && r.json().grant_code === undefined, r.text);
  r = await s.call("use_account", { account: "self" });
  assert.equal(r.json().acting_for, AGENT);
  console.log("✓ refused before signing (nothing signed, nothing counted): a session key's names, stakes and GitHub links, past its allowance or its expiry, a removed key, FANG that's locked; use_account gives a check code, and no grant for an account that doesn't exist");
}

// ---- 6. nonces ----

{
  fake.accounts.set(AGENT, { balance: 100n * FANG, nonce: acct(AGENT).nonce, exists: true, transferable: 100n * FANG });
  // Every /api/account answer takes a moment: builds made in parallel would read the same nonce.
  fake.accountDelayMs = 60;
  const refusedBefore = fake.refused.length;
  const other = await server(home);
  // Both sessions ready (pinned, wallet loaded) before they race.
  await other.call("wallet_info");
  const results = await Promise.all([s.call("send", { to: BOB, amount: "0.1" }), other.call("send", { to: BOB, amount: "0.4" }), s.call("send", { to: BOB, amount: "0.2" }), other.call("send", { to: BOB, amount: "0.5" }), s.call("send", { to: BOB, amount: "0.3" })]);
  for (const r of results) assert.ok(!r.error && r.json().block === 11, r.text);
  assert.equal(fake.refused.length, refusedBefore, `no "bad nonce": ${fake.refused.slice(refusedBefore).join("; ")}`);
  const nonces = fake.posted.filter((t) => t.from === AGENT).map((t) => t.nonce);
  assert.equal(new Set(nonces).size, nonces.length, "every nonce once");
  await other.close();
  fake.accountDelayMs = 0;
  // A node whose next_nonce leaves out its mempool: the next tx takes the last pending one's nonce + 1.
  fake.lag = true;
  fake.run = "never";
  let r = await s.call("send", { to: BOB, amount: "0.1" });
  assert.equal(r.json().status, "pending", r.text);
  const first = fake.posted.at(-1)!;
  r = await s.call("send", { to: BOB, amount: "0.1" });
  assert.ok(!r.error && r.json().status === "pending" && fake.posted.at(-1)!.nonce === first.nonce + 1, r.text);
  assert.equal(fake.refused.length, refusedBefore);
  // They run: the node's next_nonce is right again.
  for (const tx of fake.pending) fake.done.set(txId(tx), tx);
  fake.accounts.set(AGENT, { ...acct(AGENT), nonce: acct(AGENT).nonce + fake.pending.length });
  fake.pending = [];
  fake.lag = false;
  fake.run = "now";
  r = await s.call("send", { to: BOB, amount: "0.1" });
  assert.ok(!r.error && r.json().block === 11, r.text);
  // A node whose account read trails the block that just ran: a tx that ran keeps its nonce spent.
  fake.trail = true;
  for (const amount of ["0.1", "0.2"]) {
    r = await s.call("send", { to: BOB, amount });
    assert.ok(!r.error && r.json().block === 11, r.text);
  }
  assert.equal(fake.refused.length, refusedBefore);
  fake.trail = false;
  console.log("✓ nonces: three parallel sends in one session and two from another session all finalize, each with its own nonce; a node whose view lags its mempool, or trails a block that just ran, doesn't get a used nonce again");
}

// ---- 7. your own node, a chain without a faucet, copy and ergonomics ----

{
  // A chain whose genesis has no faucet: the node says so, and the answer says what to do instead.
  let r = await s.call("faucet");
  assert.ok(!r.error && r.json().status === "no_faucet" && /This chain has no faucet/.test(r.json().summary) && fake.faucetHits === 1, r.text);
  r = await s.call("send", { to: AGENT, amount: "1" });
  assert.ok(r.error && /is the account this agent acts for.*Nothing was signed/.test(r.text), r.text);
  r = await s.call("send", { to: TBOO_USER, amount: "1" });
  assert.ok(r.error && /is a testnet address \(tboo1…\), and this MCP is on local dev node, whose addresses start with boo1/.test(r.text), r.text);
  // Hunts: who awards them, when they expire, and where to look when there are none.
  let tools = await s.tools();
  const describe = (n: string) => (tools as { name: string; description?: string }[]).find((t) => t.name === n)?.description ?? "";
  assert.match(describe("list_hunts"), /creator awards the bounty after the PR merges, to the account linked to the PR author's GitHub/);
  assert.match(describe("register_name"), /the price is paid to the treasury/);
  assert.match(describe("renew_name"), /paid to the treasury/);
  assert.doesNotMatch(describe("register_name") + describe("renew_name"), /burned/);
  assert.match(describe("submit_contribution"), /returned unless validators refute the claim .* or the jury marks it spam/);
  assert.doesNotMatch(describe("submit_contribution"), /refundable/);
  assert.match(describe("link_github"), /needs an owner key.*ask the user to link GitHub in their wallet/);
  assert.match(describe("send"), /burns the account creation fee/);
  const sendTool = (await (async () => tools)()).find((t) => t.name === "send") as { inputSchema?: { properties?: Record<string, { description?: string }> } } | undefined;
  assert.match(sendTool?.inputSchema?.properties?.memo?.description ?? "", /public and permanent/);
  // Annotations: reads are read-only; what signs is destructive.
  const ann = (n: string) => tools.find((t) => t.name === n)?.annotations ?? {};
  assert.deepEqual([ann("wallet_info").readOnlyHint, ann("send").destructiveHint, ann("send").readOnlyHint, ann("vote").destructiveHint], [true, true, false, true]);
  assert.equal(tools.find((t) => t.name === "send")?.title, "Send FANG");
  r = await s.call("list_hunts");
  assert.ok(!r.error && r.json().hunts.length === 0 && /contribution_pool lists them.*familiar-friendly/.test(r.json().next), r.text);
  fake.hunts = [{ id: 7, title: "Fix it", amount_display: "10 FANG", issue_url: "https://github.com/a/b/issues/7", funders: [], expires_at: 5000 }];
  r = await s.call("list_hunts");
  assert.ok(r.json().hunts[0].expires_at_block === 5000 && r.json().next === undefined, r.text);
  // A claim whose GitHub link doesn't verify loses its bond: refused unless forced.
  fake.accounts.set(AGENT, { ...acct(AGENT), github: { handle: "vlad", gist_id: "ab".repeat(16) } });
  const before = fake.posted.length;
  r = await s.call("submit_contribution", { repo: "a/b", pr: 1 });
  assert.ok(r.error && /@vlad's link to .* doesn't verify \(gist doesn't contain.*pass force: true.*Nothing was signed/.test(r.text), r.text);
  assert.equal(fake.posted.length, before);
  r = await s.call("submit_contribution", { repo: "a/b", pr: 1, force: true });
  assert.ok(!r.error && fake.posted.at(-1)!.action.type === "submit_claim", r.text);
  fake.githubVerified = true;
  r = await s.call("submit_contribution", { repo: "a/b", pr: 2 });
  assert.ok(!r.error, r.text);
  fake.accounts.set(AGENT, { ...acct(AGENT), github: null });
  r = await s.call("submit_contribution", { repo: "a/b", pr: 3 });
  assert.ok(r.error && /has no GitHub link/.test(r.text), r.text);
  await s.close();
  // Your own node of the testnet: the public RPC is a witness by default, and names are proven
  // against it. (status reads only this node; nothing here reaches the public testnet.)
  const testnet = KNOWN_CLOCKS["blockula-testnet-2"];
  fake.status = { chain_id: "blockula-testnet-2", genesis_hash: KNOWN_GENESIS["blockula-testnet-2"], address_prefix: "tboo", height: Math.min(10, heightLimit(testnet.createdAtMs, testnet.blockTimeMs, Date.now())) };
  s = await server(join(dir, "own-node"), {}, ["--network", "testnet"]);
  r = await s.call("status");
  assert.deepEqual([r.json().network, r.json().witnesses, r.json().names], ["your node on the public testnet", [TESTNET_RPC], `proven against 1 node (${TESTNET_RPC})`], r.text);
  await s.close();
  s = await server(join(dir, "own-node"), { BLOCKULA_WITNESSES: "none" }, ["--network", "testnet"]);
  r = await s.call("status");
  assert.deepEqual([r.json().witnesses, r.json().names.startsWith("UNPROVEN: one node's word")], [[], true], r.text);
  await s.close();
  // A local node configured without --network is your node on the testnet once it reports that chain, with the same witness.
  s = await server(join(dir, "own-node"));
  r = await s.call("status");
  assert.deepEqual([r.json().network, r.json().witnesses], ["your node on the public testnet", [TESTNET_RPC]], r.text);
  await s.close();
  fake.status = {};
  console.log("✓ your own node: the network's public RPC is its witness by default (BLOCKULA_WITNESSES=none opts out); a chain without a faucet says so; copy: treasury, public memos, Hunt awards and expiry, bonds, GitHub links; no self-sends or other networks' addresses; tool titles and annotations");
}

node.close();
node.closeAllConnections();
rmSync(dir, { recursive: true, force: true });
