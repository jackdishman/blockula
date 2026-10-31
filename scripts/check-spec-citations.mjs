#!/usr/bin/env node
// Keeps the code's references to docs/PROGRAMS-V0.md pointing at something. It reads three kinds:
//
//   1. Citations that name the file: "docs/PROGRAMS-V0.md §6.2", "PROGRAMS-V0.md 5.2 item 3",
//      "PROGRAMS-V0.md section 0 and 9.3", "docs/PROGRAMS-V0.md §4.4, §5.3 and A40", in any tracked
//      text file. The text "PROGRAMS-V0" (with or without ".md") is followed, straight after it
//      (across a line break and a comment marker too), by a section ("§6.2", or a bare "5.2") or an
//      amendment ("A30"), and by any more of either joined to it by a comma, semicolon, slash, "and",
//      "or", "to" or "item N". Nothing past the first thing that isn't one.
//   2. Bare references in the programs code: every "§n.m" and every amendment id ("A30", on a
//      comment line) in node/src/programs, node/vm, node/fuzz/src, sdk-ts/src and mcp/src is read as
//      a reference to this spec, unless another document's name stands right before it ("PRG §9",
//      "docs/PROGRAMS.md §9": PRG, PROGRAMS.md, PROGRAMS-PLAN, MAINNET-DESIGN, WBS, PLAN.md,
//      GHOST-MODE, the commonware docs, ...). Those are counted and skipped.
//   3. The spec's own "§n" references before its appendix (whose rows also cite the build plan's
//      sections) and its own amendment ids.
//
// "item n" and "step n" ("§5.2 item 3", "§6.1 steps 6.1-6.4", "§2.3 step 2") are checked against the
// numbered lists of the section they follow: "3" is the third item of a list in the section, "6.4"
// the fourth item nested under its sixth.
//
// What it does NOT read: prose references with no "§", no file name and no amendment id ("see the
// spec's section on faults"); bare references outside the directories above (web/, scripts/, docs
// other than the spec); a line that names another document, whatever else it cites; markdown
// anchors (`PROGRAMS-V0.md#...`); whether a cited section still says what the citer means. It
// checks that the numbers exist, so a section keeps its number.
//
//   node scripts/check-spec-citations.mjs          # exit 0 when every reference resolves
import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(process.env.SPEC_CHECK_ROOT ?? fileURLToPath(new URL("..", import.meta.url)));
const SPEC = "docs/PROGRAMS-V0.md";
const SELF = "scripts/check-spec-citations.mjs";
// Where a bare "§n.m" or "A<n>" is this spec's.
const SCOPE = ["node/src/programs/", "node/vm/", "node/fuzz/src/", "sdk-ts/src/", "mcp/src/"];
// A "§n" right after one of these (at most 12 characters, none of them a "§", between) is that
// document's. (Not "PROGRAMS-V0": that is this one.)
const OTHER_DOC = /\b(?:PRG|PROGRAMS\.md|PROGRAMS-PLAN|MAINNET-DESIGN|MAINNET\.md|WBS|PLAN\.md|GHOST-MODE|GHOST\.md|OPERATIONS\.md|GOVERNANCE\.md|REPLAY\.md|phase0|commonware|RFC|FIPS|simplex)\b/i;

const AFTER_OTHER_DOC = new RegExp(`(?:${OTHER_DOC.source})[^§]{0,12}$`, "i");

