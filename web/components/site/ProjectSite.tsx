"use client";

import type { ReactNode } from "react";
import { Apparition } from "@/components/Haunt";
import { Command, INSTALL, LAUNCH_GUIDE, REPO, Text, USES } from "@/components/Landing";
import { Ornament, type OrnamentKind } from "@/components/Ornament";
import { ChainsSection } from "./Chains";
import { ConnectSection } from "./Connect";
import { DemoSection } from "./Demo";

const USE_CASES = `${REPO}/blob/main/docs/USE-CASES.md`;
const BUILD_ON_IT = `${REPO}/blob/main/docs/BUILD-ON-IT.md`;

const STEPS: { kind: OrnamentKind; title: string; body: ReactNode }[] = [
  { kind: "candle", title: "Launch it", body: <><code>blockula launch</code> asks a name and a ticker, then starts your chain on your computer.</> },
  { kind: "bat", title: "Invite friends", body: "Send them one link. Their phone or laptop opens the chain's wallet: nothing to install." },
  { kind: "key", title: "Pay by name", body: <>Each makes a wallet with a passkey, gets coins from the faucet, and pays <code>sam.boo</code>, not a long address.</> },
  { kind: "moon", title: "Make it yours", body: "Add WebAssembly programs, give Claude a spending budget, or change the apps with Claude Code." },
];

const INSIDE: { title: string; body: string }[] = [
  { title: "Passkey wallets", body: "Face ID or your password manager unlocks it. No seed phrase to lose." },
  { title: ".boo names", body: "Pay `alice.boo`. Names are readable, and they're yours for a year at a time." },
  { title: "Post-quantum", body: "Every account, vote and block is signed with ML-DSA-65 (NIST FIPS 204)." },
  { title: "An explorer in every node", body: "Every block, account and name, on a page your friends can open." },
  { title: "Agents welcome", body: "An MCP server for Claude, and session keys with limits the chain enforces." },
  { title: "Light", body: "An idle validator uses about 80 MB of memory. Old laptops are fine." },
];

/** The project site's page (NEXT_PUBLIC_SITE=project): blockula.app. */
export function ProjectSite() {
  return (
    <main className="site" id="top">
      <section className="site-hero center" aria-labelledby="hero-title">
        <Apparition kind="bat" burst={5} />
        <span className="eyebrow">Blockula</span>
        <h1 id="hero-title" className="site-h1">Your own little blockchain, for you and your friends</h1>
        <p className="lead site-lead">Launch one in a minute. Friends join with a link, make a wallet with a passkey, and pay each other by name.</p>
        <div className="site-cta">
          <a className="btn primary big" href="#demo">Try the demo</a>
          <a className="btn big" href="#launch">Launch your own</a>
        </div>
        <p className="site-tags caps">Free and open source · Post-quantum · No seed phrases</p>
      </section>

      <DemoSection />
      <ConnectSection />

      <section className="site-section" id="how" aria-labelledby="how-title">
        <div className="site-head">
          <span className="eyebrow">How it works</span>
          <h2 id="how-title" className="site-h2">From nothing to a chain with friends</h2>
        </div>
        <ol className="site-steps">
          {STEPS.map((s, i) => (
            <li key={s.title} className="site-step">
              <span className="site-step-n" aria-hidden="true">{["I", "II", "III", "IV"][i]}</span>
              <Ornament kind={s.kind} className="site-step-art" />
              <h3 className="site-h3">{s.title}</h3>
              <p className="dim">{s.body}</p>
            </li>
          ))}
        </ol>
      </section>

      <section className="site-section" aria-labelledby="uses-title">
        <div className="site-head">
          <span className="eyebrow">What it&apos;s for</span>
          <h2 id="uses-title" className="site-h2">What a small chain is good for</h2>
        </div>
        <div className="site-grid">
          {USES.map((u) => (
            <div key={u.title} className="site-tile">
              <h3 className="site-h3">{u.title}</h3>
              <p className="dim"><Text>{u.body}</Text></p>
            </div>
          ))}
        </div>
        <p className="center dim small"><a href={USE_CASES}>More ideas, with the commands to try</a></p>
      </section>

      <section className="site-section" aria-labelledby="inside-title">
        <div className="site-head">
          <span className="eyebrow">In the box</span>
          <h2 id="inside-title" className="site-h2">Everything a chain needs</h2>
        </div>
        <div className="site-grid site-inside">
          {INSIDE.map((u) => (
            <div key={u.title} className="site-tile">
              <h3 className="site-h3">{u.title}</h3>
              <p className="dim"><Text>{u.body}</Text></p>
            </div>
          ))}
        </div>
      </section>

      <section className="site-section" id="launch" aria-labelledby="launch-title">
        <div className="card plate stack site-launch">
          <Apparition kind="candle" />
          <div className="center">
            <span className="eyebrow">Launch your own</span>
            <h2 id="launch-title" className="site-h2">Two commands</h2>
            <p className="dim">On macOS or Linux. The first installs Blockula; the second asks four questions, each with a default, and starts your chain.</p>
          </div>
          <Command text={INSTALL} />
          <Command text="blockula launch" />
          <p className="dim small">
            Then open <code>http://localhost:7070/wallet/</code> for your wallet, and share the invite link it gives you. <a href={LAUNCH_GUIDE}>The launch guide</a> has the rest, and <a href={BUILD_ON_IT}>Build on it</a> shows how to change everything with Claude Code.
          </p>
        </div>
      </section>

      <ChainsSection />
    </main>
  );
}
