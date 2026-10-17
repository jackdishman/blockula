import { Cells, clamp, type Frame, Glyphs, idleFor, input, lanternFrom, onFontReady, onTick, rushAt, startInput, View } from "./engine";
import { makeScene, type PropKind } from "./scenes";
import { motionOn, onMotionChange } from "./settings";
import { burstBats, stage } from "./stage";

const FONT = 9;
const DEFAULT_LIGHT: [number, number, number] = [-0.45, 0.55, 0.7];

export interface PropOptions {
  /** Spin fast while true (the key turns while you unlock; the coin when money arrives). */
  busy: () => boolean;
  /** Lettering, for props that carry some (the gravestone). */
  text: () => string | undefined;
  /** Bats to release from the prop when it first appears. */
  burst?: number;
}

/**
 * Drive one 3D ASCII prop in `canvas`: it turns on its own, leans toward the cursor and takes
 * its light from it (a lantern), spins with scrolling, and starts if the cursor rushes at it.
 * Holds still when motion is off. `redraw` repaints now (say, when its text changes while
 * still); `dispose` cleans up.
 */
export function mountProp(canvas: HTMLCanvasElement, kind: PropKind, opts: PropOptions) {
  const ctx = canvas.getContext("2d");
  if (!ctx) return { redraw: () => {}, dispose: () => {} };
  const scene = makeScene(kind);
  const view = new View();
  const cells = new Cells();
  const frame: Frame = { t: 1.6, lookX: 0, lookY: 0, spin: 0, fright: 0 };
  const light: [number, number, number] = [...DEFAULT_LIGHT];
  let glyphs: Glyphs | null = null;
  let visible = true;
  let dead = false;
  let zoom = 1;
  let stopTicking = () => {};

  const setup = () => {
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    const { width, height } = canvas.getBoundingClientRect();
    if (!width || !height) return;
    canvas.width = Math.round(width * dpr);
    canvas.height = Math.round(height * dpr);
    glyphs = new Glyphs(FONT, dpr);
    cells.resize(Math.floor(canvas.width / glyphs.cw), Math.floor(canvas.height / glyphs.ch));
  };

  const draw = () => {
    if (!glyphs) return;
    frame.text = opts.text();
    cells.clear();
    view.render(scene, frame, cells, 0, 0, cells.cols, cells.rows, glyphs.cw, glyphs.ch, light, 1, zoom);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    cells.paint(ctx, glyphs, Math.floor((canvas.width - cells.cols * glyphs.cw) / 2), Math.floor((canvas.height - cells.rows * glyphs.ch) / 2));
  };

  const step = (t: number, dt: number) => {
    if (!visible || !glyphs) return;
    const r = canvas.getBoundingClientRect();
    const cx = r.left + r.width / 2, cy = r.top + r.height / 2;
    const k = 1 - Math.exp(-dt * 3.5);
    const tx = input.seen ? clamp((input.x - cx) / 520, -1, 1) : 0;
    const ty = input.seen ? clamp((cy - input.y) / 420, -1, 1) : 0;
    frame.lookX += (tx - frame.lookX) * k;
    frame.lookY += (ty - frame.lookY) * k;
    let L = lanternFrom(cx, cy, 300, DEFAULT_LIGHT);
    zoom = 1;
    if (kind === "moon" && idleFor() > 6) {
      // Left alone, the moon breathes: its phases wheel slowly round and it swells a little.
      const a = t * 0.4;
      L = [Math.cos(a), 0.25, Math.sin(a)];
      zoom = 1 + 0.025 * Math.sin(t * 1.3);
    }
    for (let i = 0; i < 3; i++) light[i] += (L[i] - light[i]) * k;
    const l = Math.hypot(...light) || 1;
    for (let i = 0; i < 3; i++) light[i] /= l;
    frame.fright = Math.max(rushAt(cx, cy), (frame.fright ?? 0) - dt * 0.9);
    frame.spin += dt * ((opts.busy() ? 5 : 0) + clamp(input.scrollV * 0.003, -4, 4));
    frame.t = t;
    draw();
  };

  // Motion can be switched on and off while we're mounted (the OS setting or the footer toggle).
  const apply = () => {
    stopTicking();
    stopTicking = motionOn() ? onTick(step) : () => {};
    if (!motionOn()) {
      frame.fright = 0;
      draw();
    }
  };

  stage(kind, 1);
  startInput();
  setup();
  draw();
  apply();
  const stopWatching = onMotionChange(apply);
  if (opts.burst && motionOn()) {
    // After the page has painted, so the backdrop is listening.
    requestAnimationFrame(() => {
      if (dead) return;
      const r = canvas.getBoundingClientRect();
      burstBats(r.left + r.width / 2, r.top + r.height / 2, opts.burst ?? 0);
    });
  }
  const ro = new ResizeObserver(() => {
    setup();
    draw();
  });
  ro.observe(canvas);
  const io = new IntersectionObserver(([e]) => (visible = e.isIntersecting));
  io.observe(canvas);
  onFontReady(FONT, () => {
    if (dead) return;
    setup();
    draw();
  });
  return {
    redraw: draw,
    dispose: () => {
      dead = true;
      stage(kind, -1);
      stopTicking();
      stopWatching();
      ro.disconnect();
      io.disconnect();
    },
  };
}
