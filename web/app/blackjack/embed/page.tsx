import type { Metadata } from "next";
import { Suspense } from "react";
import { COPY } from "@/lib/blackjack-copy";
import EmbedView from "./view";

export const metadata: Metadata = { title: `${COPY.title} · Blockula`, description: COPY.share.description, robots: { index: false } };

/**
 * A hand at the blackjack table, to look at: `?table=…&player=…` or `?table=…&game=…`. No wallet,
 * nothing signed, nothing kept in this browser. It is the one route X may put in a frame
 * (lib/csp.ts `EMBED_PATH`), as the share page's player card.
 */
export default function Page() {
  return (
    <Suspense fallback={null}>
      <EmbedView />
    </Suspense>
  );
}
