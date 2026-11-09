// The landing page: a first visit to / (no wallet on this device) shows the chain the wallet is
// talking to and a short path of things to do on it; a device that has a wallet still opens it at /.
import type { Page } from "@playwright/test";
import { CHAIN_ID, GENESIS, RPC } from "./env";
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";

const INSTALL = "curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh";

/** `/api/params` and `/api/fees` as a node with programs on (or off) answers them, over the mock's own. */
async function chainWith(page: Page, opts: { programs: boolean }) {
  await page.route(`${RPC}/api/params`, (r) =>
    r.fulfill({
      json: {
        params: { unbonding_blocks: "1814400", issuance_stakers_bps: "7000", issuance_pool_bps: "2000", account_creation_fee: "100000000" },
        genesis: { block_time_ms: 1000, epoch_blocks: 3600 },
        issuance: { year: 0, rate_ppm: 60000, per_epoch: "6844626000000" },
      },
      headers: { "access-control-allow-origin": "*" },
    }),
  );
  await page.route(`${RPC}/api/fees`, (r) =>
    r.fulfill({
      json: { account_creation_fee: "100000000", tip_cap_multiple: 20, ...(opts.programs ? { fuel: { status: "on", rate: 1000 } } : {}) },
      headers: { "access-control-allow-origin": "*" },
    }),
  );
}

test("a first visit to / shows this chain and the get-started path", async ({ page, node }) => {
  node.statusExtra = { name: "Jack in the Block", token: "JACK" };
  await chainWith(page, { programs: false });
  await watchCsp(page);
  await page.goto("/");
  const main = page.locator("main");
  // The hero is the connected chain, by its name and ticker.
  await expect(page.getByTestId("landing-chain")).toHaveText("Jack in the Block");
  await expect(main).toContainText("a chain whose unit is JACK");
  await expect(page.getByTestId("landing-no-chain")).toHaveCount(0);

  // Six steps, each with its action.
  const steps = page.locator(".landing-step");
  await expect(steps).toHaveCount(6);
  await expect(steps.locator("strong")).toHaveText(["Make a wallet", "Get coins, send one", "Invite a friend", "Bring your coding agent", "Build a program", "Make it yours"]);
  await expect(main).toContainText("alice.boo");
  await expect(main.getByRole("link", { name: "Send", exact: true })).toHaveAttribute("href", "/send");
  await expect(page.getByRole("link", { name: "Build on it" })).toHaveAttribute("href", "https://github.com/jackdishman/blockula/blob/main/docs/BUILD-ON-IT.md");
  await expect(main).toContainText("can be changed with Claude Code");
  // Programs are off here (no `fuel` in /api/fees), and the step says how a chain gets them.
  await expect(page.getByTestId("step-programs").locator(".landing-programs")).toHaveAttribute("data-state", "off");
  await expect(page.getByTestId("step-programs")).toContainText("blockula create --programs");
  await expect(main.getByRole("link", { name: "Programs", exact: true })).toHaveCount(0);

  // The invite: the wallet runs on localhost, so it says a link wouldn't open elsewhere, and gives the join command for this chain.
  const invite = page.locator("#invite");
  await expect(page.getByTestId("invite-local")).toContainText("opens only on this computer");
  await expect(invite.locator(".landing-cmd code").first()).toHaveText(INSTALL);
  await expect(invite.locator(".landing-cmd code").nth(1)).toHaveText(`blockula join --genesis ${RPC} --genesis-hash ${GENESIS} --home ~/.blockula/${CHAIN_ID}`);
  await invite.getByText("Make them a validator").click();
  await expect(invite).toContainText(`blockula validator register --home ~/.blockula/${CHAIN_ID}-validator --rpc ${RPC}`);
  await expect(invite).toContainText(`blockula validator approve <validator address> --key ~/.blockula/${CHAIN_ID}/validator.key`);

  // The agent's prompt is pinned to this chain.
  const prompt = page.getByTestId("agent-prompt");
  await expect(prompt).toContainText(`claude mcp add blockula -e BLOCKULA_RPC=${RPC} -e BLOCKULA_CHAIN_ID=${CHAIN_ID} -e BLOCKULA_GENESIS_HASH=${GENESIS} -- npx -y @blockula/mcp`);
  for (const want of ["`create_wallet`", "`faucet`", "list_hunts", "AGENTS.md and CONTRIBUTING.md", "focused PR", "JACK escrowed", "submit_contribution"]) await expect(prompt).toContainText(want);

  // The flow of the unit, with this node's numbers.
  await expect(page.getByTestId("flow-issuance")).toContainText("6% a year");
  await expect(page.getByTestId("flow-issuance")).toContainText("every 60 min");
  await expect(page.locator(".flow-share")).toHaveText(["Stakers70%", "Contribution pool20%", "Treasury10%"]);
  await expect(page.getByTestId("flow-burn")).toContainText("burns 0.1 JACK more");
  await expect(main).toContainText("21 days after you unstake");
  await expect(main).toContainText("has no value by design");

  // Use cases, and what the page never says.
  await expect(page.locator(".landing-use")).toHaveCount(6);
  await expect(main).toContainText("isn't externally audited");
  await expect(main).not.toContainText(/mainnet|\bcontracts?\b|token sale/i);

  await page.getByRole("link", { name: "Make a wallet" }).click();
  await expect(page).toHaveURL(/\/welcome$/);
  await expect(page.locator(".chapter-body")).toBeVisible();
  expect(await cspViolations(page)).toEqual([]);
});

test("on a chain with programs on, the program step gives the CLI's calls", async ({ page }) => {
  await chainWith(page, { programs: true });
  await page.goto("/");
  const step = page.getByTestId("step-programs");
  await expect(step.locator(".landing-programs")).toHaveAttribute("data-state", "on");
  await expect(step.locator(".landing-cmd code")).toHaveText([
    "blockula program upload code.wasm --interface iface.json",
    "blockula program create <wasm_hash> <interface_hash>",
    "blockula program call <address> <method>",
  ]);
  await expect(step.getByRole("link", { name: "Programs" })).toHaveAttribute("href", "/programs");
});

test("with no node answering, it shows how to start a chain", async ({ page }) => {
  await page.route(`${RPC}/api/**`, (r) => r.abort());
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Start your own quantum-safe ledger" })).toBeVisible();
  const none = page.getByTestId("landing-no-chain");
  await expect(none.locator(".landing-cmd code")).toHaveText([INSTALL, "blockula create"]);
  await expect(none).toContainText("blockula dev");
  // Labels without numbers, and placeholders in the prompt.
  await expect(page.getByTestId("flow-issuance")).toContainText("New coins each epoch");
  await expect(page.getByTestId("flow-burn")).toContainText("a creation fee");
  await expect(page.getByTestId("agent-prompt")).toContainText("BLOCKULA_RPC=<node url> -e BLOCKULA_CHAIN_ID=<chain id>");
  await expect(page.getByTestId("step-programs").locator(".landing-programs")).toHaveAttribute("data-state", "unknown");
});

test("the landing page fits a 390 px phone", async ({ page }) => {
  await chainWith(page, { programs: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await expect(page.locator(".flow-share em").first()).toHaveText("70%");
  await page.locator("#invite").getByText("Make them a validator").click();
  const by = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(by).toBeLessThanOrEqual(1);
});

test("a device with a wallet opens it at /, not the landing page", async ({ page }) => {
  await createWallet(page);
  // A new page load locks the wallet: / asks for the passkey.
  await page.goto("/");
  await expect(page.getByRole("button", { name: "Unlock with passkey" })).toBeVisible();
  await expect(page.locator(".landing")).toHaveCount(0);
});
