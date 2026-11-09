"use client";

import { useEffect, useId, useState, type ReactNode } from "react";
import { Command, INSTALL, REPO } from "@/components/Landing";
import { Copy } from "@/components/ui";
import { contactUrl, listingJson, listingLink, projectRun, units, type ListedChain } from "@/lib/chains";
import { SITE_CHAINS } from "@/lib/config";
import { CONTRIBUTE_PROMPT, walletOf } from "@/lib/site";

const REGISTRY = `${REPO}/blob/main/chains/README.md`;
const RUN_A_NODE = `${REPO}/blob/main/docs/RUN-A-NODE.md`;
const GOOD_FIRST_ISSUES = `${REPO}/issues?q=is%3Aopen+label%3A%22good+first+issue%22`;
/** The template "List your chain" opens on GitHub: placeholders the checker refuses until they're filled in. */
const TEMPLATE = listingJson({
  name: "Your chain",
  chain_id: "your-chain",
  token: "YOURS",
  description: "One sentence about it.",
  rpc: "https://chain.example.org",
  genesis_hash: "<64 hex: your node's /api/status genesis_hash>",
  validators_wanted: false,
  contact: "<your GitHub handle>",
});

/** What a card reads from a listed chain's `/api/status` (node/src/api.rs `status`). */
interface ChainStatus {
  chain_id: string;
  genesis_hash?: string;
  height: number;
  active_validators?: number;
  faucet?: boolean;
  wallet?: string | null;
}

/** What it reads from `/api/validators`: how a newcomer gets in (node/src/validator_cli.rs `way_in`). */
interface ChainSet {
  active?: unknown[];
  min_bond?: string;
  admission?: { members?: unknown[]; quorum?: number } | null;
  churn_limit?: number;
  admins?: unknown[];
}

type Live = { state: "checking" } | { state: "resting" } | { state: "mismatch" } | { state: "live"; status: ChainStatus; set: ChainSet | null };

async function read<T>(rpc: string, path: string): Promise<T> {
  const res = await fetch(`${rpc}/api${path}`, { signal: AbortSignal.timeout(6000), cache: "no-store" });
  if (!res.ok) throw new Error(`${res.status}`);
  return (await res.json()) as T;
}

/**
 * A listed chain's node, read from the visitor's browser while the tab is visible: its status and
 * validator set every half minute. One that doesn't answer, or answers for another chain than
 * its listing names, says so.
 */
function useLive(c: ListedChain): Live {
  const [live, setLive] = useState<Live>({ state: "checking" });
  useEffect(() => {
    let stop = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      if (document.visibilityState === "hidden") return void (timer = setTimeout(tick, 5000));
      let next: Live;
      try {
        const s = await read<ChainStatus>(c.rpc, "/status");
        if (!s || typeof s.chain_id !== "string" || typeof s.height !== "number") throw new Error("not a node");
        if (s.chain_id !== c.chain_id || s.genesis_hash !== c.genesis_hash) next = { state: "mismatch" };
        else next = { state: "live", status: s, set: await read<ChainSet>(c.rpc, "/validators").catch(() => null) };
      } catch {
        next = { state: "resting" };
      }
      if (stop) return;
      setLive(next);
      timer = setTimeout(tick, next.state === "live" ? 30_000 : 60_000);
    };
    tick();
    return () => {
      stop = true;
      clearTimeout(timer);
    };
  }, [c]);
  return live;
}

/** "Join a chain": the directory, how to list one, and the contributor prompt. */
export function ChainsSection() {
  return (
    <section className="site-section" id="chains" aria-labelledby="chains-title">
      <div className="site-head">
        <span className="eyebrow">Join a chain</span>
        <h2 id="chains-title" className="site-h2">Chains you can join</h2>
        <p className="lead">Chains people run and opened to everyone. Use one from its wallet, or help run it as a validator.</p>
      </div>
      {SITE_CHAINS.length ? (
        <>
          <ul className="site-chains" data-testid="chain-list">
            {SITE_CHAINS.map((c) => (
              <ChainCard key={c.chain_id} chain={c} />
            ))}
          </ul>
          <p className="center dim small site-chains-note">
            {SITE_CHAINS.some((c) => projectRun(c.rpc)) ? "Apart from the ones marked as run by the project, the project doesn't run or check these chains." : "The project doesn't run or check these chains."} A node decides what its chain looks like to you, and the coins have no value.
          </p>
        </>
      ) : (
        <Empty />
      )}
      <div className="site-chains-more">
        <ListYours />
        <HelpBuild />
      </div>
    </section>
  );
}

