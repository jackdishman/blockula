# The chain directory

[blockula.app](https://blockula.app) lists Blockula chains that anyone can open: one file here per chain, `chains/<chain id>.json`. Each gets a card on the site with its live status (read from its node by the visitor's browser), buttons for its wallet and explorer and, if it takes validators, the commands to become one.

The project doesn't run, check or vouch for these chains. Their coins have no value, and nobody sells or buys them.

## List your chain

`blockula launch` prints a **List it on blockula.app** link when your chain can be reached from the internet. It opens GitHub's new-file page with this file filled in. Fill in `contact`, check the rest, and open the pull request. By hand:

```json
{
  "name": "Jack in the Block",
  "chain_id": "jack-in-the-block",
  "token": "JACK",
  "description": "Pizza debts and a shared kitty for a few friends.",
  "rpc": "https://chain.example.org",
  "genesis_hash": "<64 hex: /api/status's genesis_hash>",
  "validators_wanted": true,
  "p2p": "chain.example.org:30300",
  "contact": "<your GitHub handle>",
  "listed_at": "2026-11-07"
}
```

| Field | | |
|---|---|---|
| `name` | required | Your chain's name, as its genesis has it. 1–48 bytes |
| `chain_id` | required | Its chain id. The file is named after it: `chains/<chain id>.json` |
| `token` | required | Its ticker: 2–8 of A–Z and 0–9, starting with a letter |
| `description` | required | One sentence, at most 160 characters |
| `rpc` | required | A node's public `https://` origin, with no path or trailing slash. It must serve the explorer at `/` and the wallet at `/wallet/` (every node does). A DNS name, not an IP: passkeys need one |
| `genesis_hash` | required | 64 lowercase hex. The site checks the node reports it, and the validator commands pin it |
| `validators_wanted` | required | `true` if people outside your group may run validators |
| `p2p` | with `validators_wanted` | `host:port` your validators listen on, reachable from the internet (TCP 30300 by default) |
| `contact` | required | A GitHub handle or an `https://` URL: where people ask to be admitted |
| `listed_at` | optional | `YYYY-MM-DD` |

No other fields are taken.

## What gets accepted

`node scripts/check-chains.mjs` runs on every pull request and refuses a listing when:

- a field is missing, has the wrong type, is too long, or has control, bidi or zero-width characters (or `<` and `>`);
- `rpc` isn't `https://`, has a path, credentials or an IP address, or names a host only a private network reaches (`localhost`, `.local`, `.lan`, `.internal`, private and Tailscale IP ranges). A `*.ts.net` name served by [Tailscale Funnel](https://tailscale.com/kb/1223/funnel) is public, so it's fine;
- `validators_wanted` is `true` without a `p2p`, or `p2p` is private or on a tailnet. Funnel carries HTTPS only, so validators can't reach a tailnet chain from outside: list it with `validators_wanted: false`;
- its name, chain id, description or host reads as Blockula itself or as "official";
- two listings share a chain id, genesis hash, node, name or p2p address.

A maintainer then opens the chain's node before merging. A chain that stops answering for a long time, or whose node reports another genesis, may be delisted.

## Delist

Open a pull request that deletes your file, or an issue asking for it. Delisting only removes the card: the chain keeps running.
