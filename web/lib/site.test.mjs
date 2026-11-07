// The project site's rules (lib/site.ts): which node addresses a visitor may open, how a pasted
// link becomes the node it points into, the demo node's build-time check, and the contributor
// prompt staying word for word what CONTRIBUTING.md says. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { CONTRIBUTE_PROMPT, demoNode, isPrivateHost, nodeAddress, walletOf } from "./site.ts";

test("https anywhere, plain http only on this machine or a private network (mcp/src/network.ts's rule)", () => {
  for (const [input, url] of [
    ["https://chain.example", "https://chain.example"],
    ["https://Chain.Example:443/", "https://chain.example"],
    ["chain.example", "https://chain.example"],
    ["  https://jack.tail1234.ts.net/wallet/  ", "https://jack.tail1234.ts.net"],
    ["https://chain.example/#/start/invite", "https://chain.example"],
    ["https://chain.example/api/status?x=1", "https://chain.example"],
    ["https://chain.example/blockula/wallet/send?to=sam.boo", "https://chain.example/blockula"],
    ["http://localhost:7070", "http://localhost:7070"],
    ["localhost:7070", "http://localhost:7070"],
    ["http://127.0.0.1:7070/wallet", "http://127.0.0.1:7070"],
    ["http://192.168.1.20:7070", "http://192.168.1.20:7070"],
    ["10.0.0.5:7070", "http://10.0.0.5:7070"],
    ["100.101.2.3:7070", "http://100.101.2.3:7070"],
    ["http://jack.tail1234.ts.net:7070", "http://jack.tail1234.ts.net:7070"],
    ["http://box.local:7070", "http://box.local:7070"],
  ]) assert.deepEqual(nodeAddress(input), { url }, input);

  for (const [input, says] of [
    ["", /Paste/],
    ["hello world", /spaces/],
    ["http://chain.example", /plain http/],
    ["8.8.8.8:7070", /plain http/],
    ["http://100.128.0.1:7070", /plain http/],
    ["http://172.32.0.1", /plain http/],
    ["ftp://chain.example", /https/],
    ["javascript:alert(1)", /web address|https/],
    ["https://user:pw@chain.example", /password/],
    ["https://", /web address/],
  ]) assert.match(nodeAddress(input).error ?? "", says, input);
});

test("private hosts", () => {
  for (const h of ["localhost", "127.0.0.1", "[::1]", "10.1.2.3", "172.16.0.1", "172.31.255.1", "192.168.0.1", "100.64.0.1", "100.127.1.1", "x.ts.net", "nas.lan", "box.internal"]) assert.ok(isPrivateHost(h), h);
  for (const h of ["chain.example", "8.8.8.8", "172.15.0.1", "100.63.0.1", "100.128.0.1", "ts.net.example.com", "localhost.example"]) assert.ok(!isPrivateHost(h), h);
});

test("the wallet a node serves, and the demo node's check", () => {
  assert.equal(walletOf("https://chain.example"), "https://chain.example/wallet/");
  assert.equal(walletOf("https://chain.example/"), "https://chain.example/wallet/");
  assert.equal(demoNode(undefined), undefined);
  assert.equal(demoNode("  "), undefined);
  assert.equal(demoNode("https://demo.blockula.app/"), "https://demo.blockula.app");
  assert.throws(() => demoNode("http://demo.blockula.app"), /NEXT_PUBLIC_DEMO_NODE/);
  assert.throws(() => demoNode("not a url"), /NEXT_PUBLIC_DEMO_NODE/);
});

test("the contributor prompt is CONTRIBUTING.md's", () => {
  const md = readFileSync(new URL("../../CONTRIBUTING.md", import.meta.url), "utf8");
  const quoted = /## Start in two minutes[\s\S]*?\n> (.+)\n/.exec(md)?.[1];
  assert.equal(CONTRIBUTE_PROMPT, quoted);
});
