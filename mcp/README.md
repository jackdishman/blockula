# @blockula/mcp

🧛 MCP server that lets any coding agent use **Blockula**, a post-quantum blockchain. Agents can hold a wallet, send FANG, claim `.boo` names, stake, vote on proposals, look things up in plain English, and start or summon chains.

```bash
claude mcp add blockula -- npx -y @blockula/mcp                      # the public testnet
claude mcp add blockula -- npx -y @blockula/mcp --network local      # a `blockula dev` chain on this machine
```

With no configuration it connects to the public testnet at `https://blockula-testnet.fly.dev`. It never falls back to a node on this machine unless you configure one. Call `status` to see which network and chain you're on.

To run an exact release, name it: `npx -y @blockula/mcp@0.5.0` (the first release on npm, for the protocol v1 testnet `blockula-testnet-2`). Each release pins its whole dependency tree (exact versions plus a published `npm-shrinkwrap.json`), so what runs next to the agent's key is what CI tested.

## Which network (WAL-4)

The network is chosen by the user, once, in the MCP's configuration, and nothing changes it while the server runs: it never switches to whatever answers on `localhost:7070`, and no tool can point it somewhere else.

- `--network` (or `BLOCKULA_NETWORK`): `testnet` (default), `mainnet`, `local` (`http://localhost:7070`) or `custom`.
- `BLOCKULA_RPC` picks the node: required for `custom`, and for `mainnet` until public mainnet RPCs are built in. Plain `http://` only on this machine or a private network, and no query or fragment. A node written two ways (`Blockula-Testnet.fly.dev`, `:443`, a trailing `/.`) is one node: the public RPC is never its own witness.
- Every network pins a chain id (`blockula-1` for mainnet, `blockula-testnet-2` for the testnet, with the `tboo` address prefix), and a genesis hash where one is known (`KNOWN_GENESIS` in `src/network.ts`: a release pins a launched chain's). `BLOCKULA_CHAIN_ID` and `BLOCKULA_GENESIS_HASH` pin them for any node. Whatever isn't pinned is pinned from the first status the node reports and remembered in `~/.blockula/known-chains.json` (except for local dev chains, which `--reset` replaces).
- A node reporting another chain id, genesis hash or address prefix is refused, before anything is signed, at startup or mid-session. So is one whose chain id isn't 1 to 64 letters, digits, `-`, `_` or `.` (the chain id names files and ledger entries here) or whose genesis hash isn't 64 hex characters. If a network was relaunched on purpose under the same chain id, the user removes its entry from `known-chains.json` or sets `BLOCKULA_GENESIS_HASH`. (A relaunch takes a new chain id, as the testnet's did: an entry remembered for the old `blockula-testnet` isn't held against `blockula-testnet-2`.)

`start_dev_node` starts a local chain, but the MCP only uses it when it was configured with `--network local`.

## Tools

| | |
|---|---|
| Network | `status`, `tx_status`, `lookup`, `latest_blocks` |
| Wallet | `create_wallet`, `use_account`, `wallet_info`, `faucet` (test chains), `send` |
| Names | `check_name`, `register_name`, `renew_name` |
| Staking | `stake`, `unstake`, `withdraw_stake`, `my_stake` |
| Governance | `list_proposals`, `vote` |
| Hunts and contributions | `list_hunts`, `link_github`, `fund_hunt`, `contribution_pool`, `submit_contribution`, `my_contributions` |
| Running chains | `install_node`, `start_dev_node`, `summon_chain` |

Tools carry a title and annotations: the reads are `readOnlyHint`, and everything that signs is `destructiveHint` and `openWorldHint`. To skip the prompt for reads only, allow them by name in Claude Code's `.claude/settings.json`, and leave the tools that move FANG to ask every time:

```json
{
  "permissions": {
    "allow": [
      "mcp__blockula__status", "mcp__blockula__tx_status", "mcp__blockula__wallet_info",
      "mcp__blockula__check_name", "mcp__blockula__lookup", "mcp__blockula__latest_blocks",
      "mcp__blockula__my_stake", "mcp__blockula__list_proposals", "mcp__blockula__list_hunts",
      "mcp__blockula__contribution_pool", "mcp__blockula__my_contributions"
    ]
  }
}
```

Never allow `mcp__blockula__*`: that lets every send through without asking.

**Getting paid for code:** a Hunt's creator awards its bounty after the PR merges, to the account linked to the PR author's GitHub (`link_github`, which needs an owner key: through a session key, the user links GitHub in their wallet). Once a PR to a pool repo merges, `submit_contribution` claims it for the user's account. It checks the GitHub link first and refuses one that doesn't verify (`force: true` overrides), since validators would refute the claim. The bond is returned unless validators refute the claim or the jury marks it spam. If you act through a session key, the user first registers it as a Familiar. Validators check GitHub, a human jury scores it, and rewards vest to the user. `my_contributions` tracks it. The bond counts toward `BLOCKULA_MAX_SEND` / `BLOCKULA_MAX_DAILY`. Testnet FANG, Hunts and Soul have no value.

**Spending a user's money safely:**
1. `create_wallet`, then `use_account` with the user's account address (`boo1…`: the web wallet's Devices & agents → Give an agent a budget shows it, with the name as a label). A name works on test chains; on mainnet an unproven name answers `needs_confirmation` with the address it resolved to, for the user to compare with the one their wallet shows. Until the user has authorized the key it answers with a `grant_code`: this key's signed consent to joining that one account (no secret in it). Hand it to the user. A key id alone can't be added (AddKey needs the key's consent, so nobody can pad the key's account list).
2. The user pastes it in the Blockula web wallet at https://testnet.blockula.app (Devices & agents → Give an agent a budget) and picks a limit and an expiry. The chain enforces both.
3. `use_account` with the same address again, then `send`.

