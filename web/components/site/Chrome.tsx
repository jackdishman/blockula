"use client";

import { usePathname } from "next/navigation";
import { MotionToggle } from "@/components/Haunt";
import { Ornament } from "@/components/Ornament";
import { CHAIN_GUIDE, REPO } from "@/components/Landing";

export const DOCS = `${REPO}/tree/main/docs`;

/**
 * The project site's masthead: the wordmark and the page's own sections. No network badge or
 * wallet links: blockula.app is the project's page, and every chain's wallet is on its own node.
 */
export function SiteHeader() {
  // On the legal pages the sections are the home page's.
  const home = usePathname() === "/" ? "" : "/";
  return (
    <header className="top site-top">
      <a href={`${home}#top`} className="brand" aria-label="Blockula, top of the page">
        <span className="wordmark">Blockula</span>
        <span className="edition">post-quantum chains</span>
      </a>
      <nav aria-label="Sections">
        <a href={`${home}#demo`}>Demo</a>
        <a href={`${home}#why`} className="site-nav-wide">Why Blockula</a>
        <a href={`${home}#start`}>Start your own</a>
        <a href={`${home}#chains`}>Join a chain</a>
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
        <a href={CHAIN_GUIDE}>Guide</a>
        <a href={`${REPO}/blob/main/CONTRIBUTING.md`}>Contributing</a>
        <a href="/terms">Terms</a>
        <a href="/privacy">Privacy</a>
      </span>
      <span data-testid="imprint-note">Free and open source (MIT), made for fun and for education and research only: no investors, no VC, no token sale. The coins have no value, and nobody sells or buys them. Experimental and not externally audited.</span>
      <span className="small" data-testid="imprint-terms">By using Blockula you agree to its <a href="/terms">Terms of Use</a> and <a href="/privacy">Privacy Policy</a>.</span>
      <MotionToggle />
    </footer>
  );
}
