"use client";

import "@/app/money.css";
import { ApiError, paramsOf, parseFang, sharesFor, stakeSummary, type StakeDelegation, type StakeSummary, type StakeUnbonding } from "@blockula/sdk";
import Link from "next/link";
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { RequireWallet } from "@/components/Gate";
import { ReviewStep } from "@/components/money/ReviewStep";
import { ErrorNote, ExplorerLink, Spinner, Tip } from "@/components/ui";
import { explorer, MAINNET_CHAIN_ID, NETWORK } from "@/lib/config";
import { friendly } from "@/lib/errors";
import { movable } from "@/lib/explorer/preflight";
import type { TxDetail } from "@/lib/explorer/types";
import { aboutWhen, blocksToHuman, displayFang, shortAddr } from "@/lib/format";
import { FOUNDER_RUN, moveRisk, stakeRisk, unstakeRisk } from "@/lib/money-copy";
import type { TxView, ValidatorsView, ValidatorView } from "@/lib/types";
import { useUnlocked } from "@/lib/wallet";

export default function StakePage() {
  return (
    <RequireWallet>
      <Stake />
    </RequireWallet>
  );
}

interface Loaded {
  /** `/api/staking` as served. */
  raw: unknown;
  validators: ValidatorView[];
  /** Blocks unbonding takes, if the node says. */
  unbonding?: number;
}

const pct = (bps: number) => `${(bps / 100).toLocaleString("en-US", { maximumFractionDigits: 2 })}%`;
const label = (v: { validator: string; validator_name: string | null }) => v.validator_name ?? shortAddr(v.validator);
const nameOf = (v: ValidatorView) => v.validator.name ?? shortAddr(v.validator.address);
const exact = (d: bigint) => displayFang(d, { exact: true });

type Run = (f: () => Promise<TxView>) => Promise<boolean>;

/** What every staking review needs to say: how long unbonding takes, and whether it's mainnet. */
interface Terms {
  unbondingFor: string;
  /** "about Nov 11", when stake unbonding now would be back. */
  backAbout: string | null;
  mainnet: boolean;
  height: number;
  blockMs: number;
}

