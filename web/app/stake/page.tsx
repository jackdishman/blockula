"use client";

import { ApiError, formatFang, paramsOf, parseFang, stakeSummary, type StakeDelegation, type StakeSummary, type StakeUnbonding } from "@blockula/sdk";
import Link from "next/link";
import { useCallback, useEffect, useState } from "react";
import { RequireWallet } from "@/components/Gate";
import { ErrorNote, ExplorerLink, Spinner, Tip } from "@/components/ui";
import { explorer } from "@/lib/config";
import { friendly } from "@/lib/errors";
import { blocksToHuman, shortAddr } from "@/lib/format";
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

  /** Runs one staking tx, then reloads everything it changed. */
  const run = async (f: () => Promise<TxView>) => {
    setError(null);
    setDone(null);
    try {
      setDone(await f());
      await Promise.all([load(), refresh()]);
    } catch (e) {
      setError(friendly(e));
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
  const unbondingFor = unbonding ? blocksToHuman(unbonding, status?.block_time_ms) : "the unbonding period";
  return (
    <main className="narrow stack-lg">
      <Link href="/" className="back">← Home</Link>
      <div className="section-head">
        <span className="eyebrow">Proof of stake</span>
        <h1>Stake</h1>
      </div>
      <section className="figures-row" aria-label="Your stake">
        <div>
          <div className="figure" data-testid="staked">{formatFang(BigInt(stake.bonded))}</div>
          <div className="figure-k">staked</div>
        </div>
        <div>
          <div className="figure">{formatFang(BigInt(stake.unbonding_total))}</div>
          <div className="figure-k">unbonding</div>
        </div>
        <div>
          <div className="figure">{account?.balance_display ?? "…"}</div>
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
        {stake.delegations.length ? stake.delegations.map((d) => <PoolRow key={d.validator} d={d} v={validators.find((x) => x.validator.address === d.validator)} unbondingFor={unbondingFor} run={run} />) : <p className="dim">You haven&apos;t staked yet.</p>}
      </section>

      {stake.unbonding.length > 0 && (
        <section className="card stack">
          <h2>Unbonding</h2>
          {stake.unbonding.map((u) => (
            <div className="pool-row" key={u.id}>
              <span>{formatFang(BigInt(u.amount))}<span className="dim"> from {shortAddr(u.validator)}</span></span>
              <span className="dim small">{ready(u) ? "ready to withdraw" : `ready at block #${u.completes_at}${status ? ` (in about ${blocksToHuman(Math.max(0, u.completes_at - status.height), status.block_time_ms)})` : ""}`}</span>
            </div>
          ))}
          <button className="btn" disabled={withdrawable === 0n} onClick={() => run(() => wallet.withdrawUnbonded())}>
            {withdrawable > 0n ? `Withdraw ${formatFang(withdrawable)}` : "Withdraw what's finished unbonding"}
          </button>
        </section>
      )}

      <Delegate validators={validators} run={run} />

      <Tip>
        Staked FANG stays yours, but it&apos;s bonded: it earns a share of issuance while the validator does its job, it&apos;s slashed with the pool if the validator double-signs or keeps missing its turns, and taking it out takes {unbondingFor}.
      </Tip>
    </main>
  );
}

function PoolRow({ d, v, unbondingFor, run }: { d: StakeDelegation; v?: ValidatorView; unbondingFor: string; run: (f: () => Promise<TxView>) => Promise<void> }) {
  const { wallet } = useUnlocked();
  const [open, setOpen] = useState(false);
  const [amount, setAmount] = useState("");
  const [busy, setBusy] = useState(false);
  const unstake = async () => {
    setBusy(true);
    // An amount is signed as an amount: the chain converts it to shares at its own price when it
    // runs. Converting it here took the price from your node's report of the stake, and a node
    // that said it was worth a little more than the amount (or was many more shares) turned a
    // partial unstake into all of it (audit gap-7-5).
    await run(() => wallet.undelegate(d.validator, amount.trim() ? { amount: amount.trim() } : "all"));
    setBusy(false);
    setOpen(false);
    setAmount("");
  };
  let amountErr: string | null = null;
  try {
    if (amount.trim() && parseFang(amount) <= 0n) amountErr = "Enter more than zero.";
    // The chain refuses more than the stake is worth; all of it is the empty field.
    else if (amount.trim() && parseFang(amount) > BigInt(d.value)) amountErr = `That's more than this stake is worth (your node says ${formatFang(BigInt(d.value))}). Leave the field empty to unstake all of it.`;
  } catch {
    amountErr = "Use a number like 1 or 0.25.";
  }
  return (
    <div className="pool-row" data-testid="pool">
      <span>
        <b>{label(d)}</b> <span className="dim small">{BigInt(d.shares).toLocaleString("en-US")} shares{BigInt(d.value) > BigInt(d.principal) ? ` · earned ${formatFang(BigInt(d.value) - BigInt(d.principal))}` : ""}{d.self_bond ? " · self-bond" : ""}{v ? ` · ${pct(v.commission_bps)} commission` : ""}{v?.jailed ? ` · jailed until block #${v.jailed.until}` : ""}</span>
      </span>
      <span className="row-gap">
        <span>{formatFang(BigInt(d.value))}</span>
        <button className="btn small" onClick={() => setOpen(!open)}>Unstake</button>
      </span>
      {open && (
        <div className="stack grow">
          <label className="field">
            <span>How much <span className="dim">(empty: all of it)</span></span>
            <div className="suffix-input">
              <input value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="all" inputMode="decimal" aria-label="Amount to unstake" />
              <span className="suffix">FANG</span>
            </div>
          </label>
          {amountErr && <span className="warn">{amountErr}</span>}
          <p className="dim small">{amount.trim() ? "The chain turns it into pool shares at its own price when it runs, so at least that much unbonds. " : ""}It stops earning now and can be withdrawn in {unbondingFor}.</p>
          <button className="btn primary" disabled={busy || !!amountErr} onClick={unstake}>{busy ? <Spinner>Sealing…</Spinner> : amount.trim() ? `Unstake ${amount} FANG` : "Unstake all"}</button>
        </div>
      )}
    </div>
  );
}

function Delegate({ validators, run }: { validators: ValidatorView[]; run: (f: () => Promise<TxView>) => Promise<void> }) {
  const { wallet, account } = useUnlocked();
  const [to, setTo] = useState("");
  const [amount, setAmount] = useState("");
  const [busy, setBusy] = useState(false);
  const eligible = validators.filter((v) => v.eligible);
  let drops: bigint | null = null;
  let amountErr: string | null = null;
  try {
    if (amount.trim()) {
      drops = parseFang(amount);
      if (drops === 0n) amountErr = "Enter more than zero.";
      else if (account && drops > BigInt(account.balance)) amountErr = `You only have ${account.balance_display}.`;
    }
  } catch {
    amountErr = "Use a number like 1 or 0.25 (up to 9 decimals).";
  }
  const v = eligible.find((x) => x.validator.address === to);
  const submit = async () => {
    if (!v || drops === null) return;
    setBusy(true);
    await run(() => wallet.delegate(v.validator.address, drops as bigint));
    setBusy(false);
    setAmount("");
  };
  return (
    <section className="card stack">
      <h2>Delegate</h2>
      <label className="field">
        <span>Validator</span>
        <select className="field-select" value={to} onChange={(e) => setTo(e.target.value)} aria-label="Validator">
          <option value="">Choose a validator…</option>
          {eligible.map((x) => (
            <option key={x.validator.address} value={x.validator.address}>
              {x.validator.name ?? shortAddr(x.validator.address)}
              {x.stake_display ? ` · ${x.stake_display} staked` : ""}
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
      <div className="name-status">{amountErr ? <span className="warn">{amountErr}</span> : <span className="dim small">Available: {account?.balance_display ?? "…"}</span>}</div>
      <button className="btn primary" disabled={!v || drops === null || !!amountErr || busy} onClick={submit}>
        {busy ? <Spinner>Sealing into a block…</Spinner> : v && drops ? `Delegate ${formatFang(drops)} to ${v.validator.name ?? shortAddr(v.validator.address)}` : "Delegate"}
      </button>
    </section>
  );
}
