/**
 * A hand drawn as SVG: browser-safe strings, no dependencies, the same bytes for the same hand.
 * The table page shows them inline; `@blockula/blackjack-render` turns them into PNG files.
 *
 * The drawing uses presentation attributes only (no style elements or attributes, no scripts,
 * nothing fetched), so a page with a strict Content-Security-Policy can inline it. Every piece
 * of text that comes from outside (the player's label, the tx id) is cleaned and escaped.
 */
import { fang } from "./codec.js";
import { Outcome, cardLabel, rank, suit } from "./rules.js";
import { phaseWords, totalWords, type TableSnapshot } from "./snapshot.js";

export const TABLE_WIDTH = 1200;
export const TABLE_HEIGHT = 675;
export const CARD_WIDTH = 250;
export const CARD_HEIGHT = 350;

/** The site's colors (web/app/globals.css). */
export const THEME = Object.freeze({ paper: "#ece3cd", paperHi: "#f4eedf", paperLo: "#e1d6ba", ink: "#17120f", inkSoft: "#3b332c", blood: "#8d1418", bloodHi: "#a91c20" });

// Shades for the bat's facets: flat fills, no gradients.
const INK_MID = "#272019";
const BLOOD_LO = "#6f0f13";
const BLOOD_DEEP = "#580b0e";

/** Font families the drawing names. The table page passes the families its own stylesheet loaded. */
export interface SvgFonts {
  /** Blackletter, for the masthead. */
  display: string;
  serif: string;
  caps: string;
  mono: string;
  /** IM Fell English, for the lines set apart. */
  fell?: string;
}

export const DEFAULT_FONTS: Required<SvgFonts> = Object.freeze({ display: "UnifrakturMaguntia", serif: "EB Garamond", caps: "IM Fell English SC", mono: "Courier Prime", fell: "IM Fell English" });

export interface SvgOptions {
  fonts?: Partial<SvgFonts>;
}

/** The note every table and poster carries. */
export const NOTE = "FANG has no value and can't be bought; no prizes.";
/** The poster's line. */
export const TAGLINE = "A game played with FANG";

/** The most characters of a player's label the footer shows (the last one an ellipsis when it was cut). */
export const MAX_PLAYER_LABEL = 22;
/** How many characters of a tx id the footer shows before the ellipsis. */
export const TX_ID_CHARS = 10;

type Fonts = Required<SvgFonts>;
const fontsOf = (opts: SvgOptions): Fonts => {
  const f = { ...DEFAULT_FONTS };
  for (const [k, v] of Object.entries(opts.fonts ?? {})) if (typeof v === "string" && v) f[k as keyof Fonts] = v;
  return f;
};

/** A number as SVG text: at most two decimals, never an exponent. */
const n = (x: number): string => {
  const r = Math.round(x * 100) / 100;
  return r === 0 ? "0" : String(r);
};

/** Drops what XML can't hold (control characters, lone surrogates) and what could reorder text (bidi controls). */
function clean(s: string): string {
  let out = "";
  for (const ch of s) {
    const c = ch.codePointAt(0)!;
    if (c < 0x20 || (c >= 0x7f && c <= 0x9f) || (c >= 0xd800 && c <= 0xdfff) || c === 0xfffe || c === 0xffff) continue;
    if (c === 0x200e || c === 0x200f || (c >= 0x202a && c <= 0x202e) || (c >= 0x2066 && c <= 0x2069)) continue;
    out += ch;
  }
  return out;
}

const esc = (s: string): string => clean(s).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

/** `s` cut to `max` characters, the last an ellipsis when it was longer. */
export function truncate(s: string, max: number): string {
  const chars = Array.from(clean(s).trim());
  return chars.length <= max ? chars.join("") : `${chars.slice(0, max - 1).join("")}…`;
}

