// The command line, offline, as a child process: `init` makes the two private files once and
// prints the address; `run` and `status` refuse a key others can read, a missing node and a
// node on another machine, and never print a secret.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { chmodSync, mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

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
  // No command: the usage, and a non-zero exit for one it doesn't know.
  assert.match(run([]).out, /blackjack-dealer <init \| run \| status>/);
  assert.equal(run(["deal"]).code, 2);
} finally {
  rmSync(dir, { recursive: true, force: true });
}
console.log("✅ the command line tests passed");
