# Blackjack X bot (dry run)

The bot that lets a player play Blockula blackjack from X: `@BlockulaChain deal 5`, then `hit` or `stand` as replies. It plays the player's **table balance** through the table program's relayer path, within the grant the player gave it.

**This build cannot post.** It has no X client, no X credentials and no code that opens a connection to X. It reads posts it is handed (JSON lines from a file or stdin) and writes each reply as files in a directory. A person reads those files; nothing is sent anywhere. `test/no-x.ts` fails if a module of this package names an X host, imports a network client, or imports the SDK's functions that fetch from X.

It also signs only on a local chain: the node must be on this machine (`http://127.0.0.1:<port>` or `http://localhost:<port>`), and a local node that follows a chain other people share is refused.

FANG has no value and can't be bought; no prizes. It is a game played with FANG.

## Commands

A post is a command only if it is nothing but mentions and one command:

```
post     = ws* mention (ws+ mention)* ws+ command ws*
mention  = "@" 1–15 of [A-Za-z0-9_]      at least one is @BlockulaChain (any case)
command  = "deal" ws+ amount | "hit" | "stand" | "balance" | "stop"     (any case)
amount   = ("0" | [1-9][0-9]{0,8}) ("." [0-9]{1,9})?                    more than zero
ws       = space | tab | line feed | carriage return
```

The leading mentions are the ones X puts in front of a reply. Everything else is ignored without a reply: extra words, two commands, a handle that only looks like the bot's (`@BlockulaChain_`), any character outside printable ASCII (look-alike letters, zero-width characters), an amount with a sign, an exponent, a comma, a leading zero or more than nine decimals. Amounts are parsed with whole-number maths.

| Command | What the bot does |
|---|---|
| `deal N` | `relay_bet` of N FANG from the player's table balance. One open hand a player. |
| `hit`, `stand` | `relay_hit` / `relay_stand`. Must be a reply in the hand's thread (under the deal, the player's own moves, or the bot's replies about the hand) by the hand's player. Anyone else's reply there is ignored. |
| `balance` | Reads the table balance and what's left of today's limits. Signs nothing. |
| `stop` | `relay_withdraw`: the player's whole table balance goes back to the player's own account (the only place a relayer can send it). The player is then **stopped**: nothing they post is answered until a `deal` goes through, which needs a deposit they make themselves. Refused while a hand is open. |

Post text, handles and chain text are data. The bot never reads a post for anything but this grammar, and never puts a post's text or a program's text in a reply.

## Run it

```sh
npm run build -w @blockula/sdk -w @blockula/blackjack -w @blockula/blackjack-render -w @blockula/blackjack-xbot

export BLACKJACK_XBOT_HOME=$(mktemp -d)        # the key, the store, config.json
export BLOCKULA_RPC=http://127.0.0.1:38500     # a local node with programs on
export BLACKJACK_TABLE=boo1…                   # the table's address

blackjack-xbot init                            # makes the relayer's key, prints its address
# The table's owner approves that address (approve_relayer); each player grants it (set_relayer).
blackjack-xbot run --posts posts.jsonl --out replies/ --proofs proofs.json
blackjack-xbot status
```

`blackjack-xbot` is `node services/blackjack/xbot/dist/cli.js` after the build. `--posts -` reads stdin. A post is one JSON object a line:

```json
{"id":"1900000000000000001","authorHandle":"Mina_H","text":"@BlockulaChain deal 5","inReplyToId":null,"createdAt":"2026-11-13T12:00:00Z"}
```

`id` is a decimal u64. `inReplyToId` is the post it replies to. A reply this build wrote has the id `dry-<post id>-<n>`, so a later post in the file can reply to it.

### What comes out

One folder a reply, `<out>/<post id>-<n>/`:

- `reply.txt`: the text, at most 280 as X counts. It states the cards and totals, what to reply next, the outcome and what came back to the table balance, so it is complete without the picture. A reply that starts or ends a hand carries the note above.
- `image.png` and `image.alt.txt`: on a reply about a hand, the table's picture (1200 × 675, from `@blockula/blackjack-render`) and its alt text. Refusals, `balance` and `stop` have no picture.
- `meta.json`: the post it answers, the player's handle (data), the account, the hand, the action, the transaction id and height, and `"posted": false`.

A folder is written under another name and renamed into place. Writing a reply again replaces its folder.

## Settings

