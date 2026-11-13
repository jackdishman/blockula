#!/usr/bin/env node
/**
 * `blackjack-dealer`: the dealer daemon's command line.
 *
 *   blackjack-dealer init    make the key and secret files (if absent) and print the dealer's address
 *   blackjack-dealer run     deal: commit ahead, reveal, stand idle players (`--dry-run` sends nothing)
 *   blackjack-dealer status  the address, its balance, unused commitments, open hands, the store's health
 *
 * Settings come from flags, `BJ_DEALER_*` variables or a JSON file (`--config`): the README has
 * the list. Secrets are only ever read from files (or the keychain), never from the command line.
 */
import { DROPS_PER_FANG, Table, fang } from "@blockula/blackjack";
import { Client, Wallet } from "@blockula/sdk";
import { mkdirSync } from "node:fs";
import { WalletSender } from "./chain.js";
import { loadConfig, parseArgs, type DealerConfig } from "./config.js";
import { Dealer, inspect, type Health } from "./dealer.js";
import { checkPrivate, createDealerKey, createStoreKey, keychainStoreKey, readDealerSeed, readStoreKey } from "./keys.js";
import { errorText, jsonLogger } from "./log.js";
import { SeedStore } from "./store.js";

const USAGE = `blackjack-dealer <init | run | status> [options]

  init      make the dealer's key file and the store's secret file (if absent), print the address
  run       deal for the table: commit ahead of the bets, reveal cards, stand idle players
  status    the dealer's address and balance, unused commitments, open hands, the store's health

  --rpc URL            the node (required; this machine's unless --allow-remote)
  --table ADDRESS      the table: the program's address
  --dir PATH           where the key, secret and store live (default ~/.blockula/blackjack-dealer)
  --config FILE        a JSON file of settings
  --dry-run            (run) log what would be sent; send nothing
  --json               (status) print JSON

Every setting, with its BJ_DEALER_* variable, is in services/blackjack/dealer/README.md.`;

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** Amount text ("2.5") to drops. */
function drops(text: string): bigint {
  const [whole, frac = ""] = text.split(".");
  return BigInt(whole) * DROPS_PER_FANG + BigInt(frac.padEnd(9, "0"));
}

function client(cfg: DealerConfig): Client {
  return new Client(cfg.rpc, { chainId: cfg.chainId, genesisHash: cfg.genesisHash });
}

function storeKey(cfg: DealerConfig): Buffer {
  return cfg.keychain ? keychainStoreKey(cfg.keychainService, cfg.keychainAccount) : readStoreKey(cfg.secretFile);
}

async function init(cfg: DealerConfig): Promise<void> {
  mkdirSync(cfg.dir, { recursive: true, mode: 0o700 });
  const madeKey = createDealerKey(cfg.keyFile);
  console.log(`${madeKey ? "made" : "kept"} the dealer's key file: ${cfg.keyFile}`);
  if (cfg.keychain) {
    console.log(`the store's secret is read from the keychain (service ${cfg.keychainService}, account ${cfg.keychainAccount}): the README's "The keychain" says how to put it there`);
  } else {
    const madeSecret = createStoreKey(cfg.secretFile);
    console.log(`${madeSecret ? "made" : "kept"} the store's secret file: ${cfg.secretFile}`);
  }
  // With a node, the address carries the chain's prefix; without one, the usual `boo`.
  const c = cfg.rpc ? client(cfg) : new Client("http://127.0.0.1:9");
  if (cfg.rpc) await c.addressPrefix();
  const wallet = Wallet.fromSeed(readDealerSeed(cfg.keyFile), c);
  console.log(`dealer address: ${wallet.address}`);
  console.log("Next: the table's owner names this address as the dealer (at init, or with set_dealer), and it needs FANG for fees and commitment deposits. Back up both files: without them the dealer can't deal the chains it committed.");
}

