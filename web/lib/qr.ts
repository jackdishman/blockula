/**
 * A small, dependency-free QR Code encoder (ISO/IEC 18004), just enough for payment links:
 * byte mode, error correction level M, versions 1-40, automatic mask selection.
 *
 * Kept self-contained on purpose: a wallet shouldn't pull a third-party package into the page
 * that renders where people send money. Adapted from Project Nayuki's QR Code generator library
 * (notice below), and lib/qr.test.mjs checks the output against it.
 *
 * Copyright (c) Project Nayuki. (MIT License)
 * https://www.nayuki.io/page/qr-code-generator-library
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy of
 * this software and associated documentation files (the "Software"), to deal in
 * the Software without restriction, including without limitation the rights to
 * use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
 * the Software, and to permit persons to whom the Software is furnished to do so,
 * subject to the following conditions:
 * - The above copyright notice and this permission notice shall be included in
 *   all copies or substantial portions of the Software.
 * - The Software is provided "as is", without warranty of any kind, express or
 *   implied, including but not limited to the warranties of merchantability,
 *   fitness for a particular purpose and noninfringement. In no event shall the
 *   authors or copyright holders be liable for any claim, damages or other
 *   liability, whether in an action of contract, tort or otherwise, arising from,
 *   out of or in connection with the Software or the use or other dealings in the
 *   Software.
 */

/** Level M: error-correction codewords per block, indexed by version (index 0 unused). */
const ECC_PER_BLOCK = [
  -1, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26,
  26, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
];
/** Level M: number of error-correction blocks, indexed by version (index 0 unused). */
const NUM_BLOCKS = [
  -1, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16,
  17, 17, 18, 20, 21, 23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49,
];
/** Format-info bits for level M. */
const ECL_BITS = 0;

/** A square grid of modules; `true` is dark. Rows are `y`, columns are `x`. */
export type QrMatrix = boolean[][];

/** Encodes text (as UTF-8 bytes) into a QR matrix, without the quiet zone. */
export function encodeQr(text: string, opts: { mask?: number } = {}): QrMatrix {
  const data = new TextEncoder().encode(text);
  let version = 1;
  for (; ; version++) {
    if (version > 40) throw new RangeError("Too much data for a QR code");
    const bits = 4 + (version < 10 ? 8 : 16) + data.length * 8;
    if (bits <= numDataCodewords(version) * 8) break;
  }
  const codewords = addEccAndInterleave(makeDataCodewords(data, version), version);
  return new Grid(version).build(codewords, opts.mask);
}

/**
 * What QrCode draws for `text`: the SVG's size (with a `border`-module quiet zone) and path, or
 * `null` when the text is too long for any QR code (`encodeQr` throws a RangeError then), so the
 * page shows that instead of crashing (audit main-11-2).
 */
export function qrSvg(text: string, border = 4): { size: number; d: string } | null {
  try {
    const m = encodeQr(text);
    return { size: m.length + 2 * border, d: qrPath(m, border) };
  } catch (e) {
    if (e instanceof RangeError) return null;
    throw e;
  }
}

/**
 * SVG path data for the dark modules, one unit per module, offset by `border` units of quiet
 * zone. Horizontal runs are merged so dense codes stay a reasonable size in the DOM.
 */
export function qrPath(m: QrMatrix, border = 4): string {
  let d = "";
  for (let y = 0; y < m.length; y++) {
    for (let x = 0; x < m.length; ) {
      if (!m[y][x]) {
        x++;
        continue;
      }
      const start = x;
      while (x < m.length && m[y][x]) x++;
      d += `M${start + border} ${y + border}h${x - start}v1h${start - x}z`;
    }
  }
  return d;
}

function numRawDataModules(ver: number): number {
  let n = (16 * ver + 128) * ver + 64;
  if (ver >= 2) {
    const align = Math.floor(ver / 7) + 2;
    n -= (25 * align - 10) * align - 55;
    if (ver >= 7) n -= 36;
  }
  return n;
}

function numDataCodewords(ver: number): number {
  return Math.floor(numRawDataModules(ver) / 8) - ECC_PER_BLOCK[ver] * NUM_BLOCKS[ver];
}

function makeDataCodewords(data: Uint8Array, ver: number): number[] {
  const bits: number[] = [];
  const push = (val: number, len: number) => {
    for (let i = len - 1; i >= 0; i--) bits.push((val >>> i) & 1);
  };
  push(0b0100, 4); // byte mode
  push(data.length, ver < 10 ? 8 : 16);
  for (const b of data) push(b, 8);
  const capacity = numDataCodewords(ver) * 8;
  push(0, Math.min(4, capacity - bits.length)); // terminator
  push(0, (8 - (bits.length % 8)) % 8);
  for (let pad = 0xec; bits.length < capacity; pad ^= 0xec ^ 0x11) push(pad, 8);
  const out: number[] = [];
  for (let i = 0; i < bits.length; i += 8) out.push(bits.slice(i, i + 8).reduce((a, b) => (a << 1) | b, 0));
  return out;
}

