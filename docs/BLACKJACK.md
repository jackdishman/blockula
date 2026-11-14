# Blackjack: a table for operators

Blackjack on Blockula is a game played with FANG: one program on a chain that runs programs
(protocol v2), a dealer daemon that deals from hash chains it committed to in advance, a page in
the wallet where a player signs every bet and move with their own passkey, and a bot for X that
exists in dry run only. FANG has no value and can't be bought; there are no prizes.

This file is for whoever runs a table: what the program does, what each key can and can't do,
how to deploy and run the pieces on a local chain, what to check before a launch, and what was
left out. Players read the rules on the table page itself (`/blackjack`). A security review of
the program and the changes it led to are in [BLACKJACK_REVIEW.md](BLACKJACK_REVIEW.md).

| Piece | Where | What it is |
|---|---|---|
| Rules | `node/vm/guests/bj-core` | Cards from a hash chain, totals, the dealer's policy, payouts. `no_std`, no dependencies. |
| Program | `node/vm/guests/blackjack` | The table: bets, turns, deadlines, the bankroll, table balances, relayers. Imports protocol v2's host functions only. |
| Library | `services/blackjack/core` (`@blockula/blackjack`) | The rules in TypeScript, the program's encoding, reads, calls, `verifyHand`. |
| Dealer | `services/blackjack/dealer` | Commits chains ahead of the bets and reveals cards. |
| Pictures | `services/blackjack/render` | A hand as a 1200 × 675 PNG, with alt text. |
| Table page | `web/app/blackjack` | Play, watch, verify; an embeddable view and a share page. |
| X bot | `services/blackjack/xbot` | Dry run only: it has no X client and can't post. |

## 1. The rules (v1)

One hand against the dealer. An infinite deck: every card is drawn from all 52 with equal chance.
The dealer stands on every 17, soft ones included. A natural (an ace and a ten-value card as the
first two cards) pays 3:2; a win pays 1:1; a push returns the stake. No split, no double, no
insurance, no surrender. A hand that reaches 21 stands by itself. If the player's first two cards
are a natural and the dealer's upcard is an ace or a ten-value card, the dealer turns one more
card: a natural of its own is a push, anything else pays 3:2. Against any other upcard the natural
is paid at once.

## 2. Fairness: the dealer's hash chains

