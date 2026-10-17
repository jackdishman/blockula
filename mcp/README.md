# @blockula/mcp

🧛 MCP server that lets any coding agent use **Blockula**, a post-quantum blockchain. Agents can hold a wallet, send FANG, claim `.boo` names, stake, vote on proposals, look things up in plain English, and start or summon chains.

```bash
claude mcp add blockula -- npx -y @blockula/mcp                      # the public testnet
claude mcp add blockula -- npx -y @blockula/mcp --network local      # a `blockula dev` chain on this machine
```

With no configuration it connects to the public testnet at `https://blockula-testnet.fly.dev`. Call `status` to see which network and chain you're on.

To run an exact release, name it: `npx -y @blockula/mcp@0.4.0`. Each release pins its whole dependency tree (exact versions plus a published `npm-shrinkwrap.json`), so what runs next to the agent's key is what CI tested.

## Which network (WAL-4)

The network is chosen by the user, once, in the MCP's configuration, and nothing changes it while the server runs: it never switches to whatever answers on `localhost:7070`, and no tool can point it somewhere else.

- `--network` (or `BLOCKULA_NETWORK`): `testnet` (default), `mainnet`, `local` (`http://localhost:7070`) or `custom`.
- `BLOCKULA_RPC` picks the node: required for `custom`, and for `mainnet` until public mainnet RPCs are built in. Plain `http://` only on this machine or a private network.
- Every network pins a chain id (`blockula-1` for mainnet, `blockula-testnet` for the testnet), and a genesis hash where one is known. `BLOCKULA_CHAIN_ID` and `BLOCKULA_GENESIS_HASH` pin them for any node. Whatever isn't pinned is pinned from the first status the node reports and remembered in `~/.blockula/known-chains.json` (except for local dev chains, which `--reset` replaces).
- A node reporting another chain id, genesis hash or address prefix is refused, before anything is signed, at startup or mid-session. If a network was relaunched on purpose, the user removes its entry from `known-chains.json` or sets `BLOCKULA_GENESIS_HASH`.

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

**Getting paid for code:** once a PR to a pool repo merges, `submit_contribution` claims it for the user's account (the PR's author must be their linked GitHub handle). If you act through a session key, the user first registers it as a Familiar. Validators check GitHub, a human jury scores it, and rewards vest to the user. `my_contributions` tracks it. The bond counts toward `BLOCKULA_MAX_SEND` / `BLOCKULA_MAX_DAILY`. Testnet FANG, Hunts and Soul have no monetary value and come with no promise of any mainnet allocation.

**Spending a user's money safely:**
1. `create_wallet`, then `use_account("theirname.boo")`. Until the user has authorized the key it answers with a `grant_code`: this key's signed consent to joining that one account (no secret in it). Hand it to the user. A key id alone can't be added (AddKey needs the key's consent, so nobody can pad the key's account list).
2. The user pastes it in the Blockula web wallet at https://blockula-wallet.vercel.app (Devices & agents → Give an agent a budget) and picks a limit and an expiry. The chain enforces both.
3. `use_account("theirname.boo")` again, then `send`.

