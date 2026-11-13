"use client";

// A hand as a picture and as text. The picture is the shared library's SVG (`tableSvg`: a pure
// string with presentation attributes only, no styles and no scripts, so the strict CSP takes it
// inline); its label is the library's alt text; and the same cards, totals and bet are listed as
// text under it for anyone who doesn't see the picture.
import { altText, tableSvg, type TableSnapshot } from "@blockula/blackjack";
import { useEffect, useMemo, useState } from "react";
import { cardsText, fangText, totalText } from "@/lib/blackjack";
import { COPY } from "@/lib/blackjack-copy";

type Fonts = { display: string; serif: string; caps: string; mono: string };

/** The font families the page's own stylesheet loaded (app/layout.tsx, next/font), for the drawing's text. */
function useSvgFonts(): Partial<Fonts> {
  const [fonts, setFonts] = useState<Partial<Fonts>>({});
  useEffect(() => {
    const css = getComputedStyle(document.documentElement);
    const family = (v: string, fallback: string) => {
      const loaded = css.getPropertyValue(v).trim();
      return loaded ? `${loaded}, ${fallback}` : fallback;
    };
    setFonts({
      display: family("--font-fraktur", "UnifrakturMaguntia, serif"),
      serif: family("--font-garamond", "EB Garamond, Georgia, serif"),
      caps: family("--font-fell-sc", "IM Fell English SC, serif"),
      mono: family("--font-courier", "Courier Prime, monospace"),
    });
  }, []);
  return fonts;
}

/** The table for one hand. `mine`: the hand is the viewer's own ("You" instead of "Player"). */
export function TableFigure({ snapshot, mine = false, list = true }: { snapshot: TableSnapshot; mine?: boolean; list?: boolean }) {
  const fonts = useSvgFonts();
  const svg = useMemo(() => tableSvg(snapshot, { fonts }), [snapshot, fonts]);
  const alt = useMemo(() => altText(snapshot), [snapshot]);
  return (
    <figure className="bj-figure" data-testid="bj-figure">
      <div className="bj-svg" role="img" aria-label={alt} data-testid="bj-svg" dangerouslySetInnerHTML={{ __html: svg }} />
      {list && (
        <figcaption>
          <dl className="bj-cards" data-testid="bj-cards">
            <dt>{COPY.hand.dealer}</dt>
            <dd data-testid="bj-dealer-cards">
              {cardsText(snapshot.dealerCards)}
              {snapshot.holeHidden && snapshot.dealerCards.length > 0 ? `, and ${COPY.hand.faceDown}` : ""} <span className="dim">({totalText(snapshot.dealerCards)})</span>
            </dd>
            <dt>{mine ? COPY.hand.player : COPY.hand.playerOther}</dt>
            <dd data-testid="bj-player-cards">
              {cardsText(snapshot.playerCards)} <span className="dim">({totalText(snapshot.playerCards)})</span>
            </dd>
            <dt>Bet</dt>
            <dd data-testid="bj-stake">{fangText(snapshot.stake)}</dd>
          </dl>
        </figcaption>
      )}
    </figure>
  );
}
