// The project site built without NEXT_PUBLIC_DEMO_NODE and with no chains listed: the demo section
// rests, the directory shows its empty state, the page talks to no node at all, and everything
// else is there.
import { expect, test } from "@playwright/test";
import { cspViolations, watchCsp } from "./helpers";

test("with no demo configured, the demo rests and the page talks to no node", async ({ page }) => {
  await watchCsp(page);
  const fetches: string[] = [];
  page.on("request", (r) => {
    if (["fetch", "xhr"].includes(r.resourceType()) && !r.url().startsWith("http://localhost")) fetches.push(r.url());
  });
  const res = await page.goto("/");
  expect(res?.headers()["content-security-policy"]).toContain("connect-src 'self';");
  const resting = page.getByTestId("demo-resting");
  await expect(resting).toContainText("The demo chain is resting");
  await expect(resting).toContainText("no demo running");
  await expect(page.getByTestId("demo-wallet")).toHaveCount(0);
  await expect(page.getByTestId("connect-input")).toBeVisible();
  await expect(page.locator("#start .landing-cmd")).toHaveCount(2);
  // No chains listed (e2e/fixtures/chains-empty): the directory says how to be the first.
  const empty = page.getByTestId("chains-empty");
  await expect(empty).toContainText("No chains listed yet");
  await expect(empty).toContainText("List it on blockula.app");
  await expect(empty.locator(".landing-cmd code")).toHaveText(["curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh", "blockula create"]);
  await expect(page.getByTestId("chain-list")).toHaveCount(0);
  await expect(page.getByTestId("chains-list-yours")).toBeVisible();
  await expect(page.getByTestId("contribute-prompt")).toContainText("Contribute to Blockula");
  await page.waitForTimeout(500);
  expect(fetches).toEqual([]);
  expect(await cspViolations(page)).toEqual([]);
});
