// Offline tests of X account linking (src/xlink.ts, docs/X-LINKING.md): the proof code's format
// and what it binds, the record's memo, which transactions count as records (and unlinks), the
// record's signature, reading X's oEmbed answer, and the whole check against a fake node and a
// fake X. No node and no network needed: `npm run test:vectors -w @blockula/sdk`.
import assert from "node:assert/strict";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { hexToBytes } from "@noble/hashes/utils.js";
import {
  Client, Wallet, X_CODE_PREFIX, X_OEMBED, X_UNLINK_MEMO, checkXProof, fetchXPost, isXHandle, parseXMemo, parseXOEmbed, sameXHandle, signingBytes, verifyXLink, xHandle, xIntentUrl, booProfileUrl, xLinkCode, xLinkFrom, xLinkMemo,
  xPostRef, xPostUrl, xProofPost, xRecordIn, xRecordOf, type Tx, type XRecordTx,
} from "../src/index.js";

const GENESIS = "6e".repeat(32);
const CHAIN = { chainId: "unit", genesisHash: GENESIS };
const RPC = "http://fake.node";

// ---- handles and post links, as typed ----
{
  for (const [typed, handle] of [["BlockulaChain", "BlockulaChain"], [" @BlockulaChain ", "BlockulaChain"], ["https://x.com/BlockulaChain", "BlockulaChain"], ["https://twitter.com/jack?lang=en", "jack"], ["a_1", "a_1"]]) assert.equal(xHandle(typed), handle, typed);
  for (const bad of ["", "@", "sixteen_chars_xx", "has space", "dot.boo", "naïve", "https://example.com/jack", "https://x.com/i/status/20", "https://x.com/home", "javascript:alert(1)", "<b>"]) assert.throws(() => xHandle(bad), /X handle|x\.com|X profile/, bad);
  assert.ok(isXHandle("jack") && !isXHandle("@jack") && !isXHandle(""));
  assert.ok(sameXHandle("BlockulaChain", "blockulachain") && !sameXHandle("jack", "jack_"));

  for (const [typed, ref] of [
    ["https://x.com/BlockulaChain/status/1234567890123456789", { id: "1234567890123456789", handle: "BlockulaChain" }],
    ["https://twitter.com/jack/status/20?s=20&t=abc", { id: "20", handle: "jack" }],
    ["x.com/jack/status/20/photo/1", { id: "20", handle: "jack" }],
    ["https://mobile.twitter.com/jack/statuses/20", { id: "20", handle: "jack" }],
    ["https://x.com/i/status/20", { id: "20" }],
    ["https://x.com/i/web/status/20", { id: "20" }],
    [" 20 ", { id: "20" }],
  ] as const) assert.deepEqual(xPostRef(typed), ref, typed);
  for (const bad of ["", "0", "abc", "https://x.com/jack", "https://x.com/jack/status/", "https://x.com/jack/status/abc", "https://evil.example/jack/status/20", "https://x.com.evil.example/jack/status/20", "https://x.com/jack/status/0", "ftp://x.com/jack/status/20", "123456789012345678901"])
    assert.throws(() => xPostRef(bad), /post|x\.com/, bad);
  assert.equal(xPostUrl("20"), "https://x.com/i/status/20");
  assert.equal(xPostUrl("20", "jack"), "https://x.com/jack/status/20");
  console.log("✓ handles and post links are read from what people paste, and nothing else is");
}

