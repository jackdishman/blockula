// lib/explorer/guestbook.ts: which payments sign the guestbook, and their memos made fit to show. Run with `npm test -w @blockula/web`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { MEMO_MAX, signaturesOf } from "./explorer/guestbook.ts";

const BOOK = "boo1book";
const who = (address, name = null) => ({ address, name, short: address });
const pay = (id, from, to, o = {}) => ({ id, summary: "", kind: "transfer", status: "finalized", error: null, height: 10, from: who(from), to: who(to), amount: "100000000", amount_display: "0.1 FANG", memo: "", ...o });

test("a signature is a finalized transfer that paid the book's account from someone else", () => {
  const txs = [
    pay("a", "boo1vlad", BOOK, { memo: "hi" }),
    pay("b", BOOK, "boo1mina", { memo: "it sent this" }),
    pay("c", "boo1lucy", BOOK, { status: "failed", error: "insufficient balance" }),
    pay("d", "boo1lucy", BOOK, { status: "pending", height: null }),
    pay("e", "boo1lucy", BOOK, { kind: "register_name" }),
    pay("f", "boo1lucy", "boo1other"),
    pay("g", "boo1mina", BOOK),
    pay("a", "boo1vlad", BOOK, { memo: "hi" }),
  ];
  assert.deepEqual(signaturesOf(txs, BOOK).map((s) => s.id), ["a", "g"], "in order, each once");
});

test("memos are text with nothing hidden, cut short", () => {
  const [hostile, sneaky, long, none] = signaturesOf(
    [
      pay("1", "boo1x", BOOK, { memo: '<img src=x onerror="alert(1)">' }),
      pay("2", "boo1x", BOOK, { memo: "a‮b\u0007\nc" }),
      pay("3", "boo1x", BOOK, { memo: "z".repeat(250) }),
      pay("4", "boo1x", BOOK, { memo: undefined }),
    ],
    BOOK,
  );
  assert.equal(hostile.memo, '<img src=x onerror="alert(1)">', "markup is left for React to escape");
  assert.equal(sneaky.memo, "a b c");
  assert.equal([...long.memo].length, MEMO_MAX + 1);
  assert.ok(long.memo.endsWith("…"));
  assert.equal(none.memo, "");
});
