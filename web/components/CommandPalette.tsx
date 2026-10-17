"use client";

import "@/app/explorer.css";
import { usePathname, useRouter } from "next/navigation";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { nodeGet } from "@/lib/explorer/node";
import { searchHref, to } from "@/lib/explorer/routes";
import type { NameDetail, SearchHit } from "@/lib/explorer/types";
import { useWallet } from "@/lib/wallet";

interface Item {
  id: string;
  group: string;
  label: string;
  hint?: string;
  href: string;
}

const RECENT = "blockula.recent";
const OPEN_EVENT = "blockula:palette";

/** Open the palette from anywhere (e.g. a "search" link in page copy). */
export const openPalette = () => window.dispatchEvent(new Event(OPEN_EVENT));

const readRecent = (): Item[] => {
  try {
    return JSON.parse(localStorage.getItem(RECENT) ?? "[]");
  } catch {
    return [];
  }
};
const remember = (i: Item) => {
  const list = [{ ...i, group: "Recent" }, ...readRecent().filter((r) => r.href !== i.href)].slice(0, 6);
  try {
    localStorage.setItem(RECENT, JSON.stringify(list));
  } catch {
    /* storage full or off */
  }
};

const typing = (e: KeyboardEvent) => {
  const t = e.target as HTMLElement | null;
  return !!t && (t.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(t.tagName));
};

/**
 * ⌘K (or `/`) anywhere: jump to any name, address, block or transaction, or do something with it.
 * Lookups go through the node's /api/search, so what you see is what the chain says.
 */
