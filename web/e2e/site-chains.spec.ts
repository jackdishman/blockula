// The project site's chain directory ("Join a chain", components/site/Chains.tsx), built from the
// fixture listings in e2e/fixtures/chains (playwright.config.ts): a card per chain with its live
// status read from its node (mocked here), the steps to become a validator filled in for it, and
// the site's CSP letting the page read exactly the listed nodes.
import { expect, test, type Page } from "@playwright/test";
import { DEMO_NODE } from "./env";
import { cspViolations, watchCsp } from "./helpers";

const cors = { "access-control-allow-origin": "*" };
const PIZZA = { rpc: "https://pizza.chains-fixture.com", genesis: "a1".repeat(32) };
const GARDEN = { rpc: "https://garden.tail1234.ts.net", genesis: "b2".repeat(32) };
const OWL = { rpc: "https://owl.chains-fixture.com", genesis: "c3".repeat(32) };
const INSTALL = "curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh";

type Node = { status?: Record<string, unknown>; validators?: Record<string, unknown> } | "down";

/** The fixture chains' nodes behind routes: Pizza Ledger a set of one with a faucet, Garden Club live, Night Shift down, unless told otherwise. */
async function chains(page: Page, nodes: { pizza?: Node; garden?: Node; owl?: Node } = {}) {
  await page.route(`${DEMO_NODE}/api/**`, (r) => r.abort());
  const serve = async (rpc: string, node: Node) =>
    page.route(`${rpc}/api/**`, (r) => {
      if (node === "down") return r.abort();
      const path = new URL(r.request().url()).pathname;
      const body = path === "/api/status" ? node.status : path === "/api/validators" ? node.validators : undefined;
      return body ? r.fulfill({ headers: cors, json: body }) : r.fulfill({ status: 404, headers: cors, body: "" });
    });
  await serve(
    PIZZA.rpc,
    nodes.pizza ?? {
      status: { chain_id: "pizza-ledger", genesis_hash: PIZZA.genesis, height: 98765, faucet: true, active_validators: 1, wallet: "/wallet/" },
      validators: { active: [{}], min_bond: "50000000000", admission: { members: [{ name: "pizza.boo" }], quorum: 1, ready_from: 540 }, churn_limit: 0, admins: [{ name: "pizza.boo" }] },
    },
  );
  await serve(GARDEN.rpc, nodes.garden ?? { status: { chain_id: "garden-club", genesis_hash: GARDEN.genesis, height: 12, faucet: true, active_validators: 2 }, validators: { active: [{}, {}], min_bond: "50000000000" } });
  await serve(OWL.rpc, nodes.owl ?? "down");
}

test("a card per listed chain: live or resting, with its wallet and explorer", async ({ page }) => {
  await watchCsp(page);
  await chains(page);
  await page.goto("/");
  await expect(page.getByRole("link", { name: "Join a chain" })).toHaveAttribute("href", "#chains");
  const list = page.getByTestId("chain-list");
  await expect(list.locator("> li")).toHaveCount(3);
  // Sorted by name.
  await expect(list.locator("> li h3")).toHaveText(["Garden Club", "Night Shift", "Pizza Ledger"]);

  const pizza = page.getByTestId("chain-pizza-ledger");
  await expect(pizza.getByTestId("chain-status")).toHaveText(/Live · block 98,765 · 1 validator · bond 50 SLICE/);
  await expect(pizza.getByTestId("chain-token")).toHaveText("SLICE");
  await expect(pizza.getByTestId("chain-wants")).toHaveText("Wants validators");
  await expect(pizza).toContainText("Who owes whom a slice");
  await expect(pizza.getByTestId("chain-wallet")).toHaveAttribute("href", `${PIZZA.rpc}/wallet/`);
  await expect(pizza.getByTestId("chain-explorer")).toHaveAttribute("href", `${PIZZA.rpc}/`);
  await expect(pizza.getByTestId("chain-validate")).toHaveAttribute("aria-expanded", "false");

  // A tailnet chain (Funnel) is open for its wallet, but takes no validators.
  const garden = page.getByTestId("chain-garden-club");
  await expect(garden.getByTestId("chain-status")).toHaveText(/Live · block 12 · 2 validators/);
  await expect(garden.getByTestId("chain-wants")).toHaveCount(0);
  await expect(garden.getByTestId("chain-validate")).toHaveCount(0);
  await expect(garden.getByTestId("chain-no-validators")).toContainText("on a tailnet");
  await expect(garden.getByTestId("chain-wallet")).toHaveAttribute("href", `${GARDEN.rpc}/wallet/`);

  const owl = page.getByTestId("chain-night-shift");
  await expect(owl.getByTestId("chain-status")).toHaveText(/Resting/);
  await expect(owl).toHaveAttribute("data-state", "resting");
  await expect(owl.getByTestId("chain-explorer")).toHaveAttribute("href", `${OWL.rpc}/`);

  await expect(page.getByTestId("chains-empty")).toHaveCount(0);
  await expect(page.locator("body")).not.toContainText(/\bcontracts?\b|token sale|\bAPR\b|\byield\b|airdrop|invest/i);
  expect(await cspViolations(page)).toEqual([]);
});