function Stake() {
  const { wallet, account, client, status, refresh } = useUnlocked();
  const [data, setData] = useState<Loaded | "inactive" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<TxView | null>(null);

  const load = useCallback(async () => {
    try {
      const [raw, vals, params] = await Promise.all([client.staking(wallet.account), client.validators() as Promise<ValidatorsView>, client.params().catch(() => null)]);
      const unbonding = Number(paramsOf(params).unbonding_blocks) || undefined;
      // Jailed ones too, so a pool you're in still shows its commission and why it's out.
      setData({ raw, validators: [...vals.active, ...vals.candidates, ...(vals.jailed ?? [])], unbonding });
    } catch (e) {
      // Nodes without staking (before protocol v1) don't serve /api/staking.
      if (e instanceof ApiError && e.status === 404) setData("inactive");
      else setError(friendly(e));
    }
  }, [client, wallet]);

  useEffect(() => {
    load();
  }, [load]);

  /** Runs one staking tx, then reloads everything it changed. True when it went through. */
  const run: Run = async (f) => {
    setError(null);
    setDone(null);
    try {
      setDone(await f());
      await Promise.all([load(), refresh()]);
      return true;
    } catch (e) {
      setError(friendly(e));
      return false;
    }
  };

  if (data === null) return <main className="narrow">{error ? <ErrorNote error={error} /> : <Spinner>Loading your stake…</Spinner>}</main>;
  if (data === "inactive")
    return (
      <main className="narrow stack-lg">
        <Link href="/" className="back">← Home</Link>
        <h1>Stake</h1>
        <section className="card learn">
          <p>Staking isn&apos;t active on this network yet. Once it is, you can delegate FANG to a validator&apos;s pool here and earn a share of the network&apos;s issuance.</p>
        </section>
      </main>
    );

  const { validators, unbonding } = data;
  const stake: StakeSummary = stakeSummary(data.raw);
  // Ready from the next block on (`release_at`): follows the head between reloads.
  const ready = (u: StakeUnbonding) => u.ready || (!!status && u.completes_at <= status.height + 1);
  const withdrawable = stake.unbonding.filter(ready).reduce((t, u) => t + BigInt(u.amount), 0n);
  const blockMs = status?.block_time_ms ?? 1000;
  const unbondingFor = unbonding ? blocksToHuman(unbonding, blockMs) : "the unbonding period";
  const terms: Terms = {
    unbondingFor,
    backAbout: unbonding ? aboutWhen(unbonding, blockMs) : null,
    mainnet: NETWORK.mainnet === true || client.pin.chainId === MAINNET_CHAIN_ID || status?.chain_id === MAINNET_CHAIN_ID,
    height: status?.height ?? 0,
    blockMs,
  };
  return (
    <main className="narrow stack-lg">
      <Link href="/" className="back">← Home</Link>
      <div className="section-head">
        <span className="eyebrow">Proof of stake</span>
        <h1>Stake</h1>
      </div>
      <section className="figures-row" aria-label="Your stake">
        <div>
          <div className="figure" data-testid="staked">{displayFang(stake.bonded)}</div>
          <div className="figure-k">staked</div>
        </div>
        <div>
          <div className="figure">{displayFang(stake.unbonding_total)}</div>
          <div className="figure-k">unbonding</div>
        </div>
        <div>
          <div className="figure">{account ? displayFang(account.balance) : "…"}</div>
          <div className="figure-k">available</div>
        </div>
      </section>

      {done && (
        <div className="banner" role="status">
          ✓ {done.summary} <ExplorerLink href={explorer.tx(done.id)}>View ↗</ExplorerLink>
        </div>
      )}
      <ErrorNote error={error} />

      <section className="card">
        <h2>Your pools</h2>
        {stake.delegations.length ? (
          stake.delegations.map((d) => <PoolRow key={d.validator} d={d} v={validators.find((x) => x.validator.address === d.validator)} validators={validators} terms={terms} run={run} />)
        ) : (
          <p className="dim">You haven&apos;t staked yet.</p>
        )}
      </section>

      {stake.unbonding.length > 0 && (
        <section className="card stack">
          <h2>Unbonding</h2>
          {stake.unbonding.map((u) => (
            <div className="pool-row" key={u.id}>
              <span>{displayFang(u.amount)}<span className="dim"> from {shortAddr(u.validator)}</span></span>
              <span className="dim small">{ready(u) ? "ready to withdraw" : `ready at block #${u.completes_at}${status ? ` (in about ${blocksToHuman(Math.max(0, u.completes_at - status.height), status.block_time_ms)})` : ""}`}</span>
            </div>
          ))}
          <button className="btn" disabled={withdrawable === 0n} onClick={() => run(() => wallet.withdrawUnbonded())}>
            {withdrawable > 0n ? `Withdraw ${displayFang(withdrawable, { exact: true })}` : "Withdraw what's finished unbonding"}
          </button>
        </section>
      )}

      <Delegate validators={validators} terms={terms} run={run} />

      <Tip>
        Staked FANG stays yours, but it&apos;s bonded: it earns a share of issuance while the validator does its job, it&apos;s slashed with the pool if the validator double-signs or keeps missing its turns, and taking it out takes {unbondingFor}.
      </Tip>
    </main>
  );
}

/** A validator in a review: its name and full address, what it charges, and whether it's in. */
/**
 * A validator's rows in a review. `heading` names it ("Validator", or "From" and "To" when moving
 * stake, whose rows then say which side they're about: "From · commission").
 */