For each future game `k` the dealer draws a secret `s_32` and hashes it down 32 times:
`s_i = BLAKE3(s_{i+1})`. It posts `c_k = s_0`, the commitment, before any bet (`commit_batch`).
A bet takes the next unused commitment **in order** (the game's id is `k`). Card `j` is dealt by
revealing `s_j`, which the program accepts only if it hashes to the link before it, and is

```text
card_j = u64_le(BLAKE3(s_j ‖ client_seed ‖ k (8 bytes, LE) ‖ j (1 byte))[..8]) mod 52
```

`client_seed` is 32 random bytes the player's wallet picks. So the dealer fixed its part before
it knew the seed, the player fixed theirs before any link was public, and neither picks a card.
The dealer's hole card is the first link after the player's last card: nobody can read it before
the player stands, the chain included. 2^64 isn't a multiple of 52, so 16 of the 52 cards are
more likely than the rest by about 3 parts in 10^18; no rejection loop is worth that.

**A bet is two transactions.** The seed must stay private until the hand holds its commitment,
or whoever sees it first could change which commitment it meets. So:

1. `bet` carries the stake and a hash, never the seed:

   ```text
   seed_hash = BLAKE3("blockula/blackjack/seed" (23 bytes) ‖ player (32 bytes) ‖ client_seed (32 bytes))
   ```

   It takes the next commitment, reserves the hand's exposure, and counts against every limit
   (the bet sizes, the daily cap, the open hands, the exposure) exactly as a bet always did. The
   hand waits for its seed (phase 0), and the player has `seed_blocks` to send it.
2. `reveal_seed` carries the seed, from the same account, once the bet is final. The program
   checks it against the hash, and only then does the dealer owe the deal: its `reveal_blocks`
   start at the seed, not at the bet.

What this buys, and for whom:

- **Nobody can get ahead of a bet.** When the seed becomes public (in the mempool, in a block)
  the hand already holds its commitment. A bet placed in front then takes a later commitment,
  the owner's `skip_commits` drops only unused ones, and nothing else moves a hand to another
  chain. Before that, a bet shows only a hash and a stake: someone who gets in front of it does
  change which commitment it takes, but blind, knowing nothing of what either would deal.
- **The house learns nothing usable from the bet.** A hash of 32 random bytes says nothing about
  them. (A wallet must use real randomness: a seed that could be guessed could be tried against
  the hash.)
- **The player learns nothing before revealing.** They know their seed and the commitment, but
  every card also needs a link of the chain, and no link of the hand is public until the dealer
  reveals one, which the program refuses before the seed is in.
- **A copied hash or seed is worth nothing.** The hash binds the player's account, so a hash
  copied into another account's bet can never be opened there (the copier loses that stake). A
  seed seen after its reveal is of no use to anyone: other hands use other chains.
- **The dealer still can't pick cards**, and stalling still pays the player (below).

**A seed that never comes forfeits the bet.** Past `seed_blocks` a seed is refused, anyone may
call `timeout`, and the hand ends as outcome 8: the stake goes to the bankroll, the reserve is
free again, the seat is free, and the dealer's commitment is spent. That is the right end, for
two reasons. The player can't know any card before revealing, so holding a seed back is never a
way out of a hand that would have lost; an honest player loses nothing by the rule. And a hand
that could be abandoned for free would let anyone fill the seats and burn the dealer's
commitments at no cost. A wallet sends the reveal by itself, right after the bet lands, and
keeps the seed where a reload finds it (the table page does both). A seed that arrives after
its deadline is refused even if nobody has called `timeout` yet: a late seed is public in the
mempool, and whoever saw it there could otherwise choose between letting it in and ending the
hand.

**Checking a hand.** The program keeps the newest link of an open hand and, for each player,
the last settled hand: its id, seed, newest link and card counts. Every earlier link is the hash
of the next, and the last hash is the commitment, so those values fix every card. `verifyHand`
(`@blockula/blackjack`) recomputes them, checks the seed against the hash the bet carried when
it is given both, and the table page's "Check this hand" does it in the browser. A `bet` event
carries the hash, a `seed` event the seed, and a `settled` event everything a check needs. A
hand that ended without its seed has no cards: its record holds the hash that was never opened.

**What a dealer can still do: stall.** Once the seed is in, the dealer sees the hand's cards
before it reveals them. The program takes the profit out of that: when the dealer misses a
deadline the player is paid the most the hand could still have paid (3:2 while a natural is
possible or held, 1:1 after), from the exposure reserved for the hand. Stalling never pays the
house better than dealing.

**What still asks for trust: getting the seed onto the chain.** The second transaction has a
deadline, and the bet is lost if it isn't met. A party that can keep a player's `reveal_seed`
out of every block for `seed_blocks` (the node a wallet submits through, if it drops the
transaction and the wallet tries no other; or the chain's block producers together) takes the
stake, and a house in that position has by then seen the seed and can choose which reveals to
lose. No rule in a program removes this: refunding an unrevealed bet instead would let the same
party cancel exactly the hands a player would have won. What limits it: `seed_blocks` is never
under 20 blocks and is 300 by default, a signed reveal is valid at any node of the chain until
its deadline, and a reveal that was submitted and never included is evidence a player can show.

The table page answers the single-node case. It sends each signed `reveal_seed` to the node the
wallet uses and to every other node the wallet was built to know (its witnesses,
`NEXT_PUBLIC_*_WITNESSES`: [web/README.md](../web/README.md)), all at once. It is one signed
transaction, so it runs once and its fee is paid once; one node taking it is enough, and the
others are still tried. If it isn't in a block when the page stops waiting, the same transaction
is sent to every node again, and the hand shows a warning for as long as a reveal was signed and
the hand still waits for its seed: it says the bet goes to the house if the reveal isn't in a
block by the deadline, and through how many nodes it went. What remains: a wallet with no
witnesses configured (a custom node, and the wallet a node serves at `/wallet/`, which knows
only that node) still sends through one node, and says so in the reveal's review and in the
warning; and nodes or block producers that all work together can still keep a reveal out. So
build the table's wallet with witnesses the house doesn't run, and play at a table whose chain
has block producers the house doesn't control (section 9).

**Relayed play trusts the relayer more.** A relayed bet stays one transaction: there is no
player's wallet to keep a seed, so its seed is derived by the program from the player and the
post id, and the hand is dealt at once. The relayer chooses *when* to submit a bet, with no
race to win, and it supplies the post id, which the program can't check against X. A relayer
run by the house therefore has a last look by design, and one that made up post ids could pick
seeds as well; made-up ids can be found afterwards by comparing `relayed` events with the
player's posts, but nothing on the chain prevents them. The X path is a convenience for small
stakes with a house the player trusts. Say so wherever the bot is offered. A player who wants
the guarantees above bets from their own account.

That is the decision for now: relayed play and the X bot stay trusted and small-stakes (the
grant's low limit per bet and per day is what bounds them), and nothing in the program changes
for them. Before the X bot goes live, each relayed bet must carry the player's own signature,
so a relayer can no longer place a bet, pick its moment or make up its post id by itself. That
isn't built; the bot stays in dry run until it is (section 10).

## 3. The money

Every drop the program holds is in exactly one of four counters, which `house` returns with the
balance:

```text
balance = bankroll + reserved + stakes + table
```

- `bankroll`: the house's free FANG. `fund` adds to it; only the owner's `defund` takes from it.
- `reserved`: 1.5 × each open hand's stake, moved out of `bankroll` when the bet is placed. A bet
  that doesn't fit the free bankroll is refused, so every hand is paid from FANG set aside for it.
- `stakes`: the open hands' stakes.
- `table`: the players' table balances (section 5).

The program is created without `accepts_transfers`, so nothing reaches it outside a method and
the equation is exact. The tests check it after every call (`node/vm/tests/blackjack.rs`), and
the end-to-end run checks it at every block.

A hand bet from an account pays that account. If that transfer can't be made (the account no
longer exists), the payout becomes the player's table balance, so a hand always settles and
`withdraw` is the way out.

**Storage deposits.** Records bond a deposit from whoever signs the call that creates them, and
it comes back when the record is removed: a hand's record (bonded by the bettor at the bet, back
at settlement, a forfeited seed included), a commitment (bonded by the dealer, back when a bet
takes it). A player's record stays. The player's `reveal_seed`, the dealer's `reveal` and
anyone's `timeout` bond nothing.

## 4. Deadlines

| Who owes a move | Has | Then |
|---|---|---|
| The player: the seed of a bet from their account | `seed_blocks` (300) from the bet. A seed after that is refused. | Anyone calls `timeout`: **the bet is the house's** (outcome 8). No card was dealt; the stake goes to the bankroll, the reserve and the seat are free. |
| The dealer: the deal, a hit's card, its own hand | `reveal_blocks` (150) from the seed's reveal (a relayed bet: the bet), the hit or the stand. A partial reveal doesn't extend it. | Anyone calls `timeout`: **the player wins** (3:2 before the deal is done or against a natural, else 1:1). |
| The player: hit or stand | `idle_blocks` (1,800) from the deal or the last card | Anyone calls `timeout`: the player stands, and the dealer finishes the hand. |

The dealer's reveal and a player's move that arrive late still count until someone has called
`timeout`. A seed doesn't: past its deadline it is refused (abort 72), whether or not anyone has
called `timeout` yet (section 2). That stays: a late seed is never counted.

## 5. Limits

The owner sets them with `set_limits` (all thirteen at once); `limits` reads them. Amounts are FANG.

| Limit | Default | What it bounds |
|---|---|---|
| `min_bet`, `max_bet` | 1, 50 | A bet. |
| `bankroll_divisor` | 20 | A bet is also at most the free bankroll divided by this. |
| `max_exposure` | 750 | The reserve over all open hands. |
| `daily_cap`, `day_blocks` | 500, 43,200 | What one account may wager in a day (a day is `day_blocks`: 43,200 two-second blocks). |
| `max_open_games` | 10 (at most 32) | Open hands at the table. One account has one open hand at a time. |
| `reveal_blocks`, `idle_blocks` | 150, 1,800 | Section 4. `reveal_blocks` is at most 86,400; `idle_blocks` at least 20. |
| `deposit_cap` | 200 | A table balance, by deposit (payouts may take it higher). |
| `relay_max_bet` | 25 | A relayed bet. |
| `grant_max_blocks` | 43,200 | How long a relayer grant may last. |
| `seed_blocks` | 300 (at least 20) | How long a player has, from the bet, to reveal its seed (section 4). |

The program refuses limits that would turn the owner's lever against a hand in play: a player
is never given fewer than 20 blocks to move or to reveal a seed (the dealer knows the cards, so
an owner who could cut a turn to one block could stand a player before a hit, or keep a stake
before its seed), and the dealer is never given more than 86,400 blocks (so a stall always ends
in a hand the player can claim). Inside those bounds a change applies to deadlines set after
it: a hand's current deadline is the one it was given.

These values stay as they are: `seed_blocks` is 300 by default, and `set_limits` refuses an
`idle_blocks` or a `seed_blocks` under 20. The bounds are the whole protection. A hand keeps no
copy of the limits it was bet under, so a `set_limits` inside the bounds still applies to a hand
in play: its next deadline (a turn after the next card, the dealer's time after a hit or a
stand) is counted with the new numbers. That is accepted, and no per-hand snapshot is planned
([BLACKJACK_REVIEW.md](BLACKJACK_REVIEW.md), BJ-02). An operator changes limits with the table
paused and no hand open, and players can see every change (a `limits` event).

`pause(true)` refuses new bets (`bet`, `relay_bet`) and nothing else: a bet waiting for its seed
can still be opened, open hands play on, timeouts, deposits and withdrawals work.

## 6. Keys and what each can do

| Key | Can | Can't |
|---|---|---|
| **Owner** (the account that created the table) | Set limits, pause, name the dealer, approve or remove relayers, take free bankroll out, drop unused commitments (`skip_commits`). | Touch a reserve, a stake or a table balance; change the rules or the code (the program is immutable). |
| **Dealer** (named by the owner) | Post commitments. Its daemon also reveals links, which anyone who knows them may do. | Move any FANG. |
| **Player** | Bet, reveal that bet's seed, hit, stand from their own account; deposit to and withdraw from their own table balance; grant and revoke one relayer. | Anything on another account's hand or balance. |
| **Relayer** (approved by the owner, then granted by a player) | Play that player's table balance within the grant (per-bet and per-day limits, an expiry of at most `grant_max_blocks`); send the whole balance back to the player's own account. | Send a balance anywhere else; bet from an account; act once the grant expires, the player revokes, or the owner removes it. |

Keep the owner key offline; it's needed only for the levers above. The dealer key and the
relayer key sit on servers: both are fee-paying keys that hold little.

## 7. Running it on a local chain

Everything below runs against a chain of your own with programs on. For a throwaway one:

```bash
export BLOCKULA_HOME=$(mktemp -d)
blockula dev --programs --port 38100 &          # a temporary home, a port of your choosing
export RPC=http://127.0.0.1:38100
```

### 7.1 The table

`createTable` (`@blockula/blackjack`) uploads the program and creates the table with `init`
(the owner is the creator, the default limits, the dealer you name). With the CLI:

```bash
P=node/vm/tests/programs
blockula program upload $P/blackjack.wasm --interface $P/blackjack.iface.json --yes --rpc $RPC
blockula program create <wasm_hash> <interface_hash> --init init --input <dealer address as 64 hex> --storage-limit 1 --yes --rpc $RPC
blockula program call <table> fund --value 2000 --yes --rpc $RPC
```

Check the code before trusting a table someone else created: `sha256sum` of the stored code is
in `node/vm/guests/SHA256SUMS` (`blackjack.wasm`), and `node/vm/guests/build.sh --check` rebuilds
it byte for byte. Create the table without `accepts_transfers` (the default).

Size the bankroll from the limits: a bet is at most `bankroll / bankroll_divisor`, and the open
hands' reserves (`max_open_games × 1.5 × max_bet` at most, capped by `max_exposure`) must fit it.
The defaults want at least 1,000 FANG for 50 FANG bets, and 750 FANG can be reserved at once.

### 7.2 The dealer

See [services/blackjack/dealer/README.md](../services/blackjack/dealer/README.md) for every
setting. In short:

```bash
npm run build -w @blockula/sdk && npm run build -w @blockula/blackjack && npm run build -w @blockula/blackjack-dealer
npx blackjack-dealer init      # makes its key file and the seed store's secret file (both mode 600), prints its address
# as the owner: name that address the dealer (init's argument, or set_dealer), and send it FANG for fees
npx blackjack-dealer run --dry-run   # reads the chain, says what it would send, sends nothing
npx blackjack-dealer run
```

The dealer's secrets are the chains of games not yet played. They live in an encrypted store with
a backup copy, written before a commitment is ever sent. **A lost store means hands that can't be
dealt**: every bet that takes one of its commitments ends in a timeout that pays the player. If
the store and its backup are gone, pause the table, then drop the unused commitments as the owner
(`skip_commits`, 64 at a time) and let the dealer commit fresh ones. Never run two dealers on one
store, and never copy a store between tables.

### 7.3 The table page

The wallet's `/blackjack` page plays one table. See [web/README.md](../web/README.md) for how the
table's address is configured. Players sign `bet`, `reveal_seed`, `hit` and `stand` with their
own passkey through the wallet's review-then-sign flow: the page shows the program's full
version digest and the most the call can cost before anything is signed. A bet is two
signatures: the page makes the seed, keeps it in the browser's storage, sends its hash with the
bet, and presents the seed's reveal by itself as soon as the bet is in a block. A reload between
the two finds the seed again by its hash. A browser that doesn't hold the seed (the bet was
placed elsewhere, or its storage was cleared) can't reveal it, and the page says so.

