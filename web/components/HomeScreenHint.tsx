"use client";

import "@/app/onboarding.css";
import { useEffect, useState } from "react";

const DISMISSED = "blockula.homeScreenHint.dismissed";

/**
 * Safari on an iPhone or iPad, in a tab (not opened from the home screen). iPadOS asks for
 * desktop sites and says "Macintosh", but has a touch screen. Chrome, Firefox and Edge on iOS
 * name themselves (CriOS, FxiOS, EdgiOS); their share sheets add to the home screen differently.
 */
export function iosSafariTab(ua: string, touchPoints: number, standalone: boolean | undefined): boolean {
  const ios = /iPhone|iPad|iPod/.test(ua) || (/Macintosh/.test(ua) && touchPoints > 1);
  return ios && /Safari\//.test(ua) && !/CriOS|FxiOS|EdgiOS|OPiOS|GSA\//.test(ua) && standalone !== true;
}

/**
 * The wallet installs like an app (app/manifest.ts, app/apple-icon.png), but iOS never offers it
 * by itself: say how, once, on iOS Safari only. Dismissing it is remembered on this device.
 */
export function HomeScreenHint() {
  const [show, setShow] = useState(false);
  useEffect(() => {
    let dismissed = false;
    try {
      dismissed = localStorage.getItem(DISMISSED) === "1";
    } catch {
      /* storage unavailable: show it, and forget the dismissal */
    }
    const nav = navigator as Navigator & { standalone?: boolean };
    setShow(!dismissed && iosSafariTab(nav.userAgent, nav.maxTouchPoints ?? 0, nav.standalone));
  }, []);
  if (!show) return null;
  return (
    <div className="banner onb-banner" role="note" data-testid="home-screen-hint">
      <b>Add Blockula to your Home Screen:</b> tap Share, then Add to Home Screen. It opens like an app.
      <button
        type="button"
        className="link"
        onClick={() => {
          try {
            localStorage.setItem(DISMISSED, "1");
          } catch {
            /* storage unavailable */
          }
          setShow(false);
        }}
      >
        Got it
      </button>
    </div>
  );
}
