"use client";

import { useId, useState } from "react";
import { nodeAddress, walletOf } from "@/lib/site";

/**
 * "Connect to any chain": paste the node address a friend sent, open that node's own wallet.
 * The site never talks to it (its CSP allows the demo node only): the visitor's browser goes there.
 */
export function ConnectSection() {
  const id = useId();
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const parsed = value.trim() ? nodeAddress(value) : null;
  return (
    <section className="site-section" id="connect" aria-labelledby="connect-title">
      <div className="card site-connect stack">
        <div>
          <span className="eyebrow">Got a link from a friend?</span>
          <h2 id="connect-title" className="site-h2">Connect to any chain</h2>
          <p className="dim">Every Blockula node serves its own wallet. Paste the address your friend sent you, and you&apos;re in.</p>
        </div>
        <form
          className="site-connect-form"
          noValidate
          onSubmit={(e) => {
            e.preventDefault();
            const a = nodeAddress(value);
            if (a.url === undefined) return setError(a.error);
            setError(null);
            window.location.assign(walletOf(a.url));
          }}
        >
          <label className="field grow" htmlFor={id}>
            <span>Node address</span>
            <input
              id={id}
              className="mono"
              type="url"
              inputMode="url"
              autoComplete="off"
              autoCapitalize="none"
              spellCheck={false}
              placeholder="https://chain.example"
              value={value}
              aria-invalid={!!error}
              aria-describedby={`${id}-note`}
              data-testid="connect-input"
              onChange={(e) => {
                setValue(e.target.value);
                setError(null);
              }}
            />
          </label>
          <button type="submit" className="btn primary" data-testid="connect-open">Open wallet</button>
        </form>
        <p id={`${id}-note`} className="small site-connect-note" aria-live="polite" data-testid="connect-note">
          {error ? (
            <span className="error" role="alert">{error}</span>
          ) : parsed?.url ? (
            <span className="dim">Opens <code>{walletOf(parsed.url)}</code></span>
          ) : (
            <span className="dim">Like <code>https://jack.tail1234.ts.net</code>. Plain <code>http://</code> works only on this computer or a private network.</span>
          )}
        </p>
        <p className="dim small">A node decides what its chain looks like to you, so open ones from people you know.</p>
      </div>
    </section>
  );
}
