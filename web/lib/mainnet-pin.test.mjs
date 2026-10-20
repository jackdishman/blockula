// Mainnet's genesis hash and clock as the launch release builds them into lib/config.ts
// (scripts/pin-network.sh blockula-1 … --mainnet), represented in memory like chain.test.mjs's
// testnet pin: a mainnet build pins both, no deploy setting changes them, and next.config.ts
// refuses a deployment whose NEXT_PUBLIC_MAINNET_GENESIS_HASH disagrees.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { build } from "esbuild";

const genesis = "ab".repeat(32);
const clock = { createdAtMs: Date.now() - 1_000_000, blockTimeMs: 1000 };

// entry (chain.ts or ../next.config.ts) bundled as a mainnet build with NEXT_PUBLIC_* from env,
// and config.ts as it reads once pinned (or, with pinned = false, before the pin, whatever this
// tree pins).
async function load(entry, env, pinned = true) {
  const bundled = await build({
    entryPoints: [new URL(entry, import.meta.url).pathname],
    bundle: true,
    platform: "node",
    format: "esm",
    write: false,
    external: ["next"],
    define: {
      ...Object.fromEntries(
        Object.entries({ NEXT_PUBLIC_NETWORK: "mainnet", NEXT_PUBLIC_RP_ID: "wallet.blockula.example", NEXT_PUBLIC_MAINNET_RPC: "https://rpc.blockula.example", NEXT_PUBLIC_BLOCKULA_RPC: "", ...env }).map(([k, v]) => [`process.env.${k}`, JSON.stringify(v)]),
      ),
      // next.config.ts is CommonJS-flavoured (it reads __dirname); here it's an ES module.
      __dirname: JSON.stringify(new URL("..", import.meta.url).pathname),
    },
    plugins: [{
      name: "mainnet-pin-fixture",
      setup(b) {
        b.onLoad({ filter: /\/lib\/config\.ts$/ }, ({ path }) => {
          const source = readFileSync(path, "utf8");
          const [hash, at] = pinned ? [`"${genesis}"`, JSON.stringify(clock)] : ["undefined", "undefined"];
          const contents = source
            .replace(/export const MAINNET_GENESIS_HASH: string \| undefined = [^;]+;/, `export const MAINNET_GENESIS_HASH: string | undefined = ${hash};`)
            .replace(/export const MAINNET_GENESIS_CLOCK: GenesisClock \| undefined = [^;]+;/, `export const MAINNET_GENESIS_CLOCK: GenesisClock | undefined = ${at};`);
          assert.ok(contents.includes(`MAINNET_GENESIS_HASH: string | undefined = ${hash};`), "config.ts has the MAINNET_GENESIS_HASH line pin-network.sh writes");
          assert.ok(contents.includes(`MAINNET_GENESIS_CLOCK: GenesisClock | undefined = ${at};`), "config.ts has the MAINNET_GENESIS_CLOCK line pin-network.sh writes");
          return { loader: "ts", contents };
        });
      },
    }],
  });
  return import(`data:text/javascript;base64,${Buffer.from(bundled.outputFiles[0].text).toString("base64")}`);
}

test("a mainnet build uses the release-pinned genesis and its clock", async () => {
  const client = (await load("./chain.ts", { NEXT_PUBLIC_MAINNET_GENESIS_HASH: genesis })).networkClient();
  assert.deepEqual([client.pin.chainId, client.pin.genesisHash, client.pin.prefix], ["blockula-1", genesis, "boo"]);
  assert.deepEqual(client.genesisClock, clock);
  await assert.rejects(client.checkSigningHeight(1_000_000), /height|block/i);
});

test("no deploy setting changes a built-in mainnet genesis", async () => {
  const client = (await load("./chain.ts", { NEXT_PUBLIC_MAINNET_GENESIS_HASH: "cd".repeat(32) })).networkClient();
  assert.equal(client.pin.genesisHash, genesis);
  assert.deepEqual(client.genesisClock, clock);
});

test("before the pin, the deployment's genesis and no built-in clock", async () => {
  const client = (await load("./chain.ts", { NEXT_PUBLIC_MAINNET_GENESIS_HASH: "cd".repeat(32) }, false)).networkClient();
  assert.equal(client.pin.genesisHash, "cd".repeat(32));
  assert.equal(client.genesisClock, undefined);
});

test("a mainnet build needs NEXT_PUBLIC_MAINNET_GENESIS_HASH, and once pinned it must agree", async () => {
  await load("../next.config.ts", { NEXT_PUBLIC_MAINNET_GENESIS_HASH: genesis });
  await assert.rejects(load("../next.config.ts", { NEXT_PUBLIC_MAINNET_GENESIS_HASH: "cd".repeat(32) }), /isn't the genesis hash this release pins/);
  await assert.rejects(load("../next.config.ts", { NEXT_PUBLIC_MAINNET_GENESIS_HASH: "" }), /must be the 64-hex genesis hash/);
  await load("../next.config.ts", { NEXT_PUBLIC_MAINNET_GENESIS_HASH: "cd".repeat(32) }, false);
});