Session keys can only send transfers: staking, voting and name renewals need an owner key (the agent's own account, or one the user made it an owner of). Staking and voting lock FANG for weeks, so ask the user first.

**Spending caps.** `BLOCKULA_MAX_SEND` bounds what one call moves and `BLOCKULA_MAX_DAILY` what leaves the account per UTC day through this MCP. Every FANG-moving tool counts: `send` (with the fee and, for a new account, the opening fee), `stake`, `fund_hunt`, `register_name` and `renew_name` (their price, computed from the chain's parameters and never below the node's quote; a node that doesn't report one is refused), `submit_contribution` (the bond), and every tx's fee. A spend is reserved before the tx is sent, under the tx's (account, nonce) slot. Only the unused part of a final tx's max fee comes back. Everything else stays counted: a tx that failed in a block (its fee was charged), one the node dropped unrun, one that is pending or unknown, and even one the node refused, since a lying node can answer "refused" and relay the signed tx anyway. A refused tx stops being counted twice when the MCP signs its next tx with the same nonce (at most one tx per slot can execute). A tx that isn't final within `BLOCKULA_TX_WAIT_MS` comes back as `"status": "pending"` (or `"dropped"`, `"unknown"`) with its `tx_id`: it was submitted and still counts, so check `tx_status` instead of sending it again. By default `BLOCKULA_MAX_DAILY` is `BLOCKULA_MAX_SEND` plus 1 FANG for fees, so one full-size call fits in a day. The chain itself enforces a session key's limit; these caps are the MCP's own (`wallet_info` shows today's total).

**Names are resolved for signatures.** `send`, `stake`, `unstake` and `use_account` take `.boo` names. With `BLOCKULA_WITNESSES` (comma-separated URLs of other nodes of the same chain, held to the same pins), each name is proven against them before it's signed, so a lying node can't redirect it. Without witnesses a name is the node's word: the result shows the exact address signed and says `UNPROVEN`, and on mainnet nothing is signed until the user confirms the address (the tool answers `needs_confirmation` with the address; call it again with the address). `use_account` checks the account's own key list, not `/api/key`. A partial `unstake` the node values at or below the amount asked is refused (pass `all` instead), so a node reporting a tiny value can't turn it into unbonding everything.

**Chain text is untrusted.** Hunt titles, memos, proposal titles and links, key labels and claim notes are written by anyone. Tools return them in fields named `untrusted_*`, cleaned of control and invisible characters, and the server's instructions tell the agent they're data, never instructions. Node JSON is nested (`status`'s `node_reports`, `lookup`'s `data`), so a node can't override what the MCP says about the network.

## install_node

Downloads the prebuilt `blockula` for this machine from the GitHub release and installs it to `~/.blockula/bin`, after the same checks as `scripts/install.sh` (docs/OPERATIONS.md "Releases"):

1. SHA-256 against the release's `checksums.txt`, always. That only catches a corrupted download.
2. Build provenance with a logged-in GitHub CLI (`gh attestation verify … --signer-workflow …/release.yml`). A failed check refuses the install.
3. The offline ML-DSA-65 signature on `checksums.txt` (`checksums.txt.sig`), verified with the SDK against the release key id (built in after the key ceremony; `BLOCKULA_RELEASE_KEY_ID` until then). Once the key id is known, every release must carry a valid signature: a missing or failed one refuses the install, even with valid provenance (an unsigned release is what a compromised GitHub account or workflow would publish).

The checks add up: each refuses a release it fails. The result lists what was verified. If only the checksum could be (no key id yet, no `gh`), it says so in a warning, and `BLOCKULA_REQUIRE_VERIFY=1` refuses instead; with a key id, `BLOCKULA_REQUIRE_VERIFY=1` requires provenance as well as the signature.

## Configuration

| Env | Default | |
|---|---|---|
| `BLOCKULA_NETWORK` | `testnet` | `mainnet`, `testnet`, `local` or `custom` (same as `--network`) |
| `BLOCKULA_RPC` | the network's public RPC | node URL |
| `BLOCKULA_CHAIN_ID` / `BLOCKULA_GENESIS_HASH` | the network's pins | the chain the node must be on |
| `BLOCKULA_HOME` | `~/.blockula` | the agent key (mode 600), the spending ledger and `known-chains.json` |
| `BLOCKULA_MAX_SEND` | `100` | max FANG one call moves: sent, staked, escrowed, paid for a name, bonded (and per vote) |
| `BLOCKULA_MAX_DAILY` | `BLOCKULA_MAX_SEND` + 1 | max FANG leaving the account per UTC day across all calls, fees and pending, dropped or refused txs included (persisted in `BLOCKULA_HOME`) |
| `BLOCKULA_WITNESSES` | none | other nodes of the same chain (comma-separated https URLs): names that get signed are proven against them |
| `BLOCKULA_TIMEOUT_MS` | `15000` | how long one request to the node may take (at most 2147483647) |
| `BLOCKULA_TX_WAIT_MS` | `15000` | how long a tx tool waits for finality before answering `pending` |
| `BLOCKULA_BIN` | `~/.blockula/bin/blockula`, else `blockula` | node binary used by `start_dev_node` / `summon_chain` |
| `BLOCKULA_REQUIRE_VERIFY` | unset | `1`: `install_node` refuses a release it can only checksum, and with a release key id requires provenance too |
| `BLOCKULA_RELEASE_KEY_ID` | built in after the key ceremony | release signing key id to trust |

`npm test -w @blockula/mcp` checks the network rules, the pins, the release checks and the dependency pins offline, and runs the built server against a fake node (`test/spend.ts`: the spending caps with pending, refused (and a node that refuses but relays), failed, dropped and final txs, name prices, untrusted text, timeouts, unproven names).

⚠️ Early and unaudited: test chains only.
