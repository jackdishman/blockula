import Link from "next/link";
import { Apparition } from "@/components/Haunt";
import { Copy } from "@/components/ui";

const REPO = "https://github.com/jackdishman/blockula";
const INSTALL = `curl -fsSL ${REPO}/releases/latest/download/install.sh | sh`;
const SUMMON = "blockula summon mychain --host <your address>";

const STEPS: { title: string; body: string }[] = [
  { title: "Start the chain", body: "`blockula summon` makes the chain on your machine and prints its genesis hash and the command your friends join with." },
  { title: "Friends join", body: "They install the same binary and run `blockula join`. Tailscale is the easy way to reach each other; a port forward works too." },
  { title: "Friends become validators", body: "A bond from the chain's faucet and one command each. They start taking turns at the next epoch, within 10 minutes." },
];

const PARTS: { title: string; body: string }[] = [
  { title: "Post-quantum throughout", body: "ML-DSA-65 signatures (NIST FIPS 204) on every transaction and vote; validators key their channels with ML-KEM-768." },
  { title: "Runs at home", body: "A block every 5 seconds by default, sized for home connections. No hosting to pay for." },
  { title: "Names, not addresses", body: "Pay `alice.boo`. Every node serves an explorer that explains each transaction in plain English." },
  { title: "Wallets on passkeys", body: "Face ID or a password manager unlocks a wallet: no seed phrase. The passkey only unlocks the key and never signs on-chain. Agents get budgets the chain enforces." },
  { title: "Staking and governance", body: "Delegation, slashing, on-chain proposals and votes, as your chain's validators and holders decide." },
  { title: "Programs (experimental)", body: "WebAssembly programs, off by default: `summon --programs` turns them on. Not audited yet." },
];

/** `code` spans in the copy above, rendered as code. */
function Text({ children }: { children: string }) {
  return <>{children.split("`").map((part, i) => (i % 2 ? <code key={i}>{part}</code> : part))}</>;
}

function Command({ text }: { text: string }) {
  return (
    <div className="landing-cmd">
      <code>{text}</code>
      <Copy text={text} />
    </div>
  );
}

/** What a first visit to `/` shows: Blockula 1.0, the kit, with the testnet wallet a click away. */
export function Landing() {
  return (
    <main className="narrow stack-lg landing">
      <div className="center">
        <Apparition kind="moon" />
        <span className="eyebrow">Blockula 1.0</span>
        <h1>Launch your own quantum-safe ledger</h1>
        <p className="lead">One command starts a chain, and your friends&apos; computers join it. Post-quantum signatures throughout, on machines you already have.</p>
      </div>
      <section className="card stack">
        <h2>Two commands</h2>
        <Command text={INSTALL} />
        <Command text={SUMMON} />
        <p className="dim small">macOS and Linux. The launch guide covers reaching each other (Tailscale, a home network or the open internet), adding validators and keeping it running.</p>
        <div className="row-gap">
          <a className="btn primary" href={`${REPO}/blob/main/docs/LAUNCH.md`}>Read the launch guide</a>
          <a className="btn" href={REPO}>Source on GitHub</a>
        </div>
      </section>
      <section className="stack">
        <h2>How a chain comes together</h2>
        <ol className="landing-steps">
          {STEPS.map((s) => (
            <li key={s.title}>
              <strong>{s.title}.</strong> <Text>{s.body}</Text>
            </li>
          ))}
        </ol>
      </section>
      <section className="stack">
        <h2>What&apos;s in it</h2>
        <div className="landing-parts">
          {PARTS.map((p) => (
            <div key={p.title}>
              <h3>{p.title}</h3>
              <p className="dim"><Text>{p.body}</Text></p>
            </div>
          ))}
        </div>
      </section>
      <section className="card stack center">
        <h2>Try it first</h2>
        <p className="dim">The public testnet runs the same software: make a wallet with a passkey, get free test FANG and claim a <code>.boo</code> name.</p>
        <Link className="btn primary big" href="/welcome">Open the testnet wallet</Link>
        <Link className="link small" href="/explore">Or explore the testnet</Link>
      </section>
      <p className="dim small center">
        Each chain&apos;s unit, FANG, has no value by design: its faucet hands it out and it only prices spam. Nobody sells it, buys it or gets paid in it. Blockula isn&apos;t externally audited.
      </p>
    </main>
  );
}
