#!/usr/bin/env python3
"""Rebuilds Blockula's vendored ML-DSA-65 known-answer vectors (SEC-2) from pinned upstream files.

    python3 node/testdata/acvp/trim.py            # download the pinned files from GitHub
    python3 node/testdata/acvp/trim.py --src DIR  # or read them from DIR/<name>.json

Every upstream file is checked against its pinned SHA-256 before use. Output (Python stdlib only,
deterministic bytes):

  node/testdata/acvp/ML-DSA-65-{keyGen,sigGen,sigVer}.json   NIST ACVP-Server internalProjection
  node/testdata/wycheproof/mldsa_65_{verify,sign_seed}_test.json   C2SP Wycheproof

Trimming keeps the upstream shape (testGroups / tests, upstream field names and ids) and only:
  * keeps ML-DSA-65 groups for the interfaces the node and SDK can reach (see GROUPS below);
  * drops fields no test reads (`deferred`, `hashAlg`, the expanded `sk` where a seed is given,
    Wycheproof's DER/PKCS#8 encodings);
  * replaces large *outputs* the tests only compare (keyGen pk/sk, sigGen signatures and pk,
    Wycheproof sign signatures and public keys) by their SHA-256, as `<field>Sha256`.
Inputs (seeds, messages, contexts, rnd, mu, and every byte a verifier reads) stay verbatim.
"""
import hashlib, json, os, sys, urllib.request

ACVP_COMMIT = "975de31eb83d87039ec88934fdc47d8c312b892d"  # ACVP-Server v1.1.0.43, 2026-08-12
WYCHEPROOF_COMMIT = "7a325932231bd7a724c791c8b0d65ea4d6d2f435"  # C2SP/wycheproof, 2026-10-12
ACVP_URL = f"https://raw.githubusercontent.com/usnistgov/ACVP-Server/{ACVP_COMMIT}/gen-val/json-files/{{}}/internalProjection.json"
WYCHEPROOF_URL = f"https://raw.githubusercontent.com/C2SP/wycheproof/{WYCHEPROOF_COMMIT}/testvectors_v1/{{}}.json"

# name -> (url, sha256 of the upstream bytes)
UPSTREAM = {
    "ML-DSA-keyGen-FIPS204": (ACVP_URL, "e67ee6540d40e11506c3c4e3b1f79fc1cefcd49820db99fc61f87cc8ba463baf"),
    "ML-DSA-sigGen-FIPS204-tr1": (ACVP_URL, "b61576c765b1eb0e6a667a67a68380df944592be8740f9c020c9ea5a89136f18"),
    "ML-DSA-sigVer-FIPS204": (ACVP_URL, "47cdd6314c7f746d02421ffcba89d4dbc7bb875ac49e07a029fdfc26fba55437"),
    "mldsa_65_verify_test": (WYCHEPROOF_URL, "1ca235f61928421a173a171780beb00264ae9155dc33c52a11e1766cc7979034"),
    "mldsa_65_sign_seed_test": (WYCHEPROOF_URL, "e26b2bb9ab8d45ed570ba6828c7e40b6fa3e1545bb8bc2c449c77531760133e0"),
}

# ACVP groups kept, by (file, tgId). Pre-hash (HashML-DSA) groups are out: nothing signs with it.
# sigGen comes from the FIPS204-tr1 revision, whose `keyFormat: seed` groups derive the key from
# the 32-byte seed exactly as the node and SDK do; `expanded` groups (and the older FIPS204 file,
# which only has expanded keys) import a 4,032-byte secret key, which neither code path does.
GROUPS = {
    "ML-DSA-keyGen-FIPS204": {2},  # KeyGen_internal(seed)
    "ML-DSA-sigGen-FIPS204-tr1": {
        5,  # external (pure, with context), deterministic: crypto.rs / pq.rs signing
        29,  # external (pure, with context), hedged (rnd): the SDK's Wallet.sign
        18,  # Sign_internal, deterministic
        42,  # Sign_internal, hedged
        17,  # external mu, deterministic
        41,  # external mu, hedged
    },
    "ML-DSA-sigVer-FIPS204": {
        3,  # external (pure, with context): crypto::verify / pq::PublicKey::verify
        10,  # Verify_internal
        9,  # external mu
    },
}

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))


