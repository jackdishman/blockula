# Blackjack: a table for operators

Blackjack on Blockula is a game played with FANG: one program on a chain that runs programs
(protocol v2), a dealer daemon that deals from hash chains it committed to in advance, a page in
the wallet where a player signs every bet and move with their own passkey, and a bot for X that
exists in dry run only. FANG has no value and can't be bought; there are no prizes.

This file is for whoever runs a table: what the program does, what each key can and can't do,
how to deploy and run the pieces on a local chain, what to check before a launch, and what was
left out. Players read the rules on the table page itself (`/blackjack`).

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
A bet takes the next unused commitment **in order** (the game's id is `k`) and fixes the player's
`client_seed`, 32 random bytes the player's wallet picks. Card `j` is dealt by revealing `s_j`,
which the program accepts only if it hashes to the link before it, and is

```text
card_j = u64_le(BLAKE3(s_j ‖ client_seed ‖ k (8 bytes, LE) ‖ j (1 byte))[..8]) mod 52
```

So the dealer fixed its part before it knew the seed, the player fixed theirs before any link was
public, and neither picks a card. The dealer's hole card is the first link after the player's last
card: nobody can read it before the player stands, the chain included. 2^64 isn't a multiple of
52, so 16 of the 52 cards are more likely than the rest by about 3 parts in 10^18; no rejection
loop is worth that.

**Checking a hand.** The program keeps the newest link of an open hand and, for each player,
the last settled hand: its id, seed, newest link and card counts. Every earlier link is the hash
of the next, and the last hash is the commitment, so those values fix every card. `verifyHand`
(`@blockula/blackjack`) recomputes them and the table page's "Check this hand" does it in the
browser. A `settled` event carries the same values for any hand.

**What a dealer can still do: stall.** It sees a bet's cards before it reveals them. The program
takes the profit out of that: when the dealer misses a deadline the player is paid the most the
hand could still have paid (3:2 while a natural is possible or held, 1:1 after), from the exposure
reserved for the hand. Stalling never pays the house better than dealing.

**What a house can still do: get ahead of a bet.** A bet's seed is public from the moment its
transaction is in the mempool, and commitments are taken in order. A house that watches the
mempool knows what the next commitment would deal that seed, and could put a bet of its own in
front (taking that commitment) whenever the player's hand would win, so the player's bet meets the
next one instead. Nothing in the program stops that: it is the price of a one-transaction bet on
a chain whose programs can't read randomness (protocol v2). It leaves a trail (the house's own
bets sit on the chain, in front of the bets they displaced), and it needs a house that also wins
races for block space, but a player can't rule it out from one hand. The fix is a bet in two
transactions: the player commits to a hash of the seed, which takes the commitment, and reveals
the seed after. It isn't in v1 (section 9).

**Relayed play trusts the relayer more.** A relayed bet's seed is not the relayer's choice (the
program derives it from the player and the post id), but the relayer chooses *when* to submit a
bet, with no race to win. A relayer run by the house therefore has a last look by design. The X
path is a convenience for small stakes with a house the player trusts. Say so wherever the bot is
offered.

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
it comes back when the record is removed: a hand's record (bonded by the bettor, back at
settlement), a commitment (bonded by the dealer, back when a bet takes it). A player's record
stays. The dealer's `reveal` and anyone's `timeout` bond nothing.

## 4. Deadlines

| Who owes a move | Has | Then |
|---|---|---|
| The dealer: the deal, a hit's card, its own hand | `reveal_blocks` (150) from the bet, the hit or the stand. A partial reveal doesn't extend it. | Anyone calls `timeout`: **the player wins** (3:2 before the deal is done or against a natural, else 1:1). |
| The player: hit or stand | `idle_blocks` (1,800) from the deal or the last card | Anyone calls `timeout`: the player stands, and the dealer finishes the hand. |

A reveal that arrives late still counts until someone has called `timeout`.

## 5. Limits

The owner sets them with `set_limits` (all twelve at once); `limits` reads them. Amounts are FANG.

| Limit | Default | What it bounds |
|---|---|---|
| `min_bet`, `max_bet` | 1, 50 | A bet. |
| `bankroll_divisor` | 20 | A bet is also at most the free bankroll divided by this. |
| `max_exposure` | 750 | The reserve over all open hands. |
| `daily_cap`, `day_blocks` | 500, 43,200 | What one account may wager in a day (a day is `day_blocks`: 43,200 two-second blocks). |
| `max_open_games` | 10 (at most 32) | Open hands at the table. One account has one open hand at a time. |
| `reveal_blocks`, `idle_blocks` | 150, 1,800 | Section 4. |
| `deposit_cap` | 200 | A table balance, by deposit (payouts may take it higher). |
| `relay_max_bet` | 25 | A relayed bet. |
| `grant_max_blocks` | 43,200 | How long a relayer grant may last. |

`pause(true)` refuses new bets (`bet`, `relay_bet`) and nothing else: open hands play on,
timeouts, deposits and withdrawals work.

## 6. Keys and what each can do

| Key | Can | Can't |
|---|---|---|
| **Owner** (the account that created the table) | Set limits, pause, name the dealer, approve or remove relayers, take free bankroll out, drop unused commitments (`skip_commits`). | Touch a reserve, a stake or a table balance; change the rules or the code (the program is immutable). |
| **Dealer** (named by the owner) | Post commitments. Its daemon also reveals links, which anyone who knows them may do. | Move any FANG. |
| **Player** | Bet, hit, stand from their own account; deposit to and withdraw from their own table balance; grant and revoke one relayer. | Anything on another account's hand or balance. |
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
table's address is configured. Players sign `bet`, `hit` and `stand` with their own passkey
through the wallet's review-then-sign flow: the page shows the program's full version digest and
the most the call can cost before anything is signed.

