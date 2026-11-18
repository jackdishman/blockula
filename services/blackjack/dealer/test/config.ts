// The configuration, offline: defaults, flags over the environment over a file, and what is
// refused (no node, a node on another machine, sizes that don't fit together).
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DEFAULT_DIR, isLocalRpc, loadConfig, nodeUrl, parseArgs } from "../src/config.js";
import { readsText, trustNotes } from "../src/reader.js";

const dir = mkdtempSync(join(tmpdir(), "bj-dealer-config."));
const TABLE = "boo1qqqsyqcyq5rqwzqfpg9scrgwpugpzysnzs23v9ccrydpk8qarc0sxsvkvl";
const RPC = "http://127.0.0.1:38200";
const load = (argv: string[], env: Record<string, string> = {}, opts?: Parameters<typeof loadConfig>[2]) => loadConfig(parseArgs(argv), env, opts);

try {
  // ---- defaults
  let c = load(["run", "--rpc", RPC, "--table", TABLE]);
  assert.deepEqual(
    { ...c },
    {
      rpc: RPC,
      table: TABLE,
      dir: DEFAULT_DIR,
      keyFile: join(DEFAULT_DIR, "dealer-key.json"),
      secretFile: join(DEFAULT_DIR, "store-secret.json"),
      storePath: join(DEFAULT_DIR, "seeds.enc.json"),
      backupPath: join(DEFAULT_DIR, "seeds.enc.json.backup"),
      lowWater: 64,
      target: 192,
      batch: 64,
      pollMs: 1000,
      dryRun: false,
      allowRemote: false,
      keychain: false,
      keychainService: "blockula-blackjack-dealer",
      keychainAccount: TABLE,
      lowBalance: "5",
      keepSettled: 256,
      txValidBlocks: 120,
      fullFuel: false,
      chainId: undefined,
      genesisHash: undefined,
      // Proven reads are off unless witnesses are named.
      witnesses: [],
      witnessQuorum: undefined,
      proofMaxLagBlocks: 10,
    },
  );
  assert.equal(readsText(c), "the node's word (a node on this machine)");
  assert.deepEqual(trustNotes(c), [], "a node on this machine: nothing new is said");

  // ---- the environment, a file, and their order: flags, then the environment, then the file
  const file = join(dir, "dealer.json");
  writeFileSync(file, JSON.stringify({ rpc: "http://localhost:38201/", table: TABLE, dir: join(dir, "from-file"), lowWater: 10, target: 30, batch: 20, pollMs: 250, dryRun: true, backupPath: join(dir, "elsewhere", "b.json") }));
  c = load(["run", "--config", file]);
  assert.deepEqual([c.rpc, c.lowWater, c.target, c.batch, c.pollMs, c.dryRun], ["http://localhost:38201", 10, 30, 20, 250, true]);
  assert.equal(c.keyFile, join(dir, "from-file", "dealer-key.json"));
  assert.equal(c.backupPath, join(dir, "elsewhere", "b.json"));
  c = load(["run"], { BJ_DEALER_CONFIG: file, BJ_DEALER_LOW_WATER: "12", BJ_DEALER_DRY_RUN: "false", BJ_DEALER_STORE: join(dir, "s.json") });
  assert.deepEqual([c.lowWater, c.target, c.dryRun, c.storePath, c.backupPath], [12, 30, false, join(dir, "s.json"), join(dir, "elsewhere", "b.json")]);
  c = load(["run", "--low-water=14", "--dry-run", "--poll-ms", "500"], { BJ_DEALER_CONFIG: file, BJ_DEALER_LOW_WATER: "12", BJ_DEALER_DRY_RUN: "false" });
  assert.deepEqual([c.lowWater, c.dryRun, c.pollMs], [14, true, 500]);
  // BLOCKULA_RPC is the node when BJ_DEALER_RPC isn't set.
  assert.equal(load(["status", "--table", TABLE], { BLOCKULA_RPC: RPC }).rpc, RPC);
  assert.equal(load(["status", "--table", TABLE], { BLOCKULA_RPC: RPC, BJ_DEALER_RPC: "http://[::1]:38202" }).rpc, "http://[::1]:38202");
  assert.deepEqual(parseArgs(["status", "--json"]).json, true);

  // ---- no default node, and never another machine's by accident
  assert.throws(() => load(["run", "--table", TABLE]), /no node: set --rpc/);
  assert.throws(() => load(["run", "--rpc", "https://demo.blockula.app", "--table", TABLE]), /isn't this machine/);
  assert.throws(() => load(["run", "--rpc", "http://10.0.0.5:7070", "--table", TABLE]), /isn't this machine/);
  assert.throws(() => load(["run", "--rpc", "http://10.0.0.5:7070", "--table", TABLE, "--allow-remote"]), /needs the chain pinned/);
  c = load(["run", "--rpc", "http://10.0.0.5:7070", "--table", TABLE, "--allow-remote", "--chain-id", "my-chain", "--genesis-hash", "AB".repeat(32)]);
  assert.deepEqual([c.allowRemote, c.chainId, c.genesisHash], [true, "my-chain", "ab".repeat(32)]);
  assert.throws(() => load(["run", "--rpc", "ftp://127.0.0.1", "--table", TABLE]), /http/);
  for (const [rpc, local] of [["http://127.0.0.1:1", true], ["http://localhost", true], ["http://[::1]:9", true], ["http://127.0.0.1.evil.example", false], ["http://localhost.evil.example:7070", false], ["nonsense", false]] as const) assert.equal(isLocalRpc(rpc), local, rpc);

  // ---- witnesses: proven reads (docs/proposals/blackjack-card-privacy.md)
  const PIN = ["--chain-id", "my-chain", "--genesis-hash", "ab".repeat(32)];
  const base = ["run", "--rpc", RPC, "--table", TABLE];
  c = load([...base, "--witnesses", "http://127.0.0.1:38201/, http://localhost:38202"]);
  assert.deepEqual([c.witnesses, c.witnessQuorum, c.proofMaxLagBlocks], [["http://127.0.0.1:38201", "http://localhost:38202"], undefined, 10]);
  assert.match(readsText(c), /^proven: .* confirmed by 2 of 2 witnesses$/);
  assert.deepEqual(trustNotes(c), [{ level: "info", action: "proven_reads", fields: { witnesses: 2, quorum: 2, max_lag_blocks: 10 } }]);
  c = load([...base, ...PIN], { BJ_DEALER_WITNESSES: "https://a.example,https://B.example:443/,http://[::1]:9", BJ_DEALER_WITNESS_QUORUM: "2", BJ_DEALER_PROOF_MAX_LAG_BLOCKS: "25" });
  assert.deepEqual([c.witnesses, c.witnessQuorum, c.proofMaxLagBlocks], [["https://a.example", "https://b.example", "http://[::1]:9"], 2, 25]);
  assert.match(readsText(c), /confirmed by 2 of 3 witnesses$/);
  writeFileSync(file, JSON.stringify({ rpc: RPC, table: TABLE, witnesses: ["http://127.0.0.1:38201"], witnessQuorum: 1, proofMaxLagBlocks: 0 }));
  c = load(["run", "--config", file]);
  assert.deepEqual([c.witnesses, c.witnessQuorum, c.proofMaxLagBlocks], [["http://127.0.0.1:38201"], 1, 0]);
  assert.match(readsText(c), /confirmed by 1 of 1 witness$/);
  // An empty setting is no witnesses: an environment variable left blank changes nothing.
  assert.deepEqual(load(base, { BJ_DEALER_WITNESSES: "" }).witnesses, []);
  assert.deepEqual(load([...base, "--witnesses", " , "]).witnesses, []);
  // A witness is another node: not the dealer's own, not one listed twice (however it's spelled).
  assert.throws(() => load([...base, "--witnesses", "http://127.0.0.1:38200/"]), /the dealer's own node/);
  assert.throws(() => load(["run", "--rpc", "https://Node.example/", "--table", TABLE, "--allow-remote", ...PIN, "--witnesses", "https://node.example:443"]), /the dealer's own node/);
  assert.throws(() => load([...base, "--witnesses", "http://127.0.0.1:38201,http://127.0.0.1:38201/"]), /listed twice/);
  // On another machine: https, and the chain pinned.
  assert.throws(() => load([...base, ...PIN, "--witnesses", "http://10.0.0.6:7070"]), /must be https/);
  assert.throws(() => load([...base, "--witnesses", "https://a.example"]), /needs the chain pinned/);
  assert.throws(() => load([...base, "--chain-id", "my-chain", "--witnesses", "https://a.example"]), /needs the chain pinned/);
  // What isn't a node's URL.
  for (const bad of ["a.example", "ftp://a.example", "https://user:pw@a.example", "https://a.example/?x=1", "https://a.example/#x"]) assert.throws(() => load([...base, ...PIN, "--witnesses", bad]), /a witness is the http/, bad);
  writeFileSync(file, JSON.stringify({ rpc: RPC, table: TABLE, witnesses: [7] }));
  assert.throws(() => load(["run", "--config", file]), /witnesses is a list of node URLs/);
  // The quorum fits the witnesses.
  assert.throws(() => load([...base, "--witness-quorum", "1"]), /witnessQuorum needs witnesses/);
  assert.throws(() => load([...base, "--witnesses", "http://127.0.0.1:38201", "--witness-quorum", "2"]), /at most the number of witnesses \(1\)/);
  assert.throws(() => load([...base, "--witnesses", "http://127.0.0.1:38201", "--witness-quorum", "0"]), /witnessQuorum is a whole number from 1/);
  assert.throws(() => load([...base, "--witnesses", "http://127.0.0.1:38201", "--proof-max-lag-blocks", "-1"]), /proofMaxLagBlocks is a whole number/);
  assert.throws(() => load([...base, "--witnesses", Array.from({ length: 17 }, (_, i) => `http://127.0.0.1:${39000 + i}`).join(",")]), /at most 16 witnesses/);
  for (const [url, same] of [["HTTP://Node.Example:80/", "http://node.example"], ["https://node.example:443//", "https://node.example"], ["https://node.example:8443/base/", "https://node.example:8443/base"]] as const) assert.equal(nodeUrl(url), same);
  for (const bad of ["", "node.example", "ws://node.example", "https://node.example/?a=1"]) assert.equal(nodeUrl(bad), undefined, bad);

  // ---- a configuration that ran before still runs, and is told what it trusts
  const remote = ["run", "--table", TABLE, "--allow-remote", ...PIN];
  c = load([...remote, "--rpc", "https://node.example"]);
  assert.match(readsText(c), /^the node's word {3}⚠ another machine's node.*--witnesses/);
  assert.deepEqual(trustNotes(c).map((n) => [n.level, n.action]), [["warn", "unproven_remote_node"]]);
  c = load([...remote, "--rpc", "http://10.0.0.5:7070"]);
  assert.deepEqual(trustNotes(c).map((n) => [n.level, n.action]), [["warn", "unproven_remote_node"], ["warn", "insecure_rpc"]]);
  c = load([...remote, "--rpc", "http://10.0.0.5:7070", "--witnesses", "https://a.example"]);
  assert.deepEqual(trustNotes(c).map((n) => [n.level, n.action]), [["info", "proven_reads"], ["warn", "insecure_rpc"]]);
  c = load([...remote, "--rpc", "https://node.example", "--witnesses", "https://a.example"]);
  assert.deepEqual(trustNotes(c).map((n) => n.action), ["proven_reads"]);

  // ---- what doesn't fit is refused, in words
  assert.throws(() => load(["run", "--rpc", RPC]), /no table: set --table/);
  assert.throws(() => load(["run", "--rpc", RPC, "--table", "alice.boo"]), /no table/);
  assert.throws(() => load(["run", "--rpc", RPC, "--table", TABLE, "--batch", "65"]), /batch is a whole number from 1 to 64/);
  assert.throws(() => load(["run", "--rpc", RPC, "--table", TABLE, "--low-water", "50", "--target", "40"]), /target can't be under lowWater/);
  assert.throws(() => load(["run", "--rpc", RPC, "--table", TABLE, "--poll-ms", "fast"]), /pollMs is a whole number/);
  assert.throws(() => load(["run", "--rpc", RPC, "--table", TABLE, "--low-balance", "lots"]), /lowBalance is an amount of FANG/);
  assert.throws(() => load(["run", "--rpc", RPC, "--table", TABLE, "--store", join(dir, "x"), "--backup", join(dir, "x")]), /another file/);
  assert.throws(() => load(["run", "--rpc", RPC, "--table", TABLE, "--genesis-hash", "abc"]), /64 hex/);
  assert.throws(() => parseArgs(["run", "--seed", "abc"]), /unknown option --seed/, "there is no flag for a secret");
  assert.throws(() => parseArgs(["run", "--rpc"]), /needs a value/);
  assert.throws(() => parseArgs(["run", "status"]), /one command/);
  writeFileSync(file, JSON.stringify({ rpc: RPC, table: TABLE, seed: "x" }));
  assert.throws(() => load(["run", "--config", file]), /a setting this daemon doesn't know: seed/);
  writeFileSync(file, "{nope");
  assert.throws(() => load(["run", "--config", file]), /can't be read as JSON/);

  // ---- `init` needs neither a node nor a table
  c = load(["init", "--dir", dir], {}, { needTable: false, needRpc: false });
  assert.deepEqual([c.rpc, c.table, c.keyFile], ["", "", join(dir, "dealer-key.json")]);
  assert.throws(() => load(["init", "--dir", dir, "--rpc", "https://demo.blockula.app"], {}, { needTable: false, needRpc: false }), /isn't this machine/);
} finally {
  rmSync(dir, { recursive: true, force: true });
}
console.log("✅ the config tests passed");
