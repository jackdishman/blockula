# @blockula/mcp

🧛 MCP server that lets any coding agent use **Blockula**, a post-quantum blockchain. Agents can hold a wallet, send FANG, claim `.boo` names, look things up in plain English, and start or summon chains.

```bash
claude mcp add blockula -- npx -y @blockula/mcp
```

It works with zero setup: if no local node is running, it connects to the public testnet at `https://blockula-testnet.fly.dev`. Call `status` to see which node you're on.

**Tools:** `status`, `tx_status`, `install_node`, `list_hunts`, `link_github`, `fund_hunt`, `contribution_pool`, `submit_contribution`, `my_contributions`, `start_dev_node`, `summon_chain`, `create_wallet`, `use_account`, `wallet_info`, `faucet`, `send`, `check_name`, `register_name`, `lookup`, `latest_blocks`.

**Getting paid for code:** once a PR to a pool repo merges, `submit_contribution` claims it for the user's account (the PR's author must be their linked GitHub handle). If you act through a session key, the user first registers it as a Familiar. Validators check GitHub, a human jury scores it, and rewards vest to the user. `my_contributions` tracks it. The bond counts toward `BLOCKULA_MAX_SEND` / `BLOCKULA_MAX_DAILY`.

**Spending a user's money safely:**
1. `create_wallet` gives you a `key_id`. Hand it to the user.
2. The user grants it a budget in the Blockula web wallet at https://blockula-wallet.vercel.app (Devices & agents → Give an agent a budget). The chain enforces the limit and expiry.
3. `use_account("theirname.boo")`, then `send`.

| Env | Default | |
|---|---|---|
| `BLOCKULA_RPC` | local node if running, else the public testnet | node URL |
| `BLOCKULA_HOME` | `~/.blockula` | where the agent key is stored (mode 600) |
| `BLOCKULA_MAX_SEND` | `100` | max FANG per `send` call |
| `BLOCKULA_MAX_DAILY` | `100` | max FANG sent per UTC day across all calls (persisted in `BLOCKULA_HOME`) |
| `BLOCKULA_BIN` | `blockula` | node binary used by `start_dev_node` / `summon_chain` |

`start_dev_node` and `summon_chain` need the `blockula` node binary (build it from the Blockula repo with `cargo build --release`). Every other tool works against the public testnet without it.

⚠️ Early and unaudited: test chains only.
