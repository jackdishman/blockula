/**
 * The dynamic routes (app/{tx,account,…}/[param]) in the served wallet (`npm run build:wallet`).
 * A static export can't list every transaction or account: it emits one page per route under the
 * placeholder `_`, the node serves that page for any id (node/src/wallet.rs), and the page reads
 * the real one from the URL (`useRouteParam`, lib/route-param.ts). Other builds render on demand
 * and list nothing.
 */
export const PLACEHOLDER = "_";

/**
 * A dynamic route's `generateStaticParams` (`export const generateStaticParams =
 * servedStaticParams("id")` in its page.tsx): the placeholder page in the served build, and no
 * function at all elsewhere. Even one returning `[]` would make Next treat the route as static,
 * and the root layout's `connection()` (dynamic rendering, for proxy.ts's nonce) would then fail
 * every request to it.
 */
export function servedStaticParams<K extends string>(param: K): (() => Record<K, string>[]) | undefined {
  if (process.env.NEXT_PUBLIC_BLOCKULA_SERVED !== "1") return undefined;
  return () => [{ [param]: PLACEHOLDER } as Record<K, string>];
}

/**
 * The parameter in a browser path under `base` (`/wallet/tx/ab12/` → `ab12`), when that path is
 * of the same section as `route` (the app path, `/tx/…`). Every dynamic route is
 * `/<section>/<param>`. `null` for a path of another section (a page in transition).
 */
export function paramFromPath(url: string, route: string, base: string): string | null {
  const path = base && url.startsWith(`${base}/`) ? url.slice(base.length) : url;
  const parts = path.split("/").filter(Boolean);
  const section = route.split("/").filter(Boolean)[0];
  if (parts.length < 2 || (section && parts[0] !== section)) return null;
  return parts[1];
}
