"use client";

import { useEffect, useState } from "react";
import { Copy, Spinner } from "@/components/ui";
import { agentPrompt } from "@/lib/agent-prompt";
import { DEMO_NODE } from "@/lib/config";
import { walletOf } from "@/lib/site";

/** What the site reads from the demo node's `/api/status` (node/src/api.rs `status`). Chain text is shown as text only. */
interface DemoStatus {
  chain_id: string;
  name?: string | null;
  description?: string | null;
  token?: string;
  height: number;
  block_time_ms?: number;
  active_validators?: number;
  accounts?: number;
  names?: number;
  faucet?: boolean;
  genesis_hash?: string;
  /** The wallet the node serves (`"/wallet/"`), or null on a node built without it. */
  wallet?: string | null;
}

/** One of `/api/blocks`' headers. */
interface DemoBlock {
  height: number;
  timestamp_ms?: number;
  tx_count?: number;
  proposer?: { name?: string | null; short?: string } | null;
}

type Demo =
  | { state: "off" }
  | { state: "waking" }
  | { state: "down" }
  | { state: "up"; status: DemoStatus; blocks: DemoBlock[] };

const clip = (s: unknown, n: number) => (typeof s === "string" && s.trim() ? (s.length > n ? `${s.slice(0, n)}…` : s) : undefined);

async function get<T>(path: string): Promise<T> {
  const res = await fetch(`${DEMO_NODE}/api${path}`, { signal: AbortSignal.timeout(6000), cache: "no-store" });
  if (!res.ok) throw new Error(`demo node ${res.status}`);
  return (await res.json()) as T;
}

function isStatus(s: unknown): s is DemoStatus {
  const v = s as DemoStatus | null;
  return !!v && typeof v.chain_id === "string" && typeof v.height === "number";
}

/**
 * The demo chain, polled from the browser while the tab is visible: its status about twice a
 * block, its newest blocks every few seconds. One that doesn't answer (or three times running,
 * once it has) is resting, and keeps being asked, more slowly, in case it wakes.
 */
function useDemo(): Demo {
  const [demo, setDemo] = useState<Demo>(DEMO_NODE ? { state: "waking" } : { state: "off" });
  useEffect(() => {
    if (!DEMO_NODE) return;
    let stop = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let misses = 0;
    let round = 0;
    let last: { status: DemoStatus; blocks: DemoBlock[] } | null = null;
    const tick = async () => {
      if (document.visibilityState === "hidden") return void (timer = setTimeout(tick, 2000));
      try {
        const status = await get<unknown>("/status");
        if (!isStatus(status)) throw new Error("not a Blockula node");
        const blocks = round % 3 === 0 || !last ? await get<unknown>("/blocks?limit=5").then((b) => (Array.isArray(b) ? (b as DemoBlock[]).filter((x) => typeof x?.height === "number").slice(0, 5) : [])).catch(() => last?.blocks ?? []) : last.blocks;
        misses = 0;
        last = { status, blocks };
        if (!stop) setDemo({ state: "up", ...last });
      } catch {
        misses += 1;
        if (!stop && (!last || misses >= 3)) {
          last = null;
          setDemo({ state: "down" });
        }
      }
      round += 1;
      const block = last?.status.block_time_ms ?? 2000;
      if (!stop) timer = setTimeout(tick, last ? Math.min(5000, Math.max(1000, block / 2)) : 15_000);
    };
    tick();
    return () => {
      stop = true;
      clearTimeout(timer);
    };
  }, []);
  return demo;
}

/** "Try the demo": the demo chain live, its wallet a click away, and the prompt for Claude Code. */
export function DemoSection() {
  const demo = useDemo();
  return (
    <section className="site-section" id="demo" aria-labelledby="demo-title">
      <div className="site-head">
        <span className="eyebrow">Try it now</span>
        <h2 id="demo-title" className="site-h2">Try the demo</h2>
        <p className="lead">A real Blockula chain, running right now. Make a wallet with your passkey, get free coins from the faucet, and pay someone by name. Nothing to install.</p>
      </div>
      {demo.state === "up" ? <Live status={demo.status} blocks={demo.blocks} /> : <Resting state={demo.state} />}
    </section>
  );
}

function Resting({ state }: { state: "off" | "waking" | "down" }) {
  if (state === "waking")
    return (
      <div className="card plate site-demo center" data-testid="demo-waking">
        <Spinner>Waking the demo chain…</Spinner>
      </div>
    );
  return (
    <div className="card plate site-demo center stack" data-testid="demo-resting">
      <h3 className="site-h3">The demo chain is resting</h3>
      <p className="dim">{state === "down" ? "It isn't answering just now." : "There's no demo running at the moment."} You can launch your own in a minute, on your own computer.</p>
      <a className="btn primary" href="#launch">Launch your own</a>
    </div>
  );
}

