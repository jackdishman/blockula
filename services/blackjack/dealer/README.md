# The blackjack dealer daemon

Blackjack on Blockula is a game played with FANG ([docs/BLACKJACK.md](../../../docs/BLACKJACK.md)). The table is a program; the dealer is this daemon. For one table it:

- **commits ahead of the bets.** For each future game it draws a secret, hashes it down into a chain and posts the last hash (`commit_batch`). A bet takes the next unused commitment, so the cards were fixed before the player's seed was known.
- **waits for the seed.** A bet from an account is two transactions: `bet`, with the stake and the hash of the player's seed, then `reveal_seed`, with the seed. Until the seed is on the chain the daemon owes nothing and sends nothing for that hand: no card can be known without the seed, and the program refuses a link before it.
- **deals.** Once a hand's seed is revealed (a relayed hand: from its bet), it reads the table and reveals the hand's next links, in order (`reveal`). The program turns links into cards.
- **ends a bet whose seed never comes.** A player has the table's `seed_blocks` to reveal. Past that the daemon calls `timeout`: the hand ends with no cards (outcome 8, `no_seed`), its stake goes to the bankroll, and its seat and its reserve are free again.
- **stands idle players.** A player who lets their deadline to move pass is stood (`timeout`), and the hand is dealt to its end.

If the dealer misses a reveal deadline, anyone may call `timeout` and the player is paid as if they had won. So the daemon is built to be restarted at any moment: everything it does is decided from the chain and from its seed store, and nothing else.

FANG has no value. This is a game.

## What it needs

- A node of the table's chain **on this machine** (`--rpc http://127.0.0.1:…`). The daemon refuses another machine's node unless you set `allowRemote` and pin the chain (below).
- The table's address (the program's `boo1…` address).
- Its own account, named as the table's `dealer` by the owner, holding some FANG: every reveal pays a fee, and every stored commitment bonds a small deposit (about 0.02 FANG) until a bet takes it. On a local chain 1,000 hands cost the dealer under 10 FANG in all. `status` warns when the balance is under `lowBalance`.
- Node 22 or newer.

## Set it up

```bash
# 1. Make the dealer's key and the store's secret (both mode 0600), and print the address.
npx tsx services/blackjack/dealer/src/cli.ts init --dir ~/.blockula/blackjack-dealer --rpc http://127.0.0.1:18080

# 2. The table's owner names that address as the dealer: when the table is created
#    (`createTable(owner, wasm, iface, dealerAddress)`), or later with `set_dealer`.
#    Send the address some FANG.

# 3. See what it would do, without sending anything.
npx tsx services/blackjack/dealer/src/cli.ts run --dry-run --rpc http://127.0.0.1:18080 --table boo1…

# 4. Run it.
npx tsx services/blackjack/dealer/src/cli.ts run --rpc http://127.0.0.1:18080 --table boo1…

# At any time:
npx tsx services/blackjack/dealer/src/cli.ts status --rpc http://127.0.0.1:18080 --table boo1…
```

