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
- **Programs anyone deploys.** Using a deployed program (protocol v2,
  [PROGRAMS.md](PROGRAMS.md)) means reading its state, building a call, previewing it and
  signing it. That only works well where the signer already lives. The Programs pages build
  that loop for the chain's built-in modules, and read deployed programs (see below).

## Principles

Borrowed from the explorers worth borrowing from, without copying any of them:

1. **Receipts, not hex dumps.** Every transaction page opens with the node's plain-English
   summary, what was signed (rendered here from the action itself with the SDK's
   `describeAction`: the node's summary isn't signed), and what changed for whom. Hashes,
   nonces and raw JSON fold away underneath. Tempo calls its transaction pages "receipts", and
   that's the right word.
2. **Balance changes are the truth.** Every receipt lists who gained, who lost and what was
   burned (the Solscan "balance changes" table), read the way protocol v1 runs it: the charged
   `fee` is the block's base fee (burned) plus the `tip` (to the block's proposer), and `to` is
   the account the action paid when it ran (`to_name`: the name it was signed to). The account
   flow chart and the CSV export are built from the same figures. Effects we can't read off the
   transaction itself (a name's price, a bounty paid from escrow, the account-creation fee a
   first payment burns) are labeled, never guessed, and the CSV's `exact` and `note` columns
   say which rows they touch.
3. **Trace the lifecycle.** Signed (by which device or agent key), pending, sealed into a block,
   finalized by a quorum certificate. A transaction dropped from the mempool unrun charged
   nothing (and another node may still include it until its `valid_until`), while one that ran
   and failed was charged its fee: receipts say which. For an agent-first chain, "which key did
   this" is the question that matters most, so it's on every receipt and filterable on every
   account.
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
| `/explore` | The pulse: live block ribbon, cadence, proposer share, chain figures (with the boos active this week: `/api/status`' `pulse`), latest transactions, mempool |
| `/boos` | Boos: everyone on the chain by `.boo` name (`/api/accounts`, `/api/names`): richest, newest active or alphabetical, a search box, the newest names, and Send on each row |
| `/block/{height}` | A block in context: neighbors on a ribbon, proposer, hashes, certificate, transactions |
| `/tx/{id}` | The receipt: summary, lifecycle trace, balance changes, signer key, details, raw |
| `/account/{name\|address}` | Balance, money flows by counterparty, keys and agent budgets, activity filterable by key and direction (and exportable as CSV), watch; Pay on someone else's, and Send, Receive and Request (a prefilled Send link with a QR code, which the payer reviews and signs) on your own |
| `/boo/{name}` | A boo's profile, by its `.boo` name (`/boo/vlad`): the name, the GitHub and X handles it links (each with how far it's been checked), Soul, hunts won, contributions claimed, its other names. Balances, keys and activity stay on `/account`. A node also answers `{node}/boo/{name}` and sends it here, so it's the link to share |
| `/trace/{name\|address}?follow=` | The money graph around an account; `follow` lists accounts expanded a hop further, so a trace is a shareable link |
| `/name/{name}` | Status (active; expired, in its grace period, when only its owner can renew it; reserved; free), owner, expiry, a pending offer, and the owner's renew, offer and release actions (or the recipient's accept); or the price and a register button |
| `/leaderboard` | Three boards worked out in the browser from the node's own reads, no node changes: the most guestbook signatures (`/api/account/{guestbook owner}/txs`, up to 5 pages), trick or treat players by net (`trickortreat.boo`'s history: plays, winnings, never memos) and Soul (`/api/souls`). Names are strangers' text, shown inert |
| `/guestbook` | The guestbook: every payment to `guestbook.boo`'s owner with its memo, newest first (`/api/name/guestbook`, `/api/account/{owner}/txs`), and how to sign it from the wallet or an agent. Memos are strangers' text, shown inert |
| `/x` | Link, change or remove this wallet's X account ([X-LINKING.md](X-LINKING.md)): the proof post with its code, then the record, a transfer of 0 to itself with an `x-link:v1:…` memo (`X_ACTIONS` in `lib/explorer/programs.ts`). Account and name pages show the handle beside its proof post (`components/explorer/XLink.tsx`); the page can't reach x.com, so it links the post instead of checking it |
| `/programs` | The chain's built-in modules, each with its state, its calls and its actions, plus deployed programs: featured pins for this chain, and creates from recent blocks |
| `/programs/{names\|hunts\|pool\|validators\|accounts}` | One program |
| `/program/{address}` | A program anyone deployed: its record (creator, code and interface hashes, version digest, who could upgrade it, balance, storage), its methods as its deployer named them, the calls that named it, forms that run its views and (for an owner key, after a review showing the digest it binds) sign calls to it; or, for a code's 64-character hash, what's stored |
| `/hunt/{id}`, `/claim/{id}`, `/seance/{id}` | Program records |

