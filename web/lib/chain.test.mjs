// Exercise the release-pinned clock through the same networkClient factory the wallet uses.
// The pinning script's output is represented in memory, so these tests need no running node.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { build } from "esbuild";

const genesis = "ab".repeat(32);
const clock = { createdAtMs: Date.now() - 1_000_000, blockTimeMs: 1000 };
async function walletClient(overrideHash = genesis) {
  const bundled = await build({
    entryPoints: [new URL("./chain.ts", import.meta.url).pathname],
    bundle: true,
    platform: "node",
    format: "esm",
    write: false,
    define: {
      "process.env.NEXT_PUBLIC_NETWORK": '""',
      "process.env.NEXT_PUBLIC_MAINNET_RPC": '""',
      "process.env.NEXT_PUBLIC_BLOCKULA_RPC": '""',
      "process.env.NEXT_PUBLIC_TESTNET_CHAIN_ID": '""',
      "process.env.NEXT_PUBLIC_TESTNET_GENESIS_HASH": JSON.stringify(overrideHash),
    },
    plugins: [{
      name: "release-genesis-fixture",
      setup(b) {
        b.onLoad({ filter: /\/config\.ts$/ }, ({ path }) => ({
          loader: "ts",
          contents: readFileSync(path, "utf8")
            .replace(/export const TESTNET_GENESIS_HASH: string \| undefined = [^;]+;/, `export const TESTNET_GENESIS_HASH: string | undefined = "${genesis}";`)
            .replace(/export const TESTNET_GENESIS_CLOCK: GenesisClock \| undefined = [^;]+;/, `export const TESTNET_GENESIS_CLOCK: GenesisClock | undefined = ${JSON.stringify(clock)};`),
        }));
      },
    }],
  });
  const module = await import(`data:text/javascript;base64,${Buffer.from(bundled.outputFiles[0].text).toString("base64")}`);
  return module.networkClient();
}

test("a release-pinned genesis clock refuses a future signing height on first contact", async () => {
  const client = await walletClient();
  let genesisReads = 0;
  client.genesis = async () => {
    genesisReads++;
    return { chain_id: client.pin.chainId, created_at_ms: 0, block_time_ms: 1 };
  };
  // Even a cached clock chosen by a hostile node cannot lift the release's bound.
  globalThis.window = {};
  globalThis.localStorage = { getItem: () => JSON.stringify({ [`${client.pin.chainId} ${genesis}`]: { created_at_ms: 0, block_time_ms: 1 } }) };
  try {
    await client.checkSigningHeight(100);
    await assert.rejects(client.checkSigningHeight(1_000_000), /height|block/i);
    assert.equal(genesisReads, 0, "a known clock never comes from the RPC");
    assert.deepEqual(client.genesisClock, clock);
  } finally {
    delete globalThis.window;
    delete globalThis.localStorage;
  }
});

test("a deployment overriding the genesis cannot borrow another genesis's clock", async () => {
  const client = await walletClient("cd".repeat(32));
  assert.equal(client.pin.genesisHash, "cd".repeat(32));
  assert.equal(client.genesisClock, undefined);
});
