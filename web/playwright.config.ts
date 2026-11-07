import { defineConfig, devices } from "@playwright/test";
import { CHAIN_ID, DEMO_NODE, GENESIS, MAINNET_GENESIS, MAINNET_PORT, MAINNET_RPC, PORT, PREFIX, RPC, SERVED_PORT, SITE_NODEMO_PORT, SITE_PORT } from "./e2e/env";

/**
 * Browser tests for the wallet (WAL-1): Chromium with a CDP virtual authenticator that supports
 * PRF, against production builds (the CSP under test is the real one) and a mocked node. Two
 * builds: a testnet one for most tests, and a mainnet one (NEXT_PUBLIC_NETWORK=mainnet) for the
 * mainnet-only rules. Two more are the project site (NEXT_PUBLIC_SITE=project, e2e/site.spec.ts), with
 * a mocked demo node and without one. `E2E_DEV=1` runs them against `next dev` instead.
 *
 * `E2E_SERVED=1` adds a third project: the wallet a real node serves at /wallet/ (e2e/served.spec.ts,
 * on a chain e2e/served-node.sh starts), which needs a release build of node/ with the wallet in
 * it (`npm run build:wallet`, then cargo). `E2E_SERVED=only` runs that project alone.
 */
const SERVED = process.env.E2E_SERVED === "1" || process.env.E2E_SERVED === "only";
const BUILDS = process.env.E2E_SERVED !== "only";

const server = (port: number, dist: string, env: Record<string, string>, path = "/welcome") => ({
  command: process.env.E2E_DEV ? `npx next dev --port ${port}` : `npx next build && npx next start --port ${port}`,
  url: `http://localhost:${port}${path}`,
  timeout: 300_000,
  reuseExistingServer: false,
  stdout: "ignore" as const,
  stderr: "pipe" as const,
  // Its own build directory, so the e2e settings never leak into a real build.
  env: { NEXT_DIST_DIR: `.next-e2e/${dist}`, ...env },
});

export default defineConfig({
  testDir: "e2e",
  timeout: 60_000,
  expect: { timeout: 10_000 },
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? "github" : "list",
  use: { ...devices["Desktop Chrome"], trace: "retain-on-failure" },
  projects: [
    ...(BUILDS
      ? [
          { name: "testnet", testIgnore: [/mainnet\.spec\.ts/, /served\.spec\.ts/, /site.*\.spec\.ts/], use: { baseURL: `http://localhost:${PORT}` } },
          { name: "mainnet", testMatch: /mainnet\.spec\.ts/, use: { baseURL: `http://localhost:${MAINNET_PORT}` } },
          { name: "site", testMatch: /site(-chains)?\.spec\.ts/, use: { baseURL: `http://localhost:${SITE_PORT}` } },
          { name: "site-nodemo", testMatch: /site-nodemo\.spec\.ts/, use: { baseURL: `http://localhost:${SITE_NODEMO_PORT}` } },
        ]
      : []),
    ...(SERVED ? [{ name: "served", testMatch: /served\.spec\.ts/, use: { baseURL: `http://localhost:${SERVED_PORT}` } }] : []),
  ],
  webServer: [
    ...(SERVED
      ? [{ command: `e2e/served-node.sh ${SERVED_PORT}`, url: `http://localhost:${SERVED_PORT}/api/status`, timeout: 60_000, reuseExistingServer: false, stdout: "ignore" as const, stderr: "pipe" as const }]
      : []),
    ...(BUILDS
      ? [
          server(PORT, "testnet", { NEXT_PUBLIC_BLOCKULA_RPC: RPC, NEXT_PUBLIC_TESTNET_CHAIN_ID: CHAIN_ID, NEXT_PUBLIC_TESTNET_PREFIX: PREFIX, NEXT_PUBLIC_TESTNET_GENESIS_HASH: GENESIS }),
          server(MAINNET_PORT, "mainnet", {
            NEXT_PUBLIC_NETWORK: "mainnet",
            // Passkeys on localhost are always allowed for development; the rpId check still runs.
            NEXT_PUBLIC_RP_ID: "wallet.blockula.test",
            NEXT_PUBLIC_MAINNET_RPC: MAINNET_RPC,
            NEXT_PUBLIC_MAINNET_GENESIS_HASH: MAINNET_GENESIS,
          }),
          // The chain directory: three fixture listings (mocked nodes), and none at all.
          server(SITE_PORT, "site", { NEXT_PUBLIC_SITE: "project", NEXT_PUBLIC_DEMO_NODE: DEMO_NODE, BLOCKULA_CHAINS_DIR: "e2e/fixtures/chains" }, "/"),
          server(SITE_NODEMO_PORT, "site-nodemo", { NEXT_PUBLIC_SITE: "project", BLOCKULA_CHAINS_DIR: "e2e/fixtures/chains-empty" }, "/"),
        ]
      : []),
  ],
});
