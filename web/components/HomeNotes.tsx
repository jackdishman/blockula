"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import "@/app/onboarding.css";
import type { NameDetail } from "@/lib/explorer/types";
import { aboutWhen, blocksToHuman, fangParts } from "@/lib/format";
import { breakdown, NAME_WARN_DAYS, nameAlert, type BalanceParts, type NameAlert, type StakeTotals } from "@/lib/onboarding";
import { SAFETY } from "@/lib/onboarding-copy";
import { KIT_LABEL } from "@/lib/recovery";
import type { AccountView } from "@/lib/types";
import { useUnlocked } from "@/lib/wallet";

/** FANG as Home's balance shows it: grouped, at most four decimals. */
const fang = (drops: bigint) => {
  const { whole, frac } = fangParts(drops);
  return `${whole}${frac ? `.${frac}` : ""} FANG`;
};

/**
 * Under Home's balance: what can be sent now, what's staked, what's locked (and until about
 * when), from `/api/account` (`transferable`, `locked`, `locks`) and `/api/staking`. Shown only
 * when the balance isn't simply all spendable.
 */
export function BalanceBreakdown({ account }: { account: AccountView }) {
  const { client, status } = useUnlocked();
  const [stake, setStake] = useState<StakeTotals | null>(null);
  const addr = account.account.address;
  // Re-read the stake when something happened on the account, and every 30 blocks or so (rewards), not on every poll.
  const latest = `${account.recent_txs[0]?.id ?? ""}/${account.balance}/${status ? Math.floor(status.height / 30) : ""}`;
  useEffect(() => {
    let live = true;
    client
      .staking(addr)
      .then((s: StakeTotals) => live && setStake(s))
      // A chain without staking (404) or a node that's away: nothing staked to show.
      .catch(() => live && setStake(null));
    return () => {
      live = false;
    };
  }, [client, addr, latest]);
  const b = breakdown(account as AccountView & BalanceParts, stake);
  if (!b.show) return null;
  return (
    <p className="onb-parts" data-testid="balance-parts">
      <span className="onb-part">
        Can send now <b>{fang(b.canSend)}</b>
      </span>
      {b.staked > 0n && (
        <span className="onb-part">
          · <Link href="/stake">Staked</Link> <b>{fang(b.staked)}</b>
        </span>
      )}
      {b.unstaking > 0n && (
        <span className="onb-part">
          · Unstaking <b>{fang(b.unstaking)}</b>
        </span>
      )}
      {b.locked > 0n && (
        <span className="onb-part">
          · Locked <b>{fang(b.locked)}</b>
          {/* As Send and Governance say when a lock ends ("until about 21:15", "until about Oct 28"). */}
          {b.lockedUntil !== null && status ? ` until ${aboutWhen(b.lockedUntil - status.height, status.block_time_ms)}` : ""}
        </span>
      )}
    </p>
  );
}

const ONE_KEY_DISMISSED = "blockula.onb.oneKey";

/**
 * Only one key opens this wallet (a passkey, or on test chains a browser-only key), there's no
 * recovery kit, and there's FANG in it: say so until the user makes a kit or links a device (or
 * hides it for this browser session).
 */
export function OnePasskeyBanner({ account }: { account: AccountView }) {
  const { hint } = useUnlocked();
  const addr = account.account.address;
  const [hidden, setHidden] = useState<string | null>(null);
  useEffect(() => {
    try {
      setHidden(sessionStorage.getItem(ONE_KEY_DISMISSED));
    } catch {
      /* storage unavailable */
    }
  }, []);
  const owners = account.keys.filter((k) => k.role.type === "owner");
  const devices = owners.filter((k) => k.label !== KIT_LABEL).length;
  const kits = owners.length - devices;
  // A kit waiting for its fee has its own banner on Home.
  const pendingKit = !!hint.pendingKit && !account.keys.some((k) => k.key_id === hint.pendingKit?.keyId);
  if (hidden === addr || devices !== 1 || kits > 0 || pendingKit || BigInt(account.balance) <= 0n) return null;
  return (
    <div className="banner onb-banner" data-testid="one-passkey">
      <b>{hint.kind === "local" ? SAFETY.oneBrowserKey : SAFETY.onePasskey}</b> {SAFETY.onePasskeyLost}{" "}
      <Link href="/devices#recovery">Make a recovery kit</Link> or <Link href="/devices#device">link a second device</Link>.
      <button
        className="link"
        onClick={() => {
          try {
            sessionStorage.setItem(ONE_KEY_DISMISSED, addr);
          } catch {
            /* storage unavailable */
          }
          setHidden(addr);
        }}
      >
        Not now
      </button>
    </div>
  );
}

/** Names worth a look per account: the first few it owns (most accounts have one). */
const NAMES_CHECKED = 5;

/**
 * A name this account owns expires within NAME_WARN_DAYS, or already has and is in its grace
 * period (only its owner can renew it then; after that anyone can register it, and every old
 * payment link to it pays them).
 */
export function NameExpiryBanner({ account }: { account: AccountView }) {
  const { client, status } = useUnlocked();
  const [alerts, setAlerts] = useState<NameAlert[]>([]);
  const me = account.account.address;
  const names = account.names.slice(0, NAMES_CHECKED).join(",");
  const bt = status?.block_time_ms ?? 1000;
  const height = useRef(0);
  height.current = status?.height ?? 0;
  // Re-read about every ten minutes of blocks, and whenever the names change.
  const tick = status ? Math.floor(status.height / 600) : null;
  useEffect(() => {
    if (!names || tick === null) return setAlerts([]);
    let live = true;
    const warnBlocks = Math.round((NAME_WARN_DAYS * 86_400_000) / bt);
    Promise.all(names.split(",").map((n) => (client.name(n) as Promise<NameDetail>).catch(() => null))).then((infos) => {
      if (!live) return;
      setAlerts(infos.flatMap((i) => (i ? [nameAlert(i, me, height.current, warnBlocks)] : [])).filter((a): a is NameAlert => a !== null));
    });
    return () => {
      live = false;
    };
  }, [client, me, names, tick, bt]);
  if (!alerts.length) return null;
  return (
    <>
      {alerts.map((a) => {
        const renew = `/programs/names?do=renew_name&name=${encodeURIComponent(a.name.replace(/\.boo$/, ""))}`;
        const left = blocksToHuman(a.blocksLeft, bt).replace(/^day$/, "a day");
        return (
          <div key={a.name} className={`banner onb-banner${a.state === "grace" ? " danger" : ""}`} data-testid="name-expiry">
            {a.state === "grace" ? (
              <>
                <b>{a.name} has expired.</b> Only you can renew it, for about {left} more; after that anyone can register it, and payments to it go to them.{" "}
              </>
            ) : (
              <>
                <b>{a.name} expires in about {left}.</b> Renew it to keep it{a.renewal ? ` (${a.renewal} a year)` : ""}.{" "}
              </>
            )}
            <Link href={renew}>Renew {a.name}</Link>
          </div>
        );
      })}
    </>
  );
}
