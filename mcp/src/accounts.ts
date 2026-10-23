// The agent's key file (`agent-wallet.json`) and the account it acts for, per chain (AGENT-2).
//
// - The account `use_account` chose is kept per chain (`accounts`, keyed `<chain_id> <genesis_hash>`).
//   0.5.1 kept one `account` for every network, so a testnet `tboo1…` account followed the agent to
//   other chains. It moves to the first chain whose prefix it matches; on any other chain it's
//   ignored (wallet_info says so) and left in place for its own chain.
// - The file is written whole (a temporary file, then a rename) with mode 600, and keeps every
//   field it was read with, so rewriting it can't lose the key.
import { linkSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { isAddress } from "@blockula/sdk";

/** The key file's secret field (written by 0.5.1 and every release since). */
const SECRET = "seed";

/** A key file as it's stored. Unknown fields are kept as they are. */
export type WalletFile = {
  address: string;
  key_id: string;
  /** The account `use_account` chose, per chain (`<chain_id> <genesis_hash>`). */
  accounts?: Record<string, string>;
  /** 0.5.1's one account for every network: moved into `accounts` by `actingFor`. */
  account?: string;
  [field: string]: unknown;
};

/** The key file's 32-byte secret (hex). */
export function secretOf(f: WalletFile): string {
  const v = f[SECRET];
  if (typeof v !== "string" || !/^[0-9a-f]{64}$/i.test(v)) throw new Error("the agent's key file has no valid key; restore it from a backup, or move it aside and call create_wallet");
  return v;
}

/** A new key file's contents. */
export function newWalletFile(address: string, keyId: string, secretHex: string): WalletFile {
  return { address, key_id: keyId, [SECRET]: secretHex };
}

export function readWalletFile(file: string): WalletFile | null {
  let raw: string;
  try {
    raw = readFileSync(file, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return null;
    throw e;
  }
  let f: unknown;
  try {
    f = JSON.parse(raw);
  } catch {
    throw new Error(`${file} is unreadable (it holds the agent's key): fix it, or restore it from a backup`);
  }
  if (!f || typeof f !== "object" || Array.isArray(f)) throw new Error(`${file} is malformed (it holds the agent's key): fix it, or restore it from a backup`);
  return f as WalletFile;
}

/** Writes the whole file (a temporary file beside it, then a rename), readable by this user only. */
export function writeWalletFile(file: string, f: WalletFile) {
  secretOf(f); // never write a file without its key
  mkdirSync(dirname(file), { recursive: true });
  const tmp = `${file}.${process.pid}.tmp`;
  try {
    writeFileSync(tmp, JSON.stringify(f, null, 2), { mode: 0o600 });
    renameSync(tmp, file);
  } finally {
    rmSync(tmp, { force: true });
  }
}

/**
 * Writes a new key file, only if there's none yet (a link, which fails if the file exists): two
 * sessions creating a wallet at once can't replace each other's key. The loser reads the winner's.
 */
export function createWalletFile(file: string, f: WalletFile) {
  secretOf(f);
  mkdirSync(dirname(file), { recursive: true });
  const tmp = `${file}.${process.pid}.new`;
  try {
    writeFileSync(tmp, JSON.stringify(f, null, 2), { mode: 0o600 });
    linkSync(tmp, file);
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code !== "EEXIST") throw e;
  } finally {
    rmSync(tmp, { force: true });
  }
}

/** Who the agent acts for on one chain. */
export interface ActingFor {
  /** The account chosen with use_account on this chain; undefined: the key's own address. */
  account?: string;
  /** A 0.5.1 account that belongs to another chain (another prefix): not used here. */
  ignored?: string;
  /** The file changed (a 0.5.1 account moved to this chain): write it back. */
  changed: boolean;
}

/** The account the agent acts for on the chain `chain` (`<chain_id> <genesis_hash>`) whose addresses start with `prefix`1. */
export function actingFor(f: WalletFile, chain: string, prefix: string): ActingFor {
  const chosen = f.accounts?.[chain];
  if (chosen !== undefined) {
    const legacy = typeof f.account === "string" && !isAddress(f.account, prefix) ? { ignored: f.account } : {};
    return isAddress(chosen, prefix) ? { account: chosen, changed: false, ...legacy } : { ignored: chosen, changed: false };
  }
  if (typeof f.account !== "string" || f.account === "") return { changed: false };
  if (!isAddress(f.account, prefix)) return { ignored: f.account, changed: false };
  // 0.5.1's account matches this chain's prefix: it's this chain's from now on.
  f.accounts = { ...(f.accounts ?? {}), [chain]: f.account };
  delete f.account;
  return { account: f.accounts[chain], changed: true };
}

/** Sets (or, with undefined, clears) the account the agent acts for on `chain`. */
export function setActingFor(f: WalletFile, chain: string, account: string | undefined) {
  const accounts = { ...(f.accounts ?? {}) };
  if (account === undefined) delete accounts[chain];
  else accounts[chain] = account;
  f.accounts = accounts;
}

/** What wallet_info says about an ignored 0.5.1 account. */
export function ignoredNote(account: string, prefix: string): string {
  const other = account.startsWith("tboo1") ? "the public testnet's" : account.startsWith("boo1") ? "a boo1… chain's" : "another chain's";
  return `This agent used to act for ${account}, ${other} account, which isn't an address on this chain (its addresses start with ${prefix}1). Here it acts for its own key until use_account is called with this chain's account.`;
}

/** The web wallet's "Give an agent a budget", by chain id (where the user pastes a grant code). */
export const WALLET_URLS: Record<string, string> = {
  "blockula-testnet-2": "https://testnet.blockula.app/devices#agent",
};

/** The short code the web wallet shows for a key (web/lib/format.ts `keyCode`), for the user to compare. */
export const checkCode = (keyId: string) => {
  const k = keyId.toUpperCase();
  return `${k.slice(0, 4)}-${k.slice(4, 8)}-${k.slice(-4)}`;
};
