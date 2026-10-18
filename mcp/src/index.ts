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
//   BLOCKULA_WITNESSES  other nodes of the same chain (comma-separated URLs): .boo names that get
//                       signed (send, stake, unstake, use_account) are proven against them, and the
//                       height each tx is signed against must not be ahead of their heads (nor, in
//                       any case, past what the chain's genesis clock allows). Without
//                       them a send signs the name itself (the chain resolves it when it runs), and
//                       where an address is needed (stake, unstake, use_account) a name is the
//                       node's word: on mainnet (any node on blockula-1, however it was configured)
//                       the address must be confirmed first.
//   BLOCKULA_HOME       where the agent wallet lives (default ~/.blockula)
//   BLOCKULA_MAX_SEND   max FANG one call moves (sent, staked, escrowed, paid for a name, bonded; default 100)
//   BLOCKULA_MAX_DAILY  max FANG leaving the account per UTC day through this MCP, fees included
//                       (default BLOCKULA_MAX_SEND + 1 FANG for fees, so one full-size call fits)
//   BLOCKULA_TIMEOUT_MS how long one request to the node may take (default 15000, at most 2147483647)
//   BLOCKULA_TX_WAIT_MS how long a tx tool waits for finality before answering "pending" (default 15000)
//   BLOCKULA_BIN        path to the `blockula` binary (default: of ~/.blockula/bin/blockula from `blockula release
//                       install` and ~/.blockula/mcp-bin/blockula from install_node, the one of this MCP's version,
//                       the first if both or neither are; else PATH)
//   BLOCKULA_REQUIRE_VERIFY=1   install_node needs provenance too (and refuses a checksum-only release)
//   BLOCKULA_RELEASE_KEY_ID     the release signing key id to trust (until it's built in; it must
//                       agree with the built-in one and with ~/.blockula/release-key-id, if pinned)
// The network never changes while the server runs (WAL-4): see network.ts. Spending caps: see
// spend.ts (reserved before a tx is sent). Chain text is untrusted: see untrusted.ts. When acting
// for a user's account via a session key, the chain also enforces that key's limit.

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { ApiError, Client, HeightAheadError, MAINNET_CHAIN_ID, NAMESPACES, RequestTimeoutError, TxFailedError, TxPendingError, Wallet, bareName, checkTimeoutMs, decodeAccount, describeKind, encodeKeyGrant, isAddress, proposalDigest, formatFang, paramsOf, parseFang, proposalSummary, stakeSummary, txDropped, txId, type Action, type ProposalKind, type ProposalSummary, type Recipient, type Resolution } from "@blockula/sdk";
import { spawn, spawnSync, execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, openSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { z } from "zod";
import { KNOWN_CLOCKS, KNOWN_GENESIS, KnownChains, clockFrom, completePin, effectivePin, mismatch, mismatchError, resolveNetwork, type GenesisClock, type Network } from "./network.js";
import { PIN_FILE, REPO, RELEASE_KEY_ID, binPaths, checkBinaryVersion, fetchVerifiedRelease, latestTagFrom, pickNodeBinary, readReleasePin, releaseKeyIdFrom, type NodeBinary } from "./release.js";
import { SignedTxs, onChain, type SignedTx } from "./signed.js";
import { SpendLedger } from "./spend.js";
import { SERVER_INSTRUCTIONS, UNTRUSTED_NOTE, chainData, clean } from "./untrusted.js";

const HOME = process.env.BLOCKULA_HOME ?? join(homedir(), ".blockula");
const WALLET_FILE = join(HOME, "agent-wallet.json");
const MAX_SEND = parseFang(process.env.BLOCKULA_MAX_SEND ?? "100");
/**
 * Fees count toward BLOCKULA_MAX_DAILY (they leave the account too), so by default the daily cap
 * is the per-call cap plus this much for fees: one call of BLOCKULA_MAX_SEND fits in a day, with
 * room for the fees of many more small txs (audit review-fix-clients-3).
 */
const DAILY_FEE_ALLOWANCE = parseFang("1");
const MAX_DAILY = process.env.BLOCKULA_MAX_DAILY ? parseFang(process.env.BLOCKULA_MAX_DAILY) : MAX_SEND + DAILY_FEE_ALLOWANCE;
const MAX_DAILY_TEXT = process.env.BLOCKULA_MAX_DAILY ? `BLOCKULA_MAX_DAILY=${process.env.BLOCKULA_MAX_DAILY} FANG` : `BLOCKULA_MAX_DAILY, by default BLOCKULA_MAX_SEND + 1 FANG for fees = ${formatFang(MAX_DAILY)}`;
const SPEND_FILE = join(HOME, "agent-spend.json");
const msEnv = (name: string, fallback: number) => {
  const v = process.env[name];
  if (v === undefined || v === "") return fallback;
  if (!/^\d+$/.test(v)) throw new Error(`${name} must be a positive number of milliseconds`);
  return checkTimeoutMs(Number(v), name);
};
const TIMEOUT_MS = msEnv("BLOCKULA_TIMEOUT_MS", 15_000);
const TX_WAIT_MS = msEnv("BLOCKULA_TX_WAIT_MS", 15_000);
const KNOWN_CHAINS = new KnownChains(join(HOME, "known-chains.json"));
const VERSION: string = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8")).version;
// install_node's binary has a path of its own: it never replaces the one `blockula release install`
// checked against the operator's pinned key (audit review-r3-integrate-compose-1). Which one runs is
// decided at each use, by version (nodeBinary).
const { mcp: INSTALLED_BIN, cli: CLI_BIN } = binPaths(HOME);

/** What a binary's `--version` prints, or undefined if it doesn't run. */
function versionOf(path: string): string | undefined {
  try {
    return execFileSync(path, ["--version"], { encoding: "utf8", timeout: 10_000, stdio: ["ignore", "pipe", "ignore"] }).trim();
  } catch {
    return undefined;
  }
}

/** The node binary to run now: BLOCKULA_BIN, else the installed one of this MCP's version (pickNodeBinary). */
function nodeBinary(): NodeBinary & { matches?: boolean } {
  if (process.env.BLOCKULA_BIN) return { path: process.env.BLOCKULA_BIN };
  return pickNodeBinary([CLI_BIN, INSTALLED_BIN].filter((p) => existsSync(p)).map((path) => ({ path, version: versionOf(path) })), VERSION);
}

/** Says which binary ran, and when it isn't this MCP's version. */
const ranWith = (b: NodeBinary & { matches?: boolean }) =>
  `Ran ${b.path}${b.version ? ` (${b.version})` : ""}${b.matches === false && b.path !== "blockula" ? `, not this MCP's version (${VERSION}): install_node v${VERSION} (or \`blockula release install v${VERSION}\`) gets one that is.` : "."}`;

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
/** The chain `pinned` is held to (tx_status reads a signed tx's record only on the chain it was signed for). */
let pinnedChain: { chainId?: string; genesisHash?: string; prefix?: string } = {};

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
  const status = await new Client(net.rpc, { timeoutMs: TIMEOUT_MS }).status();
  const why = mismatch(pin, status);
  if (why) throw mismatchError(net, why, KNOWN_CHAINS.file);
  const full = completePin(pin, status);
  KNOWN_CHAINS.save(net, full);
  const pins = { chainId: full.chainId, genesisHash: full.genesisHash, prefix: full.prefix, timeoutMs: TIMEOUT_MS };
  // Witnesses are held to the same chain; with them, names are proven (Client.resolution) and a
  // height ahead of their heads is refused. The chain's genesis clock bounds the height too.
  const genesisClock = await clockOf(net, full);
  pinned = new Client(net.rpc, { ...pins, genesisClock, witnesses: net.witnesses.map((w) => new Client(w, pins)) });
  pinnedChain = { chainId: full.chainId, genesisHash: full.genesisHash, prefix: full.prefix };
  return pinned;
}

/**
 * The pinned chain's genesis clock, which bounds the height each tx is signed against
 * (`Client.checkSigningHeight`: a node reporting a height the chain can't have reached by this
 * machine's clock gets nothing signed, since it could hold the tx and release it later; audit
 * review-r3-clients-2). Built in with a known genesis; else the one remembered for this chain id
 * and genesis hash; else the node's `/api/genesis`, taken on first use like the genesis hash and
 * remembered with it (but for local dev chains, for this session only). A node that serves none
 * leaves the height its word, as without the clock.
 */
async function clockOf(net: Network, full: { chainId?: string; genesisHash?: string }): Promise<GenesisClock | undefined> {
  const { chainId, genesisHash } = full;
  if (chainId === undefined || genesisHash === undefined) return undefined;
  if (KNOWN_GENESIS[chainId] === genesisHash && KNOWN_CLOCKS[chainId]) return KNOWN_CLOCKS[chainId];
  const remembered = KNOWN_CHAINS.clock(net, chainId, genesisHash);
  if (remembered) return remembered;
  const clock = clockFrom(await new Client(net.rpc, { timeoutMs: TIMEOUT_MS }).genesis().catch(() => undefined), chainId);
  if (clock) KNOWN_CHAINS.saveClock(net, chainId, genesisHash, clock);
  return clock;
}

