#!/usr/bin/env node
// Writes mcp/npm-shrinkwrap.json from the repo's package-lock.json, so `npx -y @blockula/mcp`
// installs exactly the dependency tree CI built and tested, transitive dependencies included,
// instead of whatever ranges resolve to that day (audit F75: the MCP holds the agent's hot key).
// npm publishes a package's npm-shrinkwrap.json with it and honours it when installing it.
//
//   node scripts/mcp-shrinkwrap.mjs [--sdk-integrity sha512-…]   write it (npm-publish.yml runs this)
//   node scripts/mcp-shrinkwrap.mjs --check                      also fail unless every entry is pinned by integrity
//   node scripts/mcp-shrinkwrap.mjs --dry-run                    check the pins and the tree, write nothing (npm test -w @blockula/mcp)
//
// The MCP's own dependencies must be exact versions. @blockula/sdk is a workspace link here; in
// the shrinkwrap it's the registry tarball of the same version, with that tarball's integrity:
// npm-publish.yml packs the SDK first and passes `npm pack --json`'s integrity, which is what the
// registry reports once that tarball is published (`npm view @blockula/sdk@<version>
// dist.integrity`; the publish job checks they match before publishing the MCP).
import { readFileSync, writeFileSync } from "node:fs";

const root = new URL("..", import.meta.url);
const read = (p) => JSON.parse(readFileSync(new URL(p, root), "utf8"));
const lock = read("package-lock.json");
const mcp = read("mcp/package.json");
const sdk = read("sdk-ts/package.json");
const args = process.argv.slice(2);
const at = args.indexOf("--sdk-integrity");
const sdkIntegrity = at >= 0 ? args[at + 1] : undefined;
const check = args.includes("--check");
const dryRun = args.includes("--dry-run");
const fail = (msg) => {
  console.error(`mcp-shrinkwrap: ${msg}`);
  process.exit(1);
};

if (lock.lockfileVersion !== 3) fail(`package-lock.json is lockfileVersion ${lock.lockfileVersion}; expected 3`);
for (const [name, range] of Object.entries(mcp.dependencies ?? {})) {
  if (!/^\d+\.\d+\.\d+$/.test(range)) fail(`mcp/package.json must pin ${name} to an exact version, not "${range}"`);
}
if (mcp.dependencies["@blockula/sdk"] !== sdk.version) fail(`mcp depends on @blockula/sdk ${mcp.dependencies["@blockula/sdk"]}, but sdk-ts is ${sdk.version}: publish them in lockstep`);
if (sdkIntegrity !== undefined && !/^sha512-[A-Za-z0-9+/]+=*$/.test(sdkIntegrity)) fail(`--sdk-integrity must be an sha512 SRI string, got "${sdkIntegrity}"`);

const packages = lock.packages;
/** Node's resolution of `name` from the package at `from` ("mcp", "node_modules/a", …), as npm laid out the repo. */
function resolve(from, name) {
  for (let p = from; ; ) {
    const candidate = `${p ? `${p}/` : ""}node_modules/${name}`;
    if (packages[candidate]) return candidate;
    if (!p) return null;
    const i = p.lastIndexOf("/node_modules/");
    p = i >= 0 ? p.slice(0, i) : "";
  }
}

// Everything the MCP needs at runtime, by its path in the repo's tree.
const closure = new Map();
const todo = Object.keys(mcp.dependencies).map((name) => ["mcp", name, false]);
while (todo.length) {
  const [from, name, optional] = todo.pop();
  const path = resolve(from, name);
  if (!path) {
    if (optional) continue;
    fail(`${name} (needed by ${from}) isn't in package-lock.json; run npm install`);
  }
  if (closure.has(path)) continue;
  // A copy nested under a workspace (mcp/node_modules/…, sdk-ts/node_modules/…) would need its
  // path rewritten for the published package; npm hoists today, so refuse rather than guess.
  if (!path.startsWith("node_modules/")) fail(`${path} (needed by ${from}) is nested under a workspace; hoist it (npm dedupe) or teach this script to relocate it`);
  const entry = packages[path];
  closure.set(path, entry);
  const own = entry.link ? entry.resolved : path;
  const def = entry.link ? packages[entry.resolved] : entry;
  for (const d of Object.keys(def.dependencies ?? {})) todo.push([own, d, false]);
  for (const d of Object.keys(def.optionalDependencies ?? {})) todo.push([own, d, true]);
}

const out = {
  name: mcp.name,
  version: mcp.version,
  lockfileVersion: 3,
  requires: true,
  packages: {
    "": { name: mcp.name, version: mcp.version, license: mcp.license, dependencies: mcp.dependencies, bin: mcp.bin, engines: mcp.engines },
  },
};
const unpinned = [];
for (const path of [...closure.keys()].sort()) {
  const entry = closure.get(path);
  if (entry.link) {
    if (path !== "node_modules/@blockula/sdk") fail(`unexpected workspace link ${path}`);
    out.packages[path] = {
      version: sdk.version,
      resolved: `https://registry.npmjs.org/@blockula/sdk/-/sdk-${sdk.version}.tgz`,
      ...(sdkIntegrity ? { integrity: sdkIntegrity } : {}),
      license: sdk.license,
      dependencies: sdk.dependencies,
      engines: sdk.engines,
    };
    if (!sdkIntegrity) unpinned.push(path);
    continue;
  }
  const { dev: _dev, devOptional: _devOptional, peer: _peer, extraneous: _extraneous, ...kept } = entry;
  if (!kept.version || !kept.resolved?.startsWith("https://registry.npmjs.org/") || !kept.integrity) unpinned.push(path);
  out.packages[path] = kept;
}

if (!dryRun) writeFileSync(new URL("mcp/npm-shrinkwrap.json", root), JSON.stringify(out, null, 2) + "\n");
console.log(`${dryRun ? "would write" : "wrote"} mcp/npm-shrinkwrap.json: ${closure.size} packages${unpinned.length ? ` (${unpinned.length} without a registry integrity: ${unpinned.join(", ")})` : ", each pinned by integrity"}`);
if (check && unpinned.length) fail(`not pinned by integrity: ${unpinned.join(", ")}`);
