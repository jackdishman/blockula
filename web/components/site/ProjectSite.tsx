"use client";

import type { ReactNode } from "react";
import { Apparition } from "@/components/Haunt";
import { CHAIN_GUIDE, Command, INSTALL, REPO, Text, USES } from "@/components/Landing";
import { Ornament, type OrnamentKind } from "@/components/Ornament";
import { ChainsSection } from "./Chains";
import { ConnectSection } from "./Connect";
import { DemoSection } from "./Demo";

const USE_CASES = `${REPO}/blob/main/docs/USE-CASES.md`;
const BUILD_ON_IT = `${REPO}/blob/main/docs/BUILD-ON-IT.md`;
const AUDIT = `${REPO}/blob/main/docs/AUDIT-2026-10.md`;

/** What sets it apart, each in plain words first and the technical name second. */
const PERKS: { title: string; body: ReactNode }[] = [
  {
    title: "Ready for quantum computers",
    body: (
      <>
        A signature proves a payment is really yours. Bitcoin&apos;s and Ethereum&apos;s are built on math a big enough quantum computer could undo. Blockula signs every account, vote and block with <b>ML-DSA-65</b> (NIST FIPS 204), designed to hold up against one, and its validators talk over a hybrid X25519 + ML-KEM-768 channel.
      </>
    ),
  },
  {
    title: "Final in about two seconds",
    body: (
      <>
        No mining and no waiting for &ldquo;confirmations&rdquo;. The validators vote on each block (simplex BFT consensus), and once it&apos;s in, it&apos;s in for good: no reorgs, nothing to undo.
      </>
    ),
  },
  {
    title: "Runs on what you already own",
    body: "An idle validator uses about 120 MB of memory. A laptop, a Mac mini in a closet or a $5-a-month server runs a chain comfortably. No data center, no 32-coin stake.",
  },
  {
    title: "Yours to host",
    body: "One program holds the node, an explorer and a wallet. It runs on your machines, for the people you choose: no company in the middle, no cloud account, nothing to rent.",
  },
  {
    title: "No seed phrases",
    body: "Friends make a wallet with Face ID, Touch ID or their password manager (a passkey). There are no 24 words to write down and lose, and nothing to install.",
  },
  {
    title: "Nobody to get rich",
    body: "No investors, no VC and no token sale: nobody is selling you anything. It's a free, open-source (MIT) project made for fun. The coins have no value, and that's the point.",
  },
];

/** How it compares, in a few rows anyone can read. */
const COMPARE: { row: string; blockula: string; bitcoin: string; ethereum: string }[] = [
  { row: "Signatures", blockula: "ML-DSA-65, built to resist quantum computers", bitcoin: "Elliptic curves (ECDSA, Schnorr): a big quantum computer could forge them", ethereum: "Elliptic curves (ECDSA, BLS): the same risk" },
  { row: "When a payment is final", blockula: "About 2 seconds, for good", bitcoin: "About an hour (6 blocks) to be safe", ethereum: "About 13 minutes" },
  { row: "Who makes blocks", blockula: "Validators you invite, voting", bitcoin: "Miners racing with warehouses of hardware", ethereum: "Validators with 32 ETH staked each" },
  { row: "To run a validator", blockula: "A laptop, a Mac mini or a $5 server", bitcoin: "Mining rigs and cheap power", ethereum: "32 ETH and a dedicated machine with a 2 TB+ disk" },
  { row: "Making a wallet", blockula: "A passkey: Face ID or Touch ID", bitcoin: "A 12–24 word seed phrase", ethereum: "A 12–24 word seed phrase" },
  { row: "Coins sold to early buyers", blockula: "None, ever: the coins have no price", bitcoin: "None", ethereum: "A 2014 presale" },
];

const STEPS: { kind: OrnamentKind; title: string; body: ReactNode }[] = [
  { kind: "candle", title: "Start it", body: <><code>blockula create</code> asks a name and a ticker, then starts your chain on your computer.</> },
  { kind: "bat", title: "Invite friends", body: "Send them one link. Their phone or laptop opens the chain's wallet: nothing to install." },
  { kind: "key", title: "Pay by name", body: <>Each makes a wallet with a passkey, gets coins from the faucet, and pays <code>sam.boo</code>, not a long address.</> },
  { kind: "moon", title: "Make it yours", body: "Add WebAssembly programs, give Claude a spending budget, or change the apps with Claude Code." },
];