async function reachable(url: string) {
  try {
    await new Client(url, { timeoutMs: Math.min(TIMEOUT_MS, 5_000) }).status();
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

const LEDGER = new SpendLedger(SPEND_FILE, MAX_DAILY, MAX_DAILY_TEXT);
/** The txs this MCP signed (their valid_until, for tx_status): see signed.ts. */
const SIGNED = new SignedTxs(join(HOME, "agent-txs.json"));

/**
 * Whether this is real money: the configured network is mainnet, or the chain the client is
 * pinned to is (audit gap-7-1, R1 bypass): BLOCKULA_RPC alone pointing at a node on blockula-1 is
 * a "custom" network, and gets mainnet's rules all the same. The client is pinned to the chain id
 * of its first status, and refuses any later status that reports another, so this is the chain
 * every signature it makes is for.
 */
async function realMoney(client: Client): Promise<boolean> {
  return network().mainnet || (await client.status()).chain_id === MAINNET_CHAIN_ID;
}

/** What one tool call spends. */
interface Spend {
  /** FANG this call moves out of the account (sent, staked, escrowed, paid for a name, bonded). */
  amount: bigint;
  /** Also leaves the account (opening the recipient's account): counted toward the daily cap. */
  extra?: bigint;
  /** What BLOCKULA_MAX_SEND is checked against, when it isn't `amount` (a vote locks FANG it doesn't spend). */
  perCall?: bigint;
  /** Names the amount in a refusal. */
  what?: string;
  /**
   * How names in fields that need an address were resolved: the SDK refuses an unproven one on
   * mainnet against the very status the tx is signed for (a second check behind `resolved`).
   */
  resolutions?: Resolution[];
}

/** A tx that was submitted (or may have been) but isn't known to be final: never "failed", never to be re-sent blindly. */
interface InFlight {
  /** `dropped`: the node dropped it from its mempool unrun (nothing charged), but another node may still include it. */
  status: "pending" | "unknown" | "dropped";
  tx_id: string;
  valid_until: number;
  counted_toward_daily_limit: string;
  next: string;
  explorer: string;
}

const isInFlight = (r: unknown): r is InFlight => typeof r === "object" && r !== null && "tx_id" in r && ["pending", "unknown", "dropped"].includes((r as InFlight).status);

/**
 * Signs `action`, reserves what it can cost (the spend plus the tx's max fee) against today's
 * budget under its (account, nonce) slot, and only then submits it. Final: the unused part of the
 * max fee is given back and the tx is returned. Everything else stays counted: failed in a block
 * (its fee was charged), dropped, pending, timed out, unknown, and refused too, since the node's
 * refusal is only its word and it may relay the signed tx anyway (audit F68, F72, F74). A refused
 * tx stops counting twice once this MCP signs another tx with its nonce (the next send, after an
 * honest refusal): see spend.ts.
 */
async function submitCapped(w: Wallet, action: Action, spend: Spend, memo = ""): Promise<any> {
  const perCall = spend.perCall ?? spend.amount;
  if (perCall > MAX_SEND) throw new Error(`${spend.what ?? "amount"} (${formatFang(perCall)}) exceeds this agent's per-call limit (BLOCKULA_MAX_SEND=${process.env.BLOCKULA_MAX_SEND ?? "100"} FANG)`);
  const tx = await w.build(action, memo, { resolutions: spend.resolutions ?? [] });
  const id = txId(tx);
  const maxFee = BigInt(tx.fee);
  const slot = { account: tx.from, nonce: tx.nonce };
  const reservation = LEDGER.reserve(spend.amount + (spend.extra ?? 0n) + maxFee, slot);
  const counted = formatFang(LEDGER.held(slot));
  // Before it can reach any node: tx_status reads its valid_until from here (review-fix-clients-2).
  try {
    SIGNED.add(id, { valid_until: tx.valid_until, account: tx.from, nonce: tx.nonce, chain_id: tx.chain_id, genesis_hash: tx.genesis_hash });
  } catch {
    /* informational only: tx_status then says valid_until is unknown */
  }
  const inFlight = (status: InFlight["status"], why: string): InFlight => ({
    status,
    tx_id: id,
    valid_until: tx.valid_until,
    counted_toward_daily_limit: counted,
    next: `${why} It can still execute until block ${tx.valid_until}, so it already counts toward the daily limit. Do NOT send it again: check it with tx_status (tx_id ${id}) and tell the user it isn't confirmed yet.`,
    explorer: explorer(`tx/${id}`),
  });
  try {
    await w.client.submit(tx, false);
  } catch (e) {
    // The node says it refused it. An honest node then never relayed it, but a lying one can say
    // so and relay it anyway, so the reservation stays: the next tx this MCP signs with the same
    // nonce (after a real refusal, the node's next nonce didn't move) takes the slot over.
    if (e instanceof ApiError && e.status >= 400 && e.status < 500) {
      throw new Error(`${e.message} (the node refused tx ${id}; it still counts toward today's limit until another tx reuses its nonce ${tx.nonce}, since a node could refuse a tx and relay it anyway)`);
    }
    // Admitted, but the first read of its status failed: wait for it below like any other.
    if (!(e instanceof TxPendingError)) return inFlight("unknown", `The node didn't answer clearly (${clean(String((e as Error)?.message ?? e), 200)}), so the tx (${id}) may or may not have arrived.`);
  }
  try {
    const done = await w.client.waitForTx(id, TX_WAIT_MS, tx.valid_until);
    // Only a whole number of drops below the max fee gives anything back (audit gap-7-4).
    const charged = /^\d+$/.test(String(done.fee ?? "")) ? BigInt(done.fee) : maxFee;
    if (charged < maxFee) LEDGER.release(reservation, maxFee - charged);
    return done;
  } catch (e) {
    // Dropped from this node's mempool unrun: nothing was charged, but it isn't dead (audit review-fix-clients-2).
    if (e instanceof TxFailedError && e.dropped) return inFlight("dropped", `This node dropped tx ${id} from its mempool without running it (${clean(e.reason, 200)}), so nothing was charged yet, but another node may still include it.`);
    if (e instanceof TxFailedError) throw new Error(`tx ${id} failed${typeof e.tx?.height === "number" ? ` in block ${e.tx.height}` : ""}: ${clean(e.reason, 300)}. It ran, so its fee was charged, and it stays counted toward the daily limit (${counted}).`);
    return inFlight("pending", `Submitted (tx ${id}), but not final after ${Math.round(TX_WAIT_MS / 1000)} s.`);
  }
}

/**
 * What opening `to`'s account would burn on top of a transfer (0 if it exists). `/api/account`
 * answers for any address, with a zero balance and the address's own "original key" when it has
 * no account, so an account counts as existing only by what a missing one can't have: a balance
 * or a nonce. An existing account with neither is counted as new: the cap errs high, never low.
 */
async function openingFee(client: Client, to: string): Promise<bigint> {
  let a: any = null;
  try {
    a = await client.account(to);
  } catch (e) {
    if (!(e instanceof ApiError && e.status === 404)) throw e;
  }
  const exists = (/^\d+$/.test(String(a?.balance ?? "")) && BigInt(a.balance) > 0n) || (Number.isSafeInteger(a?.nonce) && a.nonce > 0);
  return exists ? 0n : reported((await client.fees()).account_creation_fee, "the account creation fee");
}

/** A drops amount the node reports, which must be there for a spend to be priced. */
function reported(v: unknown, what: string): bigint {
  if ((typeof v === "string" && /^\d+$/.test(v)) || (typeof v === "number" && Number.isSafeInteger(v) && v >= 0)) return BigInt(v);
  throw new Error(`the node doesn't report ${what}, so the spend can't be checked against the spending limits; refusing`);
}

/**
 * A name's registration price and yearly renewal price, computed here from the chain's
 * parameters (node/src/params.rs `name_price`, `name_renewal_price`) and never below what
 * `/api/name` quotes: the caps count the higher. Missing prices refuse the spend.
 */
async function namePrices(client: Client, name: string): Promise<{ price: bigint; renewal: bigint; info: any }> {
  const n = bareName(name);
  const [params, info] = await Promise.all([client.params().then(paramsOf), client.name(n)]);
  const key = n.length <= 3 ? "name_price_3" : n.length === 4 ? "name_price_4" : "name_price_5";
  const own = reported(params[key], `the name price parameter ${key}`);
  const ownRenewal = (own * reported(params.name_renewal_bps, "the name_renewal_bps parameter")) / 10_000n;
  const quoted = reported(info?.price, "the name's price");
  const quotedRenewal = reported(info?.renewal_price, "the name's renewal price");
  return { price: quoted > own ? quoted : own, renewal: quotedRenewal > ownRenewal ? quotedRenewal : ownRenewal, info };
}

/**
 * A name or address resolved for a field that needs an address (staking's validator, the account
 * use_account acts for; audit gap-7-1). With BLOCKULA_WITNESSES a name is proven against them
 * (`Client.resolution`). Without, it's the node's word: on mainnet that's not enough to sign real
 * money to, so the tool answers with the address for the user to confirm instead (the agent then
 * passes the address); elsewhere it signs and says it's unproven. Payments don't come here: they
 * sign the name (`Client.recipient`), which the chain resolves itself.
 */
async function resolved(client: Client, input: string, role: string): Promise<{ res: Resolution; confirm?: undefined } | { res?: undefined; confirm: Record<string, unknown> }> {
  const res = await client.resolution(input);
  if (!res.proven && (await realMoney(client))) {
    return {
      confirm: {
        status: "not_sent",
        needs_confirmation: true,
        [role]: clean(input, 100),
        resolves_to: res.address,
        resolved_by: clean(res.how, 300),
        next: `Nothing was signed. This is real money, and ${clean(input, 100)} was resolved by one node without proof: a node can point a name anywhere. Show the user the address ${res.address}; only if they confirm it's the right ${role}, call this tool again with that address instead of the name. (With BLOCKULA_WITNESSES configured, names are proven and this step isn't needed.)`,
      },
    };
  }
  return { res };
}

/** What a tool reports about a resolved name: the exact address signed and how it was found. */
const resolvedView = (res: Resolution, role: string) => (res.name === null ? {} : { [`${role}_address`]: res.address, name_resolution: res.proven ? clean(res.how, 300) : `UNPROVEN: ${clean(res.how, 300)}` });

/** What a tool reports about a recipient the chain resolves: what was signed (a proven address, or the name itself) and how. */
const recipientView = (r: Recipient, role: string) =>
  r.name === null ? {} : r.byChain ? { [`${role}_signed_as`]: r.signed, [`${role}_owner_now_unproven`]: r.address, name_resolution: clean(r.how, 400) } : { [`${role}_address`]: r.address, name_resolution: clean(r.how, 300) };

/** A tx tool's answer: the tx's summary and links, or the in-flight notice. */
const txResult = (r: any, extra: Record<string, unknown> = {}) =>
  isInFlight(r) ? r : { summary: clean(String(r.summary ?? "")), fee: r.fee_display, tx_id: r.id, block: r.height, ...extra, explorer: explorer(`tx/${r.id}`) };

const server = new McpServer({ name: "blockula", version: VERSION }, { instructions: SERVER_INSTRUCTIONS });

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
      // Node errors are relayed, so they're cleaned like any chain text.
      const msg = clean(String((e as Error)?.message ?? e), 1000);
      const hint = /fetch failed|ECONNREFUSED/.test(msg)
        ? ` (no node at ${RPC}${NET?.key === "local" ? " — call start_dev_node" : ""})`
        : e instanceof RequestTimeoutError
          ? ` (the node at ${RPC} isn't answering)`
          : e instanceof HeightAheadError
            ? ` (the chain's genesis clock is built in, or remembered in ${KNOWN_CHAINS.file} from the first /api/genesis read for this chain)`
            : "";
      return { ...text(`Error: ${msg}${hint}`), isError: true };
    }
  }) as never);
}