### 7.4 The X bot, dry run

See [services/blackjack/xbot/README.md](../services/blackjack/xbot/README.md). It reads posts
from a file, plays the relayer path on a local chain and writes the replies it would make (text,
picture, alt text) to a directory. It has no X client, no credentials and no code that posts.

### 7.5 The end-to-end run

`scripts/blackjack-e2e.sh` starts four local validators with programs on (protocol v2) in a
temporary directory, creates and funds a table, runs the dealer daemon, plays a soak of hands from
several accounts (some through a relayer), checks the invariant at every block, kills and restarts
the dealer mid-run, lets a dealer stall so a player wins by timeout, and exercises the caps and
the pause. `HANDS=60 scripts/blackjack-e2e.sh` is the short form.

## 8. Fuel

The most each method used in `node/vm/tests/blackjack.rs` (schedule v0, `program_max_tx_fuel`
10,000,000), instantiation included. A transaction also pays the code's load at admission:
857,920 for the 21,198-byte module.

| Method | Most fuel | Method | Most fuel |
|---|---|---|---|
| `reveal` (6 links; about 98,000 a link, 12 at most) | 590,316 | `bet` | 218,448 |
| `commit_batch` (64 commitments) | 593,313 | `relay_bet` | 263,688 |
| `skip_commits` | 322,231 | `timeout` | 217,652 |
| `relay_withdraw` | 168,868 | `hit`, `stand` | 121,876 |
| `withdraw` | 151,160 | `relay_hit`, `relay_stand` | 150,233 |
| `deposit` | 144,726 | `set_relayer` | 140,758 |
| `init` | 139,823 | `defund` | 125,338 |
| the views | 90,726 to 123,956 | the owner's other calls | 109,625 to 114,682 |

The longest reveal there can be (a hit to 21 and an eleven-card dealer hand) is about 1.3 million.

## 9. What's deliberately left out, and known gaps

- **The rules are v1.** No split, double, insurance or surrender; one hand per account.
- **The owner can't be changed** and the program can't be upgraded. A new table is a new program.
- **A house can get ahead of a bet** it sees in the mempool, and **relayed play gives the
  relayer a last look** (section 2). The fix for the first is a bet whose seed is committed
  first and revealed after (two transactions); it's the first thing a v2 of the rules should add.
  The bot is dry run only.
- **A chain-wide pause of programs stops the dealer too.** Validators can pause programs for
  days (docs/PROGRAMS-V0.md §7.2); deadlines keep counting in blocks, and when the pause lifts
  every open hand is past its deadline. A late reveal still counts until someone calls `timeout`,
  so the dealer usually gets there first, but hands open at such a pause can end as the players'.
  `max_exposure` is the most that can cost.
- **Idle players hold seats.** A hand a player abandons keeps its seat and its reserve for
  `idle_blocks`; with one hand per account and `max_open_games` seats, a few accounts can fill a
  table for that long at the price of their stakes. Lower `idle_blocks` if it happens.
- **Keep `reveal_blocks` well above 12.** Anyone who sees the dealer's reveal in the mempool can
  submit its first link alone and make the dealer's own transaction fail; each such round still
  deals a card, so it costs the dealer at most one block per card it owed.
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
   `guests/blackjack` and `guests/bj-core` has happened.
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
   when a `settled` event shows a forfeit (outcome 6 or 7).
6. **The page.** The 18+ self-attest is in place, the note "FANG has no value and can't be bought;
   no prizes." is visible, and the rules on the page match the limits on the chain.
7. **A rehearsal.** A full day on a local or test chain at the real limits: hands from several
   wallets, a dealer restart, a deliberate stall, a pause, a `defund`. Every hand verified.
8. **The way out.** The steps to stop are written down and tried: `pause(true)`, let open hands
   settle or time out, players withdraw their table balances (never refused), then `defund`.
9. **The X bot stays in dry run** until it has an X client behind its interface, a review of that
   client, and a decision on the trust it asks of players (section 2).

## 11. Tests

| What | Command |
|---|---|
| Rules against an independent reference, BLAKE3, vectors | `cargo test --release --locked --manifest-path node/vm/Cargo.toml --test blackjack_rules` |
| The program: every transition, abort, cap, timeout, the invariant, fuel | `cargo test --release --locked --manifest-path node/vm/Cargo.toml --test blackjack -- --nocapture` |
| The committed module rebuilds byte for byte | `node/vm/guests/build.sh --check` |
| The TypeScript rules and codec against the vectors | `npm test -w @blockula/blackjack` |
| The table on a real local chain through the SDK | `BLOCKULA_RPC=http://127.0.0.1:38100 npm run test:live -w @blockula/blackjack` |
| The dealer, the pictures, the X bot (offline) | `npm test -w @blockula/blackjack-dealer`, `-w @blockula/blackjack-render`, `-w @blockula/blackjack-xbot` |
| Four validators, the dealer daemon, the soak | `scripts/blackjack-e2e.sh` |
| The table page | `npm test -w @blockula/web`, `npm run typecheck -w @blockula/web`, `npm run test:e2e -w @blockula/web` |
