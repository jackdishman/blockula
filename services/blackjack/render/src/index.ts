/**
 * Pictures of a blackjack hand as PNG files: the SVG `@blockula/blackjack` draws, rendered with
 * resvg and the site's fonts. Deterministic: the same hand gives the same bytes, on any machine,
 * because only the font files shipped in `assets/fonts` are used.
 */
import { fileURLToPath } from "node:url";
import { Resvg } from "@resvg/resvg-js";
import { DEFAULT_FONTS, POSTER_ALT, TABLE_HEIGHT, TABLE_WIDTH, altText, posterSvg, tableSvg, type TableSnapshot } from "@blockula/blackjack";

export interface Picture {
  /** The SVG document the PNG was rendered from. */
  svg: string;
  /** 1200 × 675 PNG. */
  png: Uint8Array;
  /** Alt text: every picture has one. */
  alt: string;
}

/** The font files a picture is drawn with (SIL Open Font License 1.1: see assets/fonts/README.md). */
export const FONT_FILES: readonly string[] = ["CourierPrime-Regular.ttf", "EBGaramond-Regular.ttf", "IMFellEnglish-Regular.ttf", "IMFellEnglishSC.ttf", "UnifrakturMaguntia.ttf"].map((f) =>
  fileURLToPath(new URL(`../assets/fonts/${f}`, import.meta.url)),
);

/**
 * The families as the font files name them. resvg matches a family by its exact name, and the
 * IM Fell files spell theirs "IM FELL English"; a browser matches without regard to case, so the
 * drawing's defaults stay as the site's stylesheet spells them.
 */
export const RENDER_FONTS = Object.freeze({ ...DEFAULT_FONTS, caps: "IM FELL English SC", fell: "IM FELL English" });

/** An SVG document as a PNG, drawn with the shipped fonts only (or with `fontFiles`, for tests). */
export function renderSvg(svg: string, fontFiles: readonly string[] = FONT_FILES): Uint8Array {
  const resvg = new Resvg(svg, {
    fitTo: { mode: "width", value: TABLE_WIDTH },
    font: { fontFiles: [...fontFiles], loadSystemFonts: false, defaultFontFamily: DEFAULT_FONTS.serif },
  });
  const image = resvg.render();
  if (image.width !== TABLE_WIDTH || image.height !== TABLE_HEIGHT) throw new Error(`rendered ${image.width} × ${image.height}, expected ${TABLE_WIDTH} × ${TABLE_HEIGHT}`);
  return new Uint8Array(image.asPng());
}

/** A hand's picture. */
export function renderTable(s: TableSnapshot): Picture {
  const svg = tableSvg(s, { fonts: RENDER_FONTS });
  return { svg, png: renderSvg(svg), alt: altText(s) };
}

/** The table's poster: the picture a link to the table shows. */
export function renderPoster(): Picture {
  const svg = posterSvg({ fonts: RENDER_FONTS });
  return { svg, png: renderSvg(svg), alt: POSTER_ALT };
}