Flags, then the environment, then `config.json` in the home directory, then the defaults.

| Setting | Flag / environment | `config.json` | Default |
|---|---|---|---|
| Home directory | `--home`, `BLACKJACK_XBOT_HOME` | | `~/.blockula/blackjack-xbot` |
| Local node | `--rpc`, `BLOCKULA_RPC` | `rpc` | none |
| Table address | `--table`, `BLACKJACK_TABLE` | `table` | none |
| Least time between one handle's commands | | `cooldownMs` | 3000 |
| Most commands a handle an hour | | `perHour` | 60 |
| Most commands a minute, over everyone | | `globalPerMinute` | 120 |
| Least time between refusal replies to a handle | | `refusalCooldownMs` | 60000 |
| Most refusal replies to a handle an hour | | `refusalsPerHour` | 6 |
| How long a checked X link is kept | | `linkTtlMs` | 600000 |
| Least time between listings of the table's accounts | | `listEveryMs` | 30000 |
| How long to wait for the dealer before replying "waiting" | | `revealWaitMs` | 20000 |
| Posts older than this are ignored (0: off) | | `maxPostAgeMs` | 0 |
| Handles never answered | | `blocked` | none |

## Rate limits

- A command counts when the bot handles it. One too soon after the handle's last (`cooldownMs`), past the handle's hour (`perHour`) or past everyone's minute (`globalPerMinute`) is recorded and not answered.
- **Refusals are rationed apart from that**, so the bot can't be made to post over and over: a handle gets at most one refusal reply every `refusalCooldownMs` and `refusalsPerHour` an hour. Further refused commands are recorded as refused, without a reply.
- Refused with a reply: no linked account, a link that didn't check out, one handle on two accounts, no grant, an expired grant, a balance too low, a bet outside the limits or past today's, a paused table, a hand already open, a move outside the hand's thread, a move while the dealer still owes cards.
- Never answered: posts that aren't commands, duplicates, the bot's own posts, stopped or blocked handles, someone else's move under a hand.
- The limits are kept in the store, so a restart doesn't reset them.

## From a handle to an account

The chain's X link goes from an account to a handle ([docs/X-LINKING.md](../../../docs/X-LINKING.md)): a transfer of 0 to itself whose memo names the handle and a proof post. No node indexes links by handle, so the bot keeps a registry in its store:

1. **Which accounts.** The accounts whose grant at the table names this relayer, read from the table's own storage (every player record), plus any enrolled by hand (`blackjack-xbot enroll <address>`).
2. **Their claims.** Each account's link is read with `client.xLink(account)`.
3. **The check.** The claim is checked against its proof post by the verifier the bot was given. In this build that is always a local stand-in: `--proofs <file>` names a JSON list of `{ "id", "handle", "text" }` and a claim holds if the post with its id is by its handle and contains the account's code (`checkXProof`, which is pure). Without `--proofs`, no link verifies and nobody is played for. The real check, which fetches the post from X, is not imported anywhere in this package.
4. **Kept, then checked again.** A checked link is kept for `linkTtlMs`, then read and checked again, so a deleted proof post or an unlink ends play within that time. An unknown handle makes the bot list the table's accounts again at most every `listEveryMs`.

A handle resolves only when **exactly one** account's checked link names it. Two accounts with checked links to one handle are both refused, and the reply says so. An account that only claims a handle (its proof post is by someone else, or missing) is never played for, and never blocks the account whose proof is good.

What this registry can't do: a node can leave a link record out (see X-LINKING.md), so a node that hides a newer unlink could keep an old link alive until the proof post is deleted.

## The store

`<home>/store.json`: one JSON file, replaced whole on every commit (written beside itself, synced, renamed). It holds post ids and how far each got, open hands and their threads, stopped handles, the rate limits' timestamps, the link registry, and the chain and table it belongs to (it refuses any other). It holds no key and no secret.

**A post is handled exactly once, across restarts.** Each command is recorded before every step that can't be taken back:

