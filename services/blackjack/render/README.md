# @blockula/blackjack-render

Pictures of a blackjack hand on Blockula, as PNG files. Blackjack here is a game played with FANG, run by a program on the chain; FANG has no value and can't be bought, and there are no prizes.

`@blockula/blackjack` draws a hand as SVG (`tableSvg`, `posterSvg`: pure strings, safe to inline in a page). This package renders those drawings to PNG with [resvg](https://github.com/yisibl/resvg-js) and the site's fonts, for places that take an image and not markup: a post on X, a link's preview card. Node only.

## API

```ts
import { renderTable, renderPoster, type Picture } from "@blockula/blackjack-render";

const picture = renderTable(snapshot); // a TableSnapshot from @blockula/blackjack
picture.png; // Uint8Array: a 1200 × 675 PNG
picture.svg; // the SVG document it was rendered from
picture.alt; // alt text: every card, both totals, the bet, where the hand stands
```

| | |
|---|---|
| `renderTable(snapshot): Picture` | One hand: the dealer's row and the player's, the hole card as the bat back while it's face down, the status line, a stamp once the hand is settled, and a footer with totals, bet, hand id, the tx id (shortened) and the player's label. |
| `renderPoster(): Picture` | The table with no hand on it: the picture a link to the table shows. |
| `renderSvg(svg, fontFiles?): Uint8Array` | Any 1200 × 675 SVG document as a PNG, with the shipped fonts. |
| `FONT_FILES` | The paths of the font files in use. |
| `RENDER_FONTS` | The font families as the files name them (resvg matches a family's name exactly). |

**Every picture carries alt text.** `Picture.alt` is never empty; post it with the image. A hand's alt text names each card.

**Deterministic.** The same hand gives the same SVG string and the same PNG bytes. Nothing depends on the clock, on randomness or on the machine's fonts: system fonts aren't loaded.

**Untrusted text.** The player's label and the tx id are shown as text only: cleaned of control characters, cut to a fixed length with an ellipsis, and escaped.

## The pictures

```sh
# A gallery: the poster, a table waiting for its deal, a fresh deal, the player's turn,
# each way a hand ends, and the longest hand the rules allow. Each as .png, .svg and .alt.txt.
npx tsx services/blackjack/render/src/cli.ts /tmp/blackjack-gallery

# The poster the web wallet serves at /blackjack/poster.png (and its alt text):
npm run poster -w @blockula/blackjack-render
```

`web/public/blackjack/poster.png` is checked in. After changing the drawing (`services/blackjack/core/src/svg.ts`), rebuild the core (`npm run build -w @blockula/blackjack`) and run `npm run poster` again: the test fails while the file differs from a fresh render.

## Tests

```sh
npm test -w @blockula/blackjack-render
```

Offline: PNG size, the same bytes on a second render, different bytes for different hands, each font family drawn from its shipped file, alt text on every picture, and the poster in `web/public` matching a fresh render byte for byte.

## Fonts

UnifrakturMaguntia (the masthead), EB Garamond, IM Fell English, IM Fell English SC and Courier Prime, in `assets/fonts`. All are under the SIL Open Font License 1.1; [assets/fonts/README.md](assets/fonts/README.md) has each one's copyright line and the state of the license text.

The bat on the card backs and the face cards is drawn as vector shapes in `svg.ts`; no image files are embedded.
