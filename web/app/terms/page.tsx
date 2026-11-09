import type { Metadata } from "next";
import Link from "next/link";
import { LEGAL_REPO as REPO, TERMS_VERSION } from "@/lib/legal";

export const metadata: Metadata = { title: "Terms of Use · Blockula" };

/**
 * The Terms of Use (lib/legal.ts TERMS_VERSION dates them; a change that alters what someone
 * agrees to takes a new date, which asks everyone to agree again). Served by the wallet, the
 * served wallet on every node and the project site alike.
 */
export default function TermsPage() {
  return (
    <main className="narrow stack-lg legal" data-testid="terms-page">
      <div className="center">
        <span className="eyebrow">Last updated {TERMS_VERSION}</span>
        <h1>Terms of Use</h1>
        <p className="lead">Blockula is free, open-source software for education and research. By using it, or any website, wallet, explorer, faucet or chain the Blockula project runs, you agree to these terms. If you don&apos;t agree, don&apos;t use them.</p>
      </div>

      <section className="card stack">
        <h2>1. What Blockula is</h2>
        <p>Blockula is an experiment: software for running small post-quantum blockchains, published under the MIT license by the Blockula project&apos;s maintainers (&quot;we&quot;, &quot;us&quot;). We run a few things ourselves: blockula.app, the demo chain at demo.blockula.app with its explorer, wallet and faucet, and the published releases (the &quot;Services&quot;).</p>
        <p>Anyone can run a Blockula chain. A chain someone else runs, including every chain in the directory on blockula.app other than ours, and the wallet its node serves, is that operator&apos;s, under their rules. We don&apos;t run, control or vouch for it, and a listing isn&apos;t an endorsement.</p>
      </section>

      <section className="card stack">
        <h2>2. For education and research only</h2>
        <p>You may use Blockula to learn, teach, experiment, test and research. It isn&apos;t a financial product, a bank, a payment service, an exchange or an investment, and nothing we publish is financial, investment, legal or tax advice.</p>
      </section>

      <section className="card stack">
        <h2>3. The coins have no value</h2>
        <ul>
          <li>FANG and every other Blockula chain&apos;s coins, .boo names, Soul, Hunt bounties and contribution rewards (&quot;chain units&quot;) have no monetary value. They aren&apos;t money, securities or property you can redeem with us, and nobody owes you anything for them.</li>
          <li>We have never sold chain units and never will, and we never ask anyone to pay for them.</li>
          <li>Don&apos;t buy, sell, trade, lend or borrow chain units for money or anything of value, offer them for sale, or accept them as payment for real goods, services or debts. Don&apos;t tell anyone they&apos;re worth something.</li>
          <li>Nothing you do on any Blockula chain earns a claim to a future token, an airdrop, a mainnet or a payment. We don&apos;t plan a value-bearing network, and if one were ever made, these chains&apos; units wouldn&apos;t carry over.</li>
        </ul>
      </section>

      <section className="card stack">
        <h2>4. Experimental software, and the risks you accept</h2>
        <ul>
          <li>Blockula is experimental and hasn&apos;t had a full independent security audit, including its post-quantum cryptography. It can have bugs that lose, freeze or expose what&apos;s on a chain.</li>
          <li>A chain can slow down, halt, fork, be upgraded or be reset, and any Service can change or stop at any time, without notice. We don&apos;t plan to reset the demo chain, but we don&apos;t promise it runs forever.</li>
          <li>You can lose access to your wallet, your chain units and your names, and nobody, us included, can restore them.</li>
        </ul>
      </section>

      <section className="card stack">
        <h2>5. Your wallet and your key</h2>
        <p>Blockula wallets are self-custodial: your key is made on your device and unlocked by your passkey. We never receive it, can&apos;t see it, and can&apos;t recover it or reverse a transaction. Keeping your passkey, devices and recovery kit safe is your responsibility. Agents and devices you give a key or budget to act for you.</p>
      </section>

      <section className="card stack">
        <h2>6. Public and permanent</h2>
        <p>Everything sent to a chain, including addresses, balances, payments, memos, .boo names, key labels and any GitHub account you link, is public, is copied by every node, and can&apos;t be changed or deleted by us or anyone. Don&apos;t put personal or sensitive information on a chain. The <Link href="/privacy">Privacy Policy</Link> says what else we handle.</p>
      </section>

      <section className="card stack">
        <h2>7. What you mustn&apos;t do</h2>
        <ul>
          <li>Break the law, or use Blockula to defraud, scam, impersonate, harass, threaten or deceive anyone, including with names, memos or programs.</li>
          <li>Present chain units as valuable, or Blockula, a chain or a program as an investment or a way to make money.</li>
          <li>Use Blockula to evade sanctions, or use it if you&apos;re on a sanctions list or in a country under comprehensive sanctions.</li>
          <li>Attack, overload or abuse the Services or other people&apos;s nodes. Security research is welcome under <a href={`${REPO}/blob/main/SECURITY.md`}>SECURITY.md</a>: report what you find privately.</li>
          <li>Claim to speak for the Blockula project, or name a chain so that it reads as ours.</li>
        </ul>
        <p>We may refuse faucet requests, delist a chain from the directory, or block access to a Service for anyone who breaks these terms.</p>
      </section>

      <section className="card stack">
        <h2>8. Who can use it</h2>
        <p>You must be 18 or older, or the age of majority where you live if that&apos;s higher, and allowed to use software like this where you are.</p>
      </section>

      <section className="card stack">
        <h2>9. The software license</h2>
        <p>Blockula&apos;s source code is licensed under the <a href={`${REPO}/blob/main/LICENSE`}>MIT license</a>, which governs your use of the code. These terms cover the Services and how chain units may be used.</p>
      </section>

      <section className="card stack">
        <h2>10. No warranty, and limits on liability</h2>
        <p className="legal-caps">Blockula and the Services are provided &quot;as is&quot; and &quot;as available&quot;, without warranties of any kind, express or implied, including merchantability, fitness for a particular purpose, title and non-infringement. To the fullest extent the law allows, we aren&apos;t liable for any loss or damage arising from or related to Blockula, the Services, any chain or chain units, including lost data, keys, names or chain units, whether in contract, tort or otherwise, even if we were told it might happen. Where the law doesn&apos;t allow these limits, our total liability is limited to the smallest amount it allows.</p>
        <p>You agree to hold us harmless from claims that arise from your use of Blockula or your breach of these terms.</p>
      </section>

      <section className="card stack">
        <h2>11. Changes</h2>
        <p>We may change these terms. The date at the top says when they last changed; when a change matters, the wallet asks you to agree again before you go on. If you keep using the Services after a change, you accept it.</p>
      </section>

      <section className="card stack">
        <h2>12. Contact</h2>
        <p>Questions go to the project&apos;s <a href={`${REPO}/issues`}>GitHub issues</a>; security reports go privately, as <a href={`${REPO}/blob/main/SECURITY.md`}>SECURITY.md</a> describes.</p>
      </section>
    </main>
  );
}