function validatorRows(v: ValidatorView | undefined, address: string, t: Terms, heading = "Validator"): [string, ReactNode][] {
  const who: ReactNode = (
    <>
      <b>{v ? nameOf(v) : shortAddr(address)}</b>
      <div className="mono small break">{address}</div>
    </>
  );
  if (!v) return [[heading, who]];
  const state = v.jailed
    ? `Jailed until block #${v.jailed.until.toLocaleString("en-US")} (${aboutWhen(v.jailed.until - t.height, t.blockMs)}): it earns nothing until it's back`
    : v.tombstoned_at != null
      ? "Removed for good after double-signing: it never validates again"
      : v.status === "active"
        ? "In the active set"
        : "A candidate: it earns once it joins the active set";
  const about = (what: string) => (heading === "Validator" ? what : `${heading} · ${what.toLowerCase()}`);
  return [
    [heading, who],
    [about("Commission"), `${pct(v.commission_bps)} of what the pool earns`],
    [about("Status"), <span key="s" className={v.jailed || v.tombstoned_at != null ? "warn" : undefined} data-testid="validator-status">{state}</span>],
  ];
}

const FEE_ROW: [string, ReactNode] = ["Network fee", <span key="f" className="dim">a small fee, burned (the receipt shows it)</span>];

/** Shares are the pool's unit: shown on request, behind their value. */
function Shares({ n }: { n: string | bigint }) {
  return (
    <details className="money-shares small">
      <summary>shares</summary> <span>{BigInt(n).toLocaleString("en-US")} pool shares</span>
    </details>
  );
}

