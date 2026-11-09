import type { Metadata } from "next";
import Link from "next/link";
import { LEGAL_REPO as REPO, TERMS_VERSION } from "@/lib/legal";

export const metadata: Metadata = { title: "Privacy Policy · Blockula" };

/**
 * The Privacy Policy, dated with the Terms of Use (lib/legal.ts TERMS_VERSION). It must stay
 * true of the code: if the wallet, the site or a node starts keeping or sending anything new,
 * this page says so in the same change.
 */
export default function PrivacyPage() {
  return (
    <main className="narrow stack-lg legal" data-testid="privacy-page">
      <div className="center">
        <span className="eyebrow">Last updated {TERMS_VERSION}</span>
        <h1>Privacy Policy</h1>
        <p className="lead">The short version: we don&apos;t ask for your name or email, there are no accounts with us, no ads and no analytics, and your wallet&apos;s key never leaves your device. What you put on a chain is public and permanent.</p>
      </div>

      <section className="card stack">
        <h2>Who this covers</h2>
        <p>This policy covers the services the Blockula project runs (&quot;we&quot;): blockula.app and the demo chain at demo.blockula.app with its explorer, wallet and faucet. A chain someone else runs, and the wallet its node serves, is that operator&apos;s: they see what their node sees, under their own policy.</p>
      </section>

      <section className="card stack">
        <h2>What stays on your device</h2>
        <ul>
          <li><b>Your passkey</b> is kept by your device or password manager (Apple, Google, 1Password and so on, under their policies). We never receive it.</li>
          <li><b>Your wallet&apos;s key</b> is derived from your passkey when you unlock, held only in that browser tab&apos;s memory, and never stored or sent.</li>
          <li><b>The browser&apos;s storage</b> keeps what the wallet needs to find your account again: your key id and passkey id, your account&apos;s address and name, that you agreed to these terms and when, and preferences such as the tutorial&apos;s progress, accounts you watch and the network you chose (one cookie names a custom network, so its address can be allowed). None of it is a secret, and clearing this site&apos;s data removes it.</li>
        </ul>
      </section>

      <section className="card stack">
        <h2>What reaches our servers</h2>
        <ul>
          <li><b>Requests.</b> Your browser asks our servers for pages and chain data. Our hosts, Vercel (blockula.app) and Fly.io (demo.blockula.app), see and briefly log what any web server does: your IP address, the time, the page asked for and your browser&apos;s user agent, under their own privacy policies.</li>
          <li><b>Rate limits.</b> The demo node uses your IP address in memory to limit requests and faucet drips (one drip per address an hour, one per IP every ten minutes). It doesn&apos;t write it to the chain.</li>
          <li><b>Transactions you send</b> go to the node and, through it, to every node on that chain (below).</li>
        </ul>
        <p>We don&apos;t use cookies to track you, run analytics or ads, or sell, rent or share data with anyone for marketing. The site&apos;s fonts are served from our own hosts.</p>
      </section>

      <section className="card stack">
        <h2>What&apos;s public and permanent</h2>
        <p>A blockchain is a public record. Everything you send to one is copied to every node and stays there: your account&apos;s address and balance, the payments you send and receive, their memos, your .boo names, your keys&apos; labels, votes, and any GitHub account you link (which also points back from a public gist). Anyone, anywhere, can read it, copy it and keep it, and nobody, including us, can change or delete it. Don&apos;t put your real name or anything personal on a chain unless you want it public forever.</p>
      </section>

      <section className="card stack">
        <h2>Children</h2>
        <p>Blockula isn&apos;t for anyone under 18, and we don&apos;t knowingly collect anything about children.</p>
      </section>

      <section className="card stack">
        <h2>Your choices and rights</h2>
        <p>You can clear what your browser keeps at any time. We hold almost nothing that identifies you, so there&apos;s little for us to show, correct or delete beyond our hosts&apos; short-lived logs; ask, and we&apos;ll help with what we can. What&apos;s on a chain can&apos;t be erased by anyone.</p>
      </section>

      <section className="card stack">
        <h2>Changes and contact</h2>
        <p>We&apos;ll update this page when what we handle changes, and the wallet asks you to agree again when it matters. Questions go to the project&apos;s <a href={`${REPO}/issues`}>GitHub issues</a>. See also the <Link href="/terms">Terms of Use</Link>.</p>
      </section>
    </main>
  );
}
