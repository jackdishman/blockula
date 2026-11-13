/**
 * The dealer's seed store: game index k → the top secret of game k's hash chain.
 *
 * These secrets are every card the table will deal. The store is one file, encrypted and
 * authenticated (AES-256-GCM) with a key from the secret file, and a byte-for-byte backup at a
 * second path. Every change writes a whole new file beside the old one, flushes it, renames it
 * over the old one and flushes the directory, first for the store and then for the backup, so a
 * crash at any point leaves a complete store (the old one or the new one) and `put` returns only
 * once both copies are on disk. The dealer calls `put` before it submits a commitment.
 *
 * A file that was changed, or opened with another key, doesn't decrypt: `open` throws and the
 * daemon doesn't start. No secret is ever part of an error message.
 */
import { createCipheriv, createDecipheriv, randomBytes } from "node:crypto";
import { closeSync, existsSync, fsyncSync, mkdirSync, openSync, readFileSync, renameSync, unlinkSync, writeFileSync, writeSync } from "node:fs";
import { dirname } from "node:path";

/** What the dealer needs from a store. `put` and `remove` return once the change is durable. */
export interface SecretStore {
  readonly size: number;
  has(k: bigint): boolean;
  /** The top link of game `k`'s chain (hex), if the store has it. */
  top(k: bigint): string | undefined;
  /** Every index held, ascending. */
  indexes(): bigint[];
  /**
   * Adds secrets, durably (the store and its backup), before returning. An index the store
   * already holds keeps its secret: giving another one for it throws. Call it with nothing to
   * add to make sure an earlier change reached the backup.
   */
  put(secrets: ReadonlyMap<bigint, string>): void;
  /** Forgets the secrets of settled games. */
  remove(indexes: readonly bigint[]): void;
}

/** The store can't be used as it is: the daemon refuses to start. */
export class StoreError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "StoreError";
  }
}

const AAD = Buffer.from("blockula/blackjack/dealer-store/v1");
const HEX32 = /^[0-9a-f]{64}$/;

export interface SeedStoreOptions {
  /** The store's file. */
  path: string;
  /** The backup's file: best on another disk. */
  backupPath: string;
  /** The 32-byte encryption key. */
  key: Buffer;
  /** The table this store is for: a store made for another table is refused. */
  table: string;
  /** Only read: no lock, nothing written (the `status` command). */
  readOnly?: boolean;
  /** Tests: called before each step that a crash could interrupt; throwing stands in for the crash. */
  fault?: (step: "rename" | "backup", path: string) => void;
}

function encrypt(key: Buffer, plain: Buffer): Buffer {
  const nonce = randomBytes(12);
  const cipher = createCipheriv("aes-256-gcm", key, nonce);
  cipher.setAAD(AAD);
  const data = Buffer.concat([cipher.update(plain), cipher.final()]);
  return Buffer.from(JSON.stringify({ v: 1, alg: "aes-256-gcm", nonce: nonce.toString("hex"), tag: cipher.getAuthTag().toString("hex"), data: data.toString("base64") }) + "\n");
}

function decrypt(key: Buffer, file: Buffer, path: string): Buffer {
  let env: { v?: unknown; alg?: unknown; nonce?: unknown; tag?: unknown; data?: unknown };
  try {
    env = JSON.parse(file.toString("utf8")) as typeof env;
  } catch {
    throw new StoreError(`the seed store ${path} is damaged: it isn't the file the dealer wrote`);
  }
  if (env === null || typeof env !== "object" || env.v !== 1 || env.alg !== "aes-256-gcm" || typeof env.nonce !== "string" || typeof env.tag !== "string" || typeof env.data !== "string" || !/^[0-9a-f]{24}$/.test(env.nonce) || !/^[0-9a-f]{32}$/.test(env.tag)) {
    throw new StoreError(`the seed store ${path} is damaged: it isn't the file the dealer wrote`);
  }
  try {
    const decipher = createDecipheriv("aes-256-gcm", key, Buffer.from(env.nonce, "hex"));
    decipher.setAAD(AAD);
    decipher.setAuthTag(Buffer.from(env.tag, "hex"));
    return Buffer.concat([decipher.update(Buffer.from(env.data, "base64")), decipher.final()]);
  } catch {
    throw new StoreError(`the seed store ${path} doesn't decrypt: it was changed, or the secret isn't the one it was written with`);
  }
}

