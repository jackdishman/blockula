#!/usr/bin/env node
// 🧛 Blockula MCP server. Add to Claude Code with:
//   claude mcp add blockula -- npx -y @blockula/mcp
//
// Env:
//   BLOCKULA_RPC        node URL (default: a local node on :7070 if one is running, else the public testnet)
//   BLOCKULA_HOME       where the agent wallet lives (default ~/.blockula)
//   BLOCKULA_MAX_SEND   max FANG per send() call (default 100)
//   BLOCKULA_MAX_DAILY  max FANG this MCP sends per UTC day, across calls (default 100)
//   BLOCKULA_BIN        path to the `blockula` binary (default: ~/.blockula/bin/blockula from install_node, else PATH)
// When acting for a user's account via a session key, the chain also enforces that key's limit.

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { Client, Wallet, formatFang, parseFang } from "@blockula/sdk";
import { spawn, execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, openSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { z } from "zod";

const LOCAL_RPC = "http://localhost:7070";
const TESTNET_RPC = "https://blockula-testnet.fly.dev";
const HOME = process.env.BLOCKULA_HOME ?? join(homedir(), ".blockula");
const WALLET_FILE = join(HOME, "agent-wallet.json");
const MAX_SEND = parseFang(process.env.BLOCKULA_MAX_SEND ?? "100");
const MAX_DAILY = parseFang(process.env.BLOCKULA_MAX_DAILY ?? "100");
const SPEND_FILE = join(HOME, "agent-spend.json");
const VERSION: string = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8")).version;
const REPO = "jackdishman/blockula";
const INSTALLED_BIN = join(HOME, "bin", "blockula");
let BIN = process.env.BLOCKULA_BIN ?? (existsSync(INSTALLED_BIN) ? INSTALLED_BIN : "blockula");

function releaseTarget(): string | null {
  const key = `${process.platform}-${process.arch}`;
  return ({ "darwin-arm64": "aarch64-apple-darwin", "darwin-x64": "x86_64-apple-darwin", "linux-x64": "x86_64-unknown-linux-gnu", "linux-arm64": "aarch64-unknown-linux-gnu" } as Record<string, string>)[key] ?? null;
}

async function download(url: string): Promise<Buffer> {
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) throw new Error(`download failed (${res.status}): ${url}`);
  return Buffer.from(await res.arrayBuffer());
}

async function reachable(url: string) {
  try {
    await new Client(url).status();
    return true;
  } catch {
    return false;
  }
}

// No configuration needed: use a local node if one is running, otherwise the public testnet.
let RPC = process.env.BLOCKULA_RPC ?? ((await reachable(LOCAL_RPC)) ? LOCAL_RPC : TESTNET_RPC);
let client = new Client(RPC);
function useRpc(url: string) {
  RPC = url;
  client = new Client(url);
}

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

const server = new McpServer({ name: "blockula", version: VERSION });

const text = (v: unknown) => ({ content: [{ type: "text" as const, text: typeof v === "string" ? v : JSON.stringify(v, null, 2) }] });

interface WalletFile { address: string; key_id: string; seed: string; account?: string }

function readWalletFile(): WalletFile | null {
  return existsSync(WALLET_FILE) ? JSON.parse(readFileSync(WALLET_FILE, "utf8")) : null;
}

function loadWallet(): Wallet | null {
  const f = readWalletFile();
  if (!f) return null;
  const w = Wallet.fromSeed(f.seed, client);
  if (f.account) w.account = f.account;
  return w;
}

function requireWallet(): Wallet {
  const w = loadWallet();
  if (!w) throw new Error("No agent wallet yet — call create_wallet first.");
  return w;
}

function tool<S extends z.ZodRawShape>(name: string, description: string, shape: S, run: (args: z.infer<z.ZodObject<S>>) => Promise<unknown>) {
  server.registerTool(name, { description, inputSchema: shape }, (async (args: z.infer<z.ZodObject<S>>) => {
    try {
      return text(await run(args));
    } catch (e) {
      const msg = (e as Error).message;
      const hint = /fetch failed|ECONNREFUSED/.test(msg) ? ` (no node at ${RPC} — call start_dev_node)` : "";
      return { ...text(`Error: ${msg}${hint}`), isError: true };
    }
  }) as never);
}

