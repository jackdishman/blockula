// The wallet's own receipt page (/tx/{id}) for the answers a node gives without the tx
// (node/src/api.rs `tx`): a tx this node dropped from its mempool unrun isn't sealed, failed or
// final, since another node may still include it until its valid_until (audit
// review-fix-clients-2, review-r4-wallet-2); a failure that ran in a block is final and charged.
import { cspViolations, expect, test, watchCsp } from "./helpers";
import { addressOf } from "./mock-node";

const DROP = "d0".repeat(32);
const OLD_NODE_DROP = "d1".repeat(32);
const RAN = "f0".repeat(32);
const EVICTED = "evicted from a full mempool by a transaction with a higher tip";
const FROM = addressOf("a1".repeat(32));
const TO = addressOf("b2".repeat(32));
const label = (address: string) => ({ address, name: null, short: `${address.slice(0, 10)}…${address.slice(-4)}` });

test("a tx dropped unrun reads as dropped and not final, never sealed or failed, and is still followed", async ({ page, node }) => {
  await watchCsp(page);
  node.txAnswers.set(DROP, { id: DROP, status: "failed", dropped: true, error: EVICTED, height: 99, summary: `Transaction dropped without running: ${EVICTED}` });
  const blockReads: string[] = [];
  page.on("request", (r) => void (/\/api\/block\//.test(r.url()) && blockReads.push(r.url())));
  await page.goto(`/tx/${DROP}`);
  const main = page.locator("main");
  const receipt = page.locator("section.receipt");
  const trace = page.getByRole("list", { name: "What happened" });
  await expect(receipt.locator(".eyebrow")).toHaveText("Dropped, not final");
  await expect(receipt.locator(".pill")).toHaveText(["dropped"]);
  await expect(main.locator(".pill.failed")).toHaveCount(0);
  await expect(trace.locator("li").filter({ hasText: /^Broadcast/ })).toContainText(
    `dropped from this node's mempool without running around block #99 (${EVICTED}): nothing was charged, but another node may still include it until its valid_until, so don't send it again before then`,
  );
  await expect(trace.locator("li").filter({ hasText: /^Sealed/ })).toContainText("not in any block this node holds: another node may still include it until its valid_until");
  await expect(trace.locator("li").filter({ hasText: /^Final/ })).toContainText("not yet: it may still run until its valid_until passes");
  // The height is where this node dropped it, not a block it ran in.
  await expect(page.getByRole("navigation", { name: "Breadcrumbs" })).not.toContainText("Block");
  for (const ran of ["in block #", "but the action failed", "blocks deep", "certificate", "fee was charged", "Included, but failed"]) await expect(main).not.toContainText(ran);
  expect(blockReads).toEqual([]);

  // Another node includes it, and this node runs that block: the receipt follows it there.
  node.txAnswers.set(DROP, {
    id: DROP, summary: "boo1… sent 1 FANG to boo1…", kind: "transfer", status: "finalized", height: 120, from: label(FROM), to: label(TO),
    amount: "1000000000", amount_display: "1 FANG", fee: "60000", fee_display: "0.00006 FANG", max_fee: "100000", tip: "0", memo: "", nonce: 4, size_bytes: 5400,
  });
  await expect(receipt.locator(".eyebrow")).toHaveText("Receipt", { timeout: 30_000 });
  await expect(receipt.locator(".pill")).toHaveText(["finalized"]);
  await expect(receipt).toContainText("in block #120");
  await expect(page.getByRole("navigation", { name: "Breadcrumbs" })).toContainText("Block #120");
  expect(await cspViolations(page)).toEqual([]);
});

test("an older node's tx-less failure, which doesn't say whether it ran, reads as a drop", async ({ page, node }) => {
  node.txAnswers.set(OLD_NODE_DROP, { id: OLD_NODE_DROP, status: "failed", error: "expired", height: 991, summary: "Transaction failed: expired" });
  await page.goto(`/tx/${OLD_NODE_DROP}`);
  const receipt = page.locator("section.receipt");
  await expect(receipt.locator(".eyebrow")).toHaveText("Dropped, not final");
  await expect(receipt.locator(".pill")).toHaveText(["dropped"]);
  await expect(page.locator("main")).not.toContainText("but the action failed");
});

test("a failure answered from memory, without the tx, ran in its block: final, and its fee was charged", async ({ page, node }) => {
  await watchCsp(page);
  node.txAnswers.set(RAN, { id: RAN, status: "failed", dropped: false, error: "insufficient balance", height: 99, summary: "Transaction failed: insufficient balance" });
  await page.goto(`/tx/${RAN}`);
  const receipt = page.locator("section.receipt");
  const trace = page.getByRole("list", { name: "What happened" });
  await expect(receipt.locator(".eyebrow")).toHaveText("Included, but failed");
  await expect(receipt.locator(".pill.failed")).toHaveText(["failed"]);
  await expect(receipt).toContainText("in block #99");
  await expect(trace.locator("li").filter({ hasText: /^Sealed/ })).toContainText("in block #99");
  await expect(trace.locator("li").filter({ hasText: /^Sealed/ })).toContainText("where it failed and its fee was charged: insufficient balance");
  await expect(receipt).toContainText("It ran in a block and failed there, so its fee was charged");
  for (const unrun of ["dropped", "may still include", "nothing was charged", "not final"]) await expect(page.locator("main")).not.toContainText(unrun);
  expect(await cspViolations(page)).toEqual([]);
});
