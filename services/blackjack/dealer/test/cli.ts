// The command line, offline, as a child process: `init` makes the two private files once and
// prints the address; `run` and `status` refuse a key others can read, a missing node and a
// node on another machine, and never print a secret.
import assert from "node:assert/strict";
import { execFile, execFileSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { chmodSync, mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { readStoreKey } from "../src/keys.js";
import { SeedStore } from "../src/store.js";

const CLI = fileURLToPath(new URL("../src/cli.ts", import.meta.url));
const dir = mkdtempSync(join(tmpdir(), "bj-dealer-cli."));
const TABLE = "boo1qqqsyqcyq5rqwzqfpg9scrgwpugpzysnzs23v9ccrydpk8qarc0sxsvkvl";

/** Runs the command line; returns its exit code and everything it printed. */
function run(args: string[], env: Record<string, string> = {}): { code: number; out: string } {
  try {
    const out = execFileSync(process.execPath, ["--import", "tsx", CLI, ...args], { env: { PATH: process.env.PATH, HOME: dir, ...env }, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
    return { code: 0, out };
  } catch (e) {
    const err = e as { status: number | null; stdout: string; stderr: string };
    return { code: err.status ?? -1, out: err.stdout + err.stderr };
  }
}

/** The same, without blocking this process: for a command that talks to a server this test runs. */
function runAsync(args: string[], env: Record<string, string> = {}): Promise<{ code: number; out: string }> {
  return new Promise((resolve) => {
    execFile(process.execPath, ["--import", "tsx", CLI, ...args], { env: { PATH: process.env.PATH, HOME: dir, ...env }, encoding: "utf8", timeout: 60_000 }, (err, stdout, stderr) => {
      resolve({ code: err ? (typeof err.code === "number" ? err.code : -1) : 0, out: stdout + stderr });
    });
  });
}

try {
  const home = join(dir, "d");
  // ---- init: two private files, an address, nothing secret printed
  let r = run(["init", "--dir", home]);
  assert.equal(r.code, 0, r.out);
  const address = /^dealer address: (boo1[0-9a-z]+)$/m.exec(r.out)?.[1];
  assert.ok(address, "init prints the dealer's address");
  const keyFile = join(home, "dealer-key.json");
  const secretFile = join(home, "store-secret.json");
  for (const f of [keyFile, secretFile]) assert.equal(statSync(f).mode & 0o777, 0o600, `${f} is private`);
  assert.equal(statSync(home).mode & 0o777, 0o700);
  const seed = (JSON.parse(readFileSync(keyFile, "utf8")) as { seed: string }).seed;
  const storeKey = (JSON.parse(readFileSync(secretFile, "utf8")) as { storeKey: string }).storeKey;
  assert.ok(!r.out.includes(seed) && !r.out.includes(storeKey), "init prints no secret");
  // Again: the same files, the same address.
  r = run(["init", "--dir", home]);
  assert.match(r.out, /kept the dealer's key file/);
  assert.ok(r.out.includes(address));
  assert.equal((JSON.parse(readFileSync(keyFile, "utf8")) as { seed: string }).seed, seed);
  // The default directory is under the home directory (here a temporary one).
  r = run(["init"]);
  assert.equal(r.code, 0, r.out);
  assert.ok(r.out.includes(join(dir, ".blockula", "blackjack-dealer", "dealer-key.json")));

  // ---- run and status: what they refuse before touching anything
  const base = ["--dir", home, "--table", TABLE];
  r = run(["run", ...base]);
  assert.equal(r.code, 1);
  assert.match(r.out, /no node: set --rpc/);
  r = run(["run", ...base, "--rpc", "https://demo.blockula.app"]);
  assert.equal(r.code, 1);
  assert.match(r.out, /isn't this machine/);
  r = run(["run", "--dir", home, "--rpc", "http://127.0.0.1:38399"]);
  assert.match(r.out, /no table/);
  for (const [file, what] of [[keyFile, "the dealer's key file"], [secretFile, "the store's secret file"]] as const) {
    chmodSync(file, 0o644);
    for (const command of ["run", "status"]) {
      r = run([command, ...base, "--rpc", "http://127.0.0.1:38399"]);
      assert.equal(r.code, 1, `${command} with ${what} readable by others`);
      assert.ok(r.out.includes(`${what} (${file}) can be read by other users (mode 644)`), r.out);
      assert.ok(!r.out.includes(seed) && !r.out.includes(storeKey));
    }
    chmodSync(file, 0o600);
  }
  // A settings file and the environment work as the flags do; a secret has no setting.
  r = run(["run", "--seed", seed, ...base]);
  assert.equal(r.code, 1);
  assert.match(r.out, /unknown option --seed/);
  assert.ok(!r.out.includes(seed));
  r = run(["status"], { BJ_DEALER_DIR: home, BJ_DEALER_TABLE: TABLE, BJ_DEALER_RPC: "http://10.1.2.3:7070", BJ_DEALER_ALLOW_REMOTE: "true" });
  assert.match(r.out, /needs the chain pinned/);
  // Witnesses are checked before anything is touched, as flags or from the environment.
  r = run(["run", ...base, "--rpc", "http://127.0.0.1:38399", "--witnesses", "http://127.0.0.1:38399/"]);
  assert.equal(r.code, 1);
  assert.match(r.out, /the dealer's own node/);
  r = run(["status", ...base, "--rpc", "http://127.0.0.1:38399"], { BJ_DEALER_WITNESSES: "http://10.1.2.3:7070" });
  assert.equal(r.code, 1);
  assert.match(r.out, /must be https/);
  assert.match(run([]).out, /--witnesses URLS/);
  // No command: the usage, and a non-zero exit for one it doesn't know.
  assert.match(run([]).out, /blackjack-dealer <init \| run \| status>/);
  assert.equal(run(["deal"]).code, 2);

  // ---- one chain a store: `status` and `run` refuse a store that another chain's dealer wrote,
  // even though the table's address is the same (it comes from the creator and a nonce, never
  // from the chain). The node here is a stand-in that only says which chain it is on.
  {
    const A = "a".repeat(64);
    const B = "b".repeat(64);
    let genesis = A;
    let asked = 0;
    const server = createServer((req, res) => {
      asked++;
      res.setHeader("content-type", "application/json");
      if (req.url === "/api/status") return void res.end(JSON.stringify({ chain_id: "blockula-test", genesis_hash: genesis, address_prefix: "boo", height: 10 }));
      res.statusCode = 404;
      res.end(JSON.stringify({ error: "not found" }));
    });
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
    const rpc = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
    try {
      // The store as the dealer left it on chain A, with one secret.
      const top = randomBytes(32).toString("hex");
      const at = { path: join(home, "seeds.enc.json"), backupPath: join(home, "seeds.enc.json.backup"), key: readStoreKey(secretFile), table: TABLE };
      const s = SeedStore.open(at);
      s.bindChain(A);
      s.put(new Map([[0n, top]]));
      s.close();
      const stored = readFileSync(at.path);

      // The node is on chain B now (the chain was made again): both commands stop at once.
      genesis = B;
      for (const command of ["status", "run"]) {
        asked = 0;
        const out = await runAsync([command, ...base, "--rpc", rpc]);
        assert.equal(out.code, 1, `${command}: ${out.out}`);
        assert.match(out.out, /is for another chain \(genesis aaaaaaaaaaaa…\), not the one this node is on \(genesis bbbbbbbbbbbb…\)/, command);
        assert.match(out.out, /needs a new, empty store/);
        assert.ok(!out.out.includes(top) && !out.out.includes(seed) && !out.out.includes(storeKey), "no secret is printed");
        assert.ok(asked >= 1, "the node was asked which chain it is on");
        assert.ok(readFileSync(at.path).equals(stored), `${command} leaves the store as it was`);
      }
      // A node that says nothing about its chain is refused the same way.
      genesis = "";
      const none = await runAsync(["status", ...base, "--rpc", rpc]);
      assert.equal(none.code, 1);
      assert.match(none.out, /reports no chain id and genesis hash/);
      // Back on chain A the store is accepted: `status` gets past it, to the table, which this
      // stand-in node doesn't have (and whose address here is no real one).
      genesis = A;
      const ok = await runAsync(["status", ...base, "--rpc", rpc]);
      assert.equal(ok.code, 1);
      assert.ok(!/another chain|genesis/.test(ok.out), ok.out);
    } finally {
      await new Promise<void>((r) => server.close(() => r()));
    }
  }
} finally {
  rmSync(dir, { recursive: true, force: true });
}
console.log("✅ the command line tests passed");
