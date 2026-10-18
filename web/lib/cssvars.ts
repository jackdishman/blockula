/**
 * A ref that sets CSS custom properties (`--w: 40%`) through the CSSOM once the element mounts.
 * The wallet's production CSP (lib/csp.ts: `style-src 'self' 'nonce-…'`) blocks `style`
 * attributes in server-rendered HTML; CSSOM writes from script aren't style attributes, so
 * sizes and positions computed at runtime go through here, and stylesheets read the variables.
 */
export function cssVars(vars: Record<`--${string}`, string | number | null | undefined>) {
  return (el: HTMLElement | SVGElement | null) => {
    if (!el) return;
    for (const [k, v] of Object.entries(vars)) {
      if (v == null) el.style.removeProperty(k);
      else el.style.setProperty(k, typeof v === "number" ? `${v}px` : v);
    }
  };
}
