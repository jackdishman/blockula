// Settings the e2e builds are made with (playwright.config.ts) and the mock node answers to.

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
export const MAINNET_GENESIS = "4d".repeat(32);
export const MAINNET_PORT = 3218;