/** A tx id as the footer shows it: its first characters and an ellipsis. */
export function shortTx(txId: string): string {
  const chars = Array.from(clean(txId).trim());
  return chars.length <= TX_ID_CHARS ? chars.join("") : `${chars.slice(0, TX_ID_CHARS).join("")}…`;
}

/** The headline stamped on a settled hand. */
export function stampWords(o: Outcome): string {
  switch (o) {
    case Outcome.Blackjack:
      return "Blackjack";
    case Outcome.Win:
    case Outcome.Forfeit:
    case Outcome.ForfeitDeal:
      return "You win";
    case Outcome.Push:
      return "Push";
    case Outcome.Lose:
      return "Dealer wins";
    case Outcome.Bust:
      return "Bust";
    case Outcome.NoSeed:
      return "No seed";
  }
}

interface TextAttrs {
  size: number;
  fill: string;
  anchor?: "middle" | "end";
  spacing?: number;
}

function text(x: number, y: number, family: string, s: string, a: TextAttrs): string {
  const anchor = a.anchor ? ` text-anchor="${a.anchor}"` : "";
  const spacing = a.spacing ? ` letter-spacing="${n(a.spacing)}"` : "";
  return `<text x="${n(x)}" y="${n(y)}"${anchor} font-family="${esc(family)}" font-size="${n(a.size)}"${spacing} fill="${a.fill}">${esc(s)}</text>`;
}

const poly = (pts: readonly number[], fill: string): string => `<path d="M${pts.map(n).join(" ")}Z" fill="${fill}"/>`;

// ---- the bat ----

/** An isometric cube: its top corner at (x, y), half as wide as `w`, `d` deep. */
function cube(x: number, y: number, w: number, d: number, fills: readonly [string, string, string]): string {
  const h = w * 0.56;
  return (
    poly([x, y, x + w, y + h, x, y + 2 * h, x - w, y + h], fills[0]) +
    poly([x - w, y + h, x, y + 2 * h, x, y + 2 * h + d, x - w, y + h + d], fills[1]) +
    poly([x, y + 2 * h, x + w, y + h, x + w, y + h + d, x, y + 2 * h + d], fills[2])
  );
}

const BAT_WIDTH = 890;
const BAT_HEIGHT = 710;
const INKS = [THEME.inkSoft, THEME.ink, INK_MID] as const;
const BLOODS = [THEME.bloodHi, THEME.blood, BLOOD_LO] as const;

/** Draws `half` (the bat's left side) and its mirror image. */
const both = (half: string): string => `${half}<g transform="translate(${BAT_WIDTH} 0) scale(-1 1)">${half}</g>`;

let batMemo: string | undefined;

/**
 * The Blockula bat, built of blocks: a black bat with red wings, black ears and red earrings.
 * A fragment drawn in an 890 × 710 box at the origin.
 */
export function batFragment(): string {
  if (batMemo !== undefined) return batMemo;
  const ear = poly([262, 10, 238, 152, 292, 226, 368, 186, 358, 112], THEME.ink) + poly([270, 52, 276, 150, 340, 168, 338, 118], INK_MID);
  const earring = cube(270, 136, 33, 38, BLOODS);
  // The wing: a peak, two points along the bottom edge, and the facets between them.
  const [px, py] = [185, 278];
  const wing =
    poly([px, py, 48, 385, 10, 565], THEME.blood) +
    poly([px, py, 10, 565, 110, 470], BLOOD_LO) +
    poly([px, py, 110, 470, 190, 558], THEME.blood) +
    poly([px, py, 190, 558, 262, 492], BLOOD_LO) +
    poly([px, py, 262, 492, 300, 492, 335, 380], BLOOD_DEEP) +
    poly([px, py, 48, 385, 62, 396, px, 304], THEME.bloodHi) +
    poly([px, py, 335, 380, 316, 390, px, 304], THEME.bloodHi);
  const shoulder = cube(355, 388, 58, 72, INKS);
  const foot = cube(318, 515, 64, 75, INKS);
  const eye = poly([322, 280, 398, 312, 390, 330, 345, 328], THEME.paperHi);
  const tooth = poly([400, 384, 426, 390, 414, 428], THEME.paperHi);
  const head = cube(445, 142, 161, 150, INKS) + cube(445, 82, 83, 87, INKS);
  const middle = cube(445, 545, 64, 75, INKS) + cube(445, 440, 62, 70, INKS);
  const nose = cube(445, 318, 44, 28, [THEME.inkSoft, INK_MID, THEME.ink]);
  batMemo = both(ear + earring) + head + both(wing) + both(shoulder + foot) + middle + both(eye + tooth) + nose;
  return batMemo;
}

