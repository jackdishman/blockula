// The home checklist is a path of stages that open in order (components/Tutorial). It remembers
// what's done per chain and account: the same key has the same address on a relaunched chain, but
// what it did on the old one isn't done on the new one.
import type { Page } from "@playwright/test";
import { CHAIN_ID, GENESIS, RPC } from "./env";
import { createWallet, expect, test } from "./helpers";

const ALL = ["fang", "name", "send", "receive", "device", "stake", "vote", "program", "invite", "validator", "agent", "coding", "layout", "deploy"];
const BASICS = ["fang", "name", "send", "receive"];
const FURTHER = ["device", "stake", "vote", "program"];
const scopeOf = (account: string) => `${CHAIN_ID}/${GENESIS}/${account}`;

const reopen = async (page: Page) => {
  await page.reload();
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(page.getByText("The account of")).toBeVisible();
};

/** Saves this account's checklist on this chain, as the wallet does, and reopens the wallet. */
const saveProgress = async (page: Page, account: string, state: Record<string, unknown>) => {
  await page.evaluate(({ scope, state }) => localStorage.setItem("blockula.tutorial", JSON.stringify({ v: 2, scopes: { [scope]: state } })), { scope: scopeOf(account), state });
  await reopen(page);
};

const stage = (page: Page, id: string) => page.locator(`[data-stage="${id}"]`);
const task = (page: Page, id: string) => page.locator(`[data-task="${id}"]`);

test("the checklist ignores ticks from another chain or account, and keeps this chain's", async ({ page }) => {
  const created = await createWallet(page);
  const count = page.getByTestId("tutorial-count");
  // The faucet paid this wallet on the way in, and the mock chain has two validators; nothing else is done.
  await expect(count).toHaveText("2 of 13");

  // Left by a wallet from before the checklist was kept per chain: it can't say which chain it was for.
  await page.evaluate((done) => localStorage.setItem("blockula.tutorial", JSON.stringify({ done, viewedExplorer: true })), ALL);
  await reopen(page);
  await expect(count).toHaveText("2 of 13");
  // Everything ticked on an old chain with this same address, and on this chain for another account.
  await page.evaluate(
    ({ done, scopes }) => localStorage.setItem("blockula.tutorial", JSON.stringify({ v: 2, scopes: Object.fromEntries(scopes.map((s) => [s, { done, viewedExplorer: true }])) })),
    { done: ALL, scopes: [`blockula-testnet-1/${"ab".repeat(32)}/${created.account}`, `${CHAIN_ID}/${GENESIS}/boo1someoneelse`] },
  );
  await reopen(page);
  await expect(count).toHaveText("2 of 13");

  // What the chain proved is kept under this chain and account, and so is a skip.
  await task(page, "name").getByRole("button", { name: "Skip for now" }).click();
  await reopen(page);
  const mine = await page.evaluate((scope) => JSON.parse(localStorage.getItem("blockula.tutorial") ?? "{}").scopes?.[scope], scopeOf(created.account));
  expect(mine).toMatchObject({ done: expect.arrayContaining(["fang", "validator"]), skipped: ["name"] });
  await expect(task(page, "name")).toHaveClass(/skipped/);
});