### 7.4 The X bot, dry run

See [services/blackjack/xbot/README.md](../services/blackjack/xbot/README.md). It reads posts
from a file, plays the relayer path on a local chain and writes the replies it would make (text,
picture, alt text) to a directory. It has no X client, no credentials and no code that posts.

### 7.5 The end-to-end run

`scripts/blackjack-e2e.sh` starts four local validators with programs on (protocol v2) in a
temporary directory, creates and funds a table, runs the dealer daemon, plays a soak of hands from
several accounts (their own bets in two transactions, some through a relayer), checks the
invariant at every block, kills and restarts the dealer mid-run, lets a dealer stall so a player
wins by timeout, lets seeds never come so those bets go to the house, and exercises the caps and
the pause. `HANDS=60 scripts/blackjack-e2e.sh` is the short form.

## 8. Fuel

The most each method used in `node/vm/tests/blackjack.rs` (schedule v0, `program_max_tx_fuel`
10,000,000), instantiation included. A transaction also pays the code's load at admission:
905,680 for the 22,392-byte module.

| Method | Most fuel | Method | Most fuel |
|---|---|---|---|
| `reveal` (6 links; about 97,800 a link, 12 at most) | 586,708 | `bet` | 219,546 |
| `commit_batch` (64 commitments) | 594,019 | `relay_bet` | 264,780 |
| `skip_commits` | 322,937 | `timeout` | 218,582 |
| `relay_withdraw` | 169,643 | `reveal_seed` | 163,589 |
| `withdraw` | 151,866 | `hit`, `stand` | 122,782 |
| `deposit` | 145,590 | `relay_hit`, `relay_stand` | 151,181 |
| `init` | 142,509 | `set_relayer` | 141,670 |
| the views | 91,432 to 124,662 | `defund` | 126,044 |
| the owner's other calls | 110,331 to 116,681 | `revoke` | 120,647 |