## Programs: native, and deployed (protocol v2)

The chain has five native programs: Names, Hunts, the
Contribution pool, Validators, and Accounts & keys. Each program is described by an interface
(`web/lib/explorer/programs.ts`) that lists:

- **reads**: the state it exposes and where the node serves it
- **calls**: the transaction kinds that belong to it, so its page can show recent calls
- **actions**: typed parameters (`fang`, `account`, `validator`, `name`, `int`, `years`, `bps`,
  `days`, `text`, `repo`, `key_id`) and a `build()` that turns them into an SDK `Action`

One generic form renders any action. It validates each parameter by type (an address against
the chain's own prefix, `web/lib/explorer/fields.ts`), builds and signs the transaction locally,
and previews exactly what will be signed: sender, signing key, fee, nonce, the action rendered
from itself, how each name in it is signed, and a hunt's expiry. Only then does it submit, and
it lands on the receipt. It signs by the SDK's rules:

- **Names.** In a field the chain resolves itself (`account`: a payment's recipient, a hunt's
  winner, a vouch, a name offer) a name is signed as `Client.recipient` says: its proven owner
  with witnesses, else the name itself, so a lying node can make the transaction fail but not
  send it elsewhere. A field that needs an address (`validator`) takes `Client.resolution`'s,
  handed to `Wallet.build` (`resolutions`), so mainnet refuses one only the node vouched for
  against the very status it signs for (`UnprovenNameError`).
- **Heights.** An action that carries a height (a hunt's expiry) is built by
  `wallet.build((height) => …)` from the status read that `valid_until` is counted from, and
  days are the chain's fixed block time (`BLOCKS_PER_DAY`), never the node's word.
- **What isn't offered here.** Votes, multisig approvals and Séance judgments bind what was
  shown (a digest), so they're signed where it's shown: Governance, Shared accounts
  (`/shared/review`, which renders a pending action from the action and signs its digest), and
  the CLI's `gov vote`, `multisig approve` and `contrib vote`. Keys join an account only through
  their grant code, on Devices & agents and Shared accounts. A multisig account's page shows its
  threshold, owners and open actions, each with a link to its review page; Shared accounts
  (`/shared`) proposes these same forms' actions for one, wrapped in `multisig_propose`
  (`web/lib/multisig.ts` `asProposal`). Unstaking is by amount (the chain converts it at its own
  price); all of a stake is on Stake. Browser-only keys never open on mainnet, here as in the
  wallet's own unlock.

The preview includes a **preflight** (`web/lib/explorer/preflight.ts`, also on the wallet's Send
review): the balance changes the receipt will show, your balance and agent budget after it, and
protocol v1's admission rules checked in advance: enough FANG counting pending transactions;
what the account can move (`/api/account` `transferable`: vesting and vote locks hold the rest,
though locked FANG can be staked, and a tip must come from what can move); the
account-creation fee a first payment burns (on the sender's cost and an agent's budget); the
new-account minimum; an agent key's limit and expiry, and that it only sends transfers; a
multisig's owner keys, which only propose and approve; hunt minimums and state, that only the
creator awards, and that the creator cancels only after the expiry. The node refuses such a
transaction outright and charges nothing, so the preflight saves a round trip and says why in
words. It's a forecast from the chain's state now; the node's answer is final. `checkTx` reads
the node and `preflightInput` (pure) turns the reads into the preflight's input, so
`web/lib/v1.test.mjs` replays a dev chain's own answers (`web/lib/v1-vectors.json`, made by
`scripts/explorer-vectors.mjs`): the preflight must clear what the chain took, at what left the
sender, and refuse what it refused, and a receipt's exact balance changes must be what the
balances did.

**Deployed programs** (a chain that has switched programs on) are listed beside these (`Program.kind` is `"native"` or `"deployed"`, `deployedProgram()`), and have a page of their own, `/program/{address}`, where its methods can be run and called (`components/explorer/ProgramCalls.tsx`). The forms are made from the program's record as the SDK checked it (`client.program`: every method name an identifier with its own selector, every type in the interface's vocabulary, the version digest recomputed), never from the node's unchecked answer: a record that doesn't check out gets no forms, and the page says why. `web/lib/explorer/program-calls.ts` (`specsFromProgram`) maps the interface onto the same shapes as the built-in programs ([PROGRAMS.md](PROGRAMS.md) section 11.3): each `call` method is an `ActionSpec` (`kind: "call_program"`, owner keys only) bound to the digest of the record the page shows, each `view` a read form; a field per argument, keyed by position (never by the deployer's names), typed by its interface type (`ParamType` adds `bool`, `u64`, `i64`, `u128`, `i128`, `address`, `hex`, `list` and `json`). Arguments are encoded by the SDK's ABI codec (`encodeProgramInput`, Borsh by convention: [PROGRAMS-V0.md](PROGRAMS-V0.md) §3.2), whose bytes are held to the node's `borsh` crate (`sdk-ts/test/program-abi-vectors.json`); each field shows what the codec would refuse (text over its bound in bytes, an integer outside its range, an address with a bad checksum or another network's prefix). An address field takes an address only, never a `.boo` name, which moves with its owner; it offers your own in one click. An option of a plain type is a field left empty for none, a vec of one a list of fields up to its bound, and anything deeper one JSON field the codec holds to the type.

