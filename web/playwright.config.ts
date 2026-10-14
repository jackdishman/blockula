import { defineConfig, devices } from "@playwright/test";
import { GENESIS, MAINNET_GENESIS, MAINNET_PORT, MAINNET_RPC, PORT, RPC } from "./e2e/env";

/**
 * Browser tests for the wallet (WAL-1): Chromium with a CDP virtual authenticator that supports
 * PRF, against production builds (the CSP under test is the real one) and a mocked node. Two
 * builds: a testnet one for most tests, and a mainnet one (NEXT_PUBLIC_NETWORK=mainnet) for the
 * mainnet-only rules. `E2E_DEV=1` runs them against `next dev` instead.
 */
const server = (port: number, dist: string, env: Record<string, string>) => ({
  command: process.env.E2E_DEV ? `npx next dev --port ${port}` : `npx next build && npx next start --port ${port}`,
  url: `http://localhost:${port}/welcome`,
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
    { name: "testnet", testIgnore: /mainnet\.spec\.ts/, use: { baseURL: `http://localhost:${PORT}` } },
    { name: "mainnet", testMatch: /mainnet\.spec\.ts/, use: { baseURL: `http://localhost:${MAINNET_PORT}` } },
  ],
  webServer: [
    server(PORT, "testnet", { NEXT_PUBLIC_BLOCKULA_RPC: RPC, NEXT_PUBLIC_TESTNET_GENESIS_HASH: GENESIS }),
    server(MAINNET_PORT, "mainnet", {
      NEXT_PUBLIC_NETWORK: "mainnet",
      // Passkeys on localhost are always allowed for development; the rpId check still runs.
      NEXT_PUBLIC_RP_ID: "wallet.blockula.test",
      NEXT_PUBLIC_MAINNET_RPC: MAINNET_RPC,
      NEXT_PUBLIC_MAINNET_GENESIS_HASH: MAINNET_GENESIS,
    }),
  ],
});
