# ML-DSA-65 known-answer vectors (SEC-2)

Blockula signs every tx, vote, certificate, proof of possession and epoch handoff with ML-DSA-65 (FIPS 204), in two implementations: the node uses RustCrypto's `ml-dsa`; the SDK (and so the MCP and the wallet) uses `@noble/post-quantum`. Neither is independently audited. These vectors check both against NIST's and Wycheproof's known answers, through the same calls our code makes:

- **Rust:** [`node/src/kat.rs`](../../src/kat.rs), part of `cargo test --manifest-path node/Cargo.toml` (or `cargo test --manifest-path node/Cargo.toml kat:: -- --nocapture` for the per-group report).
- **TypeScript:** [`sdk-ts/test/kat.ts`](../../../sdk-ts/test/kat.ts), `npm run test:kat -w @blockula/sdk`. Offline; no node needed.
- **CI:** `.github/workflows/crypto.yml` runs both on every PR and push to main, and checks that the crypto dependencies stay pinned exactly.

A failing known-answer test is a critical finding (a key-forgery or chain-split bug), not a flaky test. Report it, don't regenerate it away.

## Sources

| File | Upstream | Commit | Upstream SHA-256 |
|---|---|---|---|
| `ML-DSA-65-keyGen.json` | [ACVP-Server `ML-DSA-keyGen-FIPS204/internalProjection.json`](https://github.com/usnistgov/ACVP-Server/blob/975de31eb83d87039ec88934fdc47d8c312b892d/gen-val/json-files/ML-DSA-keyGen-FIPS204/internalProjection.json) | `975de31` (release v1.1.0.43, 2026-08-12) | `e67ee654…463baf` |
| `ML-DSA-65-sigGen.json` | [ACVP-Server `ML-DSA-sigGen-FIPS204-tr1/internalProjection.json`](https://github.com/usnistgov/ACVP-Server/blob/975de31eb83d87039ec88934fdc47d8c312b892d/gen-val/json-files/ML-DSA-sigGen-FIPS204-tr1/internalProjection.json) | `975de31` | `b61576c7…136f18` |
| `ML-DSA-65-sigVer.json` | [ACVP-Server `ML-DSA-sigVer-FIPS204/internalProjection.json`](https://github.com/usnistgov/ACVP-Server/blob/975de31eb83d87039ec88934fdc47d8c312b892d/gen-val/json-files/ML-DSA-sigVer-FIPS204/internalProjection.json) | `975de31` | `47cdd631…a55437` |
| `../wycheproof/mldsa_65_verify_test.json` | [C2SP Wycheproof `testvectors_v1/mldsa_65_verify_test.json`](https://github.com/C2SP/wycheproof/blob/7a325932231bd7a724c791c8b0d65ea4d6d2f435/testvectors_v1/mldsa_65_verify_test.json) | `7a32593` (2026-10-12) | `1ca235f6…979034` |
| `../wycheproof/mldsa_65_sign_seed_test.json` | [C2SP Wycheproof `testvectors_v1/mldsa_65_sign_seed_test.json`](https://github.com/C2SP/wycheproof/blob/7a325932231bd7a724c791c8b0d65ea4d6d2f435/testvectors_v1/mldsa_65_sign_seed_test.json) | `7a32593` | `e26b2bb9…0133e0` |

Fetched 2026-10-14. Full hashes are pinned in [`trim.py`](trim.py) and recorded in each file's `sourceSha256`.

## Trimming

`python3 node/testdata/acvp/trim.py` downloads the five files at the pinned commits (or reads them with `--src DIR`), refuses any whose SHA-256 differs, and rewrites the vendored files byte for byte. It keeps upstream's shape, ids and field names, and only:

- keeps the ML-DSA-65 groups listed below;
- drops fields no test reads (`deferred`, `hashAlg`, the expanded `sk` where a seed is given, Wycheproof's DER and PKCS#8 encodings);
- replaces large outputs the tests only compare (keyGen `pk`/`sk`, sigGen `signature`/`pk`, Wycheproof sign `sig`/`publicKey`) with their SHA-256, as `pkSha256`, `signatureSha256`, and so on. Every input stays verbatim, including every byte a verifier reads.

That brings the ACVP part to about 1.2 MB (about 0.7 MB compressed in git), plus 1.6 MB of Wycheproof (about 0.6 MB compressed).

## What runs, and against which code path

| Group | Cases | Rust (`ml-dsa` 0.1.1) | TypeScript (`@noble/post-quantum` 0.7.1) |
|---|---|---|---|
| ACVP keyGen tg2 (`KeyGen_internal`, seed) | 25 | `crypto::Keys::from_seed`, `pq::PrivateKey::from_seed_bytes` (public key); expanded secret key | `ml_dsa65.keygen` (public and secret key), `Wallet.fromSeed` |
| ACVP sigGen tg5: external, pure, with context, deterministic | 15 | `pq::PrivateKey::sign` (context = namespace), `crypto::Keys::sign` when the context is empty, raw `sign_deterministic` | `ml_dsa65.sign({ context, extraEntropy: false })` |
| ACVP sigGen tg29: external, pure, with context, hedged | 15 | raw `sign_randomized` with NIST's `rnd` | `ml_dsa65.sign({ context, extraEntropy: rnd })` (what `Wallet.sign` runs, with fresh randomness); the default context is checked to be the empty one |
| ACVP sigGen tg18 / tg42: `Sign_internal`, deterministic / hedged | 15 + 15 | raw `sign_internal` | `ml_dsa65.internal.sign` |
| ACVP sigGen tg17 / tg41: external µ, deterministic / hedged | 15 + 15 | raw `sign_mu_deterministic` / `sign_mu_randomized` | `ml_dsa65.internal.sign({ externalMu: true })` |
| ACVP sigVer tg3: external, pure, with context | 15 | `pq::PublicKey::verify` after codec decode, `crypto::verify` when the context is empty, raw `verify_with_context`; all must agree | `ml_dsa65.verify({ context })` |
| ACVP sigVer tg10: `Verify_internal` | 15 | raw `verify_internal` | `ml_dsa65.internal.verify` |
| ACVP sigVer tg9: external µ | 15 | raw `verify_mu` | `ml_dsa65.internal.verify({ externalMu: true })` |
| Wycheproof verify (bad hints, ‖z‖∞ bounds, wrong lengths, zero keys, long contexts, rejection-loop corners) | 210 | raw API, `pq::PublicKey::verify` (contexts ≤ 255 B), `crypto::verify` (empty context) | `ml_dsa65.verify` |
| Wycheproof sign from seed (rejection-loop and sampling corners, external µ, hedged, bad seed lengths, long contexts) | 105 | raw API, plus `pq::PrivateKey::sign` and `crypto::Keys::sign` on the deterministic external cases | `ml_dsa65.sign` / `internal.sign` |

**Result (2026-10-14): every group passes in both implementations. No discrepancies across 475 cases in each implementation** (160 ACVP, 315 Wycheproof; `kat.ts` reports 531 because it also counts the cross-implementation vectors in [`../cross`](../cross)).

Excluded on purpose:

- **HashML-DSA (`preHash`) groups.** Nothing in Blockula signs or verifies pre-hashed messages.
- **`keyFormat: expanded` sigGen groups and the older `ML-DSA-sigGen-FIPS204` file** (expanded keys only). Both code paths always derive keys from the 32-byte seed. NIST's expanded secret key is still compared in keyGen.
- **Wycheproof `mldsa_65_sign_noseed_test.json`** (expanded private keys, as above).

## Behaviour worth knowing

- `crypto.rs` signs txs with `ml_dsa::Signer::sign`, which is FIPS 204 deterministic signing with an empty context. `pq.rs` signs consensus messages with `sign_deterministic(msg, namespace)` and hashes namespaces longer than 255 bytes to `BLAKE3(namespace)` (tested in `long_namespaces_are_hashed_to_a_context`). The SDK signs txs hedged (fresh randomness) with an empty context. Both kinds of signature verify everywhere.
- On malformed input, noble throws (wrong public-key length, context over 255 bytes) where `ml-dsa` returns `false`. `kat.ts` treats a throw as a rejection.
- `ml-dsa`'s `Signature::decode` enforces ‖z‖∞ < γ1 − β and canonical hints, so malformed signatures fail at decode (`pq::Signature::read_cfg` returns a codec error). Wycheproof's infinity-norm and hint-encoding cases cover this.

## Pinned versions and audit status

All checked on 2026-10-14. Each pinned version is the newest release.

| Dependency | Pin | Audit status and advisories |
|---|---|---|
| `ml-dsa` (RustCrypto) | `=0.1.1` in `node/Cargo.toml` (released 2026-06-05) | README: "has never been independently audited". Advisories, all fixed before 0.1.0: [GHSA-hcp2-x6j4-29j7](https://github.com/advisories/GHSA-hcp2-x6j4-29j7) / CVE-2026-22705 (timing side channel in decomposition, ≤ rc.2), [GHSA-5x2r-hc65-25f9](https://github.com/advisories/GHSA-5x2r-hc65-25f9) / CVE-2026-24850 (repeated hint indices accepted, < rc.4), [GHSA-h37v-hp6w-2pp8](https://github.com/advisories/GHSA-h37v-hp6w-2pp8) (`UseHint` off by two when r0 = 0, ≤ rc.4). Its own dependencies (`module-lattice` 0.2.3, `hybrid-array` 0.4.15, `shake` 0.1.0, `signature` 3.0.0) are fixed by `node/Cargo.lock`. |
| `blake3` | `=1.8.7` | Addresses, key ids, block digests, state roots, long-namespace contexts. No advisories. |
| `bech32` | `=0.12.0` | Address encoding, where a decoding bug would alias accounts. |
| `@noble/post-quantum` | `0.7.1` in `sdk-ts/package.json` (released 2026-08-27) | README: "has not been independently audited yet"; self-audited at 0.6.1 (April 2026); no constant-time claim. No GitHub advisories. Pins its own dependencies exactly (`@noble/hashes`, `@noble/curves`, `@noble/ciphers` 2.4.0). |
| `@noble/hashes` | `2.4.0` | Audited by Cure53 at 1.0.0 (January 2022), but that audit's scope excluded `blake3`, which the SDK uses for addresses and key ids. The cross-implementation vectors check its BLAKE3 against the Rust crate's. |
| `@scure/base` | `2.4.0` | Address encoding. Audited by Cure53 at 1.0.0, self-audited at 2.2.0. |

Commonware (consensus, p2p, codec) is pinned separately at `=2026.9.0`. SEC-1's external audit has to cover our use of both ML-DSA libraries (MAINNET.md SEC-2).
