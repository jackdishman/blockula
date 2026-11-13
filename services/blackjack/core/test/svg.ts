// The drawings, offline: well-formed, deterministic, safe to inline under a strict CSP, and
// laid out so the longest hands stay on the sheet.
import assert from "node:assert/strict";
import {
  CARD_HEIGHT,
  CARD_WIDTH,
  DEALER_ROW_Y,
  DEFAULT_FONTS,
  MAX_DEALER_CARDS,
  MAX_PLAYER_CARDS,
  MAX_PLAYER_LABEL,
  NOTE,
  Outcome,
  PLAYER_ROW_Y,
  POSTER_ALT,
  Phase,
  ROW_LEFT,
  ROW_RIGHT,
  TABLE_CARD_SCALE,
  TABLE_HEIGHT,
  TABLE_WIDTH,
  TAGLINE,
  THEME,
  batFragment,
  cardBackFragment,
  cardBackSvg,
  cardFaceFragment,
  cardFaceSvg,
  cardLabel,
  footerLines,
  payout,
  phaseWords,
  posterSvg,
  rowLayout,
  shortTx,
  stampWords,
  statusLine,
  tableSvg,
  truncate,
  type TableSnapshot,
} from "../src/index.js";

const FANG = 1_000_000_000n;
const TX = "9f2c41d07ab35e6610c4d8e2f7b90a13c5e87d24f6a1b0c39d7e52f48a6b1c0d";
const base: TableSnapshot = { gameId: 42n, playerCards: [8, 45], dealerCards: [12], holeHidden: true, stake: 5n * FANG, phase: Phase.Player, outcome: null, payout: null, txId: TX, player: "mina.boo" };
const settled = (outcome: Outcome, over: Partial<TableSnapshot> = {}): TableSnapshot => ({ ...base, dealerCards: [12, 20], holeHidden: false, phase: Phase.Done, outcome, payout: payout(base.stake, outcome), ...over });
const longHand: TableSnapshot = { ...base, playerCards: Array.from({ length: MAX_PLAYER_CARDS }, (_, i) => (i * 13) % 52), dealerCards: Array.from({ length: MAX_DEALER_CARDS }, (_, i) => (i * 14) % 52), holeHidden: false, phase: Phase.Dealer };
const empty: TableSnapshot = { ...base, playerCards: [], dealerCards: [], holeHidden: false, phase: Phase.Deal, txId: undefined, player: undefined };
const outcomes = Object.values(Outcome);
const tables: TableSnapshot[] = [base, empty, longHand, { ...base, phase: Phase.Hit }, { ...base, dealerCards: [12, 20], holeHidden: false, phase: Phase.Dealer }, ...outcomes.map((o) => settled(o)), settled(Outcome.Bust, { dealerCards: [12], holeHidden: true })];

