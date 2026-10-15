# Post-quantum transport

Blockula signs everything with ML-DSA-65, and since protocol v1 its network traffic is keyed post-quantum too. This page covers the validator p2p handshake (CRY-1 / DEC-9) and the node's HTTPS.

| Link | Key exchange | Peer authentication | Encryption |
|---|---|---|---|
| Validator ↔ validator (p2p) | **Hybrid X25519 + ML-KEM-768**, one fixed suite | ML-DSA-65 | ChaCha20-Poly1305 |
| Node → HTTPS (GitHub checks, `join`, tx forwarding, sync) | `X25519MLKEM768` offered first; X25519 if the server lacks it | WebPKI certificates (classical) | TLS 1.3 AEAD |
| Users → public RPC | Whatever the TLS edge negotiates (see [Public RPC](#public-rpc-inbound-https)) | WebPKI certificates (classical) | TLS 1.3 AEAD |

## Threat model

- **Harvest now, decrypt later.** Someone records validator traffic today and gets a quantum computer later. Before this change the only secret input to the session keys was an X25519 shared secret, which Shor's algorithm recovers from the two public keys on the wire; replaying the public transcript then gives both AEAD keys. Now the keys also depend on an ML-KEM-768 shared secret, and an attacker has to break both.
- **Active quantum attacker.** Peers authenticate with ML-DSA-65 over the whole transcript, so nobody can impersonate a validator or sit in the middle, now or retroactively. That was already true.
- **What's at stake on p2p.** Votes, certificates and blocks are public anyway, and validator addresses are in genesis. What recorded traffic reveals is mempool gossip: transaction bytes before inclusion and which validator relayed what, when. It's metadata, not keys. The larger exposure is the user → RPC link, which ties a user's IP to their transactions (see [HTTPS](#https)).
- **A floor, not a cliff.** If ML-KEM-768 or the `ml-kem` crate turns out broken, confidentiality falls back to X25519 alone, which is where it was before, not below.
- **Unchanged and out of scope:** handshakes reveal both identities to observers, message sizes and timing are visible (no padding), and a stolen static key exposes future sessions. These are the upstream protocol's stated limits.

## What changed

- `node/vendor/commonware-stream` is Commonware's `commonware-stream` 2026.9.0 with a hybrid handshake (`src/hybrid.rs`, about 440 lines without comments) in place of `commonware-cryptography`'s X25519 one. `node/Cargo.toml` patches it in with `[patch.crates-io]`. Provenance, the exact diff and the re-port plan are in [VENDORED.md](../node/vendor/commonware-stream/VENDORED.md); `verify-vendor.sh` re-checks them.
- `commonware-p2p` and `commonware-cryptography` are untouched. p2p only calls `encrypted::{dial, listen}` and their `Config`, `Sender` and `Receiver`, whose signatures didn't change. The AEAD, its counter nonces and the record framing are upstream's.
- New dependency: RustCrypto `ml-kem` pinned `=0.3.2` with `zeroize` (the newest release on 2026-10-14; no advisories on OSV).
- `/api/status` reports `crypto.p2p_transport: "hybrid X25519 + ML-KEM-768 key exchange, ChaCha20-Poly1305, ML-DSA-65 peer auth (_BLOCKULA_STREAM_HANDSHAKE_X25519MLKEM768_V1)"`. The node builds it from `SUITE` and `PROTOCOL`, which only the vendored `hybrid` module exports, and depends on `commonware-stream` directly: if the patch stops applying (say a `commonware-p2p` bump the vendored version no longer satisfies), the node fails to compile instead of linking the classical handshake with only a cargo warning. The test `api::tests::vendored_stream_is_linked` also fails if the lockfile holds a second, registry copy of `commonware-stream`.

## The handshake

Message count and round trips are unchanged: the dialer sends its identity, then the three handshake messages follow (1.5 round trips).

```
dialer D                                                     listener L
  pk_D ─────────────────────────────────────────────────────▶  bouncer: is pk_D a peer we accept?
  Syn    t_D ‖ ek_M (1,184 B) ‖ X_D (32 B) ‖ sig_D ──────────▶  check t_D, verify sig_D
                                                               (ct_M, ss_M) = ML-KEM.Encaps(ek_M)
                                                               ss_X = X25519(x_L, X_D)
         ◀────────── SynAck   t_L ‖ ct_M (1,088 B) ‖ X_L (32 B) ‖ sig_L ‖ conf_l2d
  check t_L, verify sig_L
  ss_M = ML-KEM.Decaps(dk_M, ct_M)
  ss_X = X25519(x_D, X_L), check conf_l2d
  Ack    conf_d2l ───────────────────────────────────────────▶  check conf_d2l
```

`ek_M`, `dk_M`, `x_D`, `x_L` are fresh per connection. Both signatures and both confirmations are over one BLAKE3 transcript:

```
T = Transcript::new("blockula/<chain_id>/p2p_STREAM", V1).fork("_BLOCKULA_STREAM_HANDSHAKE_X25519MLKEM768_V1")
T ← t_D, pk_L, pk_D, ek_M ‖ X_D           sig_D = ML-DSA-65.Sign(sk_D, T)
T ← t_L, ct_M ‖ X_L                       sig_L = ML-DSA-65.Sign(sk_L, T)
T ← ss_M ‖ ss_X                           (one 64-byte packet, ML-KEM first)
key_l2d = T.noise("cipher_l2d")           key_d2l = T.noise("cipher_d2l")
conf_l2d = T.fork("confirmation_l2d")     conf_d2l = T.fork("confirmation_d2l")
```

Sizes with the node's ML-DSA-65 identities (each frame also has a 1–2 byte length prefix):

| Frame | Before (X25519) | After (hybrid) |
|---|---|---|
| Dialer identity | 1,952 B | 1,952 B |
| `Syn` | 3,349 B | **4,533 B** |
| `SynAck` | 3,381 B | **4,469 B** |
| `Ack` | 32 B | 32 B |
| Total | 8,714 B | 10,986 B (+2,272 B, +26%) |

The whole handshake still fits in one initial TCP congestion window (10 × 1,460 B), so it shouldn't cost an extra round trip. Steady-state traffic is unchanged (same AEAD, same framing). CPU cost: about +80 µs per handshake (see [Test evidence](#test-evidence)).

### Why this construction

- **The combiner is the transcript.** Both secrets are hashed together with every public input: the namespace, both timestamps, both identities, `ek_M`, `X_D`, `ct_M`, `X_L`. Keys derived from that stay secret if either secret does; this is the standard hash-based KEM combiner (Giacon, Heuer, Poettering, "KEM Combiners", PKC 2018; Bindel et al., "Hybrid Key Encapsulation Mechanisms and Authenticated Key Exchange", PQCrypto 2019). It's the shape TLS uses for `X25519MLKEM768` (concatenate the secrets, run the key schedule over a transcript that covers both shares), and it binds more than X-Wing, which leaves `ek_M` and `ct_M` out of its hash. No new hash function enters the key schedule.
- **One suite, no negotiation.** There is nothing for an attacker to downgrade. A node on the classical transport fails closed against a hybrid one: the `Syn` layouts differ, so each side rejects the other's (a classical listener refuses the larger frame, a hybrid listener fails to decode the smaller one), and even with sizes forced equal the signatures cover transcripts with different protocol labels.
- **ML-KEM first** in each concatenation, the byte order of TLS `X25519MLKEM768` and X-Wing. The key share (1,216 B) and reply (1,120 B) have X-Wing's sizes, so moving to a standard hybrid KEM later keeps message sizes.
- **Transcript framing V1 and a new label.** V1 framing is injective for any packet history, which removes the uniqueness argument V0 needs; the label separates this protocol from the classical one cryptographically.
- **Both identities before the dialer signs** (upstream committed the dialer's own key after its signature). Commonware's next handshake version does the same.
- **Not PQ-only.** ML-KEM-768 alone (what Commonware's draft #5183 does) saves 64 bytes and drops the classical floor, while the `ml-kem` crate is unaudited. Not the `x-wing` crate either: same sizes and security here, but another KDF layer and a draft we'd have to track.

### Implementation details that matter

- **Decode-time key validation.** The listener checks `ek_M` (the FIPS 203 §7.2 modulus check) while decoding `Syn`, so a malformed key is dropped before any signature work.
- **Work before authentication is unchanged.** The listener runs the bouncer on the dialer's identity, then verifies `sig_D`, and only then encapsulates. Each side decapsulates only after verifying the other's signature.
- **Bad ciphertexts.** ML-KEM decapsulation never fails: a forged `ct_M` yields an unrelated secret (implicit rejection), and key confirmation catches it.
- **Low-order X25519 keys** from either side are rejected (the all-zero-output check upstream already had).
- **Secrets.** The ML-KEM seed and the combined secret live in `Zeroizing` buffers, the X25519 secret in Commonware's `Secret`; the expanded decapsulation key is rebuilt from the seed when needed and wiped on drop (`ml-kem`'s `zeroize` feature).
- **Randomness** all comes from the runtime context passed to `dial`/`listen`, so handshakes stay reproducible under Commonware's deterministic runtime.

## Test evidence

`cargo test --manifest-path node/Cargo.toml -p commonware-stream` runs 55 tests: upstream's 20 stream and codec tests (now over the hybrid handshake) and these:

| What | Tests |
|---|---|
| Round trips, ed25519 and ML-DSA-65 identities, handshake functions and full `dial`/`listen` over a mock channel | `round_trip_*`, `stream_round_trip_mldsa`, `random_identities_handshake` |
| Exact sizes (the table above), checked against the ML-KEM types and the classical messages | `sizes` |
| Every field of `Syn`, `SynAck` and `Ack` with bits flipped (first, middle and last byte): rejected at decode or by the handshake, never accepted | `tampered_syn_fails`, `tampered_syn_ack_fails`, `tampered_ack_fails` |
| A listener that alters its ML-KEM ciphertext and re-signs it fails key confirmation | `resigned_bad_ciphertext_fails_confirmation` |
| Seven low-order X25519 encodings, from the dialer and from the listener, each re-signed | `low_order_x25519_from_*` |
| Invalid ML-KEM keys fail decode, also through `listen` | `invalid_ml_kem_key_*` |
| Classical dialer vs hybrid listener, hybrid dialer vs classical listener: both fail; classical and hybrid `Syn`s forced into each other's layout fail on the signature | `classical_*`, `hybrid_*` |
| Wrong namespace, wrong peer, timestamps outside the window | `mismatched_namespace_fails`, `wrong_identity_fails`, `timestamps_outside_the_window_fail` |
| Random bytes of every nearby length never panic the decoders or `dial_end` | `arbitrary_bytes_never_panic` |
| **Known answer:** fixed keys, clocks and RNG streams; BLAKE3 of every handshake byte plus one encrypted message each way is pinned for ed25519 and ML-DSA-65 identities. Any change to the schema, labels, derivation or layout breaks it. | `known_answer_*` |
| ML-KEM-768 against NIST ACVP (keyGen, encapsulation, decapsulation, both key checks; ACVP-Server `975de31e`) and Wycheproof (decapsulation incl. implicit rejection and malleable ciphertexts, encapsulation-key validation incl. modulus overflow; C2SP `7a325932`) | `acvp_*`, `wycheproof_*`, `modulus_check_boundary` |
| ML-KEM-768 against aws-lc-rs (FIPS 140-3 validated): their encapsulation to our keys and ours to theirs, identical implicit-rejection outputs, the same modulus-check verdicts | `aws_lc::*` |

End to end, `scripts/local-testnet.sh` (passed 2026-10-14) runs 4 genesis validators over real TCP with this transport, onboards a fifth at an epoch boundary, restarts one, joins two full nodes (one from a snapshot), runs the SDK e2e, and checks that every node has the same block hash and state root at a common height.

Mixed network (2026-10-14): a chain summoned with 4 validators, three running this build and one a build of the parent commit (classical transport). The three finalized normally; the classical one stayed at height 1. With `BLOCKULA_LOG=commonware_p2p=debug` the hybrid listeners logged `failed to complete handshake err=UnableToDecode(Invalid("hybrid::KeyShare", ...))` for its dials, and it logged `RecvTooLarge(4533)` for theirs.

Cost, from `cargo test --manifest-path node/Cargo.toml --release -p commonware-stream handshake_cost -- --ignored --nocapture` on an Apple M4 Max (both sides' work in one thread, ML-DSA-65 identities): about 620 µs per hybrid handshake against 540 µs for the classical one, so roughly +80 µs, most of it ML-KEM key generation (done twice by the dialer, which stores only the seed), encapsulation and decapsulation. ML-DSA signing and verification dominate either way, and nothing changes after the handshake.

## HTTPS

### Outbound

`main` installs rustls's aws-lc-rs provider, built with `prefer-post-quantum`, so every HTTPS request the node makes (GitHub checks, `join` and snapshot downloads, forwarding to peers) offers `X25519MLKEM768`, then X25519, P-256 and P-384. Checked on 2026-10-14 against Cloudflare's `/cdn-cgi/trace`: `kex=X25519MLKEM768` (without the provider install, the same request got `kex=X25519`). Servers that don't support the hybrid group get X25519; `api.github.com` was one of them when measured. Server certificates are still classical, which doesn't matter for recorded-traffic attacks. Unit tests that never call `main` fall back to reqwest's ring provider.

### Public RPC (inbound HTTPS)

The node serves plain HTTP; TLS is terminated in front of it, so whether a user's connection is post-quantum depends on that edge. Measured on 2026-10-14 with a client offering `X25519MLKEM768`: the Fly.io edge in front of `blockula-testnet.fly.dev` negotiated **X25519**, while the wallet's Vercel origin negotiated `X25519MLKEM768`. Operators running a public RPC can:

1. **Use an edge that negotiates `X25519MLKEM768`.** Cloudflare's proxy and Vercel do by default for clients that offer it.
2. **Terminate TLS in a proxy they run.** Caddy (Go 1.24+ enables `X25519MLKEM768` by default), or nginx/HAProxy on OpenSSL 3.5+ (e.g. nginx `ssl_ecdh_curve X25519MLKEM768:X25519;`). On Fly this means a raw TCP service on 443 (no `tls` handler), so Fly's proxy passes the connection through instead of terminating it.
3. **Terminate TLS in the node** with rustls on the same aws-lc-rs provider. Not built yet: it needs certificate management (ACME).

Check an endpoint with OpenSSL 3.5+: `openssl s_client -connect host:443 -groups X25519MLKEM768:X25519 </dev/null 2>&1 | grep -i 'negotiated.*group'`, or fetch `/cdn-cgi/trace` behind Cloudflare.

## Operations

- **Hybrid and classical nodes can't peer.** A validator running a binary without this patch (for example a v0.4.x build) can't connect to the network: the other side drops its handshake (`unable to decode` on a hybrid listener, `recv too large` on a classical one), and it counts as offline. There is deliberately no fallback.
- **Genesis ceremony:** every operator builds or downloads the release binary and compares its hash with the published one (and `git_sha` in `/api/status`), and checks `crypto.p2p_transport` on their node before the chain starts.
- **Changing the transport later** (for example to an upstream hybrid KEM) changes the wire format, so it's a coordinated upgrade: schedule it, and restart every validator at the same halt height.
- The public testnet on v0.4.x keeps its classical transport until it's replaced by a protocol-v1 testnet, which starts hybrid from genesis.

## Upstream

Commonware is building the general version of this on `main`, after the 2026.9.0 release we pin: pluggable handshakes in p2p (#4829, merged 2026-09-18), the SAKE handshake and CUPS record layer behind public `Upgrader`/`Handshake` traits (#5018/#5020, merged 2026-09-29/30, whose description suggests "a hybrid ML-KEM exchange could ship as SAKE V2"), and a draft `Kem` trait with ML-KEM-768 (#5183, opened 2026-10-08, ML-KEM alone; a hybrid `Kem` is listed as a follow-up).

Plan: offer this hybrid (same byte order and sizes) as that follow-up on #5183, and ask that the KEM suite be committed to SAKE's transcript label and that any fallback between handshake versions (#5029) never crosses KEM suites. Once a release ships `Kem`, our fork becomes a hybrid `Kem` impl in the node and `node/vendor/commonware-stream` is deleted. The steps are in [VENDORED.md](../node/vendor/commonware-stream/VENDORED.md#re-porting-on-the-next-commonware-upgrade).

## Audit scope (SEC-1)

Add to the external audit, about 1–2 auditor-days:

- The vendored diff (`verify-vendor.sh` prints it): `src/hybrid.rs` and the three small edits to upstream files. Questions: transcript schema and order, the combiner argument, decode-time key validation, implicit rejection plus key confirmation, the X25519 checks, zeroization, RNG use, fail-closed behavior, and work done before authentication.
- Our use of `ml-kem 0.3.2`, which its README says has never been independently audited. Either the auditor covers it, or accepts the ACVP, Wycheproof and aws-lc-rs differential tests above. If the audit prefers it, Cryspen's formally verified `libcrux-ml-kem` can replace it behind the same three functions (`generate`, `encapsulate`, `decapsulate`).
- The rustls provider setup in `main` (one line).
