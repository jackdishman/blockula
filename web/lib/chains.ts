/**
 * The chain directory (`chains/<chain id>.json`, chains/README.md): the chains blockula.app lists,
 * and the rules a listing must meet. `scripts/check-chains.mjs` runs these on every PR, and the
 * project site's build (next.config.ts) refuses to build with a listing that breaks them. No
 * imports, so plain Node runs it (scripts/check-chains.mjs, lib/chains.test.mjs).
 */

/** One listed chain, as its `chains/<chain id>.json` says. */
export interface ListedChain {
  /** What wallets call it, as its genesis does (1–48 bytes). */
  name: string;
  /** Its chain id, and the file's name. */
  chain_id: string;
  /** Its ticker: 2–8 of A–Z and 0–9, starting with a letter. */
  token: string;
  /** A sentence about it. */
  description: string;
  /** A node's public https origin, which serves the explorer at `/` and the wallet at `/wallet/`. */
  rpc: string;
  /** The genesis hash everyone who joins pins (64 lowercase hex). */
  genesis_hash: string;
  /** Whether it takes validators from outside. Needs `p2p`. */
  validators_wanted: boolean;
  /** `host:port` its validators dial, reachable from the internet. Required with `validators_wanted`. */
  p2p?: string;
  /** Who to ask: a GitHub handle, or an https URL. */
  contact: string;
  /** When it was listed (YYYY-MM-DD). */
  listed_at?: string;
}

export const REQUIRED_FIELDS = ["name", "chain_id", "token", "description", "rpc", "genesis_hash", "validators_wanted", "contact"] as const;
export const OPTIONAL_FIELDS = ["p2p", "listed_at"] as const;

export const MAX_NAME_BYTES = 48;
export const MAX_DESCRIPTION = 160;
export const MAX_URL = 100;

/** The repo whose `chains/` the site lists. */
export const REGISTRY_REPO = "https://github.com/jackdishman/blockula";

// Characters that make one text look like another: controls, bidi overrides, zero-width marks.
// eslint-disable-next-line no-control-regex
const SNEAKY = /[\u0000-\u001f\u007f-\u009f​-‏‪-‮⁠-⁩﻿]/;

/** Text without its look-alikes, lowercased: what "is this Blockula?" is asked of. */
function folded(s: string): string {
  return s
    .toLowerCase()
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .replace(/0/g, "o")
    .replace(/[1|!]/g, "l")
    .replace(/3/g, "e")
    .replace(/4|@/g, "a")
    .replace(/[^a-z]/g, "");
}

/** Whether `s` reads as Blockula itself, or as official: the project lists chains, it doesn't run them. */
export function soundsOfficial(s: string): boolean {
  return /bl+o+c*k+u+l+a/.test(folded(s)) || /\bofficial\b/i.test(s);
}

const PRIVATE_V4 = (ip: string): boolean => {
  const o = ip.split(".").map(Number);
  if (o.length !== 4 || o.some((n) => !Number.isInteger(n) || n < 0 || n > 255)) return true;
  const [a, b] = o;
  return (
    a === 0 || a === 10 || a === 127 || a >= 224 ||
    (a === 100 && b >= 64 && b < 128) ||
    (a === 169 && b === 254) ||
    (a === 172 && b >= 16 && b < 32) ||
    (a === 192 && b === 168) ||
    (a === 192 && b === 0) ||
    (a === 198 && (b === 18 || b === 19))
  );
};

const LOCAL_SUFFIX = /(^|\.)(localhost|local|lan|internal|home\.arpa|intranet|corp|private|test|example|invalid|onion)$/;
const DNS_NAME = /^(?=.{1,253}$)([a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z](?:[a-z0-9-]{0,61}[a-z0-9])?$/;

/**
 * Why `host` isn't a public DNS name the whole internet resolves, or null. IP literals are refused
 * too: a passkey needs a hostname, and an https certificate for an IP is rare.
 */
export function publicNameProblem(host: string): string | null {
  const h = host.toLowerCase();
  if (h.startsWith("[") || h.includes(":")) return "is an IPv6 address: use a DNS name";
  if (/^\d{1,3}(\.\d{1,3}){3}$/.test(h)) return "is an IP address: use a DNS name (passkeys need one)";
  if (!DNS_NAME.test(h)) return "isn't a DNS name like chain.example.org";
  if (LOCAL_SUFFIX.test(h)) return "is a local or reserved name nobody else can reach";
  return null;
}

/** Why `host` (a p2p host: a DNS name or an IPv4 address) isn't reachable from the internet, or null. */
function p2pHostProblem(host: string): string | null {
  const h = host.toLowerCase();
  if (/^\d{1,3}(\.\d{1,3}){3}$/.test(h)) return PRIVATE_V4(h) ? "is a private, local or reserved address" : null;
  const named = publicNameProblem(h);
  if (named) return named;
  if (h.endsWith(".ts.net")) return "is on a tailnet: Tailscale Funnel carries only HTTPS, so other validators can't dial it";
  return null;
}

const GITHUB_HANDLE = /^[A-Za-z0-9](?:[A-Za-z0-9]|-(?=[A-Za-z0-9])){0,38}$/;

function isDate(s: string): boolean {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(s)) return false;
  const d = new Date(`${s}T00:00:00Z`);
  return !Number.isNaN(d.getTime()) && d.toISOString().slice(0, 10) === s && d.getUTCFullYear() >= 2026;
}