- **A view** runs on your node (`client.view`), signed out too, and signs and pays nothing. What it returned is decoded by its declared type (`decodeProgramValue`) and shown as the program's claim: text without what hides or reorders it (and saying so), amounts with their drops, the raw bytes beside.
- **A call** has a value field only when the method is payable, and a storage limit (0 by default: the page never guesses one from a method's name, and says plainly that a method that writes traps at 0 and still pays its fee). Review builds the exact transaction with the SDK (`Wallet.call`, which prices the fuel it reserves: 11.3's `prepare`, without simulation) bound to the displayed digest, stopping before anything is signed, and refuses it unless it's the spec's own call (`sameCall`). The review shows the program, the method, the arguments read back from the input, the input's hex, the value, the storage limit, **the full version digest** it binds, the fuel it reserves, the max fee (fuel included) and the most that leaves the account; "Sign & send" asks the passkey again and signs that very transaction. Then it shows the node's `summary`, the outcome in the SDK's words (`outcomeWords`, which says when a call trapped for want of a storage limit), what it returned (decoded, as a claim) and a link to its receipt. A call the node refuses at admission (a program that changed since the review, programs paused or exit-only, past the value cap) says so in the chain's words and cost nothing; one whose fate isn't known (pending, dropped, no answer) is to be checked by its id, and the page never offers to send it again.
- **Who can call**: only an owner key of a personal account signs a program transaction. On a device that signs with an agent (session) key, or on a shared (multisig) account, which can't propose a program call yet, the form says why instead of offering to sign. Signed out, views run and calls say to sign in.

`blockula program view` and `blockula program call` stay on the page as the way to do the same from a terminal.

