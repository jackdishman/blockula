/**
 * The daemon's two private files: its signing key (the wallet seed of the table's `dealer`
 * account) and the secret its seed store is encrypted with. Both are JSON, both are mode 0600,
 * and neither is ever read from a file that the group or anyone else can open. Their contents
 * never appear in an error message.
 */
import { execFileSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { closeSync, existsSync, fsyncSync, mkdirSync, openSync, readFileSync, statSync, writeSync } from "node:fs";
import { dirname } from "node:path";

/** Throws unless `path` is a regular file only its owner can read or write. */
export function checkPrivate(path: string, what: string): void {
  let st;
  try {
    st = statSync(path);
  } catch {
    throw new Error(`${what} (${path}) is missing: run \`blackjack-dealer init\` to make it`);
  }
  if (!st.isFile()) throw new Error(`${what} (${path}) isn't a file`);
  if ((st.mode & 0o077) !== 0) throw new Error(`${what} (${path}) can be read by other users (mode ${(st.mode & 0o777).toString(8)}): run \`chmod 600 ${path}\`, and treat what it holds as seen by others`);
}

/** Writes a new private file (mode 0600), durably. Never overwrites: throws if `path` exists. */
export function writePrivate(path: string, text: string): void {
  mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
  const fd = openSync(path, "wx", 0o600);
  try {
    writeSync(fd, text);
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
}

function readPrivateJson(path: string, what: string): Record<string, unknown> {
  checkPrivate(path, what);
  try {
    const v = JSON.parse(readFileSync(path, "utf8")) as unknown;
    if (v !== null && typeof v === "object" && !Array.isArray(v)) return v as Record<string, unknown>;
  } catch {
    // Said below, without the file's text.
  }
  throw new Error(`${what} (${path}) isn't the JSON \`blackjack-dealer init\` writes`);
}

const HEX32 = /^[0-9a-f]{64}$/;

/** The dealer's wallet seed (hex), from its key file. */
export function readDealerSeed(path: string): string {
  const v = readPrivateJson(path, "the dealer's key file");
  if (v.v !== 1 || typeof v.seed !== "string" || !HEX32.test(v.seed)) throw new Error(`the dealer's key file (${path}) isn't the JSON \`blackjack-dealer init\` writes`);
  return v.seed;
}

/** The seed store's encryption key, from the secret file. */
export function readStoreKey(path: string): Buffer {
  const v = readPrivateJson(path, "the store's secret file");
  if (v.v !== 1 || typeof v.storeKey !== "string" || !HEX32.test(v.storeKey)) throw new Error(`the store's secret file (${path}) isn't the JSON \`blackjack-dealer init\` writes`);
  return Buffer.from(v.storeKey, "hex");
}

/** Makes the key file with a new random seed, unless it's there. Returns whether it made it. */
export function createDealerKey(path: string): boolean {
  if (existsSync(path)) return false;
  writePrivate(path, JSON.stringify({ v: 1, seed: randomBytes(32).toString("hex") }) + "\n");
  return true;
}

/** Makes the secret file with a new random key, unless it's there. Returns whether it made it. */
export function createStoreKey(path: string): boolean {
  if (existsSync(path)) return false;
  writePrivate(path, JSON.stringify({ v: 1, storeKey: randomBytes(32).toString("hex") }) + "\n");
  return true;
}

/** Runs macOS's `security` and returns what it printed. Replaced in tests. */
export type SecurityRunner = (args: readonly string[]) => string;

const runSecurity: SecurityRunner = (args) => execFileSync("/usr/bin/security", [...args], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });

/**
 * The store's key from the macOS keychain: a generic password (64 hex characters) under
 * `service` and `account`. `security` prints it on its standard output, so it is never in an
 * argument list. The README says how an operator puts it there.
 */
export function keychainStoreKey(service: string, account: string, run: SecurityRunner = runSecurity): Buffer {
  let out: string;
  try {
    out = run(["find-generic-password", "-s", service, "-a", account, "-w"]);
  } catch {
    throw new Error(`the keychain has no generic password for service ${JSON.stringify(service)} and account ${JSON.stringify(account)} (or it is locked): see the dealer's README, "The keychain"`);
  }
  const key = out.trim();
  if (!HEX32.test(key)) throw new Error(`the keychain's password for service ${JSON.stringify(service)} isn't 64 lowercase hex characters`);
  return Buffer.from(key, "hex");
}
