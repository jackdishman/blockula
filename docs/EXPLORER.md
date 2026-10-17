# The explorer lives in the wallet

The web wallet (`web/`) is also Blockula's explorer. Every block, transaction, account, name,
hunt, claim and Séance has a public page in the app. You don't need a wallet to read them.
When you're signed in, the same pages are written from your side and let you act on what you're
looking at. The node's built-in explorer (`node/src/explorer.html`, served at `{rpc}/#/…`)
stays as a dependency-free fallback for operators and for links the MCP and CLI print.

## Why merge instead of linking out

- **One context.** You never jump to another site, with another design and another search, to
  check what you just did. A receipt is one click from the payment and one click back.
- **Perspective.** An explorer that knows who you are can say "You sent 5 FANG to alice.boo"
  and "signed by your agent *claude*", instead of showing two addresses you have to compare.
- **Act in place.** An account page can pay that account, a name page can register the name,
  and a hunt can be funded from the hunt. Nothing needs a "connect wallet" step, because the
  wallet is already there.
- **Contracts later.** Using a contract means reading its state, building a call, previewing it
  and signing it. That only works well where the signer already lives. The Programs pages
  build that loop today for the chain's built-in modules (see below).

## Principles

Borrowed from the explorers worth borrowing from, without copying any of them:

1. **Receipts, not hex dumps.** Every transaction page opens with the node's plain-English
   summary and what changed for whom. Hashes, nonces and raw JSON fold away underneath.
   Tempo calls its transaction pages "receipts", and that's the right word.
2. **Balance changes are the truth.** Every receipt lists who gained, who lost and what was
   burned (the Solscan "balance changes" table). The account flow chart is built from the same
   figures. Effects we can't read off the transaction itself are labeled, never guessed.
3. **Trace the lifecycle.** Signed (by which device or agent key), pending, sealed into a block,
   finalized by a quorum certificate. For an agent-first chain, "which key did this" is the
   question that matters most, so it's on every receipt and filterable on every account.
4. **Live, not refreshed.** New blocks slide into the pulse. Pending transactions resolve in
   place, and charts keep their frame while they refetch.
5. **Search is a command line.** Press ⌘K (or `/`) anywhere to jump to a name, address, block or
   transaction, or to do something ("send to alice.boo", "register ghoul.boo").
6. **Names first.** Show `.boo` names wherever one exists, with the address one copy away.
7. **Teach in place.** Underlined terms explain themselves on hover and focus, using the same
   glossary as the node's explorer.
8. **Follow the money.** Any account's flows open into a graph you can walk hop by hop:
   payers to the left, payees to the right, width for amount (the "visualizer" idea from
   investigative explorers, built from the same receipts).
9. **Know before you sign.** Every preview says what will change for each side, your balance
   and agent budget after it, and whether the chain will refuse it, in words.
10. **Hear about it.** Money moving on your account (an agent spending, another device signing,
   a payment arriving) or on accounts you watch raises a notice wherever you are in the app.
11. **Honest charts.** Each chart answers one question, and each palette passes a colorblind
   check. Money in uses Prussian blue and money out uses blood red (ΔE 20 under
   deuteranopia, whereas moss and blood failed at 4.7). Direction is always encoded by
   position and sign too, never by color alone.

## Map

| Route | What |
|---|---|
| `/explore` | The pulse: live block ribbon, cadence, proposer share, chain figures, latest transactions, mempool |
| `/block/{height}` | A block in context: neighbors on a ribbon, proposer, hashes, certificate, transactions |
| `/tx/{id}` | The receipt: summary, lifecycle trace, balance changes, signer key, details, raw |
| `/account/{name\|address}` | Balance, money flows by counterparty, keys and agent budgets, activity filterable by key and direction (and exportable as CSV), watch |
| `/trace/{name\|address}?follow=` | The money graph around an account; `follow` lists accounts expanded a hop further, so a trace is a shareable link |
| `/name/{name}` | Owner, or price and a register button |
| `/programs` | The chain's built-in modules, each with its state, its calls and its actions |
| `/programs/{names\|hunts\|pool\|validators\|accounts}` | One program |
| `/hunt/{id}`, `/claim/{id}`, `/seance/{id}` | Program records |

## Programs: the contract surface, today

Blockula has no user contracts yet, but it has five native programs: Names, Hunts, the
Contribution pool, Validators, and Accounts & keys. Each program is described by an interface
(`web/lib/explorer/programs.ts`) that lists:

- **reads**: the state it exposes and where the node serves it
- **calls**: the transaction kinds that belong to it, so its page can show recent calls
- **actions**: typed parameters (`fang`, `account`, `name`, `int`, `text`, `repo`) and a
  `build()` that turns them into an SDK `Action`

One generic form renders any action. It validates each parameter by type, builds and signs the
transaction locally (`wallet.build`), and previews exactly what will be signed: sender, signing
key, fee, size, nonce and the action itself. Only then does it submit, and it lands on the
receipt.

The preview includes a **preflight** (`web/lib/explorer/preflight.ts`, also on the wallet's Send
review): the balance changes the receipt will show, your balance and agent budget after it, and
the node's admission rules checked in advance (enough FANG counting pending transactions, the
new-account minimum, an agent key's limit and expiry, hunt minimums and state). The node refuses
such a transaction outright and charges nothing, so the preflight saves a round trip and says
why in words. It's a forecast from the chain's state now; the node's answer is final. When contracts arrive, a contract's ABI maps onto the same interface (parameter types
in, a `call` action out), so contract pages reuse the form, the preview and the receipt unchanged.

## Watching and notices

Watched accounts are kept in the browser, per node (`web/lib/explorer/watch.ts`), and listed on
the pulse. While the app is open, `components/explorer/Alerts.tsx` polls your account and the
watched ones and announces only news (the first read is the baseline): payments in, an agent or
another device spending from your account (with the agent's budget so far), your own
transactions sealing once you've left the page you sent them from, and watched accounts' moves.
A hidden tab checks only your own account, less often, and counts unread notices in its title.

## What the node would need for more

All of this runs on the node's existing HTTP API. These endpoints would close the gaps that
remain. They're best built on the read path the mainnet work introduces (`ReadView` in
`node/src/view.rs`) rather than on today's handlers:

- **Balance over time.** Balances are only known at the head, and some effects aren't
  transactions (Séance payouts, bond refunds, hunt refunds). An exact balance chart needs a
  per-account balance journal (`/api/account/{id}/balances`) recorded as blocks apply.
- **Chain-wide transaction feed.** Recent transactions are found by scanning recent blocks. A
  `/api/txs?limit=` index would make the feed instant on quiet chains.
- **Certificate signers.** Receipts show the certificate's size. Listing which validators
  signed it needs the decoded signer set in `/api/block/{h}`.
- **Simulation.** The preflight mirrors the node's admission checks and reads effects off the
  transaction's fields. The node already dry-runs every transaction on admission; exposing that
  as `POST /api/simulate` (the journal's balance diffs, or the refusal) would make previews exact
  for every kind, including effects the fields don't carry.
- **Deeper traces.** A trace reads each account's 25 latest transactions. Paging an account's
  history (`/api/account/{id}/txs?before=`) would let it reach further back.
- **Push.** Notices poll. A server-sent event stream of finalized transactions per account would
  make them instant and cheaper.