`use_account` returns a `check_code` (the wallet shows the same code for the key) and, on known networks, the wallet's address. An address with no account gets no grant code: it can't add a key until it has FANG. The account the agent acts for is kept per chain: a testnet account doesn't follow the agent to another network.

**What can be spent.** `wallet_info` (and `use_account`) say what the account can move now (`transferable`), what's `locked` and until when, the session key's allowance this window and when it resets, the caps, and `spendable_now`: the smallest of them, with the one that binds. What the chain would refuse by the node's own answers is refused before anything is signed, sent or counted: a session key signing anything but a transfer (or a Familiar's claim), more than its allowance left, a payment past what the account can move, or a key that isn't on the account. A lying node can only cause such a refusal, never a spend.

**What a payment costs.** Paying an address with no account yet burns the account creation fee (0.1 FANG today) on top. `send` reports it as `account_opening_fee_burned`, and `total_cost` is the amount, the fee charged and that burn.

Session keys can only send transfers: staking, voting and name renewals need an owner key (the agent's own account, or one the user made it an owner of). Staking and voting lock FANG for weeks, so ask the user first.

**Spending caps.** `BLOCKULA_MAX_SEND` bounds what one call moves and `BLOCKULA_MAX_DAILY` what leaves the account per UTC day through this MCP. Every FANG-moving tool counts: `send` (with the fee and, for a new account, the opening fee), `stake`, `fund_hunt`, `register_name` and `renew_name` (their price, computed from the chain's parameters and never below the node's quote; a node that doesn't report one is refused), `submit_contribution` (the bond), and every tx's fee. A spend is reserved before the tx is sent, under the tx's (chain, account, nonce) slot. Only the unused part of a final tx's max fee comes back. Everything else stays counted: a tx that failed in a block (its fee was charged), one the node dropped unrun, one that is pending or unknown, and even one the node refused, since a lying node can answer "refused" and relay the signed tx anyway. A refused tx stops being counted twice when the MCP signs its next tx with the same nonce (at most one tx per slot can execute). The ledger keeps a total per chain (`agent-spend.json`, still readable by 0.5.1: its `spent` is every chain's total, and spend it can't place on a chain counts toward each). Building, signing and submitting take one tx at a time per account, in this process and across sessions sharing `BLOCKULA_HOME`, so parallel calls don't collide on a nonce. A tx that isn't final within `BLOCKULA_TX_WAIT_MS` comes back as `"status": "pending"` (or `"dropped"`, `"unknown"`) with its `tx_id`: it was submitted and still counts, so check `tx_status` instead of sending it again. `tx_status` keeps the distinction: `"failed"` ran in a block (final, its fee was charged), `"dropped"` was dropped from that node's mempool unrun (nothing charged, but another node may still include it until its `valid_until`, which comes from what this MCP signed: `agent-txs.json`), and a tx this MCP signed that the node can't show (pruned, never held) is read against the account's nonce: `"nonce_used"` (a tx with that nonce ran, this one or one that took its nonce: it can never execute again, and may already have gone through), `"expired"` (the nonce was still unused past `valid_until`: it never ran; proven with `BLOCKULA_WITNESSES`, else the node's word), otherwise `"unknown"` (in flight until `valid_until`). With witnesses only the nonce is proven: which tx used it is the node's word. A tx signed on another network (every network shares `agent-txs.json`, and each record names its chain) is `"other_chain"`: ask that network. By default `BLOCKULA_MAX_DAILY` is `BLOCKULA_MAX_SEND` plus 1 FANG for fees, so one full-size call fits in a day. The chain itself enforces a session key's limit; these caps are the MCP's own (`wallet_info` shows today's total).