function PoolRow({ d, v, validators, terms, run }: { d: StakeDelegation; v?: ValidatorView; validators: ValidatorView[]; terms: Terms; run: Run }) {
  const { wallet } = useUnlocked();
  const [open, setOpen] = useState<"unstake" | "move" | null>(null);
  const [reviewing, setReviewing] = useState(false);
  const [amount, setAmount] = useState("");
  const [to, setTo] = useState("");
  const [busy, setBusy] = useState(false);
  const toggle = (which: "unstake" | "move") => {
    setOpen(open === which ? null : which);
    setReviewing(false);
    setAmount("");
  };
  let amountErr: string | null = null;
  let drops: bigint | null = null;
  try {
    if (amount.trim()) {
      drops = parseFang(amount);
      if (drops <= 0n) amountErr = "Enter more than zero.";
      // The chain refuses more than the stake is worth; all of it is the empty field.
      else if (drops > BigInt(d.value)) amountErr = `That's more than this stake is worth (your node says ${exact(BigInt(d.value))}). Leave the field empty to ${open === "move" ? "move" : "unstake"} all of it.`;
    }
  } catch {
    amountErr = "Use a number like 1 or 0.25.";
  }
  const targets = validators.filter((x) => x.eligible && x.validator.address !== d.validator);
  const target = targets.find((x) => x.validator.address === to);
  const founder = terms.mainnet ? [FOUNDER_RUN] : [];

  const confirm = async () => {
    setBusy(true);
    const ok = await run(() =>
      open === "move" && target
        ? // Redelegation signs pool shares, worked out at the value your node reports (all of them, for "all").
          wallet.redelegate(d.validator, target.validator.address, drops === null ? "all" : sharesFor(d, drops))
        : // An amount is signed as an amount: the chain converts it to shares at its own price when it
          // runs. Converting it here took the price from your node's report of the stake, and a node
          // that said it was worth a little more than the amount (or was many more shares) turned a
          // partial unstake into all of it (audit gap-7-5).
          wallet.undelegate(d.validator, drops === null ? "all" : { amount: drops }),
    );
    setBusy(false);
    setReviewing(false);
    if (ok) {
      setOpen(null);
      setAmount("");
      setTo("");
    }
  };

  const meta = [v && `${pct(v.commission_bps)} commission`, BigInt(d.value) > BigInt(d.principal) && `earned ${displayFang(BigInt(d.value) - BigInt(d.principal))}`, d.self_bond && "self-bond", v?.jailed && `jailed until block #${v.jailed.until}`].filter(Boolean).join(" · ");
  const howMuch = drops === null ? `all of it (about ${displayFang(d.value)} at your node's latest price)` : exact(drops);

  return (
    <div className="pool-row" data-testid="pool">
      <span>
        <b>{label(d)}</b> <span className="dim small">{meta}</span> <Shares n={d.shares} />
      </span>
      <span className="row-gap">
        <span>{displayFang(d.value)}</span>
        <button className="btn small" aria-expanded={open === "unstake"} onClick={() => toggle("unstake")}>Unstake</button>
        <button className="btn small" aria-expanded={open === "move"} onClick={() => toggle("move")} disabled={!targets.length} title={targets.length ? "Move this stake to another validator" : "No other validator to move to"}>Move</button>
      </span>
      {open === "unstake" &&
        (reviewing ? (
          <ReviewStep
            title="Review: unstake"
            summary={drops === null ? `Unstake all of your stake with ${label(d)}` : `Unstake ${exact(drops)} from ${label(d)}`}
            rows={[
              ...validatorRows(v, d.validator, terms),
              ["Amount", drops === null ? howMuch : <>{howMuch} <span className="dim small">(the chain turns it into pool shares at its own price when it runs, so at least that much unbonds)</span></>],
              ["Back in", `${terms.unbondingFor}${terms.backAbout ? `, ${terms.backAbout}` : ""}. Then withdraw it here.`],
              FEE_ROW,
            ]}
            terms={[unstakeRisk(terms.unbondingFor), ...founder]}
            confirm="Confirm & unstake"
            busy={busy}
            onBack={() => setReviewing(false)}
            onConfirm={confirm}
          />
        ) : (
          <div className="stack grow">
            <label className="field">
              <span>How much <span className="dim">(empty: all of it)</span></span>
              <div className="suffix-input">
                <input value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="all" inputMode="decimal" aria-label="Amount to unstake" />
                <span className="suffix">FANG</span>
              </div>
            </label>
            {amountErr && <span className="warn">{amountErr}</span>}
            <p className="dim small">It stops earning now and can be withdrawn in {terms.unbondingFor}.</p>
            <button className="btn primary" disabled={!!amountErr} onClick={() => setReviewing(true)}>{drops !== null ? `Unstake ${amount.trim()} FANG` : "Unstake all"}</button>
          </div>
        ))}
      {open === "move" &&
        (reviewing && target ? (
          <ReviewStep
            title="Review: move stake"
            summary={`Move ${drops === null ? "all of your stake" : `about ${exact(drops)}`} from ${label(d)} to ${nameOf(target)}`}
            rows={[
              ...validatorRows(v, d.validator, terms, "From"),
              ...validatorRows(target, target.validator.address, terms, "To"),
              ["Amount", <>{drops === null ? howMuch : `about ${exact(drops)}, at your node's latest price`} <Shares n={drops === null ? d.shares : sharesFor(d, drops)} /></>],
              FEE_ROW,
            ]}
            terms={[moveRisk(terms.unbondingFor), stakeRisk(terms.unbondingFor), ...founder]}
            confirm="Confirm & move"
            busy={busy}
            onBack={() => setReviewing(false)}
            onConfirm={confirm}
          />
        ) : (
          <div className="stack grow">
            <label className="field">
              <span>Move to</span>
              <select className="field-select money-select" value={to} onChange={(e) => setTo(e.target.value)} aria-label="Move to validator">
                <option value="">Choose a validator…</option>
                {targets.map((x) => (
                  <option key={x.validator.address} value={x.validator.address}>
                    {nameOf(x)} · {pct(x.commission_bps)} commission
                  </option>
                ))}
              </select>
            </label>
            <label className="field">
              <span>How much <span className="dim">(empty: all of it)</span></span>
              <div className="suffix-input">
                <input value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="all" inputMode="decimal" aria-label="Amount to move" />
                <span className="suffix">FANG</span>
              </div>
            </label>
            {amountErr && <span className="warn">{amountErr}</span>}
            <p className="dim small">It keeps earning, with the other validator, from now on: no unbonding wait.</p>
            <button className="btn primary" disabled={!target || !!amountErr} onClick={() => setReviewing(true)}>
              {target ? `Move ${drops !== null ? `${amount.trim()} FANG` : "all"} to ${nameOf(target)}` : "Move"}
            </button>
          </div>
        ))}
    </div>
  );
}

