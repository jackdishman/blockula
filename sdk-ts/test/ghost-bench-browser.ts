// The WebAssembly Ghost Mode prover in headless Chromium: the same measurement as
// test/ghost-bench.ts (full Actions with two real spends, each proof verified), in a page that
// loads the glue and the .wasm over HTTP from a server on a port the OS picks, on 127.0.0.1.
// `npx tsx test/ghost-bench-browser.ts` (GHOST_BENCH_PROOFS=12). It uses the Playwright the web
// wallet's tests install (`npx playwright install chromium` in web/ once); the browser and the
// server are closed when it's done.
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { homedir } from "node:os";
import { join } from "node:path";
import { chromium } from "playwright";

const proofs = Number(process.env.GHOST_BENCH_PROOFS ?? 12);
const file = (path: string) => readFileSync(new URL(path, import.meta.url));
const PAGE = `<!doctype html><meta charset="utf-8"><title>ghost bench</title><script type="module">
import init, * as g from "./blockula_ghost_wasm.js";
import { benchGhost } from "./ghost-bench-core.mjs";
try {
  const started = performance.now();
  await init();
  const load_ms = Math.round(performance.now() - started);
  window.result = { load_ms, ...benchGhost(g, ${proofs}) };
} catch (e) {
  window.result = { error: String(e) };
}
</script>`;
const routes: Record<string, [string, () => Buffer | string]> = {
  "/": ["text/html", () => PAGE],
  "/blockula_ghost_wasm.js": ["text/javascript", () => file("../src/ghost-wasm/blockula_ghost_wasm.js")],
  "/blockula_ghost_wasm_bg.wasm": ["application/wasm", () => file("../src/ghost-wasm/blockula_ghost_wasm_bg.wasm")],
  "/ghost-bench-core.mjs": ["text/javascript", () => file("./ghost-bench-core.mjs")],
};
const server = createServer((req, res) => {
  const route = routes[req.url ?? ""];
  if (!route) return void res.writeHead(404).end();
  res.writeHead(200, { "content-type": route[0] }).end(route[1]());
});
await new Promise<void>((ok) => server.listen(0, "127.0.0.1", ok));
/**
 * Chromium: the build this Playwright wants, else (when it isn't downloaded) the newest headless
 * shell already in Playwright's cache, or the one GHOST_BENCH_CHROMIUM names. Nothing is downloaded.
 */
async function launch() {
  if (process.env.GHOST_BENCH_CHROMIUM) return chromium.launch({ headless: true, executablePath: process.env.GHOST_BENCH_CHROMIUM });
  try {
    return await chromium.launch({ headless: true });
  } catch (e) {
    const cache = process.env.PLAYWRIGHT_BROWSERS_PATH ?? join(homedir(), process.platform === "darwin" ? "Library/Caches/ms-playwright" : ".cache/ms-playwright");
    const builds = existsSync(cache) ? readdirSync(cache).filter((d) => /^chromium_headless_shell-\d+$/.test(d)).sort((x, y) => Number(y.split("-")[1]) - Number(x.split("-")[1])) : [];
    for (const build of builds) {
      for (const sub of readdirSync(join(cache, build))) {
        const exe = join(cache, build, sub, sub.replace(/-(mac|linux|win).*$/, "") === "chrome-headless-shell" ? "chrome-headless-shell" : "headless_shell");
        if (existsSync(exe)) return chromium.launch({ headless: true, executablePath: exe });
      }
    }
    throw e;
  }
}
const browser = await launch();
try {
  const page = await browser.newPage();
  await page.goto(`http://127.0.0.1:${(server.address() as AddressInfo).port}/`);
  await page.waitForFunction("window.result !== undefined", undefined, { timeout: 120_000 });
  const result = (await page.evaluate("window.result")) as Record<string, unknown>;
  if (result.error) throw new Error(String(result.error));
  console.log(JSON.stringify({ runtime: `headless Chromium ${browser.version()}`, ...result }, null, 2));
} finally {
  await browser.close();
  server.close();
}
