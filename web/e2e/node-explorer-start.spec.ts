// The node's built-in explorer (node/src/explorer.html), served as the node serves it, against a mocked
// freshly made chain: Start (#/start) is a task list filled in with this node's origin and the
// chain's id and genesis hash, ticks are kept per chain, steps the chain shows done tick themselves,
// a friend's invite link opens a welcome, and Learn draws the chain's tokenomics from its parameters.
import { readFileSync } from "node:fs";
import path from "node:path";
import { expect, test, type Page } from "@playwright/test";

const ORIGIN = "https://my-node.blockula.test";
const HTML = readFileSync(path.join(__dirname, "../../node/src/explorer.html"), "utf8");
const HASH = "158f5fc57712e5148b572fd8f52b294287d419e6571e557ebd2359c1810e397e";
const VALIDATOR = "boo1qrwpfntxrs2u9dqlevlgrujjmchfxp5y6qh5qyju2s53zucs4pgq4vps32";
const TREASURY = "boo1wgpg29zsxm82xmrtqs3g286hdx36c2zsy8zpfhgtsd7k67nxz34qzv3qn5";

type World = { name?: string; faucet?: boolean; validators?: number; programs?: boolean; txs?: boolean; hunts?: number };

/** A chain made with `blockula create --name "Jack in the Block" --token JACK`, a block in. */
function api(p: string, w: World): unknown {
  if (p === "/status") return { name: w.name ?? "Jack in the Block", token: "JACK", chain_id: "jack-in-the-block", genesis_hash: HASH, faucet: w.faucet ?? true, height: 3, block_time_ms: 5000, epoch: 0, active_validators: w.validators ?? 1, mempool: 0, supply: "1100000000000000000", supply_display: "1100000000 JACK", burned: "65200", burned_display: "0.0000652 JACK", accounts: w.txs ? 3 : 2, names: 0, head_age_ms: 500, history_from: 1, validators: [{ address: VALIDATOR, name: "🧛 validator", short: "boo1qrwpfn…4vps32" }] };
  if (p === "/genesis") return { chain_id: "jack-in-the-block", allocations: [{ address: VALIDATOR, amount: "1000000000000000000" }, { address: TREASURY, amount: "100000000000000000", label: "treasury" }], validators: [{ address: VALIDATOR }] };
  if (p === "/fees") return { account_creation_fee: "100000000", account_creation_fee_display: "0.1 JACK", suggested: { flat: "10000", rate: "10000" }, tip_cap_multiple: 20, ...(w.programs ? { fuel: { status: "on", state_bytes: 591 } } : {}) };
  if (p === "/params") return { params: { account_creation_fee: "100000000", issuance_initial_ppm: "60000", issuance_decay_ppm: "150000", issuance_floor_ppm: "15000", issuance_stakers_bps: "7000", issuance_pool_bps: "2000", tip_cap_multiple: "20" }, genesis: { block_time_ms: 5000, epoch_blocks: 120, epochs_per_year: 52596 }, issuance: { year: 0, rate_ppm: 0, base_supply: "0", per_epoch: "0" }, supply: "1100000000000000000", burned: "65200" };
  if (p.startsWith("/blocks")) return [{ height: 3, tx_count: 0, timestamp_ms: Date.now(), proposer: null }];
  if (p.startsWith("/validators")) return null;
  if (p.startsWith("/hunts")) return { count: w.hunts ?? 0, hunts: [], open_bounties_display: "0 JACK" };
  if (p === "/contributions") return { pool: { balance_display: "0 JACK", next_budget_display: "0 JACK" }, repos: [], jurors: { active: 0 }, open_seance: null, recent_seances: [], next_seance_at: 10, rules: {} };
  if (p.startsWith("/claims")) return { claims: [], count: 0 };
  return null;
}

