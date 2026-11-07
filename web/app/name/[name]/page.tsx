import { servedStaticParams } from "@/lib/route-params";
import NamePage from "./view";

// The served wallet is a static export: one page for every name, read from the URL (lib/route-param.ts).
// Other builds list none (undefined), so the page renders on demand like every other.
export const generateStaticParams = servedStaticParams("name");

export default function Page() {
  return <NamePage />;
}
