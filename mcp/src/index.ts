#!/usr/bin/env node
// 🧛 Blockula MCP server. Add to Claude Code with:
//   claude mcp add blockula -- npx -y @blockula/mcp                      (the public testnet)
//   claude mcp add blockula -- npx -y @blockula/mcp --network local      (a `blockula dev` chain)
//
// Env:
//   BLOCKULA_NETWORK    mainnet, testnet (default), local or custom (same as --network)
//   BLOCKULA_RPC        node URL (default: the network's public RPC; required for custom, and for
//                       mainnet until public RPCs are built in)
//   BLOCKULA_CHAIN_ID / BLOCKULA_GENESIS_HASH   pin the chain the node must be on
//   BLOCKULA_HOME       where the agent wallet lives (default ~/.blockula)
//   BLOCKULA_MAX_SEND   max FANG per send() call (default 100)
//   BLOCKULA_MAX_DAILY  max FANG this MCP sends per UTC day, across calls (default 100)
//   BLOCKULA_BIN        path to the `blockula` binary (default: ~/.blockula/bin/blockula from install_node, else PATH)
//   BLOCKULA_REQUIRE_VERIFY=1   install_node refuses a release it can only checksum
//   BLOCKULA_RELEASE_KEY_ID     the release signing key id to trust (until it's built in)
// The network never changes while the server runs (WAL-4): see network.ts. When acting for a
// user's account via a session key, the chain also enforces that key's limit.

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { ApiError, Client, Wallet, formatFang, parseFang, proposalSummary, sharesFor, stakeSummary, type ProposalSummary } from "@blockula/sdk";
import { spawn, spawnSync, execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, openSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { z } from "zod";
import { KnownChains, completePin, effectivePin, mismatch, mismatchError, resolveNetwork, type Network } from "./network.js";
import { REPO, RELEASE_KEY_ID, fetchVerifiedRelease } from "./release.js";

const HOME = process.env.BLOCKULA_HOME ?? join(homedir(), ".blockula");
const WALLET_FILE = join(HOME, "agent-wallet.json");
const MAX_SEND = parseFang(process.env.BLOCKULA_MAX_SEND ?? "100");
const MAX_DAILY = parseFang(process.env.BLOCKULA_MAX_DAILY ?? "100");
const SPEND_FILE = join(HOME, "agent-spend.json");
const KNOWN_CHAINS = new KnownChains(join(HOME, "known-chains.json"));
const VERSION: string = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8")).version;
const INSTALLED_BIN = join(HOME, "bin", "blockula");
let BIN = process.env.BLOCKULA_BIN ?? (existsSync(INSTALLED_BIN) ? INSTALLED_BIN : "blockula");

/** Testnet economics, said wherever Hunts and the contribution pool come up. */
const NO_VALUE = "Testnet FANG, Hunts and Soul have no monetary value and come with no promise of any mainnet allocation.";

function releaseTarget(): string | null {
  const key = `${process.platform}-${process.arch}`;
  return ({ "darwin-arm64": "aarch64-apple-darwin", "darwin-x64": "x86_64-apple-darwin", "linux-x64": "x86_64-unknown-linux-gnu", "linux-arm64": "aarch64-unknown-linux-gnu" } as Record<string, string>)[key] ?? null;
}

// ---- the network: chosen once, pinned, never switched ----

let NET: Network | null = null;
let NET_ERROR: Error | null = null;
try {
  NET = resolveNetwork(process.env, process.argv.slice(2));
} catch (e) {
  NET_ERROR = e as Error;
}
const RPC = NET?.rpc ?? "";
let pinned: Client | null = null;

function network(): Network {
  if (!NET) throw new Error(`The Blockula MCP isn't configured correctly: ${NET_ERROR?.message}`);
  return NET;
}

/**
 * The client for the configured network, pinned to its chain. The first call checks the node's
 * chain id, genesis hash and prefix against the network's pins (and what was remembered for it);
 * from then on the SDK re-checks every status, including the one every signature is made against.
 */
async function chain(): Promise<Client> {
  if (pinned) return pinned;
  const net = network();
  const pin = effectivePin(net, KNOWN_CHAINS.get(net));
  const status = await new Client(net.rpc).status();
  const why = mismatch(pin, status);
  if (why) throw mismatchError(net, why, KNOWN_CHAINS.file);
  const full = completePin(pin, status);
  KNOWN_CHAINS.save(net, full);
  pinned = new Client(net.rpc, { chainId: full.chainId, genesisHash: full.genesisHash, prefix: full.prefix });
  return pinned;
}

async function reachable(url: string) {
  try {
    await new Client(url).status();
    return true;
  } catch {
    return false;
  }
}

const explorer = (path: string) => `${RPC}/#/${path}`;

/** A protocol v1 read, with a plain answer on nodes whose chain doesn't have the module yet. */
async function moduleRead<T>(what: string, read: Promise<T>): Promise<T> {
  try {
    return await read;
  } catch (e) {
    if (e instanceof ApiError && e.status === 404 && /404|not found/i.test(e.message)) throw new Error(`${RPC} doesn't serve ${what}: it isn't active on this chain yet`);
    throw e;
  }
}

// ---- spending caps (BLOCKULA_MAX_SEND per call, BLOCKULA_MAX_DAILY per UTC day) ----

/** Running total of FANG sent today (UTC), persisted so the cap holds across calls and restarts. */
function spentToday(): { day: string; spent: bigint } {
  const day = new Date().toISOString().slice(0, 10);
  try {
    const f = JSON.parse(readFileSync(SPEND_FILE, "utf8"));
    if (f.day === day) return { day, spent: BigInt(f.spent) };
  } catch {
    /* no spend yet */
  }
  return { day, spent: 0n };
}

/** Refuses `drops` if it's over the per-call cap or would pass today's cap. */
function checkBudget(drops: bigint, what = "amount") {
  if (drops > MAX_SEND) throw new Error(`${what} exceeds this agent's per-send limit (BLOCKULA_MAX_SEND=${process.env.BLOCKULA_MAX_SEND ?? "100"} FANG)`);
  const today = spentToday();
  if (today.spent + drops > MAX_DAILY) {
    throw new Error(`this would exceed the agent's daily limit (BLOCKULA_MAX_DAILY=${process.env.BLOCKULA_MAX_DAILY ?? "100"} FANG; ${formatFang(today.spent)} already sent today)`);
  }
}

function recordSpend(drops: bigint) {
  const today = spentToday();
  mkdirSync(HOME, { recursive: true });
  writeFileSync(SPEND_FILE, JSON.stringify({ day: today.day, spent: (today.spent + drops).toString() }), { mode: 0o600 });
}

const server = new McpServer({ name: "blockula", version: VERSION });

const text = (v: unknown) => ({ content: [{ type: "text" as const, text: typeof v === "string" ? v : JSON.stringify(v, (_, x) => (typeof x === "bigint" ? x.toString() : x), 2) }] });

interface WalletFile { address: string; key_id: string; seed: string; account?: string }

function readWalletFile(): WalletFile | null {
  return existsSync(WALLET_FILE) ? JSON.parse(readFileSync(WALLET_FILE, "utf8")) : null;
}

async function loadWallet(): Promise<Wallet | null> {
  const f = readWalletFile();
  if (!f) return null;
  const w = Wallet.fromSeed(f.seed, await chain());
  if (f.account) w.account = f.account;
  return w;
}

async function requireWallet(): Promise<Wallet> {
  const w = await loadWallet();
  if (!w) throw new Error("No agent wallet yet — call create_wallet first.");
  return w;
}

function tool<S extends z.ZodRawShape>(name: string, description: string, shape: S, run: (args: z.infer<z.ZodObject<S>>) => Promise<unknown>) {
  server.registerTool(name, { description, inputSchema: shape }, (async (args: z.infer<z.ZodObject<S>>) => {
    try {
      return text(await run(args));
    } catch (e) {
      const msg = (e as Error).message;
      const hint = /fetch failed|ECONNREFUSED/.test(msg) ? ` (no node at ${RPC}${NET?.key === "local" ? " — call start_dev_node" : ""})` : "";
      return { ...text(`Error: ${msg}${hint}`), isError: true };
    }
  }) as never);
}

tool("status", "Blockula network status: which network and node you're connected to (and the chain it's pinned to), chain id, height, supply, fees burned, and the crypto in use.", {}, async () => {
  const net = network();
  const s = await (await chain()).status();
  return {
    network: net.label,
    real_money: net.mainnet,
    rpc: net.rpc,
    pinned: { chain_id: s.chain_id, genesis_hash: s.genesis_hash, address_prefix: s.address_prefix },
    how_to_change: "The network is fixed while this MCP runs. The user changes it in the MCP's configuration (--network mainnet|testnet|local|custom, or BLOCKULA_RPC).",
    explorer: net.rpc,
    mcp_version: VERSION,
    ...s,
  };
});

tool("tx_status", "Check a transaction by id: pending, finalized (with block), and its plain-English summary.", { tx_id: z.string().regex(/^[0-9a-f]{64}$/) }, async ({ tx_id }) => {
  const tx = await (await chain()).tx(tx_id);
  return { status: tx.status, summary: tx.summary, block: tx.height, fee: tx.fee_display, explorer: explorer(`tx/${tx_id}`) };
});

tool("start_dev_node", "Start a local Blockula dev chain (faucet + built-in explorer) on this machine. This MCP uses it only if it was configured for the local network (--network local); otherwise it keeps its network and tells you how to switch. Needs the `blockula` binary (install_node).", { reset: z.boolean().optional().describe("wipe the dev chain first") }, async ({ reset }) => {
  const net = network();
  const target = net.key === "local" ? net.rpc : "http://localhost:7070";
  const using = net.key === "local" ? "This MCP is on the local network, so it uses it now." : `This MCP stays on the ${net.label} (${net.rpc}): the network never changes while it runs. To use the dev chain, the user restarts the MCP with --network local (or BLOCKULA_NETWORK=local).`;
  if (!reset && (await reachable(target))) {
    // Asked for the dev chain explicitly: pin whichever one runs now (a reset makes a new genesis).
    if (net.key === "local") pinned = null;
    return `A node is already running at ${target} (explorer: ${target}). ${using}`;
  }
  const port = new URL(target).port || "7070";
  mkdirSync(HOME, { recursive: true });
  const log = openSync(join(HOME, "dev-node.log"), "a");
  let missing = false;
  const child = spawn(BIN, ["dev", "--port", port, ...(reset ? ["--reset"] : [])], { detached: true, stdio: ["ignore", log, log] });
  child.on("error", (e: NodeJS.ErrnoException) => {
    missing = e.code === "ENOENT";
  });
  child.unref();
  for (let i = 0; i < 40 && !missing; i++) {
    await new Promise((r) => setTimeout(r, 250));
    if (await reachable(target)) {
      // A reset dev chain has a new genesis: re-pin on the next call.
      if (net.key === "local") pinned = null;
      return `🧛 Dev chain is up. Explorer: ${target} · logs: ${join(HOME, "dev-node.log")}. ${using}`;
    }
  }
  if (missing) {
    throw new Error(`The \`${BIN}\` node binary isn't installed, so a local chain can't start. Call install_node to download the prebuilt node (checksum-verified, plus provenance or signature checks where available), then start_dev_node again.`);
  }
  throw new Error(`node didn't start. See ${join(HOME, "dev-node.log")}`);
});

tool(
  "install_node",
  "Download the prebuilt `blockula` node + CLI for this machine from the official GitHub release and install it to ~/.blockula/bin, after the same checks as scripts/install.sh: SHA-256 against the release checksums, build provenance with a logged-in GitHub CLI, and the offline ML-DSA-65 release signature when the release key id is known. It says which checks passed; with BLOCKULA_REQUIRE_VERIFY=1 it refuses a release it can only checksum. Needed for start_dev_node and summon_chain; nothing else needs it.",
  { version: z.string().regex(/^(latest|v\d+\.\d+\.\d+)$/).default("latest") },
  async ({ version }) => {
    const target = releaseTarget();
    if (!target) throw new Error(`No prebuilt node for ${process.platform}/${process.arch}. Build from source: https://github.com/${REPO}`);
    const base = version === "latest" ? `https://github.com/${REPO}/releases/latest/download` : `https://github.com/${REPO}/releases/download/${version}`;
    const asset = `blockula-${target}.tar.gz`;
    const tmp = mkdtempSync(join(tmpdir(), "blockula-"));
    try {
      const rel = await fetchVerifiedRelease(base, asset, { releaseKeyId: process.env.BLOCKULA_RELEASE_KEY_ID?.trim().toLowerCase() || RELEASE_KEY_ID, requireVerify: process.env.BLOCKULA_REQUIRE_VERIFY === "1" }, {
        async download(url) {
          const res = await fetch(url, { redirect: "follow" });
          if (res.status === 404) return null;
          if (!res.ok) throw new Error(`download failed (${res.status}): ${url}`);
          return Buffer.from(await res.arrayBuffer());
        },
        async attest(archive, name) {
          if (spawnSync("gh", ["auth", "status"], { stdio: "ignore" }).status !== 0) return { result: "unavailable" };
          const file = join(tmp, `attest-${name}`);
          writeFileSync(file, archive);
          const r = spawnSync("gh", ["attestation", "verify", file, "--repo", REPO, "--signer-workflow", `${REPO}/.github/workflows/release.yml`], { encoding: "utf8" });
          return r.status === 0 ? { result: "ok" } : { result: "failed", detail: r.stderr || r.stdout };
        },
      });
      writeFileSync(join(tmp, asset), rel.archive);
      execFileSync("tar", ["-xzf", join(tmp, asset), "-C", tmp]);
      mkdirSync(join(HOME, "bin"), { recursive: true });
      copyFileSync(join(tmp, `blockula-${target}`, "blockula"), INSTALLED_BIN);
      chmodSync(INSTALLED_BIN, 0o755);
      BIN = INSTALLED_BIN;
      const installed = execFileSync(INSTALLED_BIN, ["--version"], { encoding: "utf8" }).trim();
      return { installed, path: INSTALLED_BIN, sha256: rel.sha256, verified: rel.verified.length ? rel.verified : ["checksum only"], ...(rel.warning ? { warning: rel.warning } : {}), next: "start_dev_node runs a local chain; summon_chain creates your own network" };
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  },
);

tool("summon_chain", "Create a brand-new Blockula chain (fresh genesis + validator key) that you can run as your own network.", { chain_id: z.string().regex(/^[a-z0-9-]{3,40}$/), faucet: z.boolean().default(true) }, async ({ chain_id, faucet }) => {
  const out = execFileSync(BIN, ["summon", chain_id, ...(faucet ? [] : ["--no-faucet"])], { encoding: "utf8" });
  return out.trim();
});

tool("create_wallet", "Create this agent's post-quantum (ML-DSA-65) wallet, stored locally. Returns the existing one if it already exists.", {}, async () => {
  let w = await loadWallet();
  const existed = !!w;
  if (!w) {
    w = Wallet.generate(await chain());
    await w.client.addressPrefix();
    mkdirSync(HOME, { recursive: true });
    writeFileSync(WALLET_FILE, JSON.stringify({ address: w.address, key_id: w.keyId, seed: w.seedHex }, null, 2), { mode: 0o600 });
  }
  await w.client.addressPrefix();
  return {
    key_id: w.keyId,
    own_address: w.address,
    acting_for: w.account,
    existed,
    network: network().label,
    stored_at: WALLET_FILE,
    next_steps: "To spend from the user's account: ask them to open the Blockula web wallet → Devices & agents → 'Give an agent a budget' and paste this key_id. Then call use_account with their .boo name. (On test chains, faucet funds this key's own address for testing.)",
  };
});

tool("use_account", "Act for a user's account (name or address) that has authorized this agent's key — usually as a session key with a spending limit. Pass 'self' to go back to the key's own address.", { account: z.string() }, async ({ account }) => {
  const f = readWalletFile();
  if (!f) throw new Error("No agent wallet yet — call create_wallet first.");
  const client = await chain();
  const w = (await loadWallet())!;
  if (account === "self") {
    delete f.account;
  } else {
    const addr = await client.resolve(account);
    const { accounts } = await client.keyAccounts(w.keyId);
    const grant = accounts.find((a: { account: { address: string } }) => a.account.address === addr);
    if (!grant) throw new Error(`${account} hasn't authorized this key yet. Ask the user to add key_id ${w.keyId} in their wallet (Devices & agents).`);
    f.account = addr;
    writeFileSync(WALLET_FILE, JSON.stringify(f, null, 2), { mode: 0o600 });
    return { acting_for: grant.account.name ?? addr, role: grant.role, label: grant.label };
  }
  writeFileSync(WALLET_FILE, JSON.stringify(f, null, 2), { mode: 0o600 });
  await client.addressPrefix();
  return { acting_for: w.address };
});

tool("wallet_info", "This agent's address, balance, .boo names and recent activity.", {}, async () => {
  const w = await requireWallet();
  await w.client.addressPrefix();
  const a = await w.client.account(w.account).catch(() => null);
  const mine = a?.keys?.find((k: { key_id: string }) => k.key_id === w.keyId);
  return { key_id: w.keyId, network: network().label, acting_for: a?.account?.name ?? w.account, my_permissions: mine ?? "not authorized on this account", address: w.account, balance: a?.balance_display ?? "0 FANG", names: a?.names ?? [], recent: (a?.recent_txs ?? []).slice(0, 10).map((t: { summary: string; id: string }) => `${t.summary} (${t.id.slice(0, 12)}…)`) };
});

tool("faucet", "Get 100 test FANG on a dev/test chain (to this agent's wallet, or any name/address). Mainnet has no faucet.", { to: z.string().optional() }, async ({ to }) => {
  const client = await chain();
  let target = to;
  if (!target) {
    const w = await requireWallet();
    await w.client.addressPrefix();
    target = w.account;
  }
  const tx = await client.faucet(target);
  return { summary: tx.summary, tx_id: tx.id, explorer: explorer(`tx/${tx.id}`) };
});

tool("send", "Send FANG to a .boo name or address. Amounts are in FANG, e.g. \"1.5\".", { to: z.string(), amount: z.string(), memo: z.string().max(256).optional() }, async ({ to, amount, memo }) => {
  const drops = parseFang(amount);
  checkBudget(drops);
  const tx = await (await requireWallet()).send(to, amount, { memo });
  recordSpend(drops + BigInt(tx.fee));
  return { summary: tx.summary, fee: tx.fee_display, tx_id: tx.id, block: tx.height, explorer: explorer(`tx/${tx.id}`) };
});

tool("check_name", "Check whether a .boo name is available and what it costs, or who owns it and when it expires.", { name: z.string() }, async ({ name }) => (await chain()).name(name));

tool("register_name", "Claim a .boo name for this agent's wallet (3–32 chars, a-z 0-9 -). Shorter names cost more; names are registered for a year at a time.", { name: z.string() }, async ({ name }) => {
  const tx = await (await requireWallet()).registerName(name);
  return { summary: tx.summary, tx_id: tx.id, explorer: explorer(`tx/${tx.id}`) };
});

tool("renew_name", "Extend a .boo name the account this agent acts for owns, by 1–10 years (paid at the chain's renewal price per year, which is burned). Names that aren't renewed expire after a grace period and anyone can register them.", { name: z.string(), years: z.number().int().min(1).max(10).default(1) }, async ({ name, years }) => {
  const w = await requireWallet();
  const info = await w.client.name(name).catch(() => null);
  const perYear = info?.renewal_price ?? info?.renew_price;
  const cost = /^\d+$/.test(String(perYear ?? "")) ? BigInt(perYear) * BigInt(years) : 0n;
  checkBudget(cost, "the renewal");
  const tx = await w.renewName(name, years);
  recordSpend(cost + BigInt(tx.fee));
  return { summary: tx.summary, fee: tx.fee_display, ...(info?.expires_at !== undefined ? { expired_at_before: info.expires_at } : {}), explorer: explorer(`tx/${tx.id}`) };
});

tool("lookup", "Look up anything — a .boo name, address, block number/hash or tx id — and get a plain-English explanation.", { query: z.string() }, async ({ query }) => {
  const client = await chain();
  const hit = await client.search(query);
  const data = hit.type === "block" ? await client.block(hit.id) : hit.type === "tx" ? await client.tx(hit.id) : hit.type === "account" ? await client.account(hit.id) : await client.name(hit.id);
  return { type: hit.type, explorer: explorer(`${hit.type}/${hit.id}`), ...data };
});

tool("latest_blocks", "The most recent blocks, summarized.", { limit: z.number().int().min(1).max(50).default(10) }, async ({ limit }) => {
  const blocks = await (await chain()).blocks(limit);
  return blocks.map((b: { summary: string; hash: string }) => `${b.summary} · ${b.hash.slice(0, 12)}…`);
});

// ---- staking ----

tool("stake", "Stake (delegate) FANG from the account this agent acts for in a validator's pool. Staked FANG earns a share of issuance, is slashed if the validator misbehaves, and takes the unbonding period (21 days on mainnet) to come back after unstake. Session keys can't stake. Counts toward BLOCKULA_MAX_SEND / BLOCKULA_MAX_DAILY.", { validator: z.string().describe("validator address (or a .boo name its account holds)"), amount: z.string().describe("FANG, e.g. \"10\"") }, async ({ validator, amount }) => {
  const drops = parseFang(amount);
  checkBudget(drops);
  const tx = await (await requireWallet()).delegate(validator, drops);
  recordSpend(drops + BigInt(tx.fee));
  return { summary: tx.summary, fee: tx.fee_display, next: "my_stake shows the stake and its value", explorer: explorer(`tx/${tx.id}`) };
});

tool("unstake", "Start unbonding stake from a validator's pool: an amount in FANG (converted to pool shares at the current rate) or \"all\". It stops earning now and can be withdrawn with withdraw_stake once the unbonding period is over.", { validator: z.string(), amount: z.string().default("all").describe("FANG, or \"all\"") }, async ({ validator, amount }) => {
  const w = await requireWallet();
  const v = await w.client.resolve(validator);
  let shares: bigint | "all" = "all";
  if (amount !== "all") {
    const d = stakeSummary(await moduleRead("staking", w.client.staking(w.account))).delegations.find((x) => x.validator === v);
    if (!d) throw new Error(`${w.account} has no stake with ${validator}`);
    shares = sharesFor(d, amount);
    if (shares <= 0n) throw new Error(`${amount} FANG is less than one share of this pool`);
  }
  const tx = await w.undelegate(v, shares);
  return { summary: tx.summary, fee: tx.fee_display, explorer: explorer(`tx/${tx.id}`) };
});

tool("withdraw_stake", "Pay out every unbonding entry of the account this agent acts for that has finished unbonding.", {}, async () => {
  const tx = await (await requireWallet()).withdrawUnbonded();
  return { summary: tx.summary, fee: tx.fee_display, explorer: explorer(`tx/${tx.id}`) };
});

tool("my_stake", "The stake of the account this agent acts for: each pool (validator, shares, value), unbonding entries and when they complete, and what can be withdrawn now.", {}, async () => {
  const w = await requireWallet();
  await w.client.addressPrefix();
  const [raw, status] = await Promise.all([moduleRead("staking", w.client.staking(w.account)), w.client.status()]);
  const s = stakeSummary(raw, status.height);
  return {
    account: w.account,
    height: status.height,
    staked: formatFang(BigInt(s.bonded)),
    unbonding: formatFang(BigInt(s.unbonding_total)),
    withdrawable_now: s.withdrawable !== undefined ? formatFang(BigInt(s.withdrawable)) : "unknown",
    pools: s.delegations.map((d) => ({ validator: d.validator_name ?? d.validator, address: d.validator, shares: d.shares, value: formatFang(BigInt(d.value)), ...(d.commission_bps !== undefined ? { commission: `${d.commission_bps / 100}%` } : {}), ...(d.jailed ? { jailed: true } : {}) })),
    unbonding_entries: s.unbonding.map((u) => ({ validator: u.validator, amount: formatFang(BigInt(u.amount)), completes_at_block: u.completes_at })),
  };
});

// ---- governance ----

const proposalView = (p: ProposalSummary) => ({
  id: p.id,
  title: p.title,
  does: p.summary,
  status: p.status,
  proposer: p.proposer_name ?? p.proposer,
  aye: formatFang(BigInt(p.aye)),
  nay: formatFang(BigInt(p.nay)),
  ...(p.turnout !== undefined ? { turnout: formatFang(BigInt(p.turnout)) } : {}),
  ...(p.quorum !== undefined ? { quorum: formatFang(BigInt(p.quorum)) } : {}),
  ...(p.council ? { council: `${p.council.ayes} of ${p.council.of} validators approve${p.council.nays ? `, ${p.council.nays} reject` : ""}` } : {}),
  ...(p.voting_ends_at !== undefined ? { voting_ends_at_block: p.voting_ends_at } : {}),
  ...(p.executes_at !== undefined ? { executes_at_block: p.executes_at } : {}),
  ...(p.url ? { details: p.url } : {}),
  explorer: explorer(`proposal/${p.id}`),
});

tool("list_proposals", "On-chain governance proposals (parameter changes, treasury spends, upgrades, distributions …): what each does, its status, token-weighted tallies, the bootstrap validator council's votes, and when voting ends and the timelock lets it execute.", { status: z.string().optional().describe("e.g. \"voting\"; default: all, newest first"), limit: z.number().int().min(1).max(50).default(20) }, async ({ status, limit }) => {
  const res = await moduleRead("governance proposals", (await chain()).proposals({ status, limit }));
  const list = (Array.isArray(res) ? res : (res.proposals ?? [])) as unknown[];
  return list.slice(0, limit).map((p) => proposalView(proposalSummary(p)));
});

tool("vote", "Vote on a governance proposal for the account this agent acts for. `amount` FANG of its unlocked holdings is locked: conviction 0 locks nothing and counts 0.1×; 1–6 lock for 1, 2, 4, 8, 16 or 32 lock periods (a week each on mainnet) after voting ends and count 1×–6×. Voting again replaces the vote. Session keys can't vote. Ask the user before locking their FANG.", { proposal_id: z.number().int().min(0), vote: z.enum(["aye", "nay"]), amount: z.string().describe("FANG to lock"), conviction: z.number().int().min(0).max(6).default(1) }, async ({ proposal_id, vote, amount, conviction }) => {
  const drops = parseFang(amount);
  if (drops > MAX_SEND) throw new Error(`amount exceeds this agent's per-send limit (BLOCKULA_MAX_SEND=${process.env.BLOCKULA_MAX_SEND ?? "100"} FANG)`);
  const tx = await (await requireWallet()).vote(proposal_id, vote === "aye", drops, conviction);
  recordSpend(BigInt(tx.fee));
  return { summary: tx.summary, fee: tx.fee_display, explorer: explorer(`proposal/${proposal_id}`) };
});

// ---- Hunts and the contribution pool ----

tool("list_hunts", `Open bounties (Hunts) on GitHub issues, with FANG escrowed on-chain. Each links to an issue with acceptance criteria: implement it, open a PR, and the bounty is awarded on-chain when it merges. ${NO_VALUE}`, { status: z.enum(["open", "awarded", "cancelled", "expired"]).default("open") }, async ({ status }) => {
  const res = await (await chain()).hunts(status);
  const hunts = (Array.isArray(res) ? res : res.hunts ?? []) as { id: number; title: string; amount_display: string; issue_url: string; expires_at: number; funders?: unknown[] }[];
  return { hunts: hunts.map((h) => ({ hunt_id: h.id, bounty: h.amount_display, title: h.title, issue: h.issue_url, funders: h.funders?.length ?? 0, explorer: explorer(`hunt/${h.id}`) })), note: NO_VALUE };
});

tool("link_github", "Link a GitHub handle to the account this agent acts for, so Hunt awards show up on its profile. First create a PUBLIC gist (as that GitHub user) containing the account's address, then pass the gist id here.", { handle: z.string().regex(/^[A-Za-z0-9-]{1,39}$/), gist_id: z.string().regex(/^[0-9a-f]{20,40}$/) }, async ({ handle, gist_id }) => {
  const w = await requireWallet();
  const tx = await w.linkGithub(handle, gist_id);
  const check = await w.client.github(handle).catch(() => null);
  return { summary: tx.summary, verified: check?.verified ?? false, explorer: explorer(`tx/${tx.id}`) };
});

tool("fund_hunt", `Add FANG to an open Hunt's bounty (crowdfunding). Refunded to you if the hunt is cancelled. ${NO_VALUE}`, { hunt_id: z.number().int().min(0), amount: z.string() }, async ({ hunt_id, amount }) => {
  const drops = parseFang(amount);
  checkBudget(drops);
  const tx = await (await requireWallet()).fundHunt(hunt_id, amount);
  recordSpend(drops + BigInt(tx.fee));
  return { summary: tx.summary, explorer: explorer(`hunt/${hunt_id}`) };
});

tool("contribution_pool", `Blockula's contribution pool (Proof of Contribution): its balance, which GitHub repos earn from it, the rules, and the Séance (review jury) that's open now. Merged PRs to pool repos can be claimed for a share; payouts are judged by human juries, square-rooted per sponsor, and vest over ~90 days. ${NO_VALUE}`, {}, async () => {
  const c = await (await chain()).contributions();
  return {
    pool: c.pool.balance_display,
    next_seance_pays_up_to: c.pool.next_budget_display,
    repos: c.repos,
    claims: c.claims,
    open_seance: c.open_seance ? { id: c.open_seance.id, phase: c.open_seance.phase, claims: c.open_seance.claims.length, jurors: c.open_seance.jurors.length, reveals_until_block: c.open_seance.reveal_until } : null,
    rules: c.rules,
    how_to_earn: "Get a PR merged into a pool repo's default branch, authored by the GitHub handle linked to the account you act for (link_github), then submit_contribution. Validators check the merge on GitHub; a jury compares it with other claims.",
    note: NO_VALUE,
    explorer: explorer("contribute"),
  };
});

tool("submit_contribution", `Claim a merged PR for the contribution pool, for the account this agent acts for. The PR's author must be that account's linked GitHub handle. Costs a small refundable bond (lost if the jury marks it spam). If you act for a user through a session key, they must first register that key as a Familiar (wallet.registerFamiliar), or the chain refuses the claim; the claim then credits you as their Familiar. ${NO_VALUE}`, { repo: z.string().regex(/^[A-Za-z0-9-]{1,39}\/[A-Za-z0-9._-]{1,100}$/), pr: z.number().int().min(1) }, async ({ repo, pr }) => {
  const w = await requireWallet();
  const me = await w.client.account(w.account).catch(() => null);
  const bond = BigInt(me?.contribution?.next_claim_bond ?? "1000000000");
  checkBudget(bond, `the claim bond (${formatFang(bond)})`);
  const tx = await w.submitClaim(repo, pr);
  recordSpend(bond + BigInt(tx.fee));
  const claim = (await w.client.claims({ account: w.account })).claims.find((c: { repo: string; pr: number }) => c.repo === repo.toLowerCase() && c.pr === pr);
  return { summary: tx.summary, claim_id: claim?.id, status: claim?.status, next: "Validators check GitHub within a minute or two; then it waits for the revert window and the next Séance. Track it with my_contributions.", note: NO_VALUE, explorer: explorer(`claim/${claim?.id ?? ""}`) };
});

tool("my_contributions", `The Soul (reputation), contribution claims, vesting rewards, vouch and jury status of the account this agent acts for. ${NO_VALUE}`, {}, async () => {
  const w = await requireWallet();
  await w.client.addressPrefix();
  const a = await w.client.account(w.account);
  const p = a.contribution;
  return {
    account: a.account.name ?? a.account.address,
    soul: p.soul.display,
    vouched_by: p.vouched_by?.voucher?.name ?? p.vouched_by?.voucher?.short ?? null,
    juror: p.juror,
    rewards: { withdrawable: p.rewards.withdrawable_display, still_vesting: p.rewards.unvested_display },
    claims: p.claims.map((c: { id: number; repo: string; pr: number; status: string; score_display?: string; payout_display: string; reason?: string }) => ({ id: c.id, pr: `${c.repo}#${c.pr}`, status: c.status, score: c.score_display, earned: c.payout_display, note: c.reason })),
    next_claim_bond: p.next_claim_bond_display,
    note: NO_VALUE,
  };
});

if (NET_ERROR) console.error(`blockula-mcp: ${NET_ERROR.message}`);
await server.connect(new StdioServerTransport());
