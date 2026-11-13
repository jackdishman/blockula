#!/usr/bin/env node
/**
 * blackjack-xbot: the blackjack X bot, dry run only.
 *
 *   blackjack-xbot init                               make the relayer's key file
 *   blackjack-xbot run --posts <file|-> --out <dir>   read posts (JSON lines), write replies as files
 *   blackjack-xbot status                             what the store holds, and the table if a node is set
 *   blackjack-xbot enroll <address>                   add an account to check for an X link
 *
 * It has no X client. It reads posts it is handed and writes its replies to a directory.
 */
import { createReadStream } from "node:fs";
import { createInterface } from "node:readline";
import { parseArgs } from "node:util";
import { Client, Wallet } from "@blockula/sdk";
import { fang } from "@blockula/blackjack";
import { renderTable } from "@blockula/blackjack-render";
import { Bot } from "./bot.js";
import { defaultHome, keyPath, loadSettings, readKey, storePath, writeKey } from "./config.js";
import { runPosts } from "./run.js";
import { SdkTable, localRpc } from "./sdk-table.js";
import { DirSink } from "./sink.js";
import { fileVerifier } from "./standin.js";
import { JsonStore, pinStore } from "./store.js";
import { systemClock } from "./types.js";

const USAGE = `blackjack-xbot (dry run: it has no X client and posts nothing)

  init                               make the relayer's key file (0600), once
  run --posts <file|-> --out <dir>   read posts as JSON lines, write each reply as files
      [--proofs <file>]              local stand-in posts for checking X links
      [--drain-ms <n>]               after the last post, wait this long for dealers (default 30000)
  status                             the store's counts, and the table if a node is set
  enroll <address>                   also check this account's X link

  --home <dir>    where the key, the store and config.json live (BLACKJACK_XBOT_HOME)
  --rpc <url>     a node on this machine (BLOCKULA_RPC)
  --table <addr>  the table's address (BLACKJACK_TABLE)`;

async function main(): Promise<number> {
  const { values, positionals } = parseArgs({
    allowPositionals: true,
    options: { home: { type: "string" }, rpc: { type: "string" }, table: { type: "string" }, posts: { type: "string" }, out: { type: "string" }, proofs: { type: "string" }, "drain-ms": { type: "string" }, help: { type: "boolean", short: "h" } },
  });
  const [command, ...rest] = positionals;
  if (values.help || !command) {
    console.log(USAGE);
    return command ? 0 : 1;
  }
  const home = values.home ?? defaultHome();
  const settings = loadSettings(home, { rpc: values.rpc, table: values.table });
  const rpc = settings.rpc === null ? null : localRpc(settings.rpc);

  /** The relayer's address on the configured chain (its prefix comes from the node), or with the default prefix. */
  const relayerAddress = async () => {
    const client = rpc ? new Client(rpc, { network: "local" }) : undefined;
    if (client) await client.addressPrefix();
    return Wallet.fromSeed(readKey(home), client).address;
  };

  switch (command) {
    case "init": {
      const path = writeKey(home, Wallet.generate().seedHex);
      console.log(`Made the relayer's key: ${path} (mode 0600). Back it up; it is never shown.`);
      console.log(`Relayer address: ${await relayerAddress()}`);
      console.log("The table's owner approves this address, and each player grants it at the table.");
      return 0;
    }
    case "enroll": {
      const address = rest[0];
      if (!address || !/^[a-z]{1,10}1[a-z0-9]{20,90}$/.test(address)) throw new Error("enroll takes an account's address");
      const store = JsonStore.open(storePath(home));
      if (!store.state.enrolled.includes(address)) store.state.enrolled.push(address);
      store.commit();
      console.log(`Enrolled. ${store.state.enrolled.length} enrolled by hand.`);
      return 0;
    }
    case "status": {
      const s = JsonStore.open(storePath(home)).state;
      const posts = Object.values(s.posts);
      console.log("Dry run build: no X client, nothing is posted.");
      console.log(`Home: ${home}`);
      console.log(`Key file: ${keyPath(home)}`);
      console.log(s.chain ? `Store: for ${s.chain.chainId}, table ${s.chain.table}` : "Store: not used yet");
      console.log(`Posts recorded: ${posts.length} (${posts.filter((p) => p.state !== "done").length} unfinished)`);
      console.log(`Open hands: ${Object.keys(s.hands).length} (${Object.values(s.hands).filter((h) => h.awaiting).length} waiting on the dealer)`);
      console.log(`Stopped handles: ${Object.keys(s.stopped).length}`);
      const links = Object.values(s.links.accounts);
      console.log(`Accounts known: ${links.length} (${links.filter((l) => l.verified).length} with a checked X link), ${s.enrolled.length} enrolled by hand`);
      if (rpc && settings.table) {
        const table = await SdkTable.open({ rpc, program: settings.table, seed: readKey(home) });
        const [house, height] = [await table.house(), await table.height()];
        console.log(`Node: ${rpc} at block ${height}`);
        console.log(`Table: ${settings.table}${house.paused ? " (paused for new hands)" : ""}, ${house.open} open, bankroll ${fang(house.bankroll)} FANG`);
        console.log(`Relayer: ${table.relayer}`);
      } else {
        console.log("No node or table set: --rpc and --table (or BLOCKULA_RPC and BLACKJACK_TABLE).");
      }
      return 0;
    }
    case "run": {
      if (!values.posts || !values.out) throw new Error("run takes --posts <file|-> and --out <dir>");
      if (!rpc || !settings.table) throw new Error("run needs a local node and a table: --rpc and --table (or BLOCKULA_RPC and BLACKJACK_TABLE)");
      const drainMs = values["drain-ms"] === undefined ? 30_000 : Number(values["drain-ms"]);
      if (!Number.isSafeInteger(drainMs) || drainMs < 0) throw new Error("--drain-ms is a whole number of milliseconds");
      const table = await SdkTable.open({ rpc, program: settings.table, seed: readKey(home) });
      const store = JsonStore.open(storePath(home));
      pinStore(store, table.chain);
      const bot = new Bot({ table, store, verifyLink: fileVerifier(values.proofs ?? null), render: (snapshot) => renderTable(snapshot), sink: new DirSink(values.out), clock: systemClock, config: settings.bot });
      console.log(`Dry run on ${rpc}: replies are written to ${values.out} and nothing is posted.`);
      const input = values.posts === "-" ? process.stdin : createReadStream(values.posts, "utf8");
      const sum = await runPosts(bot, createInterface({ input, crlfDelay: Infinity }), { clock: systemClock, drainMs, log: (line) => console.log(line) });
      console.log(`${sum.posts} posts read, ${sum.replies} replies written, ${sum.unreadable} unreadable lines, ${sum.waiting} hands still waiting on the dealer.`);
      return 0;
    }
    default:
      console.log(USAGE);
      return 1;
  }
}

main().then(
  (code) => process.exit(code),
  (e: unknown) => {
    // An error's message is this program's or the SDK's words; a key never appears in one.
    console.error(e instanceof Error ? e.message : String(e));
    process.exit(1);
  },
);
