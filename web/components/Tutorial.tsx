"use client";

import Link from "next/link";
import { cssVars } from "@/lib/cssvars";
import { useEffect, useState, type ReactNode } from "react";
import { agentPrompt } from "@/lib/agent-prompt";
import { BASE_PATH, SERVED } from "@/lib/config";
import { friendly } from "@/lib/errors";
import { FIRST_FANG, TUTORIAL } from "@/lib/onboarding-copy";
import { payLink } from "@/lib/paylink";
import { loadTutorial, saveTutorial, type TutorialState } from "@/lib/session";
import type { AccountView, Status } from "@/lib/types";
import { useTutorialScope, useUnlocked } from "@/lib/wallet";
import { Copy, ErrorNote, Spinner } from "./ui";
import { Unit } from "./Unit";

const ROMAN = ["I", "II", "III", "IV", "V", "VI", "VII", "VIII"];
const BUILD_ON_IT = "https://github.com/jackdishman/blockula/blob/main/docs/BUILD-ON-IT.md";
const LAUNCH_VALIDATORS = "https://github.com/jackdishman/blockula/blob/main/docs/LAUNCH.md#3-friends-become-validators";
const REPO = "https://github.com/jackdishman/blockula";

interface Task {
  id: string;
  title: ReactNode;
  why: string;
  action: ReactNode;
  /** The chain says it's done (and it's kept once it did). Otherwise the user ticks it. */
  proven: boolean;
  /** Ticked by the user: the chain can't see it happen. */
  manual?: boolean;
}

interface Stage {
  id: keyof typeof TUTORIAL.stages;
  tasks: Task[];
}

/** This chain's and account's checklist progress (lib/session), and a way to update it. */
export function useTutorial() {
  const scope = useTutorialScope();
  // Kept with the scope it was read for, so another account's or chain's ticks are never shown or written here.
  const [s, setS] = useState<{ scope: string | null; t: TutorialState }>({ scope: null, t: {} });
  useEffect(() => {
    if (!scope) return;
    setS({ scope, t: loadTutorial(scope) });
    // Ticks can be written from other pages; re-read when we come back to the tab.
    const sync = () => setS({ scope, t: loadTutorial(scope) });
    window.addEventListener("focus", sync);
    return () => window.removeEventListener("focus", sync);
  }, [scope]);
  const t = s.scope !== null && s.scope === scope ? s.t : null;
  const update = (patch: Partial<TutorialState>) => {
    if (!scope) return;
    const next = { ...loadTutorial(scope), ...patch };
    saveTutorial(scope, next);
    setS({ scope, t: next });
  };
  return [t, update] as const;
}

/**
 * Whether this chain has programs on: its `/api/fees` has a `fuel` section only then (CLAUDE.md
 * "Programs"). null until the node has answered, and on a node that doesn't.
 */
function useProgramsOn(): boolean | null {
  const { client } = useUnlocked();
  const [on, setOn] = useState<boolean | null>(null);
  useEffect(() => {
    let live = true;
    client
      .fees()
      .then((f) => live && setOn(!!f && typeof f === "object" && "fuel" in f && !!f.fuel))
      .catch(() => live && setOn(null));
    return () => {
      live = false;
    };
  }, [client]);
  return on;
}

/**
 * The checklist: a path of stages that open in order. The current stage is open, with its next
 * task expanded; finished stages fold up and later ones are only named. `state` is null until this
 * chain's and account's progress has been read.
 */