/** The bat `width` wide, its box centered on (cx, cy). */
function batAt(cx: number, cy: number, width: number): string {
  const k = width / BAT_WIDTH;
  return `<g transform="translate(${n(cx - width / 2)} ${n(cy - (BAT_HEIGHT * k) / 2)}) scale(${n4(k)})">${batFragment()}</g>`;
}

/** A scale factor: four decimals. */
const n4 = (x: number): string => String(Math.round(x * 10000) / 10000);

// ---- suits ----

// Each suit is one path in a box about 100 across, centered on the origin.
const SUIT_PATHS = [
  "M0 -50C-18 -30 -48 -10 -48 14C-48 40 -14 44 -4 22C-5 34 -10 43 -18 50L18 50C10 43 5 34 4 22C14 44 48 40 48 14C48 -10 18 -30 0 -50Z",
  "M0 -20C-10 -46 -48 -42 -48 -10C-48 18 -18 32 0 50C18 32 48 18 48 -10C48 -42 10 -46 0 -20Z",
  "M0 -52C12 -30 24 -14 40 0C24 14 12 30 0 52C-12 30 -24 14 -40 0C-24 -14 -12 -30 0 -52Z",
  "M-21 -27a21 21 0 1 0 42 0a21 21 0 1 0 -42 0ZM-48 8a21 21 0 1 0 42 0a21 21 0 1 0 -42 0ZM6 8a21 21 0 1 0 42 0a21 21 0 1 0 -42 0ZM-15 -3a15 15 0 1 0 30 0a15 15 0 1 0 -30 0ZM-4 10C-5 32 -10 43 -18 50L18 50C10 43 5 32 4 14Z",
] as const;

const suitColor = (s: number): string => (s === 1 || s === 2 ? THEME.blood : THEME.ink);
const suitId = (s: number): string => `bj-suit-${s}`;
const BACK_ID = "bj-back";

/** One suit mark centered on (x, y); `refs` draws it as a `<use>` of the table's `<defs>`. */
function suitMark(s: number, x: number, y: number, scale: number, flip: boolean, refs: boolean): string {
  const t = `translate(${n(x)} ${n(y)})${flip ? " rotate(180)" : ""} scale(${n4(scale)})`;
  return refs ? `<use href="#${suitId(s)}" transform="${t}"/>` : `<path d="${SUIT_PATHS[s]}" transform="${t}" fill="${suitColor(s)}"/>`;
}

// ---- cards ----

const STOCK = `<rect x="1.5" y="1.5" width="247" height="347" rx="14" fill="${THEME.paperHi}" stroke="${THEME.ink}" stroke-width="3"/>`;

/** Where the pips of the cards 2 to 10 sit: [x, y] pairs. Those below the middle are drawn upside down. */
function pips(r: number): [number, number][] {
  const [L, C, R] = [82, 125, 168];
  const [T, M, B] = [80, 175, 270];
  const corners: [number, number][] = [[L, T], [R, T], [L, B], [R, B]];
  const third = (B - T) / 3;
  const inner: [number, number][] = [[L, T + third], [R, T + third], [L, B - third], [R, B - third]];
  switch (r) {
    case 1:
      return [[C, T], [C, B]];
    case 2:
      return [[C, T], [C, M], [C, B]];
    case 3:
      return corners;
    case 4:
      return [...corners, [C, M]];
    case 5:
      return [...corners, [L, M], [R, M]];
    case 6:
      return [...corners, [L, M], [R, M], [C, (T + M) / 2]];
    case 7:
      return [...corners, [L, M], [R, M], [C, (T + M) / 2], [C, (M + B) / 2]];
    case 8:
      return [...corners, ...inner, [C, M]];
    case 9:
      return [...corners, ...inner, [C, T + third / 2], [C, B - third / 2]];
    default:
      return [];
  }
}

