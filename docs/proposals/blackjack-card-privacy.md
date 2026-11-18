# Blackjack: card privacy from the dealer's node (audit finding M-4)

Status: proposed, and implemented behind a setting that is off by default (`witnesses`).
Nothing on a chain changes: not the table program, not consensus, not any wire format.

## 1. The problem

The October audit recorded this as M-4 (medium, design, not changed):

> **A remote node sees every card the dealer is asked for.** The daemon decides which links to
> reveal from one node's unproven reads. A node that lies about a hand's phase (reports "the
> player stood" while the player is still choosing) is handed the hole card and every draw, in a
> transaction it need not relay. Pinning the chain id and genesis hash does not help. With
> `allowRemote` over plain `http` the same is open to anyone on the path.

It helps to be exact about what the node does and doesn't see, because the short title suggests
something the code doesn't do.

**What the dealer does not do.** It never asks a node what a card is. It doesn't call the
program's views at all: `Table` (`services/blackjack/core/src/table.ts`) reads the program's
stored bytes through `GET /api/program/{address}/storage`. Every card is computed on the
dealer's machine from the chain's secret in its encrypted store (`chainFromTop`) and the
player's seed (`card(link, seed, game, j)`), and which links are owed is computed locally too
(`owedLinks`). So "compute the cards locally instead of asking a node" is already how it works,
and no padding, batching or private retrieval of reads would change anything: the reads are of
public records, and they are not where the cards go.

**Where the cards go.** The dealer's only secret output is the `reveal` transaction, whose input
is the links a hand is owed. It is posted to the same node the reads came from. A link is public
the moment it leaves the machine: link `j` gives every earlier link by hashing, and with the
hand's seed and id, every card up to `j`.

**So the leak is in the question "what is owed?"**, and the dealer takes the answer from the
node. Each tick it reads the house record, the open list and each open hand's record, and
`planHand` reveals `owedLinks(game, chain)` for any hand whose phase is the dealer's. The only
check on a hand's record is that its newest link is on the stored chain
(`chain[g.dealt] === g.link`), and every link already revealed is public, so the node passes
that check with any lie it likes. Nothing ties the record to the chain's real state.

### What a lying node gets

`T` is the truth, `L` what the node tells the dealer, and "learns" means the reveal's links
arrive at the node, which then drops the transaction.

| # | Truth `T` | Lie `L` | The node learns | Worth |
|---|---|---|---|---|
| 1 | Player's turn, 3 cards out | phase is `Dealer` ("the player stood") | links 4…n: the hole card and every card the dealer would draw. Link 4 is also the card a hit would get | The player's whole decision. Hit or stand with the next cards known |
| 2 | Player's turn | phase is `Hit` | link `dealt + 1`: the next card | Same, one card at a time; repeat after each real hit |
| 3 | Bet placed, seed not revealed (`Seed`) | the seed is revealed, phase is `Deal` | links 1–3 (4 against a possible natural), for the committed seed | The player sees the deal before choosing to reveal the seed. A bad hand costs only what losing it would have |
| 4 | Commitment `k` unused, no bet yet | a hand `k` exists: `dealt = 0`, `link = c_k`, any seed, phase `Deal` | links 1–3 of a hand nobody has bet on; with lie 1 on top, the rest | **The worst one.** The attacker now chooses the seed for game `k` knowing the links, so chooses the cards: search seeds for a natural, bet the maximum, repeat for every unused commitment. This is BJ-01 again, by another door |
| 5 | Anything | an old, genuine record | nothing new (see 4.4) | None for cards. It can stall the dealer |

Lies 1–3 need the node's operator to be, or to work with, the player at the table. Lie 4 needs
only an account that can bet. The loss is bounded by the table's limits (the maximum bet, the
bankroll's share, the daily cap, and how many commitments are unused), not by anything in the
dealer.

The daemon's own guard is the default refusal of any node but one on its own machine. The
deployed dealer (`deploy/blackjack-dealer/fly.toml`) sets `allowRemote` and talks to one remote
node over `https`, so it is exposed to that node's operator and to whoever compromises it.

## 2. Threat model

**What is protected.** A link is revealed only when the chain's real, final state owes it. That
is the whole property: unrevealed links (the hole card, the next draw, the deal of a hand whose
seed is not on the chain, any link of an unused commitment) stay on the dealer's machine.

**The dealer's machine is trusted**, with its key file, the store's secret and the store. So is
the chain as a whole: a majority of validators that rewrites state can do worse than read cards.

**Honest-but-curious node.** It answers truthfully and relays what it is given, and it records
everything. It learns:

- which records the dealer reads, and when (all public);
- each `reveal` a moment before the rest of the network, which is what any node's mempool shows
  (`/api/mempool`) and what BJ-10 already accepts;
- the dealer's address and its timing.

It learns no link that isn't owed. **For this adversary there is nothing to fix**, and that is
worth saying plainly: an operator who trusts their node to be honest has no card leak today.

**Malicious node** (the operator, someone who took the machine over, or with plain `http` anyone
on the path). It can answer anything, and drop, delay or reorder what it is given. It can't forge
a signature, or a Merkle proof against a state root it didn't make up, and it can't make other
honest nodes agree with a root the chain never had. It may work with a player. Today it gets
everything in the table above. Separately, and with or without this proposal, it can censor the
dealer (drop its reveals so the hand times out in the player's favour); that is availability,
which is M-3's subject, and section 6 says what remains.

**Out of scope.** Every node the dealer is told about colluding; a compromised dealer machine;
the relayer's powers (BJ-03); the wallet's side of the seed (BJ-04, BJ-12).

## 3. Options

### A. Run your own node beside the dealer (today's default)

The daemon already refuses a remote node unless `allowRemote` is set. A node the operator runs
checks every block's finalization certificate as it syncs, so its state is the chain's.

- For: no code, the strongest answer, and what the README's checklist already says.
- Against: it isn't what is deployed. A second machine-sized process (the node, its disk) beside
  a daemon that fits in 256 MB is the cost the Fly deployment was shaped to avoid. And the
  daemon can't tell a node that is its operator's from one that merely listens on `127.0.0.1`
  through a tunnel. It stays the recommendation where it's affordable; it isn't a fix for the
  rest.

### B. Proven reads: every record the dealer acts on comes with a Merkle proof, and the state root is confirmed by independent nodes (recommended)

Every node already serves `GET /api/proof/{namespace}/{key}`: a record, its Merkle proof and the
state root after a block. Program storage is in the state tree (namespace 69, key = the
program's 32 address bytes ‖ the storage key), and the SDK already has the verifier
(`sdk-ts/src/proof.ts`, kept in step with the node by shared vectors) and the witness pattern
(`Client.provenRecord`, used for names and program records).

The dealer reads each record this way, checks the proof against the root, and asks each
configured witness (another node of the same chain, pinned to the same genesis) for the state
root of that block. Only a record under a root the witnesses confirm is acted on. A lying node
then needs a witness to lie with it.

- For: closes every row of the table against a single lying node; uses machinery that exists
  and is tested; costs one extra small request per block per witness; needs no change to the
  program, the node or the chain; a witness learns nothing (it is asked for block headers).
- Against: needs at least one other node the operator has reason to believe is independent. The
  dealer now depends on the witnesses being up (4.5). It moves the trust from one node to "not
  all of these at once", it doesn't remove it.

### C. A light client in the dealer: check finalization certificates

Nodes serve finalized blocks with their certificates (`/api/sync`) and the validator-set history
with handoff signatures (`/api/epochs`). A dealer that verified those would need no witness: a
state root would be the chain's because two thirds of the stake signed it.

- For: the right end state. One node is enough, and it can be anyone's.
- Against: it is a port of the node's consensus verification into TypeScript (certificates,
  validator-set handoffs, weak subjectivity), which must match the node byte for byte and be
  kept matching. That is a project with its own review, not something to add to a dealer in
  passing. B is a step toward it: C would replace only "who vouches for the root" inside the
  same reader.

### D. Things that don't help, and why

- **Padding, batching or hiding the reads (including private information retrieval).** The
  reads are of public records. The node learns nothing from which ones are read that it can't
  read itself. The leak is in the write.
- **Reading only commitments or hashes.** The dealer's reads are already of records that hold
  no unrevealed card. What matters is whether the answers are true.
- **Asking for evidence the node can't forge: the player's signed `stand` or `hit`.** A node
  alone gains nothing from the cards; it needs a player, and a player who is in on it will
  happily sign a `stand` that never reaches a block (and then a `hit` with the same nonce).
  Signed intent isn't inclusion. Only finality is.
- **Sending the reveal to a different node than the one read from.** It just changes which node
  has to lie.
- **Encrypting the reveal to the validators.** That is a change to the chain.

## 4. The recommended design (B)

### 4.1 Reads

A new reader, `ProvenTable` (`services/blackjack/core/src/proven.ts`, exported as
`@blockula/blackjack/proven`), is a `Table` whose one primitive, `entry(key)`, is replaced. It:

1. asks the node for `/api/proof/69/{program's 32 bytes ‖ key}`;
2. checks that the answer is for exactly that record and that the proof verifies against the
   root it names (`checkProofResponse`); a proof of absence is how "no such hand" is learned;
3. has the root for that block confirmed by the witnesses (4.2);
4. checks the proof isn't old (4.4);
5. takes the program's bytes out of the stored entry (format byte 1, then the value, the
   payer, the deposit and the capacity; anything else is refused) and hands them to the same
   decoders as before.

The storage route is never used in this mode: the reader is built without it, so no read can
fall back to an unproven one. Everything above the reader (`Dealer`, `inspect`, `owedLinks`,
the store) is unchanged, which is why the calls it sends are the same.

### 4.2 Witnesses

A witness is asked two things: its status (its head, and through the SDK's pinned `Client` its
chain id and genesis hash) and `GET /api/block/{height}` for the block's `state_root`.

- A root is confirmed when `witnessQuorum` witnesses report the same root for that height. The
  default is all of them.
- A witness that hasn't reached the block yet is waited for (a few seconds; blocks are a second
  apart or less).
