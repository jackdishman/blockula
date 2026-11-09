"use client";

import { useEffect, useState } from "react";
import { passkeyProblem } from "@/lib/keys";

/** Why passkeys won't work on this page (lib/keys.ts `passkeyProblem`), once mounted; null if they will. */
export function usePasskeyProblem() {
  const [problem, setProblem] = useState<ReturnType<typeof passkeyProblem>>(null);
  useEffect(() => setProblem(passkeyProblem()), []);
  return problem;
}

/**
 * Said plainly instead of a passkey button that can only fail: why this page can't make or open
 * a passkey, what works here (a browser-only key, on a test chain), and how its node's owner
 * makes passkeys work (HTTPS, e.g. `tailscale serve`).
 */
export function PasskeyNotice({ problem, onLocalKey }: { problem: NonNullable<ReturnType<typeof passkeyProblem>>; onLocalKey?: () => void }) {
  const host = window.location.host;
  const port = window.location.port || "7070";
  return (
    <section className="card stack warn-card" data-testid="passkey-notice">
      <h2>{problem === "unsupported" ? "This browser can't use passkeys" : "Passkeys need HTTPS"}</h2>
      <p>
        {problem === "insecure" && (
          <>
            This page came over plain <code>http://</code> from <code>{host}</code>. Browsers make and open passkeys only on secure pages: <code>https://</code>, or <code>localhost</code> on the machine itself.
          </>
        )}
        {problem === "ip" && (
          <>
            Browsers make passkeys only for a site&apos;s name, and <code>{host}</code> is an IP address.
          </>
        )}
        {problem === "unsupported" && <>It has no WebAuthn. A recent Safari, Chrome, Edge or Firefox does.</>}
      </p>
      {problem !== "unsupported" && (
        <p className="dim">
          <b>For a passkey,</b> open the wallet at its HTTPS address. Whoever runs this node can give it one: on Tailscale, <code>tailscale serve --bg {port}</code> serves it at <code>https://&lt;machine&gt;.&lt;tailnet&gt;.ts.net/wallet/</code> (<code>blockula create</code> offers this: Tailscale, then HTTPS).
        </p>
      )}
      {onLocalKey && (
        <>
          <p className="dim">
            <b>To try it now,</b> a browser-only key works here: a random secret kept in this browser&apos;s storage. Clearing site data deletes it, and anyone with this browser profile could read it. Fine for a test chain.
          </p>
          <button className="btn" onClick={onLocalKey}>Use a browser-only key (for testing)</button>
        </>
      )}
    </section>
  );
}