tool("status", "Blockula network status: which network and node you're connected to (and the chain it's pinned to), chain id, height, supply, fees burned, and the crypto in use. The network, its pins and how to change them are this MCP's own facts; `node_reports` is what the node says.", {}, async () => {
  const net = network();
  const client = await chain();
  const s = await client.status();
  // The node's report is nested and picked field by field: it can't override (or add to) what
  // this MCP states about the network (audit F73).
  const pick = ["chain_id", "height", "finalized_height", "syncing", "epoch", "active_validators", "supply_display", "burned_display", "version", "protocol_version", "head_age_ms", "crypto"];
  return {
    network: net.label,
    real_money: net.mainnet || s.chain_id === MAINNET_CHAIN_ID,
    rpc: net.rpc,
    pinned: chainData({ chain_id: s.chain_id, genesis_hash: s.genesis_hash, address_prefix: s.address_prefix }),
    how_to_change: "The network is fixed while this MCP runs. The user changes it in the MCP's configuration (--network mainnet|testnet|local|custom, or BLOCKULA_RPC).",
    explorer: net.rpc,
    mcp_version: VERSION,
    node_reports: chainData(Object.fromEntries(pick.filter((k) => k in s).map((k) => [k, s[k]]))),
  };
});

/** What a node's head says about a tx this MCP signed but the node can't show (it doesn't know it, or only that it dropped it). */
interface SlotFate {
  /** The node's height, and the first block it holds (`history_from`), if it said. */
  head?: number;
  historyFrom?: number;
  /** How far ahead a valid_until may be (`max_tx_validity_blocks`): it ran no earlier than valid_until minus this. */
  maxValidity?: number;
  /** The account's executed nonce count (`nonce`, never `next_nonce`, which counts the node's mempool too). */
  accountNonce?: number;
  /** With BLOCKULA_WITNESSES: accountNonce is proven (the account record's Merkle proof, its state root confirmed by them) as of block `height`. */
  proven?: { height: number; witnesses: number };
  /** Why the witnesses didn't confirm the account's nonce (then it's left unknown). */
  proofError?: string;
  /** Another tx this MCP signed with the same (account, nonce) that the node shows in a block. */
  sibling?: { id: string; status: string; block?: number };
  /**
   * The tx itself, read again once its nonce showed used (audit review-r4-wallet-1): `ran`, the
   * node now shows it in a block (it executed between the first read and the nonce read); `absent`,
   * still in no block (404, or only its drop); unset, the read failed or caught the node mid-update.
   */
  recheck?: { ran: Record<string, unknown> } | "absent";
}

const safeInt = (v: unknown) => (Number.isSafeInteger(v) && (v as number) >= 0 ? (v as number) : undefined);

/**
 * Reads the head and the account's nonce for a tx this MCP signed (audit review-r3-wallet-1): at
 * most one tx per (account, nonce) executes, so an executed nonce past the tx's means it (or a tx
 * that took its nonce) ran, and it can never run again; a nonce still unused at a block past
 * valid_until means it can never run at all. The status is read first, so a nonce read after a
 * head past valid_until can't miss this tx running. With BLOCKULA_WITNESSES the nonce is proven
 * (as of the proof's block), not this node's word.
 */
async function slotFate(client: Client, id: string, mine: SignedTx): Promise<SlotFate> {
  const fate: SlotFate = {};
  try {
    const st = await client.status();
    [fate.head, fate.historyFrom, fate.maxValidity] = [safeInt(st.height), safeInt(st.history_from), safeInt(st.max_tx_validity_blocks)];
  } catch {
    /* unknown: answered conservatively below */
  }
  if (client.witnesses.length) {
    try {
      if (!isAddress(mine.account)) throw new Error(`${mine.account} isn't an address`);
      const r = await client.provenRecord(NAMESPACES.ACCOUNTS, mine.account, { witnesses: client.witnesses });
      // No record: provably no account, so nothing from it ever ran.
      fate.accountNonce = r.value === null ? 0 : decodeAccount(r.value).nonce;
      fate.proven = { height: r.height, witnesses: client.witnesses.length };
      fate.head = Math.max(fate.head ?? 0, r.head);
    } catch (e) {
      fate.proofError = clean(String((e as Error)?.message ?? e), 200);
    }
  } else {
    try {
      fate.accountNonce = safeInt((await client.account(mine.account)).nonce);
    } catch {
      /* unknown */
    }
  }
  if (fate.accountNonce !== undefined && fate.accountNonce > mine.nonce) {
    // The first read (that didn't find it in a block) predates the nonce read: the block it ran in
    // may have executed in between. Only a read after the nonce read can say it's in none of the
    // blocks this node holds.
    try {
      const t = await client.tx(id);
      if (t.status === "finalized" || (t.status === "failed" && !txDropped(t))) {
        fate.recheck = { ran: t };
        return fate;
      }
      if (t.status === "failed") fate.recheck = "absent";
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) fate.recheck = "absent";
    }
    for (const other of SIGNED.sameSlot(mine, id).slice(-3)) {
      const t = await client.tx(other).catch(() => null);
      if (t && (t.status === "finalized" || (t.status === "failed" && !txDropped(t)))) {
        fate.sibling = { id: other, status: String(t.status), ...(safeInt(t.height) !== undefined ? { block: safeInt(t.height) } : {}) };
        break;
      }
    }
  }
  return fate;
}

/**
 * tx_status's answer for a tx this MCP signed that the node doesn't show in a block: `how` says
 * whether the node doesn't know it at all (`unknown`: pruned, never held, or never reached it) or
 * remembers dropping it unrun (`dropped`). Says it's in flight only while the head is at or below
 * its valid_until and its nonce is unused; never "isn't confirmed" for a nonce that executed.
 */
