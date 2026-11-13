// The configuration, offline: defaults, flags over the environment over a file, and what is
// refused (no node, a node on another machine, sizes that don't fit together).
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DEFAULT_DIR, isLocalRpc, loadConfig, parseArgs } from "../src/config.js";

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
    },
  );

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
