"use client";

// Creators' pieces of the explorer (product review, wallet-creators): a GitHub chip that says
// whether the link is verified, a multisig's open actions rendered from the actions themselves,
// the per-network note on contribution pages, and a claim's timeline. Rules and copy are in
// lib/creators.ts; styles in app/creators.css (cre-*).
import "@/app/creators.css";
import { actionDigest, reviewAction, type Action } from "@blockula/sdk";
import Link from "next/link";
import { useEffect, useState } from "react";
import { Ext, Section } from "@/components/explorer/kit";
import { MAINNET_CHAIN_ID, NETWORK, RPC } from "@/lib/config";
import { approveCommand, claimTimeline, CONTRIBUTION_NOTES, GITHUB_HANDLE, MULTISIG_APPROVALS, readGithubCheck, type GithubCheck, type PocRules } from "@/lib/creators";
import { nodeGet, useNode } from "@/lib/explorer/node";
import { to } from "@/lib/explorer/routes";
import type { AccountDetail, ChainStatus, ClaimView, MultisigView } from "@/lib/explorer/types";
import { keyCode } from "@/lib/format";

// ---------------------------------------------------------------------------------------------
// GitHub (CONTRIB-12)

const CHECKS_KEY = "blockula.github-checks";

/** Checks already made this session, by node, handle, account and gist: each is asked once. */
function remembered(): Record<string, GithubCheck> {
  try {
    return JSON.parse(sessionStorage.getItem(CHECKS_KEY) ?? "{}") as Record<string, GithubCheck>;
  } catch {
    return {};
  }
}

function remember(key: string, check: GithubCheck) {
  try {
    sessionStorage.setItem(CHECKS_KEY, JSON.stringify({ ...remembered(), [key]: check }));
  } catch {
    /* storage unavailable: it's asked again next time */
  }
}

type Linked = NonNullable<AccountDetail["github"]>;

/** Checks that failed (a busy or rate-limited node), for this page load only: asked again on the next one. */
const failed = new Map<string, GithubCheck>();

/**
 * The node's off-chain check of a linked handle (`/api/github/{handle}`), once per session. Null
 * while it's being asked. A handle the chain couldn't have accepted isn't sent anywhere.
 */
export function useGithubCheck(link: Linked | null, account: string): GithubCheck | null {
  const handle = link?.handle ?? null;
  const key = link ? `${RPC}|${link.handle.toLowerCase()}|${account}|${link.gist_id}` : null;
  const [check, setCheck] = useState<{ key: string; value: GithubCheck } | null>(null);
  useEffect(() => {
    if (!handle || !key) return;
    if (!GITHUB_HANDLE.test(handle)) return setCheck({ key, value: { verified: false, reason: "not a GitHub handle the chain accepts" } });
    const known = remembered()[key] ?? failed.get(key);
    if (known) return setCheck({ key, value: known });
    let stop = false;
    nodeGet<unknown>(`/github/${encodeURIComponent(handle)}`).then(
      (body) => {
        const value = readGithubCheck(body, handle, account);
        remember(key, value);
        if (!stop) setCheck({ key, value });
      },
      (e: Error) => {
        const value = { verified: false, reason: `the node couldn't check it: ${e.message}` };
        failed.set(key, value);
        if (!stop) setCheck({ key, value });
      },
    );
    return () => void (stop = true);
  }, [key, handle, account]);
  return check && check.key === key ? check.value : null;
}

/** `github · @handle`, with an "unverified" pill (the reason on hover) unless the node verified it. */
export function GithubChip({ link, account }: { link: Linked; account: string }) {
  const check = useGithubCheck(link, account);
  const verified = check?.verified === true;
  return (
    <span className="cre-gh" data-testid="github">
      <Ext className="chip" href={`https://github.com/${encodeURIComponent(link.handle)}`}>
        <span title={verified ? `Verified by this node, off-chain: ${check.reason}` : undefined}>github · @{link.handle}</span>
      </Ext>
      {!verified && (
        <span className="pill pending cre-gh-pill" data-testid="github-unverified" tabIndex={0} title={check ? `Unverified: ${check.reason}. Anyone can link any handle; only a public gist by that user holding this account's address verifies it.` : "Checking with the node…"}>
          unverified
        </span>
      )}
    </span>
  );
}

// ---------------------------------------------------------------------------------------------
// Multisig (CRE-3)

type Pending = MultisigView["pending"][number];