// ---- the proof code ----
const me = Wallet.fromSeed("07".repeat(32), new Client(RPC));
const ME = me.address;
{
  const code = xLinkCode(CHAIN, ME, "BlockulaChain");
  // A known answer: any change to the format changes this, and breaks every post already made.
  assert.equal(ME, "boo1k3cts6u53yggzevug89yxy5u5n73xmxjglkt6zua8uph75s4lmwshek62a");
  assert.equal(code, "blockula-x-a227e7769e11ffe0f87e1da64c6cf250997adb3f");
  assert.match(code, /^blockula-x-[0-9a-f]{40}$/);
  assert.ok(code.startsWith(X_CODE_PREFIX));
  assert.equal(xLinkCode(CHAIN, ME, "blockulachain"), code, "X ignores a handle's case, so the code does");
  const other = Wallet.fromSeed("08".repeat(32)).address;
  const variants = [xLinkCode({ ...CHAIN, chainId: "unit-2" }, ME, "BlockulaChain"), xLinkCode({ ...CHAIN, genesisHash: "6f".repeat(32) }, ME, "BlockulaChain"), xLinkCode(CHAIN, other, "BlockulaChain"), xLinkCode(CHAIN, ME, "BlockulaChain_")];
  assert.equal(new Set([code, ...variants]).size, 5, "another chain id, genesis, account or handle is another code");
  // Fields are length-prefixed: moving a character between two of them changes the code.
  assert.notEqual(xLinkCode({ chainId: "unit6", genesisHash: GENESIS }, ME, "a"), xLinkCode({ chainId: "unit", genesisHash: GENESIS }, ME, "a"));
  assert.throws(() => xLinkCode(CHAIN, "vlad.boo", "jack"), /address, never a name/);
  assert.throws(() => xLinkCode({ chainId: "unit", genesisHash: "6e" }, ME, "jack"), /genesis hash/);
  assert.throws(() => xLinkCode({ chainId: "", genesisHash: GENESIS }, ME, "jack"), /chain id/);
  assert.throws(() => xLinkCode(CHAIN, ME, "@jack"), /X handle/);

  const text = xProofPost(code);
  assert.equal(text, `Verifying my Blockula account on blockula.app.\n\n${code}`);
  assert.ok(text.length <= 280, "fits a post");
  assert.ok(!/[@#$]/.test(text), "nothing X would turn into a mention or a tag");
  assert.deepEqual(text.match(/[a-z0-9-]+(\.[a-z0-9-]+)+/gi), ["blockula.app"], "the only thing X can turn into a link is blockula.app: never a .boo name, which is a web domain too");
  // With a name and a public node, the link is the account's page on that chain.
  assert.equal(booProfileUrl("https://demo.blockula.app", "vlad.boo"), "https://demo.blockula.app/boo/vlad");
  assert.equal(booProfileUrl("https://chain.example/base/", "Vlad"), "https://chain.example/base/boo/vlad");
  for (const [node, name] of [["http://localhost:7070", "vlad.boo"], ["http://fake.node", "vlad.boo"], ["https://u:p@chain.example", "vlad.boo"], ["not a url", "vlad.boo"], ["https://chain.example", undefined], ["https://chain.example", "v"], ["https://chain.example", "-vlad"], ["https://chain.example", "vl/ad"], ["https://chain.example", "vlad.boo.boo"]] as const)
    assert.equal(booProfileUrl(node, name), null, `${node} ${name}`);
  const named = xProofPost(code, booProfileUrl("https://demo.blockula.app", "vlad.boo"));
  assert.equal(named, `Verifying my Blockula account: https://demo.blockula.app/boo/vlad\n\n${code}`);
  assert.ok(!/\.boo\b/.test(named) && !/[@#$]/.test(named) && xProofPost(code, booProfileUrl("https://a-long-host-name.chains.example.org", "a".repeat(32))).length <= 280);
  assert.equal(new URL(xIntentUrl(text)).searchParams.get("text"), text);
  assert.equal(new URL(xIntentUrl(text)).origin + new URL(xIntentUrl(text)).pathname, "https://x.com/intent/post");
  console.log("✓ the proof code binds the chain, the account and the handle");
}

// ---- the record's memo ----
{
  assert.equal(xLinkMemo("BlockulaChain", "1234567890123456789"), "x-link:v1:BlockulaChain:1234567890123456789");
  assert.ok(new TextEncoder().encode(xLinkMemo("a".repeat(15), "9".repeat(20))).length <= 256, "the longest record fits a memo");
  assert.deepEqual(parseXMemo("x-link:v1:BlockulaChain:20"), { type: "link", handle: "BlockulaChain", postId: "20" });
  assert.deepEqual(parseXMemo(X_UNLINK_MEMO), { type: "unlink" });
  assert.equal(X_UNLINK_MEMO, "x-unlink:v1");
  for (const not of ["", "hi", "x-link:v1:jack", "x-link:v2:jack:20", "x-link:v1:jack:20 ", " x-link:v1:jack:20", "x-link:v1:@jack:20", "x-link:v1:jack:020", "x-link:v1:jack:20:extra", "x-link:v1:sixteen_chars_xx:20", "X-LINK:v1:jack:20", "x-link:v1:jack:20\nx-unlink:v1", "x-unlink:v1 ", "x-unlink:v1:jack", "pay me x-link:v1:jack:20", null, undefined, 7, { type: "link" }])
    assert.equal(parseXMemo(not), null, JSON.stringify(not));
  assert.throws(() => xLinkMemo("@jack", "20"), /X handle/);
  assert.throws(() => xLinkMemo("jack", "https://x.com/jack/status/20"), /post id/);
  console.log("✓ a memo is a record only when it's exactly one");
}

// ---- which transactions are records ----
const OWNER = me.keyId;
const AGENT = "aa".repeat(32);
const OTHER = Wallet.fromSeed("08".repeat(32)).address;
let n = 0;
const row = (memo: string, o: Partial<XRecordTx> = {}): XRecordTx => ({ id: `tx${++n}`, kind: "transfer", status: "finalized", error: null, height: 100 - n, from: { address: ME }, to: { address: ME }, signer_key: OWNER, memo, ...o });
{
  const link = row("x-link:v1:BlockulaChain:20");
  assert.deepEqual(xRecordOf(link, ME, [OWNER]), { type: "link", handle: "BlockulaChain", postId: "20", tx: link.id, height: link.height, signer: OWNER });
  assert.deepEqual(xRecordOf(row(X_UNLINK_MEMO, { id: "u", height: 9 }), ME, [OWNER]), { type: "unlink", tx: "u", height: 9, signer: OWNER });
  // The node's older shape: the kind under `action.type`.
  assert.ok(xRecordOf(row("x-link:v1:jack:20", { kind: undefined, action: { type: "transfer" } }), ME, [OWNER]));

  const not: [string, XRecordTx][] = [
    ["a payment to someone else with the memo", row("x-link:v1:jack:20", { to: { address: OTHER } })],
    ["someone else's payment to the account with the memo", row("x-link:v1:jack:20", { from: { address: OTHER } })],
    ["a session (agent) key's", row("x-link:v1:jack:20", { signer_key: AGENT })],
    ["a key no longer on the account", row("x-link:v1:jack:20", { signer_key: "bb".repeat(32) })],
    ["one with no signer shown", row("x-link:v1:jack:20", { signer_key: undefined })],
    ["a failed one", row("x-link:v1:jack:20", { status: "failed", error: "insufficient balance" })],
    ["a pending one", row("x-link:v1:jack:20", { status: "pending", height: null })],
    ["another kind of tx", row("x-link:v1:jack:20", { kind: "register_name" })],
    ["an unsigned ghost tx (no sender)", row("x-link:v1:jack:20", { from: null })],
    ["one with any other memo", row("gm")],
    ["an unlink with something after it", row("x-unlink:v1 please")],
  ];
  for (const [what, t] of not) assert.equal(xRecordOf(t, ME, [OWNER]), null, what);

  // Newest first: the newest record is the one in force.
  const history = [row("gm"), row("x-link:v1:jack:30", { signer_key: AGENT }), row("x-link:v1:BlockulaChain:20", { id: "newest" }), row(X_UNLINK_MEMO), row("x-link:v1:old_handle:10")];
  assert.equal(xRecordIn(history, ME, [OWNER])?.tx, "newest");
  assert.equal(xRecordIn([row("gm"), row(X_UNLINK_MEMO), row("x-link:v1:jack:20")], ME, [OWNER])?.type, "unlink", "an unlink after a link ends it");
  assert.equal(xRecordIn([row("x-link:v1:jack:20")], ME, []), null, "an account with no owner keys has no records");
  assert.equal(xRecordIn([], ME, [OWNER]), null);

  const l = xLinkFrom(xRecordOf(link, ME, [OWNER]) as never, ME, CHAIN);
  assert.deepEqual(l, { account: ME, handle: "BlockulaChain", post_id: "20", post_url: "https://x.com/BlockulaChain/status/20", profile_url: "https://x.com/BlockulaChain", code: xLinkCode(CHAIN, ME, "BlockulaChain"), tx: link.id, height: link.height, signer: OWNER });
  console.log("✓ a record is the account's own transfer to itself, signed by an owner key; the newest one wins");
}

// ---- X's oEmbed answer ----
const CODE = xLinkCode(CHAIN, ME, "BlockulaChain");
const embed = (o: { handle?: string; id?: string; text?: string; name?: string } = {}) => {
  const { handle = "BlockulaChain", id = "20", text = `Verifying my Blockula account on <a href="https://t.co/abc123">blockula.app</a>.<br><br>${CODE}`, name = "Blockula" } = o;
  return {
    url: `https://x.com/${handle}/status/${id}`,
    author_name: name,
    author_url: `https://x.com/${handle}`,
    html: `<blockquote class="twitter-tweet"><p lang="en" dir="ltr">${text}</p>&mdash; ${name} (@${handle}) <a href="https://x.com/${handle}/status/${id}?ref_src=twsrc%5Etfw">November 12, 2026</a></blockquote>\n\n`,
    provider_name: "X",
    type: "rich",
    version: "1.0",
  };
};
{
  const post = parseXOEmbed(embed());
  assert.deepEqual(post, { id: "20", handle: "BlockulaChain", untrusted_text: `Verifying my Blockula account on blockula.app.\n\n${CODE}` });
  // The real thing, as publish.x.com answered for the first post ever (2026-11).
  const first = parseXOEmbed({ url: "https://x.com/jack/status/20", author_name: "jack", author_url: "https://x.com/jack", html: '<blockquote class="twitter-tweet"><p lang="en" dir="ltr">just setting up my twttr</p>&mdash; jack (@jack) <a href="https://x.com/jack/status/20?ref_src=twsrc%5Etfw">March 21, 2006</a></blockquote>\n\n' });
  assert.deepEqual(first, { id: "20", handle: "jack", untrusted_text: "just setting up my twttr" });
  // twitter.com answers (older mirrors) read the same.
  assert.equal(parseXOEmbed({ ...embed(), url: "https://twitter.com/BlockulaChain/status/20", author_url: "https://twitter.com/BlockulaChain" }).handle, "BlockulaChain");
  // Links, mentions and entities in the text come out as text.
  assert.equal(parseXOEmbed(embed({ text: 'a &amp; b &lt;c&gt; &quot;d&quot; &#39;e&#39; &#x1F987; <a href="https://t.co/x">alice.boo</a> <a href="https://x.com/jack">@jack</a>' })).untrusted_text, 'a & b <c> "d" \'e\' 🦇 alice.boo @jack');

  const bad: [string, unknown, RegExp][] = [
    ["not an object", "<html>", /isn't a post/],
    ["null", null, /isn't a post/],
    ["no author", { ...embed(), author_url: undefined }, /who posted/],
    ["an author on another site", { ...embed(), author_url: "https://evil.example/BlockulaChain" }, /who posted/],
    ["an author URL that isn't a profile", { ...embed(), author_url: "https://x.com/BlockulaChain/status/20" }, /who posted/],
    ["a plain-http author", { ...embed(), author_url: "http://x.com/BlockulaChain" }, /who posted/],
    ["no post URL", { ...embed(), url: undefined }, /which post/],
    ["a post URL under another handle", { ...embed(), url: "https://x.com/someone_else/status/20" }, /two different authors/],
    ["no html", { ...embed(), html: undefined }, /no post text/],
    ["html without the post", { ...embed(), html: "<blockquote>gone</blockquote>" }, /no post text/],
  ];
  for (const [what, body, why] of bad) assert.throws(() => parseXOEmbed(body), why, what);
  console.log("✓ a post's author comes from X's author_url, its text from the embed's own paragraph");
}

// ---- comparing a link with its post ----
const LINK = { handle: "BlockulaChain", post_id: "20", code: CODE };
{
  assert.deepEqual(checkXProof(LINK, parseXOEmbed(embed())), { verified: true, reason: `@BlockulaChain posted ${CODE}` });
  assert.ok(checkXProof({ ...LINK, handle: "blockulachain" }, parseXOEmbed(embed())).verified, "case of the handle is ignored");
  const no = (o: Parameters<typeof embed>[0], why: RegExp, link = LINK) => {
    const c = checkXProof(link, parseXOEmbed(embed(o)));
    assert.equal(c.verified, false);
    assert.match(c.reason, why);
  };
  // Someone else posts the right code: it's the account's code, but not that handle's post.
  no({ handle: "impostor" }, /by @impostor, not @BlockulaChain/);
  // The right author, another post.
  no({ id: "21" }, /post 21, not the recorded post 20/);
  // The right author, no code; or the code for another account, another chain, another handle.
  no({ text: "gm" }, /doesn't contain/);
  no({ text: xLinkCode(CHAIN, OTHER, "BlockulaChain") }, /doesn't contain/);
  no({ text: xLinkCode({ ...CHAIN, chainId: "blockula-1" }, ME, "BlockulaChain") }, /doesn't contain/);
  no({ text: xLinkCode(CHAIN, ME, "impostor") }, /doesn't contain/);
  // A cut-off code isn't the code.
  no({ text: CODE.slice(0, -1) }, /doesn't contain/);
  // The code in the author's display name (the line under the post) doesn't count: only the post's text does.
  no({ text: "gm", name: CODE }, /doesn't contain/);
  // A post that quotes the handle it wants to be, in its own text.
  no({ handle: "impostor", text: `${CODE} — Blockula (@BlockulaChain)` }, /by @impostor/);
  console.log("✓ verified only when the recorded post, by the recorded handle, contains this account's code");
}

// ---- a fake X and a fake node ----
const posts = new Map<string, unknown>([["20", embed()], ["21", embed({ id: "21", handle: "impostor" })]]);
const asked: string[] = [];
const history: XRecordTx[] = [];
const posted: Tx[] = [];
let keys = [{ key_id: OWNER, label: "original key", role: { type: "owner" } }];
globalThis.fetch = (async (input: string | URL, init?: RequestInit) => {
  const url = new URL(String(input));
  const reply = (status: number, body: unknown) => new Response(typeof body === "string" ? body : JSON.stringify(body), { status, headers: { "content-type": typeof body === "string" ? "text/html" : "application/json" } });
  if (url.origin === new URL(X_OEMBED).origin || url.host === "mirror.example") {
    asked.push(String(url));
    const id = xPostRef(url.searchParams.get("url") ?? "").id;
    if (id === "500") return reply(500, "<html>over capacity</html>");
    if (id === "77") return reply(200, "<html>not json</html>");
    if (id === "78") return reply(200, embed({ id: "79" }));
    return posts.has(id) ? reply(200, posts.get(id)) : reply(404, "<!DOCTYPE html><html><title>X / ?</title></html>");
  }
  if (url.host !== "fake.node") throw new TypeError("fetch failed");
  const path = decodeURIComponent(url.pathname.replace(/^\/api/, ""));
  if (path === "/status") return reply(200, { chain_id: "unit", genesis_hash: GENESIS, address_prefix: "boo", height: 50, max_tx_validity_blocks: 3600 });
  if (path === "/fees") return reply(200, { height: 50, lag: 17, rate_scale: 1000, slots: [], suggested: { flat: "10000", rate: "10000" }, floor: { flat: "10000", per_byte: "10" }, account_creation_fee: "100000000" });
  if (path === `/account/${ME}` || path === "/account/vlad.boo") return reply(200, { account: { address: ME, name: "vlad.boo" }, names: ["vlad.boo"], next_nonce: posted.length, nonce: posted.length, keys, recent_txs: [] });
  if (path === `/account/${ME}/txs`) {
    // Newest first, two to a page, as the node pages (`before` is the cursor it handed out).
    const from = Number(url.searchParams.get("before") ?? 0);
    const txs = history.slice(from, from + 2);
    return reply(200, { txs, next: from + 2 < history.length ? String(from + 2) : null, history_from: 1 });
  }
  if (init?.method === "POST" && path === "/tx") {
    const tx = JSON.parse(String(init.body)) as Tx;
    posted.push(tx);
    // It runs: the node shows it at the top of the account's history.
    history.unshift({ id: `id${posted.length}`, kind: "transfer", status: "finalized", error: null, height: 50 + posted.length, from: { address: tx.from }, to: { address: (tx.action as { to: string }).to }, signer_key: me.keyId, memo: tx.memo });
    return reply(200, { id: `id${posted.length}` });
  }
  if (path.startsWith("/tx/")) return reply(200, { id: path.slice(4), status: "finalized", summary: "ok" });
  return reply(404, { error: `fake node: no route for ${path}` });
}) as typeof fetch;

{
  const post = await fetchXPost("20");
  assert.equal(post.handle, "BlockulaChain");
  // What X is asked: the post's id, under no handle, and nothing about the Blockula account.
  const q = new URL(asked.at(-1)!);
  assert.equal(q.origin + q.pathname, "https://publish.x.com/oembed");
  assert.deepEqual(Object.fromEntries(q.searchParams), { url: "https://x.com/i/status/20", omit_script: "1", dnt: "true" });
  assert.ok(!String(q).includes(ME) && !String(q).includes("BlockulaChain"));
  await assert.rejects(fetchXPost("404"), /no public post 404 \(deleted, or from a private account\)/);
  await assert.rejects(fetchXPost("500"), /X answered 500/);
  await assert.rejects(fetchXPost("77"), /isn't JSON/);
  await assert.rejects(fetchXPost("78"), /another post \(79\)/);
  await assert.rejects(fetchXPost("https://x.com/jack/status/20"), /post id is a number/);
  assert.equal((await fetchXPost("20", { endpoint: "https://mirror.example/oembed" })).id, "20");
  assert.ok(asked.at(-1)!.startsWith("https://mirror.example/oembed?"));

  assert.deepEqual(await verifyXLink(LINK), { verified: true, reason: `@BlockulaChain posted ${CODE}`, post });
  assert.deepEqual(await verifyXLink({ ...LINK, post_id: "404" }), { verified: false, reason: "X has no public post 404 (deleted, or from a private account)" });
  assert.match((await verifyXLink({ ...LINK, post_id: "21" })).reason, /by @impostor, not @BlockulaChain/);
  console.log("✓ a post is fetched by id from X's public endpoint; a missing one is unverified, not an error");
}

// ---- the wallet's helpers, end to end against the fake node ----
{
  const client = me.client;
  assert.equal(await client.xLink(ME), null, "nothing linked yet");

  const proof = await me.xProof("@BlockulaChain");
  assert.deepEqual(proof, { handle: "BlockulaChain", code: CODE, text: `Verifying my Blockula account on blockula.app.\n\n${CODE}`, intent_url: xIntentUrl(`Verifying my Blockula account on blockula.app.\n\n${CODE}`) });
  assert.equal(posted.length, 0, "the proof signs nothing");

  // Refused before anything is signed.
  await assert.rejects(me.linkX("BlockulaChain", "https://x.com/impostor/status/21"), /under @impostor, not @BlockulaChain.*Nothing was signed/);
  await assert.rejects(me.linkX("not a handle", "20"), /X handle/);
  await assert.rejects(me.linkX("BlockulaChain", "https://example.com/BlockulaChain/status/20"), /isn't to x\.com/);
  keys = [{ key_id: OWNER, label: "agent", role: { type: "session" } } as never];
  await assert.rejects(me.linkX("BlockulaChain", "20"), /only an owner key can link an X account: this is a session key.*Nothing was signed/);
  await assert.rejects(me.unlinkX(), /only an owner key can unlink an X account/);
  keys = [];
  await assert.rejects(me.linkX("BlockulaChain", "20"), /this key isn't on the account/);
  keys = [{ key_id: OWNER, label: "original key", role: { type: "owner" } }];
  assert.equal(posted.length, 0);

  // The record: the account pays itself 0 with the memo, and its signature covers all of it.
  await me.linkX("@BlockulaChain", "https://x.com/BlockulaChain/status/20?s=20");
  const tx = posted[0];
  assert.deepEqual(tx.action, { type: "transfer", to: ME, amount: "0" });
  assert.equal(tx.memo, "x-link:v1:BlockulaChain:20");
  assert.deepEqual([tx.chain_id, tx.genesis_hash, tx.from, tx.nonce], ["unit", GENESIS, ME, 0]);
  const sig = hexToBytes(tx.signature);
  assert.ok(ml_dsa65.verify(sig, signingBytes(tx), hexToBytes(tx.public_key)), "the record's signature verifies");
  // The signature binds the handle, the post, the account, the chain and the nonce: change any, and it fails.
  for (const forged of [{ ...tx, memo: "x-link:v1:impostor:20" }, { ...tx, memo: "x-link:v1:BlockulaChain:21" }, { ...tx, from: OTHER }, { ...tx, chain_id: "blockula-1" }, { ...tx, genesis_hash: "6f".repeat(32) }, { ...tx, nonce: 1 }, { ...tx, memo: X_UNLINK_MEMO }])
    assert.ok(!ml_dsa65.verify(sig, signingBytes(forged), hexToBytes(tx.public_key)), JSON.stringify(forged).slice(0, 80));

  const link = await client.xLink("vlad.boo");
  assert.deepEqual(link, { account: ME, handle: "BlockulaChain", post_id: "20", post_url: "https://x.com/BlockulaChain/status/20", profile_url: "https://x.com/BlockulaChain", code: CODE, tx: "id1", height: 51, signer: OWNER });
  assert.equal((await verifyXLink(link!)).verified, true);

  // Later payments push the record down the history; it's still found, pages later.
  for (let i = 0; i < 5; i++) history.unshift(row("gm", { id: `later${i}` }));
  assert.equal((await client.xLink(ME))?.tx, "id1");
  assert.equal(await client.xLink(ME, { maxPages: 2 }), null, "not past the pages it was allowed to read");

  // A record for a post by someone else is recorded (the chain doesn't look), and doesn't verify.
  await me.linkX("BlockulaChain", "21");
  const wrong = await client.xLink(ME);
  assert.equal(wrong?.post_id, "21");
  assert.deepEqual(await verifyXLink(wrong!).then((c) => [c.verified, c.reason]), [false, "the post is by @impostor, not @BlockulaChain"]);

  // An agent's (session key's) record in between changes nothing.
  history.unshift(row("x-link:v1:agent_handle:99", { id: "agent", signer_key: AGENT }));
  assert.equal((await client.xLink(ME))?.post_id, "21");

  // Unlink: the same kind of transaction, and the link is gone, older records and all.
  await me.unlinkX();
  assert.deepEqual([posted.at(-1)!.action, posted.at(-1)!.memo], [{ type: "transfer", to: ME, amount: "0" }, "x-unlink:v1"]);
  assert.equal(await client.xLink(ME), null);

  // Linking again after an unlink works; removing the owner key that signed drops its records.
  await me.linkX("BlockulaChain", "20");
  assert.equal((await client.xLink(ME))?.post_id, "20");
  keys = [{ key_id: "cc".repeat(32), label: "new phone", role: { type: "owner" } }];
  assert.equal(await client.xLink(ME), null, "records signed by a key no longer on the account don't count");
  console.log("✓ linkX records, xLink reads the newest record, unlinkX ends it");
}

console.log("\n🧛 X linking: all checks passed");