const FACE_WORDS = ["Jack", "Queen", "King"] as const;
const INDEX = ["A", "2", "3", "4", "5", "6", "7", "8", "9", "10", "J", "Q", "K"] as const;

function face(card: number, f: Fonts, refs: boolean): string {
  const label = cardLabel(card); // throws for anything that isn't a card
  const [r, s] = [rank(card), suit(card)];
  const color = suitColor(s);
  const index = INDEX[r];
  const corner = text(31, 52, f.serif, index, { size: index.length > 1 ? 40 : 46, fill: color, anchor: "middle", spacing: index.length > 1 ? -4 : 0 }) + suitMark(s, 31, 80, 0.26, false, refs);
  let middle: string;
  if (r === 0) {
    // The ace: one large mark in a double ring.
    middle = `<circle cx="125" cy="175" r="84" fill="none" stroke="${color}" stroke-width="3"/><circle cx="125" cy="175" r="76" fill="none" stroke="${color}" stroke-width="1"/>` + suitMark(s, 125, 175, 1.05, false, refs);
  } else if (r >= 10) {
    // A face card: a framed blackletter initial under the bat.
    middle =
      `<rect x="60" y="42" width="130" height="266" fill="${THEME.paper}" stroke="${color}" stroke-width="3"/><rect x="66" y="48" width="118" height="254" fill="none" stroke="${color}" stroke-width="1"/>` +
      batAt(125, 102, 112) +
      // The blackletter J hangs below the line: it sits higher and smaller.
      text(125, r === 10 ? 234 : 250, f.display, index, { size: r === 10 ? 98 : 112, fill: color, anchor: "middle" }) +
      suitMark(s, 78, 281, 0.13, false, refs) +
      suitMark(s, 172, 281, 0.13, false, refs) +
      `<path d="M72 264H178" stroke="${color}" stroke-width="1.5"/>` +
      text(125, 288, f.caps, FACE_WORDS[r - 10], { size: 20, fill: THEME.ink, anchor: "middle", spacing: 2 });
  } else {
    middle = pips(r)
      .map(([x, y]) => suitMark(s, x, y, 0.4, y > 175, refs))
      .join("");
  }
  return `${STOCK}<title>${esc(label)}</title>${corner}<g transform="rotate(180 125 175)">${corner}</g>${middle}`;
}

/** A card's face, as a fragment drawn in a 250 × 350 box at the origin. */
export function cardFaceFragment(card: number, opts: SvgOptions = {}): string {
  return face(card, fontsOf(opts), false);
}

let backMemo: string | undefined;

