# @blockula/mcp

🧛 MCP server that lets any coding agent use **Blockula**, a post-quantum blockchain. Agents can hold a wallet, send FANG, claim `.boo` names, stake, vote on proposals, look things up in plain English, and start or summon chains.

```bash
claude mcp add blockula -- npx -y @blockula/mcp                      # the public testnet
claude mcp add blockula -- npx -y @blockula/mcp --network local      # a `blockula dev` chain on this machine
```

With no configuration it connects to the public testnet at `https://blockula-testnet.fly.dev`. Call `status` to see which network and chain you're on.

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
1. `create_wallet` gives you a `key_id`. Hand it to the user.
2. The user grants it a budget in the Blockula web wallet at https://blockula-wallet.vercel.app (Devices & agents → Give an agent a budget). The chain enforces the limit and expiry.
3. `use_account("theirname.boo")`, then `send`.

Session keys can only send transfers: staking, voting and name renewals need an owner key (the agent's own account, or one the user made it an owner of). Staking and voting lock FANG for weeks, so ask the user first.

## install_node

Downloads the prebuilt `blockula` for this machine from the GitHub release and installs it to `~/.blockula/bin`, after the same checks as `scripts/install.sh` (docs/OPERATIONS.md "Releases"):

1. SHA-256 against the release's `checksums.txt`, always. That only catches a corrupted download.
2. Build provenance with a logged-in GitHub CLI (`gh attestation verify … --signer-workflow …/release.yml`). A failed check refuses the install.
3. The offline ML-DSA-65 signature on `checksums.txt` (`checksums.txt.sig`), verified with the SDK against the release key id (built in after the key ceremony; `BLOCKULA_RELEASE_KEY_ID` until then). A failed check refuses the install.

The result lists what was verified. If only the checksum could be, it says so in a warning, and `BLOCKULA_REQUIRE_VERIFY=1` refuses instead.

## Configuration

| Env | Default | |
|---|---|---|
| `BLOCKULA_NETWORK` | `testnet` | `mainnet`, `testnet`, `local` or `custom` (same as `--network`) |
| `BLOCKULA_RPC` | the network's public RPC | node URL |
| `BLOCKULA_CHAIN_ID` / `BLOCKULA_GENESIS_HASH` | the network's pins | the chain the node must be on |
| `BLOCKULA_HOME` | `~/.blockula` | the agent key (mode 600), the spending ledger and `known-chains.json` |
| `BLOCKULA_MAX_SEND` | `100` | max FANG per `send` / `stake` / `fund_hunt` call (and per vote) |
| `BLOCKULA_MAX_DAILY` | `100` | max FANG sent per UTC day across all calls (persisted in `BLOCKULA_HOME`) |
| `BLOCKULA_BIN` | `~/.blockula/bin/blockula`, else `blockula` | node binary used by `start_dev_node` / `summon_chain` |
| `BLOCKULA_REQUIRE_VERIFY` | unset | `1`: `install_node` refuses a release it can only checksum |
| `BLOCKULA_RELEASE_KEY_ID` | built in after the key ceremony | release signing key id to trust |

`npm test -w @blockula/mcp` checks the network rules, the pins and the release checks offline, and runs the built server against a fake node.

⚠️ Early and unaudited: test chains only.
