#!/usr/bin/env node
// Checks the name sites directory (sites/<name>.json, sites/README.md): `<name>.blockula.app` for
// the owner of `<name>.boo` on the demo chain. The rules are web/lib/sites.ts's.
//
//   node scripts/check-sites.mjs [dir]                   every listing's shape (no network)
//   node scripts/check-sites.mjs --live [name ...]       and, for these listings (default: all),
//                                                        what the demo chain and the target say
//
// --live reads the demo chain (pinned to its chain id and genesis hash) and each target:
//   - <name>.boo is active and owned by the listing's owner;
//   - the listing's record is a transfer the owner sent itself with the memo
//     site:v1:<name>:<target>, signed by one of its owner keys (the demo node keeps about two days
//     of history: a record older than that is gone, so the owner sends a new one);
//   - https://<target>/.well-known/boo has the owner's address on a line of its own.
// CI runs the shape check on every PR, and --live on the listings a PR adds or changes
// (ci.yml's `sites` job). web/lib/sites.test.mjs tests it.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const quiet = (w) => w.name === "Warning" && /MODULE_TYPELESS_PACKAGE_JSON/.test(`${w.code} ${w.message}`);
const warn = process.listeners("warning");
process.removeAllListeners("warning").on("warning", (w) => quiet(w) || warn.forEach((f) => f(w)));
const { checkSites, siteNameProblem, siteRecordProblem, wellKnownProblem, SITES_CHAIN, WELL_KNOWN_PATH } = await import("../web/lib/sites.ts");

const args = process.argv.slice(2);
const live = args[0] === "--live";
const dir = live ? join(fileURLToPath(new URL("..", import.meta.url)), "sites") : resolve(args[0] ?? join(fileURLToPath(new URL("..", import.meta.url)), "sites"));
const only = live ? args.slice(1).map((a) => a.replace(/^sites\//, "").replace(/\.json$/, "").replace(/\.boo$/, "")) : [];

/** Reads the directory and checks every listing's shape. */
export function readSites(dir) {
  const errors = [];
  const files = [];
  for (const name of readdirSync(dir)) {
    if (statSync(join(dir, name)).isDirectory()) errors.push(`sites/${name}: no directories here, only <name>.json files and README.md`);
    else files.push({ name, text: readFileSync(join(dir, name), "utf8") });
  }
  const out = checkSites(files);
  return { sites: out.sites, errors: [...errors, ...out.errors] };
}

async function getJson(url) {
  const res = await fetch(url, { headers: { accept: "application/json" }, signal: AbortSignal.timeout(15_000) });
  const text = await res.text();
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    throw new Error(`${url} answered ${res.status} without JSON`);
  }
  return { status: res.status, body };
}

/** Fails unless the demo node is on the pinned chain. */
export async function pinnedChain(rpc = SITES_CHAIN.rpc) {
  const { body } = await getJson(`${rpc}/api/status`);
  if (body?.chain_id !== SITES_CHAIN.chainId || body?.genesis_hash !== SITES_CHAIN.genesisHash) {
    throw new Error(`${rpc} reports chain ${body?.chain_id} (genesis ${body?.genesis_hash}), not ${SITES_CHAIN.chainId} (genesis ${SITES_CHAIN.genesisHash})`);
  }
  return body;
}

/** What the chain says about a site's name now: a problem, or null. Throws when the node doesn't answer. */
export async function nameProblem(site, rpc = SITES_CHAIN.rpc) {
  const { status, body } = await getJson(`${rpc}/api/name/${encodeURIComponent(site.name)}`);
  if (status !== 200) throw new Error(`${rpc} answered ${status} for ${site.name}`);
  return siteNameProblem(site, body);
}

async function liveProblems(site) {
  const problems = [];
  const name = await nameProblem(site);
  if (name) problems.push(name);
  const acct = await getJson(`${SITES_CHAIN.rpc}/api/account/${site.owner}`);
  const owners = (Array.isArray(acct.body?.keys) ? acct.body.keys : []).filter((k) => k?.role?.type === "owner").map((k) => k.key_id);
  const tx = await getJson(`${SITES_CHAIN.rpc}/api/tx/${site.record}`);
  if (tx.status === 404) problems.push(`the record ${site.record} isn't on ${SITES_CHAIN.rpc} (never sent, or older than the node's history: send a new one and list its id)`);
  else {
    const why = siteRecordProblem(site, tx.body, owners);
    if (why) problems.push(why);
  }
  const url = `https://${site.target}${WELL_KNOWN_PATH}`;
  try {
    const res = await fetch(url, { redirect: "follow", signal: AbortSignal.timeout(15_000) });
    const final = new URL(res.url || url);
    if (!res.ok) problems.push(`${url} answered ${res.status}`);
    else if (final.hostname !== site.target) problems.push(`${url} redirected to ${final.hostname}: serve the file at the target itself`);
    else {
      const why = wellKnownProblem(site.owner, (await res.text()).slice(0, 4096));
      if (why) problems.push(`${url} ${why}`);
    }
  } catch (e) {
    problems.push(`${url} didn't answer (${e instanceof Error ? e.message : e})`);
  }
  return problems;
}

const isMain = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  let read;
  try {
    read = readSites(dir);
  } catch (e) {
    console.error(`✗ can't read ${dir}: ${e.message}`);
    process.exit(1);
  }
  const errors = [...read.errors];
  let checked = read.sites;
  if (live) {
    for (const l of only) if (!read.sites.some((s) => s.label === l) && !errors.some((e) => e.startsWith(`sites/${l}.json`))) errors.push(`sites/${l}.json: no such listing`);
    checked = only.length ? read.sites.filter((s) => only.includes(s.label)) : read.sites;
    if (checked.length) {
      try {
        await pinnedChain();
      } catch (e) {
        errors.push(String(e.message));
        checked = [];
      }
    }
    for (const site of checked) {
      try {
        for (const p of await liveProblems(site)) errors.push(`sites/${site.label}.json: ${p}`);
      } catch (e) {
        errors.push(`sites/${site.label}.json: couldn't check (${e instanceof Error ? e.message : e})`);
      }
    }
  }
  if (errors.length) {
    for (const e of errors) console.error(`✗ ${e}`);
    console.error(`\n${errors.length} problem${errors.length === 1 ? "" : "s"} with name sites: sites/README.md says what a listing needs.`);
    process.exit(1);
  }
  const n = read.sites.length;
  console.log(`✓ name sites: ${n} listed${n ? ` (${read.sites.map((s) => `${s.label}.blockula.app`).join(", ")})` : ""}${live ? `; ${checked.length} checked against the chain and ${checked.length === 1 ? "its target" : "their targets"}` : ""}`);
}