/** A chain id as `blockula launch` makes them (node/src/launch_cli.rs `check_chain_id`). */
export const CHAIN_ID = /^[a-z0-9]+(-[a-z0-9]+)*$/;

function text(errors: string[], field: string, v: unknown, max: number, bytes = false): v is string {
  if (typeof v !== "string") return void errors.push(`${field} must be a string`), false;
  const size = bytes ? new TextEncoder().encode(v).length : [...v].length;
  if (!v.trim()) errors.push(`${field} is empty`);
  else if (size > max) errors.push(`${field} is ${size} ${bytes ? "bytes" : "characters"}, more than ${max}`);
  if (v.trim() !== v) errors.push(`${field} has space around it`);
  if (SNEAKY.test(v)) errors.push(`${field} has control, bidi or zero-width characters`);
  if (/[<>]/.test(v)) errors.push(`${field} has < or >`);
  return true;
}

/**
 * What's wrong with one listing, `file` being its file name (`<chain id>.json`). Empty: it's
 * accepted.
 */
export function checkChain(file: string, raw: unknown): string[] {
  const errors: string[] = [];
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return ["isn't a JSON object"];
  const c = raw as Record<string, unknown>;
  const known = new Set<string>([...REQUIRED_FIELDS, ...OPTIONAL_FIELDS]);
  for (const k of Object.keys(c)) if (!known.has(k)) errors.push(`has a field this directory doesn't take: ${k}`);
  for (const k of REQUIRED_FIELDS) if (!(k in c)) errors.push(`is missing ${k}`);

  if (typeof c.chain_id !== "string" || c.chain_id.length < 2 || c.chain_id.length > 40 || !CHAIN_ID.test(c.chain_id)) errors.push("chain_id must be 2–40 of a–z, 0–9 and single dashes (like jack-in-the-block)");
  else {
    if (file !== `${c.chain_id}.json`) errors.push(`the file must be named ${c.chain_id}.json, after its chain_id`);
    if (soundsOfficial(c.chain_id)) errors.push("chain_id reads as Blockula's own: pick one of your chain's");
  }
  if (text(errors, "name", c.name, MAX_NAME_BYTES, true) && soundsOfficial(c.name)) errors.push("name reads as Blockula itself, or as official: the directory lists chains people run, not the project's");
  if (typeof c.token !== "string" || !/^[A-Z][A-Z0-9]{1,7}$/.test(c.token)) errors.push("token must be 2–8 of A–Z and 0–9, starting with a letter (like JACK)");
  if (text(errors, "description", c.description, MAX_DESCRIPTION) && soundsOfficial(c.description)) errors.push("description reads as Blockula itself, or as official");
  if (typeof c.genesis_hash !== "string" || !/^[0-9a-f]{64}$/.test(c.genesis_hash)) errors.push("genesis_hash must be 64 lowercase hex characters (`/api/status`'s genesis_hash)");
  if (typeof c.validators_wanted !== "boolean") errors.push("validators_wanted must be true or false");

  if (typeof c.rpc !== "string" || c.rpc.length > MAX_URL) errors.push(`rpc must be an https URL of at most ${MAX_URL} characters`);
  else {
    let u: URL | null = null;
    try {
      u = new URL(c.rpc);
    } catch {
      errors.push("rpc isn't a URL");
    }
    if (u) {
      if (u.protocol !== "https:") errors.push("rpc must be https:// (wallets need it for passkeys)");
      else if (u.username || u.password) errors.push("rpc has credentials in it");
      else if (`${u.origin}` !== c.rpc) errors.push(`rpc must be the node's origin alone, with no path, query or trailing slash (like ${u.origin})`);
      const why = publicNameProblem(u.hostname);
      if (why) errors.push(`rpc's host ${u.hostname} ${why}`);
      else if (soundsOfficial(u.hostname.replace(/\.ts\.net$/, ""))) errors.push("rpc's host reads as Blockula's own");
    }
  }

  if (c.p2p !== undefined) {
    const m = typeof c.p2p === "string" ? /^([^:\s]+):(\d{1,5})$/.exec(c.p2p) : null;
    if (!m || Number(m[2]) < 1 || Number(m[2]) > 65535) errors.push("p2p must be host:port (like chain.example.org:30300)");
    else {
      const why = p2pHostProblem(m[1]);
      if (why) errors.push(`p2p's host ${m[1]} ${why}`);
    }
  }
  if (c.validators_wanted === true && c.p2p === undefined) errors.push("validators_wanted needs p2p: the host:port other validators dial");

  if (typeof c.contact !== "string" || c.contact.length > MAX_URL) errors.push("contact must be a GitHub handle or an https URL");
  else if (!GITHUB_HANDLE.test(c.contact)) {
    let ok = false;
    try {
      const u = new URL(c.contact);
      ok = u.protocol === "https:" && !u.username && !u.password && !publicNameProblem(u.hostname) && !SNEAKY.test(c.contact);
    } catch {}
    if (!ok) errors.push("contact must be a GitHub handle (like jackdishman) or an https URL");
  }
  if (c.listed_at !== undefined && (typeof c.listed_at !== "string" || !isDate(c.listed_at))) errors.push("listed_at must be a date, YYYY-MM-DD");
  return errors;
}

