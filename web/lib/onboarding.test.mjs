// lib/onboarding.ts: where a payment link may take onboarding, what /welcome may say about it,
// Home's balance parts and name banners. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { breakdown, canAffordName, cheapestName, nameAlert, payRequest, safeNext, welcomeHref } from "./onboarding.ts";

const ADDR = "boo1yuuvyc7ngus0l2vytdt6yr0zqhj2qyz9j4zyfxk97vjtpsw9rk4qzmxxe9";

test("safeNext keeps this site's own payment links", () => {
  assert.equal(safeNext("/send?to=vlad.boo&amount=3&memo=pizza"), "/send?to=vlad.boo&amount=3&memo=pizza");
  assert.equal(safeNext(`/send?to=${ADDR}&amount=1.5&chain=blockula-testnet&genesis=6e6e6e6e6e6e6e6e`), `/send?to=${ADDR}&amount=1.5&chain=blockula-testnet&genesis=6e6e6e6e6e6e6e6e`);
  // A memo with a URL in it arrives percent-encoded, as payLink writes it.
  assert.equal(safeNext("/send?to=vlad.boo&memo=https%3A%2F%2Fexample.com"), "/send?to=vlad.boo&memo=https%3A%2F%2Fexample.com");
  // A fragment is dropped.
  assert.equal(safeNext("/send?to=vlad.boo#x"), "/send?to=vlad.boo");
});

test("safeNext refuses anything that could leave the site or go elsewhere in it", () => {
  for (const bad of [
    null,
    undefined,
    "",
    "https://evil.example",
    "https://evil.example/send?to=vlad.boo",
    "//evil.example",
    "//evil.example/send?to=x",
    "/\\evil.example",
    "/send?to=x\\y",
    "/send?to=a//b",
    "\\\\evil.example/send?to=x",
    "/send",
    "/send?",
    "/sendx?to=vlad.boo",
    "/send/?to=vlad.boo",
    "/devices?to=vlad.boo",
    "/send?to=vlad.boo\n",
    "/send?to=vla d.boo",
    "/send?to=\u0000",
    " /send?to=vlad.boo",
    "javascript:alert(1)",
    "/%2F%2Fevil.example",
    `/send?to=${"a".repeat(3000)}`,
  ]) {
    assert.equal(safeNext(bad), null, JSON.stringify(bad));
  }
});

test("welcomeHref carries payment links only", () => {
  assert.equal(welcomeHref("/send", "?to=vlad.boo&amount=3"), `/welcome?next=${encodeURIComponent("/send?to=vlad.boo&amount=3")}`);
  assert.equal(new URLSearchParams(welcomeHref("/send", "?to=vlad.boo&amount=3").split("?")[1]).get("next"), "/send?to=vlad.boo&amount=3");
  assert.equal(welcomeHref("/send", ""), "/welcome");
  assert.equal(welcomeHref("/devices", "?x=1"), "/welcome");
  assert.equal(welcomeHref("/", ""), "/welcome");
});

test("payRequest shows only a well-formed recipient and amount, never the memo", () => {
  assert.deepEqual(payRequest("/send?to=Vlad.boo&amount=3&memo=pizza"), { to: "vlad.boo", amount: "3" });
  assert.deepEqual(payRequest("/send?to=vlad&amount=0001.500"), { to: "vlad.boo", amount: "1.5" });
  assert.deepEqual(payRequest(`/send?to=${ADDR}`), { to: ADDR, amount: null });
  // The sender's text: a bidi override, words, an impossible amount.
  assert.deepEqual(payRequest(`/send?to=${encodeURIComponent("‮vlad.boo")}&amount=1`), { to: null, amount: "1" });
  assert.deepEqual(payRequest(`/send?to=${encodeURIComponent("pay me at evil.example")}&amount=abc`), { to: null, amount: null });
  assert.deepEqual(payRequest("/send?to=-vlad&amount=0"), { to: null, amount: null });
  assert.deepEqual(payRequest("/send?to=vlad.boo&amount=18446744073.709551616"), { to: "vlad.boo", amount: null });
  assert.equal(payRequest(null), null);
});

test("breakdown: what can move, what's staked, what's locked and until when", () => {
  // Nothing staked or locked: no line under the balance.
  assert.equal(breakdown({ balance: "100", transferable: "100", locked: "0", locks: [] }, { bonded: "0", unbonding: "0" }).show, false);
  // A node from before protocol v1 doesn't say: everything is spendable.
  assert.equal(breakdown({ balance: "100" }, null).show, false);
  const b = breakdown(
    { balance: "100", transferable: "50", locked: "50", locks: [{ amount: "50", until: 900 }, { amount: "20", until: 1200 }, { amount: "0", until: 5000 }] },
    { bonded: "10", unbonding: "3" },
  );
  assert.deepEqual(b, { canSend: 50n, staked: 10n, unstaking: 3n, locked: 50n, lockedUntil: 1200, show: true });
  assert.equal(breakdown({ balance: "100", transferable: "100" }, { bonded: "1" }).show, true);
});

test("nameAlert: a name expiring within the window, or in grace, if it's still ours", () => {
  const me = ADDR;
  const name = (over) => ({ name: "vlad.boo", available: false, owner: { address: me, name: "vlad.boo", short: "vlad.boo" }, status: "active", expires_at: 1000, grace_until: 2000, renewal_price_display: "1 FANG", ...over });
  assert.equal(nameAlert(name({}), me, 100, 500), null);
  assert.deepEqual(nameAlert(name({}), me, 600, 500), { name: "vlad.boo", state: "expiring", blocksLeft: 400, renewal: "1 FANG" });
  assert.deepEqual(nameAlert(name({ status: "grace" }), me, 1500, 500), { name: "vlad.boo", state: "grace", blocksLeft: 500, renewal: "1 FANG" });
  // Someone else's, lapsed for good, or reserved: nothing to renew.
  assert.equal(nameAlert(name({ owner: { address: "boo1other", name: null, short: "boo1other" } }), me, 600, 500), null);
  assert.equal(nameAlert(name({ status: "available" }), me, 2500, 500), null);
  assert.equal(nameAlert(name({ status: "reserved", expires_at: null }), me, 600, 500), null);
  // Without `status`, the heights decide.
  assert.equal(nameAlert(name({ status: undefined }), me, 1500, 500)?.state, "grace");
  assert.equal(nameAlert(name({ status: undefined }), me, 900, 500)?.state, "expiring");
  assert.equal(nameAlert(name({ status: undefined }), me, 2500, 500), null);
});

test("cheapestName reads the node's prices, and canAffordName falls back to any FANG", () => {
  assert.equal(cheapestName({ name_price_3: "100000000000", name_price_4: "25000000000", name_price_5: "5000000000" }), 5_000_000_000n);
  assert.equal(cheapestName({ name_price_5: "7" }), 7n);
  assert.equal(cheapestName({ name_price_5: "not a number" }), null);
  assert.equal(cheapestName(null), null);
  assert.equal(canAffordName(4_999_999_999n, 5_000_000_000n), false);
  assert.equal(canAffordName(5_000_000_000n, 5_000_000_000n), true);
  assert.equal(canAffordName(0n, null), false);
  assert.equal(canAffordName(1n, null), true);
});
