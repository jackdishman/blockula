/**
 * The daemon's log: one JSON object a line (time, level, action, and what the action was about).
 *
 * Nothing secret is ever passed to it, and it doesn't rely on that: every string it writes has
 * any long run of hex taken out first, except the fields that are public ids by name (`tx`).
 * A chain link or a store key is 64 hex characters, so one that reached a message by accident
 * (inside a node's error text, say) still never reaches the log.
 */

export type LogValue = string | number | bigint | boolean | null | undefined | readonly LogValue[] | { readonly [key: string]: LogValue };
export type LogFields = { readonly [key: string]: LogValue };

export interface Logger {
  info(action: string, fields?: LogFields): void;
  warn(action: string, fields?: LogFields): void;
  error(action: string, fields?: LogFields): void;
}

/** Fields whose value is a public id written out in full: a transaction's id. */
const PUBLIC_HEX = new Set(["tx"]);

/** `text` without any run of 32 or more hex characters (a link, a secret, a key). */
export function scrub(text: string): string {
  return text.replace(/[0-9a-fA-F]{32,}/g, "[hex removed]");
}

/** What an error says, scrubbed and kept short: for a log line or a message a person reads. */
export function errorText(e: unknown): string {
  const text = e instanceof Error ? `${e.name}: ${e.message}` : String(e);
  return scrub(text).slice(0, 400);
}

function clean(key: string, v: LogValue): unknown {
  if (v === undefined || v === null) return null;
  if (typeof v === "bigint") return v.toString();
  if (typeof v === "string") return PUBLIC_HEX.has(key) && /^[0-9a-f]{64}$/.test(v) ? v : scrub(v);
  if (typeof v === "number" || typeof v === "boolean") return v;
  if (Array.isArray(v)) return v.map((x) => clean("", x as LogValue));
  return Object.fromEntries(Object.entries(v as { [key: string]: LogValue }).map(([k, x]) => [k, clean(k, x)]));
}

/** One log line: `{"time":…,"level":…,"action":…,…fields}`. */
export function logLine(level: "info" | "warn" | "error", action: string, fields: LogFields = {}, now: Date = new Date()): string {
  const out: Record<string, unknown> = { time: now.toISOString(), level, action: scrub(action) };
  for (const [k, v] of Object.entries(fields)) if (v !== undefined) out[k] = clean(k, v);
  return JSON.stringify(out);
}

/** A logger that writes JSON lines with `write` (standard output by default). */
export function jsonLogger(write: (line: string) => void = (line) => void process.stdout.write(line + "\n")): Logger {
  const at = (level: "info" | "warn" | "error") => (action: string, fields?: LogFields) => write(logLine(level, action, fields));
  return { info: at("info"), warn: at("warn"), error: at("error") };
}