test("stages open in order, the next task opens by itself, and the chain ticks what it can see", async ({ page, node }) => {
  const { account } = await createWallet(page);
  const checklist = page.getByTestId("tutorial");
  // Basics is open, the rest only named: the next stage says it's up next, and none of their tasks show.
  await expect(stage(page, "basics")).toHaveAttribute("aria-current", "step");
  await expect(stage(page, "further")).toContainText("Go further · up next");
  await expect(stage(page, "grow")).not.toContainText("up next");
  await expect(checklist.locator("[data-task]")).toHaveCount(4);
  // Coins came from the faucet, so the name is the one task open, with its one action.
  await expect(task(page, "fang").locator(".check")).toHaveAttribute("aria-label", "Done");
  await expect(task(page, "name").getByRole("link", { name: "Pick a name" })).toBeVisible();
  await expect(checklist.locator(".check-body")).toHaveCount(1);

  // The chain proves the rest of the basics: a name, a payment this account sent, and one it got from someone else.
  node.names.set("vlad", account);
  await expect(task(page, "name").locator(".check")).toHaveAttribute("aria-label", "Done");
  await expect(task(page, "send").getByRole("link", { name: "Send", exact: true })).toHaveAttribute("href", "/send");
  // The faucet's drip isn't a friend's payment.
  await expect(task(page, "receive")).not.toHaveClass(/done/);
  node.add({ kind: "transfer", from: account, to: "boo1friend", amount: 1n, fee: 0n, memo: "hi", summary: "sent 1 FANG" });
  node.add({ kind: "transfer", from: "boo1friend", to: account, amount: 1n, fee: 0n, memo: "back", summary: "got 1 FANG" });

  // Basics folds up, and Go further opens.
  await expect(stage(page, "further")).toHaveAttribute("aria-current", "step");
  await expect(stage(page, "basics")).toHaveClass(/finished/);
  await expect(stage(page, "basics").locator("[data-task]")).toHaveCount(0);
  await expect(stage(page, "grow")).toContainText("up next");
  await stage(page, "basics").getByRole("button", { name: /Basics/ }).click();
  await expect(stage(page, "basics").locator("[data-task]")).toHaveCount(4);
  // Staking says it locks.
  await expect(task(page, "device").getByRole("link", { name: "Make a recovery kit" })).toHaveAttribute("href", "/devices#recovery");
  await task(page, "stake").getByRole("button", { name: /Stake to a validator/ }).click();
  await expect(task(page, "stake")).toContainText("stays locked until the unbonding period ends");

  // A second owner key, a delegation and a vote tick themselves.
  node.add({ kind: "delegate", from: account, to: null, amount: 1n, fee: 0n, memo: "", summary: "staked" });
  node.add({ kind: "vote", from: account, to: null, amount: null, fee: 0n, memo: "", summary: "voted" });
  const keys = new Map(node.keysOf(account));
  keys.set("bb".repeat(32), { label: "laptop", role: { type: "owner" }, added_at: 2 });
  node.accounts.get(account)!.keys = keys;
  await expect(stage(page, "grow")).toHaveAttribute("aria-current", "step");
  await expect(page.getByTestId("tutorial-count")).toHaveText("8 of 13");
  // The chain has two validators already: helping a friend become one is done.
  await expect(task(page, "validator").locator(".check")).toHaveAttribute("aria-label", "Done");
});

test("the program task is offered only on a chain with programs on", async ({ page, node }) => {
  const { account } = await createWallet(page);
  await saveProgress(page, account, { done: BASICS });
  await expect(stage(page, "further")).toHaveAttribute("aria-current", "step");
  await expect(stage(page, "further").locator("[data-task]")).toHaveCount(3);
  await expect(page.getByTestId("tutorial")).not.toContainText(/program/i);
  await expect(page.getByTestId("tutorial-count")).toHaveText("5 of 13");

  // /api/fees has a fuel section only where programs are on (CLAUDE.md "Programs").
  await page.route(`${RPC}/api/fees`, (route) =>
    route.fulfill({ json: { height: node.height, lag: 17, rate_scale: 1000, slots: [], suggested: { flat: "10000", rate: "10000" }, floor: { flat: "10000", per_byte: "10" }, account_creation_fee: "100000000", fuel: { suggested: "1", floor: "1" } } }),
  );
  await saveProgress(page, account, { done: [...BASICS, "device", "stake", "vote"] });
  await expect(page.getByTestId("tutorial-count")).toHaveText("8 of 14");
  const program = task(page, "program");
  await expect(program).toContainText("Build a program");
  await expect(program).toContainText("This chain runs programs");
  await expect(page.getByTestId("tutorial")).not.toContainText(/contract/i);
  await expect(page.getByTestId("program-cli")).toContainText("blockula program upload code.wasm --interface iface.json");
  await expect(program.getByRole("link", { name: "See programs" })).toHaveAttribute("href", "/programs");
  // Not something the chain can tell from this account's history: the user ticks it.
  await program.getByRole("button", { name: "Mark done" }).click();
  await expect(stage(page, "grow")).toHaveAttribute("aria-current", "step");
});

