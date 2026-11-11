// Keep the explorer's QR encoder in its one CSP-hashed inline script. Reuse the wallet's
// dependency-free encoder, not a network script or another copy of the QR implementation.
import { build } from "esbuild";
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const file = new URL("../node/src/explorer.html", import.meta.url);
const start = "// BEGIN GENERATED INVITE QR (npm run build:explorer-qr)";
const end = "// END GENERATED INVITE QR";
const source = readFileSync(new URL("../web/lib/qr.ts", import.meta.url), "utf8");
// Preserve the encoder's complete MIT copyright and permission notice in the shipped bundle.
const notice = source.slice(0, source.indexOf("*/") + 2);
const result = await build({
  stdin: { contents: 'export { qrSvg } from "./web/lib/qr.ts";', resolveDir: root },
  bundle: true, minify: true, format: "iife", globalName: "ExplorerQr", target: "es2020",
  legalComments: "none", write: false,
});
const generated = `${start}\n${notice}\n${result.outputFiles[0].text.trim()}\n${end}`;
const html = readFileSync(file, "utf8");
const first = html.indexOf(start), last = html.indexOf(end, first);
if (first < 0 || last < first) throw new Error("Missing explorer QR bundle markers");
const next = html.slice(0, first) + generated + html.slice(last + end.length);
if (process.argv.includes("--check")) {
  if (next !== html) throw new Error("Explorer QR bundle is stale; run npm run build:explorer-qr");
} else {
  writeFileSync(file, next);
}
console.log(`Explorer QR: ${Buffer.byteLength(generated)} bytes, inline, no runtime dependencies`);
