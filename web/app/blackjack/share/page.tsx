import type { Metadata } from "next";
import Link from "next/link";
import { COPY, NOTE } from "@/lib/blackjack-copy";
import { shareOrigin, shareTags, shareTarget, targetQuery } from "@/lib/blackjack-share";
import { BASE_PATH, BLACKJACK_ORIGIN, BLACKJACK_TABLE, RP_ID, SERVED } from "@/lib/config";
import "../blackjack.css";

type Search = Promise<Record<string, string | string[] | undefined>>;
const one = (v: string | string[] | undefined) => (Array.isArray(v) ? v[0] : v);

/** The hand the link is for. The served wallet is a static export: it knows only the build's table. */
async function targetOf(searchParams: Search) {
  if (SERVED) return shareTarget({}, BLACKJACK_TABLE);
  const q = await searchParams;
  return shareTarget({ table: one(q.table), player: one(q.player), game: one(q.game) }, BLACKJACK_TABLE);
}

/**
 * The card X shows for a link to this page: a player card whose player is the spectator view
 * (app/blackjack/embed) and whose image is the poster, with the matching Open Graph tags
 * (lib/blackjack-share.ts). Its URLs are absolute, under the wallet's public origin.
 */
export async function generateMetadata({ searchParams }: { searchParams: Search }): Promise<Metadata> {
  const tags = shareTags(shareOrigin(BLACKJACK_ORIGIN, RP_ID, SERVED), BASE_PATH, await targetOf(searchParams));
  const og = tags.og;
  return {
    title: tags.title,
    description: tags.description,
    openGraph: {
      title: og["og:title"],
      description: og["og:description"],
      type: "website",
      siteName: og["og:site_name"],
      ...(og["og:url"] ? { url: og["og:url"], images: [{ url: og["og:image"], width: Number(og["og:image:width"]), height: Number(og["og:image:height"]), alt: og["og:image:alt"] }] } : {}),
    },
    // X's own tags, as it names them (a player card has no stream here, so Next's `twitter.players` doesn't fit).
    other: tags.twitter,
  };
}

/** A page to share: the poster, the note, and the way to the table. */
export default async function SharePage({ searchParams }: { searchParams: Search }) {
  const target = await targetOf(searchParams);
  return (
    <main className="narrow stack-lg" data-testid="bj-share">
      <div className="page-head">
        <span className="eyebrow">{COPY.eyebrow}</span>
        <h1>{COPY.share.title}</h1>
      </div>
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img className="bj-poster" src={`${BASE_PATH}/blackjack/poster.png`} alt={COPY.share.alt} width={1200} height={675} />
      <p className="bj-note" data-testid="bj-note">
        {NOTE}
      </p>
      <p className="m0">{COPY.share.description}</p>
      <Link className="btn primary self-start" href={`/blackjack${targetQuery({ table: target.table })}`} data-testid="bj-share-open">
        {COPY.share.open}
      </Link>
    </main>
  );
}
