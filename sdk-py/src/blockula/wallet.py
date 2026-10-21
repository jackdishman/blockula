"""Seeded ML-DSA-65 wallets for local/testnet development; no seed persistence."""
from copy import deepcopy
import secrets
import threading
from blake3 import blake3
from Crypto.PublicKey import MLDSA
from Crypto.Signature import mldsa

from .client import Client, ApiError
from .encoding import (address_from_public_key, is_address, signing_bytes,
                       parse_fang, bare_name, base_fee, uint, MAX_JSON_INT)


class Wallet:
    def __init__(self, seed, client=None):
        if isinstance(seed, str):
            seed = bytes.fromhex(seed)
        if not isinstance(seed, bytes) or len(seed) != 32:
            raise ValueError("seed must be exactly 32 bytes (or hex)")
        self._key = MLDSA.construct(param_set=MLDSA.ML_DSA_65, seed=seed)
        self.public_key = bytes(self._key.pk)
        self.client = client or Client()
        self._lock = threading.Lock()

    @classmethod
    def generate(cls, client=None):
        return cls(secrets.token_bytes(32), client)

    @classmethod
    def from_seed(cls, seed, client=None):
        return cls(seed, client)

    @property
    def key_id(self):
        return blake3(self.public_key).hexdigest()

    @property
    def address(self):
        if self.client.prefix is None:
            self.client.status()
        return address_from_public_key(self.public_key, self.client.prefix)

    def sign(self, tx, *, deterministic=False):
        tx = deepcopy(tx)
        signature = mldsa.new(self._key, deterministic=deterministic).sign(signing_bytes(tx))
        return {**tx, "public_key": self.public_key.hex(), "signature": signature.hex()}

    def build(self, action, *, memo="", valid_for_blocks=600):
        status = self.client.status()
        # The minimal SDK has no witness/state-proof verification yet. Refuse mainnet
        # signing rather than resolving a name or a height on one RPC's unproven word.
        if status["chain_id"] == "blockula-1":
            raise ValueError("mainnet signing is not supported by this minimal SDK")
        height = uint(status["height"], MAX_JSON_INT)
        maximum = uint(status.get("max_tx_validity_blocks", 600), MAX_JSON_INT)
        validity = min(uint(valid_for_blocks, MAX_JSON_INT), maximum)
        if validity < 1:
            raise ValueError("transaction validity must be positive")
        try:
            account = self.client.account(self.address)
        except ApiError as error:
            if error.status != 404:
                raise
            account = {"next_nonce": 0}
        tx = {"chain_id": status["chain_id"], "genesis_hash": status["genesis_hash"],
              "from": self.address, "nonce": uint(account["next_nonce"], MAX_JSON_INT),
              "fee": "0", "tip": "0", "valid_until": height + validity,
              "action": deepcopy(action), "memo": memo}
        tx["fee"] = str(base_fee(tx, self.client.fees()["suggested"]))
        return self.sign(tx)

    def send(self, to, amount, *, memo="", wait=True):
        with self._lock:
            self.client.status()
            drops = parse_fang(amount)
            if drops <= 0:
                raise ValueError("send amount must be positive")
            if not is_address(to):
                name = self.client.name(bare_name(to))
                if name.get("status") != "active":
                    raise ValueError("recipient name is not active")
                to = name.get("owner", {}).get("address")
            if not is_address(to, self.client.prefix):
                raise ValueError("recipient is not an address on this chain")
            tx = self.build({"type": "transfer", "to": to, "amount": str(drops)}, memo=memo)
            return self.client.submit(tx, wait=wait)

    def register_name(self, name, *, wait=True):
        with self._lock:
            tx = self.build({"type": "register_name", "name": bare_name(name)})
            return self.client.submit(tx, wait=wait)

    def faucet(self, *, wait=True):
        return self.client.faucet(self.address, wait=wait)