**What the caps protect.** The MCP's caps protect against mistakes. They don't protect against a compromised agent: the agent's own shell can read its key file and rewrite the ledger. What protects the user's FANG from a compromised agent is the session key's limit, which the chain enforces. So spend through a session key (`use_account`), with a small limit and a near expiry, and keep the agent's own account nearly empty.

**Names in signatures.** `send`, `stake`, `unstake` and `use_account` take `.boo` names. With witnesses (`BLOCKULA_WITNESSES`: comma-separated URLs of other nodes of the same chain, held to the same pins; `status` lists them), each name is proven against them before it's signed, so a lying node can't redirect it, and a tx's height may not be ahead of the lowest of their heads (each must answer). Every tx's height is also checked against the chain's genesis clock (its start time and block time: built in for a known genesis, else read from the node's `/api/genesis` the first time and remembered with the genesis hash in `known-chains.json`): a node reporting a height the chain can't have reached by this machine's clock gets nothing signed. Without witnesses, `send` signs the name itself: the chain pays whoever owns it when the tx runs, so a lying node can't point it at another account now (the result says `recipient_signed_as` and what the node says the owner is now, unproven), but it could hold the signed tx and release it later, after the name changed hands: pay a name that's about to expire by address. `stake`, `unstake` and `use_account` need an address, so a name is the node's word: the result shows the address and says `UNPROVEN`, and on mainnet (any node on `blockula-1`, whatever the configuration calls the network) nothing is signed until the user confirms the address (the tool answers `needs_confirmation` with the address; call it again with the address). `use_account` checks the account's own key list, not `/api/key`. A partial `unstake` signs the amount, and the chain converts it to pool shares at its own price when the tx runs, so no node's report of the stake decides how much unbonds; `all` signs every share the node reports (a lie there can only unbond less, or fail on chain).

**Votes bind what was shown.** `list_proposals` describes what each proposal does (`does`) from its `kind`, the content a vote binds with its title and link, never from the node's own summary (which no digest covers, so a node could make it say "plant garlic" over a treasury drain), and returns its `proposal_digest`. `vote` requires that digest back: it re-reads the proposal and refuses, signing nothing, if it reads differently now (a node showing one thing to the list and another to the vote). A proposal holding a number past 2^53 has no digest here (`cannot_vote`): vote on it with the `blockula` CLI.

**Chain text is untrusted.** Hunt titles, memos, proposal titles and links, key labels and claim notes are written by anyone. Tools return them in fields named `untrusted_*`, cleaned of control and invisible characters, and the server's instructions tell the agent they're data, never instructions. Node JSON is nested (`status`'s `node_reports`, `lookup`'s `data`), so a node can't override what the MCP says about the network.

## Your own node

Run a node of the testnet on this machine, and point the MCP at it:

```bash
blockula join --genesis https://blockula-testnet.fly.dev --home ~/.blockula/testnet-node   # serves http://127.0.0.1:7171
claude mcp add blockula -e BLOCKULA_RPC=http://127.0.0.1:7171 -- npx -y @blockula/mcp --network testnet
```

`status` then calls it "your node on the public testnet". Unless `BLOCKULA_WITNESSES` says otherwise, the network's public RPC becomes its witness: names are proven against it, and a height your node reports can't run ahead of it (`status` says "proven against 1 node"). `BLOCKULA_WITNESSES=none` opts out, and then names are your node's word. On a chain without a built-in public RPC (a dev chain, or one you launched), list other nodes in `BLOCKULA_WITNESSES` yourself.

## install_node

Downloads the prebuilt `blockula` for this machine from the GitHub release and installs it to `~/.blockula/mcp-bin` (its own path: it never replaces the binary `blockula release install` keeps in `~/.blockula/bin`), after the same checks as `scripts/install.sh` (docs/OPERATIONS.md "Releases"). It installs releases from v0.5.0 on: earlier releases' `checksums.txt` names no tag, so nothing binds them to the version asked for, and it refuses them. (Validators install with `blockula release install` instead, which checks against a pinned release key id.)