- **A witness that reports a different root for that height is never outvoted.** One
  disagreement refuses the read whatever the quorum, and is logged as an error
  (`node_disagreement`): either the node or that witness is lying or broken, and a person
  should look.
- A confirmed root is remembered by height, so a tick's reads at one height cost one round of
  witness requests, not one each.

Choosing witnesses is the operator's judgment and the design's soft spot. Good ones, in order:
a node the operator runs somewhere else (a home machine is enough: it is asked for headers),
a validator run by someone else, a public node of another operator. Two URLs for one machine are
no witness; the configuration refuses a witness with the node's own URL, and can do no more
than that.

### 4.3 What a lying node can still do, row by row

| Lie | Why it fails |
|---|---|
| 1, 2 (phase) | The hand's record is proven under a confirmed root, and the real record says `Player` |
| 3 (seed) | Same: the real record is in phase `Seed` and holds the seed's hash, not a seed |
| 4 (a hand that doesn't exist) | The proof for that hand's key is of absence. The open list and the house record are proven too |
| A proof under a root the node made up | No witness has that root at that height |
| A real proof for another record, another table or another chain | The answer must be for the exact key (which begins with the table's address); witnesses are pinned to the genesis hash |
| An altered value under a real root | The proof doesn't verify |

### 4.4 Old state

The node can serve a genuine old record with a genuine proof, and witnesses confirm any real
root. For cards this is harmless, and the reason is worth writing down because the design
leans on it: **a hand only moves forward, and ids are never reused.** A link that some past
state owed is, in the present, either already revealed or still owed. No past state owes a link
the present doesn't. (Timeouts need no such argument: the program checks the deadline itself.)

It is not harmless for the dealer's punctuality, so the reader also refuses a proof more than
`proofMaxLagBlocks` (default 10) behind the newest head the witnesses report. That turns a node
feeding the dealer the past into a loud failure instead of a quiet stall.

### 4.5 Failing closed

When a root can't be confirmed (a witness is down, behind for too long, or disagrees) the read
throws, the tick fails and is retried, and **nothing is revealed**. That is the point, and it
has a price: while it lasts the dealer deals nothing, and a hand whose deadline passes pays its
player. `witnessQuorum` is the dial: with three witnesses and a quorum of two, one may be down.
A disagreement still stops everything.

### 4.6 The sender

The `Client` the dealer signs with is given the same witnesses, so the SDK proves the program's
record (the version digest a call binds) and refuses a signing height ahead of the witnesses'
heads. Neither is about cards; both come free with the setting.

### 4.7 Settings (all new, all optional)

| Setting | Meaning |
|---|---|
| `witnesses` | Nodes of the same chain, comma-separated (or a JSON list in the config file). Empty: today's behaviour, unchanged |
| `witnessQuorum` | How many must confirm each state root. Default: all |
| `proofMaxLagBlocks` | How far behind the witnesses' newest head a proof may be. Default 10 |

Rules: a witness can't be the node itself or listed twice; a witness on another machine must be
`https`; with any witness on another machine the chain must be pinned (`chainId`,
`genesisHash`), as it must for a remote node today.

Two warnings are added for configurations that already run and keep running: `unproven_remote_node`
when the node is another machine's and no witness is set, and `insecure_rpc` when it is reached
over plain `http`. They are log lines, not refusals, so that upgrading the daemon changes
nothing a deployed dealer does.

## 5. What is deliberately not changed

- The table program, its interface, its storage layout and its build (`SHA256SUMS`).
- The node, the proof format, any API.
- The dealer's behaviour without `witnesses`: the same reads, the same calls, the same logs
  (plus the two warnings above on a remote node).
- `deploy/blackjack-dealer/fly.toml`. Turning the setting on for the deployed dealer is the
  owner's step, once there is a second node to name (section 8).

## 6. What still leaks, or can still go wrong

1. **Every node the dealer knows colluding.** B's guarantee is "not all at once". Option C
   removes it.
2. **A reveal is public from the moment it is submitted**, to the node first. With proven reads
   those links are owed, so this is only the head start BJ-10 describes.
3. **Censorship.** A node can still drop the dealer's reveals, or answer nothing, and let the
   hand time out in the player's favour. Proven reads don't address it. Sending each reveal
   through the witnesses as well (as the wallet does for a seed's reveal, BJ-04) is the natural
   next step and belongs with M-3.
4. **Unproven things the dealer still hears from one node:** a transaction's status and its
   events, and the dealer's balance. A lie there costs time (a retry after a back-off) or
   produces a wrong log line; it can't cause a reveal, because every plan starts from proven
   records.
5. **Timing and interest.** The node sees which records the dealer reads and when it reveals.
6. **The witnesses see** the dealer's IP address and that someone is following the chain. No
   table, no hand, no link.
7. **Liveness now depends on the witnesses** (4.5).

## 7. Test plan

Offline, in `npm test -w @blockula/blackjack` and `npm test -w @blockula/blackjack-dealer`:

- **The reader** (`core/test/proven.ts`): a record and an absence are read from proofs made by
  an independent prover; an altered value, an altered root, a proof for another key, another
  program or another namespace, a malformed stored entry and a stale proof are each refused;
  witnesses that are behind are waited for, a quorum is honoured, a disagreeing witness is never
  outvoted, roots are confirmed once per height.
- **The leak, shown first** (`dealer/test/privacy.ts`): the dealer on today's path against a
  lying node, for each row of the table in section 1. The test asserts that the links *do*
  arrive, so the finding is on record as a failing property rather than a paragraph.
- **The fix**: the same lies with proven reads. A monitor stands where the node stands and
  holds every byte the dealer hands it (each call's input, each request's path) against the
  true table: at no moment may it have received a link the truth doesn't owe, and a witness may
  never receive a link at all. It runs for the scripted lies and for a few hundred randomized
  rounds of play in which the node lies at random.
- **Same results**: with an honest node, a dealer on proven reads and a dealer on today's reads,
  given the same store, seeds and moves, send byte-for-byte the same calls and settle the same
  hands; every settled hand passes `verifyHand` down to its commitment.
- **Logs**: the existing scan (no link and no secret in any log line) covers the new paths,
  including the errors a bad proof produces.
- **Configuration**: the new settings, their refusals and the two warnings.

Live, on a local chain of real nodes (`TEST=live WITNESSES=1 scripts/blackjack-e2e.sh`): the
daemon as a child process reading proofs from one validator with the others as witnesses, the
same 30 hands, kill and restart, and timeouts as the existing live test. This is what holds the
stored-entry format and the meaning of a block's `state_root` to the real node.

## 8. Rollout

1. Merge: nothing changes for a running dealer.
2. Stand up or choose a witness for the table's chain. For the demo chain, a full node on any
   machine the owner controls (`blockula join`), reachable over `https`.
3. `status --witnesses …` against the live table: it reads everything through proofs and sends
   nothing. Then `run --dry-run --witnesses …` beside the running dealer for a while (a dry run
   takes no lock on the live store only if pointed at its own directory; use a copy).
4. Set `BJ_DEALER_WITNESSES` on the deployed dealer and restart it at a quiet moment.
5. Watch for `node_disagreement` (an alarm) and `tick_failed` (a witness that is down).