test("growing the chain: a friend's pay link, a validator's commands, and a coding agent's prompt for this chain", async ({ page, context, node }) => {
  // One validator so far: the chain can't call the validator task done.
  node.statusExtra = { active_validators: 1 };
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  const { account } = await createWallet(page);
  await saveProgress(page, account, { done: [...BASICS, ...FURTHER] });
  await expect(stage(page, "grow")).toHaveAttribute("aria-current", "step");

  // A link that opens Send to this account with 1 filled in, on this wallet, for this chain.
  const link = page.getByTestId("invite-link");
  const url = new URL((await link.textContent()) ?? "");
  expect(url.origin).toBe(new URL(page.url()).origin);
  expect(url.pathname).toBe("/send");
  expect(Object.fromEntries(url.searchParams)).toMatchObject({ to: account, amount: "1", chain: CHAIN_ID, genesis: GENESIS.slice(0, 16) });
  // This wallet runs on localhost: a friend's device can't open it, and the checklist says what they can do.
  await expect(page.getByTestId("invite-local")).toContainText("won't open on a friend's device");
  await expect(page.getByTestId("invite-join")).toHaveText(`blockula join --genesis ${RPC} --genesis-hash ${GENESIS} --home ~/.blockula/${CHAIN_ID}`);
  await task(page, "invite").getByRole("button", { name: "Copy link" }).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(url.toString());
  await task(page, "invite").getByRole("button", { name: "Mark done" }).click();

  await expect(task(page, "validator")).not.toHaveClass(/done/);
  // The validator task's commands, filled in for this chain (docs/CREATE.md section 3).
  await task(page, "validator").getByRole("button", { name: /Help a friend become a validator/ }).click();
  await expect(page.getByTestId("validator-cli")).toContainText(`blockula validator init --home ~/.blockula/${CHAIN_ID}-validator --genesis ${RPC} --genesis-hash ${GENESIS}`);

  // The coding agent's prompt pins the MCP to this chain.
  await task(page, "coding").getByRole("button", { name: /Bring your coding agent/ }).click();
  const prompt = page.getByTestId("agent-prompt");
  await expect(prompt).toContainText(`claude mcp add blockula -e BLOCKULA_RPC=${RPC} -e BLOCKULA_CHAIN_ID=${CHAIN_ID} -e BLOCKULA_GENESIS_HASH=${GENESIS} -- npx -y @blockula/mcp`);
  for (const step of ["create_wallet", "faucet", "list_hunts", "AGENTS.md", "CONTRIBUTING.md", "submit_contribution"]) await expect(prompt).toContainText(step);
  await task(page, "coding").getByRole("button", { name: "Copy prompt" }).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toContain("BLOCKULA_GENESIS_HASH");
});

test("the last stage, the finale, and hiding and showing the checklist again", async ({ page }) => {
  const { account } = await createWallet(page);
  await saveProgress(page, account, { done: ALL.filter((t) => !["layout", "deploy"].includes(t)) });
  await expect(stage(page, "yours")).toHaveAttribute("aria-current", "step");
  await expect(task(page, "layout")).toContainText("node/src/explorer.html");
  await task(page, "layout").getByRole("button", { name: "Mark done" }).click();
  await expect(task(page, "deploy").getByRole("link", { name: "Read the guide" })).toHaveAttribute("href", /docs\/BUILD-ON-IT\.md$/);
  await task(page, "deploy").getByRole("button", { name: "Skip for now" }).click();
  await expect(page.getByTestId("tutorial")).toContainText("Every task done. The chain is yours.");

  await page.getByTestId("tutorial").getByRole("button", { name: "Hide" }).click();
  await expect(page.getByTestId("tutorial")).toHaveCount(0);
  await page.getByRole("button", { name: "Show the tutorial again" }).click();
  await expect(page.getByTestId("tutorial")).toBeVisible();
});

// Audit USER-2 / TOK-10: without a faucet, "Get some FANG" says someone has to send it, and to
// share the address until there's a name to share (it used to say "share your name" to people
// who had none).
test("without a faucet, the checklist says to share your address, then your name once you have one", async ({ page, node }) => {
  node.faucet = false;
  const { account } = await createWallet(page);
  const checklist = page.getByTestId("tutorial");
  // Nothing has arrived yet, so getting coins is the open task.
  await expect(checklist).toContainText("FANG is this chain's native unit. This chain has no faucet.");
  await expect(checklist.getByRole("button", { name: "Get 100 test FANG" })).toHaveCount(0);
  const share = page.getByTestId("share-to-receive");
  await expect(share).toHaveText("Someone has to send you some: share your address from Receive.");
  await expect(share.getByRole("link", { name: "Receive" })).toHaveAttribute("href", "/receive");
  node.names.set("vlad", account);
  await expect(share).toHaveText("Someone has to send you some: share your name, vlad.boo, from Receive.");
  // Sending is signed post-quantum, and final within seconds on any chain.
  await page.getByRole("button", { name: /Send your first payment/ }).click();
  await expect(checklist).toContainText("A payment is signed with your post-quantum key and final within seconds.");
  // The coding agent's prompt asks for coins instead of the faucet.
  await saveProgress(page, account, { done: [...BASICS, ...FURTHER, "invite", "validator", "agent"] });
  await expect(page.getByTestId("agent-prompt")).toContainText("This chain has no faucet: ask me to send your wallet some FANG.");
});
