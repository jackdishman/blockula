"use client";

import Link from "next/link";
import type { ReactNode } from "react";
import { Apparition } from "@/components/Haunt";
import { Flow, type FeesView, type ParamsView } from "@/components/landing/Flow";
import { Copy } from "@/components/ui";
import { agentPrompt } from "@/lib/agent-prompt";
import { DEFAULT_NETWORK, RP_ID, RPC, SERVED, walletUrl } from "@/lib/config";
import { useNode } from "@/lib/explorer/node";
import { useWallet } from "@/lib/wallet";

export const REPO = "https://github.com/jackdishman/blockula";
export const INSTALL = `curl -fsSL ${REPO}/releases/latest/download/install.sh | sh`;
const CREATE = "blockula create";
const DEV = "blockula dev";
const BUILD_ON_IT = `${REPO}/blob/main/docs/BUILD-ON-IT.md`;
/** The guide to starting a chain of your own and inviting friends. */
export const CHAIN_GUIDE = `${REPO}/blob/main/docs/CREATE.md`;

const LOCAL = /^(localhost|127\.0\.0\.1|\[::1\])$/;
const isLocalUrl = (u: string) => {
  try {
    return LOCAL.test(new URL(u).hostname);
  } catch {
    return false;
  }
};

/** `code` spans in plain copy, rendered as code. */
export function Text({ children }: { children: string }) {
  return <>{children.split("`").map((part, i) => (i % 2 ? <code key={i}>{part}</code> : part))}</>;
}

export function Command({ text, label }: { text: string; label?: string }) {
  return (
    <div className="landing-cmd">
      <code>{text}</code>
      <Copy text={text} label={label} />
    </div>
  );
}

export const USES: { title: string; body: string }[] = [
  { title: "A friends' IOU ledger", body: "Pay `sam.boo` back for pizza, with a memo saying why. Everyone sees the same history." },
  { title: "A shared kitty", body: "A multisig account that spends only when, say, 2 of 3 of you approve." },
  { title: "Group decisions", body: "Put a question up as a proposal; holders vote with their coins." },
  { title: "Bounties", body: "Hunts escrow coins on a GitHub issue and pay whoever's PR fixes it." },
  { title: "A notary log", body: "Send yourself a file's hash as a memo: the block dates it for good." },
  { title: "Agents on a budget", body: "Give a coding agent a key with a spending limit the chain enforces." },
];

/** What a first visit to `/` shows: the chain this wallet is talking to, and a short path of things to do on it. */
export function Landing() {
  const { status, chain } = useWallet();
  const fees = useNode<FeesView>(status ? "/fees" : null, { every: 30_000 });
  const params = useNode<ParamsView>(status ? "/params" : null, { every: 60_000 });
  const down = !status && chain.state !== "checking";
  const token = status?.token ?? "FANG";
  const name = status?.name || status?.chain_id;
  // `undefined` until /api/fees answers: programs are on only when it has a `fuel` section.
  const programs = !status ? undefined : fees.data ? !!fees.data.fuel : fees.error ? false : undefined;

  return (
    <main className="narrow stack-lg landing">
      <div className="center">
        <Apparition kind="moon" />
        <span className="eyebrow">Blockula</span>
        {status ? (
          <>
            <h1 data-testid="landing-chain">{name}</h1>
            <p className="lead">
              You&apos;re looking at <b>{name}</b>, a chain whose unit is <b>{token}</b>. Here&apos;s what to do on it.
            </p>
            <span className="landing-meta mono">
              {status.chain_id} · block {status.height.toLocaleString("en-US")}
            </span>
          </>
        ) : (
          <>
            <h1>Start your own quantum-safe ledger</h1>
            <p className="lead">One command names a chain and starts it on your machine. Friends&apos; computers join it.</p>
          </>
        )}
      </div>

      {down && (
        <section className="card stack" data-testid="landing-no-chain">
          <h2>No chain is running here yet</h2>
          <p className="dim">Install Blockula and start one (macOS and Linux):</p>
          <Command text={INSTALL} />
          <Command text={CREATE} />
          <p className="dim small">
            Or <code>{DEV}</code> for a throwaway one to try things on. Then reload this page. <a href={CHAIN_GUIDE}>The guide</a> has the rest.
          </p>
        </section>
      )}

      <section className="stack" aria-labelledby="get-started">
        <h2 id="get-started">Get started</h2>
        <ol className="landing-path">
          <Step n={1} title="Make a wallet" action={<Link className="btn primary small" href="/welcome">Make a wallet</Link>}>
            A passkey unlocks it: Face ID or your password manager. No seed phrase.
          </Step>
          <Step n={2} title="Get coins, send one" action={<Link className="btn small" href="/send">Send</Link>}>
            {status?.faucet === false ? "Ask the chain's creator for some" : "The faucet fills a new wallet"}. Pay a name like <code>alice.boo</code>, not an address.
          </Step>
          <Step n={3} title="Invite a friend" action={<a className="btn small" href="#invite">Invite</a>}>
            One link to share, and how they join and become a validator.
          </Step>
          <Step n={4} title="Bring your coding agent" action={<a className="btn small" href="#agent">Get the prompt</a>}>
            Paste one prompt into Claude Code: it gets a wallet on this chain.
          </Step>
          <Step n={5} title="Build a program" action={programs ? <Link className="btn small" href="/programs">Programs</Link> : null} testid="step-programs">
            <Programs on={programs} />
          </Step>
          <Step n={6} title="Make it yours" action={<a className="btn small" href={BUILD_ON_IT}>Build on it</a>}>
            Everything here, this wallet and the node&apos;s explorer, can be changed with Claude Code.
          </Step>
        </ol>
      </section>

      <Invite />
      <Agent />

      <section className="stack" aria-labelledby="flow-title">
        <h2 id="flow-title">Where {token} goes</h2>
        <Flow params={params.data} fees={fees.data} faucet={status?.faucet} blockMs={status?.block_time_ms} />
      </section>

      <section className="stack" aria-labelledby="uses">
        <h2 id="uses">What a chain of your own is good for</h2>
        <div className="landing-uses">
          {USES.map((u) => (
            <div key={u.title} className="landing-use">
              <h3>{u.title}</h3>
              <p className="dim small"><Text>{u.body}</Text></p>
            </div>
          ))}
        </div>
      </section>

      <p className="dim small center">
        Blockula isn&apos;t externally audited. <a href={REPO}>Source on GitHub</a>.
      </p>
    </main>
  );
}

