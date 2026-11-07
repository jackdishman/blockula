#!/usr/bin/env node
// Checks the chain directory blockula.app lists (chains/<chain id>.json, chains/README.md): every
// listing's shape, https and public hosts, no look-alikes of Blockula itself, no two listings of one
// chain. The rules are web/lib/chains.ts's, which the project site's build applies too.
//
//   node scripts/check-chains.mjs [dir]     (default: chains/ in this repo)
//
// CI runs it on every PR ("Packages + wallet build"); web/lib/chains.test.mjs tests it.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// web/ has no "type" in its package.json, so Node says it reparses chains.ts as a module: it's meant to.
const quiet = (w) => w.name === "Warning" && /MODULE_TYPELESS_PACKAGE_JSON/.test(`${w.code} ${w.message}`);
const warn = process.listeners("warning");
process.removeAllListeners("warning").on("warning", (w) => quiet(w) || warn.forEach((f) => f(w)));
const { checkChains } = await import("../web/lib/chains.ts");

const dir = resolve(process.argv[2] ?? join(fileURLToPath(new URL("..", import.meta.url)), "chains"));
let names;
try {
  names = readdirSync(dir);
} catch (e) {
  console.error(`✗ can't read ${dir}: ${e.message}`);
  process.exit(1);
}
const files = [];
const errors = [];
for (const name of names) {
  if (statSync(join(dir, name)).isDirectory()) {
    errors.push(`chains/${name}: no directories here, only <chain id>.json files and README.md`);
    continue;
  }
  files.push({ name, text: readFileSync(join(dir, name), "utf8") });
}
const out = checkChains(files);
errors.push(...out.errors);
if (errors.length) {
  for (const e of errors) console.error(`✗ ${e}`);
  console.error(`\n${errors.length} problem${errors.length === 1 ? "" : "s"} in the chain directory: chains/README.md says what a listing needs.`);
  process.exit(1);
}
console.log(`✓ chain directory: ${out.chains.length} chain${out.chains.length === 1 ? "" : "s"} listed${out.chains.length ? ` (${out.chains.map((c) => c.chain_id).join(", ")})` : ""}`);