/** Checks `doc` is one well-formed element: balanced tags, quoted attributes, no bare `<` or `&` in text. Returns its tag names. */
function wellFormed(doc: string): string[] {
  const names: string[] = [];
  const stack: string[] = [];
  const tag = /<(\/?)([A-Za-z][\w:-]*)((?:\s+[\w:-]+="[^"<]*")*)\s*(\/?)>/y;
  const entity = /&(?:#\d+|#x[0-9a-fA-F]+|amp|lt|gt|quot|apos);/y;
  let i = 0;
  let roots = 0;
  while (i < doc.length) {
    const ch = doc[i];
    if (ch === "<") {
      tag.lastIndex = i;
      const m = tag.exec(doc);
      assert.ok(m, `a bad tag at ${i}: ${doc.slice(i, i + 60)}`);
      const [, close, name, attrs, selfClose] = m;
      const seen = new Set<string>();
      for (const a of attrs.matchAll(/([\w:-]+)="([^"]*)"/g)) {
        assert.ok(!seen.has(a[1]), `attribute ${a[1]} twice on <${name}>`);
        seen.add(a[1]);
        assert.ok(!/&(?!#\d+;|#x[0-9a-fA-F]+;|amp;|lt;|gt;|quot;|apos;)/.test(a[2]), `a bare & in ${a[1]}`);
      }
      if (close) {
        assert.equal(attrs + selfClose, "", "a closing tag has no attributes");
        assert.equal(stack.pop(), name, `</${name}> closes nothing`);
      } else {
        if (stack.length === 0) roots++;
        names.push(name);
        if (!selfClose) stack.push(name);
      }
      i = tag.lastIndex;
    } else if (ch === "&") {
      entity.lastIndex = i;
      assert.ok(entity.exec(doc), `a bare & at ${i}`);
      i = entity.lastIndex;
    } else {
      assert.ok(stack.length > 0, "text outside the root element");
      assert.notEqual(ch, ">", `a bare > at ${i}`);
      i++;
    }
  }
  assert.deepEqual(stack, [], "unclosed elements");
  assert.equal(roots, 1, "one root element");
  return names;
}

/** What a strict CSP (or the copy rules) wouldn't allow in an inlined drawing. */
function safe(doc: string): void {
  assert.ok(!/<style|style=|<script|<image|<foreignObject|<a[\s>]|\son\w+=|javascript:|data:|@import|url\(/i.test(doc), "only presentation attributes, nothing active");
  // The SVG namespace is the one URL a document names; nothing is fetched.
  assert.ok(!/https?:|\/\//i.test(doc.replaceAll('xmlns="http://www.w3.org/2000/svg"', "")), "no URLs");
  // Every reference is to an id the same document defines.
  const ids = new Set([...doc.matchAll(/\sid="([^"]+)"/g)].map((m) => m[1]));
  for (const m of doc.matchAll(/href="([^"]*)"/g)) assert.ok(m[1].startsWith("#") && ids.has(m[1].slice(1)), `href ${m[1]} is local`);
  assert.ok(!/[\u0000-\u0008\u000b\u000c\u000e-\u001f]/.test(doc), "no control characters");
  assert.ok(!/NaN|undefined|Infinity|\de[+-]?\d/.test(doc.replace(/#[0-9a-f]{6}/g, "")), "plain numbers only");
}

const BANNED = /contract|demo|example|mainnet|casino|jackpot|win money|cash|prize money/i;
const allowed = new Set(["svg", "defs", "g", "path", "rect", "circle", "text", "title", "use"]);

// ---- sizes ----
assert.deepEqual([TABLE_WIDTH, TABLE_HEIGHT, CARD_WIDTH, CARD_HEIGHT], [1200, 675, 250, 350]);
assert.ok(tableSvg(base).startsWith(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1200 675" width="1200" height="675"`));
assert.ok(posterSvg().startsWith(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1200 675" width="1200" height="675"`));
assert.ok(cardFaceSvg(0).startsWith(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 250 350" width="250" height="350"`));
assert.ok(cardBackSvg().startsWith(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 250 350" width="250" height="350"`));
assert.deepEqual(THEME, { paper: "#ece3cd", paperHi: "#f4eedf", paperLo: "#e1d6ba", ink: "#17120f", inkSoft: "#3b332c", blood: "#8d1418", bloodHi: "#a91c20" });
assert.deepEqual({ ...DEFAULT_FONTS }, { display: "UnifrakturMaguntia", serif: "EB Garamond", caps: "IM Fell English SC", mono: "Courier Prime", fell: "IM Fell English" });

// ---- every document: well-formed, safe, deterministic, small ----
const docs: string[] = [posterSvg(), cardBackSvg(), ...tables.map((t) => tableSvg(t))];
for (let c = 0; c < 52; c++) docs.push(cardFaceSvg(c));
for (const doc of docs) {
  for (const name of wellFormed(doc)) assert.ok(allowed.has(name), `<${name}> isn't an element the drawings use`);
  safe(doc);
  assert.ok(!BANNED.test(doc), "a word the copy rules ban");
  assert.ok(doc.length < 40_000, `a drawing of ${doc.length} bytes`);
}
assert.equal(tableSvg(base), tableSvg({ ...base, playerCards: [...base.playerCards] }));
assert.equal(posterSvg(), posterSvg());
assert.equal(cardBackSvg(), cardBackSvg());
for (let c = 0; c < 52; c++) assert.equal(cardFaceFragment(c), cardFaceFragment(c));
// A fragment is the document without its wrapper, and carries no ids (so many fit one page).
assert.ok(cardFaceSvg(17).includes(cardFaceFragment(17)));
assert.ok(cardBackSvg().includes(cardBackFragment()));
assert.ok(!/\sid=|href=/.test(cardFaceFragment(17) + cardBackFragment() + batFragment()));

// ---- cards ----
assert.equal(new Set(Array.from({ length: 52 }, (_, c) => cardFaceFragment(c))).size, 52, "52 different faces");
for (let c = 0; c < 52; c++) {
  const doc = cardFaceSvg(c);
  assert.ok(doc.includes(`<title>${cardLabel(c)}</title>`), "a face is titled with its label");
  const red = Math.floor(c / 13) === 1 || Math.floor(c / 13) === 2;
  assert.ok(doc.includes(`fill="${red ? THEME.blood : THEME.ink}"`));
  // The rank is in both corners: the same text twice, once in a half turn.
  const rank = ["A", "2", "3", "4", "5", "6", "7", "8", "9", "10", "J", "Q", "K"][c % 13];
  assert.ok(doc.split(`>${rank}</text>`).length - 1 >= 2, `${cardLabel(c)} shows its rank in two corners`);
  assert.ok(doc.includes(`<g transform="rotate(180 125 175)">`));
  // A number card has as many pips as it counts, beside the two in its corners.
  if (c % 13 >= 1 && c % 13 <= 9) assert.equal(doc.split("<path").length - 1, (c % 13) + 1 + 2, `${cardLabel(c)}'s pips`);
}
for (const [c, word] of [[10, "Jack"], [11, "Queen"], [12, "King"]] as const) assert.ok(cardFaceFragment(c).includes(`>${word}</text>`));
assert.throws(() => cardFaceFragment(52));
assert.throws(() => cardFaceFragment(-1));
assert.throws(() => cardFaceFragment(1.5));
// The back says nothing about any card, and is the bat: ink, blood, and paper for its eyes.
const back = cardBackFragment();
assert.ok(!back.includes("<text") && !back.includes("<title"), "no label on the back");
assert.ok(back.includes(batFragment()));
for (const color of [THEME.ink, THEME.blood, THEME.bloodHi, THEME.paperHi]) assert.ok(batFragment().includes(color));
// Fonts can be swapped, and a family name is escaped like any text.
assert.ok(cardFaceFragment(11, { fonts: { display: "Some Blackletter" } }).includes(`font-family="Some Blackletter"`));
assert.ok(!cardFaceFragment(11, { fonts: { display: "Some Blackletter" } }).includes(DEFAULT_FONTS.display));
assert.ok(tableSvg(base, { fonts: { mono: `x" onload="y` } }).includes(`font-family="x&#34; onload=&#34;y"`));
wellFormed(tableSvg(base, { fonts: { mono: `x" onload="y`, serif: "<b>" } }));

// ---- the table ----
for (const t of tables) {
  const doc = tableSvg(t);
  assert.ok(doc.includes(`>${NOTE.replace("'", "&#39;")}</text>`), "the note");
  assert.ok(doc.includes(">Blockula</text>") && doc.includes(">Blackjack</text>"), "the masthead");
  assert.ok(doc.includes(">Dealer</text>") && doc.includes(">Player</text>"), "the rows' labels");
  // The hole card is a back exactly when it's hidden.
  assert.equal(doc.split(`<use href="#bj-back"`).length - 1, t.holeHidden ? 1 : 0);
  assert.equal(doc.includes(`id="bj-back"`), t.holeHidden);
  assert.equal(doc.includes(batFragment()), t.holeHidden || [...t.playerCards, ...t.dealerCards].some((c) => c % 13 >= 10));
  // Every card dealt is drawn, in order.
  const titles = [...doc.matchAll(/<title>([^<]*)<\/title>/g)].map((m) => m[1]);
  assert.deepEqual(titles, [...t.dealerCards, ...t.playerCards].map(cardLabel));
  // The status line and the footer.
  const [l1, l2] = footerLines(t);
  const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
  for (const line of [statusLine(t), l1, l2]) assert.ok(doc.includes(`>${esc(line)}</text>`), line);
  assert.ok(statusLine(t).startsWith(phaseWords(t)));
  assert.ok(l1.includes("Bet 5 FANG"));
  assert.ok(l2.startsWith("Hand 42"));
  // A settled hand is stamped with its outcome.
  if (t.outcome !== null) assert.ok(doc.includes(`>${stampWords(t.outcome)}</text>`));
  // Every card lies on the sheet.
  const w = CARD_WIDTH * TABLE_CARD_SCALE;
  const h = CARD_HEIGHT * TABLE_CARD_SCALE;
  const placed = [...doc.matchAll(/transform="translate\(([\d.]+) ([\d.]+)\) scale\(0\.52\)"/g)].map((m) => [Number(m[1]), Number(m[2])]);
  assert.equal(placed.length, t.playerCards.length + t.dealerCards.length + (t.holeHidden ? 1 : 0));
  for (const [x, y] of placed) {
    assert.ok(x >= ROW_LEFT && x + w <= ROW_RIGHT, `a card at x ${x}`);
    assert.ok(y === DEALER_ROW_Y || y === PLAYER_ROW_Y);
    assert.ok(y + h < 512, "above the footer");
  }
}
assert.equal(footerLines(base)[0], "Dealer 10 · Player 16 · Bet 5 FANG");
assert.equal(footerLines(base)[1], "Hand 42 · tx 9f2c41d07a… · mina.boo");
assert.equal(footerLines(empty)[0], "Dealer – · Player – · Bet 5 FANG");
assert.equal(footerLines(empty)[1], "Hand 42");
assert.equal(footerLines({ ...base, stake: 2_500_000_000n })[0], "Dealer 10 · Player 16 · Bet 2.5 FANG");
assert.equal(shortTx(TX), "9f2c41d07a…");
assert.equal(shortTx("abc"), "abc");
assert.ok(!tableSvg(base).includes(TX), "the tx id is shortened");
assert.equal(statusLine(base), "Your move: hit or stand.");
assert.equal(statusLine(settled(Outcome.Blackjack)), "Blackjack. 12.5 FANG came back.");
assert.equal(statusLine(settled(Outcome.Bust)), "Bust. Nothing came back.");
assert.equal(statusLine(settled(Outcome.Push)), "Push. 5 FANG came back.");
assert.deepEqual(outcomes.map(stampWords), ["Blackjack", "You win", "Push", "Dealer wins", "Bust", "You win", "You win"]);
// Nothing is stamped on a hand still being played; an empty table shows empty places.
assert.ok(!tableSvg(base).includes("rotate(-5"));
assert.equal(tableSvg(empty).split("stroke-dasharray").length - 1, 4);
assert.ok(!tableSvg(base).includes("stroke-dasharray"));
assert.ok(tableSvg(base).includes(">and one face down</text>"));
assert.ok(!tableSvg(settled(Outcome.Win)).includes(">and one face down</text>"));

// ---- long hands ----
for (let count = 0; count <= MAX_PLAYER_CARDS + 1; count++) {
  const lay = rowLayout(count);
  assert.equal(lay.xs.length, count);
  assert.equal(lay.width, 130);
  assert.equal(lay.height, 182);
  if (count === 0) continue;
  assert.equal(lay.xs[0], ROW_LEFT);
  assert.ok(lay.xs[count - 1] + lay.width <= ROW_RIGHT + 1e-9, `${count} cards end at ${lay.xs[count - 1] + lay.width}`);
  for (let i = 1; i < count; i++) assert.ok(lay.xs[i] > lay.xs[i - 1]);
  // Overlapped cards still show their corner (the rank and suit: the left 50 of 250).
  assert.ok(lay.step >= 50 * TABLE_CARD_SCALE + 8, `${count} cards step ${lay.step}`);
}
assert.ok(rowLayout(5).step > rowLayout(5).width, "five cards lie side by side");
assert.ok(rowLayout(MAX_PLAYER_CARDS).step < rowLayout(MAX_PLAYER_CARDS).width, "twenty overlap");
assert.ok(PLAYER_ROW_Y >= DEALER_ROW_Y + rowLayout(1).height, "the rows don't overlap");

// ---- untrusted text ----
const hostile = tableSvg({ ...base, player: `<script>"&'`, txId: `"><svg onload=1>` });
assert.ok(hostile.includes("&#60;script&#62;&#34;&#38;&#39;"), "the player's label is escaped");
assert.ok(!hostile.includes("<script") && !hostile.includes("<svg onload") && !hostile.includes(`"&'`));
wellFormed(hostile);
safe(hostile.replace("onload=1", ""));
const long = "a".repeat(200) + ".boo";
assert.equal(truncate(long, MAX_PLAYER_LABEL), "a".repeat(MAX_PLAYER_LABEL - 1) + "…");
assert.equal(truncate("mina.boo", MAX_PLAYER_LABEL), "mina.boo");
assert.equal(Array.from(truncate("🦇".repeat(40), MAX_PLAYER_LABEL)).length, MAX_PLAYER_LABEL, "cut by character, not by code unit");
assert.ok(!tableSvg({ ...base, player: long }).includes("a".repeat(MAX_PLAYER_LABEL)));
// Control characters, lone surrogates and bidi controls are dropped.
const odd = tableSvg({ ...base, player: "a\u0000b\u001fc\ud800d‮e\nf" });
assert.ok(odd.includes("· abcdef</text>"));
wellFormed(odd);
safe(odd);
assert.equal(footerLines({ ...base, player: " \u0000 ", txId: "" })[1], "Hand 42");

// ---- the poster ----
const poster = posterSvg();
assert.ok(poster.includes(`>${TAGLINE}</text>`) && TAGLINE === "A game played with FANG");
assert.ok(poster.includes(`>${NOTE.replace("'", "&#39;")}</text>`) && NOTE === "FANG has no value and can't be bought; no prizes.");
assert.ok(poster.includes(">Blockula</text>") && poster.includes(">Blackjack</text>"));
assert.ok(poster.includes(cardBackFragment()) && poster.includes(cardFaceFragment(0)), "a face and a back");
assert.ok(posterSvg({ fonts: { fell: "Some Italic" } }).includes(`font-family="Some Italic"`));

// ---- the copy rules, over every string the module can emit ----
for (const s of [POSTER_ALT, NOTE, TAGLINE, ...outcomes.map(stampWords), ...tables.flatMap((t) => [statusLine(t), ...footerLines(t)])]) assert.ok(!BANNED.test(s), s);
assert.ok(POSTER_ALT.includes("bat") && POSTER_ALT.includes(NOTE));

console.log(`✓ svg: ${docs.length} drawings well-formed, safe to inline and deterministic; 52 faces; long hands fit`);
