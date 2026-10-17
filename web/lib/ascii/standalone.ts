/**
 * The haunted backdrop for pages outside the wallet, namely the node's built-in explorer.
 * `npm run build:haunt` bundles this into node/src/haunt.js, which the node serves at
 * /haunt.js. It adds its own canvas, keeps to the margins beside a content column of
 * `<body data-haunt-column="…">` px, and wires up a `[data-motion-toggle]` button if the page
 * has one.
 */
import { mountHaunt } from "./haunt";
import { motionOn, onMotionChange, setUserStill, systemStill } from "./settings";

function start() {
  const canvas = document.createElement("canvas");
  canvas.className = "haunt";
  canvas.setAttribute("aria-hidden", "true");
  document.body.prepend(canvas);
  mountHaunt(canvas, { column: Number(document.body.dataset.hauntColumn) || undefined });

  const toggle = document.querySelector<HTMLButtonElement>("[data-motion-toggle]");
  if (!toggle) return;
  const show = () => {
    const system = systemStill();
    toggle.disabled = system;
    toggle.textContent = system ? "Animations off (your system asks for reduced motion)" : motionOn() ? "Turn animations off" : "Turn animations on";
  };
  toggle.addEventListener("click", () => setUserStill(motionOn()));
  onMotionChange(show);
  show();
  toggle.hidden = false;
}

if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
else start();
