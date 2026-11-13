/**
 * The blackjack share page's card (app/blackjack/share): the meta tags X reads to show the table
 * as a player card (the spectator view, app/blackjack/embed, in a frame), and the matching Open
 * Graph tags. Every URL in a card must be absolute and https, so the card needs the wallet's
 * public origin: without one the page carries a title and a description and no card.
 *
 * Pure, so `node --test` runs it (lib/blackjack.test.mjs).
 */
import { POSTER_ALT, TABLE_HEIGHT, TABLE_WIDTH } from "@blockula/blackjack";
import { gameIdFrom, isTableAddress } from "./blackjack.ts";
import { COPY } from "./blackjack-copy.ts";

/** The frame X gives the player: the table's 16:9 drawing, and a line under it. */
export const PLAYER_WIDTH = 480;
export const PLAYER_HEIGHT = 330;

/** Which hand a share link or an embed is for. Only addresses and a hand's number are taken from a link. */
export interface ShareTarget {
  table?: string;
  player?: string;
  game?: string;
}

/** A link's `table`, `player` and `game`, each kept only if it is what it should be. */
export function shareTarget(q: { table?: string | null; player?: string | null; game?: string | null }, builtTable?: string): ShareTarget {
  const table = q.table && isTableAddress(q.table) ? q.table : builtTable && isTableAddress(builtTable) ? builtTable : undefined;
  const game = gameIdFrom(q.game ?? null);
  return { ...(table ? { table } : {}), ...(q.player && isTableAddress(q.player) ? { player: q.player } : {}), ...(game !== null ? { game: game.toString() } : {}) };
}

/** `?table=…&player=…` for a target (empty when it names nothing). */
export function targetQuery(t: ShareTarget): string {
  const q = new URLSearchParams();
  if (t.table) q.set("table", t.table);
  if (t.player) q.set("player", t.player);
  if (t.game) q.set("game", t.game);
  const s = q.toString();
  return s ? `?${s}` : "";
}

/** The public https origin of this wallet, or null: `explicit` (NEXT_PUBLIC_BLACKJACK_ORIGIN), else the passkey host's. */
export function shareOrigin(explicit: string | undefined, rpId: string | undefined, served: boolean): string | null {
  const given = explicit?.trim().replace(/\/+$/, "");
  if (given) return /^https:\/\/[a-z0-9.-]+(:\d+)?$/i.test(given) ? given : null;
  // The served wallet is whichever host its node has: it can't know one when it's built.
  if (served || !rpId || !/^[a-z0-9.-]+$/i.test(rpId) || rpId === "localhost") return null;
  return `https://${rpId}`;
}

export interface ShareTags {
  title: string;
  description: string;
  /** `<meta name=…>` tags: X's. Empty without an origin. */
  twitter: Record<string, string>;
  /** `<meta property=…>` tags: Open Graph's. */
  og: Record<string, string>;
}

/** The share page's tags for `target`, with URLs under `origin` + `basePath`. */
export function shareTags(origin: string | null, basePath: string, target: ShareTarget): ShareTags {
  const { title, description } = COPY.share;
  const og: Record<string, string> = { "og:title": title, "og:description": description, "og:type": "website", "og:site_name": "Blockula" };
  if (!origin) return { title, description, twitter: {}, og };
  const at = `${origin}${basePath}`;
  const image = `${at}/blackjack/poster.png`;
  const twitter = {
    "twitter:card": "player",
    "twitter:title": title,
    "twitter:description": description,
    "twitter:player": `${at}/blackjack/embed${targetQuery(target)}`,
    "twitter:player:width": String(PLAYER_WIDTH),
    "twitter:player:height": String(PLAYER_HEIGHT),
    "twitter:image": image,
    "twitter:image:alt": POSTER_ALT,
  };
  return { title, description, twitter, og: { ...og, "og:url": `${at}/blackjack/share${targetQuery(target)}`, "og:image": image, "og:image:width": String(TABLE_WIDTH), "og:image:height": String(TABLE_HEIGHT), "og:image:alt": POSTER_ALT } };
}