def load(name, src):
    url, want = UPSTREAM[name]
    if src:
        raw = open(os.path.join(src, name + ".json"), "rb").read()
    else:
        with urllib.request.urlopen(url.format(name), timeout=120) as r:
            raw = r.read()
    got = hashlib.sha256(raw).hexdigest()
    if got != want:
        sys.exit(f"{name}: sha256 {got} doesn't match the pinned {want}")
    return json.loads(raw)


def sha(hex_str):
    return hashlib.sha256(bytes.fromhex(hex_str)).hexdigest()


def hashed(t, fields):
    """Replace each present output field by `<field>Sha256`, keeping key order."""
    out = {}
    for k, v in t.items():
        if k in fields and isinstance(v, str) and v:
            out[k + "Sha256"] = sha(v)
        else:
            out[k] = v
    return out


def acvp(name, outputs, drop):
    j = load(name, SRC)
    groups = []
    for g in j["testGroups"]:
        if g["tgId"] not in GROUPS[name]:
            continue
        assert g["parameterSet"] == "ML-DSA-65", (name, g["tgId"])
        g = dict(g)
        g["tests"] = [hashed({k: v for k, v in t.items() if k not in drop}, outputs) for t in g["tests"]]
        groups.append(g)
    assert {g["tgId"] for g in groups} == GROUPS[name], name
    head = {k: v for k, v in j.items() if k != "testGroups"}
    return {
        "source": ACVP_URL.format(name),
        "sourceSha256": UPSTREAM[name][1],
        "trimmedBy": "node/testdata/acvp/trim.py",
        **head,
        "testGroups": groups,
    }


def wycheproof(name, group_outputs, test_outputs, drop):
    j = load(name, SRC)
    groups = []
    for g in j["testGroups"]:
        g = hashed({k: v for k, v in g.items() if k not in drop}, group_outputs)
        g["tests"] = [hashed(t, test_outputs) for t in g["tests"]]
        groups.append(g)
    head = {k: v for k, v in j.items() if k != "testGroups"}
    return {
        "source": WYCHEPROOF_URL.format(name),
        "sourceSha256": UPSTREAM[name][1],
        "trimmedBy": "node/testdata/acvp/trim.py",
        **head,
        "testGroups": groups,
    }


def write(rel, doc):
    path = os.path.join(ROOT, rel)
    with open(path, "w") as f:
        json.dump(doc, f, indent=1, ensure_ascii=False)
        f.write("\n")
    n = sum(len(g["tests"]) for g in doc["testGroups"])
    print(f"{rel}: {len(doc['testGroups'])} groups, {n} tests, {os.path.getsize(path)} bytes")


SRC = sys.argv[sys.argv.index("--src") + 1] if "--src" in sys.argv else None

write("acvp/ML-DSA-65-keyGen.json", acvp("ML-DSA-keyGen-FIPS204", {"pk", "sk"}, {"deferred"}))
write("acvp/ML-DSA-65-sigGen.json", acvp("ML-DSA-sigGen-FIPS204-tr1", {"pk", "signature"}, {"deferred", "hashAlg", "sk"}))
write("acvp/ML-DSA-65-sigVer.json", acvp("ML-DSA-sigVer-FIPS204", set(), {"deferred", "hashAlg", "sk"}))
write("wycheproof/mldsa_65_verify_test.json", wycheproof("mldsa_65_verify_test", set(), set(), {"publicKeyDer"}))
write("wycheproof/mldsa_65_sign_seed_test.json", wycheproof("mldsa_65_sign_seed_test", {"publicKey"}, {"sig"}, {"privateKeyPkcs8"}))