function Live({ status, blocks }: { status: DemoStatus; blocks: DemoBlock[] }) {
  const node = DEMO_NODE as string;
  const name = clip(status.name, 60) ?? clip(status.chain_id, 64) ?? "The demo chain";
  const token = clip(status.token, 12) ?? "FANG";
  const description = clip(status.description, 200);
  const served = status.wallet !== null;
  const prompt = agentPrompt({ rpc: node, chainId: status.chain_id, genesisHash: status.genesis_hash, unit: token, name: clip(status.name, 60), faucet: status.faucet });
  const figures: { k: string; v: string; testid: string }[] = [
    { k: "Unit", v: token, testid: "demo-token" },
    { k: "Validators", v: status.active_validators != null ? String(status.active_validators) : "–", testid: "demo-validators" },
    { k: "Accounts", v: status.accounts != null ? status.accounts.toLocaleString("en-US") : "–", testid: "demo-accounts" },
    { k: ".boo names", v: status.names != null ? status.names.toLocaleString("en-US") : "–", testid: "demo-names" },
  ];
  return (
    <div className="card plate site-demo stack-lg" data-testid="demo-live">
      <div className="site-demo-top">
        <div className="site-demo-chain">
          <span className="site-live" aria-hidden="true" />
          <span className="eyebrow">Live</span>
          <h3 className="site-h3" data-testid="demo-name">{name}</h3>
          {description && <p className="dim small">{description}</p>}
        </div>
        <div className="site-height" aria-label={`Block ${status.height}`}>
          <span className="figure-k">Block</span>
          <span key={status.height} className="site-height-n" data-testid="demo-height">{status.height.toLocaleString("en-US")}</span>
        </div>
      </div>

      <div className="figures-row site-figures">
        {figures.map((f) => (
          <div key={f.k}>
            <div className="figure-k">{f.k}</div>
            <div className="figure" data-testid={f.testid}>{f.v}</div>
          </div>
        ))}
      </div>

      {blocks.length > 0 && <Blocks blocks={blocks} />}

      <div className="site-demo-actions">
        {served ? (
          <a className="btn primary big" href={walletOf(node)} data-testid="demo-wallet">Open the demo wallet</a>
        ) : (
          <p className="dim" data-testid="demo-no-wallet">This node doesn&apos;t serve a wallet right now. Its explorer still shows everything.</p>
        )}
        <div className="row-gap center-row">
          <a className="btn small" href={`${node}/`} data-testid="demo-explorer">Explorer</a>
          <a className="btn small" href={`${node}/#/start/invite`} data-testid="demo-invite">Invite page</a>
        </div>
        <p className="dim small center">Your wallet lives on this device, unlocked by Face ID, Touch ID or your password manager. Demo coins are free and worth nothing, and the demo may be reset.</p>
      </div>

      <details className="landing-more site-agent">
        <summary>Bring Claude Code to the demo</summary>
        <p className="dim small">Paste this into Claude Code. It installs the Blockula MCP server pinned to this chain, makes its own wallet and gets coins from the faucet.</p>
        <div className="row-between">
          <span className="dim small">The prompt</span>
          <Copy text={prompt} label="Copy prompt" />
        </div>
        <pre className="landing-prompt" data-testid="demo-agent-prompt">{prompt}</pre>
      </details>
    </div>
  );
}

function ago(ms: number | undefined) {
  if (!ms) return "";
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  return `${Math.floor(s / 3600)}h ago`;
}

function Blocks({ blocks }: { blocks: DemoBlock[] }) {
  return (
    <div className="stack site-blocks">
      <span className="figure-k">Newest blocks</span>
      <ol className="site-block-list" data-testid="demo-blocks">
        {blocks.map((b) => (
          <li key={b.height}>
            <a href={`${DEMO_NODE}/#/block/${b.height}`} className="site-block">
              <span className="mono">#{b.height.toLocaleString("en-US")}</span>
              <span className="dim">{b.tx_count ? `${b.tx_count} ${b.tx_count === 1 ? "transaction" : "transactions"}` : "empty"}</span>
              <span className="dim small site-block-by">{clip(b.proposer?.name, 40) ?? clip(b.proposer?.short, 20) ?? ""}</span>
              <span className="dim small site-block-age">{ago(b.timestamp_ms)}</span>
            </a>
          </li>
        ))}
      </ol>
    </div>
  );
}