function Delegate({ validators, terms, run }: { validators: ValidatorView[]; terms: Terms; run: Run }) {
  const { wallet, account } = useUnlocked();
  const [to, setTo] = useState("");
  const [amount, setAmount] = useState("");
  const [reviewing, setReviewing] = useState(false);
  const [busy, setBusy] = useState(false);
  const eligible = validators.filter((v) => v.eligible);
  // Staking takes liquid FANG, locked or not (a vote's or vesting's lock can be staked: node/src/econ.rs
  // `check_locks`), less what pending transactions will take.
  const free = account ? movable({ label: account.account, balance: account.balance, recent: account.recent_txs as unknown as TxDetail[] }).spendable : null;
  let drops: bigint | null = null;
  let amountErr: string | null = null;
  try {
    if (amount.trim()) {
      drops = parseFang(amount);
      if (drops === 0n) amountErr = "Enter more than zero.";
      else if (free !== null && drops > free) amountErr = `You only have ${exact(free)}.`;
    }
  } catch {
    amountErr = "Use a number like 1 or 0.25 (up to 9 decimals).";
  }
  const v = eligible.find((x) => x.validator.address === to);
  const confirm = async () => {
    if (!v || drops === null) return;
    setBusy(true);
    const ok = await run(() => wallet.delegate(v.validator.address, drops as bigint));
    setBusy(false);
    setReviewing(false);
    if (ok) setAmount("");
  };
  if (reviewing && v && drops !== null)
    return (
      <section className="card stack">
        <ReviewStep
          title="Review: stake"
          summary={`Stake ${exact(drops)} with ${nameOf(v)}`}
          rows={[...validatorRows(v, v.validator.address, terms), ["Amount", exact(drops)], FEE_ROW]}
          terms={[stakeRisk(terms.unbondingFor), ...(terms.mainnet ? [FOUNDER_RUN] : [])]}
          confirm="Confirm & stake"
          busy={busy}
          onBack={() => setReviewing(false)}
          onConfirm={confirm}
        />
      </section>
    );
  return (
    <section className="card stack">
      <h2>Delegate</h2>
      <label className="field">
        <span>Validator</span>
        <select className="field-select money-select" value={to} onChange={(e) => setTo(e.target.value)} aria-label="Validator">
          <option value="">Choose a validator…</option>
          {eligible.map((x) => (
            <option key={x.validator.address} value={x.validator.address}>
              {nameOf(x)}
              {` · ${displayFang(x.stake)} staked`}
              {` · ${pct(x.commission_bps)} commission`}
            </option>
          ))}
        </select>
      </label>
      {v && <div className="mono small break">{v.validator.address}</div>}
      <label className="field">
        <span>Amount</span>
        <div className="suffix-input">
          <input value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="0.0" inputMode="decimal" aria-label="Amount to delegate" />
          <span className="suffix">FANG</span>
        </div>
      </label>
      <div className="name-status">{amountErr ? <span className="warn">{amountErr}</span> : <span className="dim small">Available: {free !== null ? exact(free) : "…"}</span>}</div>
      <button className="btn primary" disabled={!v || drops === null || !!amountErr || busy} onClick={() => setReviewing(true)}>
        {v && drops ? `Delegate ${exact(drops)} to ${nameOf(v)}` : "Delegate"}
      </button>
    </section>
  );
}
