// The seed store and the private files, offline: round trips, tampering, a wrong key, file
// modes, a crash at each step of a write, the backup, pruning, the lock.
import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { chmodSync, copyFileSync, existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { checkPrivate, createDealerKey, createStoreKey, keychainStoreKey, readDealerSeed, readStoreKey } from "../src/keys.js";
import { SeedStore, StoreError } from "../src/store.js";

const dir = mkdtempSync(join(tmpdir(), "bj-dealer-store."));
const TABLE = "boo1table";
const key = randomBytes(32);
const secret = () => randomBytes(32).toString("hex");
const mode = (p: string) => statSync(p).mode & 0o777;
let n = 0;
/** A fresh store location. */
const place = () => {
  const d = join(dir, `s${n++}`);
  return { path: join(d, "seeds.enc.json"), backupPath: join(d, "backup", "seeds.enc.json"), key, table: TABLE };
};
const refuses = (run: () => unknown, pattern: RegExp, what: string) => assert.throws(run, (e: unknown) => e instanceof StoreError && pattern.test(e.message), what);

try {
  // ---- a round trip: what was put is there after a reopen, and nowhere in the file in the clear
  {
    const at = place();
    const s = SeedStore.open(at);
    assert.equal(s.size, 0);
    assert.equal(existsSync(at.path), false, "an empty store writes nothing");
    const secrets = new Map([0n, 1n, 2n, 70n].map((k) => [k, secret()] as const));
    s.put(secrets);
    assert.deepEqual(s.indexes(), [0n, 1n, 2n, 70n]);
    s.close();
    const again = SeedStore.open(at);
    for (const [k, top] of secrets) assert.equal(again.top(k), top);
    assert.equal(again.has(3n), false);
    assert.equal(again.top(3n), undefined);
    const file = readFileSync(at.path, "utf8");
    for (const top of secrets.values()) assert.ok(!file.includes(top), "a secret is never on disk in the clear");
    assert.ok(!file.includes(TABLE), "nor is the table");
    assert.equal(mode(at.path), 0o600);
    assert.equal(mode(at.backupPath), 0o600);
    // The backup is the same file, byte for byte.
    assert.ok(readFileSync(at.backupPath).equals(readFileSync(at.path)));
    assert.equal(again.backupCurrent, true);
    // A committed chain is never replaced; the same secret again is fine.
    assert.throws(() => again.put(new Map([[1n, secret()]])), /already holds another secret for game 1/);
    again.put(new Map([[1n, secrets.get(1n)!]]));
    assert.throws(() => again.put(new Map([[5n, "zz"]])), /32 bytes/);
    // No temporary file is left behind.
    assert.deepEqual(readdirSync(join(at.path, "..")).filter((f) => f.includes(".tmp")), []);
    again.close();
    console.log("✓ the store round-trips, encrypted, with a byte-for-byte backup");
  }

  // ---- tampering and a wrong key are detected
  {
    const at = place();
    const s = SeedStore.open(at);
    s.put(new Map([[0n, secret()]]));
    s.close();
    const good = readFileSync(at.path, "utf8");
    const env = JSON.parse(good) as { nonce: string; tag: string; data: string };
    const flip = (hex: string) => (hex[0] === "0" ? "1" : "0") + hex.slice(1);
    const data = Buffer.from(env.data, "base64");
    data[5] ^= 1;
    for (const [what, changed] of [
      ["the ciphertext", { ...env, data: data.toString("base64") }],
      ["the tag", { ...env, tag: flip(env.tag) }],
      ["the nonce", { ...env, nonce: flip(env.nonce) }],
    ] as const) {
      writeFileSync(at.path, JSON.stringify(changed));
      refuses(() => SeedStore.open(at), /doesn't decrypt/, `a store with ${what} changed`);
    }
    writeFileSync(at.path, good.slice(0, 40));
    refuses(() => SeedStore.open(at), /is damaged/, "a truncated store");
    writeFileSync(at.path, good);
    refuses(() => SeedStore.open({ ...at, key: randomBytes(32) }), /doesn't decrypt/, "a wrong key");
    refuses(() => SeedStore.open({ ...at, table: "boo1other" }), /is for the table/, "another table's store");
    refuses(() => SeedStore.open({ ...at, key: randomBytes(16) }), /32 bytes/, "a short key");
    refuses(() => SeedStore.open({ ...at, backupPath: at.path }), /another file/, "a backup on the store itself");
    // A refused open leaves no lock behind, and the good file still opens.
    SeedStore.open(at).close();
    console.log("✓ a changed store, a wrong key and another table's store are refused");
  }

  // ---- a crash at each step of a write leaves a complete store
  {
    const at = place();
    const first = new Map([[0n, secret()]]);
    const s = SeedStore.open(at);
    s.put(first);
    s.close();
    // A crash after the new file is written and before it replaces the old one: the old store stands.
    let crash: "rename" | "backup" | null = "rename";
    const faulty = SeedStore.open({ ...at, fault: (step) => { if (step === crash) throw new Error("crash"); } });
    assert.throws(() => faulty.put(new Map([[1n, secret()]])), /crash/);
    assert.equal(faulty.has(1n), false, "what didn't reach the disk isn't in memory either");
    faulty.close();
    let again = SeedStore.open(at);
    assert.deepEqual(again.indexes(), [0n], "the old store, whole");
    again.close();
    // A crash between the store and the backup: the store has the change, and the backup is
    // owed. `put` threw, so the dealer didn't commit; the next `put` (even of nothing) pays it.
    crash = "backup";
    const second = secret();
    const f2 = SeedStore.open({ ...at, fault: (step) => { if (step === crash) throw new Error("crash"); } });
    assert.throws(() => f2.put(new Map([[1n, second]])), /crash/);
    assert.equal(f2.backupCurrent, false);
    assert.equal(f2.top(1n), second);
    assert.ok(!readFileSync(at.backupPath).equals(readFileSync(at.path)));
    crash = null;
    f2.put(new Map());
    assert.equal(f2.backupCurrent, true);
    assert.ok(readFileSync(at.backupPath).equals(readFileSync(at.path)));
    f2.close();
    // The same crash, then a restart: opening brings the backup level.
    crash = "backup";
    const f3 = SeedStore.open({ ...at, fault: (step) => { if (step === crash) throw new Error("crash"); } });
    assert.throws(() => f3.put(new Map([[2n, secret()]])), /crash/);
    f3.close();
    again = SeedStore.open(at);
    assert.deepEqual(again.indexes(), [0n, 1n, 2n]);
    assert.ok(readFileSync(at.backupPath).equals(readFileSync(at.path)), "a reopen heals the backup");
    again.close();
    console.log("✓ a crash before the rename keeps the old store; one before the backup is healed");
  }

  // ---- the backup restores a lost store; a missing store with a backup isn't silently replaced
  {
    const at = place();
    const tops = new Map([[4n, secret()], [5n, secret()]]);
    const s = SeedStore.open(at);
    s.put(tops);
    s.close();
    rmSync(at.path);
    refuses(() => SeedStore.open(at), /is missing, but its backup .* is there: copy the backup/, "a lost store with its backup");
    copyFileSync(at.backupPath, at.path);
    const restored = SeedStore.open(at);
    for (const [k, top] of tops) assert.equal(restored.top(k), top);
    restored.close();
    // A backup directory that can't be written: `put` throws, so nothing is committed on a store with no backup.
    const noBackup = { ...place() };
    writeFileSync(join(dir, "not-a-dir"), "x");
    noBackup.backupPath = join(dir, "not-a-dir", "seeds.enc.json");
    const s2 = SeedStore.open(noBackup);
    assert.throws(() => s2.put(new Map([[0n, secret()]])));
    assert.equal(s2.backupCurrent, false);
    s2.close();
    console.log("✓ the backup restores a lost store, and a store that can't be backed up says so");
  }

  // ---- pruning, and a read-only open
  {
    const at = place();
    const s = SeedStore.open(at);
    const tops = new Map(Array.from({ length: 10 }, (_, i) => [BigInt(i), secret()] as const));
    s.put(tops);
    s.remove([0n, 1n, 2n, 99n]);
    assert.deepEqual(s.indexes(), [3n, 4n, 5n, 6n, 7n, 8n, 9n]);
    const file = readFileSync(at.path);
    s.close();
    const ro = SeedStore.open({ ...at, readOnly: true });
    assert.equal(ro.size, 7);
    assert.equal(ro.top(0n), undefined, "a pruned secret is gone from the file");
    assert.throws(() => ro.put(new Map([[50n, secret()]])), /read only/);
    assert.ok(readFileSync(at.path).equals(file));
    assert.equal(existsSync(`${at.path}.lock`), false, "a read-only open takes no lock");
    console.log("✓ settled secrets are pruned from the file; a read-only open writes nothing");
  }

  // ---- one daemon a store
  {
    const at = place();
    const a = SeedStore.open(at);
    // The lock names a live process that isn't this one.
    writeFileSync(`${at.path}.lock`, `${process.ppid}\n`);
    refuses(() => SeedStore.open(at), /another dealer \(pid \d+\) is using/, "a second daemon on the same store");
    // A lock left by a killed daemon (no such process) is taken over.
    writeFileSync(`${at.path}.lock`, "2147483646\n");
    const b = SeedStore.open(at);
    b.put(new Map([[0n, secret()]]));
    b.close();
    assert.equal(existsSync(`${at.path}.lock`), false);
    a.close();
    console.log("✓ a second daemon is refused; a killed daemon's lock is taken over");
  }

  // ---- the key and secret files
  {
    const keyFile = join(dir, "keys", "dealer-key.json");
    const secretFile = join(dir, "keys", "store-secret.json");
    assert.equal(createDealerKey(keyFile), true);
    assert.equal(createStoreKey(secretFile), true);
    assert.equal(mode(keyFile), 0o600);
    assert.equal(mode(secretFile), 0o600);
    const seed = readDealerSeed(keyFile);
    assert.match(seed, /^[0-9a-f]{64}$/);
    assert.equal(readStoreKey(secretFile).length, 32);
    // Made once: a second `init` keeps what's there.
    assert.equal(createDealerKey(keyFile), false);
    assert.equal(createStoreKey(secretFile), false);
    assert.equal(readDealerSeed(keyFile), seed);
    // Readable by the group or by anyone: refused, and the message holds no secret.
    for (const m of [0o640, 0o604, 0o644, 0o660]) {
      chmodSync(keyFile, m);
      assert.throws(() => readDealerSeed(keyFile), (e: unknown) => e instanceof Error && /can be read by other users/.test(e.message) && !e.message.includes(seed), `a key file of mode ${m.toString(8)}`);
      chmodSync(secretFile, m);
      assert.throws(() => readStoreKey(secretFile), /can be read by other users/);
    }
    chmodSync(keyFile, 0o600);
    chmodSync(secretFile, 0o400);
    readStoreKey(secretFile);
    assert.throws(() => readDealerSeed(join(dir, "keys", "nope.json")), /is missing: run `blackjack-dealer init`/);
    assert.throws(() => checkPrivate(join(dir, "keys"), "the key"), /isn't a file/);
    // A file that isn't ours: the error doesn't quote it.
    const junk = join(dir, "keys", "junk.json");
    writeFileSync(junk, JSON.stringify({ v: 1, seed: "hunter2" }), { mode: 0o600 });
    assert.throws(() => readDealerSeed(junk), (e: unknown) => e instanceof Error && !e.message.includes("hunter2"));
    // The keychain: `security` prints the key; it is never an argument.
    const stored = randomBytes(32).toString("hex");
    const seen: string[][] = [];
    const got = keychainStoreKey("svc", "acct", (args) => (seen.push([...args]), `${stored}\n`));
    assert.equal(got.toString("hex"), stored);
    assert.deepEqual(seen, [["find-generic-password", "-s", "svc", "-a", "acct", "-w"]]);
    assert.throws(() => keychainStoreKey("svc", "acct", () => { throw new Error("not found"); }), /keychain has no generic password/);
    assert.throws(() => keychainStoreKey("svc", "acct", () => "not hex"), (e: unknown) => e instanceof Error && /isn't 64 lowercase hex/.test(e.message) && !e.message.includes("not hex"));
    console.log("✓ the key and secret files are made 0600 once, and refused when others can read them");
  }
} finally {
  rmSync(dir, { recursive: true, force: true });
}
console.log("✅ the store tests passed");
