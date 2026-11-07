import { servedStaticParams } from "@/lib/route-params";
import ProgramPage from "./view";

// The served wallet is a static export: one page for every address, read from the URL (lib/route-param.ts).
// Other builds list none (undefined), so the page renders on demand like every other.
export const generateStaticParams = servedStaticParams("address");

export default function Page() {
  return <ProgramPage />;
}
