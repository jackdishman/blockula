#!/usr/bin/env node
// Makes blockula.app's DNS (on Vercel) serve the name sites listed in sites/: a CNAME
// `<name>.blockula.app → <target>` for each listing whose name is active and still owned by the
// listing's owner on the demo chain, and none for any other. sites/README.md has the rules.
//
//   VERCEL_TOKEN=… [VERCEL_TEAM_ID=…] node scripts/sync-sites-dns.mjs          prints the plan
//   VERCEL_TOKEN=… [VERCEL_TEAM_ID=…] node scripts/sync-sites-dns.mjs --apply  and makes it so
//
// What it may touch: only records whose comment is SITE_RECORD_COMMENT (web/lib/sites.ts), which
// it writes on every record it makes. A listing whose subdomain already has any other record (the
// demo chain's, a hand-made one) is reported and left alone, and nothing it didn't make is ever
// updated or deleted.
//
// When the demo node doesn't answer, or reports another chain, it changes nothing: a site comes
// down only when the chain says its name lapsed or changed hands, never because a node was away.
// .github/workflows/sites.yml runs it when sites/ changes on main and once a day.
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { nameProblem, pinnedChain, readSites } from "./check-sites.mjs";

const { planDns, SITE_RECORD_COMMENT } = await import("../web/lib/sites.ts");

const DOMAIN = "blockula.app";
const API = "https://api.vercel.com";
const apply = process.argv.includes("--apply");
const token = process.env.VERCEL_TOKEN;
const team = process.env.VERCEL_TEAM_ID;
const die = (msg) => {
  console.error(`✗ ${msg}`);
  process.exit(1);
};
if (!token) die("VERCEL_TOKEN is unset");

async function vercel(method, path, body) {
  const url = new URL(path, API);
  if (team) url.searchParams.set("teamId", team);
  const res = await fetch(url, {
    method,
    headers: { authorization: `Bearer ${token}`, ...(body ? { "content-type": "application/json" } : {}) },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(20_000),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`Vercel answered ${res.status} to ${method} ${url.pathname}: ${text.slice(0, 300)}`);
  return text ? JSON.parse(text) : null;
}

async function listRecords() {
  const out = [];
  let until;
  for (let page = 0; page < 50; page++) {
    const r = await vercel("GET", `/v4/domains/${DOMAIN}/records?limit=100${until ? `&until=${until}` : ""}`);
    out.push(...(r.records ?? []));
    until = r.pagination?.next;
    if (!until) return out;
  }
  throw new Error("more than 50 pages of DNS records: stopping");
}

const { sites, errors } = readSites(join(fileURLToPath(new URL("..", import.meta.url)), "sites"));
if (errors.length) die(`sites/ has problems, so nothing was changed:\n  ${errors.join("\n  ")}`);

try {
  await pinnedChain();
} catch (e) {
  die(`the demo chain can't be read, so nothing was changed: ${e.message}`);
}
const want = new Map();
for (const site of sites) {
  let why;
  try {
    why = await nameProblem(site);
  } catch (e) {
    die(`couldn't read ${site.name}, so nothing was changed: ${e.message}`);
  }
  if (why) console.log(`- ${site.label}.${DOMAIN}: not served: ${why}`);
  else want.set(site.label, site.target);
}

const records = await listRecords();
const { changes, conflicts } = planDns(want, records);
for (const c of conflicts) console.log(`! ${c}`);
if (!changes.length) {
  console.log(`✓ ${DOMAIN}: ${want.size} name site${want.size === 1 ? "" : "s"} served, nothing to change`);
  process.exit(0);
}
for (const c of changes) {
  const line =
    c.op === "create" ? `+ ${c.name}.${DOMAIN} CNAME ${c.value}` : c.op === "update" ? `~ ${c.name}.${DOMAIN} CNAME ${c.from} → ${c.value}` : `- ${c.name}.${DOMAIN} ${c.value} (removed)`;
  console.log(line);
  if (!apply) continue;
  if (c.op === "create") await vercel("POST", `/v2/domains/${DOMAIN}/records`, { name: c.name, type: "CNAME", value: c.value, ttl: 300, comment: SITE_RECORD_COMMENT });
  else if (c.op === "update") await vercel("PATCH", `/v1/domains/records/${c.id}`, { value: c.value, comment: SITE_RECORD_COMMENT });
  else await vercel("DELETE", `/v2/domains/${DOMAIN}/records/${c.id}`);
}
console.log(apply ? `✓ ${changes.length} change${changes.length === 1 ? "" : "s"} made` : `(a plan: --apply makes ${changes.length === 1 ? "it" : "them"})`);