function afterTheFact(how: "unknown" | "dropped", tx_id: string, mine: SignedTx, fate: SlotFate): Record<string, unknown> {
  const vu = mine.valid_until;
  const facts = {
    tx_id,
    account: mine.account,
    nonce: mine.nonce,
    valid_until: vu,
    ...(fate.head !== undefined ? { node_height: fate.head } : {}),
    ...(fate.accountNonce !== undefined ? { account_nonce: fate.accountNonce } : {}),
    ...(fate.proven ? { account_nonce_proven_at: fate.proven.height, proven_by_witnesses: fate.proven.witnesses } : {}),
    ...(fate.proofError ? { proof_error: fate.proofError } : {}),
    ...(fate.historyFrom !== undefined ? { history_from: fate.historyFrom } : {}),
  };
  const seen = how === "dropped" ? "This node dropped it from its mempool without running it" : `${RPC} doesn't know this tx`;
  const proof = fate.proven ? ` (proven at block ${fate.proven.height}: its state root confirmed by ${fate.proven.witnesses} other node${fate.proven.witnesses === 1 ? "" : "s"})` : "";
  // The block the nonce was read at: the proof's, else (unproven) the node's head, read just before.
  const nonceAt = fate.proven ? fate.proven.height : fate.head;
  if (fate.accountNonce !== undefined && fate.accountNonce > mine.nonce) {
    // Its nonce was used: by this tx (through another node, or in a block this node no longer
    // holds) or by another one. Either way it's final: it can never execute again.
    let which: string;
    // With witnesses only the nonce is proven: which tx used it (a sibling's status, a 404 and
    // history_from) is still this node's word (audit review-r4-wallet-4).
    const nodeWord = fate.proven ? " Only the nonce is proven: which tx used it is this node's word." : "";
    if (fate.sibling) {
      which = `This MCP signed another tx with the same nonce, ${fate.sibling.id}, which this node shows ${fate.sibling.status === "finalized" ? "finalized" : "failed (its fee was charged)"}${fate.sibling.block !== undefined ? ` in block ${fate.sibling.block}` : ""}: that one used the nonce, so this one never ran and nothing was charged for it. Check that tx (tx_status ${fate.sibling.id}), not this one.${nodeWord}`;
    } else {
      const earliest = fate.maxValidity !== undefined ? Math.max(1, vu - fate.maxValidity) : 1;
      const holdsAll = fate.historyFrom !== undefined && fate.historyFrom <= earliest;
      // "In none of them" needs a read after the nonce read that still didn't find it in a block.
      const lacks = !holdsAll || fate.recheck !== "absent";
      which = lacks
        ? `It may have been this one${how === "dropped" ? ", included by another node after this one dropped it" : holdsAll ? ": asked again after its nonce was read, this node gave no clear answer" : `: this node ${fate.historyFrom !== undefined ? `only holds blocks from #${fate.historyFrom} on` : "may not hold the block it ran in"}, so it may have pruned or never held it`}. Look it up on an archive node (or in ${mine.account}'s history) to see whether this one ran and whether it succeeded.`
        : `This node says it holds every block it could have run in (from #${fate.historyFrom}) and ${how === "dropped" ? "shows it in none of them" : "has no record of it"}, so most likely another tx used that nonce and this one never ran; confirm on another node (or in ${mine.account}'s history).${nodeWord}`;
    }
    return {
      status: "nonce_used",
      ...facts,
      next: `${seen}, but ${mine.account}'s nonce ${mine.nonce} has been used (the account has run ${fate.accountNonce} txs${proof}), so this tx can never execute again: it already ran, or another tx with the same nonce took its place. ${which} Do NOT send it again until you know whether it was this one: if it was, the payment already went through. Tell the user exactly that, not that it isn't confirmed.`,
    };
  }
  if (nonceAt !== undefined && nonceAt > vu && fate.accountNonce !== undefined) {
    return {
      status: "expired",
      ...facts,
      next: fate.proven
        ? `${seen}, and at block ${nonceAt}, past its valid_until (${vu}), ${mine.account}'s nonce ${mine.nonce} was still unused${proof}: it never ran, can never execute, and nothing was charged for it. It stays counted toward today's limit.`
        : `${seen}, and says the chain is at block ${nonceAt}, past its valid_until (${vu}), with ${mine.account}'s nonce ${mine.nonce} still unused: if that's right, it can never execute and nothing was charged. It stays counted toward today's limit; check the height and the account's nonce with another node (or configure BLOCKULA_WITNESSES) before signing it again.`,
    };
  }
  if (fate.head !== undefined && fate.head <= vu && (fate.accountNonce !== undefined || (how === "dropped" && !fate.proofError))) {
    const inFlight = how === "dropped" ? "This node dropped it from its mempool without running it, so nothing was charged, but another node" : `${RPC} doesn't know this tx, but this MCP signed it, and a node that holds it`;
    return {
      status: how,
      ...(how === "dropped" ? { dropped: true } : {}),
      ...facts,
      next: `${inFlight} may still include it until block ${vu} (the chain is at block ${fate.head}). It still counts toward the daily limit. Do NOT send it again before then: check tx_status again, and tell the user it isn't confirmed yet.`,
    };
  }
  // The node (or the witnesses) didn't say enough: the height, or the account's nonce once past valid_until.
  const missing =
    fate.head === undefined
      ? "the chain's height"
      : fate.proofError
        ? `a proven nonce for ${mine.account} (${fate.proofError})`
        : fate.accountNonce !== undefined
          ? `${mine.account}'s nonce as of a block past its valid_until (the proof is from block ${nonceAt}, the chain at block ${fate.head})`
          : `whether ${mine.account}'s nonce ${mine.nonce} was used`;
  return {
    status: how,
    ...(how === "dropped" ? { dropped: true } : {}),
    ...facts,
    next: `${seen}, and this MCP couldn't read ${missing}, so it may still be in flight (until block ${vu}) or have run already. Do NOT send it again: check tx_status again (or ask another node), and tell the user it isn't known yet whether it went through.`,
  };
}

/** A tx the node shows in a block: finalized, or failed (it ran, so its fee was charged). Final either way. */
function inBlock(tx: any, link: string): Record<string, unknown> {
  const status = String(tx.status);
  const summary = clean(String(tx.summary ?? ""));
  const error = tx.error ? { error: clean(String(tx.error)) } : {};
  if (status === "finalized") return { status, summary, ...error, block: tx.height, fee: tx.fee_display, explorer: link };
  return { status, summary, ...error, block: tx.height, fee: tx.fee_display, next: `It ran${typeof tx.height === "number" ? ` in block ${tx.height}` : ""} and failed, so its fee was charged and nothing else happened. That's final.`, explorer: link };
}

/**
 * A tx's status as the node reports it, read the way submitCapped reads it (audit
 * review-fix-clients-2): "finalized" and "failed" (it ran in a block; its fee was charged) are
 * final; "dropped" (this node dropped it from its mempool unrun: nothing charged) and "pending"
 * aren't, since another node may still include it until its valid_until. For a tx this MCP
 * signed, valid_until, account and nonce are what it signed (signed.ts), not the node's word, and
 * one the node can't show is read against the head and the account's nonce (afterTheFact).
 */
