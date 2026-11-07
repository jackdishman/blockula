"use client";

import { MotionToggle } from "@/components/Haunt";
import { Ornament } from "@/components/Ornament";
import { LAUNCH_GUIDE, REPO } from "@/components/Landing";

export const DOCS = `${REPO}/tree/main/docs`;

/**
 * The project site's masthead: the wordmark and the page's own sections. No network badge or
 * wallet links: blockula.app is the project's page, and every chain's wallet is on its own node.
 */
export function SiteHeader() {
  return (
    <header className="top site-top">
      <a href="#top" className="brand" aria-label="Blockula, top of the page">
        <span className="wordmark">Blockula</span>
        <span className="edition">small chains</span>
      </a>
      <nav aria-label="Sections">
        <a href="#demo">Demo</a>
        <a href="#how" className="site-nav-wide">How it works</a>
        <a href="#launch">Launch</a>
        <a href="#chains">Join a chain</a>
        <a href={REPO}>GitHub</a>
      </nav>
    </header>
  );
}

export function SiteFooter() {
  return (
    <footer className="imprint site-imprint">
      <Ornament kind="bat" />
      <span className="caps">Quantum-safe ML-DSA-65 keys · Unlocked by passkeys</span>
      <span className="site-footer-links">
        <a href={REPO}>GitHub</a>
        <a href={DOCS}>Docs</a>
        <a href={LAUNCH_GUIDE}>Launch guide</a>
        <a href={`${REPO}/blob/main/CONTRIBUTING.md`}>Contributing</a>
      </span>
      <span data-testid="imprint-note">Free and open source. Test chains only: the coins have no value, and nobody sells or buys them. Not externally audited.</span>
      <MotionToggle />
    </footer>
  );
}
