"use client";

import { useParams, usePathname } from "next/navigation";
import { useSyncExternalStore } from "react";
import { BASE_PATH, SERVED } from "./config";
import { paramFromPath, PLACEHOLDER } from "./route-params";

const subscribe = (onChange: () => void) => {
  window.addEventListener("popstate", onChange);
  return () => window.removeEventListener("popstate", onChange);
};
const location = () => window.location.pathname;
const noLocation = () => null;

/**
 * A dynamic route's parameter (`app/tx/[id]` → `useRouteParam("id")`), raw as in the URL (still
 * percent-encoded, like `useParams`). Use it instead of `useParams` in every dynamic page.
 *
 * In the served wallet every id gets the placeholder's page (lib/route-params.ts), so Next's own
 * params may say `_`; the real value is the URL's segment at the same depth. Elsewhere it's
 * `useParams`.
 */
export function useRouteParam(name: string): string {
  const params = useParams<Record<string, string | string[]>>();
  // Re-renders on client navigations; the browser's URL is the one that counts.
  const pathname = usePathname();
  const url = useSyncExternalStore(subscribe, location, noLocation);
  const raw = params?.[name];
  const value = Array.isArray(raw) ? raw.join("/") : (raw ?? "");
  if (!SERVED || value !== PLACEHOLDER) return value;
  // The router's path first: during a client navigation it's the new page's before the
  // browser's URL changes. On a first load it may still be the placeholder's.
  const routed = paramFromPath(pathname, pathname, "");
  if (routed && routed !== PLACEHOLDER) return routed;
  return (url && paramFromPath(url, pathname, BASE_PATH)) || value;
}