tool("tx_status", "Check a transaction by id: finalized (with its block), failed in a block (its fee was charged), pending, or dropped from this node's mempool without running (nothing charged, but another node may still include it until valid_until, so it must not be sent again before then), with its plain-English summary. For a tx this MCP signed that the node can't show: nonce_used (its nonce ran: it can never execute again, and may already have gone through), expired (past valid_until, nonce unused: it never ran), unknown/dropped (still in flight), or other_chain (signed for another network: ask that one).", { tx_id: z.string().regex(/^[0-9a-f]{64}$/) }, async ({ tx_id }) => {
  const client = await chain();
  // A record from another network (BLOCKULA_HOME is shared by all of them) says nothing about this
  // one: its account's nonce and this chain's head are another chain's (audit review-r4-wallet-3).
  const record = SIGNED.get(tx_id);
  const mine = record && onChain(record, pinnedChain) && isAddress(record.account, pinnedChain.prefix) ? record : undefined;
  const link = explorer(`tx/${tx_id}`);
  const until = (v: number | undefined) => (v === undefined ? "its valid_until (unknown here: this MCP didn't sign it)" : `block ${v}`);
  let tx: any;
  try {
    tx = await client.tx(tx_id);
  } catch (e) {
    // This node doesn't know it: pruned or never held (it ran long ago), or never reached it.
    if (e instanceof ApiError && e.status === 404 && mine) {
      const fate = await slotFate(client, tx_id, mine);
      // It ran in a block that executed after the first read: answered as what it is.
      if (fate.recheck && fate.recheck !== "absent") return inBlock(fate.recheck.ran, link);
      return { ...afterTheFact("unknown", tx_id, mine, fate), explorer: link };
    }
    if (e instanceof ApiError && e.status === 404 && record) {
      const signedFor = record.chain_id !== undefined ? `chain ${record.chain_id} (genesis ${record.genesis_hash})` : "a chain it didn't record (it was signed before this MCP kept that)";
      return {
        status: "other_chain",
        tx_id,
        account: record.account,
        nonce: record.nonce,
        valid_until: record.valid_until,
        ...(record.chain_id !== undefined ? { signed_for_chain_id: record.chain_id, signed_for_genesis_hash: record.genesis_hash } : {}),
        next: `This MCP signed it for ${signedFor}, not the network it's on now (${pinnedChain.chainId ?? "unknown"} at ${RPC}), which can't say whether it ran. Do NOT send it again, and don't tell the user it failed or never ran: ask a node of the network it was signed for (run the MCP on that network and call tx_status again).`,
        explorer: link,
      };
    }
    throw e;
  }
  const status = String(tx.status);
  const summary = clean(String(tx.summary ?? ""));
  const error = tx.error ? { error: clean(String(tx.error)) } : {};
  const nodeUntil = Number.isSafeInteger(tx.valid_until) ? (tx.valid_until as number) : undefined;
  const validUntil = mine?.valid_until ?? nodeUntil;
  if (status === "finalized" || (status === "failed" && !txDropped(tx))) return inBlock(tx, link);
  if (status === "failed") {
    // Dropped unrun: the height in the answer is where this node dropped it, not a block it ran in.
    const at = typeof tx.height === "number" ? { dropped_at_height: tx.height } : {};
    if (mine) {
      const fate = await slotFate(client, tx_id, mine);
      // Another node included it after this one dropped it, and this node has run that block since.
      if (fate.recheck && fate.recheck !== "absent") return inBlock(fate.recheck.ran, link);
      const r = afterTheFact("dropped", tx_id, mine, fate);
      return { ...r, ...(r.status === "dropped" ? { dropped: true } : {}), summary, ...error, ...at, explorer: link };
    }
    const head = await client.status().then((st) => safeInt(st.height)).catch(() => undefined);
    const past = validUntil !== undefined && head !== undefined && head > validUntil;
    const next = past
      ? `This node dropped it without running it, and says the chain is at block ${head}, past its valid_until (${validUntil}): if that height is right, it can never execute and nothing was charged. It stays counted toward today's limit; check the height with another node before signing it again.`
      : `This node dropped it from its mempool without running it, so nothing was charged, but another node may still include it until ${until(validUntil)}. It still counts toward the daily limit. Do NOT send it again before then: check tx_status again, and tell the user it isn't confirmed yet.`;
    return { status: "dropped", dropped: true, summary, ...error, ...at, valid_until: validUntil ?? "unknown", ...(head !== undefined ? { node_height: head } : {}), next, explorer: link };
  }
  return {
    status: clean(status),
    summary,
    ...error,
    valid_until: validUntil ?? "unknown",
    ...(status === "pending" ? { next: `Not final yet: it can still execute until ${until(validUntil)}. Do NOT send it again: check tx_status again.` } : {}),
    explorer: link,
  };
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
  const bin = nodeBinary();
  const child = spawn(bin.path, ["dev", "--port", port, ...(reset ? ["--reset"] : [])], { detached: true, stdio: ["ignore", log, log] });
  child.on("error", (e: NodeJS.ErrnoException) => {
    missing = e.code === "ENOENT";
  });
  child.unref();
  for (let i = 0; i < 40 && !missing; i++) {
    await new Promise((r) => setTimeout(r, 250));
    if (await reachable(target)) {
      // A reset dev chain has a new genesis: re-pin on the next call.
      if (net.key === "local") pinned = null;
      return `🧛 Dev chain is up. Explorer: ${target} · logs: ${join(HOME, "dev-node.log")}. ${ranWith(bin)} ${using}`;
    }
  }
  if (missing) {
    throw new Error(`The \`${bin.path}\` node binary isn't installed, so a local chain can't start. Call install_node to download the prebuilt node (checksum-verified, plus provenance or signature checks where available), then start_dev_node again.`);
  }
  throw new Error(`node didn't start. See ${join(HOME, "dev-node.log")}`);
});

tool(
  "install_node",
  "Download the prebuilt `blockula` node + CLI for this machine from the official GitHub release and install it to ~/.blockula/mcp-bin (never over the binary `blockula release install` keeps in ~/.blockula/bin), after the same checks as scripts/install.sh: the release must be the version asked for (`latest` is resolved to its tag first), SHA-256 against the release checksums, build provenance from that tag with a logged-in GitHub CLI, and the offline ML-DSA-65 release signature. Once the release key id is known (pinned in ~/.blockula/release-key-id by `blockula release install`, built in, or BLOCKULA_RELEASE_KEY_ID, which must all agree), a release without a valid signature is refused. It says which checks passed; with BLOCKULA_REQUIRE_VERIFY=1 it also requires provenance and refuses a release it can only checksum. Needed for start_dev_node and summon_chain; nothing else needs it.",
  { version: z.string().regex(/^(latest|v\d+\.\d+\.\d+)$/).default("latest") },
  async ({ version }) => {
    const target = releaseTarget();
    if (!target) throw new Error(`No prebuilt node for ${process.platform}/${process.arch}. Build from source: https://github.com/${REPO}`);
    // The operator's pin (written by `blockula release install`) binds this MCP too.
    const pinFile = join(HOME, PIN_FILE);
    const releaseKeyId = releaseKeyIdFrom(process.env.BLOCKULA_RELEASE_KEY_ID, RELEASE_KEY_ID, readReleasePin(pinFile), pinFile);
    // Every check is bound to one tag: GitHub redirects /releases/latest to /releases/tag/<tag>.
    const tag = version === "latest" ? latestTagFrom((await fetch(`https://github.com/${REPO}/releases/latest`, { method: "HEAD", redirect: "follow", signal: AbortSignal.timeout(60_000) })).url) : version;
    const base = `https://github.com/${REPO}/releases/download/${tag}`;
    const asset = `blockula-${target}.tar.gz`;
    const tmp = mkdtempSync(join(tmpdir(), "blockula-"));
    try {
      const rel = await fetchVerifiedRelease(base, asset, { releaseKeyId, requireVerify: process.env.BLOCKULA_REQUIRE_VERIFY === "1", tag }, {
        async download(url) {
          const res = await fetch(url, { redirect: "follow", signal: AbortSignal.timeout(300_000) });
          if (res.status === 404) return null;
          if (!res.ok) throw new Error(`download failed (${res.status}): ${url}`);
          return Buffer.from(await res.arrayBuffer());
        },
        async attest(archive, name, tag) {
          if (spawnSync("gh", ["auth", "status"], { stdio: "ignore" }).status !== 0) return { result: "unavailable" };
          const file = join(tmp, `attest-${name}`);
          writeFileSync(file, archive);
          const r = spawnSync("gh", ["attestation", "verify", file, "--repo", REPO, "--signer-workflow", `${REPO}/.github/workflows/release.yml`, "--source-ref", `refs/tags/${tag}`], { encoding: "utf8" });
          return r.status === 0 ? { result: "ok" } : { result: "failed", detail: r.stderr || r.stdout };
        },
      });
      writeFileSync(join(tmp, asset), rel.archive);
      execFileSync("tar", ["-xzf", join(tmp, asset), "-C", tmp]);
      // The binary must be the version asked for before it replaces anything.
      const extracted = join(tmp, `blockula-${target}`, "blockula");
      let printed = "";
      try {
        printed = execFileSync(extracted, ["--version"], { encoding: "utf8" });
      } catch {
        // checkBinaryVersion reports it as unrunnable.
      }
      checkBinaryVersion(printed, tag);
      // Atomically: a temporary file beside it, then a rename.
      mkdirSync(dirname(INSTALLED_BIN), { recursive: true });
      const staged = `${INSTALLED_BIN}.new-${process.pid}`;
      try {
        copyFileSync(extracted, staged);
        chmodSync(staged, 0o755);
        renameSync(staged, INSTALLED_BIN);
      } finally {
        rmSync(staged, { force: true });
      }
      const installed = execFileSync(INSTALLED_BIN, ["--version"], { encoding: "utf8" }).trim();
      const runs = nodeBinary();
      const which = runs.path === INSTALLED_BIN ? "they run this one" : `they run ${runs.path}${runs.version ? ` (${runs.version})` : ""}, ${process.env.BLOCKULA_BIN ? "BLOCKULA_BIN" : "the binary of this MCP's version or `blockula release install`'s"}, not this one`;
      return { installed, path: INSTALLED_BIN, sha256: rel.sha256, verified: rel.verified.length ? rel.verified : ["checksum only"], ...(rel.warning ? { warning: rel.warning } : {}), next: `start_dev_node runs a local chain; summon_chain creates your own network (${which})` };
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  },
);

tool("summon_chain", "Create a brand-new Blockula chain (fresh genesis + validator key) that you can run as your own network.", { chain_id: z.string().regex(/^[a-z0-9-]{3,40}$/), faucet: z.boolean().default(true) }, async ({ chain_id, faucet }) => {
  const bin = nodeBinary();
  const out = execFileSync(bin.path, ["summon", chain_id, ...(faucet ? [] : ["--no-faucet"])], { encoding: "utf8" });
  return `${out.trim()}\n${ranWith(bin)}`;
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
    next_steps: "To spend from the user's account: ask which account (their .boo name), then call use_account with it. Until they've authorized this key it answers with a grant_code (this key's signed consent to joining that one account) for them to paste in the Blockula web wallet → Devices & agents → 'Give an agent a budget'. A key id alone can't be added. (On test chains, faucet funds this key's own address for testing.)",
  };
});

