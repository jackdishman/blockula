# Vendored `commonware-stream` (Blockula)

This is Commonware's `commonware-stream` crate with a hybrid post-quantum key exchange added.
The node builds it in place of the crates.io release through `[patch.crates-io]` in
`node/Cargo.toml`. `commonware-p2p` and `commonware-cryptography` are untouched: p2p only calls
`encrypted::{dial, listen, Config, Sender, Receiver, MAX_SIZE}`, and none of those signatures changed.
Why and how: [docs/PQ-TRANSPORT.md](../../../docs/PQ-TRANSPORT.md).

## Origin

| | |
|---|---|
| Crate | `commonware-stream` **2026.9.0** from crates.io (published 2026-09-03), MIT OR Apache-2.0 |
| Tarball sha256 | `e4448b7e1d6a0d7a3fc9521be6ddebba0e3bde321da63e414233b6c53bd39e77` (the checksum `node/Cargo.lock` recorded before the patch) |
| Upstream source | github.com/commonwarexyz/monorepo, commit `d476a2361ce6840d2b9d0aa6fb30a924429046d4`, path `stream/` (from `.cargo_vcs_info.json`) |
| Version kept | `2026.9.0`, so `commonware-p2p`'s `^2026.9.0` requirement resolves to this copy |

`bash node/vendor/commonware-stream/verify-vendor.sh` re-downloads (or reuses Cargo's cached) tarball,
checks its sha256, fails if any upstream file other than the three below changed, and prints the
diff and the list of added files. The first commit that added this directory is the pristine
tarball, so `git log -p node/vendor/commonware-stream` shows the same thing.

## What changed

Upstream files (each change is marked `Blockula:`):

- `Cargo.toml`: the `[lints.clippy]` and `[lints.rust]` deny tables are dropped (path dependencies
  aren't lint-capped, and `warnings = "deny"` would break our build on a newer rustc); the
  `unexpected_cfgs` table stays. `resolver` is dropped (the workspace root sets it). Adds
  `ml-kem = "=0.3.2"` with `zeroize`, names x25519-dalek's `zeroize` feature, and swaps the unused
  `criterion` dev-dependency for the test-only ones (ML-DSA signer, vectors, aws-lc-rs oracle).
- `src/lib.rs`: declares `pub mod hybrid` and says so in the crate docs.
- `src/encrypted.rs`: imports `Syn`, `SynAck`, `Ack`, `Context` and the four handshake functions
  from `crate::hybrid` instead of `commonware_cryptography::handshake`, and updates the module docs.
  `dial` and `listen` are otherwise byte-for-byte upstream.
- `Cargo.lock` is removed (a workspace member's lockfile is ignored anyway).

Added files:

- `src/hybrid.rs`: the hybrid handshake. It is upstream's `commonware_cryptography::handshake`
  (2026.9.0, `src/handshake.rs` and `key_exchange.rs`) with the key exchange replaced by
  ML-KEM-768 + X25519, both identities committed before the dialer signs, transcript framing V1
  and a new protocol label. The AEAD (`SendCipher`/`RecvCipher`) and `Error` are still upstream's.
  It exports `SUITE` and `PROTOCOL`, which the node's `/api/status` is built from, so a node that
  doesn't link this copy fails to compile (keep them when re-porting).
- `src/hybrid/tests.rs`, `src/hybrid/mldsa.rs` (a test-only ML-DSA-65 signer matching
  `node/src/pq.rs`), `src/hybrid/ml_kem_vectors.rs`, and `testdata/` (NIST ACVP and Wycheproof
  ML-KEM-768 vectors, trimmed by `testdata/trim.py`; sources and their sha256 are inside the JSON).
- This file and `verify-vendor.sh`.

Tests: `cargo test --manifest-path node/Cargo.toml -p commonware-stream` (the crate is a workspace
member only so its tests run against the node's lockfile).

## Re-porting on the next Commonware upgrade

Upstream has moved since 2026.9.0, in the direction we need:

- **#4829** (merged 2026-09-18): p2p takes a pluggable stream handshake.
- **#5018 / #5020** (merged 2026-09-29/30): the handshake becomes SAKE and the record layer CUPS;
  `stream` exposes `Upgrader` and `Handshake` traits, and p2p's discovery and lookup take any
  `Upgrader`. Its description names our plan: "a hybrid ML-KEM exchange could ship as SAKE V2".
- **#5183** (draft, opened 2026-10-08): a `cryptography::Kem` trait (`generate`, `encapsulate`,
  `decapsulate`, all randomness from the caller), `X25519` and `MlKem768` (RustCrypto `ml-kem 0.3.2`)
  as `Kem`s, and SAKE generic over the KEM. It ships ML-KEM-768 alone; a hybrid `Kem` is listed as
  a follow-up.

When the node moves to a Commonware release after 2026.9.0:

1. **If it has #5183's `Kem` trait:** write a hybrid `Kem` in the node (encapsulation key
   `ek_ML-KEM ‖ X25519`, ciphertext `ct_ML-KEM ‖ X25519`, shared secret `ss_ML-KEM ‖ ss_X25519`,
   the same order and sizes as here), hand it to SAKE, delete this directory and the
   `[patch.crates-io]` entry. Port `tests.rs` (tampering, low-order keys, fail-closed, KAT) and
   `ml_kem_vectors.rs` to the node. Better still, upstream the hybrid `Kem` on #5183.
2. **If it has `Upgrader`/`Handshake` but no `Kem`:** implement `src/hybrid.rs` as a `Handshake`
   in the node, paired with CUPS. Still no fork.
3. **Only if neither:** re-vendor the new `commonware-stream`, re-apply the three upstream-file
   edits above, update the version and sha256 here and in `verify-vendor.sh`, and re-run the
   tests.

What to ask upstream: commit the KEM suite to SAKE's transcript label, not just to the frame sizes,
and make sure any fallback between handshake versions (issue #5029) never crosses KEM suites.

Any re-port changes the wire format and the transcript, so it ships as a coordinated upgrade
(every validator at once, e.g. a scheduled halt height). There is deliberately no negotiation, so
old and new nodes can't talk to each other, and the known-answer hashes in `tests.rs` change.