async function open(page: Page, hash: string, w: World = {}) {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.route(`${ORIGIN}/`, (r) => r.fulfill({ contentType: "text/html", body: HTML }));
  await page.route(`${ORIGIN}/api/**`, (r) => {
    const u = new URL(r.request().url());
    if (u.pathname === "/api/health") return r.fulfill({ contentType: "application/json", body: JSON.stringify({ ok: true }) });
    const body = api(u.pathname.replace(/^\/api/, "") + u.search, w);
    return body === null ? r.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "not found" }) }) : r.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  });
  await page.route(/fonts\.(googleapis|gstatic)\.com/, (r) => r.abort());
  await page.route(/\/haunt\.js$/, (r) => r.fulfill({ contentType: "text/javascript", body: "" }));
  await page.goto(`${ORIGIN}/${hash}`);
  return errors;
}

const task = (page: Page, title: string | RegExp) => page.locator("li.task", { has: page.locator("h3", { hasText: title }) });

test("Start is a task list with this node's commands, and hides what a fresh chain doesn't have", async ({ page }) => {
  const errors = await open(page, "#/start");
  const app = page.locator("#app");
  await expect(app.locator("h1")).toHaveText("Get going on Jack in the Block");
  await expect(app.locator("#start-progress")).toHaveText("0 of 8 done");
  await expect(task(page, "Get a wallet")).toContainText(`blockula wallet new --rpc ${ORIGIN}`);
  await expect(task(page, "Get 100 JACK")).toContainText(`blockula wallet faucet --rpc ${ORIGIN}`);
  await expect(task(page, "Send your first transaction")).toContainText(`blockula wallet send friend.boo 1 --memo "hello" --rpc ${ORIGIN}`);
  await expect(task(page, "Make a friend a validator")).toContainText(`blockula validator init --home ~/.blockula/jack-in-the-block-validator --genesis ${ORIGIN} --genesis-hash ${HASH}`);
  await expect(task(page, "Connect Claude")).toContainText(`claude mcp add blockula -e BLOCKULA_RPC=${ORIGIN} -e BLOCKULA_CHAIN_ID=jack-in-the-block -e BLOCKULA_GENESIS_HASH=${HASH} -- npx -y @blockula/mcp`);
  // Programs are off: no checkbox, and how a chain gets them.
  const programs = task(page, "Programs are off on this chain");
  await expect(programs).toContainText("blockula create --programs");
  await expect(programs.locator("input")).toHaveCount(0);
  await expect(task(page, "Make it yours").getByRole("link", { name: "Change anything with Claude Code →" })).toHaveAttribute("href", "https://github.com/jackdishman/blockula/blob/main/docs/BUILD-ON-IT.md");
  // The invite link, and the paste-in prompt for a coding agent.
  await expect(app.locator(".invite pre").first()).toHaveText(`${ORIGIN}/#/start/invite`);
  const prompt = app.locator("section", { has: page.locator("h2", { hasText: "For your coding agent" }) }).locator("pre");
  await expect(prompt).toContainText(`BLOCKULA_GENESIS_HASH=${HASH}`);
  await expect(prompt).toContainText("Call create_wallet, then faucet.");
  await expect(prompt).toContainText("Call list_hunts");
  await expect(prompt).toContainText("read AGENTS.md and CONTRIBUTING.md");
  await expect(app).toContainText("What people use a chain like this for");
  // A fresh chain has no Hunts and no pool: their nav links go; the pages stay.
  await expect(page.locator('nav a[href="#/hunts"]')).toBeHidden();
  await expect(page.locator('nav a[href="#/contribute"]')).toBeHidden();
  await expect(page.locator('nav a[href="#/start"]')).toHaveAttribute("aria-current", "");
  await expect(app).not.toContainText(/testnet|mainnet/i);
  expect(errors).toEqual([]);
});