tool("use_account", "Act for a user's account (name or address) that has authorized this agent's key — usually as a session key with a spending limit. If it hasn't yet, answers with a grant_code for the user to paste in their wallet (Devices & agents → Give an agent a budget); call again once they have. Pass 'self' to go back to the key's own address. The key's label is untrusted text.", { account: z.string() }, async ({ account }) => {
  const f = readWalletFile();
  if (!f) throw new Error("No agent wallet yet — call create_wallet first.");
  const client = await chain();
  const w = (await loadWallet())!;
  if (account === "self") {
    delete f.account;
  } else {
    // The account is an address every later tx names: on mainnet an unproven name waits for the
    // user to confirm the address (audit gap-7-1).
    const { res, confirm } = await resolved(client, account, "account");
    if (confirm) return confirm;
    const addr = res.address;
    // The account's own key list, not /api/key's list of accounts (which others can pad: audit gap-7-2).
    const a = await client.account(addr);
    const grant = (Array.isArray(a?.keys) ? a.keys : []).find((k: { key_id?: unknown }) => k.key_id === w.keyId);
    if (!grant) {
      // Adding a key needs its consent to the account (audit gap-7-2): this key signs it for this
      // one address, and the user's wallet refuses a code made for any other account.
      return {
        status: "not_authorized_yet",
        account: clean(account, 100),
        address: addr,
        ...(res.name === null ? {} : { name_resolution: res.proven ? clean(res.how, 300) : `UNPROVEN: ${clean(res.how, 300)}` }),
        key_id: w.keyId,
        // The resolution itself, so the SDK refuses it too if it's unproven on mainnet.
        grant_code: encodeKeyGrant(await w.keyGrant(res)),
        next: `Nothing was signed on-chain. Give the user the grant_code (this agent key's consent to joining ${addr} only; it holds no secret and can't spend anything). They paste it in the Blockula web wallet → Devices & agents → 'Give an agent a budget' and choose a limit and an expiry. Then call use_account again.`,
      };
    }
    f.account = addr;
    writeFileSync(WALLET_FILE, JSON.stringify(f, null, 2), { mode: 0o600 });
    return { acting_for: clean(String(a?.account?.name ?? addr)), address: addr, ...(res.name === null ? {} : { name_resolution: res.proven ? clean(res.how, 300) : `UNPROVEN: ${clean(res.how, 300)}` }), role: chainData(grant.role), untrusted_label: clean(String(grant.label ?? ""), 100) };
  }
  writeFileSync(WALLET_FILE, JSON.stringify(f, null, 2), { mode: 0o600 });
  await client.addressPrefix();
  return { acting_for: w.address };
});

tool("wallet_info", "This agent's address, balance, .boo names, recent activity (others' txs naming this account included: their summaries are untrusted text) and today's spending against BLOCKULA_MAX_DAILY.", {}, async () => {
  const w = await requireWallet();
  await w.client.addressPrefix();
  const a = await w.client.account(w.account).catch(() => null);
  const mine = Array.isArray(a?.keys) ? a.keys.find((k: { key_id: string }) => k.key_id === w.keyId) : undefined;
  return {
    key_id: w.keyId,
    network: network().label,
    acting_for: clean(String(a?.account?.name ?? w.account)),
    my_permissions: mine ? chainData(mine) : "not authorized on this account",
    address: w.account,
    balance: clean(String(a?.balance_display ?? "0 FANG")),
    names: chainData(a?.names ?? []),
    recent: (Array.isArray(a?.recent_txs) ? a.recent_txs : []).slice(0, 10).map((t: { summary?: unknown; id?: unknown }) => `${clean(String(t.summary ?? ""), 300)} (${clean(String(t.id ?? "")).slice(0, 12)}…)`),
    spent_today: `${formatFang(LEDGER.spent())} of ${formatFang(MAX_DAILY)} (BLOCKULA_MAX_DAILY; txs that are pending, dropped or refused still count until they're final or their nonce is reused)`,
    note: UNTRUSTED_NOTE,
  };
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
  return { summary: clean(String(tx.summary ?? "")), tx_id: tx.id, explorer: explorer(`tx/${tx.id}`) };
});

tool("send", "Send FANG to a .boo name or address. Amounts are in FANG, e.g. \"1.5\". A name is signed as the name (the chain pays whoever owns it when the tx runs) unless BLOCKULA_WITNESSES proved its owner. Counts toward BLOCKULA_MAX_SEND / BLOCKULA_MAX_DAILY (with the fee, and what opening a new account burns). If it comes back \"pending\", it was submitted: check tx_status, never send it again.", { to: z.string(), amount: z.string(), memo: z.string().max(256).optional() }, async ({ to, amount, memo }) => {
  const drops = parseFang(amount);
  if (drops <= 0n) throw new Error("send more than 0");
  const w = await requireWallet();
  await w.client.addressPrefix();
  // With witnesses a name's proven owner is signed; without, the name itself, which the chain
  // resolves when the tx runs: a lying node can't point it at another account now, though one
  // holding the signed tx could release it later in its validity window (audit gap-7-1,
  // review-r3-clients-2).
  const rc = await w.client.recipient(to);
  const opening = await openingFee(w.client, rc.address);
  const r = await submitCapped(w, { type: "transfer", to: rc.signed, amount: drops.toString() }, { amount: drops, extra: opening }, memo ?? "");
  return isInFlight(r) ? { ...r, ...recipientView(rc, "recipient") } : txResult(r, recipientView(rc, "recipient"));
});

tool("check_name", "Check whether a .boo name is available and what it costs, or who owns it and when it expires.", { name: z.string() }, async ({ name }) => chainData(await (await chain()).name(name)));

tool("register_name", "Claim a .boo name for the account this agent acts for (3–32 chars, a-z 0-9 -). Shorter names cost more (the price is burned); names are registered for a year at a time. The price counts toward BLOCKULA_MAX_SEND / BLOCKULA_MAX_DAILY. Session keys can't register names.", { name: z.string() }, async ({ name }) => {
  const w = await requireWallet();
  const n = bareName(name);
  const { price } = await namePrices(w.client, n);
  return txResult(await submitCapped(w, { type: "register_name", name: n }, { amount: price, what: "the name's price" }), { price: formatFang(price) });
});

tool("renew_name", "Extend a .boo name the account this agent acts for owns, by 1–10 years (paid at the chain's renewal price per year, which is burned; counts toward BLOCKULA_MAX_SEND / BLOCKULA_MAX_DAILY). Names that aren't renewed expire after a grace period and anyone can register them.", { name: z.string(), years: z.number().int().min(1).max(10).default(1) }, async ({ name, years }) => {
  const w = await requireWallet();
  const n = bareName(name);
  const { renewal, info } = await namePrices(w.client, n);
  const cost = renewal * BigInt(years);
  return txResult(await submitCapped(w, { type: "renew_name", name: n, years }, { amount: cost, what: "the renewal" }), { cost: formatFang(cost), ...(typeof info?.expires_at === "number" ? { expired_at_before: info.expires_at } : {}) });
});

const LOOKUP_TYPES = new Set(["block", "tx", "account", "name"]);

tool("lookup", `Look up anything — a .boo name, address, block number/hash or tx id — and get a plain-English explanation. ${UNTRUSTED_NOTE}`, { query: z.string() }, async ({ query }) => {
  const client = await chain();
  const hit = await client.search(query);
  const type = String(hit?.type ?? "");
  const id = String(hit?.id ?? "");
  if (!LOOKUP_TYPES.has(type)) throw new Error(`nothing found for ${clean(query, 100)}`);
  const data = type === "block" ? await client.block(id) : type === "tx" ? await client.tx(id) : type === "account" ? await client.account(id) : await client.name(id);
  // The node's JSON is nested (it can't override `type` or `explorer`), cleaned and labeled.
  return { type, explorer: explorer(`${type}/${encodeURIComponent(id)}`), data: chainData(data), note: UNTRUSTED_NOTE };
});

tool("latest_blocks", "The most recent blocks, summarized.", { limit: z.number().int().min(1).max(50).default(10) }, async ({ limit }) => {
  const blocks = await (await chain()).blocks(limit);
  return (Array.isArray(blocks) ? blocks : []).map((b: { summary?: unknown; hash?: unknown }) => `${clean(String(b.summary ?? ""), 200)} · ${clean(String(b.hash ?? "")).slice(0, 12)}…`);
});

// ---- staking ----