The longest reveal there can be (a hit to 21 and an eleven-card dealer hand) is about 1.3 million.

## 9. What's deliberately left out, and known gaps

- **The rules are v1.** No split, double, insurance or surrender; one hand per account.
- **The owner can't be changed** and the program can't be upgraded. A new table is a new program.
- **Relayed play gives the relayer a last look**, and its seeds come from post ids the relayer
  supplies (section 2; [BLACKJACK_REVIEW.md](BLACKJACK_REVIEW.md), BJ-03). Relayed bets don't go
  through the seed's commit and reveal: there is no player's wallet in that path to keep a seed.
  Relayed play stays trusted and small-stakes for now, and the bot is dry run only: the player's
  signature on each relayed bet is required before it goes live, and isn't built (section 2).
- **A seed has to reach the chain in time.** Whoever can keep a player's `reveal_seed` out of
  every block for `seed_blocks` takes that stake, and can choose which (section 2; BJ-04). The
  table page sends each reveal through the wallet's node and every witness it was built with,
  and warns while a reveal hasn't landed. A wallet built without witnesses (or the one a node
  serves) still has one node, and says so; nodes and block producers that all work together
  can still keep a reveal out. Witnesses the house doesn't run, and a chain whose block
  producers aren't the house's, are what answer it. A late seed stays refused.
