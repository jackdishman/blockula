import copy
import json
from pathlib import Path
import unittest
from unittest.mock import patch
from Crypto.PublicKey import MLDSA
from Crypto.Signature import mldsa

from blockula import (Client, Wallet, ApiError, TxFailedError, TxPendingError,
                      address_from_public_key, is_address, parse_fang, signing_bytes, tx_id)

ROOT = Path(__file__).resolve().parents[2]
VECTORS = json.loads((Path(__file__).parent / "wallet-vectors.json").read_text())["vectors"]


class EncodingTests(unittest.TestCase):
    def test_shared_node_vectors(self):
        cases = json.loads((ROOT / "sdk-ts/test/tx-vectors.json").read_text())["cases"]
        for case in cases:
            with self.subTest(case=case["label"]):
                if case["tx"]["action"]["type"] in ("transfer", "register_name"):
                    self.assertEqual(signing_bytes(case["tx"]).hex(), case["signing_bytes"])
                else:
                    with self.assertRaises(ValueError):
                        signing_bytes(case["tx"])

    def test_seed_public_key_address_signature_id_match_typescript(self):
        for vector in VECTORS:
            with self.subTest(case=vector["label"]):
                wallet = Wallet.from_seed(vector["seed"], Client(prefix="boo"))
                self.assertEqual(wallet.public_key.hex(), vector["public_key"])
                self.assertEqual(wallet.key_id, vector["key_id"])
                self.assertEqual(wallet.address, vector["address"])
                self.assertEqual(address_from_public_key(wallet.public_key, "tboo"), vector["testnet_address"])
                self.assertTrue(is_address(wallet.address, "boo"))
                signed = wallet.sign(vector["tx"], deterministic=True)
                self.assertEqual(signed["signature"], vector["signature"])
                self.assertEqual(tx_id(signed), vector["tx_id"])
                mldsa.new(MLDSA.construct(param_set=MLDSA.ML_DSA_65, pk=wallet.public_key)).verify(
                    signing_bytes(signed), bytes.fromhex(signed["signature"]))

    def test_bad_address_checksums_prefix_and_padding(self):
        address = VECTORS[0]["address"]
        self.assertFalse(is_address(address, "tboo"))
        self.assertFalse(is_address(address.upper()))
        self.assertFalse(is_address(address[:-1] + ("q" if address[-1] != "q" else "p")))
        self.assertFalse(is_address("boo1not-an-address"))

    def test_amounts_exact_and_bounded(self):
        self.assertEqual(parse_fang("1.000000001"), 1000000001)
        self.assertEqual(parse_fang(1), 1)
        for amount in (1.5, True, -1, "1e3", "1.0000000001", "-1", " 1", str(1 << 64)):
            with self.subTest(amount=amount), self.assertRaises(ValueError):
                parse_fang(amount)

    def test_malformed_tx_not_signed(self):
        wallet = Wallet.from_seed(bytes(32), Client(prefix="boo"))
        for field, value in (("nonce", -1), ("nonce", 1 << 53), ("fee", "-1"),
                             ("fee", str(1 << 64)), ("valid_until", True),
                             ("memo", None), ("genesis_hash", "ab")):
            tx = copy.deepcopy(VECTORS[0]["tx"])
            tx[field] = value
            with self.subTest(field=field, value=value), self.assertRaises((ValueError, TypeError)):
                wallet.sign(tx)

    def test_signature_tampering_rejected(self):
        vector = VECTORS[0]
        key = MLDSA.construct(param_set=MLDSA.ML_DSA_65, pk=bytes.fromhex(vector["public_key"]))
        with self.assertRaises(ValueError):
            mldsa.new(key).verify(signing_bytes(vector["tx"]) + b"tampered", bytes.fromhex(vector["signature"]))


class ClientTests(unittest.TestCase):
    def test_pins_refuse_chain_changes(self):
        c = Client()
        status = {"chain_id": "dev", "genesis_hash": "ab" * 32, "address_prefix": "boo"}
        with patch.object(c, "_call", return_value=status):
            c.status()
        with patch.object(c, "_call", return_value={**status, "genesis_hash": "cd" * 32}):
            with self.assertRaisesRegex(ValueError, "chain mismatch"):
                c.status()

    def test_escaped_read_paths(self):
        c = Client()
        with patch.object(c, "_call", return_value={}) as call:
            c.account("a/b?c")
            call.assert_called_once_with("/account/a%2Fb%3Fc")

    def test_submission_never_retried(self):
        c = Client()
        vector = VECTORS[0]
        tx = {**vector["tx"], "signature": vector["signature"], "public_key": vector["public_key"]}
        with patch.object(c, "_call", side_effect=TimeoutError) as call:
            with self.assertRaises(TxPendingError) as caught:
                c.submit(tx)
            self.assertEqual(caught.exception.tx_id, vector["tx_id"])
            self.assertEqual(call.call_count, 1)

    def test_failed_and_pending_distinguished(self):
        c = Client()
        with patch.object(c, "tx", return_value={"status": "failed", "error": "insufficient balance"}):
            with self.assertRaises(TxFailedError):
                c.wait_for_tx("x")
        with patch.object(c, "tx", return_value={"status": "pending"}):
            with self.assertRaises(TxPendingError):
                c.wait_for_tx("x", timeout=0.01)

    def test_submission_server_error_is_unknown_but_rejection_is_not(self):
        c = Client()
        vector = VECTORS[0]
        tx = {**vector["tx"], "signature": vector["signature"], "public_key": vector["public_key"]}
        for status in (408, 500, 502, 503, 504):
            with self.subTest(status=status), patch.object(c, "_call", side_effect=ApiError(status, "error")) as call:
                with self.assertRaises(TxPendingError) as caught:
                    c.submit(tx)
                self.assertEqual(caught.exception.tx_id, vector["tx_id"])
                self.assertEqual(call.call_count, 1)
        with patch.object(c, "_call", side_effect=ApiError(400, "invalid transaction")):
            with self.assertRaises(ApiError):
                c.submit(tx)

    def test_mainnet_signing_refused(self):
        c = Client(prefix="boo")
        wallet = Wallet.from_seed(bytes(32), c)
        with patch.object(c, "status", return_value={"chain_id": "blockula-1"}):
            with self.assertRaisesRegex(ValueError, "mainnet"):
                wallet.build({"type": "register_name", "name": "alice"})

    def test_transport_error_not_swallowed_as_missing_account(self):
        c = Client(prefix="boo")
        wallet = Wallet.from_seed(bytes(32), c)
        with patch.object(c, "status", return_value={"chain_id": "dev", "height": 1}), \
             patch.object(c, "account", side_effect=ApiError(503, "unavailable")):
            with self.assertRaises(ApiError):
                wallet.build({"type": "register_name", "name": "alice"})


if __name__ == "__main__":
    unittest.main()
