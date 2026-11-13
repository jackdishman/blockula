# Name sites

Own a `.boo` name on Blockula's demo chain and you can have the matching subdomain of blockula.app for your project's site: own `blood.boo` and you can have **blood.blockula.app**. Your site stays on your own host (Vercel, Netlify, GitHub Pages, Fly, your own server…). blockula.app's DNS points the subdomain at it with a CNAME.

One file here per site, `sites/<name>.json`. A maintainer reads each one before it goes live, and the DNS follows the chain: let your name lapse, or hand it to someone else, and the subdomain stops pointing at your site.

A site at a subdomain isn't run, checked or vouched for by the project. Each one is its owner's.

## Get your subdomain

Say your name is `blood.boo`, your site is at `blood-blockula.vercel.app`, and your account is `boo1…`.

1. **Own the name** on the demo chain (`https://demo.blockula.app`): register it in the wallet, or with `blockula wallet name blood --rpc https://demo.blockula.app`. A name lasts a year at a time, so renew it.

2. **Have your host serve `blood.blockula.app`.** Add it as a custom domain wherever your site lives (Vercel: Project → Settings → Domains; Netlify: Domain management; GitHub Pages: a `CNAME` file; Fly: `fly certs add blood.blockula.app`). The host gets its HTTPS certificate once the CNAME exists. blockula.app's CAA records let Let's Encrypt, Google Trust Services and Sectigo issue certificates. Vercel, Netlify, GitHub Pages and Fly all use one of those.

3. **Say on your host that the site is yours:** serve your account's address, on a line of its own, at `https://blood-blockula.vercel.app/.well-known/boo`. For a static site, that's a file `public/.well-known/boo` holding the address.

4. **Record it on chain.** Send yourself 0 FANG with the memo `site:v1:<name>:<target>`, from an owner key of the account that owns the name (a session key's record doesn't count):

   ```bash
   blockula wallet send boo1… 0 --memo "site:v1:blood:blood-blockula.vercel.app" --rpc https://demo.blockula.app
   ```

   Or with the SDK: `wallet.send(wallet.account, "0", { memo: "site:v1:blood:blood-blockula.vercel.app" })`. Keep the transaction's id. The demo node keeps about two days of history, so open the pull request within two days, or send a new record.

5. **Open a pull request** adding `sites/blood.json`:

   ```json
   {
     "name": "blood.boo",
     "owner": "boo1…",
     "target": "blood-blockula.vercel.app",
     "record": "<the record's transaction id: 64 hex>",
     "contact": "<your GitHub handle>",
     "listed_at": "2026-11-13"
   }
   ```

Once it merges, the DNS sync adds `blood.blockula.app CNAME blood-blockula.vercel.app` within a few minutes.

| Field | | |
|---|---|---|
| `name` | required | `<name>.boo`. The file is named after it: `sites/<name>.json` |
| `owner` | required | The `boo1…` address that owns the name (never a name) |
| `target` | required | The host that serves your site: a lowercase DNS name with no `https://`, path or trailing dot, at most 200 characters. Not on blockula.app |
| `record` | required | The id of your site record (step 4) |
| `contact` | required | A GitHub handle or an `https://` URL |
| `listed_at` | optional | `YYYY-MM-DD` |

No other fields are taken.

## What gets accepted

`node scripts/check-sites.mjs` checks every listing's shape on every pull request, and CI (`.github/workflows/ci.yml`, job `sites`) also runs `--live` on the listings a pull request adds or changes, checking them against the chain and the target. A listing is refused when:

- a field is missing or has the wrong shape;
- the name is one the project keeps (`www`, `demo`, `wallet`, `api`, `docs`, `testnet`, `mainnet`, `mail` and the rest of `RESERVED_LABELS` in `web/lib/sites.ts`), or reads as Blockula's own or as "official" (`blockula`, `b1ockula-wallet`…), or has `--` as its 3rd and 4th characters;
- the target is an IP address, a local or private name, or on blockula.app;
- the name isn't active, or isn't owned by `owner`;
- the record isn't a finalized transfer from `owner` to itself, signed by one of its owner keys, with exactly the memo `site:v1:<name>:<target>`;
- the target's `/.well-known/boo` doesn't have `owner` on a line of its own.

A maintainer then opens the site before merging. A site that impersonates someone, collects passwords, seeds or payment details, or serves malware is removed.

## Change or remove your site

- **A new target:** add a new `/.well-known/boo` on it, send a new record, and open a pull request with the new `target` and `record`.
- **Remove it:** open a pull request that deletes your file, or an issue asking for it.
- **Your name lapses or changes hands:** the sync, which reads the chain once a day and after every merge, takes the CNAME away. If you renew the name, the site comes back on the next sync. A new owner opens their own pull request.

## How the sync works

`scripts/sync-sites-dns.mjs` (run by `.github/workflows/sites.yml` on main) reads this directory and asks the demo chain, pinned to its genesis, whether each name is active and still owned by its listing's `owner`. It then makes blockula.app's DNS hold a CNAME for each one that is, and none for the rest. It touches only records it made itself, which carry the comment `blockula-sites (sites/README.md)`. A subdomain that already has any other record, such as the demo chain's, is left alone. When the chain can't be read, it changes nothing.

Only the subdomains in this directory are boos' sites. The project's own hosts (`PROJECT_HOSTS` in `web/lib/chains.ts`, such as `demo.blockula.app`) are the only ones blockula.app marks as run by the project.