- **Limits apply to hands in play.** `set_limits` inside the program's bounds (section 5)
  changes the deadlines an open hand is given next. Accepted: a hand takes no snapshot of the
  limits it was bet under. Change limits with the table paused and empty.
- **A chain-wide pause of programs stops the players and the dealer too.** Validators can pause
  programs for days (docs/PROGRAMS-V0.md §7.2); deadlines keep counting in blocks, and when the
  pause lifts every open hand is past its deadline. A bet that was waiting for its seed is then
  the house's, and can't be opened. A dealer's late reveal still counts until someone calls
  `timeout`, so the dealer usually gets there first, but hands open at such a pause can end as
  the players'. `max_exposure` is the most that can cost the house, the stakes of the bets
  waiting for a seed the most it can cost players; an owner can make those players whole from
  its own account, and nothing in the program does.
- **The daily cap is a window that starts every `day_blocks` blocks**, not the last
  `day_blocks`: an account can wager the cap at the end of one window and again at the start of
  the next. Changing `day_blocks` renumbers the windows.
- **Idle players hold seats.** A hand a player abandons keeps its seat and its reserve for
  `idle_blocks`; with one hand per account and `max_open_games` seats, a few accounts can fill a
  table for that long at the price of playing those hands. Lower `idle_blocks` if it happens. (A
  bet whose seed never comes holds its seat for `seed_blocks` and costs its whole stake.)