| State | Written | Means |
|---|---|---|
| `intent` | before a transaction is signed | the bot decided to act |
| `sent` | after signing, **before** submitting (the SDK's `onSigned`) | the transaction's id and last valid block are on disk |
| `acted` | when it ran | the action happened; the reply isn't written yet |
| `done` | after the reply | answered, refused or ignored |

At start (`recover`) every post that isn't `done` is finished from the chain:

- `intent`: nothing was signed, so nothing was submitted. The table is asked all the same; if its state doesn't show the action, the command is handled afresh.
- `sent`: the transaction is looked up by id. If it ran, its receipt's `relayed` event (which carries the post's id) is the proof. If the table refused it, the player gets the refusal. If the node doesn't know it, the table's state answers: an open hand carries the post id of its bet, a settled hand's seed is derived from the post id, a hit shows as one more card, a stand as the dealer's turn. If none of that shows it and its last valid block has passed, it can never run, and the player is told to post again. Until then the bot waits.
- `acted`: the reply is written. A reply's folder is named for its post and its number, so writing it twice gives one folder.

A post already in the store is never handled again, whatever state it's in. The bot's own `timeout` on a hand whose dealer missed its deadline is recorded the same way, under the hand's id.

## The key

`init` makes the relayer's key once: `<home>/relayer.key.json`, mode 0600, holding the seed and nothing else. It is never overwritten, never printed, never logged and never passed on a command line; the bot refuses a key file others can read. Back it up yourself. The relayer's account pays the fees of the calls it relays, so it needs a little FANG.

What the key can do is what the table program lets a relayer do: bet, hit and stand from the table balance of a player who granted it, within that grant's limits and expiry, and send a player's table balance back to that player's own account. It can't send a balance anywhere else.

## Tests

```sh
npm test -w @blockula/blackjack-xbot          # offline: types, parser, copy, store, limits, links, bot, the 100 threads, no X
npm run fixtures -w @blockula/blackjack-xbot  # rewrite test/fixtures (deterministic; commit the result)
npm run test:live:chain -w @blockula/blackjack-xbot   # a one-validator chain on 127.0.0.1:38500 (p2p 38520) and the live test on it
BLOCKULA_RPC=http://127.0.0.1:38500 npm run test:live -w @blockula/blackjack-xbot   # the live test on a local chain already running
```

- `test/parse.ts`: 130 posts, accepted or ignored.
- `test/copy.ts`: every reply the bot can write, with the longest amounts and hands: at most 280, no banned word, the note where it belongs.
- `test/store.ts`: the process killed before signing, between signing and submitting, after the transaction ran, before and after the reply was written, for each command; then restarted from the file.
- `test/limits.ts`, `test/links.ts`, `test/bot.ts`: rate limits on a fake clock, link checking, the rules of threads, stop and balance, the dry run's files, the key file.
- `test/threads.ts`: 100 scripted threads (`test/fixtures/threads.json`) replayed over an in-memory table with real hash chains; every reply must equal `test/fixtures/expected.json`, every hand must settle and verify, and the table balances must reconcile.
- `test/no-x.ts`: no module can reach X.
- `test/live.ts`: a real local chain, the real table program, the real renderer.

## Before this could go live

None of this is built. Each item needs its own review.

- [ ] An X client behind interfaces the bot already has seams for: a source of posts (mentions, with ids, authors and reply targets as X reports them, never parsed from text) and a `ReplySink` that posts and returns the real post id. It lives outside this package's dry-run build, and `test/no-x.ts` keeps guarding this one.
- [ ] Posting exactly once: record a reply as "about to post" before posting and reconcile after a crash (a reply that was posted but not recorded must not be posted twice). Today a rewritten reply just replaces a folder.
- [ ] The real link check (`verifyXLink`) as the injected verifier, with its failure modes handled (X unreachable, rate limits), and a decision on how long a link may be trusted.
- [ ] Credentials: where X credentials live, who can read them, rotation, and that they never reach logs, the store, argv or replies. The same review for the relayer key on a server (an HSM or a secrets manager rather than a file).
- [ ] The local-only guards (`localRpc`, `sharedChain`) replaced by an explicit, pinned chain (chain id and genesis hash), with witnesses for reads that matter, and a decision on which chains the bot may ever sign for.
- [ ] Media upload with alt text, and a check that X's own length count agrees with `xLength` for every template.
- [ ] Abuse review: reply rate limits against X's rules, blocked handles, what the bot does when it is mentioned in bulk, a kill switch, monitoring and alerts on refusals, timeouts and unfinished posts.
- [ ] The store: pruning old post records (it only grows today), a database if several processes ever run, backups.
- [ ] Copy and policy review of every reply for the platform's rules on games, and of the note, in every language it will post in.
- [ ] An independent security review of the relayer path end to end, and a staged rollout with low grant limits.
