/**
 * A small ASCII 3D renderer for the wallet's haunted decorations.
 *
 * Scenes (signed distance fields or analytic shapes) are ray-cast into a grid of character
 * cells and shaded like an engraving on the page's bone paper: lit surfaces stay mostly bare,
 * shadow fills with dense ink, and silhouettes are drawn with contour strokes (| / - \) that
 * follow the shape. Glowing things (a candle flame, a jack-o'-lantern's insides) print in the
 * spot colors. Each row is printed as a few monospace text runs (one per color and opacity),
 * so even a full-screen grid costs a few hundred fillText calls per frame.
 */

// ---------------------------------------------------------------------------------------------
// Glyphs

/** Cell height as a multiple of font size. */
export const LINE = 1.16;

export const CHARS = " .,'`:;-~_=+*^v<>|/\\!()#%@ox";
const CODES = Uint16Array.from(CHARS, (c) => c.charCodeAt(0));
const LOOKUP = new Int16Array(128);
for (let i = 0; i < CHARS.length; i++) LOOKUP[CHARS.charCodeAt(i)] = i;
/** Index of a character in CHARS (0, the space, for anything unknown). */
export const glyph = (c: string) => LOOKUP[c.charCodeAt(0) & 127] ?? 0;

export const INK = 0;
export const BLOOD = 1;
export const EMBER = 2;
const COLORS = ["#17120f", "#8d1418", "#c0561b"];
/** Opacity is quantized to this many steps so cells can share text runs. */
const LEVELS = 12;

/** The page's mono face (Courier Prime via next/font), for canvas text. */
export function monoFamily() {
  if (typeof document === "undefined") return "monospace";
  const v = getComputedStyle(document.documentElement).getPropertyValue("--font-courier").trim();
  return v ? `${v}, "Courier New", monospace` : `"Courier Prime", "Courier New", monospace`;
}

let measurer: CanvasRenderingContext2D | null = null;

/** Type metrics for one font size at one pixel ratio. */
export class Glyphs {
  readonly font: string;
  /** Cell size in device pixels; the width is the font's real advance so runs stay on the grid. */
  readonly cw: number;
  readonly ch: number;
  constructor(readonly size: number, readonly dpr: number) {
    this.font = `${size * dpr}px ${monoFamily()}`;
    measurer ??= document.createElement("canvas").getContext("2d");
    let cw = size * 0.6 * dpr;
    if (measurer) {
      measurer.font = this.font;
      cw = measurer.measureText("0".repeat(20)).width / 20 || cw;
    }
    this.cw = cw;
    this.ch = Math.max(1, Math.round(size * LINE * dpr));
  }
}

/** Re-measure once the mono webfont arrives (it swaps in after first paint). */
export function onFontReady(size: number, cb: () => void) {
  if (typeof document === "undefined" || !document.fonts) return;
  document.fonts.load(`${size}px ${monoFamily()}`).then(cb, () => {});
}

// ---------------------------------------------------------------------------------------------
// Cells: what's printed where

export class Cells {
  cols = 0;
  rows = 0;
  glyph = new Uint8Array(0);
  color = new Uint8Array(0);
  alpha = new Float32Array(0);
  private keys = new Uint8Array(0);
  private run = new Uint16Array(0);
  private seen = new Uint8Array(COLORS.length * (LEVELS + 1));

  resize(cols: number, rows: number) {
    if (cols === this.cols && rows === this.rows) return;
    this.cols = cols;
    this.rows = rows;
    this.glyph = new Uint8Array(cols * rows);
    this.color = new Uint8Array(cols * rows);
    this.alpha = new Float32Array(cols * rows);
    this.keys = new Uint8Array(cols);
    this.run = new Uint16Array(cols);
  }

  clear() {
    this.glyph.fill(0);
  }

  put(c: number, r: number, g: number, color: number, alpha: number) {
    if (c < 0 || r < 0 || c >= this.cols || r >= this.rows) return;
    const i = r * this.cols + c;
    this.glyph[i] = g;
    this.color[i] = color;
    this.alpha[i] = alpha;
  }