/** A card's back (the Blockula bat), as a fragment drawn in a 250 × 350 box at the origin. */
export function cardBackFragment(): string {
  if (backMemo !== undefined) return backMemo;
  // A lattice of diagonals over the blood field, cut to the field by arithmetic (no clip path).
  const [x0, y0, x1, y1] = [16, 16, 234, 334];
  let lattice = "";
  for (let c = x0 + y0 + 22; c < x1 + y1; c += 22) {
    // x + y = c
    const ax = Math.min(x1, c - y0);
    const bx = Math.max(x0, c - y1);
    lattice += `M${n(ax)} ${n(c - ax)}L${n(bx)} ${n(c - bx)}`;
  }
  for (let c = x0 - y1 + 20; c < x1 - y0; c += 22) {
    // x - y = c
    const ax = Math.max(x0, c + y0);
    const bx = Math.min(x1, c + y1);
    lattice += `M${n(ax)} ${n(ax - c)}L${n(bx)} ${n(bx - c)}`;
  }
  const lozenge = (x: number, y: number): string => `M${x} ${y - 9}L${x + 7} ${y}L${x} ${y + 9}L${x - 7} ${y}Z`;
  backMemo =
    STOCK +
    `<rect x="${x0}" y="${y0}" width="${x1 - x0}" height="${y1 - y0}" rx="5" fill="${THEME.blood}"/>` +
    `<path d="${lattice}" fill="none" stroke="${BLOOD_DEEP}" stroke-width="2"/>` +
    `<rect x="${x0}" y="${y0}" width="${x1 - x0}" height="${y1 - y0}" rx="5" fill="none" stroke="${THEME.ink}" stroke-width="3"/>` +
    `<rect x="24" y="24" width="202" height="302" fill="none" stroke="${THEME.paperHi}" stroke-width="1.5"/>` +
    `<path d="${lozenge(125, 50)}${lozenge(125, 300)}" fill="${THEME.paperHi}"/>` +
    `<circle cx="125" cy="175" r="92" fill="${THEME.paperHi}" stroke="${THEME.ink}" stroke-width="4"/><circle cx="125" cy="175" r="84" fill="none" stroke="${THEME.ink}" stroke-width="1"/>` +
    batAt(125, 177, 150);
  return backMemo;
}

const svgOpen = (w: number, h: number, label: string): string => `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${w} ${h}" width="${w}" height="${h}" role="img" aria-label="${esc(label)}">`;

/** One card's face as its own SVG document. */
export function cardFaceSvg(card: number, opts: SvgOptions = {}): string {
  return `${svgOpen(CARD_WIDTH, CARD_HEIGHT, cardLabel(card))}${cardFaceFragment(card, opts)}</svg>`;
}

/** A card's back as its own SVG document. */
export function cardBackSvg(): string {
  return `${svgOpen(CARD_WIDTH, CARD_HEIGHT, "A card face down")}${cardBackFragment()}</svg>`;
}

// ---- the table ----

/** How big the table draws a card (of 250 × 350). */
export const TABLE_CARD_SCALE = 0.52;
/** The strip the cards lie in, left to right. */
export const ROW_LEFT = 250;
export const ROW_RIGHT = 1152;
/** The top of the dealer's and the player's row. */
export const DEALER_ROW_Y = 120;
export const PLAYER_ROW_Y = 316;
const ROW_GAP = 14;

export interface RowLayout {
  /** The left edge of each card, in order. */
  xs: number[];
  /** A drawn card's width and height. */
  width: number;
  height: number;
  /** How far apart neighbors' left edges are: less than `width` when the cards overlap. */
  step: number;
}

/** Where `count` cards lie in a row: side by side while they fit, overlapped (each showing its corner) when they don't. */
export function rowLayout(count: number): RowLayout {
  const width = CARD_WIDTH * TABLE_CARD_SCALE;
  const height = CARD_HEIGHT * TABLE_CARD_SCALE;
  const room = ROW_RIGHT - ROW_LEFT - width;
  const step = count > 1 ? Math.min(width + ROW_GAP, room / (count - 1)) : width + ROW_GAP;
  const xs: number[] = [];
  for (let i = 0; i < count; i++) xs.push(Math.round((ROW_LEFT + i * step) * 100) / 100);
  return { xs, width, height, step };
}

const defs = (back: boolean): string =>
  `<defs>${SUIT_PATHS.map((d, s) => `<path id="${suitId(s)}" d="${d}" fill="${suitColor(s)}"/>`).join("")}${back ? `<g id="${BACK_ID}">${cardBackFragment()}</g>` : ""}</defs>`;

