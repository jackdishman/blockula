"""Bounded HTTP reads and single-submit transaction handling. No automatic POST retries."""
import json
import math
import time
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlsplit
from urllib.request import Request, urlopen

from .encoding import uint, MAX_JSON_INT


class ApiError(Exception):
    def __init__(self, status, message):
        self.status = status
        super().__init__(f"HTTP {status}: {message}")


class TxPendingError(Exception):
    """Outcome is unknown/pending: check tx_id; never blindly resubmit."""
    def __init__(self, tx_id, message="transaction may still execute"):
        self.tx_id = tx_id
        super().__init__(f"{tx_id}: {message}")


class TxFailedError(Exception):
    def __init__(self, tx_id, receipt):
        self.tx_id, self.receipt = tx_id, receipt
        super().__init__(f"{tx_id}: {receipt.get('error', 'transaction failed')}")


class Client:
    def __init__(self, rpc="http://localhost:7070", *, chain_id=None, genesis_hash=None,
                 prefix=None, timeout=10):
        url = urlsplit(rpc)
        if url.scheme not in ("http", "https") or not url.hostname or url.username or url.password or url.query or url.fragment:
            raise ValueError("RPC must be an http(s) base URL without credentials, query or fragment")
        if not isinstance(timeout, (int, float)) or not math.isfinite(timeout) or timeout <= 0:
            raise ValueError("timeout must be positive and finite")
        if prefix is not None and prefix not in ("boo", "tboo"):
            raise ValueError("unknown address prefix")
        self.rpc, self.timeout = rpc.rstrip("/"), timeout
        self.chain_id, self.genesis_hash, self.prefix = chain_id, genesis_hash, prefix

    def _call(self, path, data=None):
        raw = None if data is None else json.dumps(data, allow_nan=False).encode()
        request = Request(self.rpc + "/api" + path, data=raw,
                          headers={"Content-Type": "application/json", "Accept": "application/json"})
        try:
            with urlopen(request, timeout=self.timeout) as response:
                return json.load(response)
        except HTTPError as error:
            try:
                message = json.loads(error.read()).get("error", error.reason)
            except (ValueError, AttributeError):
                message = error.reason
            raise ApiError(error.code, message) from error

    def status(self):
        status = self._call("/status")
        for field, expected in (("chain_id", self.chain_id), ("genesis_hash", self.genesis_hash), ("address_prefix", self.prefix)):
            actual = status.get(field)
            if not isinstance(actual, str) or not actual:
                raise ValueError(f"node did not report {field}")
            if expected is not None and actual != expected:
                raise ValueError(f"chain mismatch: {field}")
        if status["address_prefix"] not in ("boo", "tboo"):
            raise ValueError("unknown node address prefix")
        self.chain_id = status["chain_id"]
        self.genesis_hash = status["genesis_hash"]
        self.prefix = status["address_prefix"]
        return status

    def account(self, address_or_name):
        return self._call("/account/" + quote(address_or_name, safe=""))

    def name(self, name):
        return self._call("/name/" + quote(name, safe=""))

    def tx(self, tx_id):
        return self._call("/tx/" + quote(tx_id, safe=""))

    def blocks(self, limit=20, *, before=None):
        limit = uint(limit, MAX_JSON_INT)
        query = f"/blocks?limit={limit}"
        if before is not None:
            query += f"&before={uint(before, MAX_JSON_INT)}"
        return self._call(query)

    def fees(self):
        return self._call("/fees")

    def wait_for_tx(self, tx_id, *, timeout=30):
        if not isinstance(timeout, (int, float)) or not math.isfinite(timeout) or timeout <= 0:
            raise ValueError("timeout must be positive and finite")
        deadline = time.monotonic() + timeout
        while True:
            try:
                receipt = self.tx(tx_id)
                if receipt.get("status") == "finalized":
                    return receipt
                if receipt.get("status") == "failed":
                    raise TxFailedError(tx_id, receipt)
            except ApiError as error:
                if error.status not in (404, 429, 500, 502, 503, 504):
                    raise
            except (TimeoutError, URLError):
                pass
            if time.monotonic() >= deadline:
                raise TxPendingError(tx_id)
            time.sleep(0.25)

    def submit(self, tx, *, wait=True):
        from .encoding import tx_id
        expected = tx_id(tx)
        try:
            result = self._call("/tx", tx)
        except ApiError as error:
            if error.status >= 500 or error.status == 408:
                raise TxPendingError(expected, "submission outcome unknown; query this id") from error
            raise
        except (TimeoutError, URLError, ValueError) as error:
            raise TxPendingError(expected, "submission outcome unknown; query this id") from error
        if result.get("id") != expected:
            raise TxPendingError(expected, "node returned a different transaction id")
        return self.wait_for_tx(expected) if wait else result

    def faucet(self, to, *, wait=True):
        result = self._call("/faucet", {"to": to})
        return self.wait_for_tx(result["id"]) if wait else result
