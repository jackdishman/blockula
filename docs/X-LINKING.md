# Link an X account

A Blockula account can show which X account is its owner's: `x · @handle` on the account's page and on its `.boo` names, next to the post that proves it. You link it with one public post and one transaction, and you can take it away again.

It needs no protocol change, no API key and no server. The chain sees an ordinary transfer; the rest is read and checked by clients (`@blockula/sdk`, the wallet).

FANG has no value, and a linked handle says only who an account's owner is on X. It isn't an endorsement by the project, whose only X account is [@BlockulaChain](https://x.com/BlockulaChain).

## How it works

A link is two public statements that point at each other.

| | Who says it | What it is | What it proves |
|---|---|---|---|
| **The post** | `@handle`, on X | A post containing a code: `blockula-x-` and 40 hex characters | That X account says "this Blockula account is mine", on this chain |
| **The record** | The account, on chain | A transfer of 0 from the account to itself with the memo `x-link:v1:<handle>:<post id>`, signed by an owner key | The account says "that X account is mine, and here is its post" |

The link holds while both do: the newest record names a post, that post is still up, it's by that handle, and it contains the code for this chain, this account and that handle.

**The code** is `blockula-x-` followed by the first 20 bytes, in hex, of

```
BLAKE3("blockula/x-link/v1" ‖ chain id ‖ genesis hash ‖ address ‖ lowercase handle)
```

with each field after the domain prefixed by its length (u32, little-endian), as in the transaction encoding. It holds no secret. Anyone can compute it, and it means something only in a post by `@handle`. Another chain, account or handle gives another code, so a post can't be reused for any of them.

**The signature** is the record's own. Every transaction is signed with ML-DSA-65 over its chain id, genesis hash, account, nonce and memo, so the record is the account's signed statement naming the handle and the post, on one chain, once. That's why the code is a hash and not a signature: an ML-DSA-65 signature is 3,309 bytes, and a post holds 280 characters.

**Which transactions are records.** A reader counts a transaction as a record only when all of these hold (`xRecordOf` in `sdk-ts/src/xlink.ts`):

- It's a transfer that ran: finalized, no error.
- It's from the account to itself. A payment to someone else with that memo is never a record, so nobody can get one made by asking to be paid "with this memo". The wallet's Send page refuses to pay yourself, so a payment link can't make one either.
- An owner key of the account signed it, and that key is still on the account. Session keys (an agent's budget) can send transfers, memo included; they don't speak for who the account is.
- Its memo is exactly `x-link:v1:<handle>:<post id>` (a handle is 1 to 15 of `A–Z a–z 0–9 _`, a post id is digits) or exactly `x-unlink:v1`.

The newest record wins. `x-unlink:v1` ends the link.

## Link yours

### In the wallet

Open **Link X** on your account's page (or `/x` in the wallet, or "Link your X account" in the command palette).

1. Type your handle. The page shows the post, with your code in it, and a button that opens X with it filled in. If your account has a `.boo` name, the post links its page on this chain (`https://<node>/boo/<name>`); it never writes the name as `you.boo`, which X would turn into a link to an unrelated website.
2. Post it from that handle, as it is.
3. Paste the post's link and press **Preview**, then **Sign & send**. It costs the fee and moves nothing.

Your account's page now shows `x · @handle` and **claimed · proof post**, which opens your post. To change it, link another handle: the new record replaces the old. To remove it, **Unlink** on the same page, or delete the post.

### With the SDK

```ts
import { Client, Wallet, verifyXLink } from "@blockula/sdk";

const proof = await wallet.xProof("@BlockulaChain");
// proof.text is what to post; proof.intent_url opens X with it filled in.

await wallet.linkX("BlockulaChain", "https://x.com/BlockulaChain/status/1234567890123456789");

const link = await client.xLink("vlad.boo");     // { handle, post_id, post_url, code, tx, … } or null
const check = await verifyXLink(link);            // { verified: true, reason: "@BlockulaChain posted blockula-x-…" }

await wallet.unlinkX();
```

`linkX` and `unlinkX` need an owner key and refuse a session key before anything is signed.

## Check someone's link

`client.xLink(account)` reads the account's newest record from a node. That's the account's **claim**. `verifyXLink(link)` fetches the post and checks it, which makes it a verified link:

```sh
npm run build -w @blockula/sdk
node scripts/verify-x-link.mjs vlad.boo                  # a local node
node scripts/verify-x-link.mjs vlad.boo https://a.node   # another one
```

The post is fetched from X's public oEmbed endpoint (`https://publish.x.com/oembed`), which needs no key and no account. The request holds the post's id and nothing else. The author comes from the answer's `author_url`, never from the post's text, and the code must be in the post's own text.

By hand: open the proof post. It must be posted by the handle the account shows, and contain the code the wallet shows when you hover **claimed · proof post**.

The wallet's pages show the handle and link the post, and don't fetch it. A page in the wallet talks only to its node (its Content-Security-Policy allows nothing else, and X sends no CORS headers), and most wallets are static files a node serves, with no server to ask. So in the wallet a link reads as the account's claim ("claimed") with its proof one click away; the SDK and the script check it.

## What it does and doesn't promise

- **A node can leave a record out, never make one up.** Records are read from one node's history (`/api/account/{id}/txs`), which carries no Merkle proof. A lying node could show an old link or none. It can't show a link that verifies for a handle whose owner never posted this account's code.
- **Deleting the post breaks the link.** So does the handle changing hands: the check compares the post's author now.
- **Pruning nodes forget.** Validators keep about two days of blocks by default, so a record older than that is gone from them. Ask a node that keeps everything (a full node, or one run with `--archive`).
- **Busy accounts.** The wallet looks through an account's newest 525 transactions for a record (the SDK, 2,000 by default, `maxPages`). Past that, link again: it's one cheap transaction.
- **Removing an owner key** drops the records it signed. Link again from a key that's still on the account.
- **Multisig accounts** can't link yet: their owner keys act only through proposals, which carry no memo.
- **A record is public and permanent**, like every transaction. Unlinking ends the link; the old records stay in history.
- **Handles and posts are strangers' text.** Readers take only the exact shapes above and never act on what a post says.

## Why not a new transaction type

The chain has a native claim for GitHub (`link_github`, tag 8): one handle per account, one account per handle, kept in state and served by `/api/account`. The same for X would be tidier: an indexed record, a Merkle proof, uniqueness enforced by the chain, nothing lost to pruning.

It would also be a protocol change: a new action tag in the transaction encoding (`node/src/types.rs` and `sdk-ts/src/tx.ts`), new state, a protocol version to gate it so v1 history still replays ([REPLAY.md](REPLAY.md)), and an upgrade every validator of every running chain has to take before the first link. The memo record works on every chain today, the demo chain included, and loses only what's listed above. If linking proves worth it, a native `link_x` can be added later and read alongside the memo records.
