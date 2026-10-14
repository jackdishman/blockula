import { NextResponse, type NextRequest } from "next/server";
import { RPC_COOKIE } from "./lib/config";
import { contentSecurityPolicy, customOrigin } from "./lib/csp";

/**
 * A fresh CSP nonce per request. Next.js reads it from the request's CSP header and puts it on
 * every script it renders; the same policy goes out on the response.
 */
export function proxy(request: NextRequest) {
  const nonce = btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(18))));
  const csp = contentSecurityPolicy(nonce, {
    custom: customOrigin(request.cookies.get(RPC_COOKIE)?.value),
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