test("become a validator: the commands for that chain, with a copy button each", async ({ page }) => {
  await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
  await watchCsp(page);
  await chains(page);
  await page.goto("/");
  const pizza = page.getByTestId("chain-pizza-ledger");
  await expect(pizza.getByTestId("chain-status")).toContainText("Live");
  await pizza.getByTestId("chain-validate").click();
  await expect(pizza.getByTestId("chain-validate")).toHaveAttribute("aria-expanded", "true");
  const steps = pizza.getByTestId("validator-steps");
  await expect(steps).toContainText("stays on and online");
  await expect(steps).toContainText("TCP 30300");
  const cmds = steps.locator(".landing-cmd code");
  await expect(cmds).toHaveText([
    INSTALL,
    `blockula join --genesis ${PIZZA.rpc} --genesis-hash ${PIZZA.genesis} --home ~/.blockula/pizza-ledger --bind 0.0.0.0`,
    `blockula validator init --home ~/.blockula/pizza-ledger-validator --genesis ${PIZZA.rpc} --genesis-hash ${PIZZA.genesis} --public-p2p-addr <your host>:30300 --public-http-addr http://<your host>:7171`,
    `blockula wallet faucet --to <your validator address> --rpc ${PIZZA.rpc}`,
    "blockula run --home ~/.blockula/pizza-ledger-validator --p2p-bind 0.0.0.0",
    `blockula validator register --home ~/.blockula/pizza-ledger-validator --rpc ${PIZZA.rpc}`,
    "blockula validator admit <your validator address> --key ~/.blockula/pizza-ledger/validator.key --rpc http://localhost:7070",
  ]);
  await expect(steps.getByTestId("step-bond-faucet")).toContainText("The bond is 50 SLICE");
  // A set of one: the owner admits newcomers; the contact is a GitHub handle.
  await expect(steps.getByTestId("step-admit")).toContainText("This chain has 1 validator");
  await expect(steps.getByTestId("chain-contact")).toHaveAttribute("href", "https://github.com/pizzajack");

  // The visitor's own host fills in the init command; something that isn't a host doesn't.
  await steps.getByTestId("validator-host").fill("My-Box.example.org");
  await expect(cmds.nth(2)).toContainText("--public-p2p-addr my-box.example.org:30300 --public-http-addr http://my-box.example.org:7171");
  await steps.getByTestId("validator-host").fill("my box; rm -rf");
  await expect(cmds.nth(2)).toContainText("--public-p2p-addr <your host>:30300");

  // Every command copies as shown.
  const copies = steps.locator(".landing-cmd button");
  await expect(copies).toHaveCount(7);
  for (const i of [1, 6]) {
    await copies.nth(i).click();
    await expect(copies.nth(i)).toHaveText("Copied ✓");
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(await cmds.nth(i).textContent());
  }
  await pizza.getByTestId("chain-validate").click();
  await expect(pizza.getByTestId("validator-steps")).toHaveCount(0);
  expect(await cspViolations(page)).toEqual([]);
});

test("a bigger set without a faucet: ask the owner for the bond, join by stake", async ({ page }) => {
  await chains(page, {
    owl: {
      status: { chain_id: "night-shift", genesis_hash: OWL.genesis, height: 4000, faucet: false, active_validators: 5 },
      validators: { active: [{}, {}, {}, {}, {}], min_bond: "10000000000000", admission: null, churn_limit: 1, admins: [] },
    },
  });
  await page.goto("/");
  const owl = page.getByTestId("chain-night-shift");
  await expect(owl.getByTestId("chain-status")).toHaveText(/Live · block 4,000 · 5 validators · bond 10,000 OWL/);
  await owl.getByTestId("chain-validate").click();
  const steps = owl.getByTestId("validator-steps");
  await expect(steps.getByTestId("step-bond-ask")).toContainText("This chain has no faucet. Ask the owner (owlnurse) to send 10,000 OWL");
  await expect(steps.getByTestId("step-stake")).toContainText("takes newcomers by stake");
  await expect(steps.locator(".landing-cmd code")).toHaveCount(5);
  await expect(steps).not.toContainText("wallet faucet");
});

