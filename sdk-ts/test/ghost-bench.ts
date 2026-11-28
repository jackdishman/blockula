// The WebAssembly Ghost Mode prover in Node: prove and verify times over full Actions (two real
// spends), and the bundle's size. `npm run bench:ghost -w @blockula/sdk` (GHOST_BENCH_PROOFS=12).
// test/ghost-bench-browser.ts runs the same measurement in headless Chromium.
import { readFileSync } from "node:fs";
import { gzipSync } from "node:zlib";
import * as glue from "../src/ghost-wasm/blockula_ghost_wasm.js";
import { loadGhostWasm } from "../src/ghost.js";
// @ts-expect-error a plain module, shared with the browser page
import { benchGhost } from "./ghost-bench-core.mjs";

const proofs = Number(process.env.GHOST_BENCH_PROOFS ?? 12);
await loadGhostWasm();
const size = (file: string) => {
  const bytes = readFileSync(new URL(`../src/ghost-wasm/${file}`, import.meta.url));
  return { raw: bytes.length, gzip: gzipSync(bytes, { level: 9 }).length };
};
console.log(JSON.stringify({ runtime: `node ${process.version} (${process.platform} ${process.arch})`, ...benchGhost(glue, proofs), wasm_bytes: size("blockula_ghost_wasm_bg.wasm"), glue_bytes: size("blockula_ghost_wasm.js") }, null, 2));
