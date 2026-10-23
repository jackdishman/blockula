# Examples

Two scripts that use [`@blockula/sdk`](../sdk-ts/README.md) the way [docs/INTEGRATION.md](../docs/INTEGRATION.md) says to.

| Script | What it shows |
|---|---|
| [`receive-payments.mjs`](receive-payments.mjs) | Watch an address for payments with `watchPayments`, match them to orders by memo, credit each once by its tx id, and resume after a restart from a stored height |
| [`pay-from-a-script.mjs`](pay-from-a-script.mjs) | Pay an address: `quote` the full cost (the fee, and the 0.1 FANG burned when the payment opens an account), sign, store the signed tx before sending it, wait for it, and rebroadcast the same bytes if anything is unclear |

## Run them against `blockula dev`

From the repository's root, with the SDK built:

```bash
npm ci && npm run build -w @blockula/sdk
blockula dev --reset                 # a local chain with a faucet, at http://localhost:7070
```

In a second terminal, pick an address to be paid (any new key's) and watch it for order `order-42`, owed 1.5 FANG:

```bash
SHOP=$(node -e 'import("@blockula/sdk").then(({ Wallet }) => console.log(Wallet.generate().address))')
node examples/receive-payments.mjs "$SHOP" --network local --expect order-42=1.5
```

In a third, pay it from a new key the faucet funds:

```bash
node examples/pay-from-a-script.mjs "$SHOP" 1.5 --memo order-42 --network local --faucet
```

The payer prints the quote (1.5 FANG, the fee, and the 0.1 FANG opening burn, since the shop's address has no account yet), then the receipt. The watcher prints `✓ 1.5 FANG for "order-42" … paid in full` and exits.

Then try the rest:

- **Rebroadcast.** `node examples/pay-from-a-script.mjs --rebroadcast <tx id> --network local` posts the stored bytes again. The node answers with the same tx, and nothing is paid twice. A node that no longer holds the tx (it pruned the block) refuses it as "bad nonce" even when it ran, so on a refusal the script reads the account's nonce: once used, the tx can never run again and may already have paid, so don't sign it again until you know which tx used it.
- **Many payments at once.** The script pays one per run: `wallet.build()` doesn't take the nonce it signs with. A service paying several at once from one key uses `wallet.send(to, amount, { onSigned })`, which stores each signed tx before it leaves, in the wallet's nonce queue.
- **Resume.** Run the watcher again with the same address: it starts from the block after the last one it read (kept in `payments-<…>.json`) and credits nothing twice.
- **Your own key.** `blockula wallet new` writes a key file; pass it with `--key "$BLOCKULA_HOME/wallet.json"` instead of `--faucet`.

## Other networks

`--network testnet` uses the public testnet's RPC and pins its chain; add `--rpc http://localhost:7171` to go through your own node (`blockula join`). The testnet's faucet gives 100 test FANG an hour per address.

On a chain of your own ([docs/LAUNCH.md](../docs/LAUNCH.md)), pass one of its nodes with `--rpc`.
