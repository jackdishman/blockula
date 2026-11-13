/**
 * Name sites (sites/README.md): `<name>.blockula.app` is a site of the boo who owns `<name>.boo` on
 * Blockula's demo chain, pointed by a CNAME at a host of their choosing.
 *
 * A site is listed by pull request (`sites/<name>.json`), so a maintainer reads every one before
 * blockula.app's DNS serves it, and the listing is proven three ways:
 *
 * 1. **The name.** `<name>.boo` is active and owned by the listing's `owner` (`/api/name`). This is
 *    state, so it's checked again on every DNS sync: a name that lapses or changes hands loses its
 *    site until a new listing says otherwise.
 * 2. **The record.** The owner sent itself 0 with the memo `site:v1:<name>:<target>`, signed by one
 *    of its owner keys: the account saying "point my name's site there". The listing names that
 *    transaction. Validators prune history after about two days, so the record is checked when the
 *    listing is added or changed, not on every sync.
 * 3. **The target.** `https://<target>/.well-known/boo` has the owner's address on a line of its
 *    own: the host saying "I serve this boo's site", so nobody's host is pointed at by a listing
 *    its operator didn't ask for.
 *
 * Everything a listing, a memo or a target says is a stranger's text: the checks here accept only
 * the exact shapes above, and nothing in them is followed as an instruction.
 */
import { PROJECT_DOMAIN, publicNameProblem, soundsOfficial } from "./chains.ts";

/** The chain whose `.boo` names hold `*.blockula.app`: the demo chain (chains/blockula-demo-2.json), pinned. */
export const SITES_CHAIN = {
  chainId: "blockula-demo-2",
  genesisHash: "e712d9376917f0c9d10c164fa7c1a53fef7c8dc1c24100e7924037d0250f705e",
  rpc: "https://demo.blockula.app",
} as const;

/** What the DNS sync writes in the comment of each record it manages. It never touches a record without it. */
export const SITE_RECORD_COMMENT = "blockula-sites (sites/README.md)";

/** Where a target says which account's site it serves. */
export const WELL_KNOWN_PATH = "/.well-known/boo";

/**
 * Subdomains the project keeps: its own hosts, and names a visitor would take for the project's
 * (a wallet, an API, mail). A name that reads as Blockula's or "official" is refused too.
 */
export const RESERVED_LABELS: ReadonlySet<string> = new Set([
  "about", "account", "accounts", "admin", "api", "app", "apps", "assets", "auth", "beta", "blog", "bot", "bots",
  "cdn", "chain", "chains", "dashboard", "demo", "dev", "dns", "docs", "download", "downloads", "explore", "explorer",
  "faucet", "ftp", "gov", "help", "home", "house", "id", "imap", "join", "launch", "login", "mail", "mainnet",
  "metrics", "mx", "node", "nodes", "ns", "ns1", "ns2", "pay", "play", "playground", "pop", "preview", "relay",
  "rpc", "security", "seed", "signin", "signup", "site", "sites", "smtp", "staging", "static", "status", "support",
  "sync", "test", "testnet", "validator", "validators", "wallet", "wallets", "webmail", "www", "x",
]);

/** The fields a listing takes. */
export const SITE_FIELDS = ["name", "owner", "target", "record", "contact", "listed_at"] as const;
const REQUIRED = ["name", "owner", "target", "record", "contact"] as const;

/** Longest target host: the memo (`site:v1:` + name + `:` + target) fits the chain's 256-byte memo. */
export const MAX_TARGET = 200;

const ADDRESS = /^boo1[02-9ac-hj-np-z]{20,120}$/;
const TX_ID = /^[0-9a-f]{64}$/;
const GITHUB_HANDLE = /^[A-Za-z0-9](?:[A-Za-z0-9]|-(?=[A-Za-z0-9])){0,38}$/;
const SITE_MEMO = /^site:v1:([a-z0-9-]{3,32}):([a-z0-9.-]{1,200})$/;

/** A listing, as `checkSite` accepted it. */
export interface SiteListing {
  /** The subdomain: `<label>.blockula.app`, for `<label>.boo`. */
  label: string;
  name: string;
  owner: string;
  target: string;
  record: string;
  contact: string;
  listed_at?: string;
}

/** Why `label` can't be a site's subdomain, or null. The rules of `.boo` names (sdk `bareName`), less the project's own. */
export function siteLabelProblem(label: string): string | null {
  if (label.length < 3 || label.length > 32) return "is 3–32 characters, like the .boo name";
  if (!/^[a-z0-9-]+$/.test(label)) return "may only use a–z, 0–9 and '-'";
  if (label.startsWith("-") || label.endsWith("-")) return "can't start or end with '-'";
  if (label.slice(2, 4) === "--") return "can't have '--' in its 3rd and 4th characters (that's how DNS spells other alphabets, xn--)";
  if (RESERVED_LABELS.has(label)) return "is one the project keeps for itself";
  if (soundsOfficial(label)) return "reads as Blockula's own, or as official";
  return null;
}

