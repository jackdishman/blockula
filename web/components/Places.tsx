import { SITE_URL, walletOf } from "@/lib/site";

/**
 * Where you are, on every page: blockula.app (the project), and a chain's two apps on its own
 * node, the explorer (`/`) and the wallet (`/wallet/`). The node's explorer carries the same bar
 * (node/src/explorer.html `.places`), so moving between the three never changes where to look.
 * The site says whose explorer and wallet they are (`label`); on a node the masthead already does.
 * Plain links: each is another app, on another origin or outside this one's base path.
 */
export function Places({ here, node = "", label }: { here: "site" | "wallet"; node?: string; label?: string }) {
  const current = { "aria-current": "page" } as const;
  return (
    <div className="places" role="navigation" aria-label={here === "site" ? "Blockula, and its demo chain" : "This chain's apps, and Blockula"} data-testid="places">
      <a href={here === "site" ? "/" : `${SITE_URL}/`} {...(here === "site" ? current : {})}>
        {here === "site" ? "blockula.app" : "‹ blockula.app"}
      </a>
      <span className="places-group">
        {label && <span className="places-label">{label}</span>}
        <a href={`${node}/`} data-testid="places-explorer">Explorer</a>
        <a href={walletOf(node)} data-testid="places-wallet" {...(here === "wallet" ? current : {})}>Wallet</a>
      </span>
    </div>
  );
}