async function run(cfg: DealerConfig): Promise<void> {
  const log = jsonLogger();
  const seed = readDealerSeed(cfg.keyFile);
  const store = SeedStore.open({ path: cfg.storePath, backupPath: cfg.backupPath, key: storeKey(cfg), table: cfg.table });
  process.on("exit", () => store.close());
  let stopping = false;
  const stop = () => {
    stopping = true;
  };
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);

  const c = client(cfg);
  const wallet = Wallet.fromSeed(seed, c);
  const table = Table.at(cfg.rpc, cfg.table);
  const dealer = new Dealer(table, new WalletSender(wallet, c, cfg.table), store, log, {
    lowWater: cfg.lowWater,
    target: cfg.target,
    batch: cfg.batch,
    dryRun: cfg.dryRun,
    keepSettled: cfg.keepSettled,
    lowBalance: drops(cfg.lowBalance),
    txValidBlocks: cfg.txValidBlocks,
    fullFuel: cfg.fullFuel,
  });

  // A node that isn't up yet (or a table that isn't created yet) is waited for, not a reason to exit.
  let fails = 0;
  const wait = async (what: string, e: unknown) => {
    fails++;
    log.warn(what, { error: errorText(e), fails });
    await sleep(Math.min(cfg.pollMs * 2 ** Math.min(fails, 6), 5_000));
  };
  for (let started = false; !started && !stopping; ) {
    try {
      await c.addressPrefix();
      await dealer.start();
      started = true;
      fails = 0;
    } catch (e) {
      await wait("start_failed", e);
    }
  }
  while (!stopping) {
    try {
      await dealer.tick();
      fails = 0;
      await sleep(cfg.pollMs);
    } catch (e) {
      // The node didn't answer, or answered nonsense: read again shortly. Deadlines are counted
      // in blocks, so the wait stays short.
      await wait("tick_failed", e);
    }
  }
  // What's on its way is given a moment to come back; anything else is picked up from the chain at the next start.
  await Promise.race([dealer.drain(), sleep(10_000)]);
  log.info("stopped", {});
  store.close();
  process.exit(0);
}

function statusText(cfg: DealerConfig, address: string, balance: bigint, h: Health, backupCurrent: boolean): string {
  const lines = [
    `table            ${cfg.table}`,
    `node             ${cfg.rpc} (block ${h.height})`,
    `dealer address   ${address}${h.isDealer ? "" : "   ⚠ not this table's dealer: the owner names it with set_dealer"}`,
    `balance          ${fang(balance)} FANG${balance < drops(cfg.lowBalance) ? `   ⚠ under ${cfg.lowBalance}: it pays every reveal's fee and bonds a deposit for every commitment` : ""}`,
    `commitments      ${h.unused} unused (games ${h.nextGame}–${h.nextCommit - 1n}), ${h.servable} this store can deal; tops up under ${cfg.lowWater} to ${cfg.target}`,
    `open hands       ${h.open.length}`,
    ...h.open.map((g) => `  game ${g.id}: ${g.dealers ? "the dealer's move" : "the player's move"}, deadline block ${g.deadline}${g.dealers && !g.hasSecret ? "   ⚠ no secret for it in the store" : ""}`),
    `store            ${cfg.storePath}: ${h.stored} secrets (${h.uncommitted} not committed yet)`,
    `backup           ${cfg.backupPath}: ${backupCurrent ? "same as the store" : "⚠ missing or behind the store (the next change rewrites it)"}`,
  ];
  if (h.missing.length) lines.push(`⚠ the store has no secret for unused commitments ${h.missing.map((r) => (r.from === r.to ? `${r.from}` : `${r.from}–${r.to}`)).join(", ")}: restore the backup, or the owner pauses the table and drops them with skip_commits`);
  if (h.mismatched.length) lines.push(`⚠ the store's secrets don't hash to the chain's commitments for games ${h.mismatched.join(", ")}`);
  return lines.join("\n");
}

async function status(cfg: DealerConfig, json: boolean): Promise<void> {
  // The private files first: a key others can read is refused before anything else happens.
  const seed = readDealerSeed(cfg.keyFile);
  const store = SeedStore.open({ path: cfg.storePath, backupPath: cfg.backupPath, key: storeKey(cfg), table: cfg.table, readOnly: true });
  const c = client(cfg);
  await c.addressPrefix();
  const wallet = Wallet.fromSeed(seed, c);
  const sender = new WalletSender(wallet, c, cfg.table);
  const [h, balance] = [await inspect(Table.at(cfg.rpc, cfg.table), store, wallet.address), await sender.balance()];
  if (!json) return void console.log(statusText(cfg, wallet.address, balance, h, store.backupCurrent));
  const text = (v: unknown): unknown => (typeof v === "bigint" ? v.toString() : Array.isArray(v) ? v.map(text) : v !== null && typeof v === "object" ? Object.fromEntries(Object.entries(v).map(([k, x]) => [k, text(x)])) : v);
  console.log(JSON.stringify(text({ table: cfg.table, rpc: cfg.rpc, address: wallet.address, balance, low_balance: balance < drops(cfg.lowBalance), backup_current: store.backupCurrent, ...h }), null, 2));
}

async function main(): Promise<void> {
  const args = parseArgs(process.argv.slice(2));
  switch (args.command) {
    case "init":
      return init(loadConfig(args, process.env, { needTable: false, needRpc: false }));
    case "run":
      return run(loadConfig(args, process.env));
    case "status":
      return status(loadConfig(args, process.env), args.json);
    default:
      console.log(USAGE);
      if (args.command !== undefined && args.command !== "help") process.exitCode = 2;
  }
}

main().catch((e) => {
  // What went wrong, never a secret: messages are scrubbed of anything that looks like one.
  console.error(`blackjack-dealer: ${errorText(e).replace(/^Error: /, "")}`);
  process.exit(1);
});
