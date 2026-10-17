import type { PropKind } from "./scenes";

/**
 * What's on stage: the props pages are showing (so the backdrop can stand its own copy down),
 * and events pages send to the backdrop (a burst of bats when you send FANG).
 */

export const onStage = new Map<PropKind, number>();
const stageWatchers = new Set<() => void>();

export function stage(kind: PropKind, delta: number) {
  onStage.set(kind, (onStage.get(kind) ?? 0) + delta);
  // Let watchers see the whole commit's mounts and unmounts at once.
  queueMicrotask(() => stageWatchers.forEach((fn) => fn()));
}

/** Call `fn` when props come or go. Returns an unsubscribe function. */
export function onStageChange(fn: () => void) {
  stageWatchers.add(fn);
  return () => void stageWatchers.delete(fn);
}

type BatBurst = (x: number, y: number, n: number) => void;
const bursts = new Set<BatBurst>();

/** Send `n` bats flying out from (x, y), viewport CSS px. Ignored if nothing is listening. */
export function burstBats(x: number, y: number, n: number) {
  bursts.forEach((fn) => fn(x, y, n));
}

export function onBatBurst(fn: BatBurst) {
  bursts.add(fn);
  return () => void bursts.delete(fn);
}
