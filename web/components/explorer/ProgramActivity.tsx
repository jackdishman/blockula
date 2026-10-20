"use client";

import { useState } from "react";
import { callTitle, filterActivity, type ActivitySort } from "@/lib/explorer/activity";
import type { Program } from "@/lib/explorer/programs";
import { useProgramActivity } from "@/lib/explorer/useProgramActivity";
import type { TxStatus } from "@/lib/explorer/types";
import { Amount, Section, Stat, plural } from "./kit";
import { Ledger } from "./Ledger";

export function ProgramActivity({ program }: { program: Program }) {
  const activity = useProgramActivity(program.calls);
  const [call, setCall] = useState("all");
  const [status, setStatus] = useState<TxStatus | "all">("all");
  const [search, setSearch] = useState("");
  const [sort, setSort] = useState<ActivitySort>("newest");
  const shown = filterActivity(activity.txs, { call, status, search, sort });
  const filtered = call !== "all" || status !== "all" || search.trim() !== "";
  const fees = activity.txs.reduce((sum, t) => sum + (t.status === "pending" ? 0n : BigInt(t.fee ?? "0")), 0n);
  const count = (s: TxStatus) => activity.txs.filter((t) => t.status === s).length;

  return (
    <Section title="Recent transactions" aside={<span className="dim small">{program.title} activity</span>}>
      <div className="figures program-activity-stats" aria-label="Loaded activity summary">
        <Stat v={activity.loading ? "…" : count("finalized")} k="finalized" />
        <Stat v={activity.loading ? "…" : count("pending")} k="pending" />
        <Stat v={activity.loading ? "…" : count("failed")} k="failed" />
        <Stat v={activity.loading ? "…" : <Amount drops={fees} unit={false} />} k="fees charged · FANG" />
      </div>
      <div className="activity-controls">
        <label className="activity-search">Search transactions
          <input type="search" placeholder=".boo name, summary, memo, or transaction ID" value={search} onChange={(e) => setSearch(e.target.value)} />
        </label>
        <label>Action
          <select className="field-select" aria-label="Action" value={call} onChange={(e) => setCall(e.target.value)}>
            <option value="all">All actions</option>
            {program.calls.map((kind) => <option key={kind} value={kind}>{callTitle(kind)}</option>)}
          </select>
        </label>
        <label>Status
          <select className="field-select" aria-label="Status" value={status} onChange={(e) => setStatus(e.target.value as TxStatus | "all")}>
            <option value="all">All statuses</option>
            <option value="finalized">Finalized</option>
            <option value="pending">Pending</option>
            <option value="failed">Failed</option>
          </select>
        </label>
        <label>Sort by
          <select className="field-select" aria-label="Sort by" value={sort} onChange={(e) => setSort(e.target.value as ActivitySort)}>
            <option value="newest">Newest first</option>
            <option value="oldest">Oldest first</option>
            <option value="fee-high">Highest fee</option>
            <option value="fee-low">Lowest fee</option>
          </select>
        </label>
      </div>
      {activity.error && <p className="error" role="alert">Couldn&apos;t load transaction history: {activity.error.message} <button className="link small" onClick={activity.retry} disabled={activity.busy}>Retry</button></p>}
      {activity.pendingError && <p className="error" role="alert">Pending transactions are unavailable: {activity.pendingError.message}</p>}
      <div className="row-between small mb-14">
        <span className="dim" role="status">{activity.loading ? "Reading recent blocks…" : `${shown.length} of ${plural(activity.txs.length, "loaded transaction")}`}</span>
        {filtered && <button className="link small" onClick={() => { setCall("all"); setStatus("all"); setSearch(""); }}>Clear filters</button>}
      </div>
      <Ledger txs={shown} showFee empty={activity.loading ? "Loading activity…" : activity.error && !activity.txs.length ? "Transaction history is unavailable. Retry to read it again." : filtered ? "No transactions match these filters." : "No transactions for this program in the blocks searched."} />
      <div className="tools mt-16">
        {!activity.exhausted && <button className="btn small" onClick={activity.loadOlder} disabled={activity.busy || activity.loading}>{activity.busy ? "Reading blocks…" : "Load older transactions"}</button>}
      </div>
      <p className="card-note">
        {activity.oldest != null && activity.newest != null ? `Searched blocks #${activity.oldest.toLocaleString("en-US")}–#${activity.newest.toLocaleString("en-US")}. ` : ""}
        Summaries and filters cover loaded transactions. Pending calls are from the node&apos;s latest 200 pending transactions; their fees haven&apos;t been charged.
        {activity.exhausted ? " Reached the end of this node’s available history; older blocks may be pruned." : " Load older to search further back."}
      </p>
    </Section>
  );
}
