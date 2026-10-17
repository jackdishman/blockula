"use client";

import Link from "next/link";
import { useState, type ReactNode } from "react";
import { markTutorial } from "@/lib/session";

export function Copy({ text, label = "Copy" }: { text: string; label?: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      className="btn small ghost"
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
        } catch {
          const ta = document.createElement("textarea");
          ta.value = text;
          document.body.appendChild(ta);
          ta.select();
          document.execCommand("copy");
          ta.remove();
        }
        setDone(true);
        setTimeout(() => setDone(false), 1500);
      }}
    >
      {done ? "Copied ✓" : label}
    </button>
  );
}

export function ErrorNote({ error }: { error: string | null }) {
  return error ? <p className="error" role="alert">{error}</p> : null;
}

export function Spinner({ children }: { children?: ReactNode }) {
  return (
    <span className="spinner-row">
      <span className="spinner" aria-hidden /> {children}
    </span>
  );
}

/** Link to an explorer page (in this app). Ticks the "view it in the explorer" tutorial item. */
export function ExplorerLink({ href, children, className }: { href: string; children: ReactNode; className?: string }) {
  return (
    <Link href={href} className={className} onClick={() => markTutorial({ viewedExplorer: true })}>
      {children}
    </Link>
  );
}

export function Tip({ children }: { children: ReactNode }) {
  return (
    <p className="tip">
      <span className="nb">N.B.</span> {children}
    </p>
  );
}
