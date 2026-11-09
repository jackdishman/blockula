// Add to Home Screen (app/manifest.ts, app/apple-icon.png, components/HomeScreenHint.tsx): the
// manifest and touch icon are served with their types, the pages link them, and iOS Safari, which
// never offers to install by itself, gets a hint that goes away for good once dismissed.
// (The wallet a node serves is checked in served.spec.ts, under the chain's name.)
import { cspViolations, expect, test, watchCsp } from "./helpers";

const IPHONE_SAFARI = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Mobile/15E148 Safari/604.1";
const IPHONE_CHROME = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/138.0.7204.156 Mobile/15E148 Safari/604.1";

test("the manifest and touch icon are served and linked", async ({ page, request }) => {
  await page.goto("/");
  const manifestHref = await page.locator('link[rel="manifest"]').getAttribute("href");
  const iconHref = await page.locator('link[rel="apple-touch-icon"]').getAttribute("href");
  await expect(page.locator('meta[name="apple-mobile-web-app-capable"]')).toHaveAttribute("content", "yes");
  await expect(page.locator('meta[name="apple-mobile-web-app-title"]')).toHaveAttribute("content", "Blockula");
  const m = await request.get(manifestHref as string);
  expect(m.status()).toBe(200);
  expect(m.headers()["content-type"]).toMatch(/^application\/manifest\+json/);
  const manifest = await m.json();
  expect(manifest).toMatchObject({ name: "Blockula Wallet", short_name: "Blockula", start_url: "./", scope: "./", display: "standalone", background_color: "#ece3cd" });
  const base = new URL(manifestHref as string, page.url());
  for (const href of [iconHref as string, ...manifest.icons.map((i: { src: string }) => new URL(i.src, base).pathname)]) {
    const r = await request.get(href);
    expect(r.status(), href).toBe(200);
    expect(r.headers()["content-type"], href).toBe("image/png");
  }
});

test.describe("on iOS Safari", () => {
  test.use({ userAgent: IPHONE_SAFARI, viewport: { width: 390, height: 844 } });

  test("the hint says how, and stays dismissed", async ({ page }) => {
    await watchCsp(page);
    await page.goto("/explore");
    const hint = page.getByTestId("home-screen-hint");
    await expect(hint).toContainText("Add Blockula to your Home Screen");
    await expect(hint).toContainText("Share");
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    await hint.getByRole("button", { name: "Got it" }).click();
    await expect(hint).toHaveCount(0);
    await page.reload();
    await expect(page.getByRole("heading", { name: /live$/ })).toBeVisible();
    await expect(page.getByTestId("home-screen-hint")).toHaveCount(0);
    expect(await cspViolations(page)).toEqual([]);
  });

  test("opened from the home screen, there's no hint", async ({ page }) => {
    await page.addInitScript(() => Object.defineProperty(navigator, "standalone", { value: true }));
    await page.goto("/explore");
    await expect(page.getByRole("heading", { name: /live$/ })).toBeVisible();
    await expect(page.getByTestId("home-screen-hint")).toHaveCount(0);
  });
});

test.describe("elsewhere", () => {
  test.use({ userAgent: IPHONE_CHROME });

  test("Chrome on iOS and desktop browsers get no hint", async ({ page }) => {
    await page.goto("/explore");
    await expect(page.getByRole("heading", { name: /live$/ })).toBeVisible();
    await expect(page.getByTestId("home-screen-hint")).toHaveCount(0);
  });
});
