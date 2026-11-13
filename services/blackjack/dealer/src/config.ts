/**
 * The daemon's configuration: command-line flags, then the environment, then a JSON file, then
 * the defaults. The README lists every knob.
 *
 * There is no default node: the RPC URL must be given, and it must be this machine's
 * (127.0.0.1, localhost or ::1) unless `allowRemote` says otherwise and the chain is pinned.
 */
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { MAX_COMMIT_BATCH } from "@blockula/blackjack";

export interface DealerConfig {
  /** The node's URL. */
  rpc: string;
  /** The table: the program's address. Empty only for `init`. */
  table: string;
  /** Where the files below live unless they're given. */
  dir: string;
  keyFile: string;
  secretFile: string;
  storePath: string;
  backupPath: string;
  /** Top up when fewer unused commitments than this are left. */
  lowWater: number;
  /** Top up to this many unused commitments. */
  target: number;
  /** The most commitments in one `commit_batch` (1 to 64). */
  batch: number;
  /** Milliseconds between reads of the table. */
  pollMs: number;
  /** Read and log what would be sent; send nothing, write no secret. */
  dryRun: boolean;
  /** Let `rpc` be another machine's node. Needs `chainId` and `genesisHash`. */
  allowRemote: boolean;
  /** Read the store's key from the macOS keychain instead of the secret file. */
  keychain: boolean;
  keychainService: string;
  keychainAccount: string;
  /** Warn when the dealer's balance is under this (FANG, a decimal string). */
  lowBalance: string;
  /** Secrets of settled games kept before pruning. */
  keepSettled: number;
  /** Blocks a commitment or reveal transaction stays valid. */
  txValidBlocks: number;
  /** Reserve the chain's maximum fuel for every call (dearer, and three such calls fill a block). */
  fullFuel: boolean;
  /** The chain id and genesis hash the node must report, when pinned. */
  chainId?: string;
  genesisHash?: string;
}

/** Every setting by its JSON key, with its flag and environment variable. */
const KNOBS = [
  ["rpc", "--rpc", "BJ_DEALER_RPC"],
  ["table", "--table", "BJ_DEALER_TABLE"],
  ["dir", "--dir", "BJ_DEALER_DIR"],
  ["keyFile", "--key-file", "BJ_DEALER_KEY_FILE"],
  ["secretFile", "--secret-file", "BJ_DEALER_SECRET_FILE"],
  ["storePath", "--store", "BJ_DEALER_STORE"],
  ["backupPath", "--backup", "BJ_DEALER_BACKUP"],
  ["lowWater", "--low-water", "BJ_DEALER_LOW_WATER"],
  ["target", "--target", "BJ_DEALER_TARGET"],
  ["batch", "--batch", "BJ_DEALER_BATCH"],
  ["pollMs", "--poll-ms", "BJ_DEALER_POLL_MS"],
  ["dryRun", "--dry-run", "BJ_DEALER_DRY_RUN"],
  ["allowRemote", "--allow-remote", "BJ_DEALER_ALLOW_REMOTE"],
  ["keychain", "--keychain", "BJ_DEALER_KEYCHAIN"],
  ["keychainService", "--keychain-service", "BJ_DEALER_KEYCHAIN_SERVICE"],
  ["keychainAccount", "--keychain-account", "BJ_DEALER_KEYCHAIN_ACCOUNT"],
  ["lowBalance", "--low-balance", "BJ_DEALER_LOW_BALANCE"],
  ["keepSettled", "--keep-settled", "BJ_DEALER_KEEP_SETTLED"],
  ["txValidBlocks", "--tx-valid-blocks", "BJ_DEALER_TX_VALID_BLOCKS"],
  ["fullFuel", "--full-fuel", "BJ_DEALER_FULL_FUEL"],
  ["chainId", "--chain-id", "BJ_DEALER_CHAIN_ID"],
  ["genesisHash", "--genesis-hash", "BJ_DEALER_GENESIS_HASH"],
] as const;
type Key = (typeof KNOBS)[number][0];
const BOOLEANS: ReadonlySet<string> = new Set(["dryRun", "allowRemote", "keychain", "fullFuel"]);

export interface ParsedArgs {
  command: string | undefined;
  /** Settings given as flags, by JSON key. */
  flags: Partial<Record<Key, string>>;
  /** `--config <file>`. */
  configFile?: string;
  json: boolean;
}

/** Splits `argv` (after the program's name) into the command and its flags. Throws on a flag it doesn't know. */
export function parseArgs(argv: readonly string[]): ParsedArgs {
  const out: ParsedArgs = { command: undefined, flags: {}, json: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (!a.startsWith("--")) {
      if (out.command !== undefined) throw new Error(`one command at a time: ${out.command}, then ${a}?`);
      out.command = a;
      continue;
    }
    const eq = a.indexOf("=");
    const name = eq < 0 ? a : a.slice(0, eq);
    const take = (): string => {
      if (eq >= 0) return a.slice(eq + 1);
      const next = argv[++i];
      if (next === undefined) throw new Error(`${name} needs a value`);
      return next;
    };
    if (name === "--json") {
      out.json = true;
      continue;
    }
    if (name === "--config") {
      out.configFile = take();
      continue;
    }
    const knob = KNOBS.find((k) => k[1] === name);
    if (!knob) throw new Error(`unknown option ${name}`);
    out.flags[knob[0]] = BOOLEANS.has(knob[0]) ? (eq >= 0 ? a.slice(eq + 1) : "true") : take();
  }
  return out;
}

function bool(v: unknown, what: string): boolean {
  if (typeof v === "boolean") return v;
  if (v === "true" || v === "1" || v === "yes") return true;
  if (v === "false" || v === "0" || v === "no" || v === "") return false;
  throw new Error(`${what} is true or false`);
}

