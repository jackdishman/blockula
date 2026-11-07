import type { NextConfig } from "next";
import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { checkChains, type ListedChain } from "./lib/chains";
import { isNodeUrl, MAINNET_GENESIS_HASH, MAINNET_GENESIS_HASH_ENV, NETWORKS, rpIdProblem } from "./lib/config";
import { STATIC_HEADERS } from "./lib/csp";

// Node URLs end up in the CSP and genesis pins in every status check: a malformed one must fail
// the build, not every request.
for (const n of Object.values(NETWORKS)) {
  if (n && !isNodeUrl(n.rpc)) throw new Error(`${n.label} RPC ${JSON.stringify(n.rpc)} isn't an http(s) URL.`);
  if (n?.genesisHash && !/^[0-9a-f]{64}$/.test(n.genesisHash)) throw new Error(`${n.label} genesis hash must be 64 hex characters.`);
}

// A mainnet deployment must pin its domain and its chain before it can build (DEC-8, WAL-4).
if (process.env.NEXT_PUBLIC_NETWORK === "mainnet") {
  const problems = [
    rpIdProblem(),
    !NETWORKS.mainnet && "NEXT_PUBLIC_MAINNET_RPC is unset",
    !/^[0-9a-f]{64}$/.test(MAINNET_GENESIS_HASH_ENV ?? "") && "NEXT_PUBLIC_MAINNET_GENESIS_HASH must be the 64-hex genesis hash",
    // Once the release builds the hash in (scripts/pin-network.sh), the deployment must agree.
    MAINNET_GENESIS_HASH && MAINNET_GENESIS_HASH_ENV !== MAINNET_GENESIS_HASH && `NEXT_PUBLIC_MAINNET_GENESIS_HASH isn't the genesis hash this release pins (${MAINNET_GENESIS_HASH})`,
  ].filter(Boolean);
  if (problems.length) throw new Error(`Refusing to build the mainnet wallet: ${problems.join("; ")}.`);
}

/**
 * `BLOCKULA_SERVED=1` (`npm run build:wallet`): the wallet every node serves at `/wallet/`, a
 * static export into `out/` that the node binary embeds (node/build.rs, node/src/wallet.rs).
 * No server runs it, so there's no proxy.ts (the node sets the CSP, hashing the page's inline
 * scripts) and no headers() (the node sends them); it reads its node, chain and passkey domain
 * from the page's location at runtime (lib/config.ts `servedConfig`).
 */
const SERVED = process.env.BLOCKULA_SERVED === "1";
if (SERVED && process.env.NEXT_PUBLIC_NETWORK === "mainnet") throw new Error("The served wallet is for the chain of whichever node serves it; it can't be a mainnet build.");

// The project site (NEXT_PUBLIC_SITE=project, lib/site.ts) is a landing page, not a wallet: Next
// serves it (its proxy sets the CSP), and lib/config.ts has already refused a malformed
// NEXT_PUBLIC_DEMO_NODE by the time this runs.
const site = process.env.NEXT_PUBLIC_SITE;
if (site && site !== "project") throw new Error(`NEXT_PUBLIC_SITE is ${JSON.stringify(site)}; the only site is "project".`);
if (site && (SERVED || process.env.NEXT_PUBLIC_NETWORK === "mainnet")) throw new Error("The project site is its own build: not the served wallet, not a mainnet one.");

/**
 * The chain directory the project site lists (chains/*.json, lib/chains.ts), read and checked at
 * build time: a listing that breaks the rules fails the build. `BLOCKULA_CHAINS_DIR` points it at
 * another directory (the e2e builds' fixtures). The site reads the list as BLOCKULA_SITE_CHAINS
 * (lib/config.ts SITE_CHAINS), and its CSP lets the page read each listed node (lib/csp.ts).
 */
function siteChains(): ListedChain[] {
  const dir = path.resolve(process.env.BLOCKULA_CHAINS_DIR || path.join(__dirname, "../chains"));
  const files = readdirSync(dir)
    .filter((name) => !statSync(path.join(dir, name)).isDirectory())
    .map((name) => ({ name, text: readFileSync(path.join(dir, name), "utf8") }));
  const { chains, errors } = checkChains(files);
  if (errors.length) throw new Error(`The chain directory (${dir}) has listings the site can't take:\n  ${errors.join("\n  ")}`);
  return chains;
}
const chainEnv: Record<string, string> = site ? { BLOCKULA_SITE_CHAINS: JSON.stringify(siteChains()) } : {};

const served: NextConfig = {
  output: "export",
  basePath: "/wallet",
  // Every page is a directory with an index.html (and its RSC payloads beside it), which the
  // node maps a dynamic route's id onto (`app/tx/[id]` → `tx/_/`).
  trailingSlash: true,
  // The export (in this mode distDir is where it goes): web/out, which node/build.rs embeds.
  distDir: "out",
  env: { NEXT_PUBLIC_BLOCKULA_SERVED: "1" },
  // The build id is in every page, and Next makes a random one. The commit's makes the export the
  // same bytes on every machine, so a release binary that embeds it rebuilds bit for bit.
  generateBuildId: () => servedBuildId(),
  // One page at a time, so how each page's inline data is cut into scripts doesn't depend on
  // the machine's load.
  experimental: { sri: { algorithm: "sha384" }, cpus: 1, staticGenerationMaxConcurrency: 1 },
};

function servedBuildId(): string {
  if (process.env.BLOCKULA_GIT_SHA) return process.env.BLOCKULA_GIT_SHA;
  try {
    return execFileSync("git", ["rev-parse", "--short=12", "HEAD"], { encoding: "utf8" }).trim();
  } catch {
    return "wallet";
  }
}

const nextConfig: NextConfig = {
  reactStrictMode: true,
  poweredByHeader: false,
  // Don't let `next dev` write an AGENTS.md into web/; the repo has its own.
  agentRules: false,
  // The e2e tests build with their own settings; keep that out of the real build's directory.
  distDir: process.env.NEXT_DIST_DIR || ".next",
  // Monorepo: the workspace root holds the lockfile and the linked @blockula/sdk.
  turbopack: { root: path.join(__dirname, "..") },
  outputFileTracingRoot: path.join(__dirname, ".."),
  // Integrity hashes on every script Next.js emits, on top of the nonce-based CSP (proxy.ts).
  experimental: { sri: { algorithm: "sha384" } },
  env: chainEnv,
  async headers() {
    return [{ source: "/:path*", headers: STATIC_HEADERS }];
  },
};

export default SERVED ? { ...nextConfig, headers: undefined, ...served } : nextConfig;
