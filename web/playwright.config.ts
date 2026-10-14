import { defineConfig, devices } from "@playwright/test";
import { GENESIS, PORT, RPC } from "./e2e/env";

/**
 * Browser tests for the wallet (WAL-1): Chromium with a CDP virtual authenticator that supports
 * PRF, against a production build (the CSP under test is the real one) and a mocked node.
 * `E2E_DEV=1` runs them against `next dev` instead.
 */
export default defineConfig({
  testDir: "e2e",
  timeout: 60_000,
  expect: { timeout: 10_000 },
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? "github" : "list",
  use: { ...devices["Desktop Chrome"], baseURL: `http://localhost:${PORT}`, trace: "retain-on-failure" },
  webServer: {
    command: process.env.E2E_DEV ? `npx next dev --port ${PORT}` : `npx next build && npx next start --port ${PORT}`,
    url: `http://localhost:${PORT}/welcome`,
    timeout: 300_000,
    reuseExistingServer: false,
    stdout: "ignore",
    stderr: "pipe",
    env: {
      // Its own build directory, so the e2e settings never leak into a real build.
      NEXT_DIST_DIR: ".next-e2e",
      NEXT_PUBLIC_BLOCKULA_RPC: RPC,
      NEXT_PUBLIC_TESTNET_GENESIS_HASH: GENESIS,
    },
  },
});
