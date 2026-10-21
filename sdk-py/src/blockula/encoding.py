"""Canonical transfer/name bytes, matching node/src/types.rs and sdk-ts/src/tx.ts."""
import re
from blake3 import blake3

MAX_U64 = (1 << 64) - 1
MAX_JSON_INT = (1 << 53) - 1
PUBLIC_KEY_LEN = 1952
SIGNATURE_LEN = 3309


def uint(value, maximum=MAX_U64):
    if isinstance(value, str) and re.fullmatch(r"[0-9]+", value, flags=re.ASCII):
        value = int(value)
    if type(value) is not int or not 0 <= value <= maximum:
        raise ValueError(f"expected unsigned integer from 0 to {maximum}")
    return value


def text(value):
    if not isinstance(value, str):
        raise TypeError("expected a string")
    raw = value.encode("utf-8")
    if len(raw) > 65535:
        raise ValueError("UTF-8 string exceeds 65535 bytes")
    return len(raw).to_bytes(2, "little") + raw


def signing_bytes(tx):
    """Encode only the two actions this minimal SDK implements; refuse every other tag."""
    genesis = tx["genesis_hash"]
    if not isinstance(genesis, str) or not re.fullmatch(r"[0-9a-f]{64}", genesis):
        raise ValueError("genesis_hash must be 32 bytes of lowercase hex")
    action = tx["action"]
    if action["type"] == "transfer":
        act = b"\x00" + text(action["to"]) + uint(action["amount"]).to_bytes(8, "little")
    elif action["type"] == "register_name":
        act = b"\x01" + text(action["name"])
    else:
        raise ValueError(f"unsupported action: {action['type']}")
    return (b"BLOCKULA_TX_V3" + text(tx["chain_id"]) + bytes.fromhex(genesis)
            + text(tx["from"]) + uint(tx["nonce"], MAX_JSON_INT).to_bytes(8, "little")
            + uint(tx["fee"]).to_bytes(8, "little")
            + uint(tx.get("tip", "0")).to_bytes(8, "little")
            + uint(tx["valid_until"], MAX_JSON_INT).to_bytes(8, "little")
            + act + text(tx.get("memo", "")))


def tx_id(tx):
    signature = bytes.fromhex(tx["signature"])
    if len(signature) != SIGNATURE_LEN:
        raise ValueError("invalid ML-DSA-65 signature length")
    return blake3(signing_bytes(tx) + signature).hexdigest()


def base_fee(tx, slot):
    size = len(signing_bytes(tx)) + PUBLIC_KEY_LEN + SIGNATURE_LEN
    return uint(slot["flat"]) + uint(slot["rate"]) * size // 1000


def parse_fang(amount):
    """A decimal FANG string or an integer number of drops. Floats are never accepted."""
    if type(amount) is int:
        return uint(amount)
    if not isinstance(amount, str) or not re.fullmatch(r"[0-9]+(?:\.[0-9]{1,9})?", amount):
        raise ValueError("amount must be a decimal FANG string or integer drops (not float)")
    whole, _, fraction = amount.partition(".")
    return uint(int(whole) * 1_000_000_000 + int(fraction.ljust(9, "0")))


def bare_name(name):
    if not isinstance(name, str):
        raise TypeError("name must be a string")
    name = name.strip().lower().removesuffix(".boo")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{1,30}[a-z0-9]", name):
        raise ValueError("name must have 3-32 ASCII letters, digits or internal hyphens")
    return name


_CHARS = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"


def _polymod(values):
    chk = 1
    for value in values:
        top = chk >> 25
        chk = ((chk & 0x1ffffff) << 5) ^ value
        for i, generator in enumerate((0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3)):
            if (top >> i) & 1:
                chk ^= generator
    return chk


def _hrp(prefix):
    return [ord(c) >> 5 for c in prefix] + [0] + [ord(c) & 31 for c in prefix]


def address_from_public_key(public_key, prefix="boo"):
    if prefix not in ("boo", "tboo") or len(public_key) != PUBLIC_KEY_LEN:
        raise ValueError("invalid prefix or ML-DSA-65 public key length")
    digest = blake3(public_key).digest()
    bits = int.from_bytes(digest, "big") << 4
    words = [(bits >> shift) & 31 for shift in range(255, -1, -5)]
    check = _polymod(_hrp(prefix) + words + [0] * 6) ^ 0x2bc830a3
    checksum = [(check >> (5 * (5 - i))) & 31 for i in range(6)]
    return prefix + "1" + "".join(_CHARS[x] for x in words + checksum)


def is_address(value, prefix=None):
    if not isinstance(value, str) or value != value.lower():
        return False
    hrp, sep, body = value.rpartition("1")
    if not sep or hrp not in ("boo", "tboo") or (prefix and hrp != prefix) or len(body) != 58:
        return False
    try:
        words = [_CHARS.index(c) for c in body]
    except ValueError:
        return False
    return words[-7] & 15 == 0 and _polymod(_hrp(hrp) + words) == 0x2bc830a3
