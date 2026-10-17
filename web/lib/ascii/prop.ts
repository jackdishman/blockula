import { Cells, clamp, type Frame, Glyphs, input, lanternFrom, onFontReady, onTick, reducedMotion, View } from "./engine";
import { makeScene, type PropKind } from "./scenes";

const FONT = 9;

/** Props currently on screen, by kind, so the backdrop can stand its own copy down. */
export const onStage = new Map<PropKind, number>();
const stageWatchers = new Set<() => void>();
const stage = (kind: PropKind, delta: number) => {
  onStage.set(kind, (onStage.get(kind) ?? 0) + delta);
  // Let watchers see the whole commit's mounts and unmounts at once.
  queueMicrotask(() => stageWatchers.forEach((fn) => fn()));
};
/** Call `fn` when props come or go. Returns an unsubscribe function. */
export function onStageChange(fn: () => void) {
  stageWatchers.add(fn);
  return () => void stageWatchers.delete(fn);
}
const DEFAULT_LIGHT: [number, number, number] = [-0.45, 0.55, 0.7];

/**
 * Drive one 3D ASCII prop in `canvas`: it turns on its own, leans toward the cursor, takes its
 * light from the cursor (a lantern), and spins with scrolling or while `busy()` is true.
 * Static under prefers-reduced-motion. Returns a cleanup function.
 */
export function mountProp(canvas: HTMLCanvasElement, kind: PropKind, busy: () => boolean) {
  const ctx = canvas.getContext("2d");
  if (!ctx) return () => {};
  const scene = makeScene(kind);
  const view = new View();
  const cells = new Cells();
  const frame: Frame = { t: 1.6, lookX: 0, lookY: 0, spin: 0 };
  const light: [number, number, number] = [...DEFAULT_LIGHT];
  let glyphs: Glyphs | null = null;
  let visible = true;
  let dead = false;

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
    cells.clear();
    view.render(scene, frame, cells, 0, 0, cells.cols, cells.rows, glyphs.cw, glyphs.ch, light);
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
    const L = lanternFrom(cx, cy, 300, DEFAULT_LIGHT);
    for (let i = 0; i < 3; i++) light[i] += (L[i] - light[i]) * k;
    const l = Math.hypot(...light) || 1;
    for (let i = 0; i < 3; i++) light[i] /= l;
    frame.spin += dt * ((busy() ? 5 : 0) + clamp(input.scrollV * 0.003, -4, 4));
    frame.t = t;
    draw();
  };

  stage(kind, 1);
  setup();
  draw();
  const still = reducedMotion();
  const stop = still ? () => {} : onTick(step);
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
  return () => {
    dead = true;
    stage(kind, -1);
    stop();
    ro.disconnect();
    io.disconnect();
  };
}