After `npm run build -w @blockula/blackjack-dealer` the same commands are `node services/blackjack/dealer/dist/cli.js …` (the package's bin, `blackjack-dealer`).

`init` never overwrites a file: run it again and it keeps what is there. `run` refuses to start if the key file or the secret file can be read by the group or by others (`chmod 600` them, and treat what they held as seen).

Run one daemon a table. The store is locked while a daemon uses it; a second one on the same store refuses to start.

## Commands

| Command | Does |
|---|---|
| `init` | Makes the key file and the secret file if they are absent, and prints the dealer's address. With `--rpc` the address has that chain's prefix. |
| `run` | Deals until it is stopped (SIGINT or SIGTERM: it lets what it sent come back, then exits 0). `--dry-run` reads the table and logs what it would commit, reveal and time out; it sends nothing and writes no secret. |
| `status` | The address and whether it is the table's dealer, its balance (with a warning when low), unused commitments and how many the store can deal, open hands and whose move each is, the store's size and whether its backup is current. `--json` for a machine. It only reads. |

## Settings

Each setting can be a flag, an environment variable, or a key in a JSON file (`--config dealer.json`, or `BJ_DEALER_CONFIG`). A flag wins over the environment, which wins over the file. Secrets are never settings: they are read from files (or the keychain), never from the command line or the environment.

| JSON key | Flag | Environment | Default | |
|---|---|---|---|---|
| `rpc` | `--rpc` | `BJ_DEALER_RPC` (or `BLOCKULA_RPC`) | none: required | The node's URL. Must be `127.0.0.1`, `localhost` or `::1` unless `allowRemote`. |
| `table` | `--table` | `BJ_DEALER_TABLE` | none: required | The table: the program's address. |
| `dir` | `--dir` | `BJ_DEALER_DIR` | `~/.blockula/blackjack-dealer` | Where the files below live unless given. |
| `keyFile` | `--key-file` | `BJ_DEALER_KEY_FILE` | `<dir>/dealer-key.json` | The dealer's signing key (its wallet seed). Mode 0600. |
| `secretFile` | `--secret-file` | `BJ_DEALER_SECRET_FILE` | `<dir>/store-secret.json` | The key the seed store is encrypted with. Mode 0600. |
| `storePath` | `--store` | `BJ_DEALER_STORE` | `<dir>/seeds.enc.json` | The encrypted seed store. |
| `backupPath` | `--backup` | `BJ_DEALER_BACKUP` | `<storePath>.backup` | The store's backup, rewritten on every change. **Point it at another disk.** |
| `lowWater` | `--low-water` | `BJ_DEALER_LOW_WATER` | 64 | Top up when fewer unused commitments than this are left. |
| `target` | `--target` | `BJ_DEALER_TARGET` | 192 | Top up to this many. |
| `batch` | `--batch` | `BJ_DEALER_BATCH` | 64 | The most commitments in one `commit_batch` (the program takes at most 64). |
| `pollMs` | `--poll-ms` | `BJ_DEALER_POLL_MS` | 1000 | Milliseconds between looks at the table. Keep it under the chain's block time. |
| `dryRun` | `--dry-run` | `BJ_DEALER_DRY_RUN` | false | Log what would be sent; send nothing, store nothing. |
| `lowBalance` | `--low-balance` | `BJ_DEALER_LOW_BALANCE` | `5` | Warn when the dealer's balance is under this many FANG. |
| `keepSettled` | `--keep-settled` | `BJ_DEALER_KEEP_SETTLED` | 256 | Secrets of settled games kept before they are pruned. |
| `txValidBlocks` | `--tx-valid-blocks` | `BJ_DEALER_TX_VALID_BLOCKS` | 120 | Blocks a commit or a reveal stays valid. (A timeout's is always short: half of the table's `reveal_blocks`.) |
| `fullFuel` | `--full-fuel` | `BJ_DEALER_FULL_FUEL` | false | Reserve the chain's maximum fuel for every call instead of the daemon's own figures. Dearer, and three such calls fill a block: leave it off. |
| `allowRemote` | `--allow-remote` | `BJ_DEALER_ALLOW_REMOTE` | false | Let `rpc` be another machine's node. Needs `chainId` and `genesisHash`. |
| `chainId` | `--chain-id` | `BJ_DEALER_CHAIN_ID` | not pinned | The chain id the node must report. |
| `genesisHash` | `--genesis-hash` | `BJ_DEALER_GENESIS_HASH` | not pinned | The genesis hash the node must report. |
| `keychain` | `--keychain` | `BJ_DEALER_KEYCHAIN` | false | macOS: read the store's key from the keychain instead of `secretFile`. |
| `keychainService` | `--keychain-service` | `BJ_DEALER_KEYCHAIN_SERVICE` | `blockula-blackjack-dealer` | The keychain item's service. |
| `keychainAccount` | `--keychain-account` | `BJ_DEALER_KEYCHAIN_ACCOUNT` | the table's address | The keychain item's account. |

How many commitments to keep: a bet is refused (abort 53) when none is left, so `lowWater` should cover the bets the table can take while the daemon is down or a batch is on its way. Each unused commitment holds a small deposit of the dealer's.

Pin the chain (`chainId`, `genesisHash`) whenever the node isn't a throwaway local one: the daemon then refuses a node that reports another chain.

## The seed store, and backing it up

The store holds, for each game not yet settled, the secret every card of that game comes from. Whoever reads it knows the cards; whoever loses it can't deal.

- It is one file, encrypted and authenticated (AES-256-GCM) with the key in the secret file. A store that was changed, or opened with another key, or made for another table, is refused and the daemon doesn't start.
- Every change writes a complete new file, flushes it, renames it over the old one and flushes the directory; then does the same for the backup. A crash leaves the old store or the new one, never half of one.
- A chain's secret is in the store **and** in the backup before its commitment is sent. If either can't be written, nothing is committed and the log says `store_write_failed`.
- Secrets are kept by game index. A batch that is planned again (after a restart, a lost transaction, an abort) commits the same chains, never a second set.
- Settled games' secrets are pruned (`keepSettled` are kept), only once the chain shows the hand's record gone.
- No secret and no link is ever logged, put in an argument list or included in an error message. The log scrubs any long run of hex that isn't a transaction's id.

**Back up three things, off the machine:** the key file, the secret file, and the store's backup (`backupPath`). The first two never change. The backup changes with every batch committed (about every `target − lowWater` hands), so point `backupPath` at another disk or a synced folder rather than copying it by hand; it is encrypted, and useless without the secret file.

### Restore from the backup

The store is lost or damaged, the backup is fine:

```bash
# Stop the daemon first.
cp /path/to/backup/seeds.enc.json.backup ~/.blockula/blackjack-dealer/seeds.enc.json
npx tsx services/blackjack/dealer/src/cli.ts status --rpc … --table …   # "N this store can deal", no ⚠
npx tsx services/blackjack/dealer/src/cli.ts run --rpc … --table …
```

If the store is missing and the backup is there, `run` refuses to start and says exactly this, rather than starting on an empty store.

### If the store is lost for good

The table still holds commitments whose secrets are gone. A bet that takes one can't be dealt: at its reveal deadline anyone calls `timeout` and the player is paid as a win (3:2 on a hand that was never dealt). To stop that:

1. **The owner pauses the table** (`pause(true)`). No new bet is taken; open hands, timeouts and withdrawals carry on.
2. Start the daemon on a new, empty store (`init` makes a new secret if you lost that too; the key file must be the same one, or the owner names the new address with `set_dealer`). It logs `lost_secrets` with the range of games it can't deal, and commits new chains after them.
3. **The owner drops the lost commitments** with `skip_commits(count)` (at most 64 a call), until the game the log names is next: `status` then shows every unused commitment as one the store can deal.
4. Open hands the daemon can't deal are paid to their players by `timeout` at their deadlines. Let them, or call it.
5. The owner unpauses.

The daemon never calls `timeout` on a hand it owes cards on, whatever state its store is in.

### The keychain (macOS, optional)

With `--keychain` the store's key is read with `security find-generic-password -s <service> -a <account> -w` instead of from the secret file. Put it there once; `security` asks for the password, so it is never in an argument list:

```bash
openssl rand -hex 32 | pbcopy          # 64 hex characters, on the clipboard
security add-generic-password -s blockula-blackjack-dealer -a <the table's address> -w   # paste when asked
```

Keep a copy of that key somewhere safe: without it the store and its backup can't be read.

## The log

One JSON object a line on standard output: `time`, `level`, `action`, and what it was about (`game`, `tx`, `height`, `cards`, `outcome`, `fuel`…).

| `action` | Means |
|---|---|
| `started`, `stopped` | The daemon's start (with what it found: unused, servable, stored) and clean stop. |
| `commit`, `reveal`, `timeout` | A call that is final: its `tx`, the block, the cards dealt, the outcome and payout if the hand settled. |
| `would_commit`, `would_reveal`, `would_timeout` | A dry run's plan. |
| `…_unconfirmed` | The call was submitted and its fate isn't known yet. It is followed by its id, not sent again. |
| `…_lost` | That transaction never reached the node, or can't run any more: the call is planned again from the chain. |
| `…_refused` | The program refused it. With `moved: true` the table had moved on (someone's call got there first: aborts 54, 57, 58, 59, 60, 61), which is harmless. A reveal that someone got ahead of with some of its links isn't refused: the program passes over the links the hand already holds and deals the rest. |
| `…_not_sent` | Nothing was submitted (the node refused it, or couldn't be reached). Tried again after a short wait. |
| `…_out_of_fuel` | A call ran out of the fuel it reserved; it is sent again at once with the chain's maximum. |
| `timeout` with `turn: "seed"`, `outcome: "no_seed"` | A bet's seed never came and the daemon ended the hand at the seed's deadline. This is the player's doing, not a fault: nothing to fix. Many of them from many accounts is someone holding seats; each costs them a stake. |
| `timeout` with `turn: "player"`, `stood: "idle"` | A player let their time to move pass; the hand was stood and is dealt to its end. |
| `past_deadline` (warn) | A hand the dealer owes cards on is past its deadline. It is still dealt if nobody has called `timeout` yet. Find out why the daemon was late. |
| `low_balance` (warn) | Send the dealer FANG. |
| `tick_failed`, `start_failed` (warn) | The node didn't answer. Retried, with a wait of at most 5 seconds. |
| `lost_secrets`, `hand_without_secret`, `secrets_mismatch` (error) | The store lacks secrets the table holds commitments for: see "If the store is lost for good". |
| `hand_without_seed` (error) | A hand the dealer owes cards on shows no seed. The program never stores one that way: the node's answer is wrong. Nothing is revealed for it. |
| `not_the_dealer` (error) | The key isn't the table's dealer: the owner calls `set_dealer`. |
| `store_write_failed` (error) | The store or its backup couldn't be written. Nothing was committed. Check the disks. |

## How it stays safe to restart

- **The chain is the state.** Each look reads the house, the open hands and each hand's record, and plans from those. Nothing in memory is needed after a restart.
- **Every call can be repeated.** The program takes a link only as its chain's next (abort 58; a link the hand already holds is passed over), a batch only where the last one ended (61), and a timeout only past the deadline (60).
- **A transaction of unknown fate is followed by its id** until it is final, failed or expired. Only one the node has never heard of is planned again.
- **A hand waiting for its seed is left alone.** The daemon doesn't touch its store for it and reveals nothing, before or after a restart. When the seed lands, the next look deals the hand.
- **A timeout is only ever sent for a hand that waits for its player** (for the seed, or for a move), and it is valid for half of `reveal_blocks` at most, so a copy that lands late can't find the hand in the dealer's phase with a deadline already behind it. One that meets a seed that arrived just in time is refused (abort 60), pays nobody, and the hand is dealt.
- **Outcomes to watch.** A `settled` event with outcome 6 or 7 means the dealer missed a deadline and the player was paid for it: find out why. Outcome 8 means a player never revealed a seed: that is theirs, and expected now and then.
- **Fuel.** A call pays for the fuel it reserves, and a block carries only so much of it: at the chain's maximum for every call, three calls fill a block. The daemon reserves what its calls were measured to use, with a wide margin.

## Before a launch: the dealer's part of the checklist

- [ ] The key file, the secret file and the backup path are backed up off the machine, and a restore has been tried (`status` shows no ⚠).
- [ ] `backupPath` is on another disk than `storePath`.
- [ ] `status` shows the address is the table's dealer, a balance well over `lowBalance`, and `unused` ≥ `lowWater`.
- [ ] The chain is pinned (`chainId`, `genesisHash`), and `rpc` is a node on this machine that you run.
- [ ] `pollMs` is under the block time; the table's `reveal_blocks` leaves room for a restart (a restart takes a few seconds; the default 150 blocks is minutes).
- [ ] The daemon runs under a supervisor that restarts it (launchd, systemd), with its output kept. One daemon a table.
- [ ] Something watches the log for `level: "error"`, `past_deadline` and `low_balance`, the process for restarts, and the table's `settled` events for outcomes 6 and 7 (the dealer missed a deadline). Outcome 8 (a seed that never came) is no alarm.
- [ ] The owner knows the lost-store steps above (`pause`, `skip_commits`).
- [ ] A dry run (`run --dry-run`) against the launch table logged the plan you expect.

## Hosting on Fly

[deploy/blackjack-dealer](../../../deploy/blackjack-dealer) runs the daemon on Fly.io the way the house bots run (`deploy/bots`): one machine, no public ports, a volume at `/data`.

| File | |
|---|---|
| `fly.toml` | The app (`blockula-blackjack-dealer`, `iad`), one `shared-cpu-1x` machine that is always restarted, the volume, and the settings that aren't secret (`BJ_DEALER_RPC`, `BJ_DEALER_TABLE`, `BJ_DEALER_ALLOW_REMOTE`, `BJ_DEALER_CHAIN_ID`, `BJ_DEALER_GENESIS_HASH`). No `[[services]]`, no `[http_service]`. |
| `Dockerfile`, `Dockerfile.dockerignore` | Builds the SDK, `@blockula/blackjack` and this package from the checkout; the image holds the built JS and the locked production dependencies, and the dealer runs as `node`. |
| `entrypoint.sh` | Starts as root only to own `/data`, puts the first boot's secrets into files, waits at the start gate, then runs `blackjack-dealer run` as `node`. |

The node is another machine's, so the chain is pinned and `allowRemote` is set (see "Settings"). Everything lives on the volume: `/data/dealer-key.json`, `/data/store-secret.json`, `/data/seeds.enc.json` and its backup `/data/seeds.enc.json.backup`. The backup is on the same disk as the store there, so **copy the key file, the secret file and the backup off the machine yourself** (the backup after every batch, or on a schedule): a lost volume is a lost store.

**The start gate.** The entrypoint deals nothing until `/data/ready` exists; until then it logs `waiting for /data/ready` every 15 seconds. That is what lets a table move here without two dealers ever running on it:

```bash
# 1. Once: the app and its volume. Never allocate a dedicated IPv4 (or any public IP) for it.
fly apps create blockula-blackjack-dealer
fly volumes create data -a blockula-blackjack-dealer -r iad -s 1

# 2. The two secrets, each the whole JSON file `init` made (one line), piped in so that neither is
#    in an argument list. Skip this if you'd rather copy the two files to /data in step 4.
{ printf 'DEALER_KEY_JSON=%s\n' "$(cat <dir>/dealer-key.json)"
  printf 'STORE_SECRET_JSON=%s\n' "$(cat <dir>/store-secret.json)"
} | fly secrets import -a blockula-blackjack-dealer --stage

# 3. Deploy, from the repository's root. One machine: --ha=false.
fly deploy . -c deploy/blackjack-dealer/fly.toml \
  --ignorefile deploy/blackjack-dealer/Dockerfile.dockerignore --ha=false
fly logs -a blockula-blackjack-dealer        # "waiting for /data/ready"

# 4. Stop the dealer that runs now (wait for its "stopped"), then copy its encrypted store over.
#    A table with no store yet skips the copy.
fly ssh sftp shell -a blockula-blackjack-dealer   # put <dir>/seeds.enc.json /data/seeds.enc.json

# 5. Open the gate. The dealer starts within 15 seconds.
fly ssh console -a blockula-blackjack-dealer -C "touch /data/ready"

# 6. Once it logs "started": the key and the secret are on the volume, so the Fly secrets go.
fly secrets unset -a blockula-blackjack-dealer --stage DEALER_KEY_JSON STORE_SECRET_JSON
```

- **Secrets.** `DEALER_KEY_JSON` and `STORE_SECRET_JSON` are read on a boot where their file is missing, written to `/data` (mode 0600, owned by `node`), and unset before anything else runs: the dealer's environment never holds one. A file that exists is never overwritten from a secret. Nothing secret goes in `fly.toml`.
- **Stop and start.** `fly machine stop -a blockula-blackjack-dealer` (the dealer gets SIGTERM, lets what it sent come back and exits 0) and `fly machine start -a blockula-blackjack-dealer`. `/data/ready` stays, so a start or a restart deals at once. To hold the machine at the gate at its next start, remove the file before you stop it (`fly ssh console -a blockula-blackjack-dealer -C "rm /data/ready"`).
- **Logs.** `fly logs -a blockula-blackjack-dealer`: the daemon's JSON lines ("The log", above).
- **One machine.** Always deploy with `--ha=false` and never `fly scale count` past 1: the volume and the store's lock belong to one dealer.
- **Other settings** (`BJ_DEALER_LOW_WATER`, `BJ_DEALER_POLL_MS`, `BJ_DEALER_DRY_RUN`, …) go in `fly.toml`'s `[env]`. The paths are fixed to `/data` by the entrypoint.

## Tests

```bash
npm test -w @blockula/blackjack-dealer          # offline: the store, the configuration, the dealer against a table in memory
TEST=live scripts/blackjack-e2e.sh              # the daemon as a child process on a local chain: 30 hands, SIGKILL mid-hand, a seed that never comes
HANDS=60 scripts/blackjack-e2e.sh               # the soak on 4 local validators (1,000 hands by default)
```