- **Getting ahead of the dealer's reveal does nothing.** Anyone who sees the dealer's reveal in
  the mempool can submit its links first (a link is its own authorization). The dealer's
  transaction then passes over the links the hand already holds and deals the rest; if nothing
  is left it changes nothing and still succeeds (BJ-08, fixed). A link that is neither held nor
  the chain's next is refused as before. Keep `reveal_blocks` well above 12 all the same: it is
  the dealer's whole margin for a slow node or a full block.
- **A table needs a live dealer.** A dealer that is down pays every open hand as a player's win.
  Run one dealer, watch it, and pause the table before maintenance.
- **Views are optional.** Clients read the program's storage through the node's storage route,
  which every node serves; the `game`, `player`, `house`, `limits`, `open_games` and `commitment`
  views exist for wallets that read through the interface.
- **A player's record keeps one settled hand.** Older hands are verified from their `settled`
  events, which a pruning node drops with their blocks.
- **Programs are experimental** and not audited, and so is this one.

## 10. Launch checklist

Before a table takes bets on any chain people use:

1. **The code.** `node/vm/guests/build.sh --check` passes; the stored code's hash is
   `SHA256SUMS`' `blackjack.wasm`; `cargo test --release --locked --manifest-path node/vm/Cargo.toml`
   and `scripts/blackjack-e2e.sh` pass on the release you run. An independent review of
   `guests/blackjack` and `guests/bj-core` by people has happened:
   [BLACKJACK_REVIEW.md](BLACKJACK_REVIEW.md) is an AI agent's review and doesn't stand in for
   one. Its open findings are read and decided.
