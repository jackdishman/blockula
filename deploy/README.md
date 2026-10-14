# Deploying the Blockula testnet (Fly.io)

`deploy/launch.sh` creates:

| App | What | Public address |
|---|---|---|
| `blockula-v0` … `blockula-v3` | One validator each, in iad / sjc / fra / ams | Its own dedicated IPv4, **p2p `:30300` only**. HTTP stays private |
| `blockula-testnet` | Public full node: API, explorer, sync source for joiners | `https://blockula-testnet.fly.dev` (Fly handles HTTPS) |

The public node forwards txs and faucet requests to validators over Fly's private, WireGuard-encrypted network (`http://blockula-vN.internal:7070`), authenticated with a shared forwarder secret.

**Rough cost:** ~$25/month. Each validator is one `shared-cpu-1x` 512 MB machine + a $2/mo dedicated IPv4 + a 1 GB volume, plus the public node. Check [fly.io/pricing](https://fly.io/pricing) for current rates. Validators need a dedicated IPv4 because Fly's free shared IPv4 only carries HTTP, and p2p is raw TCP.

## Launch

```bash
brew install flyctl jq
fly auth login
deploy/launch.sh          # idempotent: re-run to upgrade the node everywhere
```

Options (env vars):

| Variable | Default |
|---|---|
| `ORG` | `personal` |
| `PREFIX` | `blockula` |
| `REGIONS` | `"iad sjc fra ams"` (one validator per region) |
| `RPC_REGION` | `iad` |

## Keys and secrets

- **Season 0 admin key:** `~/.config/blockula/admin.json` approves and removes validators. It exists only on your laptop. Back it up.
- **Validator keys:** generated once in `~/.config/blockula/testnet/v*/validator.key` (dir mode 700). Each one is uploaded only to its own app, as the Fly secret `VALIDATOR_KEY_B64` (through stdin, never argv). On first boot it's copied onto that app's volume, and then **launch.sh deletes the secret from Fly**. A cloned, scaled or replacement machine with an empty volume refuses to start instead of double-signing. The local copies are your backup.
- **Containers** run as an unprivileged `blockula` user.
- **Genesis:** `~/.config/blockula/testnet/genesis.json`. It's public, and identical on every node.

## Day 2

| Task | Command |
|---|---|
| Upgrade all nodes | `deploy/launch.sh` |
| Start a brand-new testnet (wipes chain data) | `RESET=1 deploy/launch.sh` |
| Logs | `fly logs -a blockula-v0` |
| Status | `curl https://blockula-testnet.fly.dev/api/status` |
| Give validators a GitHub token (contribution claims, link checks) | `pbpaste \| deploy/set-github-token.sh` (fine-grained, public-read-only token; restarts apps one at a time) |
| Approve a validator | `node/target/release/blockula validator approve <boo1…> --key ~/.config/blockula/admin.json --rpc https://blockula-testnet.fly.dev` |
| Add one of *our* validators later | `fly apps create`, a volume and a dedicated IPv4, then the normal "Become a validator" flow (register + approve). No new genesis needed |

## Caveats
- Upgrades redeploy validators in parallel, so block production pauses briefly.
- Each validator app must stay at exactly **one machine** (`--ha=false`). Never `fly scale count`, clone a validator machine, or restore a volume snapshot: two copies of a validator key double-sign. The entrypoint and node guard against the common cases, but don't rely on that.
- If a validator's volume is lost, its key is gone from Fly by design. Re-seed it deliberately from your local backup (`printf 'VALIDATOR_KEY_B64=…' | fly secrets import -a blockula-vN`), then re-run launch.sh.