/** The directory's files: `name` (a file name in `chains/`) and its text. */
export interface ChainFile {
  name: string;
  text: string;
}

/**
 * Every listing in `chains/`, checked one by one and against each other (no two share a chain id,
 * genesis, node, name or p2p address). Files other than `*.json` and `README.md` are refused.
 * The chains come back sorted by name.
 */
export function checkChains(files: ChainFile[]): { chains: ListedChain[]; errors: string[] } {
  const errors: string[] = [];
  const chains: ListedChain[] = [];
  for (const f of [...files].sort((a, b) => a.name.localeCompare(b.name))) {
    if (f.name === "README.md") continue;
    if (!f.name.endsWith(".json")) {
      errors.push(`chains/${f.name}: only <chain id>.json files and README.md belong here`);
      continue;
    }
    let raw: unknown;
    try {
      raw = JSON.parse(f.text);
    } catch (e) {
      errors.push(`chains/${f.name}: isn't valid JSON (${(e as Error).message})`);
      continue;
    }
    const why = checkChain(f.name, raw);
    if (why.length) errors.push(...why.map((w) => `chains/${f.name}: ${w}`));
    else chains.push(raw as ListedChain);
  }
  const seen = new Map<string, string>();
  for (const c of chains) {
    const keys: [string, string][] = [
      ["genesis_hash", c.genesis_hash],
      ["rpc", c.rpc.toLowerCase()],
      ["name", c.name.toLowerCase()],
      ...(c.p2p ? ([["p2p", c.p2p.toLowerCase()]] as [string, string][]) : []),
    ];
    for (const [k, v] of keys) {
      const other = seen.get(`${k}:${v}`);
      if (other) errors.push(`chains/${c.chain_id}.json: has the same ${k} as chains/${other}.json`);
      else seen.set(`${k}:${v}`, c.chain_id);
    }
  }
  return { chains: chains.sort((a, b) => a.name.localeCompare(b.name)), errors };
}

/** Where a contact goes: a GitHub handle's profile, or the URL itself. */
export const contactUrl = (contact: string) => (GITHUB_HANDLE.test(contact) ? `https://github.com/${contact}` : contact);

/** A listing as the JSON its file holds: the fields in the README's order, two-space indent. */
export function listingJson(c: Partial<ListedChain>): string {
  const ordered: Record<string, unknown> = {};
  for (const k of [...REQUIRED_FIELDS.slice(0, 7), "p2p", "contact", "listed_at"] as const) if (c[k] !== undefined) ordered[k] = c[k];
  return `${JSON.stringify(ordered, null, 2)}\n`;
}

/** GitHub's new-file page for `chains/<chain id>.json`, filled in with `json`: one click to a PR. */
export const listingLink = (chainId: string, json: string) => `${REGISTRY_REPO}/new/main?filename=${encodeURIComponent(`chains/${chainId}.json`)}&value=${encodeURIComponent(json)}`;

/** `drops` (a decimal string, a billion to one unit) as units: "50", "0.5", "10,000". */
export function units(drops: string): string {
  if (!/^\d+$/.test(drops)) return drops;
  const d = drops.padStart(10, "0");
  const whole = d.slice(0, -9).replace(/^0+(?=\d)/, "");
  const frac = d.slice(-9).replace(/0+$/, "");
  return `${whole.replace(/\B(?=(\d{3})+(?!\d))/g, ",")}${frac ? `.${frac}` : ""}`;
}