/** The sheet: paper, an ink frame, the masthead and the rules under it. */
function sheet(f: Fonts): string {
  const [w, h] = [TABLE_WIDTH, TABLE_HEIGHT];
  return (
    `<rect width="${w}" height="${h}" fill="${THEME.paper}"/>` +
    `<rect x="14" y="14" width="${w - 28}" height="${h - 28}" fill="none" stroke="${THEME.ink}" stroke-width="4"/><rect x="22" y="22" width="${w - 44}" height="${h - 44}" fill="none" stroke="${THEME.ink}" stroke-width="1"/>` +
    text(48, 84, f.display, "Blockula", { size: 64, fill: THEME.ink }) +
    text(300, 82, f.caps, "Blackjack", { size: 38, fill: THEME.blood, spacing: 7 }) +
    text(ROW_RIGHT, 80, f.caps, TAGLINE, { size: 21, fill: THEME.inkSoft, anchor: "end", spacing: 3 }) +
    `<path d="M48 101H${ROW_RIGHT}" stroke="${THEME.ink}" stroke-width="4"/><path d="M48 108H${ROW_RIGHT}" stroke="${THEME.ink}" stroke-width="1"/>`
  );
}

function row(label: string, cards: readonly number[], hidden: boolean, y: number, f: Fonts): string {
  const total = cards.length ? totalWords(cards) : "–";
  let out = text(48, y + 34, f.caps, label, { size: 25, fill: THEME.blood, spacing: 4 });
  out += text(48, y + (total.length <= 2 ? 104 : 88), f.serif, total, { size: total.length <= 2 ? 76 : 42, fill: THEME.ink });
  if (hidden) out += text(48, y + 140, f.fell, "and one face down", { size: 21, fill: THEME.inkSoft });
  const count = cards.length + (hidden ? 1 : 0);
  const lay = rowLayout(count);
  if (count === 0) {
    // Nothing dealt yet: two empty places.
    for (const x of rowLayout(2).xs) out += `<rect x="${n(x + 1)}" y="${n(y + 1)}" width="${n(lay.width - 2)}" height="${n(lay.height - 2)}" rx="7" fill="none" stroke="${THEME.inkSoft}" stroke-width="1.5" stroke-dasharray="7 6"/>`;
    return out;
  }
  const k = n4(TABLE_CARD_SCALE);
  cards.forEach((c, i) => (out += `<g transform="translate(${n(lay.xs[i])} ${y}) scale(${k})">${face(c, f, true)}</g>`));
  if (hidden) out += `<use href="#${BACK_ID}" transform="translate(${n(lay.xs[cards.length])} ${y}) scale(${k})"/>`;
  return out;
}

/** A settled hand's headline, stamped at the foot of the sheet. */
function stamp(words: string, f: Fonts): string {
  const [cx, cy, w, h] = [985, 578, 310, 84];
  const size = words.length > 9 ? 40 : 50;
  return (
    `<g transform="rotate(-5 ${cx} ${cy})"><rect x="${cx - w / 2}" y="${cy - h / 2}" width="${w}" height="${h}" fill="${THEME.paper}" stroke="${THEME.blood}" stroke-width="5"/><rect x="${cx - w / 2 + 8}" y="${cy - h / 2 + 8}" width="${w - 16}" height="${h - 16}" fill="none" stroke="${THEME.blood}" stroke-width="1.5"/>` +
    text(cx, cy + size * 0.32, f.caps, words, { size, fill: THEME.blood, anchor: "middle", spacing: 3 }) +
    `</g>`
  );
}

/** The footer's two lines: totals and bet, then the hand id, the tx id (shortened) and the player's label. */
export function footerLines(s: TableSnapshot): [string, string] {
  const totals = `Dealer ${s.dealerCards.length ? totalWords(s.dealerCards) : "–"} · Player ${s.playerCards.length ? totalWords(s.playerCards) : "–"} · Bet ${fang(s.stake)} FANG`;
  const ids = [`Hand ${s.gameId}`];
  if (s.txId && clean(s.txId).trim()) ids.push(`tx ${shortTx(s.txId)}`);
  if (s.player && clean(s.player).trim()) ids.push(truncate(s.player, MAX_PLAYER_LABEL));
  return [totals, ids.join(" · ")];
}