  paint(ctx: CanvasRenderingContext2D, g: Glyphs, x0 = 0, y0 = 0) {
    const { cols, rows, glyph: gl, color, alpha, keys, run, seen } = this;
    ctx.font = g.font;
    ctx.textAlign = "left";
    ctx.textBaseline = "middle";
    for (let r = 0; r < rows; r++) {
      const base = r * cols;
      seen.fill(0);
      let any = false;
      for (let c = 0; c < cols; c++) {
        if (!gl[base + c]) continue;
        const k = color[base + c] * (LEVELS + 1) + Math.round(clamp(alpha[base + c]) * LEVELS);
        keys[c] = k;
        seen[k] = 1;
        any = true;
      }
      if (!any) continue;
      for (let k = 0; k < seen.length; k++) {
        if (!seen[k] || !(k % (LEVELS + 1))) continue;
        let first = -1, last = -1;
        for (let c = 0; c < cols; c++) {
          if (gl[base + c] && keys[c] === k) {
            run[c] = CODES[gl[base + c]];
            if (first < 0) first = c;
            last = c;
          } else run[c] = 32;
        }
        ctx.fillStyle = COLORS[Math.floor(k / (LEVELS + 1))];
        ctx.globalAlpha = (k % (LEVELS + 1)) / LEVELS;
        ctx.fillText(String.fromCharCode.apply(null, run.subarray(first, last + 1) as unknown as number[]), x0 + first * g.cw, y0 + (r + 0.55) * g.ch);
      }
    }
    ctx.globalAlpha = 1;
  }
}

// ---------------------------------------------------------------------------------------------
// Math

export type M3 = Float64Array; // row-major 3×3

export const m3 = (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number): M3 =>
  Float64Array.of(a, b, c, d, e, f, g, h, i);
export const rotX = (t: number) => m3(1, 0, 0, 0, Math.cos(t), -Math.sin(t), 0, Math.sin(t), Math.cos(t));
export const rotY = (t: number) => m3(Math.cos(t), 0, Math.sin(t), 0, 1, 0, -Math.sin(t), 0, Math.cos(t));
export const rotZ = (t: number) => m3(Math.cos(t), -Math.sin(t), 0, Math.sin(t), Math.cos(t), 0, 0, 0, 1);
export function mul(...ms: M3[]): M3 {
  let a = ms[0];
  for (let k = 1; k < ms.length; k++) {
    const b = ms[k];
    const o = new Float64Array(9);
    for (let r = 0; r < 3; r++)
      for (let c = 0; c < 3; c++) o[r * 3 + c] = a[r * 3] * b[c] + a[r * 3 + 1] * b[3 + c] + a[r * 3 + 2] * b[6 + c];
    a = o;
  }
  return a;
}

export const clamp = (x: number, a = 0, b = 1) => (x < a ? a : x > b ? b : x);
export const mix = (a: number, b: number, t: number) => a + (b - a) * t;
export const smooth = (a: number, b: number, x: number) => {
  const t = clamp((x - a) / (b - a));
  return t * t * (3 - 2 * t);
};

const PERM = new Uint8Array(512);
{
  let s = 1337;
  const p = Array.from({ length: 256 }, (_, i) => i);
  for (let i = 255; i > 0; i--) {
    s = (s * 1103515245 + 12345) & 0x7fffffff;
    const j = s % (i + 1);
    [p[i], p[j]] = [p[j], p[i]];
  }
  for (let i = 0; i < 512; i++) PERM[i] = p[i & 255];
}
const h3 = (i: number, j: number, k: number) => PERM[PERM[PERM[i & 255] + (j & 255)] + (k & 255)] / 255;

/** Smooth 3D value noise in [0, 1]. */
export function noise3(x: number, y: number, z: number) {
  const xi = Math.floor(x), yi = Math.floor(y), zi = Math.floor(z);
  let u = x - xi, v = y - yi, w = z - zi;
  u = u * u * (3 - 2 * u);
  v = v * v * (3 - 2 * v);
  w = w * w * (3 - 2 * w);
  const a = mix(mix(h3(xi, yi, zi), h3(xi + 1, yi, zi), u), mix(h3(xi, yi + 1, zi), h3(xi + 1, yi + 1, zi), u), v);
  const b = mix(mix(h3(xi, yi, zi + 1), h3(xi + 1, yi, zi + 1), u), mix(h3(xi, yi + 1, zi + 1), h3(xi + 1, yi + 1, zi + 1), u), v);
  return mix(a, b, w);
}

// ---------------------------------------------------------------------------------------------
// Scenes

export const SURFACE = 0;
export const GLOW = 1;

/** Filled in by a scene's intersect(): object-space hit point and normal, plus material. */
export interface Hit {
  t: number;
  x: number;
  y: number;
  z: number;
  nx: number;
  ny: number;
  nz: number;
  mat: number;
  /** SURFACE: how much light the ink reflects (1 = bare paper when lit). GLOW: brightness. */
  value: number;
}