function Empty() {
  return (
    <div className="card plate site-chains-empty stack" data-testid="chains-empty">
      <div className="center">
        <h3 className="site-h3">No chains listed yet</h3>
        <p className="dim">Start one and list it: yours would be the first.</p>
      </div>
      <ol className="site-list">
        <li>Start it, reached over <b>the open internet</b> (or Tailscale Funnel, for wallets only).</li>
        <li>Serve it over HTTPS on a name of its own, so wallets can make passkeys.</li>
        <li>Open the <b>List it on blockula.app</b> link <code>blockula create</code> prints, and send the pull request.</li>
      </ol>
      <Command text={INSTALL} />
      <Command text="blockula create" />
    </div>
  );
}

function ListYours() {
  return (
    <div className="card stack site-mini" id="list" data-testid="chains-list-yours">
      <span className="eyebrow">List your chain</span>
      <h3 className="site-h3">One file, one pull request</h3>
      <p className="dim small">
        A chain gets a card here from a file in the repo, <code>chains/&lt;chain id&gt;.json</code>. It needs an <code>https://</code> node anyone can reach. <code>blockula create</code> prints a link with yours filled in.
      </p>
      <div className="row-gap">
        <a className="btn small" href={listingLink("your-chain", TEMPLATE)} data-testid="list-template">Start the file on GitHub</a>
        <a className="btn small ghost" href={REGISTRY}>What gets listed</a>
      </div>
    </div>
  );
}

function HelpBuild() {
  return (
    <div className="card stack site-mini" id="contribute" data-testid="help-build">
      <div className="row-between site-wrap">
        <div>
          <span className="eyebrow">Help build Blockula</span>
          <h3 className="site-h3">With your coding agent</h3>
        </div>
        <Copy text={CONTRIBUTE_PROMPT} label="Copy prompt" />
      </div>
      <p className="dim small">Paste this into Claude Code in an empty folder. People and agents are both welcome.</p>
      <details className="landing-more">
        <summary>Read the prompt</summary>
        <pre className="landing-prompt" data-testid="contribute-prompt">{CONTRIBUTE_PROMPT}</pre>
      </details>
      <div className="row-gap">
        <a className="btn small" href={GOOD_FIRST_ISSUES}>Good first issues</a>
        <a className="btn small ghost" href={`${REPO}/blob/main/CONTRIBUTING.md`}>Contributing guide</a>
      </div>
    </div>
  );
}

function ChainCard({ chain: c }: { chain: ListedChain }) {
  const live = useLive(c);
  const [open, setOpen] = useState(false);
  const id = useId();
  const takes = c.validators_wanted && !!c.p2p;
  const usable = live.state !== "mismatch";
  return (
    <li className="card site-chain stack" data-testid={`chain-${c.chain_id}`} data-state={live.state}>
      <div className="site-chain-top">
        <div className="site-chain-name">
          <h3 className="site-h3">{c.name}</h3>
          <span className="pill site-ticker" data-testid="chain-token">{c.token}</span>
          {projectRun(c.rpc) && <span className="pill site-ours" data-testid="chain-ours">Run by the Blockula project</span>}
          {takes && <span className="pill site-wants" data-testid="chain-wants">Wants validators</span>}
        </div>
        <Status live={live} token={c.token} />
      </div>
      <p className="dim">{c.description}</p>
      {usable && (
        <div className="row-gap">
          <a className="btn small primary" href={walletOf(c.rpc)} data-testid="chain-wallet">Open wallet</a>
          <a className="btn small" href={`${c.rpc}/`} data-testid="chain-explorer">Explorer</a>
          {takes && (
            <button type="button" className="btn small ghost" aria-expanded={open} aria-controls={`${id}-steps`} data-testid="chain-validate" onClick={() => setOpen((o) => !o)}>
              {open ? "Hide the steps" : "Become a validator"}
            </button>
          )}
        </div>
      )}
      {usable && !takes && <p className="dim small" data-testid="chain-no-validators">This chain isn&apos;t taking validators from outside{c.rpc.endsWith(".ts.net") ? ": it's on a tailnet, which other validators can't reach" : ""}. Its wallet is open to everyone.</p>}
      {usable && takes && open && <ValidatorSteps id={`${id}-steps`} chain={c} live={live} />}
    </li>
  );
}