tool("stake", "Stake (delegate) FANG from the account this agent acts for in a validator's pool. Staked FANG earns a share of issuance, is slashed if the validator misbehaves, and takes the unbonding period (21 days on mainnet) to come back after unstake. Session keys can't stake. Counts toward BLOCKULA_MAX_SEND / BLOCKULA_MAX_DAILY.", { validator: z.string().describe("validator address (or a .boo name its account holds)"), amount: z.string().describe("FANG, e.g. \"10\"") }, async ({ validator, amount }) => {
  const drops = parseFang(amount);
  if (drops <= 0n) throw new Error("delegate more than 0");
  const w = await requireWallet();
  const { res, confirm } = await resolved(w.client, validator, "validator");
  if (confirm) return confirm;
  const r = await submitCapped(w, { type: "delegate", validator: res.address, amount: drops.toString() }, { amount: drops, resolutions: [res] });
  return isInFlight(r) ? { ...r, ...resolvedView(res, "validator") } : txResult(r, { ...resolvedView(res, "validator"), next: "my_stake shows the stake and its value" });
});

tool("unstake", "Start unbonding stake from a validator's pool: an amount in FANG (signed as that amount: the chain converts it to pool shares at its own price when the tx runs) or \"all\". It stops earning now and can be withdrawn with withdraw_stake once the unbonding period is over.", { validator: z.string(), amount: z.string().default("all").describe("FANG, or \"all\"") }, async ({ validator, amount }) => {
  const drops = amount === "all" ? null : parseFang(amount);
  if (drops !== null && drops <= 0n) throw new Error("unstake more than 0 FANG, or \"all\"");
  const w = await requireWallet();
  const { res, confirm } = await resolved(w.client, validator, "validator");
  if (confirm) return confirm;
  const v = res.address;
  await w.client.addressPrefix();
  const d = stakeSummary(await moduleRead("staking", w.client.staking(w.account))).delegations.find((x) => x.validator === v);
  if (!d || BigInt(d.shares) === 0n) throw new Error(`${w.account} has no stake with ${clean(validator, 100)}`);
  // An amount is signed as an amount, and the chain converts it at the pool's price when it runs:
  // converting it here took the price from this node's report of the stake, and one that said a
  // 1,000 FANG stake was worth 10 FANG and a drop (or was 100× as many shares) turned "unstake
  // 10" into unbonding all of it (audit gap-7-5). The node's value only words a refusal now.
  if (drops !== null && drops > BigInt(d.value)) throw new Error(`the node reports this stake is worth ${formatFang(BigInt(d.value))}, less than the ${formatFang(drops)} asked for: nothing was signed (the chain refuses more than the stake is worth). To unbond all of it, pass amount "all".`);
  // Only the fee leaves the account (it counts toward the daily limit).
  const action: Action = drops === null ? { type: "undelegate", validator: v, shares: d.shares } : { type: "undelegate", validator: v, shares: "0", amount: drops.toString() };
  const view = drops === null
    ? { ...resolvedView(res, "validator"), unbonding: `all ${d.shares} shares (the node reports they're worth ${formatFang(BigInt(d.value))})` }
    : { ...resolvedView(res, "validator"), unbonding: `${formatFang(drops)} (at least: the chain sells the shares that much buys at the pool's price when the tx runs)` };
  const r = await submitCapped(w, action, { amount: 0n, resolutions: [res] });
  return isInFlight(r) ? { ...r, ...view } : txResult(r, view);
});

tool("withdraw_stake", "Pay out every unbonding entry of the account this agent acts for that has finished unbonding.", {}, async () => {
  return txResult(await submitCapped(await requireWallet(), { type: "withdraw_unbonded" }, { amount: 0n }));
});

tool("my_stake", "The stake of the account this agent acts for: each pool (validator, shares, value), unbonding entries and when they complete, and what can be withdrawn now.", {}, async () => {
  const w = await requireWallet();
  await w.client.addressPrefix();
  const [raw, status] = await Promise.all([moduleRead("staking", w.client.staking(w.account)), w.client.status()]);
  const s = stakeSummary(raw);
  return {
    account: w.account,
    height: status.height,
    summary: s.summary,
    staked: formatFang(BigInt(s.bonded)),
    unbonding: formatFang(BigInt(s.unbonding_total)),
    withdrawable_now: formatFang(BigInt(s.withdrawable)),
    pools: s.delegations.map((d) => ({ validator: d.validator_name ?? d.validator, address: d.validator, shares: d.shares, value: formatFang(BigInt(d.value)), earned: formatFang(BigInt(d.value) > BigInt(d.principal) ? BigInt(d.value) - BigInt(d.principal) : 0n), ...(d.self_bond ? { self_bond: true } : {}) })),
    unbonding_entries: s.unbonding.map((u) => ({ validator: u.validator, amount: formatFang(BigInt(u.amount)), completes_at_block: u.completes_at, ready: u.ready })),
    ...(s.redelegations.length ? { redelegations: s.redelegations.map((r) => ({ from: r.from, to: r.to, amount: formatFang(BigInt(r.amount)), slashable_by_from_until_block: r.completes_at })) } : {}),
  };
});

// ---- governance ----

/**
 * A proposal as the agent sees it. `does` is rendered here from the proposal's kind, the content a
 * vote binds (with its title and link): the node's own summary isn't covered by any digest, so a
 * node could make it say anything (audit gap-7-6), and it isn't relayed. `proposal_digest` is what
 * `vote` needs back: it refuses a proposal that reads differently by then.
 */
const proposalView = (p: ProposalSummary) => ({
  id: p.id,
  untrusted_title: clean(p.title, 120),
  does: clean(describeKind(p.kind, { freeText: false }), 600),
  kind: chainData(p.kind),
  ...(p.digest ? { proposal_digest: p.digest } : { proposal_digest: null, cannot_vote: clean(`${p.digest_error ?? "unknown content"}: vote with the blockula CLI, which reads it exactly`, 600) }),
  status: clean(p.status, 40),
  proposer: clean(p.proposer_name ?? p.proposer ?? "", 100),
  aye_weight: formatFang(BigInt(p.aye)),
  nay_weight: formatFang(BigInt(p.nay)),
  turnout: formatFang(BigInt(p.turnout)),
  ...(p.quorum !== undefined ? { quorum: formatFang(BigInt(p.quorum)), quorum_reached: p.quorum_reached } : {}),
  ...(p.council.ayes || p.council.nays ? { council: `${p.council.ayes} validator${p.council.ayes === 1 ? "" : "s"} approve${p.council.nays ? `, ${p.council.nays} reject` : ""}${p.council.of ? ` (of ${p.council.of})` : ""}` } : {}),
  ...(p.supermajority ? { needs: p.threshold_bps ? `${p.threshold_bps / 100}% of the weight` : "a supermajority" } : {}),
  voting_ends_at_block: p.voting_ends_at,
  ...(p.executes_at !== undefined ? { executes_at_block: p.executes_at } : {}),
  ...(p.outcome ? { outcome: clean(p.outcome, 300) } : {}),
  ...(p.url ? { untrusted_url: clean(p.url, 200) } : {}),
  explorer: explorer(`proposal/${p.id}`),
});

tool("list_proposals", `On-chain governance proposals (parameter changes, treasury spends, upgrades, distributions …): what each does (\`does\`, rendered by this MCP from the proposal's \`kind\`, the content a vote binds: show it to the user with the title), its \`proposal_digest\` (pass it to vote), its status, token-weighted tallies, the bootstrap validator council's votes, and when voting ends and the timelock lets it execute. Titles, links and reasons are written by proposers. ${UNTRUSTED_NOTE}`, { status: z.string().optional().describe("e.g. \"voting\"; default: all, newest first"), limit: z.number().int().min(1).max(50).default(20) }, async ({ status, limit }) => {
  const res = await moduleRead("governance proposals", (await chain()).proposals({ status, limit }));
  const list = (Array.isArray(res.proposals) ? res.proposals : []) as unknown[];
  return { proposals: list.slice(0, limit).map((p) => proposalView(proposalSummary(p, res.params))), note: UNTRUSTED_NOTE };
});

