# Proposal: a dealer that stops should pay for nothing it didn't deal (blackjack rules v2)

**Status:** proposed, and implemented on this branch as a new, separate program (`blackjack2`).
Nothing deployed changes: the `blackjack` program, its rules crate, its committed module and
every table created from it are untouched. A v2 table exists only if someone creates one.

**Answers:** audit finding M-3 ("a dealer outage pays every open and future bet 3:2") and I-1 ("a
stall during the deal pays 3:2 even when the cards already out rule a natural out").

## 1. The problem

Rules v1 ([BLACKJACK.md](../BLACKJACK.md), `node/vm/guests/blackjack`) settle a hand whose dealer
misses a reveal in the player's favour, at the most a hand can pay: 3:2 until the three cards of
the deal are out or while the player holds a natural, 1:1 after. The reason is sound. From the
moment the player's seed is public the dealer can compute every card of the hand, so if a stall
ever paid less than the cards would have, a dealer would stall exactly the hands it was about to
lose.

The rule doesn't ask whether the dealer *could* have known anything. So it also covers the case
where the dealer daemon is simply down:

- **M-3.** While the daemon is down, its commitments are still on the chain (it keeps up to
  `target`, 192 by default). Anyone can bet against one, reveal a seed, wait `reveal_blocks`
  (150 blocks, five minutes) and collect 3:2 without a card being dealt. Ten seats at the 50 FANG
  maximum are 750 FANG every five minutes, which is the table's whole exposure limit; the daily
  cap is per account and accounts cost 0.1 FANG. Most of a default table's 2,000 FANG bankroll
  is gone within twenty minutes unless the owner pauses first (the bets shrink with the
  bankroll, so the last of it goes more slowly). The same holds for a dealer that is slow, out
  of fee money, or behind a node that stopped syncing.
- **I-1.** During the deal v1 pays 3:2 even when the cards already on the table make a natural
  impossible (the player's first card is a seven, say). An exhaustive walk of the rules finds 88
  such states. That costs the house more than "the most the hand could still have paid".

The two findings are one design gap seen from two sides: v1's forfeit is the maximum over all
hands, not over the hands that are still possible given what is known, and not over what the
dealer could have known.

## 2. Threat model

Who is involved, and what each can do:

| Party | Can | Wants (when hostile) |
|---|---|---|
| **The house** (the owner and the dealer it names; in practice one operator) | Commit chains; reveal or withhold links; see every public seed; pause; set limits within the program's bounds. Knows every chain's links. | Keep the stakes of hands it would lose. |
| **A player** | Bet, choose a seed (fixed by its hash at the bet), reveal it or not, hit, stand, call `timeout`. Sees the mempool. | Be paid without the hand's odds: bet only when the dealer can't answer; keep good hands and drop bad ones. |
| **An opportunist** | Nothing special. Watches for a stalled table. | Fill the seats during an outage. |
| **An attacker who can stop the dealer** (flood its node, or its host) | Cause an outage on demand, for a while. | Turn that into payouts, or just cost the house money. |
| **Block producers** | Order and delay transactions; together, keep one out. | Either side's goals, if they collude with it. |
| **A relayer** (owner-approved, granted by a player) | Place and play a player's table-balance hands; chooses when. | Out of scope here, except that it must not become the way around the fix. |

Out of scope: a broken hash, a majority of block producers acting against the chain, and the
known v1 limits that this proposal doesn't touch (BJ-03, BJ-04, BJ-05 in
[BLACKJACK_REVIEW.md](../BLACKJACK_REVIEW.md)).

What must hold, in order of importance:

1. **Solvency.** Every hand is paid from FANG set aside for it; no rule may pay more than a
   hand's stake and reserve.
2. **The dealer never gains by withholding a card it could know.** (v1's property. Keep it.)
3. **A player never gains by a stall over playing the hand.** (New. v1 fails it: a stall is
   worth 3:2.)
4. **Nobody gets a free option**: a point where one side has seen something and can still choose
   between two endings.
5. Griefing costs the griefer about what it costs the victim, and one incident has a bound.

## 3. Options considered

### A. Refund the stake on any dealer stall

Simple, and it makes an outage free for the house. It breaks property 2 outright: from the seed
on, the dealer knows the hand, so it deals the hands it wins and "stalls" the ones it loses, and
every one of those is a refund. Players would only ever lose. The earlier review reached the same
conclusion for seeds (BJ-04: "a refund instead of a forfeit lets the same party cancel exactly
the hands the player would have won"). **Rejected** as a general rule. It is right in exactly one
situation, which option E isolates: when the dealer cannot know a card yet.

### B. A dealer bond that is slashed and burned

On a stall the player gets the stake back, and the house pays a penalty (at least the hand's
maximum) that goes to nobody: locked in the program for good. Neither side profits from a stall,
so properties 2 and 3 both hold, and an attacker who can stop the dealer gains nothing.

Costs: the program can't burn, so the penalty has to sit in a fifth counter that no method ever
pays out, which grows forever and counts against the chain's cap on FANG held by programs. An
honest player whose strong hand is cut off by an honest outage gets only the stake back, and has
to take on trust that the money really is out of the owner's reach. And an attacker who can stop
the dealer can still burn the house's money at no cost to themselves beyond fees, so property 5
is weaker than it looks. **Not chosen**, but the nearest alternative; section 9 says when it
would become the better choice.

### C. A bounded or decaying forfeit, or a circuit breaker alone

Pay the v1 forfeit, but cap what an outage can cost: the first forfeit pauses the table (a
breaker), or the payout decays with the number of forfeits. A breaker alone bounds an outage at
one table-full (`max_open_games` hands × 3:2 of each stake: 750 FANG by default), once, instead
of every five minutes. That is a real improvement and cheap, but the opportunist still collects
3:2 on up to ten hands for noticing an outage, so property 3 still fails. A decaying payout
punishes whichever honest players happen to come late. **Kept as a second layer** (the breaker),
not as the fix.

### D. A liveness signal: heartbeats, or bets refused when the dealer looks dead

The dealer proves it is alive every N blocks, and a bet is refused when the last proof is older
than that. It leaves a window of N blocks after the dealer dies in which bets still pay 3:2, so N
must be small, and then an idle dealer sends a transaction every few blocks for nothing: about
1,400 a day at N = 30. It also proves the wrong thing: that the dealer was alive *before* the
bet, not that it was there *for* the bet. **Rejected.**

### E. The dealer opens each hand before it can know a card (recommended)

The liveness proof is made per hand, at the one moment it costs nothing and proves the right
thing: after the bet, before the seed. Section 4.

### F. An off-chain watchdog (the audit's suggestion)

An owner-side process that calls `pause(true)` when reveals lag. Worth running on a v1 table
today. It needs a hot owner key (the key that can take the bankroll out), it races the people it
is trying to stop, and it does nothing for hands already open. **A mitigation for v1, not a fix.**

## 4. The recommended design: rules v2

Four changes to v1. Everything else (the hash chain, the card formula, totals, the dealer's
policy, the showdown, the payouts of a played hand, the money invariant, limits, table balances,
relayers) is v1's, and the rules crate runs v1's own code for it.

### 4.1 The dealer opens a hand before the seed

A card is `BLAKE3(link ‖ client_seed ‖ game ‖ j) mod 52`. A link alone says nothing about a card:
without the seed, every card is still equally likely. v1 never uses that. v2 does:

```text
v1:  bet(seed hash) ─→ reveal_seed ─→ [dealer: 3 links = the deal] ─→ play …
v2:  bet(seed hash) ─→ [dealer: 3 links, no cards] ─→ reveal_seed = the deal ─→ play …
         phase 6 "open"        phase 0 "seed"             phase 2 (or 4, or settled)
```

1. `bet` is unchanged: the stake and `BLAKE3("blockula/blackjack/seed" ‖ player ‖ seed)`. The
   hand takes its commitment and its reserve, and is now in a new phase, **open**.
2. The dealer has `open_blocks` (150 by default, a new fourteenth limit) to reveal the chain's
   first three links with `reveal`. They are checked against the commitment as always, and they
   deal nothing. The dealer has only the seed's hash, so it opens (or doesn't) without knowing a
   card. **This is the dealer's proof that it is there for this hand.**
3. Then the player owes the seed. `reveal_seed` checks the seed against the hash and computes
   cards 1 to 3 from the three links already on the chain, in the same transaction. There is no
   state in which the dealer knows the deal and still owes it. The player's time is
   `seed_blocks` counted from the **opening's deadline**, not from the opening: the dealer
   chooses when it opens, and must not be able to choose a moment when the player is away. A
   player who looks again when the refund would have been due finds either the refund or an
   opened hand with all of `seed_blocks` left.
4. From there the hand is v1's.

The dealer sends the same number of transactions as under v1 (the three links just move one step
earlier). The player's wait is the same too: one dealer transaction between the bet and the cards.

### 4.2 A hand that is never opened is refunded

Past `open_blocks`, anyone calls `timeout` and the stake goes back (a new outcome 9, **refund**).
Nothing else is paid and nothing is kept. This is option A, confined to where it is safe: the
dealer has seen no card, so not opening a hand can't be a way of choosing hands (property 2), and
the player gets exactly what they put in (property 3).

Two details close the free options (property 4):

- **An opening link past the deadline is refused** (abort 73), the mirror of v1's late-seed rule.
  Otherwise a player who reads the late link in the mempool knows their first three cards and can
  choose between letting it in and calling `timeout`.
- **A seed is refused until the hand is opened** (abort 70). Otherwise a player could hand the
  dealer the seed first, and the dealer could look before opening.
- **The dealer daemon keeps a margin.** It doesn't send opening links with the deadline a block
  or two away (a quarter of `open_blocks`, between 2 and 20 blocks). Links sent that late are
  public with almost no time to land, and a player who had read them would only have to crowd
  that one transaction out of the last blocks to turn a bad start into a refund. Inside the
  margin the daemon lets the hand be refunded.

A refunded bet also comes off the player's daily count: it was never a wager, and an outage
shouldn't use up a player's day.

### 4.3 After the seed, a stall pays exactly what the cards still allow

From the seed on the dealer knows everything, so property 2 needs the v1 rule: a missed reveal
pays the player the most the hand could still have paid. v2 computes that maximum from the cards
on the table instead of taking the maximum over all hands:

| The hand when the dealer stalls | v1 | v2 |
|---|---|---|
| Not opened (an account's bet) | n/a | **stake back** |
| A relayed hand's deal, no card out | 3:2 | 3:2 |
| A relayed hand's deal, the player's first card an ace or a ten-value card | 3:2 | 3:2 |
| A relayed hand's deal, the player's first card anything else | 3:2 | **1:1** |
| The player holds a natural, the dealer's hole card is owed | 3:2 | 3:2 |
| A hit's card, or the dealer's hand, is owed | 1:1 | 1:1 |

`node/vm/tests/blackjack2_rules.rs` walks every reachable state (35,279 of them) and checks, with
an oracle that searches every continuation, that the v2 forfeit is **exactly** the most the hand
could still pay: never less (property 2), never more (property 3, and I-1). The same walk finds
the 88 states where v1 paid more.

### 4.4 A circuit breaker

A hand that ends by the dealer's missed deadline at the house's cost (outcomes 6 and 7) sets the
table's pause to a third value, **tripped**. No bet is taken until the owner calls
`pause(false)`. Open hands play on, timeouts and withdrawals work, exactly as under an owner's
pause. A refund doesn't trip it: it cost nothing, and tripping would let a short restart close
the table. So one outage costs at most the hands that were open when it began (option C, as a
second layer).

Only the owner clears a trip. That is deliberate: a forfeit means something is wrong with the
dealer (its host, its node, its fee balance, its store), and the hot dealer key clearing it
automatically would let whatever caused the first forfeit cause the next.

### 4.5 Relayed hands

A relayed bet's seed is derived from the player and the post id, so it is public at the bet and
the dealer can't open such a hand blind. v2 therefore deals a relayed hand as v1 does (phase 1,
the dealer reveals the three cards), and applies 4.3 and 4.4 to it: a stall still pays the
player, by the tighter table, and trips the breaker.

Refunding a relayed stall instead would have made an outage free through relayers too, but it
would let the dealer alone decline the relayed hands it reads as losers. Today cheating a relayed
hand takes the dealer and the relayer (or the owner) together (BJ-03); the proposal doesn't
weaken that. The cost is a residual exposure, bounded by the breaker: at most `max_open_games`
relayed hands at `relay_max_bet`, once (375 FANG at the defaults), and only through a relayer the
owner approved and runs. Section 9 has the follow-up that removes it.

## 5. Economics

**House solvency.** Unchanged, and re-proved. A hand still reserves 1.5 × its stake at the bet.
A refund pays the stake and returns the whole reserve; every other ending is v1's or pays less
than v1. Tests: payouts against `u128` arithmetic for 20,000 stakes (each ending fits the stake
plus its reserve; what isn't paid returns to the bankroll; a stake cut in pieces never pays
more); a full table of naturals paid in full after the owner has taken out every free drop; the
invariant `balance = bankroll + reserved + stakes + table` after every call of a 500-hand soak.

**What an outage costs the house**, at the default limits (10 seats, 50 FANG maximum bet):

| | v1 | v2 |
|---|---|---|
| A bet placed while the dealer is down | +75 FANG to the player, every time | 0: refunded |
| Thirty such bets (three rounds of a full table) | 2,250 FANG at 3:2, more than the 2,000 FANG bankroll (which runs out first) | 0 |
| Hands in play when the dealer stops | up to 10 × 75 = 750 FANG | up to 10 × 50 = 500 FANG (1:1), plus 3:2 on naturals actually held |
| After that | the same again every `reveal_blocks`, until the owner pauses | nothing: the breaker holds |
| Through an approved relayer | as above | up to 10 × 37.5 = 375 FANG, once |

`node/vm/tests/blackjack2.rs` runs the first three rows against the compiled program.

**Player fairness.** A played hand is paid exactly as under v1 (10,000 hands compared card by
card). A player is never worse off for a dealer's failure: an unopened hand returns the stake
(and doesn't count toward the day's cap), a hand opened late still leaves the player all of
`seed_blocks` after the opening's deadline, and from the seed on a stall pays the most the cards
allowed. What a player does have to do is look again once `open_blocks` have passed: by then
the hand is either refundable or waiting for their seed. What a player loses is the windfall:
3:2 for a hand that was never dealt, and 3:2 for a stalled deal that could no longer be a
natural.

The player sees their first three cards before revealing the seed. That gives them nothing: the
seed was fixed by its hash at the bet, and not revealing it forfeits the stake, which is never
better than playing the hand. (Under v1 the player saw nothing before revealing; under both, a
seed that never comes costs the stake.)

**Griefing.**

- *Free seats.* While the dealer is down, bets are refunded, so seats and commitments can be used
  for the price of fees. They are worthless while the dealer is down, and when it returns it
  refunds stale hands itself and tops its commitments up. While the dealer is up, a hand is
  opened within a block or two, and from then on abandoning it costs the stake, as under v1.
- *Stopping the dealer on purpose.* An attacker who can take the dealer down for
  `reveal_blocks` turns their own in-flight hands into 1:1 wins: at most a table-full, once,
  then the breaker. Under v1 the same attacker took 3:2 per hand for as long as they liked.
  Option B would reduce this to pure griefing; see section 9.
- *Tripping the breaker.* A player can't cause a forfeit: only the dealer's missed deadline can.
  Getting ahead of the dealer's reveal with its own links doesn't delay it (BJ-08, already
  fixed). So the breaker can't be used to close someone else's table, short of stopping the
  dealer.

**Known limits (stated, not fixed).**

- *Block producers who side with a player.* The opening links are public as soon as the dealer
  sends them. A player who can keep that transaction out of every block until `open_blocks` has
  passed gets a refund after reading their first three cards: a free look, usable to drop bad
  starts. It takes control of block production for the whole window (150 blocks by default) and
  is the mirror of BJ-04, where producers who side with the house can keep a seed out. Under v1
  the same producers could keep the dealer's reveals out and collect 3:2 on everything, so v2 is
  strictly better here, but not immune. A larger `open_blocks` makes it costlier; a table on a
  chain whose producers aren't its players' doesn't have it. The cheap form of the same thing,
  crowding a link out of the last block or two before the deadline, is what the daemon's margin
  (4.2) is for; a dealer other than this daemon must keep one too.
- *A seed sent too early.* The program refuses a seed before the hand is opened, but a refused
  transaction's input is public all the same. A client that sends `reveal_seed` on seeing the
  opening in the mempool, before it is in a block, has shown the dealer the seed while the
  dealer can still withhold the opening. The wallet waits for the opened hand on the chain.
  Any other client must too, and must never reuse a seed whose reveal was refused.
- *An owner's limits.* `open_blocks` can be set as high as 86,400 (two days), like
  `reveal_blocks`, and a stake waits that long for a dealer that never opens. `seed_blocks`
  can't be under 20. Limits are public and apply to hands in play, as under v1 (BJ-02).
- *A weak or reused seed.* The opening is blind only if the seed can't be guessed from its hash.
  The wallet's seeds are 32 fresh random bytes, never used twice. A client that used guessable
  seeds, or the same seed for two hands (the hash binds the account and the seed, so the dealer
  would recognise it), would give the dealer a look before it opens, and with it the choice not
  to open. That costs such a player their winning hands (they are refunded instead); it costs
  the house and other players nothing.
- *A chain-wide pause of programs* (BJ-05) still ends hands by the clock; under v2 unopened
  hands are refunded rather than paid, which is an improvement, and the first forfeit after the
  pause trips the breaker.

## 6. Migration

**Nothing is altered in place.** A program's code is immutable, so there is no upgrade path for a
deployed table and this proposal doesn't invent one.

- `blackjack2` is a new guest crate (`node/vm/guests/blackjack2`) with its own rules crate
  (`bj2-core`, which depends on `bj-core` and calls it). `blackjack`, `bj-core` and the six
  committed modules are byte-for-byte what they were (a fresh build of the workspace, with the
  two new crates in it, gives the same six files; `SHA256SUMS` gained one line).
- No consensus rule, host function, protocol version or genesis changes. `blackjack2` imports
  only protocol v2's host functions, like `blackjack`. The node doesn't embed or deploy either
  program; a table exists only when someone uploads the code and creates it.
- A table says which rules it runs: a `version` view answers 2, and a stored record `b"V"` holds
  `[2]` (a v1 table has neither). Clients read it once.

**For an operator who wants v2:**

1. Upload `node/vm/tests/programs/blackjack2.wasm` with `blackjack2.iface` and create a new table
   (`createTable` in `@blockula/blackjack` works unchanged); fund it; set limits (fourteen now).
2. Point a dealer at it, with a **new seed store** (a store is bound to one table and one chain).
   The daemon needs no flag: it opens a hand when it sees one waiting to be opened.
3. Point the wallet at the new table's address (`NEXT_PUBLIC_BLACKJACK_TABLE`, or a table link).
4. On the old table: `pause(true)`, let open hands finish, `defund`. Players withdraw table
   balances whenever they like; nothing on the old table expires.

**What ships with this branch, and what doesn't:**

| Piece | State |
|---|---|
| Rules (`bj2-core`), program (`blackjack2`), interface, committed module | Done, tested. |
| TypeScript rules, codec, fairness check (`@blockula/blackjack`) | Done: v2 beside v1, held to Rust-written vectors. |
| Dealer daemon | Done: opens v2 hands, refunds ones it missed, reports a trip. Offline tests only. |
| Wallet table page | Done: waits for the opening, offers the refund, checks v2 hands, v2 rules text. Unit tests only; no Playwright spec for a v2 table yet. |
| X bot | Unchanged, and not needed: a relayed hand's flow is v1's. Its replies already word every outcome. A liveness check before relaying is a follow-up (section 9). |
| `docs/BLACKJACK.md` | A pointer to this proposal; the operator's guide stays v1's until v2 is adopted. |
| A run on a real chain (`scripts/blackjack-e2e.sh` for v2) | **Not done.** Needs a local chain; see the test plan. |

## 7. Test plan

Done on this branch:

- **Rules, on the host** (`cargo test --manifest-path node/vm/Cargo.toml --test blackjack2_rules`):
  the exhaustive state walk against the oracle (4.3); 10,000 hands that play out identically
  under v1 and under both v2 starts; payout conservation, solvency and overflow; the numbers.
- **The compiled program, on every engine** (`--test blackjack2`): the hand's new shape and every
  refusal on the way; a refund to the drop for 0, 1 and 2 opening links; the late link; a seed
  that never comes; forfeits after the seed and the breaker (who can clear it, what keeps
  working); relayed stalls for each row of 4.3; thirty bets against a dead dealer costing
  nothing; an outage with hands in flight costing exactly their stakes; a full table of naturals
  after a full defund; the largest hand a chain can hold; a 500-hand soak with every ending;
  fuel for every method (the dearest, a reveal, about 600,000 of 10,000,000).
- **The shape of the module** (`--test guests --test interfaces`): WebAssembly 1.0 without
  floats, protocol v2's imports only, the documented exports, its interface.
- **TypeScript** (`npm test -w @blockula/blackjack`): rules and bytes against
  `test/vectors2.json`; `verifyHand` on every prefix of 36 hands, opened and relayed, accepting
  the right ending and refusing every other.
- **The dealer** (`npm test -w @blockula/blackjack-dealer`): 300 v2 hands opened with exactly
  three links; never a fourth link before the seed, never a link past the deadline, never a
  timeout on a hand it owes cards on; restarts and lost answers; no link in any log line.
- **The wallet** (`npm test -w @blockula/web`): the hand's status, the refund, the check, the
  copy.

Before a v2 table carries anything that matters:

1. `node/vm/guests/build.sh --check` in CI (it now builds seven modules).
2. `scripts/blackjack-e2e.sh` adapted to upload `blackjack2` and play the v2 flow, including a
   dealer killed mid-run: bets placed while it is down must all be refunded, and the soak's
   bankroll must equal stakes less payouts. The dealer's `test/soak.ts` and `test/live.ts`
   drive that script and need the bet's new step.
3. A Playwright spec for the table page on a v2 table (`web/e2e/blackjack-world.ts` plays v1).
4. An independent review of `blackjack2/src/lib.rs` against `blackjack/src/lib.rs`. The program
   is a copy with its changes in a few places (`diff node/vm/guests/blackjack/src/lib.rs
   node/vm/guests/blackjack2/src/lib.rs`): the limits, `init`, `pause`, `open_hand`'s first
   phase, `reveal_seed`, `settle`'s trip, `reveal`'s opening branch, `timeout`, `version`.
5. A period on a chain whose FANG nobody minds losing, with a deliberate outage.

## 8. What changed in the program, exactly

| | v1 (`blackjack`) | v2 (`blackjack2`) |
|---|---|---|
| Stored version | none | `b"V"` → `[2]`; a `version` view |
| Limits | 13 (104 bytes) | 14 (112 bytes): + `open_blocks`, 1 to 86,400, 150 by default |
| A bet from an account starts | waiting for its seed (phase 0) | waiting to be opened (phase 6); the seed is owed once it is (phase 0) |
| `reveal` on such a hand | refused until the seed | takes the three opening links, deals no card, emits `opened`; refused past the deadline (73) |
| The seed's deadline | `seed_blocks` from the bet | `seed_blocks` from the opening's deadline, whenever the dealer opened |
| The daily cap | counts every bet | a refunded bet comes off the day's count |
| `reveal_seed` | starts the dealer's deadline | deals cards 1 to 3 (and settles a natural the upcard can't match); returns phase ‖ outcome |
| `timeout`, dealer's deadline missed | 3:2 or 1:1 | refund (unopened), or the tight forfeit; a forfeit trips the breaker |
| `paused` | 0, 1 | 0, 1 (owner), 2 (tripped); `pause(false)` clears both |
| Outcomes | 1 to 8 | 1 to 9 (9: refund) |
| Events | | + `opened`, `tripped` |
| Aborts | 1, 2, 40 to 72 | + 73 (too late to open) |
| Records | house 114, hand 157, player 195 bytes | the same layouts |

## 9. Open questions and follow-ups

- **Relayed hands and outages.** The residual exposure in 4.5 goes away if a relayed bet commits
  to a seed the dealer can't know, so that it too can be opened blind: the relayer sends a hash
  with the bet and the seed after the opening, as a wallet does. That keeps today's trust (the
  dealer and the relayer together) and makes a relayed stall refundable. It changes the X bot
  (a secret per hand, a second transaction) and wasn't attempted here. Until then, the bot
  should refuse to relay a bet while any open hand is past a dealer deadline.
- **Burning instead of paying (option B) for stalls after the seed.** If stopping the dealer on
  demand turns out to be cheap for attackers, pay the player the stake and lock the rest. It
  would be a third rules version; the argument for it is in section 3.
- **Who clears a trip.** Owner only, here. A table whose owner key is cold may prefer a
  dealer-side `resume` with a rate limit.
- **`open_blocks`.** 150 matches the dealer's other deadline. Shorter returns stakes sooner
  during an outage; longer makes the free look in section 5 costlier. It is the owner's to set.
- **`docs/BLACKJACK.md`** should be rewritten for v2 when a v2 table is adopted, not before: it
  is the operator's guide to what is deployed.