function Step({ n, title, action, children, testid }: { n: number; title: string; action: ReactNode; children: ReactNode; testid?: string }) {
  return (
    <li className="landing-step" data-testid={testid}>
      <span className="landing-n" aria-hidden="true">{n}</span>
      <div className="landing-step-body">
        <strong>{title}</strong>
        <span className="dim">{children}</span>
      </div>
      {action && <div className="landing-step-action">{action}</div>}
    </li>
  );
}

/** Step 5: on chains with programs, the three CLI calls; elsewhere, how a chain gets them. */
function Programs({ on }: { on: boolean | undefined }) {
  if (on)
    return (
      <span className="stack landing-programs" data-state="on">
        <span>Programs are on here: WebAssembly anyone can upload and call.</span>
        <Command text="blockula program upload code.wasm --interface iface.json" />
        <Command text="blockula program create <wasm_hash> <interface_hash>" />
        <Command text="blockula program call <address> <method>" />
      </span>
    );
  return (
    <span className="landing-programs" data-state={on === false ? "off" : "unknown"}>
      {on === false ? "Programs are off on this chain. " : "WebAssembly programs are off unless a chain is made with them. "}
      A new chain gets them with <code>blockula create --programs</code> (or <code>blockula dev --programs</code>); an existing one only by a governance vote.
    </span>
  );
}

/**
 * The invite: one link to this wallet when a friend's browser can use it (HTTPS, on the host its
 * passkeys are bound to), and in every case the command their own node joins with.
 */
function Invite() {
  return SERVED ? <ServedInvite /> : <HostedInvite />;
}

/** The commands a friend's own node joins with, and their validator, after the link. */
function JoinCommands({ node }: { node: string }) {
  const { status } = useWallet();
  const id = status?.chain_id ?? "<chain id>";
  const hash = status?.genesis_hash ?? "<genesis hash>";
  return (
    <>
      <details className="landing-more">
        <summary>Or they run their own node</summary>
        <p className="dim small">Their node checks every block itself, and serves them this wallet too.</p>
        <Command text={INSTALL} />
        <Command text={`blockula join --genesis ${node} --genesis-hash ${hash} --home ~/.blockula/${id}`} label="Copy join command" />
      </details>
      <ValidatorSteps node={node} />
    </>
  );
}

/**
 * The served wallet's invite (lib/config.ts SERVED): the link is this wallet, `<origin>/wallet/`.
 * Over HTTPS it's all a friend needs; on localhost it opens on this machine only, and over plain
 * http elsewhere it opens but passkeys don't, so each says what to do.
 */