tool("vote", "Vote on a governance proposal for the account this agent acts for. Pass the proposal_digest list_proposals returned with what you showed the user: the vote binds exactly that content, and is refused (nothing signed) if the proposal reads differently now. `amount` FANG of its unlocked holdings is locked: conviction 0 locks nothing and counts 0.1×; 1–6 lock for 1, 2, 4, 8, 16 or 32 lock periods (a week each on mainnet) after voting ends and count 1×–6×. Voting again replaces the vote. Session keys can't vote. Ask the user before locking their FANG.", { proposal_id: z.number().int().min(0), proposal_digest: z.string().regex(/^[0-9a-f]{64}$/).describe("list_proposals' proposal_digest for this proposal: what the user was shown"), vote: z.enum(["aye", "nay"]), amount: z.string().describe("FANG to lock"), conviction: z.number().int().min(0).max(6).default(1) }, async ({ proposal_id, proposal_digest: shown, vote, amount, conviction }) => {
  const drops = parseFang(amount);
  if (drops <= 0n) throw new Error("vote with more than 0");
  // The amount is locked, not spent: it's held to BLOCKULA_MAX_SEND, and only the fee counts toward the daily limit.
  const w = await requireWallet();
  // The vote binds the content list_proposals showed (its digest, passed back here), not a fresh
  // read: a node that showed one thing and answers another now is refused before anything is
  // signed, and on chain the digest must match the stored proposal (audit gap-7-6).
  const p = await w.client.proposal(proposal_id);
  if (!p?.kind || typeof p.kind !== "object") throw new Error(`the node doesn't report what proposal #${proposal_id} does, so the vote can't be bound to it`);
  let proposal_digest: string;
  try {
    proposal_digest = proposalDigest(p.kind as ProposalKind, String(p.title ?? ""), typeof p.url === "string" ? p.url : "");
  } catch (e) {
    throw new Error(`proposal #${proposal_id} holds content this MCP can't read exactly (${clean(String((e as Error).message), 200)}), so it can't bind a vote to it: vote with the blockula CLI`);
  }
  if (proposal_digest !== shown) {
    throw new Error(`proposal #${proposal_id} doesn't read the way it did when it was shown (its digest is now ${proposal_digest}, not ${shown}): nothing was signed. The node may be showing different content to different reads. Call list_proposals again and show the user what it does now before voting.`);
  }
  const voted_on = { untrusted_title: clean(String(p.title ?? ""), 200), does: clean(describeKind(p.kind, { freeText: false }), 600), kind: chainData(p.kind), proposal_digest };
  const r = await submitCapped(w, { type: "vote", proposal_id, aye: vote === "aye", amount: drops.toString(), conviction, proposal_digest }, { amount: 0n, perCall: drops, what: "the vote's amount" });
  return isInFlight(r) ? { ...r, voted_on } : { summary: clean(String(r.summary ?? "")), fee: r.fee_display, tx_id: r.id, voted_on, explorer: explorer(`proposal/${proposal_id}`) };
});

// ---- Hunts and the contribution pool ----

tool("list_hunts", `Open bounties (Hunts) on GitHub issues, with FANG escrowed on-chain. Each links to an issue with acceptance criteria: implement it, open a PR, and the bounty is awarded on-chain when it merges. Titles are written by whoever created the hunt. ${UNTRUSTED_NOTE} ${NO_VALUE}`, { status: z.enum(["open", "awarded", "cancelled", "expired"]).default("open") }, async ({ status }) => {
  const res = await (await chain()).hunts(status);
  const hunts = (Array.isArray(res) ? res : Array.isArray(res?.hunts) ? res.hunts : []) as { id: unknown; title?: unknown; amount_display?: unknown; issue_url?: unknown; funders?: unknown }[];
  const funders = (f: unknown) => (Array.isArray(f) ? f.length : f && typeof f === "object" ? Object.keys(f).length : 0);
  return {
    hunts: hunts.slice(0, 100).map((h) => ({ hunt_id: Number(h.id), bounty: clean(String(h.amount_display ?? ""), 40), untrusted_title: clean(String(h.title ?? ""), 120), issue: clean(String(h.issue_url ?? ""), 200), funders: funders(h.funders), explorer: explorer(`hunt/${Number(h.id)}`) })),
    note: `${UNTRUSTED_NOTE} ${NO_VALUE}`,
  };
});

tool("link_github", "Link a GitHub handle to the account this agent acts for, so Hunt awards show up on its profile. First create a PUBLIC gist (as that GitHub user) containing the account's address, then pass the gist id here.", { handle: z.string().regex(/^[A-Za-z0-9-]{1,39}$/), gist_id: z.string().regex(/^[0-9a-f]{20,40}$/) }, async ({ handle, gist_id }) => {
  const w = await requireWallet();
  const r = await submitCapped(w, { type: "link_github", handle, gist_id }, { amount: 0n });
  if (isInFlight(r)) return r;
  const check = await w.client.github(handle).catch(() => null);
  return { summary: clean(String(r.summary ?? "")), verified: check?.verified === true, explorer: explorer(`tx/${r.id}`) };
});

tool("fund_hunt", `Add FANG to an open Hunt's bounty (crowdfunding). Refunded to you if the hunt is cancelled. ${NO_VALUE}`, { hunt_id: z.number().int().min(0), amount: z.string() }, async ({ hunt_id, amount }) => {
  const drops = parseFang(amount);
  if (drops <= 0n) throw new Error("fund more than 0");
  const r = await submitCapped(await requireWallet(), { type: "fund_hunt", hunt_id, amount: drops.toString() }, { amount: drops });
  return isInFlight(r) ? r : { summary: clean(String(r.summary ?? "")), fee: r.fee_display, tx_id: r.id, explorer: explorer(`hunt/${hunt_id}`) };
});

tool("contribution_pool", `Blockula's contribution pool (Proof of Contribution): its balance, which GitHub repos earn from it, the rules, and the Séance (review jury) that's open now. Merged PRs to pool repos can be claimed for a share; payouts are judged by human juries, rate a sponsor's claims in a Séance together (one pooled win rate; each next a quarter of the one before), and vest over ~90 days. ${NO_VALUE}`, {}, async () => {
  const c = await (await chain()).contributions();
  return {
    pool: chainData(c.pool?.balance_display),
    next_seance_pays_up_to: chainData(c.pool?.next_budget_display),
    repos: chainData(c.repos),
    claims: chainData(c.claims),
    open_seance: c.open_seance ? { id: c.open_seance.id, phase: chainData(c.open_seance.phase), claims: Array.isArray(c.open_seance.claims) ? c.open_seance.claims.length : 0, jurors: Array.isArray(c.open_seance.jurors) ? c.open_seance.jurors.length : 0, reveals_until_block: c.open_seance.reveal_until } : null,
    rules: chainData(c.rules),
    how_to_earn: "Get a PR merged into a pool repo's default branch, authored by the GitHub handle linked to the account you act for (link_github), then submit_contribution. Validators check the merge on GitHub; a jury compares it with other claims.",
    note: NO_VALUE,
    explorer: explorer("contribute"),
  };
});

tool("submit_contribution", `Claim a merged PR for the contribution pool, for the account this agent acts for. The PR's author must be that account's linked GitHub handle. Costs a small refundable bond (lost if the jury marks it spam). If you act for a user through a session key, they must first register that key as a Familiar (wallet.registerFamiliar), or the chain refuses the claim; the claim then credits you as their Familiar. ${NO_VALUE}`, { repo: z.string().regex(/^[A-Za-z0-9-]{1,39}\/[A-Za-z0-9._-]{1,100}$/), pr: z.number().int().min(1) }, async ({ repo, pr }) => {
  const w = await requireWallet();
  await w.client.addressPrefix();
  const me = await w.client.account(w.account).catch(() => null);
  // node/src/poc.rs `claim_bond`: CLAIM_BOND (1 FANG) per claim, more with open claims; never counted below it.
  const quoted = me?.contribution?.next_claim_bond;
  const bond = /^\d+$/.test(String(quoted ?? "")) && BigInt(quoted) > 1_000_000_000n ? BigInt(quoted) : 1_000_000_000n;
  const r = await submitCapped(w, { type: "submit_claim", repo, pr }, { amount: bond, what: `the claim bond (${formatFang(bond)})` });
  if (isInFlight(r)) return r;
  const claims = await w.client.claims({ account: w.account }).catch(() => null);
  const claim = (Array.isArray(claims?.claims) ? claims.claims : []).find((c: { repo: string; pr: number }) => c.repo === repo.toLowerCase() && c.pr === pr);
  return { summary: clean(String(r.summary ?? "")), claim_id: claim?.id, status: claim ? clean(String(claim.status)) : undefined, next: "Validators check GitHub within a minute or two; then it waits for the revert window and the next Séance. Track it with my_contributions.", note: NO_VALUE, explorer: explorer(`claim/${claim?.id ?? ""}`) };
});

tool("my_contributions", `The Soul (reputation), contribution claims, vesting rewards, vouch and jury status of the account this agent acts for. A claim's untrusted_note is written by whoever judged or rejected it. ${NO_VALUE}`, {}, async () => {
  const w = await requireWallet();
  await w.client.addressPrefix();
  const a = await w.client.account(w.account);
  const p = a.contribution ?? {};
  return {
    account: clean(String(a.account?.name ?? a.account?.address ?? w.account)),
    soul: chainData(p.soul?.display),
    vouched_by: chainData(p.vouched_by?.voucher?.name ?? p.vouched_by?.voucher?.short ?? null),
    juror: chainData(p.juror),
    rewards: chainData({ withdrawable: p.rewards?.withdrawable_display, still_vesting: p.rewards?.unvested_display }),
    claims: (Array.isArray(p.claims) ? p.claims : []).map((c: { id: number; repo: string; pr: number; status: string; score_display?: string; payout_display: string; reason?: string }) => ({ id: c.id, pr: clean(`${c.repo}#${c.pr}`, 160), status: clean(String(c.status)), score: chainData(c.score_display), earned: chainData(c.payout_display), ...(c.reason ? { untrusted_note: clean(c.reason, 300) } : {}) })),
    next_claim_bond: chainData(p.next_claim_bond_display),
    note: `${UNTRUSTED_NOTE} ${NO_VALUE}`,
  };
});

if (NET_ERROR) console.error(`blockula-mcp: ${NET_ERROR.message}`);
await server.connect(new StdioServerTransport());
