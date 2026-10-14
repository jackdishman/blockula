#!/usr/bin/env python3
"""Build ml-kem-768.json: the ML-KEM-768 test vectors the hybrid handshake tests run.

Usage: trim.py <dir with the six source files> <out file>

Sources (pinned commits; sha256 of each file is recorded in the output):
  NIST ACVP-Server gen-val/json-files/ML-KEM-{keyGen,encapDecap}-FIPS203/internalProjection.json
  C2SP Wycheproof testvectors_v1/mlkem_768_{test,encaps_test}.json

Keeps every ML-KEM-768 ACVP group (keyGen, encapsulation, decapsulation, both key checks) and a
deterministic Wycheproof subset: every special-source case (CCTV, implicit rejection), every case
whose key or ciphertext has the wrong length, and the first 24 valid and 24 invalid full-size
FIPS 203 cases of each file. Drops fields no test reads.
"""
import hashlib, json, os, sys

ACVP = "975de31eb83d87039ec88934fdc47d8c312b892d"
WYCHEPROOF = "7a325932231bd7a724c791c8b0d65ea4d6d2f435"
src, out = sys.argv[1], sys.argv[2]


def load(name):
    raw = open(os.path.join(src, name), "rb").read()
    return json.loads(raw), hashlib.sha256(raw).hexdigest()


def pick(t, keys):
    return {k: t[k] for k in keys if k in t}


result = {
    "sources": {
        "acvp": f"https://github.com/usnistgov/ACVP-Server/tree/{ACVP}/gen-val/json-files",
        "wycheproof": f"https://github.com/C2SP/wycheproof/tree/{WYCHEPROOF}/testvectors_v1",
        "sha256": {},
    },
    "acvp": {},
    "wycheproof": {},
}

keygen, h = load("ML-KEM-keyGen-FIPS203.json")
result["sources"]["sha256"]["ML-KEM-keyGen-FIPS203/internalProjection.json"] = h
for g in keygen["testGroups"]:
    if g["parameterSet"] == "ML-KEM-768":
        result["acvp"]["keyGen"] = [pick(t, ["tcId", "d", "z", "ek", "dk"]) for t in g["tests"]]

encdec, h = load("ML-KEM-encapDecap-FIPS203.json")
result["sources"]["sha256"]["ML-KEM-encapDecap-FIPS203/internalProjection.json"] = h
fields = {
    "encapsulation": ["tcId", "ek", "m", "c", "k"],
    "decapsulation": ["tcId", "dk", "c", "k"],
    "encapsulationKeyCheck": ["tcId", "ek", "testPassed"],
    "decapsulationKeyCheck": ["tcId", "dk", "testPassed"],
}
for g in encdec["testGroups"]:
    if g["parameterSet"] == "ML-KEM-768":
        result["acvp"][g["function"]] = [pick(t, fields[g["function"]]) for t in g["tests"]]

for name, keep in [
    ("mlkem_768_test.json", ["tcId", "comment", "seed", "ek", "c", "K", "result"]),
    ("mlkem_768_encaps_test.json", ["tcId", "comment", "ek", "m", "c", "K", "result"]),
]:
    j, h = load(name)
    result["sources"]["sha256"][name] = h
    tests, seen = [], {}
    for g in j["testGroups"]:
        special = g["source"]["name"] != "FIPS 203"
        for t in g["tests"]:
            # Bucket FIPS 203 cases by outcome and whether the key has the right length; keep the
            # first 24 of each full-size bucket and every short or long (cheap) one.
            bucket = (t["result"], len(t.get("ek", "")) == 2 * 1184)
            if not special:
                seen[bucket] = seen.get(bucket, 0) + 1
            if special or not bucket[1] or seen[bucket] <= 24:
                tests.append(pick(t, keep) | {"flags": t.get("flags", [])})
    result["wycheproof"][name.removesuffix(".json")] = tests

json.dump(result, open(out, "w"), indent=0, separators=(",", ":"))
print(out, os.path.getsize(out), "bytes;",
      {k: len(v) for k, v in result["acvp"].items()}, {k: len(v) for k, v in result["wycheproof"].items()})