export function Tutorial({ state, update }: { state: TutorialState | null; update: (p: Partial<TutorialState>) => void }) {
  const { wallet, account, status, client, refresh } = useUnlocked();
  const programs = useProgramsOn();
  const [open, setOpen] = useState<string | null>(null);
  const [peek, setPeek] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [origin, setOrigin] = useState("");
  useEffect(() => setOrigin(`${window.location.origin}${BASE_PATH}`), []);

  const faucet = async () => {
    setBusy(true);
    setError(null);
    try {
      await wallet.faucet();
      await refresh();
    } catch (e) {
      setError(friendly(e));
    } finally {
      setBusy(false);
    }
  };

  const ctx = account && state ? { account, status, state, busy, faucet, programs, origin, rpc: client.rpc, pin: client.pin } : null;
  const stages = ctx ? buildStages(ctx) : [];
  const tasks = stages.flatMap((s) => s.tasks);
  const doneIds = new Set([...(state?.done ?? []), ...(state?.done?.includes("kit") ? ["device"] : [])]);
  const skipped = new Set(state?.skipped ?? []);
  const isDone = (t: Task) => t.proven || doneIds.has(t.id);
  const settled = (t: Task) => isDone(t) || skipped.has(t.id);
  // Remember what the chain proved, so it stays ticked (e.g. after revoking the agent you set up).
  const newlyDone = tasks.filter((t) => t.proven && !doneIds.has(t.id)).map((t) => t.id).join();
  useEffect(() => {
    if (newlyDone) update({ done: [...(state?.done ?? []), ...newlyDone.split(",")] });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [newlyDone]);

  if (!account || !state || state.dismissed) return null;
  const done = tasks.filter(isDone).length;
  const current = stages.findIndex((s) => !s.tasks.every(settled));
  // A tick or a skip moves on to the next task.
  const tick = (id: string) => {
    setOpen(null);
    update({ done: [...(state.done ?? []).filter((d) => d !== id), id], skipped: (state.skipped ?? []).filter((d) => d !== id) });
  };
  const skip = (id: string) => {
    setOpen(null);
    update({ skipped: [...(state.skipped ?? []).filter((d) => d !== id), id] });
  };
  const unskip = (id: string) => update({ skipped: (state.skipped ?? []).filter((d) => d !== id) });

  const taskList = (s: Stage, live: boolean) => {
    // The next thing to do opens by itself; the user can open another.
    const next = s.tasks.find((t) => !settled(t))?.id;
    // `open`: null follows the path, "" is every task closed, else the task the user opened.
    const shown = open !== null && (open === "" || s.tasks.some((t) => t.id === open)) ? open : live ? next : null;
    return (
      <ul className="checklist">
        {s.tasks.map((t, n) => {
          const d = isDone(t);
          const sk = !d && skipped.has(t.id);
          const expanded = shown === t.id;
          return (
            <li key={t.id} className={d ? "done" : sk ? "skipped" : ""} data-task={t.id}>
              <button className="check-row" onClick={() => setOpen(expanded && open === t.id ? "" : t.id)} aria-expanded={expanded}>
                <span className="check" aria-label={d ? "Done" : sk ? "Skipped" : undefined}>{d ? "✓" : sk ? "–" : n + 1}</span>
                <span className="grow">{t.title}</span>
                <span className="check-chevron" aria-hidden>{expanded ? "−" : "+"}</span>
              </button>
              {expanded && (
                <div className="check-body">
                  <p>{t.why}</p>
                  {!d && t.action}
                  {!d && (
                    <span className="row-gap check-tick">
                      {t.manual && <button className="link small" onClick={() => tick(t.id)}>Mark done</button>}
                      {sk ? <button className="link small" onClick={() => unskip(t.id)}>Bring it back</button> : <button className="link small dim" onClick={() => skip(t.id)}>Skip for now</button>}
                    </span>
                  )}
                </div>
              )}
            </li>
          );
        })}
      </ul>
    );
  };

  return (
    <section className="card learn" data-testid="tutorial">
      <div className="row-between">
        <h2>{TUTORIAL.heading}</h2>
        <span className="row-gap">
          <span className="dim small" data-testid="tutorial-count">{done} of {tasks.length}</span>
          <button className="link small" onClick={() => update({ dismissed: true })}>Hide</button>
        </span>
      </div>
      <div className="progress"><span ref={cssVars({ "--w": `${tasks.length ? (done / tasks.length) * 100 : 0}%` })} /></div>
      <ol className="stages">
        {stages.map((s, k) => {
          const copy = TUTORIAL.stages[s.id];
          const finished = current === -1 || k < current;
          const count = `${s.tasks.filter(isDone).length} of ${s.tasks.length}`;
          if (k === current)
            return (
              <li key={s.id} className="stage current" data-stage={s.id} aria-current="step">
                <div className="stage-head">
                  <span className="stage-num">{ROMAN[k]}</span>
                  <span className="grow"><b>{copy.title}</b> <span className="dim small">{copy.blurb}</span></span>
                  <span className="dim small">{count}</span>
                </div>
                {taskList(s, true)}
              </li>
            );
          if (finished)
            return (
              <li key={s.id} className="stage finished" data-stage={s.id}>
                <button className="stage-head" onClick={() => setPeek(peek === s.id ? null : s.id)} aria-expanded={peek === s.id}>
                  <span className="stage-num" aria-label="Done">✓</span>
                  <span className="grow">{copy.title}</span>
                  <span className="dim small">{count}</span>
                </button>
                {peek === s.id && taskList(s, false)}
              </li>
            );
          return (
            <li key={s.id} className="stage upcoming" data-stage={s.id}>
              <div className="stage-head">
                <span className="stage-num">{ROMAN[k]}</span>
                <span className="grow">{copy.title} <span className="small">{k === current + 1 ? "· up next" : ""}</span></span>
                <span className="small">{s.tasks.length} tasks</span>
              </div>
            </li>
          );
        })}
      </ol>
      <ErrorNote error={error} />
      {current === -1 && <p className="finale">{TUTORIAL.finale}</p>}
    </section>
  );
}

interface Ctx {
  account: AccountView;
  status: Status | null;
  state: TutorialState;
  busy: boolean;
  faucet: () => void;
  /** Whether the chain has programs on (null: not known yet, so the task isn't offered). */
  programs: boolean | null;
  /** This wallet's own origin, for links friends open. */
  origin: string;
  /** The node this wallet talks to. */
  rpc: string;
  pin: { chainId?: string; genesisHash?: string };
}

const LOOPBACK = /^(localhost|127(\.\d+){3}|\[::1\]|::1|0\.0\.0\.0)$/i;
const isLoopback = (url: string) => {
  try {
    return LOOPBACK.test(new URL(url).hostname);
  } catch {
    return false;
  }
};

/** A command or text to copy, with its button. */
function Snippet({ text, label = "Copy", testId }: { text: string; label?: string; testId?: string }) {
  return (
    <div className="tutorial-snippet">
      <pre className="snippet" data-testid={testId}>{text}</pre>
      <Copy text={text} label={label} />
    </div>
  );
}

function buildStages(c: Ctx): Stage[] {
  const { account, status } = c;
  const me = account.account.address;
  const name = account.names[0];
  const unit = status?.token ?? "FANG";
  const txs = account.recent_txs;
  // Any tx kind this account signed (the node's kinds: node/src/types.rs `Action::kind`).
  const signed = (...kinds: string[]) => txs.some((t) => t.from.address === me && kinds.includes(t.kind as string));
  const faucetAddr = status?.validator.address;
  const received = txs.some((t) => t.kind === "transfer" && t.to?.address === me && t.from.address !== me && t.from.address !== faucetAddr && !/faucet/i.test(t.memo ?? ""));
  const ownerKeys = account.keys.filter((k) => k.role.type === "owner").length;
  const locks = (account as AccountView & { locks?: { id: string }[] }).locks ?? [];
  const validators = (status as (Status & { active_validators?: number }) | null)?.active_validators ?? 0;
  const chainId = c.pin.chainId ?? status?.chain_id;
  const genesisHash = c.pin.genesisHash ?? status?.genesis_hash;
  const hash = genesisHash ?? "<genesis hash>";
  const home = `~/.blockula/${chainId ?? "<chain id>"}`;
  // A friend can't reach this machine's localhost: they put in an address they can reach it at (docs/LAUNCH.md "Reaching each other").
  const localNode = isLoopback(c.rpc);
  const reach = localNode ? c.rpc.replace(/\/\/[^/:]+/, "//<your address>") : c.rpc.replace(/\/+$/, "");
  const localWallet = !!c.origin && isLoopback(c.origin);
  const invite = c.origin ? payLink(c.origin, name ?? me, "1", "hello from a friend", chainId ? { chainId, genesisHash } : undefined) : null;

  return [
    {
      id: "basics",
      tasks: [
        {
          id: "fang",
          title: <>Get some <Unit /></>,
          proven: BigInt(account.balance) > 0n || txs.some((t) => t.to?.address === me),
          why: `${TUTORIAL.why.fang(unit)} ${status?.faucet ? "This chain's faucet hands it out free." : FIRST_FANG.noFaucet}`,
          action: status?.faucet ? (
            <button className="btn small" disabled={c.busy} onClick={c.faucet}>{c.busy ? <Spinner>Dripping…</Spinner> : <>Get 100 test <Unit /></>}</button>
          ) : (
            <span className="dim small" data-testid="share-to-receive">
              Someone has to send you some: share your {name ? <>name, <b>{name}</b>,</> : "address"} from <Link href="/receive">Receive</Link>.
            </span>
          ),
        },
        {
          id: "name",
          title: "Claim a .boo name",
          proven: account.names.length > 0,
          why: TUTORIAL.why.name,
          action: <Link className="btn small" href="/name">Pick a name</Link>,
        },
        {
          id: "send",
          title: "Send your first payment",
          proven: signed("transfer"),
          why: TUTORIAL.why.send,
          action: (
            <span className="row-gap">
              <Link className="btn small" href="/send">Send</Link>
              {faucetAddr && <Link className="btn small ghost" href={`/send?to=${faucetAddr}&amount=1&memo=${encodeURIComponent("my first payment")}`}>Try: 1 <Unit /> to the validator</Link>}
            </span>
          ),
        },
        {
          id: "receive",
          title: "Get paid by someone",
          proven: received,
          why: TUTORIAL.why.receive,
          action: <Link className="btn small" href="/receive">Share your pay link</Link>,
        },
      ],
    },
    {
      id: "further",
      tasks: [
        {
          id: "device",
          title: "Add a second passkey or a recovery kit",
          proven: ownerKeys > 1,
          why: TUTORIAL.why.device,
          action: (
            <span className="row-gap">
              <Link className="btn small" href="/devices#device">Link a device</Link>
              <Link className="btn small ghost" href="/devices#recovery">Make a recovery kit</Link>
            </span>
          ),
        },
        {
          id: "stake",
          title: "Stake to a validator",
          proven: signed("delegate", "register_validator"),
          why: TUTORIAL.why.stake(unit),
          action: <Link className="btn small" href="/stake">Stake</Link>,
        },
        {
          id: "vote",
          title: "Vote on a proposal, or make one",
          proven: signed("vote", "propose") || locks.some((l) => l.id.startsWith("vote:")),
          why: TUTORIAL.why.vote,
          action: <Link className="btn small" href="/governance">Open governance</Link>,
        },
        ...(c.programs
          ? [
              {
                id: "program",
                title: "Build a program",
                proven: signed("upload_code", "create_program"),
                manual: true,
                why: TUTORIAL.why.program,
                action: (
                  <>
                    <Snippet text={"blockula program upload code.wasm --interface iface.json\nblockula program create <wasm_hash> <interface_hash>\nblockula program call <address> <method>"} testId="program-cli" />
                    <Link className="btn small" href="/programs">See programs</Link>
                  </>
                ),
              },
            ]
          : []),
      ],
    },
    {
      id: "grow",
      tasks: [
        {
          id: "invite",
          title: "Invite a friend",
          proven: false,
          manual: true,
          why: TUTORIAL.why.invite,
          action: invite ? (
            <>
              <Snippet text={invite} label="Copy link" testId="invite-link" />
              {localWallet && SERVED && (
                <p className="dim small" data-testid="invite-local">
                  This link points at <code>localhost</code>, so it opens on this computer only. Put your node online and the same link works on any phone or laptop: on Tailscale, <code>tailscale serve --bg {new URL(c.rpc).port || "7070"}</code> serves it at <code>https://&lt;this machine&gt;.&lt;your tailnet&gt;.ts.net/wallet/</code> (<code>blockula launch</code> offers it). Open the wallet there, and copy the link again. Or your friend runs their own node:
                </p>
              )}
              {localWallet && !SERVED && (
                <p className="dim small" data-testid="invite-local">
                  This wallet runs on this computer only, so the link won&apos;t open on a friend&apos;s device. Put the wallet online first (<a href={BUILD_ON_IT} target="_blank" rel="noreferrer">how</a>), or have your friend run their own node and wallet:
                </p>
              )}
              {localWallet && <Snippet text={`blockula join --genesis ${reach} --genesis-hash ${hash} --home ${home}`} testId="invite-join" />}
              {localWallet && localNode && <p className="dim small">Replace &lt;your address&gt; with one they can reach this machine at, such as its Tailscale address.</p>}
            </>
          ) : (
            <Spinner>Making your link…</Spinner>
          ),
        },
        {
          id: "validator",
          title: "Help a friend become a validator",
          proven: validators > 1,
          why: TUTORIAL.why.validator,
          action: (
            <>
              <span className="dim small">On their machine:</span>
              <Snippet
                testId="validator-cli"
                text={[
                  `blockula validator init --home ${home}-validator --genesis ${reach} --genesis-hash ${hash} --public-p2p-addr <their address>:30300 --public-http-addr http://<their address>:7070`,
                  ...(status?.faucet ? [`blockula wallet faucet --to <validator address> --rpc ${reach}`] : []),
                  `blockula run --home ${home}-validator --bind 0.0.0.0`,
                  `blockula validator register --home ${home}-validator --rpc ${reach}`,
                ].join("\n")}
              />
              <span className="dim small">Then approve it on the machine that launched the chain:</span>
              <Snippet text={`blockula validator approve <validator address> --key ${home}/validator.key --rpc http://localhost:7070`} />
              <a className="small" href={LAUNCH_VALIDATORS} target="_blank" rel="noreferrer">The full guide</a>
            </>
          ),
        },
        {
          id: "agent",
          title: "Give an agent a budget",
          proven: account.keys.some((k) => k.role.type === "session"),
          why: TUTORIAL.why.agent,
          action: <Link className="btn small" href="/devices#agent">Set a budget</Link>,
        },
        {
          id: "coding",
          title: "Bring your coding agent",
          proven: false,
          manual: true,
          why: TUTORIAL.why.coding,
          action: <Snippet text={agentPrompt({ rpc: c.rpc, chainId, genesisHash, unit, name: status?.name, faucet: status?.faucet })} label="Copy prompt" testId="agent-prompt" />,
        },
      ],
    },
    {
      id: "yours",
      tasks: [
        {
          id: "layout",
          title: "Change the app's layout",
          proven: false,
          manual: true,
          why: TUTORIAL.why.layout,
          action: (
            <>
              <Snippet text={`git clone ${REPO} && cd blockula && claude`} />
              <span className="dim small">Then ask for what you want: the wallet is in web/, the node&apos;s explorer in node/src/explorer.html.</span>
            </>
          ),
        },
        {
          id: "deploy",
          title: "Put your wallet online",
          proven: false,
          manual: true,
          why: TUTORIAL.why.deploy,
          action: <a className="btn small" href={BUILD_ON_IT} target="_blank" rel="noreferrer">Read the guide</a>,
        },
      ],
    },
  ];
}
