// The PNG renderer, offline: the right size, the same bytes every time, the shipped fonts in
// use, alt text on every picture, and the poster in web/public up to date.
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { POSTER_ALT, altText, cardName, posterSvg, tableSvg } from "@blockula/blackjack";
import { GALLERY } from "../src/cli.js";
import { FONT_FILES, RENDER_FONTS, renderPoster, renderSvg, renderTable, type Picture } from "../src/index.js";

const same = (a: Uint8Array, b: Uint8Array): boolean => Buffer.from(a).equals(Buffer.from(b));

/** A PNG's width and height, from its IHDR (the first chunk). */
function size(png: Uint8Array): [number, number] {
  const b = Buffer.from(png);
  assert.equal(b.subarray(0, 8).toString("hex"), "89504e470d0a1a0a", "the PNG signature");
  assert.equal(b.toString("latin1", 12, 16), "IHDR");
  return [b.readUInt32BE(16), b.readUInt32BE(20)];
}

// ---- every picture: 1200 × 675, deterministic, with alt text ----
const pictures: [string, Picture][] = [["poster", renderPoster()], ...Object.entries(GALLERY).map(([name, s]): [string, Picture] => [name, renderTable(s)])];
for (const [name, p] of pictures) {
  assert.deepEqual(size(p.png), [1200, 675], name);
  assert.ok(p.alt.trim().length > 20, `${name} has alt text`);
  assert.ok(same(renderSvg(p.svg), p.png), `${name} renders to the same bytes twice`);
}
assert.equal(new Set(pictures.map(([, p]) => Buffer.from(p.png).toString("base64"))).size, pictures.length, "different hands, different pictures");
for (const [name, s] of Object.entries(GALLERY)) {
  const again = renderTable(structuredClone(s));
  const first = pictures.find(([n]) => n === name)![1];
  assert.equal(again.svg, first.svg);
  assert.ok(same(again.png, first.png));
  assert.equal(first.svg, tableSvg(s, { fonts: RENDER_FONTS }));
  assert.equal(first.alt, altText(s));
  // The alt text names every card on the table.
  for (const c of [...s.playerCards, ...s.dealerCards]) assert.ok(first.alt.includes(cardName(c)), `${name}: ${cardName(c)}`);
  assert.ok(first.alt.includes("FANG"));
}
const poster = renderPoster();
assert.equal(poster.svg, posterSvg({ fonts: RENDER_FONTS }));
assert.equal(poster.alt, POSTER_ALT);
// The gallery covers every stage the brief names.
for (const name of ["seed-wait", "no-seed", "waiting", "deal", "player-turn", "blackjack", "win", "push", "dealer-wins", "bust", "timeout-dealer", "timeout-deal", "long-hand"]) assert.ok(name in GALLERY, name);

// A bet waiting for its seed, and one whose seed never came: an empty table, said in words and stamped.
assert.ok(pictures.find(([n]) => n === "seed-wait")![1].alt.includes("Waiting for the player's seed."));
assert.ok(pictures.find(([n]) => n === "no-seed")![1].svg.includes("No seed"));
assert.ok(pictures.find(([n]) => n === "no-seed")![1].alt.includes("Nothing came back."));

// ---- fonts: each family comes from its shipped file, and from nowhere else ----
assert.equal(FONT_FILES.length, 5);
for (const f of FONT_FILES) assert.ok(existsSync(f), f);
const line = (family: string): string => `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1200 675" width="1200" height="675"><text x="40" y="300" font-family="${family}" font-size="120">Blockula 21</text></svg>`;
const blank = renderSvg(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1200 675" width="1200" height="675"></svg>`);
const byFamily = new Map<string, Uint8Array>();
const fileOf: Record<string, string> = { display: "UnifrakturMaguntia.ttf", serif: "EBGaramond-Regular.ttf", caps: "IMFellEnglishSC.ttf", mono: "CourierPrime-Regular.ttf", fell: "IMFellEnglish-Regular.ttf" };
for (const [slot, family] of Object.entries(RENDER_FONTS)) {
  const png = renderSvg(line(family));
  assert.ok(!same(png, blank), `${family} draws text`);
  // Without its own file the family isn't found: the line comes out in another face (or not at all).
  const without = FONT_FILES.filter((f) => !f.endsWith(fileOf[slot]));
  assert.equal(without.length, FONT_FILES.length - 1, slot);
  assert.ok(!same(renderSvg(line(family), without), png), `${family} is drawn from ${fileOf[slot]}`);
  byFamily.set(family, png);
}
assert.equal(new Set([...byFamily.values()].map((p) => Buffer.from(p).toString("base64"))).size, 5, "five different faces");
// No system fonts: with no files at all, nothing is drawn, even for a family every machine has.
assert.ok(same(renderSvg(line("Helvetica, Arial, sans-serif"), []), blank), "system fonts aren't loaded");
// A family that isn't shipped falls back to the serif.
assert.ok(same(renderSvg(line("No Such Family")), byFamily.get(RENDER_FONTS.serif)!));

// ---- the poster in web/public is the one this code draws ----
const pub = fileURLToPath(new URL("../../../../web/public/blackjack/", import.meta.url));
// Byte for byte where the committed poster was rendered (resvg's builds for other platforms may
// round a pixel differently); elsewhere, that it's there and the right size.
const committed = readFileSync(`${pub}poster.png`);
if (process.platform === "darwin" && process.arch === "arm64") assert.ok(same(committed, poster.png), "web/public/blackjack/poster.png is stale: npm run poster -w @blockula/blackjack-render");
else assert.deepEqual([committed.readUInt32BE(16), committed.readUInt32BE(20)], [1200, 675], "web/public/blackjack/poster.png is 1200 × 675");
assert.equal(readFileSync(`${pub}poster.alt.txt`, "utf8"), `${POSTER_ALT}\n`);

console.log(`✓ render: ${pictures.length} pictures at 1200 × 675, deterministic, drawn with the 5 shipped fonts; the poster in web/public is current`);