/** Why `target` can't be pointed at, or null: a public DNS name of someone's host, lowercase, outside the project's domain. */
export function siteTargetProblem(target: string): string | null {
  if (target.length > MAX_TARGET) return `is more than ${MAX_TARGET} characters`;
  if (target !== target.toLowerCase()) return "must be lowercase";
  if (target.endsWith(".")) return "takes no trailing dot";
  const why = publicNameProblem(target);
  if (why) return why;
  if (!target.includes(".")) return "isn't a full host name, like you.vercel.app";
  if (target === PROJECT_DOMAIN || target.endsWith(`.${PROJECT_DOMAIN}`)) return `is on ${PROJECT_DOMAIN} itself: point at the host that serves your site`;
  return null;
}

/** The memo of a site record. Throws on a label or target the directory wouldn't take. */
export function siteMemo(label: string, target: string): string {
  const why = siteLabelProblem(label) ?? (siteTargetProblem(target) && `the target ${siteTargetProblem(target)}`);
  if (why) throw new Error(`${label}: ${why}`);
  return `site:v1:${label}:${target}`;
}

/** What a memo records, if it's exactly a site record; `null` for any other text. */
export function parseSiteMemo(memo: unknown): { label: string; target: string } | null {
  if (typeof memo !== "string") return null;
  const m = memo.match(SITE_MEMO);
  return m ? { label: m[1], target: m[2] } : null;
}

function isDate(s: string): boolean {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(s)) return false;
  const d = new Date(`${s}T00:00:00Z`);
  return !Number.isNaN(d.getTime()) && d.toISOString().slice(0, 10) === s && d.getUTCFullYear() >= 2026;
}

/** What's wrong with one listing, `file` being its file name (`<name>.json`). Empty: its shape is accepted. Reads nothing from the network. */
export function checkSite(file: string, raw: unknown): string[] {
  const errors: string[] = [];
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return ["isn't a JSON object"];
  const s = raw as Record<string, unknown>;
  const known = new Set<string>(SITE_FIELDS);
  for (const k of Object.keys(s)) if (!known.has(k)) errors.push(`has a field this directory doesn't take: ${k}`);
  for (const k of REQUIRED) if (!(k in s)) errors.push(`is missing ${k}`);

  const label = file.replace(/\.json$/, "");
  if (!file.endsWith(".json")) errors.push("must be a .json file named after the name, like blood.json");
  else {
    const why = siteLabelProblem(label);
    if (why) errors.push(`${label}.${PROJECT_DOMAIN} can't be a site: the name ${why}`);
  }
  if (typeof s.name !== "string" || s.name !== `${label}.boo`) errors.push(`name must be ${label}.boo, the name the file is named after`);
  if (typeof s.owner !== "string" || !ADDRESS.test(s.owner)) errors.push("owner must be the name owner's boo1… address (never a name)");
  if (typeof s.target !== "string") errors.push("target must be a host name, like you.vercel.app");
  else {
    const why = siteTargetProblem(s.target);
    if (why) errors.push(`target ${why}`);
  }
  if (typeof s.record !== "string" || !TX_ID.test(s.record)) errors.push("record must be the site record's transaction id (64 lowercase hex)");
  if (typeof s.contact !== "string" || s.contact.length > 200) errors.push("contact must be a GitHub handle or an https URL");
  else if (!GITHUB_HANDLE.test(s.contact)) {
    let ok = false;
    try {
      const u = new URL(s.contact);
      ok = u.protocol === "https:" && !u.username && !u.password && !/[<>\s]/.test(s.contact);
    } catch {
      /* not a URL */
    }
    if (!ok) errors.push("contact must be a GitHub handle or an https URL");
  }
  if (s.listed_at !== undefined && (typeof s.listed_at !== "string" || !isDate(s.listed_at))) errors.push("listed_at must be a date, YYYY-MM-DD");
  return errors;
}

/** Every listing's shape, and the directory's: only `<name>.json` files and README.md. */
export function checkSites(files: readonly { name: string; text: string }[]): { sites: SiteListing[]; errors: string[] } {
  const sites: SiteListing[] = [];
  const errors: string[] = [];
  for (const f of [...files].sort((a, b) => a.name.localeCompare(b.name))) {
    if (f.name === "README.md") continue;
    let raw: unknown;
    try {
      raw = JSON.parse(f.text);
    } catch (e) {
      errors.push(`sites/${f.name}: isn't JSON (${e instanceof Error ? e.message : e})`);
      continue;
    }
    const errs = checkSite(f.name, raw);
    if (errs.length) errors.push(...errs.map((e) => `sites/${f.name}: ${e}`));
    else sites.push({ label: f.name.replace(/\.json$/, ""), ...(raw as Omit<SiteListing, "label">) });
  }
  return { sites, errors };
}

// ---- what the chain and the target say ----

