// The MCP's network handling (WAL-4) and install_node's release checks, offline:
//   npm test -w @blockula/mcp      (builds first: the last part runs the real server over stdio)
// 1. resolveNetwork: the network comes from --network / BLOCKULA_NETWORK / BLOCKULA_RPC only;
//    with nothing set it's the public testnet, never whatever answers on localhost.
// 2. Pins: chain id, genesis hash, prefix, known genesis per chain id, trust on first use.
// 3. fetchVerifiedRelease: version binding, checksum, provenance and signature outcomes.
// 4. The built server against a fake node: refuses a wrong chain before signing anything,
//    remembers the chain across restarts, and notices a node that switches mid-session.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Client as McpClient } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import { RELEASE_SIGNATURE_FORMAT, releaseMessage } from "@blockula/sdk";
import { KNOWN_GENESIS, KnownChains, LOCAL_RPC, TESTNET_RPC, completePin, effectivePin, mismatch, networkArg, resolveNetwork } from "../src/network.js";
import { checkBinaryVersion, fetchVerifiedRelease, latestTagFrom, releaseKeyIdFrom, type ReleaseDeps } from "../src/release.js";

const G1 = "11".repeat(32);
const G2 = "22".repeat(32);

// ---- 1. choosing the network ----

assert.deepEqual(
  [resolveNetwork({}).key, resolveNetwork({}).rpc, resolveNetwork({}).pin.chainId],
  ["testnet", TESTNET_RPC, "blockula-testnet-2"],
  "nothing configured: the public testnet, never a local node",
);
assert.equal(resolveNetwork({}, ["--network", "local"]).rpc, LOCAL_RPC);
assert.equal(resolveNetwork({}, ["--network=local"]).key, "local");
assert.equal(resolveNetwork({ BLOCKULA_NETWORK: "local" }).key, "local");
assert.equal(networkArg(["--foo", "--network", "mainnet"]), "mainnet");
assert.equal(resolveNetwork({ BLOCKULA_RPC: "http://127.0.0.1:7391/" }).key, "local", "a localhost RPC is the local network");
assert.equal(resolveNetwork({ BLOCKULA_RPC: "http://127.0.0.1:7391/" }).rpc, "http://127.0.0.1:7391");
assert.equal(resolveNetwork({ BLOCKULA_RPC: TESTNET_RPC }).key, "testnet", "the testnet's own URL keeps its pins");
assert.equal(resolveNetwork({ BLOCKULA_RPC: "https://rpc.example" }).key, "custom");
assert.equal(resolveNetwork({ BLOCKULA_RPC: "http://10.0.0.5:7070" }).key, "custom", "plain http on a private network");
const main = resolveNetwork({ BLOCKULA_RPC: "https://rpc.example" }, ["--network", "mainnet"]);
assert.deepEqual([main.key, main.mainnet, main.pin.chainId, main.pin.prefix], ["mainnet", true, "blockula-1", "boo"]);
assert.equal(resolveNetwork({ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_CHAIN_ID: "blockula-1" }).mainnet, true, "a custom node pinned to blockula-1 is real money");
const pinned = resolveNetwork({ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_CHAIN_ID: "x", BLOCKULA_GENESIS_HASH: G1.toUpperCase() });
assert.deepEqual(pinned.pin, { chainId: "x", genesisHash: G1 });
const refusals: [Record<string, string>, string[], RegExp][] = [
  [{}, ["--network", "mainnet"], /no public mainnet RPC/],
  [{}, ["--network", "custom"], /needs BLOCKULA_RPC/],
  [{ BLOCKULA_NETWORK: "moon" }, [], /unknown network/],
  [{ BLOCKULA_RPC: "http://rpc.example" }, [], /must use https/],
  [{ BLOCKULA_RPC: "https://user:pw@rpc.example" }, [], /without credentials/],
  [{ BLOCKULA_RPC: "ftp://rpc.example" }, [], /http\(s\)/],
  [{ BLOCKULA_RPC: "not a url" }, [], /isn't a URL/],
  [{ BLOCKULA_RPC: "https://rpc.example" }, ["--network", "local"], /on this machine/],
  [{ BLOCKULA_CHAIN_ID: "blockula-1" }, ["--network", "testnet"], /contradicts/],
  [{ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_GENESIS_HASH: "abc" }, [], /64 hex/],
  [{ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_WITNESSES: "http://w.example" }, [], /BLOCKULA_WITNESSES must use https/],
  [{ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_WITNESSES: "https://rpc.example/" }, [], /other nodes than BLOCKULA_RPC/],
  [{ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_WITNESSES: "https://w.example,https://w.example" }, [], /twice/],
];
// gap-7-1: witnesses names are proven against.
assert.deepEqual(resolveNetwork({ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_WITNESSES: " https://a.example/ , https://b.example ," }).witnesses, ["https://a.example", "https://b.example"]);
assert.deepEqual(resolveNetwork({}).witnesses, []);
for (const [env, argv, re] of refusals) assert.throws(() => resolveNetwork(env, argv), re, JSON.stringify([env, argv]));
console.log("✓ the network comes from the configuration only (default: the public testnet)");

// ---- 2. pins ----

const status = (chain_id: string, genesis_hash: string, address_prefix = "boo") => ({ chain_id, genesis_hash, address_prefix });
assert.equal(mismatch({}, status("a", G1)), null);
assert.match(mismatch({ chainId: "b" }, status("a", G1)) ?? "", /chain "a", not "b"/);
assert.match(mismatch({ genesisHash: G2 }, status("a", G1)) ?? "", /genesis/);
assert.match(mismatch({ prefix: "tboo" }, status("a", G1)) ?? "", /"boo1…" addresses/);
assert.match(mismatch({}, { genesis_hash: G1 }) ?? "", /chain id/);
assert.equal(mismatch({ genesisHash: G1 }, status("a", G1.toUpperCase())), null, "hex case doesn't matter");
// A known chain id must come with its known genesis, whatever the configuration says.
KNOWN_GENESIS["blockula-1"] = G1;
assert.match(mismatch({}, status("blockula-1", G2)) ?? "", /genesis/);
assert.equal(mismatch({}, status("blockula-1", G1)), null);
assert.equal(resolveNetwork({ BLOCKULA_RPC: "https://rpc.example" }, ["--network", "mainnet"]).pin.genesisHash, G1);
assert.throws(() => resolveNetwork({ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_GENESIS_HASH: G2 }, ["--network", "mainnet"]), /contradicts blockula-1's known genesis/);
delete KNOWN_GENESIS["blockula-1"];
assert.deepEqual(completePin({ chainId: "a" }, status("a", G1.toUpperCase(), "tboo")), { chainId: "a", genesisHash: G1, prefix: "tboo" });
const dir = mkdtempSync(join(tmpdir(), "blockula-mcp-test-"));
const known = new KnownChains(join(dir, "known-chains.json"));
const custom = resolveNetwork({ BLOCKULA_RPC: "https://rpc.example" });
assert.equal(known.get(custom), undefined);
known.save(custom, { chainId: "a", genesisHash: G1 });
assert.deepEqual(new KnownChains(known.file).get(custom), { chainId: "a", genesisHash: G1 });
assert.deepEqual(effectivePin(custom, known.get(custom)), { chainId: "a", genesisHash: G1, prefix: undefined });
assert.deepEqual(effectivePin(pinned, { chainId: "other", genesisHash: G2 }), { chainId: "x", genesisHash: G1, prefix: undefined }, "configuration beats memory");
// The testnet relaunched under a new chain id: what was remembered for the old one (its genesis)
// isn't held against the new one, which is pinned on first use.
const testnet = resolveNetwork({});
assert.deepEqual([testnet.pin.chainId, testnet.pin.prefix, testnet.pin.genesisHash], ["blockula-testnet-2", "tboo", undefined], "its genesis exists only once it launches");
assert.deepEqual(effectivePin(testnet, { chainId: "blockula-testnet", genesisHash: G2, prefix: "boo" }), { chainId: "blockula-testnet-2", genesisHash: undefined, prefix: "tboo" });
assert.deepEqual(effectivePin(testnet, { chainId: "blockula-testnet-2", genesisHash: G1, prefix: "tboo" }), { chainId: "blockula-testnet-2", genesisHash: G1, prefix: "tboo" });
const local = resolveNetwork({}, ["--network", "local"]);
known.save(local, { chainId: "blockula-dev", genesisHash: G1 });
assert.equal(known.get(local), undefined, "dev chains are re-pinned each session (a reset makes a new genesis)");
console.log("✓ pins: chain id, genesis, prefix, known genesis, trust on first use");

// ---- 3. release checks ----

const asset = "blockula-x86_64-unknown-linux-gnu.tar.gz";
const archive = Buffer.from("pretend this is a tarball");
const tag = "v9.9.9";
// checksums.txt as release.yml writes it (scripts/release-checksums.sh): the tag first.
const checksums = (t: string | null) => Buffer.from(`${t ? `# blockula-release ${t}\n` : ""}${createHash("sha256").update(archive).digest("hex")}  ${asset}\n`);
const sums = checksums(tag);
const releaseKey = ml_dsa65.keygen(new Uint8Array(32).fill(7));
const keyId = bytesToHex(blake3(releaseKey.publicKey));
const sig = (file: Buffer, key = releaseKey) => Buffer.from(JSON.stringify({ format: RELEASE_SIGNATURE_FORMAT, key_id: bytesToHex(blake3(key.publicKey)), public_key: bytesToHex(key.publicKey), signature: bytesToHex(ml_dsa65.sign(releaseMessage(file), key.secretKey)) }));
// The fake attestation was made on refs/tags/<attestedTag>; like gh with --source-ref, asking for
// another tag's provenance fails.
const deps = (files: Record<string, Buffer | null>, attest: Awaited<ReturnType<ReleaseDeps["attest"]>>["result"] = "unavailable", attestedTag = tag): ReleaseDeps => ({
  download: async (url) => files[url.split("/").pop() as string] ?? null,
  attest: async (_archive, _name, asked) => (attest === "ok" && asked !== attestedTag ? { result: "failed", detail: `source ref refs/tags/${asked} doesn't match` } : { result: attest, detail: "no attestation found" }),
});
const files = { [asset]: archive, "checksums.txt": sums, "checksums.txt.sig": sig(sums) };
const base = "https://example/release";
const opts = (releaseKeyId: string, requireVerify: boolean, t = tag) => ({ releaseKeyId, requireVerify, tag: t });
const ok = await fetchVerifiedRelease(base, asset, opts(keyId, false), deps(files));
assert.deepEqual(ok.verified, [`release signature (ML-DSA-65 key ${keyId.slice(0, 16)}…)`]);
assert.equal(ok.warning, undefined);
const both = await fetchVerifiedRelease(base, asset, opts(keyId, true), deps(files, "ok"));
assert.equal(both.verified.length, 2, "REQUIRE_VERIFY with a key id: provenance and the signature");
assert.match(both.verified[0], /tag v9\.9\.9/);
const provenanceOnly = await fetchVerifiedRelease(base, asset, opts("", true), deps({ ...files, "checksums.txt.sig": null }, "ok"));
assert.equal(provenanceOnly.verified.length, 1, "before the key ceremony, provenance is what REQUIRE_VERIFY can require");
const onlySum = await fetchVerifiedRelease(base, asset, opts("", false), deps(files));
assert.deepEqual(onlySum.verified, []);
assert.match(onlySum.warning ?? "", /ONLY THE CHECKSUM/);
const other = ml_dsa65.keygen(new Uint8Array(32).fill(8));
const tampered = Buffer.from(archive.toString() + "!");
// supply-chain-B-2: an older release, genuinely signed and attested on its own tag, served as the
// one asked for.
const oldSums = checksums("v9.9.8");
const oldRelease = { ...files, "checksums.txt": oldSums, "checksums.txt.sig": sig(oldSums) };
const untaggedSums = checksums(null);
const untagged = { ...files, "checksums.txt": untaggedSums, "checksums.txt.sig": sig(untaggedSums) };
const releaseRefusals: [string, Promise<unknown>, RegExp][] = [
  ["an older release replayed (signature and its own provenance)", fetchVerifiedRelease(base, asset, opts(keyId, true), deps(oldRelease, "ok", "v9.9.8")), /is for v9\.9\.8, not v9\.9\.9/],
  ["an older release replayed, checksum only", fetchVerifiedRelease(base, asset, opts("", false), deps(oldRelease)), /is for v9\.9\.8, not v9\.9\.9/],
  ["a checksums.txt naming no release", fetchVerifiedRelease(base, asset, opts(keyId, false), deps(untagged)), /names no release/],
  ["provenance from another ref (a branch build)", fetchVerifiedRelease(base, asset, opts(keyId, false), deps(files, "ok", "v9.9.8")), /provenance check FAILED .*from tag v9\.9\.9/],
  ["not a release tag", fetchVerifiedRelease(base, asset, opts(keyId, false, "latest"), deps(files)), /vX\.Y\.Z tag/],
  ["unverifiable with REQUIRE_VERIFY", fetchVerifiedRelease(base, asset, opts("", true), deps(files)), /BLOCKULA_REQUIRE_VERIFY=1/],
  // Audit F69: with a release key id, a release without its signature is refused, even with
  // valid provenance (all a compromised GitHub account or workflow can produce).
  ["no signature file (key id known)", fetchVerifiedRelease(base, asset, opts(keyId, false), deps({ ...files, "checksums.txt.sig": null })), /no checksums.txt.sig/],
  ["no signature file, provenance passing", fetchVerifiedRelease(base, asset, opts(keyId, false), deps({ ...files, "checksums.txt.sig": null }, "ok")), /no checksums.txt.sig/],
  ["no signature file with REQUIRE_VERIFY", fetchVerifiedRelease(base, asset, opts(keyId, true), deps({ ...files, "checksums.txt.sig": null }, "ok")), /no checksums.txt.sig/],
  ["signature without provenance under REQUIRE_VERIFY", fetchVerifiedRelease(base, asset, opts(keyId, true), deps(files)), /requires build provenance as well/],
  ["a tampered archive", fetchVerifiedRelease(base, asset, opts(keyId, false), deps({ ...files, [asset]: tampered })), /checksum mismatch/],
  ["an archive missing from checksums.txt", fetchVerifiedRelease(base, asset, opts(keyId, false), deps({ ...files, "checksums.txt": Buffer.from(`# blockula-release ${tag}\n00  other\n`) })), /checksum mismatch/],
  ["failed provenance", fetchVerifiedRelease(base, asset, opts(keyId, false), deps(files, "failed")), /provenance check FAILED/],
  ["checksums signed by another key", fetchVerifiedRelease(base, asset, opts(keyId, false), deps({ ...files, "checksums.txt.sig": sig(sums, other) })), /signature check FAILED/],
  ["a signature over other checksums", fetchVerifiedRelease(base, asset, opts(keyId, false), deps({ ...files, "checksums.txt.sig": sig(Buffer.from("x")) })), /signature check FAILED/],
  ["a garbled signature file", fetchVerifiedRelease(base, asset, opts(keyId, false), deps({ ...files, "checksums.txt.sig": Buffer.from("{") })), /isn't valid JSON/],
  ["no checksums.txt", fetchVerifiedRelease(base, asset, opts(keyId, false), deps({ ...files, "checksums.txt": null })), /no checksums.txt/],
];
for (const [label, p, re] of releaseRefusals) await assert.rejects(p, re, label);
// `latest` is resolved to its tag before anything is downloaded, and the binary must report it.
assert.equal(latestTagFrom("https://github.com/jackdishman/blockula/releases/tag/v0.5.0"), "v0.5.0");
for (const url of ["https://github.com/jackdishman/blockula/releases/tag/nightly", "https://github.com/jackdishman/blockula/releases", "https://github.com/jackdishman/blockula/releases/tag/v0.5.0/x"]) assert.throws(() => latestTagFrom(url), /isn't a release tag/, url);
checkBinaryVersion("blockula 9.9.9\n", tag);
assert.throws(() => checkBinaryVersion("blockula 9.9.8\n", tag), /is blockula 9\.9\.8, not 9\.9\.9/);
assert.throws(() => checkBinaryVersion("", tag), /unrunnable/);
// F49/F69: the operator's key id (from an independent channel) and a built-in one must agree.
assert.equal(releaseKeyIdFrom(undefined, ""), "");
assert.equal(releaseKeyIdFrom(` ${keyId.toUpperCase()} `, ""), keyId);
assert.equal(releaseKeyIdFrom(undefined, keyId), keyId);
assert.equal(releaseKeyIdFrom(keyId, keyId), keyId);
assert.throws(() => releaseKeyIdFrom("cd".repeat(32), keyId), /isn't BLOCKULA_RELEASE_KEY_ID/);
assert.throws(() => releaseKeyIdFrom("abc", ""), /64 hex characters/);
console.log(`✓ install_node: version binding, checksum, provenance and signature checks (${releaseRefusals.length} bad releases refused)`);

// ---- 4. the server, over stdio, against a fake node ----

let nodeStatus = { chain_id: "fake-chain", genesis_hash: G1, address_prefix: "boo", height: 10, max_tx_validity_blocks: 3600 };
const posted: string[] = [];
const fake: Server = createServer((req, res) => {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    const reply = (code: number, v: unknown) => res.writeHead(code, { "content-type": "application/json" }).end(JSON.stringify(v));
    if (req.url === "/api/status") return reply(200, nodeStatus);
    if (req.method === "POST") posted.push(body);
    if (req.url?.startsWith("/api/account/")) return reply(404, { error: "no account" });
    return reply(404, { error: `fake node: no route for ${req.url}` });
  });
});
await new Promise<void>((r) => fake.listen(0, "127.0.0.1", r));
const rpc = `http://127.0.0.1:${(fake.address() as AddressInfo).port}`;
const home = join(dir, "home");

async function server(env: Record<string, string>) {
  const transport = new StdioClientTransport({ command: process.execPath, args: [new URL("../dist/index.js", import.meta.url).pathname], env: { PATH: process.env.PATH ?? "", BLOCKULA_HOME: home, ...env }, stderr: "ignore" });
  const c = new McpClient({ name: "test", version: "0" });
  await c.connect(transport);
  const call = async (name: string, args: Record<string, unknown> = {}) => {
    const r = (await c.callTool({ name, arguments: args })) as { content: { text: string }[]; isError?: boolean };
    return { error: !!r.isError, text: r.content[0].text };
  };
  return { call, tools: async () => (await c.listTools()).tools as { name: string; description?: string }[], close: () => c.close() };
}

// A wallet made on the right chain …
let s = await server({ BLOCKULA_RPC: rpc });
let r = await s.call("create_wallet");
assert.ok(!r.error, r.text);
await s.close();
// … and a configuration pinned to another chain id: refused before anything is signed.
s = await server({ BLOCKULA_RPC: rpc, BLOCKULA_CHAIN_ID: "another-chain" });
const tools = await s.tools();
for (const name of ["stake", "unstake", "my_stake", "withdraw_stake", "list_proposals", "vote", "renew_name"]) assert.ok(tools.some((t) => t.name === name), `tool ${name}`);
for (const name of ["list_hunts", "lookup", "list_proposals"]) assert.match(tools.find((t) => t.name === name)?.description ?? "", /untrusted_\*/, `${name} says its chain text is untrusted`);
for (const name of ["list_hunts", "fund_hunt", "contribution_pool", "submit_contribution", "my_contributions"]) {
  assert.match(tools.find((t) => t.name === name)?.description ?? "", /Testnet FANG, Hunts and Soul have no monetary value/, `${name} carries the disclaimer`);
}
r = await s.call("status");
assert.ok(r.error && /Refusing .*chain "fake-chain", not "another-chain"/.test(r.text), r.text);
r = await s.call("send", { to: "boo1x", amount: "1" });
assert.ok(r.error && /Refusing/.test(r.text) && posted.length === 0, "nothing signed or sent");
await s.close();

s = await server({ BLOCKULA_RPC: rpc });
r = await s.call("status");
assert.ok(!r.error, r.text);
assert.equal(JSON.parse(r.text).pinned.genesis_hash, G1);
assert.equal(JSON.parse(r.text).network, "local dev node");
// The node switches chains mid-session: refused on the next call.
nodeStatus = { ...nodeStatus, genesis_hash: G2 };
r = await s.call("status");
assert.ok(r.error && /genesis_hash/.test(r.text), r.text);
await s.close();

// Remembered across restarts for remote networks (here: a custom https-less private node pinned explicitly).
s = await server({ BLOCKULA_RPC: rpc, BLOCKULA_NETWORK: "custom" });
r = await s.call("status");
assert.ok(!r.error, r.text);
await s.close();
assert.equal(JSON.parse(readFileSync(join(home, "known-chains.json"), "utf8"))[`custom ${rpc}`].genesisHash, G2);
nodeStatus = { ...nodeStatus, genesis_hash: G1 };
s = await server({ BLOCKULA_RPC: rpc, BLOCKULA_NETWORK: "custom" });
r = await s.call("status");
assert.ok(r.error && /starts from genesis .*not 2222/.test(r.text) && r.text.includes("known-chains.json"), r.text);
await s.close();

// A broken configuration fails every tool with the reason, instead of guessing a network.
s = await server({ BLOCKULA_NETWORK: "moon" });
r = await s.call("status");
assert.ok(r.error && /isn't configured correctly: unknown network "moon"/.test(r.text), r.text);
await s.close();

fake.close();
rmSync(dir, { recursive: true, force: true });
console.log("✓ the server refuses the wrong chain, notices a switch, and remembers chains across restarts");