tool("status", "Blockula network status: which node you're connected to, chain id, height, supply, fees burned, and the crypto in use.", {}, async () => ({
  rpc: RPC,
  network: RPC === TESTNET_RPC ? "public testnet" : RPC === LOCAL_RPC ? "local node" : "custom node",
  explorer: RPC,
  mcp_version: VERSION,
  ...(await client.status()),
}));

tool("tx_status", "Check a transaction by id: pending, finalized (with block), and its plain-English summary.", { tx_id: z.string().regex(/^[0-9a-f]{64}$/) }, async ({ tx_id }) => {
  const tx = await client.tx(tx_id);
  return { status: tx.status, summary: tx.summary, block: tx.height, fee: tx.fee_display, explorer: `${RPC}/#/tx/${tx_id}` };
});

tool("start_dev_node", "Start a local Blockula dev chain (faucet + built-in explorer) and switch to it. Needs the `blockula` binary; without it, keep using the public testnet, which supports everything except running your own chain.", { reset: z.boolean().optional().describe("wipe the dev chain first") }, async ({ reset }) => {
  const target = process.env.BLOCKULA_RPC ?? LOCAL_RPC;
  if (!reset && (await reachable(target))) {
    useRpc(target);
    return `Already running at ${target} (explorer: ${target})`;
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
      useRpc(target);
      return `🧛 Dev chain is up. Explorer: ${target} · logs: ${join(HOME, "dev-node.log")}`;
    }
  }
  if (missing) {
    throw new Error(`The \`${BIN}\` node binary isn't installed, so a local chain can't start. You're connected to ${RPC}${RPC === TESTNET_RPC ? " (the public testnet)" : ""}, which works for wallets, the faucet, names and sends. Call install_node to download the prebuilt node (checksum-verified), then start_dev_node again.`);
  }
  throw new Error(`node didn't start. See ${join(HOME, "dev-node.log")}`);
});