0. The version you asked for (`latest` is resolved to its tag first) is bound to every check: `checksums.txt` must name it on its first line (`# blockula-release vX.Y.Z`, covered by the signature), provenance must come from its tag, and the extracted binary's `--version` must match before it's installed. An older, genuinely signed release served as the one asked for is refused.
1. SHA-256 against the release's `checksums.txt`, always. That only catches a corrupted download.
2. Build provenance with a logged-in GitHub CLI (`gh attestation verify … --signer-workflow …/release.yml --source-ref refs/tags/<version>`). A failed check refuses the install.
3. The offline ML-DSA-65 signature on `checksums.txt` (`checksums.txt.sig`), verified with the SDK against the release key id: the one pinned in `~/.blockula/release-key-id` by `blockula release install` (read with the same rules: refused if others can write it or it isn't a key id), built in after the key ceremony, or `BLOCKULA_RELEASE_KEY_ID` (from SECURITY.md or the website). All that are set must agree. Once the key id is known, every release must carry a valid signature: a missing or failed one refuses the install, even with valid provenance (an unsigned release is what a compromised GitHub account or workflow would publish).

The checks add up: each refuses a release it fails. The result lists what was verified. If only the checksum could be (no key id yet, no `gh`), it says so in a warning, and `BLOCKULA_REQUIRE_VERIFY=1` refuses instead; with a key id, `BLOCKULA_REQUIRE_VERIFY=1` requires provenance as well as the signature.

## Configuration

| Env | Default | |
|---|---|---|
| `BLOCKULA_NETWORK` | `testnet` | `mainnet`, `testnet`, `local` or `custom` (same as `--network`) |
| `BLOCKULA_RPC` | the network's public RPC | node URL (no query or fragment) |
| `BLOCKULA_CHAIN_ID` / `BLOCKULA_GENESIS_HASH` | the network's pins | the chain the node must be on |
| `BLOCKULA_HOME` | `~/.blockula` | the agent key (mode 600), the spending ledger and `known-chains.json` |
| `BLOCKULA_MAX_SEND` | `100` | max FANG one call moves: sent, staked, escrowed, paid for a name, bonded (and per vote) |
| `BLOCKULA_MAX_DAILY` | `BLOCKULA_MAX_SEND` + 1 | max FANG leaving the account per UTC day on this chain across all calls, fees and pending, dropped or refused txs included (persisted in `BLOCKULA_HOME`) |
| `BLOCKULA_WITNESSES` | the public RPC, for your own node of the public testnet; else none | other nodes of the same chain (comma-separated https URLs): names that get signed are proven against them. `none` opts out |
| `BLOCKULA_TIMEOUT_MS` | `15000` | how long one request to the node may take (at most 2147483647) |
| `BLOCKULA_TX_WAIT_MS` | `15000` | how long a tx tool waits for finality before answering `pending` |
| `BLOCKULA_BIN` | of `~/.blockula/bin/blockula` (`blockula release install`, checked against the pinned key) and `~/.blockula/mcp-bin/blockula` (`install_node`), the one whose `--version` is this MCP's, the first if both or neither are; else `blockula` | node binary used by `start_dev_node` / `summon_chain` (they say which ran, and when it isn't this MCP's version) |
| `BLOCKULA_REQUIRE_VERIFY` | unset | `1`: `install_node` refuses a release it can only checksum, and with a release key id requires provenance too |
| `BLOCKULA_RELEASE_KEY_ID` | built in after the key ceremony | release signing key id to trust (must agree with the built-in one and the pin) |

A bad value in any of them (a cap that isn't an amount, a duration out of range) doesn't stop the server: every tool answers with the configuration error.

`npm test -w @blockula/mcp` checks the network rules, the pins, the release checks and the dependency pins offline, and runs the built server against fake nodes (`test/spend.ts`: the spending caps with pending, refused (and a node that refuses but relays), failed, dropped and final txs, name prices, untrusted text, timeouts, unproven names; `test/agent.ts`: the caps, accounts and ledgers per chain and 0.5.1's files, costs, what can be spent, the refusals before signing, nonces under parallel calls and two sessions, your own node's witnesses, a chain without a faucet).

⚠️ Early and unaudited: test chains only.