Everything a deployed program or its deployer chose (a method's, an argument's or an event's name, an abort message, what a call returned) is a stranger's text. The explorer shows it as text, in a box of its own (`.named`), with what hides or reorders text removed and cut short (`web/lib/explorer/inert.ts`), marked "named by the deployer", and never in its own sentences; it's never markup, and the node's own explorer (`node/src/explorer.html`) escapes it the same way. A program transaction's receipt (`ProgramReceipt`) shows **facts** (the outcome in words from its numbers, the fee in bytes, fuel and tip, the fuel reserved and used, deposits and the account creation fee that left outside the program's own diff, the balance changes its programs made, what its programs' own sends burned (`burned_by_programs`: the creation fee of an account a program opened, out of the programs' balance), the program a create made, what an upload stored) apart from **claims** ("what the program reports": what each call returned, the events, an abort message). `lib/explorer/ledger.ts` reads the facts as money: the signer's balance moves by exactly −(fee + what left outside the diff) + the program diff's entry for it, deposits sit bonded, the creation fee burns, and what the programs burned is the receipt's own `burned_by_programs`, which must equal what conservation implies (a receipt that doesn't balance is never read as money); `scripts/explorer-program-vectors.mjs` records a `blockula dev --programs` chain's own answers (`web/lib/program-vectors.json`) and `web/lib/program.test.mjs` holds the ledger to the balances they moved. (`web/lib/v1-vectors.json` is the v1 file and is left as it is: its script makes new wallets and names every run, so a fresh run against a `blockula dev --programs` chain is compared with it by `node scripts/vdiff.mjs web/lib/v1-vectors.json fresh.json`, which normalises the random wallets, names and numbers and exits 1 on any difference in a refusal, a result, a balance change or a read's key paths.) "What was signed, exactly" (the node explorer's "Raw data") abridges only the blobs a program action carries (`wasm`, `interface`, `input`): every v1 action prints exactly as `JSON.stringify(action, null, 2)` does, an added key's public key and its proof whole. Until the node serves a list of programs, the Programs page lists the creates in a window of recent blocks, and says so; an older program is opened by its address unless this wallet pins it. To add a featured program, append its address with this wallet's title and description under that chain's id and genesis hash in `FEATURED_PROGRAMS` (`web/lib/explorer/featured.ts`): the page shows those pins above the recent creates, and hides one the node doesn't have.

## Watching and notices

Watched accounts are kept in the browser, per node (`web/lib/explorer/watch.ts`), and listed on
the pulse. While the app is open, `components/explorer/Alerts.tsx` polls your account and the
watched ones and announces only news (the first read is the baseline): payments in, an agent or
another device spending from your account (with the agent's budget so far), your own
transactions sealing once you've left the page you sent them from, and watched accounts' moves.
A hidden tab checks only your own account, less often, and counts unread notices in its title.
Notices only come from a node the wallet accepted as its chain: each round re-reads the pinned
status, so a node on another chain raises none. The node lists an account's newest 25
transactions, so anyone could push a spend out of view between two polls with dust; the
account's nonce and its keys' spending catch that ("your agent spent"), and arrivals in one
round fold into one notice so they can't crowd out a spend.

## Building it under the wallet's CSP

The wallet's production CSP (`web/lib/csp.ts`) allows only nonce'd styles, so no explorer page
renders a `style` attribute: layout is classes, and sizes and positions computed at run time are
CSS custom properties set through the CSSOM after mount (`web/lib/cssvars.ts`; SVG geometry
uses attributes). `web/e2e/explorer-pages.spec.ts` visits every page and flow against the e2e
mock node, filled with protocol v1's shapes by `web/e2e/explorer-world.ts` (a transaction of
every kind, failed and dropped ones, strangers' markup and `javascript:` links), and fails on any
CSP violation, console error, server-rendered style attribute or markup run from chain text.
`node scripts/seed-dev.mjs` fills a real `blockula dev` chain with the same variety.

## What the node would need for more

All of this runs on the node's existing HTTP API. These endpoints would close the gaps that
remain. They're best built on the newer read path (`ReadView` in
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
- **What a transaction burned opening accounts.** A receipt can't tell from `/api/tx` whether a
  payment to an address opened its account (and burned the creation fee from the sender); it
  asks the recipient's history, and says so when that doesn't settle it. Recording the accounts
  each transaction opened would make it exact.
- **Deeper traces.** A trace reads each account's 25 latest transactions. Paging an account's
  history (`/api/account/{id}/txs?before=`) would let it reach further back.
- **Push.** Notices poll. A server-sent event stream of finalized transactions per account would
  make them instant and cheaper.