function whole(v: unknown, what: string, min: number, max: number): number {
  const n = typeof v === "number" ? v : typeof v === "string" && /^\d+$/.test(v.trim()) ? Number(v.trim()) : NaN;
  if (!Number.isSafeInteger(n) || n < min || n > max) throw new Error(`${what} is a whole number from ${min} to ${max}`);
  return n;
}

/** Whether `rpc` is a node on this machine. */
export function isLocalRpc(rpc: string): boolean {
  let url: URL;
  try {
    url = new URL(rpc);
  } catch {
    return false;
  }
  return ["127.0.0.1", "localhost", "[::1]"].includes(url.hostname);
}

export const DEFAULT_DIR = join(homedir(), ".blockula", "blackjack-dealer");

/**
 * The configuration from `args`, `env` and the config file (`--config`, or `BJ_DEALER_CONFIG`).
 * `needTable` is false for `init`, which only makes the key and secret files.
 */
export function loadConfig(args: ParsedArgs, env: Readonly<Record<string, string | undefined>>, opts: { needTable?: boolean; needRpc?: boolean } = {}): DealerConfig {
  const configFile = args.configFile ?? env.BJ_DEALER_CONFIG;
  let file: Record<string, unknown> = {};
  if (configFile) {
    let parsed: unknown;
    try {
      parsed = JSON.parse(readFileSync(configFile, "utf8"));
    } catch (e) {
      throw new Error(`the config file ${configFile} can't be read as JSON (${(e as Error).message})`);
    }
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error(`the config file ${configFile} must hold one JSON object`);
    file = parsed as Record<string, unknown>;
    const known = new Set<string>(KNOBS.map((k) => k[0]));
    for (const k of Object.keys(file)) if (!known.has(k)) throw new Error(`the config file ${configFile} has a setting this daemon doesn't know: ${k}`);
  }
  const raw = (key: Key): unknown => {
    const knob = KNOBS.find((k) => k[0] === key)!;
    const fromEnv = env[knob[2]] ?? (key === "rpc" ? env.BLOCKULA_RPC : undefined);
    return args.flags[key] ?? (fromEnv !== undefined && fromEnv !== "" ? fromEnv : undefined) ?? file[key];
  };
  const str = (key: Key): string | undefined => {
    const v = raw(key);
    if (v === undefined) return undefined;
    if (typeof v !== "string") throw new Error(`${key} is text`);
    return v.trim();
  };
  const num = (key: Key, fallback: number, min: number, max: number): number => (raw(key) === undefined ? fallback : whole(raw(key), key, min, max));
  const flag = (key: Key): boolean => (raw(key) === undefined ? false : bool(raw(key), key));

  const rpc = (str("rpc") ?? "").replace(/\/+$/, "");
  const allowRemote = flag("allowRemote");
  const chainId = str("chainId") || undefined;
  const genesisHash = str("genesisHash")?.toLowerCase() || undefined;
  if (genesisHash !== undefined && !/^[0-9a-f]{64}$/.test(genesisHash)) throw new Error("genesisHash is 64 hex characters");
  if (opts.needRpc !== false || rpc !== "") {
    if (rpc === "") throw new Error("no node: set --rpc (or BJ_DEALER_RPC) to the URL of the node the dealer talks to. There is no default");
    if (!/^https?:\/\//.test(rpc)) throw new Error("rpc is an http:// or https:// URL");
    if (!isLocalRpc(rpc)) {
      if (!allowRemote) throw new Error(`${new URL(rpc).host} isn't this machine: the dealer talks to a node on 127.0.0.1 unless allowRemote is set`);
      if (!chainId || !genesisHash) throw new Error("a node on another machine needs the chain pinned: set chainId and genesisHash too");
    }
  }
  const table = str("table") ?? "";
  if (opts.needTable !== false && !/^t?boo1[0-9a-z]+$/.test(table)) throw new Error("no table: set --table (or BJ_DEALER_TABLE) to the program's address (boo1…)");

  const dir = resolve(str("dir") || DEFAULT_DIR);
  const path = (key: Key, name: string): string => resolve(str(key) || join(dir, name));
  const storePath = path("storePath", "seeds.enc.json");
  const cfg: DealerConfig = {
    rpc,
    table,
    dir,
    keyFile: path("keyFile", "dealer-key.json"),
    secretFile: path("secretFile", "store-secret.json"),
    storePath,
    backupPath: resolve(str("backupPath") || `${storePath}.backup`),
    lowWater: num("lowWater", 64, 1, 4096),
    target: num("target", 192, 1, 8192),
    batch: num("batch", MAX_COMMIT_BATCH, 1, MAX_COMMIT_BATCH),
    pollMs: num("pollMs", 1000, 50, 60_000),
    dryRun: flag("dryRun"),
    allowRemote,
    keychain: flag("keychain"),
    keychainService: str("keychainService") || "blockula-blackjack-dealer",
    keychainAccount: str("keychainAccount") || table || "store",
    lowBalance: str("lowBalance") || "5",
    keepSettled: num("keepSettled", 256, 0, 1_000_000),
    txValidBlocks: num("txValidBlocks", 120, 2, 9000),
    fullFuel: flag("fullFuel"),
    chainId,
    genesisHash,
  };
  if (!/^\d+(\.\d{1,9})?$/.test(cfg.lowBalance)) throw new Error("lowBalance is an amount of FANG, like 5 or 2.5");
  if (cfg.target < cfg.lowWater) throw new Error("target can't be under lowWater: the dealer tops up to target when fewer than lowWater commitments are left");
  if (cfg.storePath === cfg.backupPath) throw new Error("the backup must be another file than the store");
  return cfg;
}
