"use client";

import type { AccountChoice } from "@/lib/keys";

export function AccountPicker({ choices, onPick }: { choices: AccountChoice[]; onPick: (c: AccountChoice) => void }) {
  return (
    <div className="stack">
      <h2>Which account?</h2>
      <p className="dim">This passkey is allowed to act for more than one account. Pick the one to open.</p>
      {choices.map((c) => (
        <button key={c.address} className="choice" onClick={() => onPick(c)}>
          <span>
            <b>{c.name ?? c.short}</b>
            <span className="dim block">{c.role === "owner" ? "Owner" : "Limited (agent)"} · {c.label}</span>
          </span>
          <span className="dim">{c.balance}</span>
        </button>
      ))}
    </div>
  );
}
