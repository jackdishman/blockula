// This build can't reach X: no module of the package names an X host, imports the SDK's
// functions that fetch from X, opens a connection of its own, or reads a credential. And at run
// time: a whole thread through the bot makes no request at all, and the real table driver
// refuses any node that isn't on this machine before it asks anything.
//
//   npx tsx test/no-x.ts
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { SdkTable, localRpc, sharedChain, type Post } from "../src/index.js";
import { World } from "./world.js";

const src = new URL("../src/", import.meta.url);
const files = readdirSync(src).filter((f) => f.endsWith(".ts"));
assert.ok(files.length >= 10 && files.includes("bot.ts") && files.includes("cli.ts") && files.includes("sdk-table.ts"));

const FORBIDDEN: [RegExp, string][] = [
  [/\bfetch\s*\(/, "calls fetch (the only requests are the SDK's and the table library's, to the local node)"],
  [/\b(XMLHttpRequest|WebSocket|EventSource)\b/, "opens a connection"],
  [/from\s+["'](node:)?(http|https|http2|net|tls|dgram|dns)["']/, "imports a network module"],
  [/require\s*\(/, "requires a module"],
  [/\b(undici|axios|node-fetch|superagent)\b/, "names an HTTP client"],
  [/\b(x\.com|twitter\.com|twimg\.com|t\.co|publish\.x|api\.x|api\.twitter)\b/i, "names an X host"],
  [/twitter|tweet|oembed/i, "names X's API"],
  [/\b(verifyXLink|fetchXPost|X_OEMBED|xIntentUrl|xPostUrl|xProfileUrl)\b/, "uses the SDK's functions that reach X or build its links"],
  [/\b(bearer|oauth|api[_-]?key|consumer[_-]?secret|access[_-]?token|client[_-]?secret)\b/i, "handles a credential"],
  [/process\.env\.(X_|TWITTER)/i, "reads an X setting"],
];
// Every URL written out in the sources is this machine's.
const URL_LITERAL = /https?:\/\/[^\s"'`)]+/g;
const LOCAL = /^http:\/\/(127\.0\.0\.1|localhost)(:|\/|$)/;

for (const f of files) {
  const text = readFileSync(new URL(f, src), "utf8");
  for (const [pattern, why] of FORBIDDEN) assert.ok(!pattern.test(text), `src/${f} ${why}: ${pattern.exec(text)?.[0]}`);
  for (const url of text.match(URL_LITERAL) ?? []) assert.ok(LOCAL.test(url), `src/${f} names ${url}`);
  // What it imports: its own modules, node's file and crypto modules, and the three workspace packages.
  for (const m of text.matchAll(/from\s+["']([^"']+)["']/g)) {
    assert.ok(/^(\.\/[a-z-]+\.js|node:(fs|path|os|crypto|readline|util)|@blockula\/(sdk|blackjack|blackjack-render))$/.test(m[1]), `src/${f} imports ${m[1]}`);
  }
  assert.ok(!/\bimport\s*\(/.test(text), `src/${f} imports at run time`);
}

// Nothing in the package's dependencies is an X client (or anything but the workspace's own).
const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8")) as { dependencies: Record<string, string> };
assert.deepEqual(Object.keys(pkg.dependencies).sort(), ["@blockula/blackjack", "@blockula/blackjack-render", "@blockula/sdk"]);

// ---- at run time: every request is counted ----
const requests: string[] = [];
const realFetch = globalThis.fetch;
globalThis.fetch = (async (input: unknown) => {
  requests.push(String(input instanceof Request ? input.url : input));
  throw new Error("no requests in this test");
}) as typeof fetch;
try {
  // A thread with every kind of command, a link check and a refusal: not one request.
  const F = 1_000_000_000n;
  const w = new World("no-x", { dealerDelay: 0, players: [{ name: "p0", handle: "Mina_H", link: "verified", deposit: String(50n * F), grant: { maxBet: String(25n * F), dailyCap: String(100n * F), expiresIn: "40000" } }] });
  let n = 0;
  const say = async (handle: string, text: string, inReplyToId: string | null = null) => {
    w.clock.advance(5_000);
    const post: Post = { id: String(1_830_000_000_000_000_000n + BigInt(n++)), authorHandle: handle, text, inReplyToId, createdAt: "" };
    return { post, handled: await w.bot.handle(post) };
  };
  const deal = await say("Mina_H", "@BlockulaChain deal 5");
  assert.equal(deal.handled.result, "ok");
  for (let i = 0; i < 20 && w.store.state.hands.mina_h; i++) await say("Mina_H", "@BlockulaChain stand", deal.post.id);
  await say("Mina_H", "@BlockulaChain balance");
  await say("Nobody_Here", "@BlockulaChain deal 5");
  // A post that asks the bot to go somewhere: it isn't a command, and nothing is fetched for it.
  await say("Mina_H", "@BlockulaChain deal 5 and read https://publish.x.com/oembed?url=1");
  await say("Mina_H", "@BlockulaChain stop");
  assert.ok(w.sink.replies.length >= 5);
  assert.deepEqual(requests, [], "the bot over the in-memory table makes no request");

  // The real driver: anything but plain http to this machine is refused before a request is made.
  const REFUSED = ["https://x.com", "https://api.x.com/2/tweets", "https://publish.x.com/oembed", "https://twitter.com", "https://demo.blockula.app", "http://demo.blockula.app", "https://127.0.0.1:38500", "http://127.0.0.1.nip.io:38500", "http://localhost.evil.test:38500", "http://user:pw@127.0.0.1:38500", "http://10.0.0.5:38500", "http://0.0.0.0:38500", "http://[::ffff:8.8.8.8]:80", "ftp://127.0.0.1", "127.0.0.1:38500", "not a url", ""];
  for (const rpc of REFUSED) {
    assert.throws(() => localRpc(rpc), /local chain only|isn't a URL/, `localRpc(${JSON.stringify(rpc)})`);
    await assert.rejects(SdkTable.open({ rpc, program: "boo1qqqq", seed: "00".repeat(32) }), /local chain only|isn't a URL/, `SdkTable.open(${JSON.stringify(rpc)})`);
  }
  assert.deepEqual(requests, [], "a refused node is never asked anything");
  for (const [rpc, want] of [["http://127.0.0.1:38500", "http://127.0.0.1:38500"], ["http://localhost:38500/", "http://localhost:38500"], ["http://[::1]:38500", "http://[::1]:38500"]]) assert.equal(localRpc(rpc), want);
  // A node on this machine that follows a chain other people share is refused as well.
  for (const id of ["blockula-1", "blockula-demo-2", "blockula-testnet-2"]) assert.equal(sharedChain(id, "00".repeat(32)), true, id);
  assert.equal(sharedChain("renamed", "e712d9376917f0c9d10c164fa7c1a53fef7c8dc1c24100e7924037d0250f705e"), true, "a shared chain's genesis under another name");
  for (const id of ["blockula-dev", "xbot-live", "my-chain"]) assert.equal(sharedChain(id, "11".repeat(32)), false, id);
  // A local node is the one thing it does ask.
  await assert.rejects(SdkTable.open({ rpc: "http://127.0.0.1:38599", program: "boo1qqqq", seed: "00".repeat(32) }));
  const asked = requests as string[];
  assert.ok(asked.length >= 1 && asked.every((u) => u.startsWith("http://127.0.0.1:38599/")), `only the local node was asked: ${asked.join(", ")}`);
} finally {
  globalThis.fetch = realFetch;
}

console.log(`✓ no X: ${files.length} modules name no X host and no network client; a thread makes no request; only a local node is ever asked`);