function ServedInvite() {
  const link = `${walletUrl()}/`;
  const host = window.location.hostname;
  const local = LOCAL.test(host);
  const port = window.location.port || (window.location.protocol === "https:" ? "443" : "80");
  const node = local ? `http://<your address>:${port}` : RPC;
  return (
    <section className="card stack landing-invite" id="invite" aria-labelledby="invite-title">
      <h2 id="invite-title">Invite a friend</h2>
      {local ? (
        <>
          <p data-testid="invite-local">
            Friends open this wallet from your node. Here it&apos;s at <code>localhost</code>, which only this computer can open, so put your node online first. On Tailscale:
          </p>
          <Command text={`tailscale serve --bg ${port}`} />
          <p className="dim small">
            Then send them <code>https://&lt;this machine&gt;.&lt;your tailnet&gt;.ts.net/wallet/</code>: they open it, make a passkey wallet and can pay right away. <code>blockula create</code> offers to set this up (Tailscale, then HTTPS). <a href={CHAIN_GUIDE}>Other ways to reach each other</a>.
          </p>
        </>
      ) : (
        <>
          <p>Send them this link. They open it on their phone or laptop, make a wallet with a passkey, and can pay right away.</p>
          <Command text={link} label="Copy link" />
          {!window.isSecureContext && (
            <p className="dim small" data-testid="invite-http">
              This is plain <code>http://</code>, so their browser won&apos;t make a passkey here (it can keep a browser-only key for testing). For passkeys, serve your node over HTTPS: on Tailscale, <code>tailscale serve --bg {port}</code>, and share <code>https://&lt;this machine&gt;.&lt;your tailnet&gt;.ts.net/wallet/</code>.
            </p>
          )}
          <p className="dim small">
            Want them to pay you first? <Link href="/receive">Receive</Link> makes a payment link to you.
          </p>
        </>
      )}
      <JoinCommands node={node} />
    </section>
  );
}

/** A hosted wallet's invite (`npm run wallet`, or a deployment of web/). */
function HostedInvite() {
  const { status } = useWallet();
  const origin = window.location.origin;
  const host = window.location.hostname;
  const link = `${origin}/welcome`;
  const shareable = window.isSecureContext && !LOCAL.test(host) && (host === RP_ID || host.endsWith(`.${RP_ID}`));
  const nodeLocal = isLocalUrl(RPC);
  const node = nodeLocal ? "http://<your address>:7070" : RPC;
  // A friend's browser starts on this deployment's default node; another one they pick in the header.
  const pickNode = RPC !== DEFAULT_NETWORK.rpc.replace(/\/$/, "") || nodeLocal;
  const id = status?.chain_id ?? "<chain id>";
  const join = `blockula join --genesis ${node} --genesis-hash ${status?.genesis_hash ?? "<genesis hash>"} --home ~/.blockula/${id}`;
  return (
    <section className="card stack landing-invite" id="invite" aria-labelledby="invite-title">
      <h2 id="invite-title">Invite a friend</h2>
      {shareable ? (
        <>
          <p>Send them this link. They make a wallet and can transact right away.</p>
          <Command text={link} label="Copy link" />
          {pickNode && (
            <p className="dim small">
              In the wallet&apos;s header they choose <b>Custom node</b> and enter <code>{node}</code>.
            </p>
          )}
        </>
      ) : (
        <>
          <p data-testid="invite-local">
            This wallet runs at <code>{host}</code>, so a link to it opens only on this computer. A friend joins with their own node instead:
          </p>
          <Command text={INSTALL} />
          <Command text={join} label="Copy join command" />
          <p className="dim small">
            Then they run the wallet against their node (<a href={CHAIN_GUIDE}>Apps, in the guide</a>). Serve this wallet over HTTPS on your tailnet and this box becomes a link to share.
          </p>
        </>
      )}
      <ValidatorSteps node={node} />
    </section>
  );
}

function ValidatorSteps({ node }: { node: string }) {
  const { status } = useWallet();
  const id = status?.chain_id ?? "<chain id>";
  return (
    <details className="landing-more">
      <summary>Make them a validator</summary>
      <p className="dim small">On their machine: a validator home, a bond from the faucet, then start and register it.</p>
      <Command text={`blockula validator init --home ~/.blockula/${id}-validator --genesis ${node} --genesis-hash ${status?.genesis_hash ?? "<genesis hash>"} --public-p2p-addr <their address>:30300 --public-http-addr http://<their address>:7070`} />
      <Command text={`blockula wallet faucet --to <validator address> --rpc ${node}`} />
      <Command text={`blockula run --home ~/.blockula/${id}-validator --bind 0.0.0.0`} />
      <Command text={`blockula validator register --home ~/.blockula/${id}-validator --rpc ${node}`} />
      <p className="dim small">Then you approve it, and it starts taking turns at the next epoch:</p>
      <Command text={`blockula validator approve <validator address> --key ~/.blockula/${id}/validator.key --rpc http://localhost:7070`} />
      <p className="dim small">
        <code>~/.blockula/{id}/README.md</code> on your machine has these with your addresses filled in.
      </p>
    </details>
  );
}

/** The paste-in prompt for a coding agent, pinned to this chain (CLAUDE.md, "Contribute to Blockula"). */
function Agent() {
  const { status } = useWallet();
  const prompt = agentPrompt(status ? { rpc: RPC, chainId: status.chain_id, genesisHash: status.genesis_hash, unit: status.token, name: status.name, faucet: status.faucet } : null);
  return (
    <section className="card stack" id="agent" aria-labelledby="agent-title">
      <div className="row-between">
        <h2 id="agent-title">For your coding agent</h2>
        <Copy text={prompt} label="Copy prompt" />
      </div>
      <pre className="landing-prompt" data-testid="agent-prompt">{prompt}</pre>
      {isLocalUrl(RPC) && <p className="dim small">From another computer, put your node&apos;s address in place of localhost.</p>}
    </section>
  );
}
