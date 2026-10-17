/**
 * Visitor-facing switches for the haunted ASCII, shared by the wallet and the explorer:
 * whether anything moves (the OS's reduced-motion setting, or our own footer toggle), and
 * which season it is (the full haunt in October, just fog and the moon the rest of the year).
 */

const MOTION_KEY = "blockula.motion";
const SEASON_KEY = "blockula.season";

export type Season = "halloween" | "quiet";

const watchers = new Set<() => void>();
let systemReduced = false;
let started = false;

function start() {
  if (started || typeof window === "undefined") return;
  started = true;
  const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
  systemReduced = mq.matches;
  mq.addEventListener?.("change", () => {
    systemReduced = mq.matches;
    watchers.forEach((fn) => fn());
  });
  // Another tab flipped the toggle.
  window.addEventListener("storage", (e) => e.key === MOTION_KEY && watchers.forEach((fn) => fn()));
}

const read = (store: () => Storage, key: string) => {
  try {
    return store().getItem(key);
  } catch {
    return null;
  }
};

/** Is the OS asking for reduced motion? Then the toggle can't turn animation on. */
export function systemStill() {
  start();
  return systemReduced;
}

/** Has the visitor switched animation off with our toggle? */
export function userStill() {
  return read(() => localStorage, MOTION_KEY) === "off";
}

/** Should the haunt animate at all? */
export function motionOn() {
  start();
  return !systemReduced && !userStill();
}

export function setUserStill(still: boolean) {
  try {
    if (still) localStorage.setItem(MOTION_KEY, "off");
    else localStorage.removeItem(MOTION_KEY);
  } catch {
    /* storage unavailable: the switch lasts until reload */
  }
  watchers.forEach((fn) => fn());
}

/** Call `fn` when motionOn() may have changed. Returns an unsubscribe function. */
export function onMotionChange(fn: () => void) {
  start();
  watchers.add(fn);
  return () => void watchers.delete(fn);
}

/**
 * October 1 to November 2 (through the Day of the Dead) is the full haunt. `?season=halloween`
 * or `?season=quiet` overrides it for this tab, for previewing.
 */
export function season(now = new Date()): Season {
  if (typeof window !== "undefined") {
    const q = new URLSearchParams(window.location.search).get("season");
    if (q === "halloween" || q === "quiet") {
      try {
        sessionStorage.setItem(SEASON_KEY, q);
      } catch {
        /* ignore */
      }
      return q;
    }
    const kept = read(() => sessionStorage, SEASON_KEY);
    if (kept === "halloween" || kept === "quiet") return kept;
  }
  const m = now.getMonth(), d = now.getDate();
  return m === 9 || (m === 10 && d <= 2) ? "halloween" : "quiet";
}
