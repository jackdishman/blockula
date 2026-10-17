import {
  BLOOD,
  Cells,
  clamp,
  EMBER,
  type Frame,
  glyph,
  Glyphs,
  idleFor,
  INK,
  input,
  lanternFrom,
  noise3,
  onFontReady,
  onQualityChange,
  onTick,
  quality,
  rushAt,
  smooth,
  startInput,
  View,
} from "./engine";
import { makeScene } from "./scenes";
import { motionOn, onMotionChange, season } from "./settings";
import { onBatBurst, onStage, onStageChange } from "./stage";

/**
 * The haunted backdrop behind every page, printed in ASCII on a fixed full-viewport canvas:
 * a 3D moon in the right margin whose phase follows the cursor (your lantern) and which
 * breathes when left alone, and ground fog that parts as the cursor moves through it and glows
 * warm where it rests. In the Halloween season there's more: a jack-o'-lantern in the fog that
 * watches the cursor (and gutters if you rush at it), and bats, crossing in flocks, bursting
 * out of the fog when you scroll fast, released by clicking empty paper, and now and then a
 * big 3D one swooping past. Pages can send bats flying too (see stage.burstBats). It all
 * keeps to the margins or fades behind the content column. With motion off it's a still print.
 */

export interface HauntOptions {
  /** Width of the page's content column, CSS px; the props keep to the margins beside it. */
  column?: number;
}

const FONT = 11;
const FOG = [".", ",", "-", "~", "="].map(glyph);

// Sprite bats: wings up, level, down, level. Spaces are transparent.
const SMALL = [["\\^v^/"], ["-^v^-"], ["/^v^\\"], ["-^v^-"]];
const BIG = [
  [" \\     / ", "  \\^v^/  ", "         "],
  ["         ", "~--^v^--~", "         "],
  ["         ", "  /^v^\\  ", " /     \\ "],
  ["         ", "~--^v^--~", "         "],
];
const SPRITES = [SMALL, BIG].map((frames) => frames.map((rows) => rows.map((r) => [...r].map((c) => (c === " " ? 0 : glyph(c))))));

interface Bat {
  x: number;
  y: number;
  vx: number;
  vy: number;
  phase: number;
  rate: number;
  big: boolean;
  alpha: number;
}

interface Swoop {
  start: number;
  dur: number;
  dir: number;
  y: number;
}

type Vec = [number, number, number];

