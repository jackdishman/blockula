# Blockula Python SDK

Minimal synchronous Python 3.10+ client for Blockula. Install from this checkout:
`python -m pip install ./sdk-py`. Native ML-DSA-65 comes from
[PyCryptodome](https://www.pycryptodome.org/src/signature/mldsa); BLAKE3 hashes
and bech32m addresses match the Rust/TypeScript implementations.

## Ten-line quickstart

Start a disposable local node with `blockula dev`, then:

```python
from blockula import Client, Wallet
client = Client("http://localhost:7070")
alice = Wallet.generate(client)
bob = Wallet.generate(client)
alice.faucet()
bob.faucet()
bob.register_name("python-example")
receipt = alice.send("python-example.boo", "1.5")
print(receipt["summary"])
print(client.account(bob.address))
```

Reads: `status()`, `account(address_or_name)`, `name(name)`, `tx(id)`,
`blocks(limit=20, before=None)`, and `fees()`. They return the node's JSON.
`Wallet.from_seed(bytes_or_hex, client)` derives the same key/address as the
TS SDK. The SDK never saves seeds or reads an existing wallet file.

Amounts are **decimal strings in FANG**, or **integer drops** (1 FANG =
1,000,000,000 drops). Floats and bools are refused. `send()` resolves an active
name and signs its owner's address, never a guessed or unchecked raw string.
The resolved address is one RPC's word, not an independently verified proof.

The client pins the first observed chain id, genesis hash and address prefix
for its lifetime. Supply `chain_id=`, `genesis_hash=` and `prefix=` to pin them
before the first request. Reads support mainnet, but this minimal wallet
deliberately refuses **online mainnet signing** until it has the witness/clock
checks of the TS SDK. Only `transfer` and `register_name` can be encoded/signed;
other action types raise `ValueError`. This is not a full TS SDK port.

Signing uses hedged ML-DSA by default. `sign(tx, deterministic=True)` is for
reproducible fixtures; both signatures verify identically. `signing_bytes(tx)`
is the canonical BLOCKULA_TX_V3 encoding, and `tx_id(tx)` is BLAKE3 of those
bytes plus the signature (not the public key).

## Submission outcomes

`send()`/`register_name()` submit **once** and wait for finalization. `wait=False`
returns the accepted `{id}` without claiming finality. `TxFailedError` includes
the receipt (a failed executed tx can still charge a fee). `TxPendingError`
includes `tx_id`: query it or `wait_for_tx(id)`; **do not send again blindly**.
An interrupted/timeout submission can have reached the node. Different Wallet
instances sharing a key, concurrent callers using `wait=False`, or other
applications using the same nonce must coordinate their outstanding txs.

## Verification

```sh
python -m pip install ./sdk-py
python -m unittest discover -s sdk-py/tests -v
BLOCKULA_RPC=http://127.0.0.1:7070 python sdk-py/tests/e2e.py
# Or launch and stop a disposable dev node automatically:
BIN=node/target/release/blockula bash sdk-py/tests/local-e2e.sh
```

The offline tests load the existing Rust-generated `sdk-ts/test/tx-vectors.json`:
all vectors for the supported transfer/name actions must match byte-for-byte,
and every unsupported action must be refused. Additional committed fixtures
from the TS SDK compare seeded public keys, key ids, boo/tboo addresses,
deterministic signatures and tx ids. Regenerate them with
`npm run build -w @blockula/sdk` then
`node sdk-py/tests/generate-vectors.mjs > sdk-py/tests/wallet-vectors.json`.
CI runs the same tests plus send/name e2e on a fresh loopback dev chain.
