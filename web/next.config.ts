import type { NextConfig } from "next";
import path from "node:path";
import { isNodeUrl, MAINNET_GENESIS_HASH, NETWORKS, rpIdProblem } from "./lib/config";
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
    !/^[0-9a-f]{64}$/.test(MAINNET_GENESIS_HASH ?? "") && "NEXT_PUBLIC_MAINNET_GENESIS_HASH must be the 64-hex genesis hash",
  ].filter(Boolean);
  if (problems.length) throw new Error(`Refusing to build the mainnet wallet: ${problems.join("; ")}.`);
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
  async headers() {
    return [{ source: "/:path*", headers: STATIC_HEADERS }];
  },
};

export default nextConfig;