test("ticks are kept per chain, and the home page shows the progress", async ({ page }) => {
  await open(page, "#/start");
  const box = task(page, "Get a wallet").locator("input[type=checkbox]");
  await box.check();
  await expect(page.locator("#start-progress")).toHaveText("1 of 8 done");
  await expect(task(page, "Get a wallet")).toHaveClass(/done/);
  await page.reload();
  await expect(task(page, "Get a wallet").locator("input[type=checkbox]")).toBeChecked();
  await expect(page.locator("#start-progress")).toHaveText("1 of 8 done");
  expect(await page.evaluate((h) => localStorage.getItem(`blockula-start:${h}`), HASH)).toBe('["wallet"]');
  await page.goto(`${ORIGIN}/#/`);
  await expect(page.locator("#app")).toContainText("1 of 8 steps done");
  await expect(page.getByRole("link", { name: "Start here →" })).toHaveAttribute("href", "#/start");
});

test("steps the chain shows done tick themselves, and programs are a step once they're on", async ({ page }) => {
  await open(page, "#/start", { validators: 2, programs: true, txs: true });
  await expect(page.locator("#start-progress")).toHaveText("3 of 9 done");
  for (const t of ["Send your first transaction", "Make a friend a validator", "Run a program"]) {
    await expect(task(page, t).locator("input")).toBeChecked();
    await expect(task(page, t).locator("input")).toBeDisabled();
    await expect(task(page, t)).toContainText("the chain shows it");
  }
  await expect(task(page, "Run a program")).toContainText(`blockula program upload node/vm/tests/programs/counter.wasm --interface node/vm/tests/programs/counter.iface.json --rpc ${ORIGIN}`);
});

test("Copy puts the command on the clipboard", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"], { origin: ORIGIN });
  await open(page, "#/start");
  const step = task(page, "Get 100 JACK");
  await step.getByRole("button", { name: "Copy" }).click();
  await expect(step.getByRole("button")).toHaveText("Copied");
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(`blockula wallet faucet --rpc ${ORIGIN}`);
});

test("a friend's invite link welcomes them with a wallet, coins and the join command", async ({ page }) => {
  await open(page, "#/start/invite");
  const welcome = page.locator("#app .invite").first();
  await expect(welcome.locator("h1")).toHaveText("You've been invited to Jack in the Block");
  await expect(welcome).toContainText(`blockula wallet new --rpc ${ORIGIN}\nblockula wallet faucet --rpc ${ORIGIN}`);
  await expect(welcome).toContainText(`blockula join --genesis ${ORIGIN} --genesis-hash ${HASH} --home ~/.blockula/jack-in-the-block`);
  await expect(welcome).toContainText("Its JACK has no value");
});

test("a chain without a faucet says to ask for coins", async ({ page }) => {
  await open(page, "#/start/invite", { faucet: false });
  await expect(page.locator("#app .invite").first()).toContainText("ask whoever invited you");
  await expect(task(page, "Get some JACK")).toContainText("no faucet");
});

test("the chain's name is text, never markup, in Start and its prompt", async ({ page }) => {
  const name = '<img src=x id="pwned">';
  const errors = await open(page, "#/start", { name });
  await expect(page.locator("#app h1")).toHaveText(`Get going on ${name}`);
  await expect(page.locator("#pwned")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("Learn draws the chain's tokenomics from its parameters", async ({ page }) => {
  await open(page, "#/learn/tokenomics");
  const tok = page.locator("#tokenomics");
  await expect(tok.locator("h2")).toHaveText("How JACK works");
  await expect(tok.locator("svg")).toHaveCount(4);
  await expect(tok).toContainText("1.1B JACK");
  await expect(tok).toContainText("treasury: 100M JACK (9.09%)");
  await expect(tok).toContainText("base fee: burned, gone for good");
  await expect(tok).toContainText("6% a year");
  await expect(tok).toContainText("stakers: 70%");
  await expect(tok).toContainText("contribution pool: 20%");
  await expect(tok).toContainText("treasury: 10%");
  await expect(tok).toContainText("100 JACK");
  await expect(tok).toContainText("This chain's JACK has no value; it only prices spam.");
  // No Hunts on this chain: Learn doesn't send people to them.
  await expect(page.locator("#app")).not.toContainText("Useful work can be paid");
});
