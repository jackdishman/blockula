import { servedStaticParams } from "@/lib/route-params";
import TxPage from "./view";

// The served wallet is a static export: one page for every id, read from the URL (lib/route-param.ts).
// Elsewhere there is none, so the page renders on demand like every other.
export const generateStaticParams = servedStaticParams("id");

export default function Page() {
  return <TxPage />;
}
