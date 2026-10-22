// Settings the e2e builds are made with (playwright.config.ts) and the mock node answers to.
import { readFileSync } from "node:fs";
import { join } from "node:path";

/** The testnet RPC the e2e build is configured with. Nothing listens there: tests mock it with routes. */
export const RPC = "https://rpc.blockula.test";
/**
 * The mock testnet's chain id and address prefix. The e2e build sets them explicitly
 * (NEXT_PUBLIC_TESTNET_CHAIN_ID, NEXT_PUBLIC_TESTNET_PREFIX), so the specs don't depend on the
 * public testnet's defaults in lib/config.ts.
 */
export const CHAIN_ID = "blockula-testnet";
export const PREFIX = "boo";
/** Pinned in the e2e build (NEXT_PUBLIC_TESTNET_GENESIS_HASH), so a node with another genesis is refused. */
export const GENESIS = "6e".repeat(32);
export const PORT = 3217;

/** The mainnet build (NEXT_PUBLIC_NETWORK=mainnet), tested by e2e/mainnet.spec.ts. */
export const MAINNET_RPC = "https://rpc-mainnet.blockula.test";
const config = readFileSync(join(__dirname, "../lib/config.ts"), "utf8");
/**
 * The mainnet genesis the build pins: the one lib/config.ts builds in once the launch release pins
 * it (a mainnet build refuses any other), else a stand-in.
 */
export const MAINNET_GENESIS = /export const MAINNET_GENESIS_HASH: string \| undefined = "([0-9a-f]{64})";/.exec(config)?.[1] ?? "4d".repeat(32);
/**
 * The genesis clock lib/config.ts builds in with that hash, if it does. The wallet then refuses to
 * sign against a height the chain can't have reached by the browser's clock, so the mock mainnet
 * runs on this clock (e2e/mock-node.ts, e2e/helpers.ts).
 */
const clock = /export const MAINNET_GENESIS_CLOCK: GenesisClock \| undefined = \{ createdAtMs: (\d+), blockTimeMs: (\d+) \};/.exec(config);
export const MAINNET_GENESIS_CLOCK = clock ? { createdAtMs: Number(clock[1]), blockTimeMs: Number(clock[2]) } : undefined;
export const MAINNET_PORT = 3218;
