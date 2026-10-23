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
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Client as McpClient } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import { MAINNET_CHAIN_ID, RELEASE_SIGNATURE_FORMAT, releaseMessage } from "@blockula/sdk";
import { BLOCKULA_1, KNOWN_CLOCKS, KNOWN_GENESIS, KnownChains, LOCAL_RPC, TESTNET_RPC, canonicalUrl, completePin, defaultWitnesses, effectivePin, mismatch, networkArg, networkLabel, resolveNetwork, unpinnedBlockula1, unusableChain } from "../src/network.js";
import { RELEASE_KEY_ID, binPaths, checkBinaryVersion, fetchVerifiedRelease, latestTagFrom, pickNodeBinary, readReleasePin, releaseKeyIdFrom, type ReleaseDeps } from "../src/release.js";

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
assert.equal(networkArg(["--foo", "--network", "local"]), "local");
assert.equal(resolveNetwork({ BLOCKULA_RPC: "http://127.0.0.1:7391/" }).key, "local", "a localhost RPC is the local network");
assert.equal(resolveNetwork({ BLOCKULA_RPC: "http://127.0.0.1:7391/" }).rpc, "http://127.0.0.1:7391");
assert.equal(resolveNetwork({ BLOCKULA_RPC: TESTNET_RPC }).key, "testnet", "the testnet's own URL keeps its pins");
assert.equal(resolveNetwork({ BLOCKULA_RPC: "https://rpc.example" }).key, "custom");
assert.equal(resolveNetwork({ BLOCKULA_RPC: "http://10.0.0.5:7070" }).key, "custom", "plain http on a private network");
// There's no mainnet network: a node on blockula-1 is reached with BLOCKULA_CHAIN_ID, like any chain.
const onBlockula1 = resolveNetwork({ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_CHAIN_ID: "blockula-1" });
assert.deepEqual([onBlockula1.key, onBlockula1.pin.chainId, onBlockula1.pin.prefix], ["custom", "blockula-1", undefined]);
const pinned = resolveNetwork({ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_CHAIN_ID: "x", BLOCKULA_GENESIS_HASH: G1.toUpperCase() });
assert.deepEqual(pinned.pin, { chainId: "x", genesisHash: G1 });
const refusals: [Record<string, string>, string[], RegExp][] = [
  [{ BLOCKULA_RPC: "https://rpc.example" }, ["--network", "mainnet"], /unknown network "mainnet" \(testnet, local or custom\)/],
  [{ BLOCKULA_NETWORK: "Mainnet" }, [], /unknown network "mainnet"/],
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
  // Audit MCK-1: a node spelled two ways is one node, for the guards as for the defaults.
  [{ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_WITNESSES: "https://RPC.example:443/." }, [], /other nodes than BLOCKULA_RPC/],
  [{ BLOCKULA_RPC: "https://Rpc.Example:443/.", BLOCKULA_WITNESSES: "https://rpc.example" }, [], /other nodes than BLOCKULA_RPC/],
  [{ BLOCKULA_RPC: "https://Blockula-Testnet.fly.dev", BLOCKULA_WITNESSES: TESTNET_RPC }, [], /other nodes than BLOCKULA_RPC/],
  [{ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_WITNESSES: "https://w.example,https://W.example:443/" }, [], /twice/],
  // What the client appends /api/… to is a node's URL: a query or a fragment would be dropped or misread.
  [{ BLOCKULA_RPC: "https://rpc.example/?key=1" }, [], /without a query or a fragment/],
  [{ BLOCKULA_RPC: "https://rpc.example/#x" }, [], /without a query or a fragment/],
  [{ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_WITNESSES: "https://w.example/?k=1" }, [], /BLOCKULA_WITNESSES must be a node's URL, without a query or a fragment/],
  // Audit MCK-3: a pinned chain id is one this MCP can key its ledger and files by.
  [{ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_CHAIN_ID: "../x" }, [], /BLOCKULA_CHAIN_ID must be a chain id/],
  [{ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_CHAIN_ID: "a".repeat(65) }, [], /BLOCKULA_CHAIN_ID must be a chain id/],
];
// gap-7-1: witnesses names are proven against.
assert.deepEqual(resolveNetwork({ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_WITNESSES: " https://a.example/ , https://b.example ," }).witnesses, ["https://a.example", "https://b.example"]);
assert.deepEqual(resolveNetwork({}).witnesses, []);
// OPERATOR-6: your own node of the public testnet gets its public RPC as a witness unless
// BLOCKULA_WITNESSES says otherwise (`none` opts out); the public RPC itself gets none, and so do
// chains without built-in public RPCs.
const own = resolveNetwork({ BLOCKULA_RPC: "http://127.0.0.1:7171" }, ["--network", "testnet"]);
assert.deepEqual([own.key, own.rpc, own.witnesses, own.witnessesSet], ["testnet", "http://127.0.0.1:7171", [TESTNET_RPC], false]);
assert.deepEqual([resolveNetwork({ BLOCKULA_RPC: "http://127.0.0.1:7171", BLOCKULA_WITNESSES: "none" }, ["--network", "testnet"]).witnesses, resolveNetwork({ BLOCKULA_RPC: "http://127.0.0.1:7171", BLOCKULA_WITNESSES: "None" }, ["--network", "testnet"]).witnessesSet], [[], true]);
assert.deepEqual(resolveNetwork({ BLOCKULA_RPC: "http://127.0.0.1:7171", BLOCKULA_WITNESSES: "https://w.example" }, ["--network", "testnet"]).witnesses, ["https://w.example"], "listed witnesses replace the default");
assert.deepEqual(resolveNetwork({}, ["--network", "testnet"]).witnesses, [], "the public RPC isn't its own witness");
assert.deepEqual([defaultWitnesses("blockula-testnet-2", "http://127.0.0.1:7171"), defaultWitnesses("blockula-testnet-2", TESTNET_RPC), defaultWitnesses("blockula-dev", "http://127.0.0.1:7070"), defaultWitnesses("blockula-1", "http://127.0.0.1:7171"), defaultWitnesses(undefined, "x")], [[TESTNET_RPC], [], [], [], []]);
// Audit MCK-1: the public RPC under any spelling is the public RPC, and never its own witness (a
// proof checked against the state root the same node reports proves nothing, yet would be called
// "proven against 1 independent node").
for (const spelled of ["https://Blockula-Testnet.fly.dev", "https://BLOCKULA-TESTNET.FLY.DEV/", "https://blockula-testnet.fly.dev:443", "https://blockula-testnet.fly.dev/.", "https://blockula-testnet.fly.dev./", "https://blockula-testnet.fly.dev:443/./"]) {
  assert.deepEqual(defaultWitnesses("blockula-testnet-2", spelled), [], spelled);
  const n = resolveNetwork({ BLOCKULA_RPC: spelled });
  assert.deepEqual([n.key, n.rpc, n.witnesses, n.pin.chainId], ["testnet", TESTNET_RPC, [], "blockula-testnet-2"], `${spelled} is the public testnet`);
}
assert.deepEqual(defaultWitnesses("blockula-testnet-2", "http://127.0.0.1:7171/."), [TESTNET_RPC], "your own node, however spelled, still gets the public RPC");
assert.deepEqual(resolveNetwork({ BLOCKULA_RPC: "http://LOCALHOST:7171//" }, ["--network", "testnet"]).witnesses, [TESTNET_RPC]);
// A URL has one spelling: the node's own, as the URL parser normalizes it, without the trailing slash.
for (const [typed, canonical] of [
  ["https://rpc.example/", "https://rpc.example"],
  ["HTTPS://Rpc.Example:443", "https://rpc.example"],
  ["http://LocalHost:80/a/./b//", "http://localhost/a/b"],
  ["https://rpc.example./", "https://rpc.example"],
  ["https://rpc.example.:8443/x/", "https://rpc.example:8443/x"],
  ["http://[::1]:7070/", "http://[::1]:7070"],
  ["https://rpc.example/Node1", "https://rpc.example/Node1"], // a path is case-sensitive
  ["http://127.1:7070", "http://127.0.0.1:7070"],
  ["ftp://rpc.example/", "ftp://rpc.example/"], // not a node URL: as it is
  ["not a url", "not a url"],
]) assert.equal(canonicalUrl(typed), canonical, typed);
for (const u of [TESTNET_RPC, LOCAL_RPC]) assert.equal(canonicalUrl(u), u, `${u}: the built-in RPCs are in canonical spelling`);
assert.deepEqual(resolveNetwork({ BLOCKULA_RPC: "https://Rpc.Example:443/.", BLOCKULA_WITNESSES: "https://W.example/ , https://x.example:8443/" }).witnesses, ["https://w.example", "https://x.example:8443"]);
// A node on this machine is "your node on" the public testnet once its chain is known.
assert.equal(networkLabel(resolveNetwork({ BLOCKULA_RPC: "http://127.0.0.1:7171" }), "blockula-testnet-2"), "your node on the public testnet");
assert.equal(networkLabel(resolveNetwork({ BLOCKULA_RPC: "http://127.0.0.1:7171" }), "blockula-dev"), "local dev node");
assert.equal(networkLabel(resolveNetwork({}), "blockula-testnet-2"), "public testnet");
for (const [env, argv, re] of refusals) assert.throws(() => resolveNetwork(env, argv), re, JSON.stringify([env, argv]));
console.log("✓ the network comes from the configuration only (default: the public testnet); a node is one node however its URL is spelled, so the public RPC is never its own witness");

// ---- 2. pins ----

const status = (chain_id: string, genesis_hash: string, address_prefix = "boo") => ({ chain_id, genesis_hash, address_prefix });
assert.equal(mismatch({}, status("a", G1)), null);
assert.match(mismatch({ chainId: "b" }, status("a", G1)) ?? "", /chain "a", not "b"/);
assert.match(mismatch({ genesisHash: G2 }, status("a", G1)) ?? "", /genesis/);
assert.match(mismatch({ prefix: "tboo" }, status("a", G1)) ?? "", /"boo1…" addresses/);
assert.match(mismatch({}, { genesis_hash: G1 }) ?? "", /chain id/);
assert.equal(mismatch({ genesisHash: G1 }, status("a", G1.toUpperCase())), null, "hex case doesn't matter");
// A known chain id must come with its known genesis, whatever the configuration says. (A stand-in
// for blockula-1's: the one the launch release pins, if any, is put back after.)
const blockula1Genesis: string | undefined = KNOWN_GENESIS["blockula-1"];
KNOWN_GENESIS["blockula-1"] = G1;
assert.match(mismatch({}, status("blockula-1", G2)) ?? "", /genesis/);
assert.equal(mismatch({}, status("blockula-1", G1)), null);
assert.equal(resolveNetwork({ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_CHAIN_ID: "blockula-1" }).pin.genesisHash, G1);
assert.throws(() => resolveNetwork({ BLOCKULA_RPC: "https://rpc.example", BLOCKULA_CHAIN_ID: "blockula-1", BLOCKULA_GENESIS_HASH: G2 }), /contradicts blockula-1's known genesis/);
if (blockula1Genesis === undefined) delete KNOWN_GENESIS["blockula-1"];
else KNOWN_GENESIS["blockula-1"] = blockula1Genesis;
// A node that says it's on blockula-1 is used only where the user pinned that chain: it's never
// trusted on first use (as the CLI doesn't), so a configuration that never named it can't land there.
assert.equal(BLOCKULA_1, MAINNET_CHAIN_ID, "the same chain id as the SDK's");
assert.match(unpinnedBlockula1({}, status("blockula-1", G1)) ?? "", /says it's on blockula-1, and this MCP isn't pinned to that chain.*BLOCKULA_CHAIN_ID=blockula-1/);
assert.match(unpinnedBlockula1({ genesisHash: G1, prefix: "boo" }, status("blockula-1", G1)) ?? "", /isn't pinned to that chain/, "a genesis hash or prefix isn't the chain");
assert.equal(unpinnedBlockula1({ chainId: "blockula-1" }, status("blockula-1", G1)), null);
assert.equal(unpinnedBlockula1({}, status("a", G1)), null);
assert.match(unpinnedBlockula1({ chainId: "a" }, status("blockula-1", G1)) ?? "", /isn't pinned to that chain/, "a pin on another chain isn't a pin on blockula-1 (mismatch says so first)");
assert.match(mismatch({ chainId: "a" }, status("blockula-1", G1)) ?? "", /chain "blockula-1", not "a"/);
assert.throws(() => resolveNetwork({ BLOCKULA_RPC: "https://rpc.example" }, ["--network", "mainnet"]), /there's no mainnet network, and a node that says it's on blockula-1 is used only with BLOCKULA_CHAIN_ID=blockula-1/);
// Every known genesis comes with its clock (scripts/pin-network.sh pins them together).
for (const [id, hash] of Object.entries(KNOWN_GENESIS)) {
  assert.match(hash, /^[0-9a-f]{64}$/, id);
  assert.ok(KNOWN_CLOCKS[id] && Number.isSafeInteger(KNOWN_CLOCKS[id].createdAtMs) && KNOWN_CLOCKS[id].blockTimeMs >= 1, `${id}: KNOWN_CLOCKS has its genesis clock`);
}
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
// Its genesis is built in once a release pins it (scripts/pin-network.sh); until then it's pinned
// on first use. Either way these hold.
const testnet = resolveNetwork({});
const builtIn: string | undefined = KNOWN_GENESIS["blockula-testnet-2"];
assert.deepEqual([testnet.pin.chainId, testnet.pin.prefix, testnet.pin.genesisHash], ["blockula-testnet-2", "tboo", builtIn], "its genesis is built in only once a release pins it");
assert.deepEqual(effectivePin(testnet, { chainId: "blockula-testnet", genesisHash: G2, prefix: "boo" }), { chainId: "blockula-testnet-2", genesisHash: builtIn, prefix: "tboo" });
assert.deepEqual(effectivePin(testnet, { chainId: "blockula-testnet-2", genesisHash: G1, prefix: "tboo" }), { chainId: "blockula-testnet-2", genesisHash: builtIn ?? G1, prefix: "tboo" });
if (builtIn) {
  assert.match(builtIn, /^[0-9a-f]{64}$/);
  assert.match(mismatch(testnet.pin, status("blockula-testnet-2", G1 === builtIn ? G2 : G1, "tboo")) ?? "", /genesis/, "a node on another instance of the testnet is refused");
  assert.equal(mismatch(testnet.pin, status("blockula-testnet-2", builtIn, "tboo")), null);
}
// Audit MCK-3: a node's chain id is a ledger key and part of file names, so a node reporting one
// this MCP can't key by (a path, a space, past 64 characters) isn't pinned at all.
assert.equal(unusableChain(status("fake-chain", G1)), null);
assert.equal(unusableChain(status("blockula_dev.1", G1.toUpperCase())), null);
assert.equal(unusableChain({ chain_id: "a".repeat(64) }), null, "a node that reports no genesis hash is pinned by its chain id alone");
for (const bad of ["../../escaped/x", "a/b", "a\\b", "a b", "", "a".repeat(65), "x\n", "é"]) assert.match(unusableChain(status(bad, G1)) ?? "", /the node reports the chain id .*1 to 64 letters/, JSON.stringify(bad));
assert.match(unusableChain(status("a".repeat(100), G1)) ?? "", /"a{80}…"/, "a long id is cut short in the message");
for (const bad of ["abc", "zz".repeat(32), `${G1}0`, 5, ""]) assert.match(unusableChain({ chain_id: "a", genesis_hash: bad }) ?? "", /genesis hash that isn't 64 hex/, JSON.stringify(bad));
assert.equal(unusableChain({ chain_id: 5 }), null, "(not a string: `mismatch` says it reports no chain id)");
// A release before canonicalUrl remembered a node's URL as it was typed. That entry is the same
// node's, so its pin holds (the node isn't pinned afresh on trust) and moves to the canonical spelling.
const legacyFile = join(dir, "legacy-known-chains.json");
const clockEntry = { created_at_ms: 1, block_time_ms: 1000 };
writeFileSync(legacyFile, JSON.stringify({ "custom https://Rpc.Example:443/.": { chainId: "a", genesisHash: G1 }, "custom https://other.example": { chainId: "b", genesisHash: G2 }, "testnet http://127.0.0.1:7171/.": { chainId: "t", genesisHash: G2 }, [`clock a ${G1}`]: clockEntry }));
const legacy = new KnownChains(legacyFile);
const rpcExample = resolveNetwork({ BLOCKULA_RPC: "https://rpc.example" });
assert.deepEqual(legacy.get(rpcExample), { chainId: "a", genesisHash: G1 });
assert.deepEqual(JSON.parse(readFileSync(legacyFile, "utf8")), { "custom https://rpc.example": { chainId: "a", genesisHash: G1 }, "custom https://other.example": { chainId: "b", genesisHash: G2 }, "testnet http://127.0.0.1:7171/.": { chainId: "t", genesisHash: G2 }, [`clock a ${G1}`]: clockEntry }, "moved: the entry an error names is the only one");
assert.deepEqual(new KnownChains(legacyFile).get(rpcExample), { chainId: "a", genesisHash: G1 });
assert.equal(legacy.get(resolveNetwork({ BLOCKULA_RPC: "http://127.0.0.1:7171" }, ["--network", "custom"])), undefined, "another network's entry for the same URL isn't this one's");
assert.deepEqual(legacy.get(resolveNetwork({ BLOCKULA_RPC: "http://127.0.0.1:7171" }, ["--network", "testnet"])), { chainId: "t", genesisHash: G2 });
assert.equal(legacy.get(resolveNetwork({ BLOCKULA_RPC: "https://elsewhere.example" })), undefined);
const local = resolveNetwork({}, ["--network", "local"]);
known.save(local, { chainId: "blockula-dev", genesisHash: G1 });
assert.equal(known.get(local), undefined, "dev chains are re-pinned each session (a reset makes a new genesis)");
console.log("✓ pins: chain id, genesis, prefix, known genesis, trust on first use; a chain id that can't key files and ledgers isn't pinned; a pin remembered under another spelling of a URL moves to the canonical one");

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
// Audit review-r3-integrate-compose-1: the operator's pin (`blockula release install` writes
// $BLOCKULA_HOME/release-key-id) binds install_node too, with release_cli.rs read_pin's rules.
const pinDir = mkdtempSync(join(tmpdir(), "blockula-mcp-pin-"));
const pinFile = join(pinDir, "release-key-id");
assert.equal(readReleasePin(pinFile), null, "no pin");
writeFileSync(pinFile, ` ${keyId.toUpperCase()}\n`);
chmodSync(pinFile, 0o644);
assert.equal(readReleasePin(pinFile), keyId);
chmodSync(pinFile, 0o664);
assert.throws(() => readReleasePin(pinFile), /writable by others/);
chmodSync(pinFile, 0o644);
writeFileSync(pinFile, "not a key id\n");
assert.throws(() => readReleasePin(pinFile), /isn't a release key id pin/);
// Every key id that's set must agree: the pin, BLOCKULA_RELEASE_KEY_ID and the built-in one.
const otherId = "cd".repeat(32);
assert.equal(releaseKeyIdFrom(undefined, "", keyId, pinFile), keyId);
assert.equal(releaseKeyIdFrom(keyId, keyId, keyId, pinFile), keyId);
assert.throws(() => releaseKeyIdFrom(otherId, "", keyId, pinFile), /pinned in .* is [0-9a-f]{64}, not BLOCKULA_RELEASE_KEY_ID/);
assert.throws(() => releaseKeyIdFrom(undefined, otherId, keyId, pinFile), /built with release key [0-9a-f]{64}, but .* pins [0-9a-f]{64}/);
// The finding's case: nothing built into this MCP, the operator pinned K, and GitHub serves a
// release with provenance and no signature. Before, provenance alone installed it; now the pin
// requires the signature.
await assert.rejects(fetchVerifiedRelease(base, asset, opts(releaseKeyIdFrom(undefined, "", keyId, pinFile), false), deps({ ...files, "checksums.txt.sig": null }, "ok")), /no checksums.txt.sig/);
await assert.rejects(fetchVerifiedRelease(base, asset, opts(releaseKeyIdFrom(undefined, "", keyId, pinFile), false), deps({ ...files, "checksums.txt.sig": sig(sums, other) }, "ok")), /signature check FAILED/);
rmSync(pinDir, { recursive: true, force: true });
// install_node's binary has a path of its own, never the one `blockula release install` writes.
assert.notEqual(binPaths("/h").mcp, binPaths("/h").cli);
assert.equal(binPaths("/h").cli, join("/h", "bin", "blockula"));
// review-r4-supply-3: install_node's binary doesn't shadow a newer one `release install` put in
// ~/.blockula/bin. The one of this MCP's version runs, `release install`'s first; else the first
// that runs, flagged as another version.
const cli = { path: "/h/bin/blockula", version: "blockula 0.6.0" };
const mcpBin = { path: "/h/mcp-bin/blockula", version: "blockula 0.5.0" };
assert.deepEqual(pickNodeBinary([cli, mcpBin], "0.6.0"), { ...cli, matches: true });
assert.deepEqual(pickNodeBinary([cli, mcpBin], "0.5.0"), { ...mcpBin, matches: true });
assert.deepEqual(pickNodeBinary([cli, { ...mcpBin, version: "blockula 0.6.0" }], "0.6.0"), { ...cli, matches: true }, "both match: the pinned-key-checked one");
assert.deepEqual(pickNodeBinary([cli, mcpBin], "0.7.0"), { ...cli, matches: false });
assert.deepEqual(pickNodeBinary([{ path: cli.path }, mcpBin], "0.7.0"), { ...mcpBin, matches: false }, "one that doesn't run comes last");
assert.deepEqual(pickNodeBinary([], "0.6.0"), { path: "blockula", matches: false });
// The built-in key id is the same in all three copies (release_cli.rs checks it too).
const releaseKeyIn = (file: string, re: RegExp) => re.exec(readFileSync(new URL(file, import.meta.url), "utf8"))?.[1];
assert.equal(releaseKeyIn("../../node/src/release_cli.rs", /^pub const BUILTIN_KEY_ID: &str = "([0-9a-f]*)";$/m), RELEASE_KEY_ID, "release_cli.rs BUILTIN_KEY_ID");
assert.equal(releaseKeyIn("../../scripts/install.sh", /^BUILTIN_KEY_ID="([0-9a-f]*)"$/m), RELEASE_KEY_ID, "install.sh BUILTIN_KEY_ID");
console.log(`✓ install_node: version binding, checksum, provenance and signature checks (${releaseRefusals.length} bad releases refused), the pinned key, its own install path`);

// ---- 4. the server, over stdio, against a fake node ----

let nodeStatus = { chain_id: "fake-chain", genesis_hash: G1, address_prefix: "boo", height: 10, max_tx_validity_blocks: 3600 };
const posted: string[] = [];
const fake: Server = createServer((req, res) => {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    const reply = (code: number, v: unknown) => res.writeHead(code, { "content-type": "application/json" }).end(JSON.stringify(v));
    if (req.url === "/api/status") return reply(200, nodeStatus);
    if (req.url?.startsWith("/api/hunts")) return reply(200, { hunts: [] });
    if (req.url === "/api/contributions") return reply(200, { pool: { balance_display: "0 FANG" }, repos: [], claims: [], rules: {} });
    if (req.method === "POST") posted.push(body);
    if (req.url?.startsWith("/api/account/")) return reply(404, { error: "no account" });
    return reply(404, { error: `fake node: no route for ${req.url}` });
  });
});
await new Promise<void>((r) => fake.listen(0, "127.0.0.1", r));
const rpc = `http://127.0.0.1:${(fake.address() as AddressInfo).port}`;
const home = join(dir, "home");

async function server(env: Record<string, string>, argv: string[] = []) {
  const transport = new StdioClientTransport({ command: process.execPath, args: [new URL("../dist/index.js", import.meta.url).pathname, ...argv], env: { PATH: process.env.PATH ?? "", BLOCKULA_HOME: home, ...env }, stderr: "ignore" });
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
  assert.match(tools.find((t) => t.name === name)?.description ?? "", / FANG, Hunts and Soul have no value\.$/, `${name} carries the disclaimer`);
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

// A pin remembered under another spelling of the node's URL (before canonicalUrl) still holds after
// the upgrade, and the entry the error names is the one in the file (audit MCK-1).
const legacyHome = join(dir, "legacy-home");
mkdirSync(legacyHome, { recursive: true });
writeFileSync(join(legacyHome, "known-chains.json"), JSON.stringify({ [`custom ${rpc}/.`]: { chainId: "fake-chain", genesisHash: G2 } }));
s = await server({ BLOCKULA_RPC: rpc, BLOCKULA_NETWORK: "custom", BLOCKULA_HOME: legacyHome });
r = await s.call("status");
assert.ok(r.error && /starts from genesis .*not 2222/.test(r.text) && r.text.includes(`("custom ${rpc}")`), r.text);
assert.deepEqual(Object.keys(JSON.parse(readFileSync(join(legacyHome, "known-chains.json"), "utf8"))), [`custom ${rpc}`]);
await s.close();

// A node whose chain can't key the ledger, the locks or a file name isn't pinned: every tool refuses
// it for that reason (not for a missing genesis hash), and nothing is written outside BLOCKULA_HOME
// (audit MCK-3: `use_account` used to say the node reported no genesis hash).
mkdirSync(join(dir, "escaped"));
const outside = readdirSync(dir).sort();
for (const [chain_id, genesis_hash, why] of [
  ["x/../../escaped/y", G1, /the node reports the chain id "x\/\.\.\/\.\.\/escaped\/y", and a chain id here is 1 to 64/],
  ["a".repeat(65), G1, /the node reports the chain id "a{65}", and a chain id here/],
  ["fake-chain", "zz".repeat(32), /the node reports a genesis hash that isn't 64 hex characters/],
] as const) {
  nodeStatus = { ...nodeStatus, chain_id, genesis_hash };
  s = await server({ BLOCKULA_RPC: rpc });
  for (const [tool, args] of [["status", {}], ["create_wallet", {}], ["use_account", { account: "self" }]] as const) {
    r = await s.call(tool, args);
    assert.ok(r.error && /^Error: Refusing .*Nothing was signed or sent\.$/.test(r.text) && why.test(r.text), `${tool}: ${r.text}`);
    assert.ok(!/genesis hash, so the account can't be kept|relaunched|start_dev_node/.test(r.text), `${tool}: the reason is the chain's, and forgetting a remembered chain wouldn't help: ${r.text}`);
  }
  await s.close();
}
assert.deepEqual(readdirSync(dir).sort(), outside, "nothing was written outside BLOCKULA_HOME");
assert.deepEqual(readdirSync(join(dir, "escaped")), [], "where a chain id like that would have put a grant file");
assert.ok(!readdirSync(home).some((f) => f.startsWith("grant-")), "no grant file");
// A node that reports no genesis hash can be used, but can't keep an account: that's what use_account says.
const { genesis_hash: _none, ...noGenesis } = { ...nodeStatus, chain_id: "fake-chain", genesis_hash: G1 };
nodeStatus = noGenesis as typeof nodeStatus;
s = await server({ BLOCKULA_RPC: rpc });
r = await s.call("use_account", { account: "self" });
assert.ok(r.error && /doesn't report its chain's genesis hash, so the account can't be kept for this chain/.test(r.text), r.text);
await s.close();
nodeStatus = { ...nodeStatus, chain_id: "fake-chain", genesis_hash: G1 };

// Hunts and the contribution pool say FANG is worth nothing, in a tool's description and in a
// result, on every chain: a pinned test chain, a node whose chain isn't known until it answers, and
// a node on blockula-1 alike.
const NO_VALUE = /FANG, Hunts and Soul have no value\.$/;
const VALUE_TOOLS = ["list_hunts", "fund_hunt", "contribution_pool", "submit_contribution", "my_contributions"];
const described = async (srv: Awaited<ReturnType<typeof server>>) => {
  const all = await srv.tools();
  return VALUE_TOOLS.map((n) => all.find((t) => t.name === n)?.description ?? "");
};
const sayNoValue = async (srv: Awaited<ReturnType<typeof server>>, what: string) => {
  for (const d of await described(srv)) assert.ok(NO_VALUE.test(d) && !/mainnet|real money|testnet|test chains/i.test(d), `${what}: "${d}"`);
  for (const tool of ["list_hunts", "contribution_pool"]) {
    const res = await srv.call(tool);
    assert.ok(!res.error && /FANG, Hunts and Soul have no value\.$/.test(JSON.parse(res.text).note), `${what} ${tool}: ${res.text}`);
  }
};
nodeStatus = { ...nodeStatus, chain_id: "fake-chain" };
s = await server({ BLOCKULA_RPC: rpc });
await sayNoValue(s, "a node whose chain isn't pinned");
await s.close();
s = await server({ BLOCKULA_RPC: rpc, BLOCKULA_CHAIN_ID: "fake-chain" });
await sayNoValue(s, "a node pinned to a test chain");
await s.close();
// (Reporting blockula-1's genesis once a release pins it: any other is refused.)
nodeStatus = { ...nodeStatus, chain_id: "blockula-1", genesis_hash: KNOWN_GENESIS["blockula-1"] ?? G1 };
s = await server({ BLOCKULA_RPC: rpc, BLOCKULA_CHAIN_ID: "blockula-1" });
await sayNoValue(s, "a node pinned to blockula-1");
await s.close();
nodeStatus = { ...nodeStatus, chain_id: "fake-chain", genesis_hash: G1 };
console.log("✓ FANG, Hunts and Soul are called worthless on every chain, in descriptions and results");

// A node that says it's on blockula-1 is refused unless the user pinned that chain: BLOCKULA_RPC
// alone (a "local" or "custom" network) at such a node gets nothing signed, sent or remembered, from
// every tool. With BLOCKULA_CHAIN_ID=blockula-1 it's used, and remembered as pinned.
nodeStatus = { ...nodeStatus, chain_id: "blockula-1", genesis_hash: KNOWN_GENESIS["blockula-1"] ?? G1 };
const b1Home = join(dir, "blockula-1-home");
for (const env of [{ BLOCKULA_RPC: rpc }, { BLOCKULA_RPC: rpc, BLOCKULA_NETWORK: "custom" }]) {
  s = await server({ ...env, BLOCKULA_HOME: b1Home });
  for (const [tool, args] of [["status", {}], ["create_wallet", {}], ["list_hunts", {}], ["send", { to: "boo1x", amount: "1" }], ["use_account", { account: "self" }]] as const) {
    r = await s.call(tool, args);
    assert.ok(r.error && /^Error: Refusing .*the node says it's on blockula-1, and this MCP isn't pinned to that chain.*BLOCKULA_CHAIN_ID=blockula-1.*Nothing was signed or sent\.$/.test(r.text), `${JSON.stringify(env)} ${tool}: ${r.text}`);
    assert.ok(!/relaunched|known-chains/.test(r.text), `forgetting a remembered chain wouldn't help: ${r.text}`);
  }
  await s.close();
}
assert.equal(posted.length, 0, "nothing was sent");
assert.ok(!readdirSync(join(dir)).includes("blockula-1-home") || !readdirSync(b1Home).some((f) => f === "known-chains.json" || f === "agent-wallet.json"), "the node wasn't remembered, and no wallet was made for it");
s = await server({ BLOCKULA_RPC: rpc, BLOCKULA_NETWORK: "custom", BLOCKULA_CHAIN_ID: "blockula-1", BLOCKULA_HOME: b1Home });
r = await s.call("status");
assert.ok(!r.error && JSON.parse(r.text).pinned.chain_id === "blockula-1", r.text);
await s.close();
s = await server({ BLOCKULA_RPC: rpc, BLOCKULA_NETWORK: "custom", BLOCKULA_HOME: b1Home });
r = await s.call("status");
assert.ok(!r.error, `a pin the user's configuration named, remembered, still counts: ${r.text}`);
await s.close();
// Another chain id pinned: that's the refusal the pin always gave.
s = await server({ BLOCKULA_RPC: rpc, BLOCKULA_CHAIN_ID: "fake-chain", BLOCKULA_HOME: b1Home });
r = await s.call("status");
assert.ok(r.error && /chain "blockula-1", not "fake-chain"/.test(r.text), r.text);
await s.close();
nodeStatus = { ...nodeStatus, chain_id: "fake-chain", genesis_hash: G1 };
console.log("✓ a node that says it's on blockula-1 is refused (nothing signed, sent or remembered) unless BLOCKULA_CHAIN_ID pins that chain");

// A broken configuration fails every tool with the reason, instead of guessing a network.
s = await server({ BLOCKULA_NETWORK: "moon" });
r = await s.call("status");
assert.ok(r.error && /isn't configured correctly: unknown network "moon"/.test(r.text), r.text);
await s.close();

fake.close();
rmSync(dir, { recursive: true, force: true });
console.log("✓ the server refuses the wrong chain, notices a switch, remembers chains across restarts (and across a URL's spellings), and refuses a node whose chain id or genesis hash can't be pinned");
