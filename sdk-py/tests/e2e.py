"""Run against an explicitly selected, disposable local `blockula dev` node."""
import os
import secrets
from urllib.parse import urlsplit
from blockula import Client, Wallet

rpc = os.environ.get("BLOCKULA_RPC", "http://127.0.0.1:7070")
if urlsplit(rpc).hostname not in ("localhost", "127.0.0.1", "::1"):
    raise RuntimeError("e2e sends test funds: only loopback RPCs are permitted")
client = Client(rpc)
status = client.status()
if status["chain_id"] == "blockula-1":
    raise RuntimeError("refusing mainnet")
alice, bob = Wallet.generate(client), Wallet.generate(client)
alice.faucet()
bob.faucet()
name = "py-" + secrets.token_hex(6)
registered = bob.register_name(name)
assert registered["status"] == "finalized", registered
assert client.name(name)["owner"]["address"] == bob.address
before = int(client.account(bob.address)["balance"])
receipt = alice.send(name + ".boo", "1.000000001", memo="Python SDK e2e")
assert receipt["status"] == "finalized", receipt
assert client.tx(receipt["id"])["status"] == "finalized"
assert int(client.account(bob.address)["balance"]) - before == 1_000_000_001
assert client.blocks(2)
print("PASS: faucet, name registration/resolution, signed transfer, status/account/name/tx/blocks")
