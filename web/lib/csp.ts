// Security headers for every response (WAL-3). The wallet derives keys in page JS, so whoever
// can run script on this origin can take them: the CSP admits only this origin's nonce'd scripts
// and lets the page talk only to the nodes it knows.
import { NETWORKS } from "./config";

/** Origins the page may fetch from: every configured node, and a local one on 127.0.0.1 too. */
export function nodeOrigins(): string[] {
  const out = new Set<string>(["http://localhost:7070", "http://127.0.0.1:7070"]);
  for (const n of Object.values(NETWORKS)) if (n) for (const u of [n.rpc, ...(n.witnesses ?? [])]) out.add(new URL(u).origin);
  return [...out];
}

/** A custom node's origin from the `blockula_rpc` cookie, if it's a plain http(s) origin. */
export function customOrigin(cookie: string | undefined): string | null {
  if (!cookie) return null;
  try {
    const u = new URL(decodeURIComponent(cookie));
    // Only scheme://host[:port], and nothing that could smuggle another CSP token in.
    if (!/^https?:$/.test(u.protocol) || u.origin !== decodeURIComponent(cookie) || !/^https?:\/\/[a-z0-9.\-[\]:]+$/i.test(u.origin)) return null;
    return u.origin;
  } catch {
    return null;
  }
}

/** The Content-Security-Policy for one response. `dev` relaxes what `next dev` needs (eval, HMR styles). */
export function contentSecurityPolicy(nonce: string, opts: { custom?: string | null; dev?: boolean } = {}) {
  const connect = ["'self'", ...nodeOrigins(), ...(opts.custom ? [opts.custom] : [])];
  return [
    "default-src 'self'",
    `script-src 'self' 'nonce-${nonce}' 'strict-dynamic'${opts.dev ? " 'unsafe-eval'" : ""}`,
    opts.dev ? "style-src 'self' 'unsafe-inline'" : `style-src 'self' 'nonce-${nonce}'`,
    "font-src 'self'",
    "img-src 'self' data:",
    `connect-src ${[...new Set(connect)].join(" ")}`,
    "frame-ancestors 'none'",
    "frame-src 'none'",
    "base-uri 'none'",
    "form-action 'self'",
    "object-src 'none'",
  ].join("; ");
}

/** Headers that don't change per request, set on every route (next.config.ts). */
export const STATIC_HEADERS: { key: string; value: string }[] = [
  { key: "Strict-Transport-Security", value: "max-age=63072000; includeSubDomains" },
  { key: "X-Content-Type-Options", value: "nosniff" },
  { key: "Referrer-Policy", value: "no-referrer" },
  {
    key: "Permissions-Policy",
    value: "publickey-credentials-get=(self), publickey-credentials-create=(self), camera=(), microphone=(), geolocation=(), payment=(), usb=()",
  },
  { key: "Cross-Origin-Opener-Policy", value: "same-origin" },
  { key: "Cross-Origin-Resource-Policy", value: "same-origin" },
  { key: "X-Frame-Options", value: "DENY" },
];