/** Flushes a directory, so a rename inside it survives a power cut. */
function syncDir(dir: string): void {
  let fd: number | undefined;
  try {
    fd = openSync(dir, "r");
    fsyncSync(fd);
  } catch {
    // Some filesystems don't flush directories; the file itself was flushed.
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}

/** Replaces `path` with `bytes`: a complete new file or the old one, never a part of either. */
function atomicWrite(path: string, bytes: Buffer, beforeRename?: () => void): void {
  mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
  const tmp = `${path}.tmp-${process.pid}`;
  const fd = openSync(tmp, "w", 0o600);
  try {
    writeSync(fd, bytes);
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
  beforeRename?.();
  renameSync(tmp, path);
  syncDir(dirname(path));
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return (e as NodeJS.ErrnoException).code === "EPERM";
  }
}

export class SeedStore implements SecretStore {
  private secrets = new Map<bigint, string>();
  /** The store's file holds something its backup doesn't yet. */
  private backupBehind = false;
  private locked = false;

  private constructor(private readonly opts: SeedStoreOptions) {}

  /**
   * Opens the store, or starts an empty one when neither it nor its backup exists (nothing is
   * written until the first `put`). Throws `StoreError` when the file doesn't decrypt, is for
   * another table, is missing while its backup is there, or another daemon holds it.
   */
  static open(opts: SeedStoreOptions): SeedStore {
    if (opts.key.length !== 32) throw new StoreError("the store's key must be 32 bytes");
    if (opts.path === opts.backupPath) throw new StoreError("the backup must be another file than the store");
    const store = new SeedStore(opts);
    if (!opts.readOnly) store.lock();
    try {
      store.load();
    } catch (e) {
      store.close();
      throw e;
    }
    return store;
  }

  private get lockPath(): string {
    return `${this.opts.path}.lock`;
  }

  /** One daemon a store: two would each write the file without the other's secrets. */
  private lock(): void {
    mkdirSync(dirname(this.opts.path), { recursive: true, mode: 0o700 });
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        writeFileSync(this.lockPath, `${process.pid}\n`, { flag: "wx", mode: 0o600 });
        this.locked = true;
        return;
      } catch (e) {
        if ((e as NodeJS.ErrnoException).code !== "EEXIST") throw e;
      }
      const pid = Number(readFileSync(this.lockPath, "utf8").trim());
      if (Number.isSafeInteger(pid) && pid > 0 && pid !== process.pid && alive(pid)) throw new StoreError(`another dealer (pid ${pid}) is using the seed store ${this.opts.path}: one daemon a table`);
      // Left by a daemon that was killed: take it over.
      unlinkSync(this.lockPath);
    }
    throw new StoreError(`couldn't take the lock ${this.lockPath}`);
  }

  private load(): void {
    const { path, backupPath, key, table } = this.opts;
    if (!existsSync(path)) {
      if (existsSync(backupPath)) throw new StoreError(`the seed store ${path} is missing, but its backup ${backupPath} is there: copy the backup to ${path} and start again (the README's "Restore from the backup")`);
      return;
    }
    const file = readFileSync(path);
    let plain: { table?: unknown; secrets?: unknown };
    try {
      plain = JSON.parse(decrypt(key, file, path).toString("utf8")) as typeof plain;
    } catch (e) {
      if (e instanceof StoreError) throw e;
      throw new StoreError(`the seed store ${path} is damaged: it isn't the file the dealer wrote`);
    }
    if (plain === null || typeof plain !== "object" || typeof plain.table !== "string" || plain.secrets === null || typeof plain.secrets !== "object") throw new StoreError(`the seed store ${path} is damaged: it isn't the file the dealer wrote`);
    if (plain.table !== table) throw new StoreError(`the seed store ${path} is for the table ${plain.table}, not ${table}: give each table its own store`);
    for (const [k, top] of Object.entries(plain.secrets as Record<string, unknown>)) {
      if (!/^\d+$/.test(k) || typeof top !== "string" || !HEX32.test(top)) throw new StoreError(`the seed store ${path} is damaged: it isn't the file the dealer wrote`);
      this.secrets.set(BigInt(k), top);
    }
    // A crash between the two writes leaves the backup one change behind: bring it level.
    this.backupBehind = !existsSync(backupPath) || !readFileSync(backupPath).equals(file);
    if (this.backupBehind && !this.opts.readOnly) {
      atomicWrite(backupPath, file);
      this.backupBehind = false;
    }
  }

  /** Whether the backup is the same file as the store (always, after a `put` that returned). */
  get backupCurrent(): boolean {
    return !this.backupBehind;
  }

  get size(): number {
    return this.secrets.size;
  }

  has(k: bigint): boolean {
    return this.secrets.has(k);
  }

  top(k: bigint): string | undefined {
    return this.secrets.get(k);
  }

  indexes(): bigint[] {
    return [...this.secrets.keys()].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  }

  private write(next: Map<bigint, string>): void {
    if (this.opts.readOnly) throw new StoreError("this store was opened to be read only");
    const { path, backupPath, key, table, fault } = this.opts;
    const secrets: Record<string, string> = {};
    for (const [k, top] of [...next].sort((a, b) => (a[0] < b[0] ? -1 : 1))) secrets[k.toString()] = top;
    const file = encrypt(key, Buffer.from(JSON.stringify({ table, secrets })));
    atomicWrite(path, file, () => fault?.("rename", path));
    // From here the store's file holds `next`: memory follows it, and the backup is owed.
    this.secrets = next;
    this.backupBehind = true;
    fault?.("backup", backupPath);
    atomicWrite(backupPath, file);
    this.backupBehind = false;
  }

  put(secrets: ReadonlyMap<bigint, string>): void {
    const next = new Map(this.secrets);
    let changed = false;
    for (const [k, top] of secrets) {
      if (k < 0n || k >= 1n << 64n) throw new Error("a game index is a u64");
      if (!HEX32.test(top)) throw new Error("a chain's top is 32 bytes of lowercase hex");
      const held = next.get(k);
      if (held === undefined) {
        next.set(k, top);
        changed = true;
      } else if (held !== top) throw new Error(`the store already holds another secret for game ${k}: a committed chain is never replaced`);
    }
    if (changed || this.backupBehind) this.write(next);
  }

  remove(indexes: readonly bigint[]): void {
    const next = new Map(this.secrets);
    let changed = false;
    for (const k of indexes) changed = next.delete(k) || changed;
    if (changed || this.backupBehind) this.write(next);
  }

  /** Gives the lock back. */
  close(): void {
    if (!this.locked) return;
    this.locked = false;
    try {
      unlinkSync(this.lockPath);
    } catch {
      // Already gone.
    }
  }
}
