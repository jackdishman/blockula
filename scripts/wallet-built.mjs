#!/usr/bin/env node
// The last step of `npm run build:wallet`: checks the export node/build.rs embeds (web/out) and
// touches node/build.rs, so the next cargo build embeds it even if the binary was built before
// any export existed (build.rs watches web/out only once it exists).
import { existsSync, readdirSync, statSync, utimesSync } from "node:fs";
import { join } from "node:path";

const root = new URL("..", import.meta.url).pathname;
const out = join(root, "web/out");
if (!existsSync(join(out, "index.html"))) {
  console.error("web/out/index.html is missing: the served build (BLOCKULA_SERVED=1) didn't export the wallet.");
  process.exit(1);
}
for (const route of ["tx", "account", "block", "name", "program", "programs", "hunt", "claim", "seance", "trace"]) {
  if (!existsSync(join(out, route, "_", "index.html"))) {
    console.error(`web/out/${route}/_/index.html is missing: every dynamic route needs its placeholder page (web/lib/route-params.ts).`);
    process.exit(1);
  }
}
let files = 0;
let bytes = 0;
const walk = (d) => {
  for (const e of readdirSync(d, { withFileTypes: true })) {
    const p = join(d, e.name);
    if (e.isDirectory()) walk(p);
    else (files++, (bytes += statSync(p).size));
  }
};
walk(out);
const now = new Date();
utimesSync(join(root, "node/build.rs"), now, now);
console.log(`Wallet exported to web/out (${files} files, ${(bytes / 1048576).toFixed(1)} MiB). The next cargo build of node/ serves it at /wallet/.`);
