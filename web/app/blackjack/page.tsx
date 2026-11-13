import type { Metadata } from "next";
import { Suspense } from "react";
import { Splash } from "@/components/Gate";
import { COPY } from "@/lib/blackjack-copy";
import BlackjackView from "./view";

export const metadata: Metadata = { title: `${COPY.title} · Blockula`, description: COPY.share.description };

/** The blackjack table: a game played with FANG (app/blackjack/view.tsx). The table comes from `?table=` or the build. */
export default function Page() {
  return (
    <Suspense fallback={<Splash />}>
      <BlackjackView />
    </Suspense>
  );
}