function Status({ live, token }: { live: Live; token: string }) {
  if (live.state === "checking") return <span className="dim small site-chain-status" data-testid="chain-status">Checking…</span>;
  if (live.state === "resting")
    return (
      <span className="dim small site-chain-status" data-testid="chain-status">
        <span className="site-dot rest" aria-hidden="true" /> Resting: not answering just now
      </span>
    );
  if (live.state === "mismatch")
    return (
      <span className="error small site-chain-status" role="alert" data-testid="chain-status">
        Its node reports another chain than this listing. Don&apos;t use it until that&apos;s fixed.
      </span>
    );
  const { status: s, set } = live;
  const validators = set?.active?.length ?? s.active_validators;
  return (
    <span className="small site-chain-status" data-testid="chain-status">
      <span className="site-dot" aria-hidden="true" /> Live · block {s.height.toLocaleString("en-US")}
      {validators != null && ` · ${validators} validator${validators === 1 ? "" : "s"}`}
      {set?.min_bond && <span className="dim"> · bond {units(set.min_bond)} {token}</span>}
    </span>
  );
}

/** A host a visitor typed for their own machine, if it's one a command can hold. */
const hostOf = (s: string) => (/^[a-z0-9]([a-z0-9.-]{0,251}[a-z0-9])?$/i.test(s.trim()) ? s.trim().toLowerCase() : null);

/**
 * How to run a validator for `chain`, as copyable commands with its node, genesis hash and home
 * filled in: the same steps as docs/RUN-A-NODE.md, "Validate someone else's chain". The commands
 * are the CLI's (node/src/main.rs `join`, node/src/validator_cli.rs `init`, `register`, `admit`,
 * `approve`; `wallet faucet`); what the chain's set needs comes from its `/api/validators`.
 */