/** Per-frame inputs a scene poses itself from. */
export interface Frame {
  t: number;
  /** Eased cursor direction relative to the object, roughly -1..1 (x right, y up). */
  lookX: number;
  lookY: number;
  /** Extra rotation from scrolling or a busy state, radians. */
  spin: number;
}

export interface Scene {
  /** Bounding sphere radius around the object-space origin. */
  radius: number;
  /** Half-extents, in object units, that the view must show. */
  fit: [number, number];
  /** Object→world rotation for this frame (and any animation state). */
  pose(f: Frame): M3;
  intersect(ox: number, oy: number, oz: number, dx: number, dy: number, dz: number, t0: number, t1: number, h: Hit): boolean;
  /** Custom lighting in object space; defaults to Lambert + a little specular from the light. */
  light?(h: Hit, lx: number, ly: number, lz: number, vx: number, vy: number, vz: number): number;
  /** Ramp ceiling: 1 lets the deepest shadow print as '@'. */
  maxInk?: number;
}

export type Sdf = (x: number, y: number, z: number) => number;

/** Sphere-trace an SDF. Fills position + normal; the caller sets the material. */
export function march(sdf: Sdf, ox: number, oy: number, oz: number, dx: number, dy: number, dz: number, t0: number, t1: number, h: Hit, k = 0.9, steps = 90) {
  let t = t0;
  for (let i = 0; i < steps && t < t1; i++) {
    const x = ox + dx * t, y = oy + dy * t, z = oz + dz * t;
    const d = sdf(x, y, z);
    if (d < 0.0015) {
      h.t = t;
      h.x = x;
      h.y = y;
      h.z = z;
      const e = 0.002;
      const a = sdf(x + e, y - e, z - e), b = sdf(x - e, y - e, z + e), c = sdf(x - e, y + e, z - e), g = sdf(x + e, y + e, z + e);
      const nx = a - b - c + g, ny = -a - b + c + g, nz = -a + b - c + g;
      const l = Math.hypot(nx, ny, nz) || 1;
      h.nx = nx / l;
      h.ny = ny / l;
      h.nz = nz / l;
      return true;
    }
    t += d * k;
  }
  return false;
}

// SDF primitives
export const sdBox = (x: number, y: number, z: number, bx: number, by: number, bz: number) => {
  const qx = Math.abs(x) - bx, qy = Math.abs(y) - by, qz = Math.abs(z) - bz;
  return Math.hypot(Math.max(qx, 0), Math.max(qy, 0), Math.max(qz, 0)) + Math.min(Math.max(qx, qy, qz), 0);
};
/** Capped cylinder along x. */
export const sdCylX = (x: number, y: number, z: number, r: number, h: number) => {
  const a = Math.abs(x) - h, b = Math.hypot(y, z) - r;
  return Math.min(Math.max(a, b), 0) + Math.hypot(Math.max(a, 0), Math.max(b, 0));
};
/** Capped cylinder along y. */
export const sdCylY = (x: number, y: number, z: number, r: number, h: number) => sdCylX(y, x, z, r, h);
/** Ring lying in the xy-plane (axis z). */
export const sdTorusZ = (x: number, y: number, z: number, R: number, r: number) => Math.hypot(Math.hypot(x, y) - R, z) - r;
/** Ring lying in the xz-plane (axis y). */
export const sdTorusY = (x: number, y: number, z: number, R: number, r: number) => Math.hypot(Math.hypot(x, z) - R, y) - r;
export const sdCapsule = (x: number, y: number, z: number, ax: number, ay: number, az: number, bx: number, by: number, bz: number, r: number) => {
  const px = x - ax, py = y - ay, pz = z - az, qx = bx - ax, qy = by - ay, qz = bz - az;
  const k = clamp((px * qx + py * qy + pz * qz) / (qx * qx + qy * qy + qz * qz));
  return Math.hypot(px - qx * k, py - qy * k, pz - qz * k) - r;
};
export const smin = (a: number, b: number, k: number) => {
  const h = clamp(0.5 + (0.5 * (b - a)) / k);
  return mix(b, a, h) - k * h * (1 - h);
};

// ---------------------------------------------------------------------------------------------
// Rendering a scene into a block of cells