function addEccAndInterleave(data: number[], ver: number): number[] {
  const numBlocks = NUM_BLOCKS[ver];
  const eccLen = ECC_PER_BLOCK[ver];
  const raw = Math.floor(numRawDataModules(ver) / 8);
  const numShort = numBlocks - (raw % numBlocks);
  const shortLen = Math.floor(raw / numBlocks);
  const divisor = rsDivisor(eccLen);
  const blocks: number[][] = [];
  for (let i = 0, k = 0; i < numBlocks; i++) {
    const dat = data.slice(k, k + shortLen - eccLen + (i < numShort ? 0 : 1));
    k += dat.length;
    const ecc = rsRemainder(dat, divisor);
    if (i < numShort) dat.push(0); // placeholder, skipped when interleaving
    blocks.push(dat.concat(ecc));
  }
  const out: number[] = [];
  for (let i = 0; i < blocks[0].length; i++) {
    blocks.forEach((b, j) => {
      if (i !== shortLen - eccLen || j >= numShort) out.push(b[i]);
    });
  }
  return out;
}

/** Multiply in GF(2^8) with the QR polynomial x^8 + x^4 + x^3 + x^2 + 1. */
function gfMul(x: number, y: number): number {
  let z = 0;
  for (let i = 7; i >= 0; i--) {
    z = (z << 1) ^ ((z >>> 7) * 0x11d);
    z ^= ((y >>> i) & 1) * x;
  }
  return z;
}

function rsDivisor(degree: number): number[] {
  const result = new Array<number>(degree).fill(0);
  result[degree - 1] = 1;
  let root = 1;
  for (let i = 0; i < degree; i++) {
    for (let j = 0; j < degree; j++) {
      result[j] = gfMul(result[j], root);
      if (j + 1 < degree) result[j] ^= result[j + 1];
    }
    root = gfMul(root, 0x02);
  }
  return result;
}

function rsRemainder(data: number[], divisor: number[]): number[] {
  const result = divisor.map(() => 0);
  for (const b of data) {
    const factor = b ^ (result.shift() as number);
    result.push(0);
    divisor.forEach((coef, i) => (result[i] ^= gfMul(coef, factor)));
  }
  return result;
}

