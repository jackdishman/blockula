import { seedWorld } from "./explorer-world";
import { RPC } from "./env";
import { cspViolations, expect, test, watchCsp } from "./helpers";
import type { MockNode } from "./mock-node";

const activity = (page: import("@playwright/test").Page) => page.locator("section", { has: page.getByRole("heading", { name: "Recent transactions", exact: true }) });
const nameTx = (node: MockNode, from: string, extra: Partial<Parameters<MockNode["add"]>[0]> = {}) => node.add({
  kind: "renew_name", from, to: null, amount: null, fee: 2000n, memo: "", signer: null,
  summary: "vlad.boo renewed castle.boo for 1 year", action: { type: "renew_name", name: "castle", years: 1 }, ...extra,
});

test("program activity has summaries, combined filters, sorting and receipt links", async ({ page, node }) => {
  const w = seedWorld(node);
  const failed = nameTx(node, w.vlad, { status: "failed", error: "name expired", summary: "vlad.boo could not renew castle.boo" });
  const pending = nameTx(node, w.vlad, { status: "pending", fee: 9000n });
  await watchCsp(page);
  await page.goto("/programs/names");
  const section = activity(page);
  await expect(section.getByRole("status")).toHaveText("6 of 6 loaded transactions");
  await expect(section.locator(".ledger-summary")).toHaveText([
    "vlad.boo renewed castle.boo for 1 year", "vlad.boo could not renew castle.boo", "renfield.boo released bat.boo",
    "vlad.boo offered castle.boo to mina.boo", "lucy.boo renewed lucy.boo for 1 year", "vlad.boo claimed castle.boo",
  ]);
  await expect(section.getByLabel("Loaded activity summary").locator(".amt")).toHaveAttribute("title", "0.00026168 FANG");
  await expect(section.locator(".ledger-row").first()).toHaveAttribute("href", `/tx/${pending}`);
  await section.getByLabel("Action", { exact: true }).selectOption("renew_name");
  await section.getByLabel("Status", { exact: true }).selectOption("failed");
  await expect(section.locator(".ledger-row")).toHaveCount(1);
  await expect(section.locator(".ledger-row")).toHaveAttribute("href", `/tx/${failed}`);
  await expect(section.locator(".ledger-row")).toContainText("name expired");
  await section.getByLabel("Search transactions").fill("lucy.boo");
  await expect(section.getByText("No transactions match these filters.")).toBeVisible();
  await section.getByRole("button", { name: "Clear filters" }).click();
  await section.getByLabel("Search transactions").fill("LUCY.BOO");
  await expect(section.locator(".ledger-row")).toHaveCount(1);
  await section.getByRole("button", { name: "Clear filters" }).click();
  await section.getByLabel("Sort by").selectOption("oldest");
  await expect(section.locator(".ledger-row").first()).toHaveAttribute("href", `/tx/${w.ids.registered}`);
  await section.getByLabel("Sort by").selectOption("fee-low");
  await expect(section.locator(".ledger-row").first()).toHaveAttribute("href", `/tx/${failed}`);
  await section.getByLabel("Sort by").selectOption("fee-high");
  await expect(section.locator(".ledger-row").last()).toHaveAttribute("href", `/tx/${pending}`);
  await section.locator(".ledger-row").last().click();
  await expect(page).toHaveURL(`/tx/${pending}`);
  expect(await cspViolations(page)).toEqual([]);
});

test("each program shows its own calls and resets filters when navigating", async ({ page, node }) => {
  const w = seedWorld(node);
  for (const [id, tx] of [["names", w.ids.registered], ["hunts", w.ids.hunt], ["pool", w.ids.github], ["validators", w.ids.delegated], ["accounts", w.ids.sent]]) {
    await page.goto(`/programs/${id}`);
    const section = activity(page);
    await expect(section.locator(`a[href="/tx/${tx}"]`)).toBeVisible();
    const unrelated = id === "names" ? w.ids.sent : w.ids.registered;
    await expect(section.locator(`a[href="/tx/${unrelated}"]`)).toHaveCount(0);
    await section.getByLabel("Search transactions").fill("no matching transaction");
    await expect(section.locator(".ledger-row")).toHaveCount(0);
  }
});

test("loads older program history past unrelated activity and retries failed reads", async ({ page, node }) => {
  const w = seedWorld(node);
  node.height += 600;
  await page.goto("/programs/names");
  const section = activity(page);
  await expect(section.getByRole("status")).toHaveText("0 of 0 loaded transactions");
  // Fail one historical block: the cursor must stay put so Retry can recover every call.
  let fail = true;
  await page.route(`${RPC}/api/block/*`, (route) => fail
    ? route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ error: "temporarily unavailable" }) })
    : route.fallback());
  await section.getByRole("button", { name: "Load older transactions" }).click();
  await expect(section.getByRole("alert")).toContainText("temporarily unavailable");
  fail = false;
  await section.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(section.locator(`a[href="/tx/${w.ids.registered}"]`)).toBeVisible();
  await expect(section.getByRole("status")).toHaveText("4 of 4 loaded transactions");
  await expect(section.getByRole("button", { name: "Load older transactions" })).toHaveCount(0);
  await expect(section.getByText(/Reached the end of this node/)).toBeVisible();
});

test("refreshes new calls and replaces a pending transaction with its receipt once", async ({ page, node }) => {
  const w = seedWorld(node);
  const pending = nameTx(node, w.vlad, { status: "pending" });
  await page.goto("/programs/names");
  const section = activity(page);
  await expect(section.getByRole("status")).toHaveText("5 of 5 loaded transactions");
  node.height++;
  const record = node.txs.get(pending)!;
  record.height = node.height;
  record.status = "finalized";
  const newer = nameTx(node, w.vlad, { summary: "vlad.boo renewed another.boo for 1 year" });
  await expect(section.getByRole("status")).toHaveText("6 of 6 loaded transactions");
  await expect(section.locator(`a[href="/tx/${pending}"]`)).toHaveCount(1);
  await expect(section.locator(".ledger-row").first()).toHaveAttribute("href", `/tx/${newer}`);
  await section.getByLabel("Status", { exact: true }).selectOption("pending");
  await expect(section.locator(".ledger-row")).toHaveCount(0);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(section.getByLabel("Search transactions")).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});
