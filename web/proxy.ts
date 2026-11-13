import { NextResponse, type NextRequest } from "next/server";
import { RPC_COOKIE, SITE } from "./lib/config";
import { contentSecurityPolicy, customOrigin } from "./lib/csp";

/** The project site's pages: the landing page, its legal pages, and how to link an X account. */
const SITE_PATHS = new Set(["/", "/terms", "/privacy", "/x", "/x/"]);

/** `/boo/vlad` or `/boo/vlad.boo`: a name's label is 3 to 32 of a-z, 0-9 and `-`, not at either end. */
const BOO_PATH = /^\/boo\/([a-z0-9][a-z0-9-]{1,30}[a-z0-9])(?:\.boo)?\/?$/i;

/**
 * A fresh CSP nonce per request. Next.js reads it from the request's CSP header and puts it on
 * every script it renders; the same policy goes out on the response.
 */
export function proxy(request: NextRequest) {
  // /boo/<name>: an account's page by its name, the short link the X proof post carries. A node
  // answers it the same way for the wallet it serves (node/src/wallet.rs). Names are per chain,
  // so the project site has none.
  const boo = SITE ? null : request.nextUrl.pathname.match(BOO_PATH);
  if (boo) return NextResponse.redirect(new URL(`/account/${boo[1].toLowerCase()}.boo`, request.url), 307);
  // The project site is the landing page, its legal pages, and /x. The wallet's other routes
  // aren't for this origin, so they go home.
  if (SITE && !SITE_PATHS.has(request.nextUrl.pathname)) return NextResponse.redirect(new URL("/", request.url), 307);
  const nonce = btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(18))));
  const csp = contentSecurityPolicy(nonce, {
    custom: SITE ? null : customOrigin(request.cookies.get(RPC_COOKIE)?.value),
    dev: process.env.NODE_ENV === "development",
  });
  const headers = new Headers(request.headers);
  headers.set("content-security-policy", csp);
  const response = NextResponse.next({ request: { headers } });
  response.headers.set("content-security-policy", csp);
  return response;
}

export const config = {
  matcher: [
    {
      // Pages only: static assets don't run in a document, and prefetches carry no scripts.
      source: "/((?!_next/static|_next/image|favicon.ico|icon.svg).*)",
      missing: [
        { type: "header", key: "next-router-prefetch" },
        { type: "header", key: "purpose", value: "prefetch" },
      ],
    },
  ],
};