tool("install_node", "Download the prebuilt `blockula` node + CLI for this machine from the official GitHub release, verify its SHA-256 checksum, and install it to ~/.blockula/bin. Needed for start_dev_node and summon_chain; nothing else needs it.", { version: z.string().regex(/^(latest|v\d+\.\d+\.\d+)$/).default("latest") }, async ({ version }) => {
  const target = releaseTarget();
  if (!target) throw new Error(`No prebuilt node for ${process.platform}/${process.arch}. Build from source: https://github.com/${REPO}`);
  const base = version === "latest" ? `https://github.com/${REPO}/releases/latest/download` : `https://github.com/${REPO}/releases/download/${version}`;
  const asset = `blockula-${target}.tar.gz`;
  const [tarball, sums] = await Promise.all([download(`${base}/${asset}`), download(`${base}/checksums.txt`)]);
  const expected = sums.toString("utf8").split("\n").map((l) => l.trim().split(/\s+/)).find(([, f]) => f === asset)?.[0];
  const actual = createHash("sha256").update(tarball).digest("hex");
  if (!expected || expected !== actual) throw new Error(`checksum mismatch for ${asset}; not installing`);
  const tmp = mkdtempSync(join(tmpdir(), "blockula-"));
  try {
    writeFileSync(join(tmp, asset), tarball);
    execFileSync("tar", ["-xzf", join(tmp, asset), "-C", tmp]);
    mkdirSync(join(HOME, "bin"), { recursive: true });
    copyFileSync(join(tmp, `blockula-${target}`, "blockula"), INSTALLED_BIN);
    chmodSync(INSTALLED_BIN, 0o755);
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
  BIN = INSTALLED_BIN;
  const installed = execFileSync(INSTALLED_BIN, ["--version"], { encoding: "utf8" }).trim();
  return { installed, path: INSTALLED_BIN, sha256: actual, next: "start_dev_node runs a local chain; summon_chain creates your own network" };
});

tool("summon_chain", "Create a brand-new Blockula chain (fresh genesis + validator key) that you can run as your own network.", { chain_id: z.string().regex(/^[a-z0-9-]{3,40}$/), faucet: z.boolean().default(true) }, async ({ chain_id, faucet }) => {
  const out = execFileSync(BIN, ["summon", chain_id, ...(faucet ? [] : ["--no-faucet"])], { encoding: "utf8" });
  return out.trim();
});

tool("create_wallet", "Create this agent's post-quantum (ML-DSA-65) wallet, stored locally. Returns the existing one if it already exists.", {}, async () => {
  let w = loadWallet();
  const existed = !!w;
  if (!w) {
    w = Wallet.generate(client);
    mkdirSync(HOME, { recursive: true });
    writeFileSync(WALLET_FILE, JSON.stringify({ address: w.address, key_id: w.keyId, seed: w.seedHex }, null, 2), { mode: 0o600 });
  }
  return {
    key_id: w.keyId,
    own_address: w.address,
    acting_for: w.account,
    existed,
    stored_at: WALLET_FILE,
    next_steps: "To spend from the user's account: ask them to open the Blockula web wallet → Devices & agents → 'Give an agent a budget' and paste this key_id. Then call use_account with their .boo name. (Or use faucet to fund this key's own address for testing.)",
  };
});

tool("use_account", "Act for a user's account (name or address) that has authorized this agent's key — usually as a session key with a spending limit. Pass 'self' to go back to the key's own address.", { account: z.string() }, async ({ account }) => {
  const f = readWalletFile();
  if (!f) throw new Error("No agent wallet yet — call create_wallet first.");
  const w = loadWallet()!;
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
  return { acting_for: w.address };
});

tool("wallet_info", "This agent's address, balance, .boo names and recent activity.", {}, async () => {
  const w = requireWallet();
  const a = await client.account(w.account).catch(() => null);
  const mine = a?.keys?.find((k: { key_id: string }) => k.key_id === w.keyId);
  return { key_id: w.keyId, acting_for: a?.account?.name ?? w.account, my_permissions: mine ?? "not authorized on this account", address: w.account, balance: a?.balance_display ?? "0 FANG", names: a?.names ?? [], recent: (a?.recent_txs ?? []).slice(0, 10).map((t: { summary: string; id: string }) => `${t.summary} (${t.id.slice(0, 12)}…)`) };
});

tool("faucet", "Get 100 test FANG on a dev/test chain (to this agent's wallet, or any name/address).", { to: z.string().optional() }, async ({ to }) => {
  const target = to ?? requireWallet().account;
  const tx = await client.faucet(target);
  return { summary: tx.summary, tx_id: tx.id, explorer: `${RPC}/#/tx/${tx.id}` };
});

tool("send", "Send FANG to a .boo name or boo1… address. Amounts are in FANG, e.g. \"1.5\".", { to: z.string(), amount: z.string(), memo: z.string().max(256).optional() }, async ({ to, amount, memo }) => {
  const drops = parseFang(amount);
  if (drops > MAX_SEND) throw new Error(`amount exceeds this agent's per-send limit (BLOCKULA_MAX_SEND=${process.env.BLOCKULA_MAX_SEND ?? "100"} FANG)`);
  const today = spentToday();
  if (today.spent + drops > MAX_DAILY) {
    throw new Error(`this would exceed the agent's daily limit (BLOCKULA_MAX_DAILY=${process.env.BLOCKULA_MAX_DAILY ?? "100"} FANG; ${formatFang(today.spent)} already sent today)`);
  }
  const tx = await requireWallet().send(to, amount, { memo });
  mkdirSync(HOME, { recursive: true });
  writeFileSync(SPEND_FILE, JSON.stringify({ day: today.day, spent: (today.spent + drops + BigInt(tx.fee)).toString() }), { mode: 0o600 });
  return { summary: tx.summary, fee: tx.fee_display, tx_id: tx.id, block: tx.height, explorer: `${RPC}/#/tx/${tx.id}` };
});

tool("check_name", "Check whether a .boo name is available and what it costs.", { name: z.string() }, ({ name }) => client.name(name));

tool("register_name", "Claim a .boo name for this agent's wallet (3–32 chars, a-z 0-9 -). Shorter names cost more.", { name: z.string() }, async ({ name }) => {
  const tx = await requireWallet().registerName(name);
  return { summary: tx.summary, tx_id: tx.id, explorer: `${RPC}/#/tx/${tx.id}` };
});

tool("lookup", "Look up anything — a .boo name, boo1… address, block number/hash or tx id — and get a plain-English explanation.", { query: z.string() }, async ({ query }) => {
  const hit = await client.search(query);
  const data = hit.type === "block" ? await client.block(hit.id) : hit.type === "tx" ? await client.tx(hit.id) : hit.type === "account" ? await client.account(hit.id) : await client.name(hit.id);
  return { type: hit.type, explorer: `${RPC}/#/${hit.type}/${hit.id}`, ...data };
});

tool("latest_blocks", "The most recent blocks, summarized.", { limit: z.number().int().min(1).max(50).default(10) }, async ({ limit }) => {
  const blocks = await client.blocks(limit);
  return blocks.map((b: { summary: string; hash: string }) => `${b.summary} · ${b.hash.slice(0, 12)}…`);
});

tool("list_hunts", "Open bounties (Hunts) on GitHub issues, with FANG escrowed on-chain. Each links to an issue with acceptance criteria: implement it, open a PR, and the bounty is awarded on-chain when it merges.", { status: z.enum(["open", "awarded", "cancelled", "expired"]).default("open") }, async ({ status }) => {
  const res = await client.hunts(status);
  const hunts = (Array.isArray(res) ? res : res.hunts ?? []) as { id: number; title: string; amount_display: string; issue_url: string; expires_at: number; funders?: unknown[] }[];
  return hunts.map((h) => ({ hunt_id: h.id, bounty: h.amount_display, title: h.title, issue: h.issue_url, funders: h.funders?.length ?? 0, explorer: `${RPC}/#/hunt/${h.id}` }));
});

tool("link_github", "Link a GitHub handle to the account this agent acts for, so Hunt awards show up on its profile. First create a PUBLIC gist (as that GitHub user) containing the account's boo1 address, then pass the gist id here.", { handle: z.string().regex(/^[A-Za-z0-9-]{1,39}$/), gist_id: z.string().regex(/^[0-9a-f]{20,40}$/) }, async ({ handle, gist_id }) => {
  const tx = await requireWallet().linkGithub(handle, gist_id);
  const check = await client.github(handle).catch(() => null);
  return { summary: tx.summary, verified: check?.verified ?? false, explorer: `${RPC}/#/tx/${tx.id}` };
});

tool("fund_hunt", "Add FANG to an open Hunt's bounty (crowdfunding). Refunded to you if the hunt is cancelled.", { hunt_id: z.number().int().min(0), amount: z.string() }, async ({ hunt_id, amount }) => {
  const drops = parseFang(amount);
  if (drops > MAX_SEND) throw new Error(`amount exceeds this agent's per-send limit (BLOCKULA_MAX_SEND=${process.env.BLOCKULA_MAX_SEND ?? "100"} FANG)`);
  const today = spentToday();
  if (today.spent + drops > MAX_DAILY) throw new Error(`this would exceed the agent's daily limit (BLOCKULA_MAX_DAILY=${process.env.BLOCKULA_MAX_DAILY ?? "100"} FANG)`);
  const tx = await requireWallet().fundHunt(hunt_id, amount);
  writeFileSync(SPEND_FILE, JSON.stringify({ day: today.day, spent: (today.spent + drops + BigInt(tx.fee)).toString() }), { mode: 0o600 });
  return { summary: tx.summary, explorer: `${RPC}/#/hunt/${hunt_id}` };
});

tool("contribution_pool", "Blockula's contribution pool (Proof of Contribution): its balance, which GitHub repos earn from it, the rules, and the Séance (review jury) that's open now. Merged PRs to pool repos can be claimed for a share; payouts are judged by human juries, square-rooted per sponsor, and vest over ~90 days.", {}, async () => {
  const c = await client.contributions();
  return {
    pool: c.pool.balance_display,
    next_seance_pays_up_to: c.pool.next_budget_display,
    repos: c.repos,
    claims: c.claims,
    open_seance: c.open_seance ? { id: c.open_seance.id, phase: c.open_seance.phase, claims: c.open_seance.claims.length, jurors: c.open_seance.jurors.length, reveals_until_block: c.open_seance.reveal_until } : null,
    rules: c.rules,
    how_to_earn: "Get a PR merged into a pool repo's default branch, authored by the GitHub handle linked to the account you act for (link_github), then submit_contribution. Validators check the merge on GitHub; a jury compares it with other claims.",
    explorer: `${RPC}/#/contribute`,
  };
});

tool("submit_contribution", "Claim a merged PR for the contribution pool, for the account this agent acts for. The PR's author must be that account's linked GitHub handle. Costs a small refundable bond (lost if the jury marks it spam). If you act for a user through a session key, they must first register that key as a Familiar (wallet.registerFamiliar), or the chain refuses the claim; the claim then credits you as their Familiar.", { repo: z.string().regex(/^[A-Za-z0-9-]{1,39}\/[A-Za-z0-9._-]{1,100}$/), pr: z.number().int().min(1) }, async ({ repo, pr }) => {
  const w = requireWallet();
  const me = await client.account(w.account).catch(() => null);
  const bond = BigInt(me?.contribution?.next_claim_bond ?? "1000000000");
  if (bond > MAX_SEND) throw new Error(`the claim bond (${formatFang(bond)}) exceeds this agent's per-send limit (BLOCKULA_MAX_SEND)`);
  const today = spentToday();
  if (today.spent + bond > MAX_DAILY) throw new Error(`the claim bond would exceed the agent's daily limit (BLOCKULA_MAX_DAILY)`);
  const tx = await w.submitClaim(repo, pr);
  mkdirSync(HOME, { recursive: true });
  writeFileSync(SPEND_FILE, JSON.stringify({ day: today.day, spent: (today.spent + bond + BigInt(tx.fee)).toString() }), { mode: 0o600 });
  const claim = (await client.claims({ account: w.account })).claims.find((c: { repo: string; pr: number }) => c.repo === repo.toLowerCase() && c.pr === pr);
  return { summary: tx.summary, claim_id: claim?.id, status: claim?.status, next: "Validators check GitHub within a minute or two; then it waits for the revert window and the next Séance. Track it with my_contributions.", explorer: `${RPC}/#/claim/${claim?.id ?? ""}` };
});

tool("my_contributions", "The Soul (reputation), contribution claims, vesting rewards, vouch and jury status of the account this agent acts for.", {}, async () => {
  const w = requireWallet();
  const a = await client.account(w.account);
  const p = a.contribution;
  return {
    account: a.account.name ?? a.account.address,
    soul: p.soul.display,
    vouched_by: p.vouched_by?.voucher?.name ?? p.vouched_by?.voucher?.short ?? null,
    juror: p.juror,
    rewards: { withdrawable: p.rewards.withdrawable_display, still_vesting: p.rewards.unvested_display },
    claims: p.claims.map((c: { id: number; repo: string; pr: number; status: string; score_display?: string; payout_display: string; reason?: string }) => ({ id: c.id, pr: `${c.repo}#${c.pr}`, status: c.status, score: c.score_display, earned: c.payout_display, note: c.reason })),
    next_claim_bond: p.next_claim_bond_display,
  };
});

await server.connect(new StdioServerTransport());