test("a set of three: the admin approves; a resting chain still shows how to get in", async ({ page }) => {
  await chains(page, {
    pizza: {
      status: { chain_id: "pizza-ledger", genesis_hash: PIZZA.genesis, height: 5, faucet: true, active_validators: 3 },
      validators: { active: [{}, {}, {}], min_bond: "50000000000", admission: null, churn_limit: 0, admins: [{ name: "pizza.boo" }] },
    },
  });
  await page.goto("/");
  const pizza = page.getByTestId("chain-pizza-ledger");
  await expect(pizza.getByTestId("chain-status")).toContainText("3 validators");
  await pizza.getByTestId("chain-validate").click();
  await expect(pizza.getByTestId("step-approve")).toContainText("blockula validator approve <your validator address> --key ~/.blockula/pizza-ledger/validator.key --rpc http://localhost:7070");

  const owl = page.getByTestId("chain-night-shift");
  await expect(owl.getByTestId("chain-status")).toHaveText(/Resting/);
  await owl.getByTestId("chain-validate").click();
  await expect(owl.getByTestId("step-unknown")).toContainText("blockula validator admit <your validator address>");
  await expect(owl.getByTestId("step-bond-faucet")).toContainText("otherwise ask the owner");
});

test("a node that reports another chain than its listing gets no buttons", async ({ page }) => {
  await chains(page, { pizza: { status: { chain_id: "pizza-ledger", genesis_hash: "ff".repeat(32), height: 5 } }, garden: { status: { chain_id: "someone-else", genesis_hash: GARDEN.genesis, height: 5 } } });
  await page.goto("/");
  for (const id of ["pizza-ledger", "garden-club"]) {
    const card = page.getByTestId(`chain-${id}`);
    await expect(card.getByRole("alert")).toContainText("reports another chain");
    await expect(card.getByTestId("chain-wallet")).toHaveCount(0);
    await expect(card.getByTestId("chain-validate")).toHaveCount(0);
  }
});

test("the page may read the demo node and the listed nodes, nothing else (CSP connect-src)", async ({ page }) => {
  const res = await page.goto("/");
  const csp = res?.headers()["content-security-policy"] ?? "";
  expect(/connect-src ([^;]*);/.exec(csp)?.[1]).toBe(`'self' ${DEMO_NODE} ${GARDEN.rpc} ${OWL.rpc} ${PIZZA.rpc}`);
});

test("list your chain, and help build Blockula", async ({ page }) => {
  await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
  await chains(page);
  await page.goto("/");
  const list = page.getByTestId("chains-list-yours");
  const href = await list.getByTestId("list-template").getAttribute("href");
  const u = new URL(href ?? "");
  expect(u.origin + u.pathname).toBe("https://github.com/jackdishman/blockula/new/main");
  expect(u.searchParams.get("filename")).toBe("chains/your-chain.json");
  expect(JSON.parse(u.searchParams.get("value") ?? "")).toMatchObject({ chain_id: "your-chain", rpc: "https://chain.example.org", validators_wanted: false });
  await expect(list.getByRole("link", { name: "What gets listed" })).toHaveAttribute("href", "https://github.com/jackdishman/blockula/blob/main/chains/README.md");

  const help = page.getByTestId("help-build");
  await help.getByRole("button", { name: "Copy prompt" }).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toMatch(/^Contribute to Blockula \(https:\/\/github\.com\/jackdishman\/blockula\)/);
  await expect(help.getByRole("link", { name: "Good first issues" })).toBeVisible();
});

for (const scheme of ["light", "dark"] as const)
  test(`the directory fits a 390 px phone, steps open (${scheme})`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await page.setViewportSize({ width: 390, height: 844 });
    await chains(page);
    await page.goto("/");
    const pizza = page.getByTestId("chain-pizza-ledger");
    await expect(pizza.getByTestId("chain-status")).toContainText("Live");
    await pizza.getByTestId("chain-validate").click();
    await page.getByTestId("help-build").getByText("Read the prompt").click();
    await expect(pizza.getByTestId("validator-steps")).toBeVisible();
    const by = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(by).toBeLessThanOrEqual(1);
    // The commands get the card's width, not a sliver of it.
    for (const w of await pizza.locator(".landing-cmd code").evaluateAll((els) => els.map((e) => e.getBoundingClientRect().width))) expect(w).toBeGreaterThan(240);
  });