/** The line under the cards: where the hand stands and, once settled, what came back. */
export function statusLine(s: TableSnapshot): string {
  const words = phaseWords(s);
  if (s.outcome === null || s.payout === null) return words;
  return `${words} ${s.payout > 0n ? `${fang(s.payout)} FANG came back.` : "Nothing came back."}`;
}

/** The whole table for one hand: 1200 × 675, the dealer's cards above the player's, the footer with totals, bet, hand id and tx id. */
export function tableSvg(s: TableSnapshot, opts: SvgOptions = {}): string {
  const f = fontsOf(opts);
  const [line1, line2] = footerLines(s);
  const status = statusLine(s);
  return (
    svgOpen(TABLE_WIDTH, TABLE_HEIGHT, `Blockula Blackjack, hand ${s.gameId}`) +
    defs(s.holeHidden) +
    sheet(f) +
    row("Dealer", s.dealerCards, s.holeHidden, DEALER_ROW_Y, f) +
    row("Player", s.playerCards, false, PLAYER_ROW_Y, f) +
    `<path d="M48 512H${ROW_RIGHT}" stroke="${THEME.ink}" stroke-width="1.5"/>` +
    text(48, 552, f.serif, status, { size: status.length > 44 ? 28 : 34, fill: THEME.blood }) +
    text(48, 586, f.mono, line1, { size: 19, fill: THEME.ink }) +
    text(48, 611, f.mono, line2, { size: 19, fill: THEME.ink }) +
    text(48, 642, f.serif, NOTE, { size: 19, fill: THEME.inkSoft }) +
    (s.outcome !== null ? stamp(stampWords(s.outcome), f) : "") +
    `</svg>`
  );
}

/** The table with no hand on it: the picture a link to the table shows. */
export function posterSvg(opts: SvgOptions = {}): string {
  const f = fontsOf(opts);
  const [w, h] = [TABLE_WIDTH, TABLE_HEIGHT];
  const cx = 400;
  return (
    svgOpen(w, h, "Blockula Blackjack") +
    `<rect width="${w}" height="${h}" fill="${THEME.paper}"/>` +
    `<rect x="14" y="14" width="${w - 28}" height="${h - 28}" fill="none" stroke="${THEME.ink}" stroke-width="4"/><rect x="22" y="22" width="${w - 44}" height="${h - 44}" fill="none" stroke="${THEME.ink}" stroke-width="1"/>` +
    text(cx - 10, 258, f.display, "Blockula", { size: 172, fill: THEME.ink, anchor: "middle" }) +
    `<path d="M80 306H720" stroke="${THEME.ink}" stroke-width="5"/><path d="M80 315H720" stroke="${THEME.ink}" stroke-width="1"/>` +
    text(cx + 8, 392, f.caps, "Blackjack", { size: 70, fill: THEME.blood, anchor: "middle", spacing: 16 }) +
    `<path d="M80 424H720" stroke="${THEME.ink}" stroke-width="1"/><path d="M80 433H720" stroke="${THEME.ink}" stroke-width="5"/>` +
    text(cx, 500, f.fell, TAGLINE, { size: 50, fill: THEME.ink, anchor: "middle" }) +
    text(cx, 612, f.serif, NOTE, { size: 25, fill: THEME.inkSoft, anchor: "middle" }) +
    `<g transform="translate(800 165) rotate(-9 112 400) scale(0.9)">${cardFaceFragment(0, opts)}</g>` +
    `<g transform="translate(892 175) rotate(8 112 400) scale(0.9)">${cardBackFragment()}</g>` +
    `</svg>`
  );
}

/** Alt text for the poster. */
export const POSTER_ALT = `Blockula Blackjack: a game played with FANG. Two playing cards are fanned out: the ace of spades, and a card face down, its back a black bat with red wings. ${NOTE}`;
