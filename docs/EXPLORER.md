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
8. **Honest charts.** Each chart answers one question, and each palette passes a colorblind
   check. Money in uses Prussian blue and money out uses blood red (ΔE 20 under
   deuteranopia, whereas moss and blood failed at 4.7). Direction is always encoded by
   position and sign too, never by color alone.

## Map

| Route | What |
|---|---|
| `/explore` | The pulse: live block ribbon, cadence, proposer share, chain figures, latest transactions, mempool |
| `/block/{height}` | A block in context: neighbors on a ribbon, proposer, hashes, certificate, transactions |
| `/tx/{id}` | The receipt: summary, lifecycle trace, balance changes, signer key, details, raw |
| `/account/{name\|address}` | Balance, money flows by counterparty, keys and agent budgets, activity filterable by key and direction |
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
receipt. When contracts arrive, a contract's ABI maps onto the same interface (parameter types
in, a `call` action out), so contract pages reuse the form, the preview and the receipt unchanged.

## What the node would need for more

All of this runs on the node's existing HTTP API. Things it can't show yet, and the endpoint
each would take:

- **Balance over time.** Balances are only known at the head, and some effects aren't
  transactions (Séance payouts, bond refunds, hunt refunds). An exact balance chart needs a
  per-account balance journal (`/api/account/{id}/balances`) recorded as blocks apply.
- **Chain-wide transaction feed.** Recent transactions are found by scanning recent blocks. A
  `/api/txs?limit=` index would make the feed instant on quiet chains.
- **Certificate signers.** Receipts show the certificate's size. Listing which validators
  signed it needs the decoded signer set in `/api/block/{h}`.
- **Simulation.** Previews are exact for fees and for what gets signed, but they can't
  dry-run state changes. `POST /api/simulate` would return effects before signing.