/** A multisig's open actions, each described from the action itself (what an approval binds), never from the node's summary. */
export function MultisigActions({ ms, address, head }: { ms: MultisigView; address: string; head?: number }) {
  const labels = new Map(ms.owners.map((o) => [o.key_id, o.label || keyCode(o.key_id)]));
  return (
    <Section title="Open actions" aside={<span className="dim small">{ms.pending.length ? `${ms.pending.length} waiting` : "none waiting"}</span>}>
      {ms.pending.length ? (
        <ul className="ledger" data-testid="ms-actions">
          {ms.pending.map((p) => (
            <PendingAction key={p.id} p={p} threshold={ms.threshold} labels={labels} address={address} head={head} />
          ))}
        </ul>
      ) : (
        <p className="ledger-empty">Nothing is waiting for approval.</p>
      )}
      <p className="card-note">Each action is described here from the action itself, the content an approval binds by its digest, not from the node&apos;s words. {MULTISIG_APPROVALS}</p>
    </Section>
  );
}

function PendingAction({ p, threshold, labels, address, head }: { p: Pending; threshold: number; labels: Map<string, string>; address: string; head?: number }) {
  const action = p.action as Record<string, unknown> | undefined;
  // A ballot binds its proposal by digest: describe it only from a proposal whose content hashes to it.
  const ballot = action?.type === "vote" && Number.isSafeInteger(action.proposal_id) ? (action.proposal_id as number) : null;
  const proposal = useNode<unknown>(ballot != null ? `/proposal/${ballot}` : null);
  const review = reviewAction(action, ballot != null ? { proposal: proposal.data } : {});
  let digest: string | null = null;
  try {
    digest = actionDigest(action as unknown as Action);
  } catch {
    digest = null;
  }
  const approvable = review.readable && !!digest && !p.expired;
  const by = p.approvals.map((k) => labels.get(k) ?? `a key that isn't an owner (${keyCode(k)})`);
  return (
    <li className="cre-ms-action" data-testid="ms-action">
      <div className="row-between">
        <span className="cre-ms-id">#{p.id}</span>
        <span className={`pill ${p.expired ? "dim" : "pending"}`}>
          {p.valid_approvals} of {threshold} approvals
        </span>
      </div>
      <p className="cre-ms-does" data-testid="ms-does">
        {review.does}
      </p>
      {!review.readable && <p className="warn small m0">This wallet can&apos;t read all of it: don&apos;t approve it.</p>}
      <span className="small dim block">
        {by.length ? `approved by ${by.join(", ")}` : "no approvals yet"} · {p.expired ? "expired at" : "expires at"} block #{p.expires_at.toLocaleString("en-US")}
        {!p.expired && head != null && p.expires_at <= head ? " (any moment now)" : ""}
      </span>
      {digest && (
        <span className="mono small block wrap-any" data-testid="ms-digest">
          digest {digest}
        </span>
      )}
      {approvable && digest && <code className="cre-cmd small block wrap-any">{approveCommand(p.id, address, digest)}</code>}
    </li>
  );
}

// ---------------------------------------------------------------------------------------------
// Contribution pages (CONTRIB-2, CONTRIB-16)

/**
 * Whether the page is about mainnet: the network picked says so, or its node reports blockula-1.
 * Null until that's known, so neither network's note shows on the other.
 */
export function useMainnet(): boolean | null {
  const status = useNode<ChainStatus>(NETWORK.mainnet ? null : "/status", { every: 30_000 });
  if (NETWORK.mainnet) return true;
  if (!status.data) return null;
  return status.data.chain_id === MAINNET_CHAIN_ID;
}

/** The note every contribution page carries, for the network it's on (lib/creators.ts CONTRIBUTION_NOTES, D7). */
export function ContributionNote() {
  const mainnet = useMainnet();
  if (mainnet == null) return null;
  return (
    <p className="cre-note" data-testid="contribution-note">
      {mainnet ? CONTRIBUTION_NOTES.mainnet : CONTRIBUTION_NOTES.testnet}
    </p>
  );
}

/** A claim's way to a reward, with times from the claim and the pool's rules. */
export function ClaimTimeline({ claim, rules, head, blockMs }: { claim: ClaimView; rules?: PocRules | null; head?: number; blockMs?: number }) {
  const steps = claimTimeline(claim, rules, head, blockMs);
  return (
    <Section title="Timeline" aside={<span className="dim small">from merge to reward</span>}>
      <ol className="cre-timeline" data-testid="claim-timeline">
        {steps.map((s) => (
          <li key={s.label} className={`cre-step ${s.state}`}>
            <span className="cre-step-label">{s.label}</span>
            <span className="cre-step-when">
              {s.seance != null && (
                <>
                  <Link href={to.seance(s.seance)}>Séance #{s.seance}</Link>
                  {": "}
                </>
              )}
              {s.when}
            </span>
          </li>
        ))}
      </ol>
    </Section>
  );
}