/** `/api/name/{name}`'s answer, as far as a site reads it. */
export interface SiteNameAnswer {
  status?: unknown;
  owner?: { address?: unknown } | null;
}

/** Why the name doesn't hold this site now, or null: it's active and the listing's owner holds it. Re-checked on every sync. */
export function siteNameProblem(site: Pick<SiteListing, "name" | "owner">, name: SiteNameAnswer): string | null {
  if (name.status !== "active") return `${site.name} is ${typeof name.status === "string" ? name.status : "not active"}${name.status === "grace" ? " (expired: its owner can still renew it)" : ""}`;
  const owner = name.owner?.address;
  if (owner !== site.owner) return `${site.name} is owned by ${typeof owner === "string" ? owner : "nobody the node named"}, not the listing's owner ${site.owner}`;
  return null;
}

/** The parts of `/api/tx/{id}`'s answer a record is read from. */
export interface SiteRecordTx {
  id?: unknown;
  kind?: unknown;
  action?: { type?: unknown } | null;
  status?: unknown;
  error?: unknown;
  from?: { address?: unknown } | null;
  to?: { address?: unknown } | null;
  signer_key?: unknown;
  memo?: unknown;
}

/**
 * Why `tx` isn't this listing's site record, or null: a transfer that ran, from the owner to
 * itself, signed by one of `ownerKeys` (the account's owner keys now: a session key, an agent's
 * budget, doesn't speak for what the account's name points at), whose memo is exactly
 * `site:v1:<label>:<target>`.
 */
export function siteRecordProblem(site: Pick<SiteListing, "label" | "owner" | "target" | "record">, tx: SiteRecordTx, ownerKeys: Iterable<string>): string | null {
  const owners = new Set(ownerKeys);
  if (tx.id !== site.record) return `the node answered with another transaction (${String(tx.id)})`;
  if ((tx.kind ?? tx.action?.type) !== "transfer") return "the record isn't a transfer";
  if (tx.status !== "finalized" || tx.error) return `the record didn't run (${typeof tx.status === "string" ? tx.status : "no status"})`;
  if (tx.from?.address !== site.owner || tx.to?.address !== site.owner) return "the record isn't a transfer from the owner to itself";
  if (typeof tx.signer_key !== "string" || !owners.has(tx.signer_key)) return "the record wasn't signed by one of the owner's owner keys";
  const want = `site:v1:${site.label}:${site.target}`;
  if (tx.memo !== want) return `the record's memo isn't ${want}`;
  return null;
}

/** Why a target's `/.well-known/boo` doesn't say it serves `owner`'s site, or null: one of its lines is the address. */
export function wellKnownProblem(owner: string, body: string): string | null {
  const lines = body.split(/\r?\n/).map((l) => l.trim());
  return lines.includes(owner) ? null : `doesn't have ${owner} on a line of its own`;
}

// ---- DNS ----

/** A DNS record as Vercel lists it (`GET /v4/domains/{domain}/records`). */
export interface DnsRecord {
  id: string;
  name: string;
  type: string;
  value: string;
  comment?: string;
}

/** What the sync does to blockula.app's DNS. */
export type DnsChange =
  | { op: "create"; name: string; value: string }
  | { op: "update"; id: string; name: string; from: string; value: string }
  | { op: "delete"; id: string; name: string; value: string };

const host = (v: string) => v.toLowerCase().replace(/\.$/, "");

/**
 * The changes that make blockula.app's DNS serve `want` (label → target) and nothing else of the
 * sites': a CNAME per site. Only records whose comment is `SITE_RECORD_COMMENT` are the sync's: it
 * never creates a site over a record of any other kind or origin (that's a `conflict`), and never
 * deletes or updates one.
 */
export function planDns(want: ReadonlyMap<string, string>, records: readonly DnsRecord[]): { changes: DnsChange[]; conflicts: string[] } {
  const changes: DnsChange[] = [];
  const conflicts: string[] = [];
  const ours = records.filter((r) => r.comment === SITE_RECORD_COMMENT);
  for (const [label, target] of [...want].sort(([a], [b]) => a.localeCompare(b))) {
    const theirs = records.filter((r) => r.name === label && r.comment !== SITE_RECORD_COMMENT);
    if (theirs.length) {
      conflicts.push(`${label}: blockula.app already has ${theirs.map((r) => `${r.type} ${r.value}`).join(", ")} there, which the sync didn't make; left alone`);
      continue;
    }
    const mine = ours.filter((r) => r.name === label);
    const keep = mine.find((r) => r.type === "CNAME");
    if (!keep) changes.push({ op: "create", name: label, value: target });
    else if (host(keep.value) !== target) changes.push({ op: "update", id: keep.id, name: label, from: host(keep.value), value: target });
    for (const r of mine) if (r !== keep) changes.push({ op: "delete", id: r.id, name: r.name, value: r.value });
  }
  for (const r of ours) if (!want.has(r.name)) changes.push({ op: "delete", id: r.id, name: r.name, value: r.value });
  return { changes, conflicts };
}