export function CommandPalette() {
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const [hits, setHits] = useState<Item[]>([]);
  const [busy, setBusy] = useState(false);
  const [sel, setSel] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const router = useRouter();
  const path = usePathname();
  const { hint } = useWallet();
  // Wallet pages unlock on arrival, so a known (even locked) wallet gets its actions here too.
  const known = !!hint?.account;
  const myPage = hint?.name ?? hint?.account;

  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setOpen((o) => !o);
      } else if (e.key === "/" && !typing(e)) {
        e.preventDefault();
        setOpen(true);
      }
    };
    const show = () => setOpen(true);
    window.addEventListener("keydown", key);
    window.addEventListener(OPEN_EVENT, show);
    return () => {
      window.removeEventListener("keydown", key);
      window.removeEventListener(OPEN_EVENT, show);
    };
  }, []);

  useEffect(() => setOpen(false), [path]);
  useEffect(() => {
    if (!open) return;
    setQ("");
    setHits([]);
    setSel(0);
    setTimeout(() => input.current?.focus(), 0);
  }, [open]);

  // Resolve what was typed against the chain.
  useEffect(() => {
    const raw = q.trim().replace(/^#/, "");
    if (!raw) return setHits([]);
    let live = true;
    setBusy(true);
    const t = setTimeout(async () => {
      const out: Item[] = [];
      try {
        const hit = await nodeGet<SearchHit>(`/search/${encodeURIComponent(raw.toLowerCase())}`);
        if (hit.type === "account") {
          out.push({ id: "acct", group: "On the chain", label: hit.id, hint: "account", href: searchHref(hit) });
          if (known && /\.boo$/.test(hit.id)) out.push({ id: "pay", group: "Do", label: `Send FANG to ${hit.id}`, hint: "send", href: `/send?to=${encodeURIComponent(hit.id)}` });
          out.push({ id: "trace", group: "Do", label: `Follow the money around ${/\.boo$/.test(hit.id) ? hit.id : `${hit.id.slice(0, 12)}…`}`, hint: "trace", href: to.trace(hit.id) });
        } else if (hit.type === "name") {
          const n = await nodeGet<NameDetail>(`/name/${encodeURIComponent(hit.id)}`).catch(() => null);
          out.push({ id: "name", group: "On the chain", label: n?.name ?? `${hit.id}.boo`, hint: n?.available ? `unclaimed · ${n.price_display ?? ""}` : "name", href: to.name(hit.id) });
        } else {
          out.push({ id: hit.type, group: "On the chain", label: hit.type === "block" ? `Block #${hit.id}` : `Transaction ${hit.id.slice(0, 12)}…`, hint: hit.type, href: searchHref(hit) });
        }
      } catch {
        if (/^[0-9a-f]{64}$/.test(raw.toLowerCase())) out.push({ id: "key", group: "On the chain", label: `Not a transaction this node knows: ${raw.slice(0, 12)}…`, hint: "tx", href: to.tx(raw.toLowerCase()) });
      }
      if (live) {
        setHits(out);
        setBusy(false);
      }
    }, 180);
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [q, known]);

  const places = useMemo<Item[]>(() => {
    const go: Item[] = [
      ...(hint?.account ? [{ id: "home", group: "Wallet", label: "Wallet home", href: "/" }] : [{ id: "home", group: "Wallet", label: "Open the wallet", href: "/" }]),
      ...(known
        ? [
            { id: "send", group: "Wallet", label: "Send FANG", href: "/send" },
            { id: "recv", group: "Wallet", label: "Receive", href: "/receive" },
            { id: "dev", group: "Wallet", label: "Devices & agents", href: "/devices" },
          ]
        : []),
      ...(myPage
        ? [
            { id: "me", group: "Wallet", label: "My account, as the chain sees it", href: to.account(myPage) },
            { id: "mytrace", group: "Wallet", label: "Follow my money", href: to.trace(myPage) },
          ]
        : []),
      { id: "pulse", group: "Explore", label: "The pulse: live blocks", href: to.explore() },
      { id: "programs", group: "Explore", label: "Programs", href: to.programs() },
      { id: "hunts", group: "Explore", label: "Hunts (bounties)", href: to.program("hunts") },
      { id: "pool", group: "Explore", label: "Contribution pool", href: to.program("pool") },
      { id: "vals", group: "Explore", label: "Validators", href: to.program("validators") },
      { id: "names", group: "Explore", label: "Names", href: to.program("names") },
    ];
    const needle = q.trim().toLowerCase();
    return needle ? go.filter((i) => i.label.toLowerCase().includes(needle)) : go;
  }, [q, hint, known, myPage]);

  const items = useMemo(() => [...hits, ...(q.trim() ? [] : readRecent()), ...places], [hits, places, q]);
  useEffect(() => setSel((s) => Math.min(s, Math.max(0, items.length - 1))), [items.length]);

  const go = useCallback(
    (i: Item) => {
      remember(i);
      setOpen(false);
      router.push(i.href);
    },
    [router],
  );

  if (!open) return null;
  let group = "";
  return (
    <div className="palette-veil" onMouseDown={(e) => e.target === e.currentTarget && setOpen(false)}>
      <div className="palette" role="dialog" aria-modal="true" aria-label="Search and commands">
        <input
          ref={input}
          value={q}
          onChange={(e) => {
            setQ(e.target.value);
            setSel(0);
          }}
          onKeyDown={(e) => {
            if (e.key === "Escape") setOpen(false);
            else if (e.key === "ArrowDown") {
              e.preventDefault();
              setSel((s) => Math.min(items.length - 1, s + 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setSel((s) => Math.max(0, s - 1));
            } else if (e.key === "Enter" && items[sel]) {
              e.preventDefault();
              go(items[sel]);
            }
          }}
          placeholder="A name, address, block or transaction…"
          aria-label="Search"
          aria-controls="palette-list"
          aria-activedescendant={items[sel] ? `pi-${sel}` : undefined}
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
        />
        <ul className="palette-list" id="palette-list" role="listbox">
          {busy && !hits.length && q.trim() && <li className="palette-group">Asking the chain…</li>}
          {items.map((i, n) => {
            const head = i.group !== group ? (group = i.group) : null;
            return (
              <li key={`${i.group}-${i.id}-${i.href}`} role="presentation">
                {head && <div className="palette-group">{head}</div>}
                <button id={`pi-${n}`} role="option" aria-selected={n === sel} className="palette-item" onMouseMove={() => setSel(n)} onClick={() => go(i)}>
                  <span>{i.label}</span>
                  {i.hint && <span className="hint">{i.hint}</span>}
                </button>
              </li>
            );
          })}
          {!items.length && !busy && <li className="palette-group">Nothing on the chain matches.</li>}
        </ul>
        <div className="palette-foot">
          <span><span className="kbd">↑↓</span> choose</span>
          <span><span className="kbd">↵</span> open</span>
          <span><span className="kbd">esc</span> close</span>
        </div>
      </div>
    </div>
  );
}