function ValidatorSteps({ id, chain: c, live }: { id: string; chain: ListedChain; live: Live }) {
  const [hostInput, setHostInput] = useState("");
  const host = hostOf(hostInput) ?? "<your host>";
  const status = live.state === "live" ? live.status : null;
  const set = live.state === "live" ? live.set : null;
  const home = `~/.blockula/${c.chain_id}-validator`;
  const me = "<your validator address>";
  const bond = set?.min_bond ? `${units(set.min_bond)} ${c.token}` : `the chain's bond (in ${c.token})`;
  const ownerHome = `~/.blockula/${c.chain_id}`;
  const admit = `blockula validator admit ${me} --key ${ownerHome}/validator.key --rpc http://localhost:7070`;
  const approve = `blockula validator approve ${me} --key ${ownerHome}/validator.key --rpc http://localhost:7070`;
  const contact = (
    <a href={contactUrl(c.contact)} data-testid="chain-contact">
      {c.contact}
    </a>
  );
  // A set of one or two takes a newcomer only once its members admit it; one of three only by an
  // admin's approval; from four on, by stake (staking::churn_limit).
  const members = set?.admission?.members?.length;
  let wayIn: ReactNode;
  if (set?.admission)
    wayIn = (
      <Step n={7} title="Ask to be admitted" testid="step-admit">
        <p>
          This chain has {members ?? "one or two"} validator{members === 1 ? "" : "s"}, and every one is needed for each block, so a newcomer joins only once {set.admission.quorum && members && set.admission.quorum < members ? `${set.admission.quorum} of them admit` : "they admit"} it. Send your validator address to the owner ({contact}), check it with them, and ask them to run:
        </p>
        <Command text={admit} />
        <p className="dim small">Keep your node running: it tells the chain it&apos;s ready by itself, and joins at the end of that epoch.</p>
      </Step>
    );
  else if (set && set.churn_limit === 0)
    wayIn = (
      <Step n={7} title="Ask to be approved" testid="step-approve">
        <p>A set of three takes newcomers only by the admin&apos;s approval. Send your validator address to the owner ({contact}) and ask them to run:</p>
        <Command text={approve} />
      </Step>
    );
  else if (set)
    wayIn = (
      <Step n={7} title="Wait for the next epoch" testid="step-stake">
        <p>This set takes newcomers by stake: yours joins at an epoch boundary once its stake ranks in the set, a few newcomers an epoch. <code>register</code> said where it stands. Questions: {contact}.</p>
      </Step>
    );
  else
    wayIn = (
      <Step n={7} title="Get let in" testid="step-unknown">
        <p><code>register</code> says how this chain lets you in. With one or two validators, the owner ({contact}) admits you:</p>
        <Command text={admit} />
        <p className="dim small">With three, they approve you instead (<code>blockula validator approve</code>). From four on, a validator joins by stake.</p>
      </Step>
    );

  return (
    <div id={id} className="site-validate stack" data-testid="validator-steps">
      <div className="site-validate-intro">
        <p>
          <b>A validator takes turns making blocks.</b> You need a computer that stays on and online, with TCP 30300 (validators) and 7171 (your node) open to the internet: forward them on your router. One that sleeps or drops off misses its turns and gets jailed.
        </p>
        <p className="dim small">Never run two copies of one <code>validator.key</code>: signing twice bans it for good. Setting it up as a service: <a href={RUN_A_NODE}>Run a node</a>.</p>
      </div>
      <label className="field">
        <span>Your machine&apos;s public IP or DNS name</span>
        <input className="mono" autoComplete="off" autoCapitalize="none" spellCheck={false} placeholder="my-box.example.org" value={hostInput} onChange={(e) => setHostInput(e.target.value)} data-testid="validator-host" />
      </label>
      <ol className="site-validate-steps">
        <Step n={1} title="Install Blockula">
          <Command text={INSTALL} />
        </Step>
        <Step n={2} title="Run your own node of the chain">
          <p>It checks the chain against its genesis hash and keeps a copy. It&apos;s also your validator&apos;s public address for reads. Leave it running.</p>
          <Command text={`blockula join --genesis ${c.rpc} --genesis-hash ${c.genesis_hash} --home ~/.blockula/${c.chain_id} --bind 0.0.0.0`} />
        </Step>
        <Step n={3} title="Make the validator">
          <p>In another terminal. It prints your validator address (<code>boo1…</code>).</p>
          <Command text={`blockula validator init --home ${home} --genesis ${c.rpc} --genesis-hash ${c.genesis_hash} --public-p2p-addr ${host}:30300 --public-http-addr http://${host}:7171`} />
        </Step>
        <Step n={4} title="Get coins for the bond">
          {status?.faucet === false ? (
            <p data-testid="step-bond-ask">This chain has no faucet. Ask the owner ({contact}) to send {bond}, plus a little for fees, to your validator address.</p>
          ) : (
            <>
              <p data-testid="step-bond-faucet">The bond is {bond}, plus a little for fees. {status?.faucet ? "The chain's faucet gives 100 at a time:" : "If the chain has a faucet, it gives 100 at a time (otherwise ask the owner):"}</p>
              <Command text={`blockula wallet faucet --to ${me} --rpc ${c.rpc}`} />
            </>
          )}
        </Step>
        <Step n={5} title="Start the validator">
          <Command text={`blockula run --home ${home} --p2p-bind 0.0.0.0`} />
        </Step>
        <Step n={6} title="Register it">
          <p>It bonds {bond} and says how this chain lets you in.</p>
          <Command text={`blockula validator register --home ${home} --rpc ${c.rpc}`} />
        </Step>
        {wayIn}
      </ol>
    </div>
  );
}

function Step({ n, title, children, testid }: { n: number; title: string; children: ReactNode; testid?: string }) {
  return (
    <li className="site-validate-step" data-testid={testid}>
      <span className="site-validate-n" aria-hidden="true">{n}</span>
      <div className="stack">
        <h4 className="site-h4">{title}</h4>
        {children}
      </div>
    </li>
  );
}