/** The section numbers ("0", "1.7", "10.6"), amendment ids ("A1") and numbered-list items a spec defines. */
export function targets(text) {
  const sections = new Set();
  const amendments = new Set();
  const items = new Map(); // section id -> Set of "3", "6.4", ...
  let section = null;
  let fence = false;
  let stack = []; // [{ indent, id }]
  for (const line of text.split("\n")) {
    if (/^\s*```/.test(line)) fence = !fence;
    if (fence) continue;
    const h = /^#{2,4}\s+(\d+(?:\.\d+)*)\.?(?:\s|$)/.exec(line);
    if (h) {
      sections.add(h[1]);
      section = h[1];
      items.set(section, new Set());
      stack = [];
    } else if (/^#{1,4}\s/.test(line)) {
      section = null;
      stack = [];
    }
    const a = /^\|\s*(A\d+)\s*\|/.exec(line);
    if (a) amendments.add(a[1]);
    const n = section && /^( *)(\d+)\.\s+\S/.exec(line);
    if (n) {
      const indent = n[1].length;
      while (stack.length && stack[stack.length - 1].indent >= indent) stack.pop();
      const id = stack.length ? `${stack[stack.length - 1].id}.${n[2]}` : n[2];
      items.get(section).add(id);
      stack.push({ indent, id });
    } else if (section && line.trim() === "") {
      // A blank line between list items keeps the list.
    } else if (section && /^\S/.test(line) && !/^\d+\./.test(line)) {
      stack = []; // a paragraph at column 0 ends the list
    }
  }
  return { sections, amendments, items };
}

const NUM = "\\d+(?:\\.\\d+)*";
const REF = `(?:§\\s*${NUM}|A\\d+)`;
const RANGE = `${NUM}(?:\\s*[–-]\\s*${NUM})?`;
// "item 3", "steps 6.1–6.4", "step 6", "items 2, 4 and 5": what may follow a section reference.
const ITEMS_RE = `(?:items?|steps?)\\s+${RANGE}(?:\\s*(?:,|and)\\s*${RANGE})*`;
const ITEMS = new RegExp(`^\\s*,?\\s*(?:and\\s+)?(${ITEMS_RE})`);
// After the file name: closing marks and spaces, an optional line break and comment marker, then a
// first reference (a bare number allowed), each with an optional items clause, then more references
// each joined to the one before.
const CHAIN = new RegExp(
  `^[\\s\`'")\\],:(]*(?:${NUM}|${REF})(?:\\s*,?\\s*${ITEMS_RE})?(?:\\s*(?:,|;|/|&|and|or|to|–|-)\\s*${REF}(?:\\s*,?\\s*${ITEMS_RE})?)*`,
);
// "docs/PROGRAMS-V0.md section 0 and 9.3": the word "section" makes bare numbers references too.
const WORD = new RegExp(
  `^[\\s\`'")\\],:(]*sections?\\s+(?:${NUM}|A\\d+)(?:\\s*(?:,|;|/|&|and|or|to|–|-)\\s*(?:§\\s*)?(?:${NUM}|A\\d+))*`,
);
const WRAP = /\n[ \t]*(?:\/\/[\/!]?|#|\*|--)?[ \t]?/g;

/** The item or step numbers named right after a section reference ("item 3", "steps 6.1–6.4"). */
export function itemsAfter(text) {
  const m = ITEMS.exec(text.slice(0, 120));
  return m ? [...m[1].replace(/^(?:items?|steps?)\s+/, "").matchAll(new RegExp(NUM, "g"))].map((x) => x[0]) : [];
}

/** The references one citation names, from the text just after "PROGRAMS-V0[.md]". */
export function cited(after) {
  // Join a wrapped comment line to the one before it.
  const flat = after.slice(0, 400).replace(WRAP, " ");
  const word = WORD.exec(flat);
  if (word) {
    return [...word[0].replace(/^[^s]*sections?\s+/, "").matchAll(new RegExp(`${NUM}|A\\d+`, "g"))].map((r) =>
      r[0].startsWith("A") ? { kind: "amendment", id: r[0] } : { kind: "section", id: r[0] },
    );
  }
  const m = CHAIN.exec(flat);
  if (!m) return [];
  const out = [];
  const first = /^[\s`'")\],:(]*(\d+(?:\.\d+)*)(?![\d.]*[A-Za-z§])/.exec(m[0]);
  let rest = m[0];
  if (first && !/§|A\d/.test(m[0].slice(0, first[0].length))) {
    out.push({ kind: "section", id: first[1], at: first[0].length });
    rest = m[0].slice(first[0].length);
  }
  const base = m[0].length - rest.length;
  for (const r of rest.matchAll(/§\s*(\d+(?:\.\d+)*)|\b(A\d+)\b/g)) {
    const at = base + r.index + r[0].length;
    out.push(r[1] ? { kind: "section", id: r[1], at } : { kind: "amendment", id: r[2], at });
  }
  // Item and step numbers named after a section.
  for (const o of out) {
    if (o.kind === "section") o.items = itemsAfter(flat.slice(o.at));
  }
  return out;
}

/** Bare references on one line of the programs code: "§6.1 step 4", "A30" (comment text only). A
 *  "§n" right after another document's name is not this spec's: `skipped` counts those. */
export function bare(line, code, skipped = { n: 0 }) {
  // In a source file, only the comment part of a line; in a markdown file, the whole line.
  let text = line;
  if (code) {
    const c = /(?:\/\/|\/\*|^\s*\*|^\s*#)/.exec(line);
    if (!c) return [];
    text = line.slice(c.index);
  }
  const out = [];
  for (const r of text.matchAll(/§\s*(\d+(?:\.\d+)*)/g)) {
    if (AFTER_OTHER_DOC.test(text.slice(Math.max(0, r.index - 60), r.index))) {
      skipped.n++;
      continue;
    }
    out.push({ kind: "section", id: r[1], items: itemsAfter(text.slice(r.index + r[0].length)) });
  }
  for (const r of text.matchAll(/(?<![\w`/.-])(A\d{1,2})(?![\w])/g)) out.push({ kind: "amendment", id: r[1] });
  return out;
}

function selfTest() {
  const eq = (got, want) => {
    const g = JSON.stringify(got), w = JSON.stringify(want);
    if (g !== w) throw new Error(`check-spec-citations self-test: got ${g}, want ${w}`);
  };
  const ids = (xs) => xs.map((x) => (x.items && x.items.length ? `${x.id}[${x.items.join(",")}]` : x.id));
  eq(ids(cited(" §6.2, which")), ["6.2"]);
  eq(ids(cited(" 5.2 item 3), the")), ["5.2[3]"]);
  eq(ids(cited(" §7.1, A35, A40).")), ["7.1", "A35", "A40"]);
  eq(ids(cited(" A30): the VM")), ["A30"]);
  eq(ids(cited(" §4.4, §5.3 and A40; then §9")), ["4.4", "5.3", "A40"]);
  eq(ids(cited("\n//! §3.2, the")), ["3.2"]);
  eq(ids(cited(" and its §3")), []);
  eq(ids(cited("` §5.2 item 3's first")), ["5.2[3]"]);
  eq(ids(cited(" section 0 and 9.3 (the kit)")), ["0", "9.3"]);
  eq(ids(cited(" section 10.6 there lists")), ["10.6"]);
  eq(ids(cited(" sections 0 and 9.3), the")), ["0", "9.3"]);
  eq(ids(cited(" §6.1 steps 6.1–6.4, §10.4")), ["6.1[6.1,6.4]", "10.4"]);
  eq(ids(cited(" §6.1 steps 3-4; WBS")), ["6.1[3,4]"]);
  eq(ids(cited(" §5.2 item 77, §6.1 step 99")), ["5.2[77]", "6.1[99]"]);
  eq(ids(bare("    // the cap (§7.1, A40) and §6.1 step 4", true)), ["7.1", "6.1[4]", "A40"]);
  eq(ids(bare("let a = A1;", true)), []);
  eq(ids(bare("let x = 0xA1; // fine", true)), []);
  eq(ids(bare("Spec §5.2 item 3 and A3", false)), ["5.2[3]", "A3"]);
  eq(ids(bare("docs/PROGRAMS.md §9.2, but §7.1 here (PRG 5.2's test, §5.6)", false)), ["7.1", "5.6"]);
  eq(ids(bare("PRG §14.2 and MAINNET-DESIGN §3.9", false)), []);
  eq(ids(bare("see MAINNET-DESIGN §3.9 and WBS S4c; none consensus, §9.2", false)), ["9.2"]);
  const spec = "## 5. Fuel\n### 5.2 Schedule\n\n1. one\n2. two\n3. three\n   - bullet\n\n### 6.1 How\n\n1. a\n2. b\n6. f\n   1. f1\n   2. f2\n   6. f6\n\nText.\n\n1. again\n";
  const t = targets("## 0. Scope\n### 1.7 `x` rule\n## 3. Interface (C3)\n| A1 | x |\n| A12 | y |\n#### 4.4.1 z\n" + spec);
  eq([...t.sections], ["0", "1.7", "3", "4.4.1", "5", "5.2", "6.1"]);
  eq([...t.amendments], ["A1", "A12"]);
  eq([...t.items.get("5.2")], ["1", "2", "3"]);
  eq([...t.items.get("6.1")], ["1", "2", "6", "6.1", "6.2", "6.6"]);
  eq(OTHER_DOC.test("PRG 14.2") && !OTHER_DOC.test("docs/PROGRAMS-V0.md §9.3") && OTHER_DOC.test("docs/PROGRAMS.md §9.3"), true);
}

function files() {
  try {
    const out = execFileSync("git", ["ls-files", "-z"], { cwd: ROOT, maxBuffer: 1 << 28 }).toString("utf8");
    return out.split("\0").filter(Boolean);
  } catch {
    const all = [];
    const walk = (d) => {
      for (const e of readdirSync(join(ROOT, d))) {
        if ([".git", "node_modules", "target", ".next"].includes(e)) continue;
        const rel = d ? `${d}/${e}` : e;
        if (statSync(join(ROOT, rel)).isDirectory()) walk(rel);
        else all.push(rel);
      }
    };
    walk("");
    return all;
  }
}

function text(rel) {
  let buf;
  try {
    const st = statSync(join(ROOT, rel));
    if (!st.isFile() || st.size > 8 << 20) return null;
    buf = readFileSync(join(ROOT, rel));
  } catch {
    return null;
  }
  if (buf.subarray(0, 8000).includes(0)) return null;
  return buf.toString("utf8");
}

function lineOf(s, index) {
  let n = 1;
  for (let i = s.indexOf("\n"); i !== -1 && i < index; i = s.indexOf("\n", i + 1)) n++;
  return n;
}

selfTest();
const spec = readFileSync(join(ROOT, SPEC), "utf8");
const { sections, amendments, items } = targets(spec);
if (sections.size < 30 || amendments.size < 30) {
  console.error(`${SPEC}: found only ${sections.size} sections and ${amendments.size} amendments; its headings or amendment table changed shape`);
  process.exit(1);
}

const bad = [];
let count = 0;
let itemCount = 0;
const check = (where, refs) => {
  for (const r of refs) {
    count++;
    const ok = r.kind === "section" ? sections.has(r.id) : amendments.has(r.id);
    if (!ok) {
      bad.push(`${where}: cites ${r.kind === "section" ? "§" : ""}${r.id}, which ${SPEC} doesn't have`);
      continue;
    }
    for (const i of r.items ?? []) {
      itemCount++;
      if (!items.get(r.id)?.has(i)) bad.push(`${where}: cites §${r.id} item or step ${i}, which that section's numbered lists don't have`);
    }
  }
};

const filed = new Set();
let bareCount = 0;
let exempt = 0;
for (const rel of files()) {
  if (rel === SELF) continue;
  const s = text(rel);
  if (s === null) continue;
  if (rel !== SPEC && s.includes("PROGRAMS-V0")) {
    for (const m of s.matchAll(/PROGRAMS-V0(?:\.md)?/g)) {
      check(`${rel}:${lineOf(s, m.index)}`, cited(s.slice(m.index + m[0].length)));
      filed.add(rel);
    }
  }
  if (rel !== SPEC && SCOPE.some((p) => rel.startsWith(p))) {
    const code = !rel.endsWith(".md");
    const skipped = { n: 0 };
    s.split("\n").forEach((line, i) => {
      const refs = bare(line, code, skipped);
      bareCount += refs.length;
      check(`${rel}:${i + 1}`, refs);
    });
    exempt += skipped.n;
  }
}

// The spec's own "§n" before its appendix, with their item and step numbers.
const body = spec.slice(0, spec.indexOf("\n## Appendix"));
for (const m of body.matchAll(/§\s*(\d+(?:\.\d+)*)/g)) {
  check(`${SPEC}:${lineOf(spec, m.index)}`, [{ kind: "section", id: m[1], items: itemsAfter(body.slice(m.index + m[0].length)) }]);
}
// Its own amendment ids ("A27") before the appendix, where they name its table's rows.
for (const m of body.matchAll(/(?<![\w`/-])(A\d+)\b(?!\w)/g)) {
  const id = m[1];
  if (id.length <= 4 && !amendments.has(id) && Number(id.slice(1)) <= 99) {
    bad.push(`${SPEC}:${lineOf(spec, m.index)}: mentions ${id}, which isn't in its amendment table`);
  }
}

if (bad.length) {
  const unique = [...new Set(bad)]; // a line that names the spec is read twice, as a citation and as bare references
  console.error(unique.join("\n"));
  console.error(`\n${unique.length} reference(s) to ${SPEC} name something it doesn't have.`);
  process.exit(1);
}
console.log(
  `ok: ${count} references resolve (${filed.size} files name the spec; ${bareCount} bare § and amendment references in the programs code, ${exempt} skipped for following another document's name; ${itemCount} item and step numbers; the spec's own) to ${sections.size} sections and ${amendments.size} amendments of ${relative(ROOT, join(ROOT, SPEC))}`,
);
