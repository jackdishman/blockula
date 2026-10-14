// Settings the e2e build is made with (playwright.config.ts) and the mock node answers to.

/** The testnet RPC the e2e build is configured with. Nothing listens there: tests mock it with page routes. */
export const RPC = "https://rpc.blockula.test";
export const CHAIN_ID = "blockula-testnet";
/** Pinned in the e2e build (NEXT_PUBLIC_TESTNET_GENESIS_HASH), so a node with another genesis is refused. */
export const GENESIS = "6e".repeat(32);
export const PORT = 3217;