const INSIDE: { title: string; body: string }[] = [
  { title: ".boo names", body: "Pay `alice.boo`. Names are readable, and they're yours for a year at a time." },
  { title: "An explorer in every node", body: "Every block, account and name, on a page your friends can open." },
  { title: "Agents welcome", body: "An MCP server for Claude, and session keys with spending limits the chain enforces." },
  { title: "Programs", body: "Optional WebAssembly programs, for when a payment isn't enough." },
];

/** The project site's page (NEXT_PUBLIC_SITE=project): blockula.app. */
export function ProjectSite() {
  return (
    <main className="site" id="top">
      <section className="site-hero center" aria-labelledby="hero-title">
        <Apparition kind="bat" burst={5} />
        <span className="eyebrow">Blockula</span>
        <h1 id="hero-title" className="site-h1">A post-quantum blockchain you run yourself</h1>
        <p className="lead site-lead">Start your own chain on a laptop, a Mac mini or a $5 server. Friends join with a link, make a wallet with Face ID, and pay each other by name. Every block is final in about two seconds.</p>
        <div className="site-cta">
          <a className="btn primary big" href="#demo">Try the demo</a>
          <a className="btn big" href="#start">Start your own</a>
        </div>
        <p className="site-tags caps">Post-quantum · Self-hosted · Lightweight · Open source · No investors</p>
      </section>

      <DemoSection />
      <ConnectSection />

      <section className="site-section" id="why" aria-labelledby="why-title">
        <div className="site-head">
          <span className="eyebrow">Why Blockula</span>
          <h2 id="why-title" className="site-h2">Modern cryptography, none of the baggage</h2>
          <p className="lead">The ideas behind blockchains, rebuilt for people who know each other: quick, light and ready for what comes after today&apos;s cryptography.</p>
        </div>
        <div className="site-grid site-perks">
          {PERKS.map((p) => (
            <div key={p.title} className="site-tile">
              <h3 className="site-h3">{p.title}</h3>
              <p className="dim">{p.body}</p>
            </div>
          ))}
        </div>
      </section>

      <section className="site-section" id="compare" aria-labelledby="compare-title">
        <div className="site-head">
          <span className="eyebrow">How it compares</span>
          <h2 id="compare-title" className="site-h2">Blockula, Bitcoin and Ethereum</h2>
        </div>
        <div className="card site-compare-wrap" tabIndex={0} role="region" aria-labelledby="compare-title">
          <table className="site-compare" data-testid="compare">
            <thead>
              <tr>
                <th scope="col"><span className="site-sr">Question</span></th>
                <th scope="col" className="site-compare-us">Blockula</th>
                <th scope="col">Bitcoin</th>
                <th scope="col">Ethereum</th>
              </tr>
            </thead>
            <tbody>
              {COMPARE.map((r) => (
                <tr key={r.row}>
                  <th scope="row">{r.row}</th>
                  <td className="site-compare-us">{r.blockula}</td>
                  <td>{r.bitcoin}</td>
                  <td>{r.ethereum}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        <p className="dim small site-compare-note">
          Fair&apos;s fair: Bitcoin and Ethereum are guarded by thousands of nodes and years of real use. Blockula is experimental, its post-quantum libraries haven&apos;t been independently audited yet (<a href={AUDIT}>our own review</a>), and it&apos;s made for chains among friends, not for holding value.
        </p>
      </section>

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
          <h2 id="uses-title" className="site-h2">What a chain of your own is good for</h2>
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

      <section className="site-section" id="start" aria-labelledby="start-title">
        <div className="card plate stack site-launch">
          <Apparition kind="candle" />
          <div className="center">
            <span className="eyebrow">Start your own</span>
            <h2 id="start-title" className="site-h2">Two commands</h2>
            <p className="dim">On macOS or Linux. The first installs Blockula; the second asks four questions, each with a default, and starts your chain.</p>
          </div>
          <Command text={INSTALL} />
          <Command text="blockula create" />
          <p className="dim small">
            Then open <code>http://localhost:7070/wallet/</code> for your wallet, and share the invite link it gives you. <a href={CHAIN_GUIDE}>The guide</a> has the rest, and <a href={BUILD_ON_IT}>Build on it</a> shows how to change everything with Claude Code.
          </p>
        </div>
      </section>

      <ChainsSection />
    </main>
  );
}
