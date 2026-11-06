"use client";

import { cssVars } from "@/lib/cssvars";
import { blocksToHuman, displayFang } from "@/lib/format";

/** What the landing page reads from `/api/params` (node/src/api.rs `params`). Every field may be missing. */
export interface ParamsView {
  params?: Record<string, string | undefined>;
  genesis?: { block_time_ms?: number; epoch_blocks?: number };
  issuance?: { rate_ppm?: number; per_epoch?: string };
}

/** What it reads from `/api/fees`: the account creation fee, the tip cap, and `fuel` only when programs are on. */
export interface FeesView {
  account_creation_fee?: string;
  tip_cap_multiple?: number;
  fuel?: { status?: string };
}

const pct = (bps: number) => `${(bps / 100).toLocaleString("en-US", { maximumFractionDigits: 2 })}%`;
const num = (v: string | number | undefined) => (v != null && /^\d+$/.test(String(v)) ? Number(v) : undefined);

/**
 * How the chain's unit moves, drawn in boxes and arrows: where it comes from (the faucet, issuance each
 * epoch), what accounts do with it, and where it goes (burned, the proposer's tip, locked in stake). The
 * numbers are the connected node's; without a node it shows the labels alone, since every chain sets its own.
 */
export function Flow({ params, fees, faucet, blockMs }: { params?: ParamsView; fees?: FeesView; faucet?: boolean; blockMs?: number }) {
  const p = params?.params ?? {};
  const stakers = num(p.issuance_stakers_bps);
  const pool = num(p.issuance_pool_bps);
  const treasury = stakers != null && pool != null ? 10_000 - stakers - pool : undefined;
  const rate = params?.issuance?.rate_ppm;
  const perEpoch = params?.issuance?.per_epoch;
  const epochBlocks = params?.genesis?.epoch_blocks;
  const ms = params?.genesis?.block_time_ms ?? blockMs;
  const unbonding = num(p.unbonding_blocks);
  const creation = fees?.account_creation_fee ?? p.account_creation_fee;
  const tipCap = fees?.tip_cap_multiple ?? num(p.tip_cap_multiple);
  const split = [
    { key: "stakers", label: "Stakers", bps: stakers },
    { key: "pool", label: "Contribution pool", bps: pool },
    { key: "treasury", label: "Treasury", bps: treasury },
  ];
  return (
    <figure className="flow" aria-label="How the unit flows">
      <div className="flow-col">
        <span className="flow-head">Comes in</span>
        <div className="flow-box">
          <b>{faucet === false ? "The starting supply" : "The faucet"}</b>
          <span className="dim small">{faucet === false ? "No faucet here: the creator's account hands it out." : "Hands out coins to anyone who asks."}</span>
        </div>
        <div className="flow-box" data-testid="flow-issuance">
          <b>Issuance{epochBlocks && ms ? `, every ${blocksToHuman(epochBlocks, ms)}` : ", every epoch"}</b>
          <span className="dim small">
            {rate != null ? `${pct(rate / 100)} a year` : "New coins each epoch"}
            {perEpoch && /^\d+$/.test(perEpoch) ? ` (${displayFang(perEpoch)} an epoch)` : ""}, split:
          </span>
          <div className="flow-split">
            {split.map((s) => (
              <span key={s.key} className={`flow-share ${s.key}`} ref={cssVars({ "--w": s.bps != null ? `${s.bps / 100}%` : null })}>
                {s.label}
                {s.bps != null && <em>{pct(s.bps)}</em>}
              </span>
            ))}
          </div>
        </div>
      </div>
      <div className="flow-arrow" aria-hidden="true" />
      <div className="flow-col flow-mid">
        <div className="flow-box flow-accounts">
          <b>Accounts</b>
          <span className="dim small">Send, pay names, stake, vote.</span>
        </div>
      </div>
      <div className="flow-arrow" aria-hidden="true" />
      <div className="flow-col">
        <span className="flow-head">Goes out</span>
        <div className="flow-box burn" data-testid="flow-burn">
          <b>Burned</b>
          <span className="dim small">
            Every transaction&apos;s base fee, priced per byte. Opening a new account burns {creation && /^\d+$/.test(creation) ? displayFang(creation) : "a creation fee"} more.
          </span>
        </div>
        <div className="flow-box">
          <b>The block&apos;s proposer</b>
          <span className="dim small">The tip, if you add one{tipCap ? ` (at most ${tipCap}× the base fee)` : ""}.</span>
        </div>
        <div className="flow-box">
          <b>Locked</b>
          <span className="dim small">Stake comes back {unbonding && ms ? `${blocksToHuman(unbonding, ms)} after you unstake` : "after an unbonding period"}; votes lock for a while too.</span>
        </div>
      </div>
      <figcaption className="dim small">It has no value by design: it only prices spam. Nobody sells it, buys it or gets paid in it.</figcaption>
    </figure>
  );
}