/** Light to dark. Lit surfaces keep a dot so the form still reads on paper. */
const SHADE = [".", ":", "-", "=", "+", "*", "#", "%", "@"].map(glyph);
const GLOWS = [":", "+", "*", "#", "@"].map(glyph);
const G_PIPE = glyph("|"), G_BACK = glyph("\\"), G_DASH = glyph("-"), G_SLASH = glyph("/");
const CAMERA = 6;

export class View {
  private depth = new Float32Array(0);
  private value = new Float32Array(0);
  private mat = new Uint8Array(0);
  private hit: Hit = { t: 0, x: 0, y: 0, z: 0, nx: 0, ny: 0, nz: 0, mat: 0, value: 0 };

  /**
   * Render `scene` into the cols×rows block of `out` whose top-left cell is (c0, r0). Cells the
   * object doesn't cover are left alone, so it composites over whatever is already there.
   * `light` is the world-space direction toward the light (x right, y up, z toward the viewer).
   */
  render(
    scene: Scene,
    frame: Frame,
    out: Cells,
    c0: number,
    r0: number,
    cols: number,
    rows: number,
    cw: number,
    ch: number,
    light: [number, number, number],
    alpha = 1,
  ) {
    const n = cols * rows;
    if (this.depth.length < n) {
      this.depth = new Float32Array(n);
      this.value = new Float32Array(n);
      this.mat = new Uint8Array(n);
    }
    const { depth, value, mat, hit: h } = this;
    const R = scene.pose(frame);
    const W = cols * cw, H = rows * ch;
    const S = Math.min(W / (2 * scene.fit[0]), H / (2 * scene.fit[1]));
    // Camera and light into object space (R⁻¹ = Rᵀ).
    const ox = R[6] * CAMERA, oy = R[7] * CAMERA, oz = R[8] * CAMERA;
    const [Lx, Ly, Lz] = light;
    const lx = R[0] * Lx + R[3] * Ly + R[6] * Lz, ly = R[1] * Lx + R[4] * Ly + R[7] * Lz, lz = R[2] * Lx + R[5] * Ly + R[8] * Lz;
    const rad = scene.radius, far = Math.fround(CAMERA + rad * 2); // as stored in a Float32Array
    const oo = ox * ox + oy * oy + oz * oz - rad * rad;

    for (let j = 0, i = 0; j < rows; j++) {
      const Y = -((j + 0.5) * ch - H / 2) / S;
      for (let k = 0; k < cols; k++, i++) {
        depth[i] = far;
        const X = ((k + 0.5) * cw - W / 2) / S;
        const il = 1 / Math.sqrt(X * X + Y * Y + CAMERA * CAMERA);
        const wx = X * il, wy = Y * il, wz = -CAMERA * il;
        const dx = R[0] * wx + R[3] * wy + R[6] * wz, dy = R[1] * wx + R[4] * wy + R[7] * wz, dz = R[2] * wx + R[5] * wy + R[8] * wz;
        const b = ox * dx + oy * dy + oz * dz;
        const disc = b * b - oo;
        if (disc < 0) continue;
        const sq = Math.sqrt(disc);
        if (!scene.intersect(ox, oy, oz, dx, dy, dz, Math.max(0, -b - sq), -b + sq, h)) continue;
        depth[i] = h.t;
        mat[i] = h.mat;
        if (h.mat === GLOW) {
          value[i] = h.value;
        } else if (scene.light) {
          value[i] = scene.light(h, lx, ly, lz, -dx, -dy, -dz);
        } else {
          const ndl = h.nx * lx + h.ny * ly + h.nz * lz;
          const diff = Math.max(0, ndl);
          // Blinn-ish highlight: a polished key glints bare paper.
          const hx = lx - dx, hy = ly - dy, hz = lz - dz;
          const hl = Math.hypot(hx, hy, hz) || 1;
          const spec = Math.pow(Math.max(0, (h.nx * hx + h.ny * hy + h.nz * hz) / hl), 24);
          value[i] = clamp(h.value * (0.1 + 0.9 * diff) + 0.5 * spec);
        }
      }
    }

    // Ink it: contour strokes where a neighbour falls away behind, shading elsewhere.
    const maxInk = scene.maxInk ?? 1;
    const at = (c: number, r: number) => (c < 0 || r < 0 || c >= cols || r >= rows ? far : depth[r * cols + c]);
    const gap = 0.22;
    for (let j = 0, i = 0; j < rows; j++) {
      for (let k = 0; k < cols; k++, i++) {
        const d = depth[i];
        if (d >= far) continue;
        let g: number;
        let color = INK;
        if (mat[i] === GLOW) {
          const v = clamp(value[i]);
          g = GLOWS[Math.min(GLOWS.length - 1, Math.floor(v * GLOWS.length))];
          color = v > 0.72 ? EMBER : BLOOD;
        } else {
          const l = at(k - 1, j), r = at(k + 1, j), u = at(k, j - 1), w = at(k, j + 1);
          const edge = l > d + gap || r > d + gap || u > d + gap || w > d + gap;
          if (edge) {
            // Sobel on (clamped) depth: the gradient points out of the shape.
            const cl = (v: number) => Math.min(v, d + 1);
            const ul = cl(at(k - 1, j - 1)), ur = cl(at(k + 1, j - 1)), dl = cl(at(k - 1, j + 1)), dr = cl(at(k + 1, j + 1));
            const gx = (ur + 2 * cl(r) + dr - ul - 2 * cl(l) - dl) / cw;
            const gy = (dl + 2 * cl(w) + dr - ul - 2 * cl(u) - ur) / ch;
            // Normal angle (y up) folded to 0..180; Courier's / and \ lean about 62°.
            let a = (Math.atan2(-gy, gx) * 180) / Math.PI;
            if (a < 0) a += 180;
            g = a < 14 || a >= 166 ? G_PIPE : a < 59 ? G_BACK : a < 121 ? G_DASH : G_SLASH;
          } else {
            const ink = Math.min(maxInk, 1 - clamp(value[i]));
            g = SHADE[Math.min(SHADE.length - 1, Math.floor(ink * SHADE.length))];
          }
        }
        out.put(c0 + k, r0 + j, g, color, alpha);
      }
    }
  }
}

