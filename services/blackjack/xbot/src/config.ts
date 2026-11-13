/**
 * Where the bot keeps its things, and its settings. The home directory holds the relayer's key
 * file, the store and an optional `config.json`. Settings come from flags, then the
 * environment, then `config.json`, then the defaults.
 *
 * The key file holds the relayer's seed and nothing else. It is made once by `init`, readable
 * by its owner only (0600), never overwritten, and refused if anyone else can read it. The
 * seed is never printed, logged or passed on a command line.
 */
import { chmodSync, existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { DEFAULT_CONFIG, type BotConfig } from "./types.js";

export const defaultHome = () => process.env.BLACKJACK_XBOT_HOME || join(homedir(), ".blockula", "blackjack-xbot");
export const keyPath = (home: string) => join(home, "relayer.key.json");
export const storePath = (home: string) => join(home, "store.json");
export const configPath = (home: string) => join(home, "config.json");

/** Writes a new key file holding `seedHex`. Refuses to replace one. */
export function writeKey(home: string, seedHex: string): string {
  mkdirSync(home, { recursive: true, mode: 0o700 });
  const path = keyPath(home);
  // "wx": fails if the file exists, so a key is never overwritten.
  writeFileSync(path, JSON.stringify({ seed: seedHex }) + "\n", { mode: 0o600, flag: "wx" });
  chmodSync(path, 0o600);
  return path;
}

/** The seed in the key file. Refuses a file that its group or anyone else can read. */
export function readKey(home: string): string {
  const path = keyPath(home);
  if (!existsSync(path)) throw new Error(`no relayer key at ${path}: run "blackjack-xbot init" first`);
  if ((statSync(path).mode & 0o077) !== 0) throw new Error(`${path} can be read by others: it must be mode 0600`);
  const seed = (JSON.parse(readFileSync(path, "utf8")) as { seed?: unknown }).seed;
  if (typeof seed !== "string" || !/^[0-9a-f]{64}$/.test(seed)) throw new Error(`${path} isn't a relayer key file`);
  return seed;
}

export interface Settings {
  /** The local node. */
  rpc: string | null;
  /** The table's address. */
  table: string | null;
  bot: BotConfig;
}

const NUMBERS = ["cooldownMs", "perHour", "globalPerMinute", "refusalCooldownMs", "refusalsPerHour", "linkTtlMs", "listEveryMs", "revealWaitMs", "pollMs", "maxPostAgeMs"] as const;

/** The settings in `home`'s `config.json` (if any), under the environment's and `flags`. */
export function loadSettings(home: string, flags: { rpc?: string; table?: string } = {}): Settings {
  let file: Record<string, unknown> = {};
  if (existsSync(configPath(home))) {
    const parsed: unknown = JSON.parse(readFileSync(configPath(home), "utf8"));
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error(`${configPath(home)} isn't a JSON object`);
    file = parsed as Record<string, unknown>;
  }
  const bot: BotConfig = { ...DEFAULT_CONFIG };
  for (const k of NUMBERS) {
    const v = file[k];
    if (v === undefined) continue;
    if (typeof v !== "number" || !Number.isSafeInteger(v) || v < 0) throw new Error(`config.json: ${k} must be a whole number, 0 or more`);
    bot[k] = v;
  }
  if (file.blocked !== undefined) {
    if (!Array.isArray(file.blocked) || !file.blocked.every((h) => typeof h === "string" && /^[A-Za-z0-9_]{1,15}$/.test(h))) throw new Error("config.json: blocked must be a list of X handles, without the @");
    bot.blocked = (file.blocked as string[]).map((h) => h.toLowerCase());
  }
  const str = (v: unknown) => (typeof v === "string" && v !== "" ? v : null);
  return { rpc: flags.rpc ?? str(process.env.BLOCKULA_RPC) ?? str(file.rpc), table: flags.table ?? str(process.env.BLACKJACK_TABLE) ?? str(file.table), bot };
}
