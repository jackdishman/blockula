// The project site built without NEXT_PUBLIC_DEMO_NODE: the demo section rests, the page talks to
// no node at all, and everything else is there.
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
  await expect(page.getByTestId("connect-demo")).toHaveCount(0);
  await expect(page.locator("#launch .landing-cmd")).toHaveCount(2);
  await expect(page.getByTestId("contribute-prompt")).toBeVisible();
  await page.waitForTimeout(500);
  expect(fetches).toEqual([]);
  expect(await cspViolations(page)).toEqual([]);
});