export function mountHaunt(canvas: HTMLCanvasElement, options: HauntOptions = {}) {
  const ctx = canvas.getContext("2d");
  if (!ctx) return () => {};
  /** Content column plus breathing room, CSS px. */
  const COLUMN = (options.column ?? 580) + 60;
  const full = season() === "halloween";
  const cells = new Cells();
  const moon = makeScene("moon"), pumpkin = makeScene("pumpkin"), bat = makeScene("bat");
  // Keep the dark sides to a hatch so they never print as black blots in the margins.
  moon.maxInk = 0.6;
  pumpkin.maxInk = 0.74;
  const moonView = new View(), pumpkinView = new View(), batView = new View();
  const moonFrame: Frame = { t: 0, lookX: 0, lookY: 0, spin: 0 };
  const pumpkinFrame: Frame = { t: 0, lookX: 0, lookY: 0, spin: 0, fright: 0 };
  const batFrame: Frame = { t: 0, lookX: 0, lookY: 0, spin: 0 };
  const moonLight: Vec = [-0.7, 0.35, 0.3];
  const pumpkinLight: Vec = [0.5, 0.6, 0.6];
  const bats: Bat[] = [];
  let glyphs: Glyphs | null = null;
  let dpr = 1, W = 0, H = 0;
  let fog = new Float32Array(0);
  let stir = new Float32Array(0);
  let wind = 0;
  let swoop: Swoop | null = null;
  // Fade out the backdrop's moon or pumpkin while a page shows its own.
  let moonShown = 1, pumpkinShown = 1;
  let nextSwoop = -1, nextFlock = -1, scrollCooldown = 0;
  let dead = false;
  let still = !motionOn();

  const setup = () => {
    dpr = Math.min(quality.dpr, window.devicePixelRatio || 1);
    W = window.innerWidth;
    H = window.innerHeight;
    canvas.width = Math.round(W * dpr);
    canvas.height = Math.round(H * dpr);
    glyphs = new Glyphs(FONT, dpr);
    cells.resize(Math.ceil(canvas.width / glyphs.cw), Math.ceil(canvas.height / glyphs.ch));
    if (fog.length !== cells.glyph.length) {
      fog = new Float32Array(cells.glyph.length);
      stir = new Float32Array(cells.glyph.length);
    }
  };

  // Where the props sit, in CSS px. The moon rises a little as you scroll.
  const layout = () => {
    const margin = (W - COLUMN) / 2;
    const wide = margin >= 190;
    const moonSize = wide ? clamp(margin * 0.72, 130, 300) : 120;
    const moonAt = wide
      ? { x: W - margin / 2, y: 104 + moonSize / 2 - input.scrollY * 0.12, a: 0.82 }
      : { x: W - 26, y: 150 - input.scrollY * 0.12, a: 0.2 };
    const pumpkinSize = full && margin >= 210 ? clamp(margin * 0.6, 140, 280) : 0;
    return { moonSize, moonAt, pumpkinSize, pumpkinAt: { x: margin / 2, y: H - pumpkinSize * 0.46 } };
  };

  /** A w×h CSS px block of cells centered on (x, y). */
  const block = (x: number, y: number, w: number, h = w) => {
    const g = glyphs as Glyphs;
    const cols = Math.max(4, Math.round((w * dpr) / g.cw)), rows = Math.max(3, Math.round((h * dpr) / g.ch));
    return { c0: Math.round((x * dpr) / g.cw - cols / 2), r0: Math.round((y * dpr) / g.ch - rows / 2), cols, rows };
  };

  const ease = (target: Vec, into: Vec, k: number) => {
    for (let i = 0; i < 3; i++) into[i] += (target[i] - into[i]) * k;
    const l = Math.hypot(...into) || 1;
    for (let i = 0; i < 3; i++) into[i] /= l;
  };

  const addBat = (x: number, y: number, vx: number, vy: number, big = Math.random() < 0.4) =>
    bats.push({ x, y, vx, vy, phase: Math.random() * 4, rate: big ? 7 : 10, big, alpha: big ? 0.72 : 0.5 });

  /** A few bats crossing the sky from one side. */
  const flock = (n: number) => {
    const dir = Math.random() < 0.5 ? 1 : -1, y = H * (0.1 + Math.random() * 0.32);
    for (let i = 0; i < n; i++) {
      const x = dir > 0 ? -40 - i * 50 - Math.random() * 40 : W + 40 + i * 50 + Math.random() * 40;
      addBat(x, y + (Math.random() - 0.5) * 70, dir * (75 + Math.random() * 70), (Math.random() - 0.5) * 20);
    }
  };

  /** Startled bats bursting up out of the fog in the margins. */
  const startle = (n: number) => {
    const margin = Math.max(60, (W - COLUMN) / 2);
    for (let i = 0; i < n; i++) {
      const side = Math.random() < 0.5 ? -1 : 1;
      const x = side < 0 ? Math.random() * margin : W - Math.random() * margin;
      addBat(x, H + 10 + Math.random() * 60, side * (30 + Math.random() * 70), -170 - Math.random() * 130);
    }
  };

  /** Bats bursting out of a point (a page event), mostly upward and outward. */
  const burst = (x: number, y: number, n: number) => {
    if (still) return;
    for (let i = 0; i < n; i++) {
      const a = -Math.PI / 2 + (i / Math.max(1, n - 1) - 0.5) * 2.6 + (Math.random() - 0.5) * 0.3;
      const v = 160 + Math.random() * 120;
      addBat(x + (Math.random() - 0.5) * 30, y + (Math.random() - 0.5) * 20, Math.cos(a) * v, Math.sin(a) * v, Math.random() < 0.6);
    }
  };

  // Clicking empty paper lets a bat out.
  const release = (e: PointerEvent) => {
    if (still || !full) return;
    const el = e.target as Element | null;
    if (el?.closest?.("a, button, input, select, textarea, label, summary, [role=button], .card, pre, code, table")) return;
    addBat(e.clientX, e.clientY, (e.clientX < W / 2 ? -1 : 1) * (90 + Math.random() * 60), -70 - Math.random() * 40, true);
  };

  const frame = (t: number, dt: number) => {
    if (!glyphs) return;
    const g = glyphs;
    const { cols, rows } = cells;
    cells.clear();
    const L = layout();
    const idle = idleFor() > 6;

    // --- Fog: two octaves of drifting noise, densest at the foot of the page.
    wind += dt * (0.035 + clamp(input.scrollV * 0.0005, -1.5, 1.5));
    const fogTop = H * 0.5;
    const pointerFresh = !still && input.seen && !idle;
    const px = input.x, py = input.y;
    // Moving the cursor through the fog parts it; the wake fills back in over a second or two.
    if (!still && input.seen && input.speed > 40 && py > fogTop - 60) {
      const amt = clamp(input.speed / 1400) * 0.55;
      const pc = Math.round((px * dpr) / g.cw), pr = Math.round((py * dpr) / g.ch);
      for (let r = pr - 4; r <= pr + 4; r++) {
        if (r < 0 || r >= rows) continue;
        for (let c = pc - 9; c <= pc + 9; c++) {
          if (c < 0 || c >= cols) continue;
          const d = Math.hypot((c - pc) / 9, (r - pr) / 4);
          if (d < 1) {
            const i = r * cols + c;
            stir[i] = Math.min(1.3, stir[i] + amt * (1 - d));
          }
        }
      }
    }
    const decay = Math.exp(-dt * 1.1);
    for (let r = 0, i = 0; r < rows; r++) {
      const y = ((r + 0.5) * g.ch) / dpr;
      if (y < fogTop - 30) {
        fog.fill(0, i, i + cols);
        stir.fill(0, i, i + cols);
        i += cols;
        continue;
      }
      const hm = smooth(fogTop, H * 1.02, y);
      const ny = (y + input.scrollY * 0.35) * 0.011;
      for (let c = 0; c < cols; c++, i++) {
        stir[i] *= decay;
        const x = ((c + 0.5) * g.cw) / dpr;
        const nx = x * 0.0055 + wind;
        const n = noise3(nx, ny, t * 0.07) * 0.62 + noise3(nx * 2.3 - t * 0.04, ny * 2.3, t * 0.12 + 9) * 0.38;
        // Banks of mist with clear gaps between them, thickening toward the ground.
        const d = smooth(0.42, 0.78, n + hm * 0.2) * (0.25 + 0.75 * hm) - stir[i] * 1.5;
        fog[i] = d;
        if (d <= 0.06) continue;
        const cm = 0.3 + 0.7 * smooth(COLUMN * 0.4, COLUMN * 0.66, Math.abs(x - W / 2));
        let alpha = (0.12 + 0.3 * hm) * cm * Math.min(1, d * 2.5);
        let color = INK;
        // A lantern resting in the mist warms it.
        if (pointerFresh) {
          const glow = smooth(150, 25, Math.hypot(x - px, (y - py) * 1.6));
          if (glow > 0.2) {
            color = glow > 0.62 ? EMBER : BLOOD;
            alpha = Math.max(alpha, 0.55 * glow);
          }
        }
        cells.put(c, r, FOG[Math.min(FOG.length - 1, Math.floor(d * 1.15 * FOG.length))], color, alpha);
      }
    }

    // --- Moon: lit from the cursor. Near it, it's full; far away, it wanes to a crescent. Left
    // alone, it breathes: its phases wheel slowly round and it swells a little.
    const fade = 1 - Math.exp(-dt * 4);
    moonShown += ((onStage.get("moon") ? 0 : 1) - moonShown) * (dt ? fade : 1);
    pumpkinShown += ((onStage.get("pumpkin") ? 0 : 1) - pumpkinShown) * (dt ? fade : 1);
    if (moonShown > 0.03) {
      const { x, y, a } = L.moonAt;
      let target: Vec = [-0.75, 0.3, 0.35];
      let zoom = 1;
      if (!still && idle) {
        const ang = t * 0.3;
        target = [Math.cos(ang), 0.2, Math.sin(ang)];
        zoom = 1 + 0.03 * Math.sin(t * 1.1);
      } else if (input.seen) {
        target = lanternFrom(x, y, 420 - Math.hypot(input.x - x, input.y - y) * 0.45, moonLight);
      }
      ease(target, moonLight, dt ? 1 - Math.exp(-dt * 2.5) : 1);
      moonFrame.t = t;
      moonFrame.spin += dt * clamp(input.scrollV * 0.002, -3, 3);
      const b = block(x, y, L.moonSize);
      moonView.render(moon, moonFrame, cells, b.c0, b.r0, b.cols, b.rows, g.cw, g.ch, moonLight, a * moonShown, zoom);
    }

    // --- The big 3D bat, crossing in front of the moon now and then.
    if (swoop) {
      const p = (t - swoop.start) / swoop.dur;
      if (p >= 1) swoop = null;
      else {
        const size = 170 + 90 * Math.sin(p * Math.PI);
        const x = swoop.dir > 0 ? -size + (W + size * 2) * p : W + size - (W + size * 2) * p;
        const y = swoop.y + Math.sin(p * Math.PI) * -60 + Math.sin(t * 3) * 10;
        batFrame.t = t;
        batFrame.lookX = swoop.dir * 0.9;
        batFrame.lookY = 0.2;
        const b = block(x, y, size, size * 0.6);
        batView.render(bat, batFrame, cells, b.c0, b.r0, b.cols, b.rows, g.cw, g.ch, [0.2, 0.7, 0.68], 0.78);
      }
    }

    // --- Jack-o'-lantern in the fog; it watches the cursor, and gutters if you rush at it.
    if (L.pumpkinSize && pumpkinShown > 0.03) {
      const { x, y } = L.pumpkinAt;
      const k = dt ? 1 - Math.exp(-dt * 3) : 1;
      pumpkinFrame.lookX += ((input.seen ? clamp((input.x - x) / 700, -1, 1) : 0.3) - pumpkinFrame.lookX) * k;
      pumpkinFrame.lookY += ((input.seen ? clamp((y - input.y) / 600, -1, 1) : 0.2) - pumpkinFrame.lookY) * k;
      pumpkinFrame.fright = still ? 0 : Math.max(rushAt(x, y), (pumpkinFrame.fright ?? 0) - dt * 0.9);
      pumpkinFrame.t = t;
      ease(lanternFrom(x, y, 380, [0.5, 0.6, 0.6]), pumpkinLight, k);
      const b = block(x, y, L.pumpkinSize);
      pumpkinView.render(pumpkin, pumpkinFrame, cells, b.c0, b.r0, b.cols, b.rows, g.cw, g.ch, pumpkinLight, 0.92 * pumpkinShown);
      // Thick mist curls around its base.
      for (let r = Math.max(0, b.r0 + Math.floor(b.rows * 0.7)); r < Math.min(rows, b.r0 + b.rows); r++) {
        for (let c = Math.max(0, b.c0); c < Math.min(cols, b.c0 + b.cols); c++) {
          const i = r * cols + c;
          if (fog[i] > 0.6) cells.put(c, r, FOG[Math.min(FOG.length - 1, Math.floor(fog[i] * 1.15 * FOG.length))], INK, 0.4);
        }
      }
    }

    // --- Sprite bats. Out of season only page events send any.
    if (still) return paint();
    if (full) {
      if (nextFlock < 0) nextFlock = t + 2.5;
      if (nextSwoop < 0) nextSwoop = t + 7;
      if (t > nextFlock) {
        flock(1 + Math.floor(Math.random() * 3));
        nextFlock = t + 9 + Math.random() * 10;
      }
      if (t > nextSwoop && !swoop) {
        swoop = { start: t, dur: 4.2, dir: Math.random() < 0.5 ? 1 : -1, y: H * (0.22 + Math.random() * 0.12) };
        nextSwoop = t + 28 + Math.random() * 20;
      }
      scrollCooldown -= dt;
      if (Math.abs(input.scrollV) > 1300 && scrollCooldown <= 0) {
        startle(3 + Math.floor(Math.random() * 3));
        scrollCooldown = 2.5;
      }
    }
    for (let i = bats.length - 1; i >= 0; i--) {
      const b = bats[i];
      b.phase += b.rate * dt;
      // Scatter away from the cursor.
      if (input.seen) {
        const dx = b.x - input.x, dy = b.y - input.y, d = Math.hypot(dx, dy);
        if (d < 130 && d > 1) {
          b.vx += (dx / d) * 500 * dt;
          b.vy += (dy / d) * 500 * dt;
        }
      }
      b.x += b.vx * dt;
      b.y += (b.vy + Math.sin(b.phase * 0.4) * 18) * dt;
      if (b.x < -120 || b.x > W + 120 || b.y < -80 || b.y > H + 80) {
        bats.splice(i, 1);
        continue;
      }
      const sprite = SPRITES[b.big ? 1 : 0][Math.floor(b.phase) & 3];
      const c0 = Math.round((b.x * dpr) / g.cw - sprite[0].length / 2), r0 = Math.round((b.y * dpr) / g.ch - sprite.length / 2);
      sprite.forEach((row, r) => row.forEach((k, c) => k && cells.put(c0 + c, r0 + r, k, INK, b.alpha)));
    }
    paint();
  };

  const paint = () => {
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    if (glyphs) cells.paint(ctx, glyphs);
  };

  const now = () => performance.now() / 1000;
  const redraw = () => {
    setup();
    if (still) frame(now(), 0);
  };

  // Motion can be switched on and off while we're mounted. A still print redraws only when
  // something changes: size, the font, or which props the page shows.
  let stopTicking = () => {};
  const apply = () => {
    still = !motionOn();
    stopTicking();
    stopTicking = still ? onStageChange(redraw) : onTick(frame);
    if (still) {
      bats.length = 0;
      swoop = null;
      frame(now(), 0);
    }
  };

  startInput();
  setup();
  frame(now(), 0);
  apply();
  const unwatch = [onMotionChange(apply), onQualityChange(redraw), onBatBurst(burst)];
  window.addEventListener("resize", redraw);
  window.addEventListener("pointerdown", release, { passive: true });
  onFontReady(FONT, () => !dead && redraw());
  return () => {
    dead = true;
    stopTicking();
    unwatch.forEach((fn) => fn());
    window.removeEventListener("resize", redraw);
    window.removeEventListener("pointerdown", release);
  };
}