// ---------------------------------------------------------------------------------------------
// Shared clock and input

export const input = {
  /** Pointer in CSS px (viewport), and whether we've seen one yet. */
  x: 0,
  y: 0,
  seen: false,
  /** Pointer speed in px/s, smoothed. */
  speed: 0,
  movedAt: 0,
  scrollY: 0,
  /** Scroll velocity in px/s, smoothed. */
  scrollV: 0,
  reduced: false,
};

let tracking = false;
let lastX = 0, lastY = 0, lastScroll = 0;
function track() {
  if (tracking || typeof window === "undefined") return;
  tracking = true;
  const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
  input.reduced = mq.matches;
  mq.addEventListener?.("change", () => (input.reduced = mq.matches));
  input.scrollY = lastScroll = window.scrollY;
  window.addEventListener(
    "pointermove",
    (e) => {
      input.x = e.clientX;
      input.y = e.clientY;
      if (!input.seen) {
        lastX = e.clientX;
        lastY = e.clientY;
      }
      input.seen = true;
      input.movedAt = performance.now();
    },
    { passive: true },
  );
}

type Tick = (t: number, dt: number) => void;
const subs = new Set<Tick>();
let raf = 0;
let prev = 0;
const FRAME = 1000 / 30; // ASCII doesn't need more, and it keeps the CPU calm.

function loop(now: number) {
  raf = requestAnimationFrame(loop);
  if (now - prev < FRAME - 2) return;
  const dt = Math.min(0.1, (now - prev) / 1000);
  prev = now;
  const sy = window.scrollY;
  input.scrollV = mix(input.scrollV, (sy - lastScroll) / dt, 0.25);
  input.scrollY = lastScroll = sy;
  input.speed = mix(input.speed, Math.hypot(input.x - lastX, input.y - lastY) / dt, 0.3);
  lastX = input.x;
  lastY = input.y;
  for (const fn of subs) fn(now / 1000, dt);
}

/** Run `fn` on the shared ~30fps clock until the returned function is called. */
export function onTick(fn: Tick) {
  track();
  subs.add(fn);
  if (!raf) {
    prev = performance.now();
    raf = requestAnimationFrame(loop);
  }
  return () => {
    subs.delete(fn);
    if (!subs.size && raf) {
      cancelAnimationFrame(raf);
      raf = 0;
    }
  };
}

export function reducedMotion() {
  track();
  return input.reduced;
}

/** Direction from a point on screen (CSS px) toward the cursor "lantern", world space. */
export function lanternFrom(cx: number, cy: number, depth: number, fallback: [number, number, number]): [number, number, number] {
  if (!input.seen) return fallback;
  const x = input.x - cx, y = cy - input.y;
  const l = Math.hypot(x, y, depth) || 1;
  return [x / l, y / l, depth / l];
}
