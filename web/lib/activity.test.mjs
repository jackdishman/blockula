import assert from "node:assert/strict";
import { test } from "node:test";
import { filterActivity, scanActivity } from "./explorer/activity.ts";

const tx = (id, extra = {}) => ({ id, kind: "register_name", summary: `${id}.boo registered`, status: "finalized", height: 10, fee: "1000", ...extra });
const options = { call: "all", status: "all", search: "", sort: "newest" };

test("activity searches names, summaries, memos and ids and combines action/status filters", () => {
  const txs = [tx("bat", { status: "failed", memo: "too late" }), tx("lucy", { kind: "renew_name", from: { name: "lucy.boo" } }), tx("hidden", { action: { type: "register_name", name: "castle" } })];
  assert.deepEqual(filterActivity(txs, { ...options, search: "  LUCY.BOO " }).map((t) => t.id), ["lucy"]);
  assert.deepEqual(filterActivity(txs, { ...options, search: "too late", status: "failed", call: "register_name" }).map((t) => t.id), ["bat"]);
  assert.deepEqual(filterActivity(txs, { ...options, search: "castle" }).map((t) => t.id), ["hidden"]);
  assert.deepEqual(filterActivity(txs, { ...options, status: "failed", call: "renew_name" }), []);
  assert.deepEqual(filterActivity(txs, { ...options, search: "hidden" }).map((t) => t.id), ["hidden"]);
});

test("sorts exact fees beyond Number precision and keeps uncharged pending caps last", () => {
  const txs = [tx("pending", { status: "pending", height: null, fee: "1" }), tx("high", { fee: "9007199254740993" }), tx("low", { fee: "9007199254740992" })];
  assert.deepEqual(filterActivity(txs, { ...options, sort: "fee-low" }).map((t) => t.id), ["low", "high", "pending"]);
  assert.deepEqual(filterActivity(txs, { ...options, sort: "fee-high" }).map((t) => t.id), ["high", "low", "pending"]);
  assert.deepEqual(filterActivity(txs, { ...options, sort: "oldest" }).map((t) => t.id), ["low", "high", "pending"]);
  assert.equal(txs[0].id, "pending");
});

function reader(blocks, historyFrom = 1) {
  const requests = [];
  const get = async (path) => {
    requests.push(path);
    if (path.startsWith("/blocks?")) {
      const before = Number(new URL(`https://node.test${path}`).searchParams.get("before") ?? Infinity);
      return blocks.filter((b) => b.height < before && b.height >= historyFrom).slice(0, 100).map(({ txs, ...b }) => ({ ...b, tx_count: txs.length }));
    }
    const height = Number(path.split("/").at(-1));
    return blocks.find((b) => b.height === height);
  };
  return { get, requests };
}

test("scans past unrelated transactions, skips empty blocks, and keeps every call in the last block", async () => {
  const blocks = Array.from({ length: 210 }, (_, i) => ({ height: 210 - i, txs: [] }));
  blocks[0].txs = Array.from({ length: 60 }, (_, i) => tx(`transfer-${i}`, { kind: "transfer" }));
  blocks[120].txs = [tx("first"), tx("second")];
  blocks[205].txs = [tx("older", { kind: undefined, action: { type: "register_name" } })];
  const { get, requests } = reader(blocks);
  const page = await scanActivity(get, ["register_name"], { target: 1 });
  assert.deepEqual(page.txs.map((t) => t.id), ["second", "first"]);
  assert.equal(page.next, 11);
  assert.equal(page.exhausted, false);
  assert.deepEqual(requests.filter((p) => p.startsWith("/block/")), ["/block/210", "/block/90"]);
  const older = await scanActivity(get, ["register_name"], { before: page.next });
  assert.deepEqual(older.txs.map((t) => t.id), ["older"]);
  assert.equal(older.exhausted, true);
});

test("stops at a pruned boundary and refreshes only blocks above the known head", async () => {
  const blocks = [{ height: 102, txs: [tx("new")] }, { height: 101, txs: [tx("old")] }];
  const { get, requests } = reader(blocks, 101);
  const fresh = await scanActivity(get, ["register_name"], { after: 101 });
  assert.deepEqual(fresh.txs.map((t) => t.id), ["new"]);
  assert.ok(!requests.includes("/block/101"));
  const all = await scanActivity(get, ["register_name"]);
  assert.equal(all.exhausted, true);
  assert.equal(all.oldest, 101);
});

test("a failed block read does not silently omit transactions", async () => {
  const { get } = reader([{ height: 3, txs: [tx("three")] }, { height: 2, txs: [tx("two")] }]);
  await assert.rejects(scanActivity(async (path) => {
    if (path === "/block/2") throw new Error("node unavailable");
    return get(path);
  }, ["register_name"]), /node unavailable/);
});

test("bounds full-block reads on a busy unrelated chain without skipping the continuation", async () => {
  const blocks = Array.from({ length: 100 }, (_, i) => ({ height: 100 - i, txs: [tx(`unrelated-${i}`, { kind: "transfer" })] }));
  blocks[45].txs = [tx("name-call")];
  const { get, requests } = reader(blocks);
  const first = await scanActivity(get, ["register_name"]);
  assert.equal(first.txs.length, 0);
  assert.equal(requests.filter((p) => p.startsWith("/block/")).length, 40);
  assert.equal(first.next, 61);
  const second = await scanActivity(get, ["register_name"], { before: first.next });
  assert.deepEqual(second.txs.map((t) => t.id), ["name-call"]);
});
