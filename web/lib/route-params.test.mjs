// lib/route-params.ts: the served wallet's one page per dynamic route, and reading its id from
// the URL. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { paramFromPath, servedStaticParams, PLACEHOLDER } from "./route-params.ts";

test("the served build lists one placeholder page per dynamic route; other builds have no generateStaticParams", () => {
  const was = process.env.NEXT_PUBLIC_BLOCKULA_SERVED;
  try {
    delete process.env.NEXT_PUBLIC_BLOCKULA_SERVED;
    assert.equal(servedStaticParams("id"), undefined, "not even one returning []: that makes the route static");
    process.env.NEXT_PUBLIC_BLOCKULA_SERVED = "1";
    assert.deepEqual(servedStaticParams("id")?.(), [{ id: PLACEHOLDER }]);
    assert.deepEqual(servedStaticParams("height")?.(), [{ height: "_" }]);
  } finally {
    if (was === undefined) delete process.env.NEXT_PUBLIC_BLOCKULA_SERVED;
    else process.env.NEXT_PUBLIC_BLOCKULA_SERVED = was;
  }
});

test("the id is the URL's segment after the route's section", () => {
  assert.equal(paramFromPath("/wallet/tx/ab12/", "/tx/_", "/wallet"), "ab12");
  assert.equal(paramFromPath("/wallet/account/alice.boo", "/account/_", "/wallet"), "alice.boo");
  assert.equal(paramFromPath("/wallet/name/vlad%20x/", "/name/vlad%20x", "/wallet"), "vlad%20x", "still escaped, as useParams gives it");
  assert.equal(paramFromPath("/block/12", "/block/12", ""), "12");
  // Mid-navigation the browser can still be on another page: no id from it.
  assert.equal(paramFromPath("/wallet/explore/", "/block/1", "/wallet"), null);
  assert.equal(paramFromPath("/wallet/tx/", "/tx/_", "/wallet"), null);
  assert.equal(paramFromPath("/wallet/", "/", "/wallet"), null);
});
