/**
 * The project site (`NEXT_PUBLIC_SITE=project`, https://blockula.app): the landing page for
 * Blockula itself, not a wallet. Its demo chain is `NEXT_PUBLIC_DEMO_NODE`, and visitors can
 * paste the address of any node a friend sent them to open that node's own wallet
 * (`<node>/wallet/`, node/src/wallet.rs). No imports, so lib/site.test.mjs runs it under plain Node.
 */

/** This machine: `localhost`, 127.x, `[::1]`. */
const LOCAL_HOST = /^(localhost|127(\.\d{1,3}){3}|\[::1\])$/i;

/**
 * Hosts only this machine or a private network can reach: the rule the MCP applies to plain
 * http (mcp/src/network.ts `isPrivateUrl`). 10/8, 192.168/16, 172.16/12, Tailscale's
 * 100.64/10 and `.ts.net` (WireGuard-encrypted, tailnet members only), and `.internal`,
 * `.local`, `.lan` names.
 */
export function isPrivateHost(hostname: string): boolean {
  const h = hostname.toLowerCase().replace(/\.$/, "");
  if (LOCAL_HOST.test(h)) return true;
  return /^(10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.|100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.)/.test(h) || /\.(internal|local|lan|ts\.net)$/.test(h);
}

/** Something a person typed or pasted, as a node's base URL, or why it isn't one. */
export type NodeAddress = { url: string; error?: undefined } | { url?: undefined; error: string };

/**
 * A node's base URL from what a friend sent: `https://chain.example`, `chain.example`,
 * `100.101.2.3:7070`, or a link into it (`…/wallet/`, `…/#/start/invite`, `…/api/status`), which
 * all mean the same node. Plain `http://` only for this machine or a private network: anywhere
 * else a passkey can't be made, and anyone on the way could change what the page shows.
 */
export function nodeAddress(input: string): NodeAddress {
  const s = input.trim();
  if (!s) return { error: "Paste the address your friend sent you." };
  if (/\s/.test(s)) return { error: "That has spaces in it. Paste just the address, like https://chain.example." };
  let raw = s;
  if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(raw)) {
    // No scheme: https, except on this machine or a private network, where nodes speak plain http.
    const host = raw.split(/[/:?#]/)[0];
    raw = `${isPrivateHost(host) || /^\d{1,3}(\.\d{1,3}){3}$/.test(host) ? "http" : "https"}://${raw}`;
  }
  let u: URL;
  try {
    u = new URL(raw);
  } catch {
    return { error: "That isn't a web address. It should look like https://chain.example." };
  }
  if (!/^https?:$/.test(u.protocol)) return { error: "A node's address starts with https://." };
  if (u.username || u.password) return { error: "That address has a password in it. Ask your friend for the plain one." };
  if (!u.hostname || !/^[a-z0-9.\-[\]:]+$/i.test(u.hostname)) return { error: "That isn't a web address. It should look like https://chain.example." };
  if (u.protocol === "http:" && !isPrivateHost(u.hostname))
    return { error: "That address uses plain http://. Only https:// works across the internet (http:// is for this computer or a private network)." };
  // A link into the node means the node: drop the app's own path, the query and the fragment.
  const path = u.pathname.replace(/\/(wallet|api)(\/.*)?$/, "").replace(/\/+$/, "");
  return { url: `${u.origin}${path}` };
}

/** The wallet a node serves (node/src/wallet.rs): what "Open" takes a visitor to. */
export const walletOf = (node: string) => `${node.replace(/\/+$/, "")}/wallet/`;

/**
 * NEXT_PUBLIC_DEMO_NODE as the site uses it: the node's base URL, or undefined when it's unset.
 * The build fails on one that isn't a node address (next.config.ts), so a typo can't ship.
 */
export function demoNode(v: string | undefined): string | undefined {
  const s = v?.trim();
  if (!s) return undefined;
  const a = nodeAddress(s);
  if (a.error) throw new Error(`NEXT_PUBLIC_DEMO_NODE (${JSON.stringify(s)}): ${a.error}`);
  return a.url;
}

/** CONTRIBUTING.md's "Start in two minutes" prompt, word for word (lib/site.test.mjs checks it). */
export const CONTRIBUTE_PROMPT =
  "Contribute to Blockula (https://github.com/jackdishman/blockula). Clone it, read AGENTS.md and CONTRIBUTING.md, run `npm ci && npm run build`, and start a local chain with `blockula dev` (or `node/target/release/blockula dev`). Pick an open issue labeled `good first issue` or `familiar-friendly` (`gh issue list --repo jackdishman/blockula --label familiar-friendly`), or ask me what to work on. Make one focused change with tests, run what CONTRIBUTING.md's \"What to run\" table says for the parts you touched, and open a PR that references the issue and explains how you verified it. One solid PR beats many small ones.";

/** docs/BECOME-A-VALIDATOR.md's "Let your coding agent do it" prompt, word for word with its
 * placeholders (lib/site.test.mjs checks it); `validatorPrompt` fills in a chain's node and hash. */
export const VALIDATOR_PROMPT = `Set me up as a validator for a Blockula chain, following docs/BECOME-A-VALIDATOR.md in
https://github.com/jackdishman/blockula (clone it if it isn't here).

  Node:          <the chain's node URL, https://…>
  Genesis hash:  <its 64-hex genesis hash>
  Path:          <A (Fly.io, or a VPS I'll give you SSH to), B (this home machine, port forwarding) or C (this home machine behind a relay VPS over Tailscale)>

Rules:
- First check that the node's /api/status reports exactly that genesis hash. Stop if it doesn't.
- Install only a verified release: check its build provenance with \`gh attestation verify\` and its
  line in checksums.txt (deploy/host-validator.sh does this for Path A on Fly).
- Never print, paste, upload or move validator.key, and never put it in a commit, a log or a chat.
  Never start the same key in two places. Tell me where the key's backup is and ask me to keep it
  offline.
- Ask me before anything that costs money (a Fly app, a VPS) or changes my network (router port
  forwards, firewall rules, Tailscale), and wait for my yes.
- For Path B or C, use \`blockula validator setup\` (with --node, --genesis-hash, --host, and --relay
  for C): it does the key, the full node, the bond, the background service and register. Run it
  with --dry-run first and show me its review; run it with --yes once I say yes.
- Take the bond from the chain's faucet if it has one; if it doesn't, tell me how much to ask the
  chain's operator for and wait.
- Run \`blockula validator register\`, then STOP: give me my validator address (boo1…) and the exact
  admit or approve command register printed, for me to send to the chain's operator. Don't try to
  admit or approve yourself.
- Afterwards, show me how to check it: /api/validators, /api/health and the logs.`;

/** The setup prompt for one chain: its node and genesis hash filled in, the path left to choose. */
export const validatorPrompt = (node: string, genesisHash: string) =>
  VALIDATOR_PROMPT.replace("<the chain's node URL, https://…>", node).replace("<its 64-hex genesis hash>", genesisHash);
