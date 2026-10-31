// lib/explorer/inert.ts: what a program or its deployer chose, made fit to show. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { abridged, inertText, shortAddress, signedAction, typeText } from "./explorer/inert.ts";

test("inertText removes what hides or reorders text, collapses space and cuts by code point", () => {
  assert.equal(inertText("a‮b\u0007​c\nd e"), "a b c d e");
  assert.equal(inertText("<script>alert(1)</script>"), "<script>alert(1)</script>", "markup is left to the renderer to escape: it's only text here");
  assert.equal(inertText("x".repeat(70)), `${"x".repeat(64)}…`);
  assert.equal(inertText("😀".repeat(70), 3), "😀😀😀…", "never splits a surrogate pair");
  assert.equal(inertText(null), "");
  assert.equal(inertText({ toString: () => "\u{E0041}tag" }), "tag", "Unicode tag characters go too");
});

test("typeText reads the interface's closed vocabulary, and nothing else", () => {
  assert.equal(typeText("u64"), "u64");
  assert.equal(typeText({ vec: ["address", 8] }), "vec<address, 8>");
  assert.equal(typeText({ option: { vec: [{ bytes: 32 }, 4] } }), "option<vec<bytes(32), 4>>");
  assert.equal(typeText({ text: 64 }), "text(64)");
  assert.equal(typeText({ nope: 1 }), "?");
  assert.equal(typeText(null), "?");
  assert.equal(typeText({ option: { option: { option: { option: { option: { option: "u8" } } } } } }), "option<option<option<option<option<?>>>>>", "nesting is bounded");
  assert.equal(typeText("<img src=x onerror=1>".repeat(3)).length <= 17, true, "a scalar name is cut");
});

test("abridged cuts long strings and writes hidden characters out", () => {
  const a = JSON.parse(abridged({ wasm: "ab".repeat(5000), short: "ok" }));
  assert.match(a.wasm, /^abababab.*… \(10,000 characters\)$/);
  assert.equal(a.short, "ok");
  assert.equal(abridged({ m: "a‮b" }), '{\n  "m": "a\\\\u202eb"\n}');
  assert.ok(!abridged({ m: "a‮b​c" }).match(/[‮​]/));
});

test("abridged cuts a program's blobs only: a key, a proof or any other field stays whole", () => {
  const a = JSON.parse(abridged({ wasm: "ab".repeat(5000), interface: "cd".repeat(500), input: "ef".repeat(500), data: "01".repeat(500), return_data: "02".repeat(500), public_key: "ab".repeat(1952), pop: "cd".repeat(3309), url: "u".repeat(300) }));
  for (const k of ["wasm", "interface", "input", "data", "return_data"]) assert.match(a[k], /… \(\d[\d,]* characters\)$/, k);
  assert.equal(a.public_key, "ab".repeat(1952));
  assert.equal(a.pop, "cd".repeat(3309));
  assert.equal(a.url, "u".repeat(300), "even a long url is whole");
});

test("signedAction: a program action's blobs are abridged, every other action prints exactly as JSON.stringify does", () => {
  const addKey = { type: "add_key", key_id: "ab".repeat(32), label: "phone", role: { type: "owner" }, public_key: "0c".repeat(1952), pop: "0d".repeat(3309) };
  assert.equal(signedAction(addKey), JSON.stringify(addKey, null, 2), "a v1 action is byte for byte what it was");
  assert.ok(signedAction(addKey).includes("0c".repeat(1952)) && signedAction(addKey).includes("0d".repeat(3309)), "the whole key and its proof");
  const propose = { type: "propose", kind: { type: "param_change", key: "x", value: "1" }, title: "t", url: "https://example.org/" + "a".repeat(200), deposit: "10" };
  assert.equal(signedAction(propose), JSON.stringify(propose, null, 2), "a proposal's link is whole");
  const evidence = { type: "report_equivocation", evidence: "ef".repeat(2000) };
  assert.equal(signedAction(evidence), JSON.stringify(evidence, null, 2));
  // The same field names on a v1 action aren't cut: only the program actions' are.
  const v1WithAProgramField = { type: "transfer", to: "x.boo", amount: "1", input: "ab".repeat(500), data: "cd".repeat(500) };
  assert.equal(signedAction(v1WithAProgramField), JSON.stringify(v1WithAProgramField, null, 2));
  const upload = { type: "upload_code", wasm: "ab".repeat(5000), interface: "cd".repeat(400), storage_limit: "1" };
  const shown = JSON.parse(signedAction(upload));
  assert.match(shown.wasm, /\(10,000 characters\)$/);
  assert.match(shown.interface, /\(800 characters\)$/);
  assert.equal(shown.storage_limit, "1");
  const call = { type: "call_program", calls: [{ program: "boo1x", version_digest: "00".repeat(32), method: "get", input: "ab".repeat(500), value: "0" }], fuel_limit: 1, storage_limit: "0", bindings: [] };
  const c = JSON.parse(signedAction(call));
  assert.match(c.calls[0].input, /\(1,000 characters\)$/);
  assert.equal(c.calls[0].version_digest, "00".repeat(32), "the digest a call binds is whole");
  // Exactly as before means exactly: a hidden character in a v1 string is as JSON.stringify writes it (no \\uXXXX), a program action's long non-blob field is whole.
  const hidden = { type: "propose", kind: { type: "param_change", key: "x", value: "1" }, title: "a\u202eb", url: "", deposit: "10" };
  assert.equal(signedAction(hidden), JSON.stringify(hidden, null, 2));
  assert.ok(signedAction(hidden).includes("\u202e"));
  const longAuthority = { type: "create_program", wasm_hash: "00".repeat(32), interface_hash: "11".repeat(32), authority: "x".repeat(300), upgrade_delay_blocks: 0, policy: "open", accepts_transfers: false, init: null, fuel_limit: 0, storage_limit: "0", bindings: [] };
  assert.equal(JSON.parse(signedAction(longAuthority)).authority, "x".repeat(300), "only the blobs of a program action are cut");
  assert.equal(signedAction(null), "null");
  assert.equal(signedAction(undefined), undefined);
});

test("shortAddress is the node's short form", () => {
  assert.equal(shortAddress("boo1juzwlx65f3vp0s6v5aj46pjqr7h5m573uksm2k6ply8hgsmuqqpq949pt3"), "boo1juzwlx…949pt3");
  assert.equal(shortAddress("short"), "short");
});