const MASKS: ((x: number, y: number) => boolean)[] = [
  (x, y) => (x + y) % 2 === 0,
  (_, y) => y % 2 === 0,
  (x) => x % 3 === 0,
  (x, y) => (x + y) % 3 === 0,
  (x, y) => (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0,
  (x, y) => ((x * y) % 2) + ((x * y) % 3) === 0,
  (x, y) => (((x * y) % 2) + ((x * y) % 3)) % 2 === 0,
  (x, y) => (((x + y) % 2) + ((x * y) % 3)) % 2 === 0,
];

class Grid {
  readonly size: number;
  readonly version: number;
  private readonly m: boolean[][];
  private readonly fn: boolean[][];

  constructor(version: number) {
    this.version = version;
    this.size = version * 4 + 17;
    this.m = Array.from({ length: this.size }, () => new Array<boolean>(this.size).fill(false));
    this.fn = Array.from({ length: this.size }, () => new Array<boolean>(this.size).fill(false));
  }

  build(codewords: number[], forcedMask?: number): QrMatrix {
    this.drawFunctionPatterns();
    this.drawCodewords(codewords);
    let mask = forcedMask ?? -1;
    if (mask < 0) {
      let best = Infinity;
      for (let i = 0; i < 8; i++) {
        this.applyMask(i);
        this.drawFormatBits(i);
        const score = this.penalty();
        if (score < best) [best, mask] = [score, i];
        this.applyMask(i); // XOR undoes it
      }
    }
    this.applyMask(mask);
    this.drawFormatBits(mask);
    return this.m.map((row) => row.slice());
  }

  private set(x: number, y: number, dark: boolean) {
    this.m[y][x] = dark;
    this.fn[y][x] = true;
  }

  private drawFunctionPatterns() {
    const s = this.size;
    for (let i = 0; i < s; i++) {
      this.set(6, i, i % 2 === 0);
      this.set(i, 6, i % 2 === 0);
    }
    this.drawFinder(3, 3);
    this.drawFinder(s - 4, 3);
    this.drawFinder(3, s - 4);
    const pos = this.alignmentPositions();
    const n = pos.length;
    for (let i = 0; i < n; i++) {
      for (let j = 0; j < n; j++) {
        // Skip the three that would overlap a finder pattern.
        if ((i === 0 && j === 0) || (i === 0 && j === n - 1) || (i === n - 1 && j === 0)) continue;
        for (let dy = -2; dy <= 2; dy++)
          for (let dx = -2; dx <= 2; dx++) this.set(pos[i] + dx, pos[j] + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
      }
    }
    this.drawFormatBits(0); // reserve the area; redrawn with the real mask later
    this.drawVersion();
  }

  private drawFinder(cx: number, cy: number) {
    for (let dy = -4; dy <= 4; dy++) {
      for (let dx = -4; dx <= 4; dx++) {
        const x = cx + dx;
        const y = cy + dy;
        if (x < 0 || x >= this.size || y < 0 || y >= this.size) continue;
        const dist = Math.max(Math.abs(dx), Math.abs(dy));
        this.set(x, y, dist !== 2 && dist !== 4);
      }
    }
  }

  private alignmentPositions(): number[] {
    if (this.version === 1) return [];
    const n = Math.floor(this.version / 7) + 2;
    const step = Math.floor((this.version * 8 + n * 3 + 5) / (n * 4 - 4)) * 2;
    const out = [6];
    for (let p = this.size - 7; out.length < n; p -= step) out.splice(1, 0, p);
    return out;
  }

  private drawFormatBits(mask: number) {
    const data = (ECL_BITS << 3) | mask;
    let rem = data;
    for (let i = 0; i < 10; i++) rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
    const bits = ((data << 10) | rem) ^ 0x5412;
    const bit = (i: number) => ((bits >>> i) & 1) !== 0;
    const s = this.size;
    for (let i = 0; i <= 5; i++) this.set(8, i, bit(i));
    this.set(8, 7, bit(6));
    this.set(8, 8, bit(7));
    this.set(7, 8, bit(8));
    for (let i = 9; i < 15; i++) this.set(14 - i, 8, bit(i));
    for (let i = 0; i < 8; i++) this.set(s - 1 - i, 8, bit(i));
    for (let i = 8; i < 15; i++) this.set(8, s - 15 + i, bit(i));
    this.set(8, s - 8, true); // the dark module
  }

  private drawVersion() {
    if (this.version < 7) return;
    let rem = this.version;
    for (let i = 0; i < 12; i++) rem = (rem << 1) ^ ((rem >>> 11) * 0x1f25);
    const bits = (this.version << 12) | rem;
    for (let i = 0; i < 18; i++) {
      const dark = ((bits >>> i) & 1) !== 0;
      const a = this.size - 11 + (i % 3);
      const b = Math.floor(i / 3);
      this.set(a, b, dark);
      this.set(b, a, dark);
    }
  }

  private drawCodewords(data: number[]) {
    let i = 0;
    for (let right = this.size - 1; right >= 1; right -= 2) {
      if (right === 6) right = 5;
      const upward = ((right + 1) & 2) === 0;
      for (let v = 0; v < this.size; v++) {
        const y = upward ? this.size - 1 - v : v;
        for (let j = 0; j < 2; j++) {
          const x = right - j;
          if (this.fn[y][x] || i >= data.length * 8) continue;
          this.m[y][x] = ((data[i >>> 3] >>> (7 - (i & 7))) & 1) !== 0;
          i++;
        }
      }
    }
  }

  private applyMask(mask: number) {
    const f = MASKS[mask];
    for (let y = 0; y < this.size; y++)
      for (let x = 0; x < this.size; x++) if (!this.fn[y][x] && f(x, y)) this.m[y][x] = !this.m[y][x];
  }

  private penalty(): number {
    const s = this.size;
    const m = this.m;
    let score = 0;
    // Rules 1 and 3, along rows then columns.
    for (const vertical of [false, true]) {
      for (let a = 0; a < s; a++) {
        let color = false;
        let run = 0;
        const hist = [0, 0, 0, 0, 0, 0, 0];
        for (let b = 0; b < s; b++) {
          const dark = vertical ? m[b][a] : m[a][b];
          if (dark === color) {
            run++;
            if (run === 5) score += 3;
            else if (run > 5) score++;
          } else {
            this.addHistory(run, hist);
            if (!color) score += this.finderLike(hist) * 40;
            color = dark;
            run = 1;
          }
        }
        if (color) {
          this.addHistory(run, hist);
          run = 0;
        }
        this.addHistory(run + s, hist); // the quiet zone counts as light
        score += this.finderLike(hist) * 40;
      }
    }
    // Rule 2: 2x2 blocks of one color.
    for (let y = 0; y < s - 1; y++)
      for (let x = 0; x < s - 1; x++) {
        const c = m[y][x];
        if (c === m[y][x + 1] && c === m[y + 1][x] && c === m[y + 1][x + 1]) score += 3;
      }
    // Rule 4: dark/light balance.
    let dark = 0;
    for (const row of m) for (const c of row) if (c) dark++;
    const total = s * s;
    score += (Math.ceil(Math.abs(dark * 20 - total * 10) / total) - 1) * 10;
    return score;
  }

  private addHistory(run: number, hist: number[]) {
    if (hist[0] === 0) run += this.size; // light border before the first run
    hist.pop();
    hist.unshift(run);
  }

  /** Counts 1:1:3:1:1 dark/light patterns with 4 units of light on one side. */
  private finderLike(h: number[]): number {
    const n = h[1];
    const core = n > 0 && h[2] === n && h[3] === n * 3 && h[4] === n && h[5] === n;
    return (core && h[0] >= n * 4 && h[6] >= n ? 1 : 0) + (core && h[6] >= n * 4 && h[0] >= n ? 1 : 0);
  }
}