2. **The chain.** Programs are on; `program_value_cap` leaves room for the bankroll and the table
   balances; the block time is the one the limits assume (the defaults assume two-second blocks:
   150 blocks is five minutes, 1,800 an hour, 43,200 a day). Recompute them for another block time.
3. **The keys.** The owner key is offline and backed up. The dealer and relayer keys are files of
   mode 600 on their own machines, with only fee money. Nobody else can read the dealer's store,
   its secret file or its backup, and the backup is on another disk.
4. **The limits.** Set and read back (`limits`). The bankroll covers them (section 7.1). Start
   small: lower `max_bet`, `daily_cap` and `max_exposure` first and raise them later.
5. **The dealer.** A dry run reads the table. A real run on a local copy of the chain has been
   killed and restarted mid-hand. Its balance is watched (fees and commitment deposits), its log is
   kept, and an alert fires when unused commitments run low, when a hand's deadline is close, and
   when a `settled` event shows a forfeit by the dealer (outcome 6 or 7). Outcome 8 (a seed that
   never came) is a player's, not the dealer's; many of them from different accounts mean
   players' reveals aren't reaching the chain, which is the table's problem to find.
6. **The page.** The 18+ self-attest is in place, the note "FANG has no value and can't be bought;
   no prizes." is visible, and the rules on the page match the limits on the chain. A bet's two
   steps have been tried on the wallets players use, a reload between them included, and
   `seed_blocks` leaves a slow passkey prompt plenty of time. The wallet is built with witnesses
   (`NEXT_PUBLIC_*_WITNESSES`), nodes of the chain that the house doesn't run, so a seed's
   reveal goes through more than one node: the reveal's review names how many, and with none it
   says the one node decides.
7. **A rehearsal.** A full day on a local or test chain at the real limits: hands from several
   wallets, a dealer restart, a deliberate stall, a bet whose seed is held back, a pause, a
   `defund`. Every hand verified.
8. **The way out.** The steps to stop are written down and tried: `pause(true)`, let open hands
   settle or time out, players withdraw their table balances (never refused), then `defund`.
9. **The X bot stays in dry run** until it has an X client behind its interface, a review of that
   client, and the player's signature on each relayed bet (section 2): that signature is
   required before the bot goes live, and isn't built. Until then relayed play is trusted and
   small-stakes, and is offered in those words.

## 11. Tests

| What | Command |
|---|---|
| Rules against an independent reference, BLAKE3, vectors | `cargo test --release --locked --manifest-path node/vm/Cargo.toml --test blackjack_rules` |
| The program: every transition, abort, cap, timeout, the two-transaction bet (a wrong, foreign, second or late seed, the forfeit, a front-runner who can't steer a hand), a reveal that passes over links already held, the invariant, fuel | `cargo test --release --locked --manifest-path node/vm/Cargo.toml --test blackjack -- --nocapture` |
| The committed module rebuilds byte for byte | `node/vm/guests/build.sh --check` |
| The TypeScript rules and codec against the vectors | `npm test -w @blockula/blackjack` |
| The table on a real local chain through the SDK | `BLOCKULA_RPC=http://127.0.0.1:38100 npm run test:live -w @blockula/blackjack` |
| The dealer, the pictures, the X bot (offline) | `npm test -w @blockula/blackjack-dealer`, `-w @blockula/blackjack-render`, `-w @blockula/blackjack-xbot` |
| Four validators, the dealer daemon, the soak | `scripts/blackjack-e2e.sh` |
| The table page | `npm test -w @blockula/web`, `npm run typecheck -w @blockula/web`, `npm run test:e2e -w @blockula/web` |
