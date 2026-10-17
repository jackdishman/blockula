# Rust ⇄ TypeScript ML-DSA-65 vectors (SEC-2)

The node (`ml-dsa`) and the SDK (`@noble/post-quantum`) must agree on every key and every signature, or a wallet's tx is rejected (or, worse, nodes disagree). These files pin that down with the messages Blockula actually signs.

| File | Made by | Contents |
|---|---|---|
| `rust-signed.json` | `node/src/kat.rs` `write_rust_signed`, using the node's own code | Seed → public key hash, key id, and the `boo1…` and `tboo1…` (testnet) addresses for 5 seeds. Deterministic signatures from the node's signing paths over: three Tx v3 txs bound to the vectors' chain instance (a fixed `genesis_hash`, also recorded at the top level), one from a `tboo1…` account and one an `AddKey` carrying the added key's proof of possession (`Tx::sign`: signing bytes, empty context; each with its wire form, `Tx::to_bytes`), simplex notarize/finalize/nullify votes (signed by `MlDsaScheme` with the `blockula/<chain>/<genesis hash>_NOTARIZE`… namespaces: the consensus domain binds the chain instance, `consensus::namespace`), a validator PoP (`POP_CONTEXT`), a key's consent to joining an account (`key-pop`, `KEY_POP_CONTEXT`, signed by `Keys::sign_with_context`), an epoch handoff (`HANDOFF_CONTEXT`), an empty message, a 255-byte context and a 300-byte namespace (hashed to its BLAKE3 by pq.rs). |
| `ts-signed.json` | `sdk-ts/test/kat.ts --write`, using the SDK | The same seed → key id / address table, derived by the SDK. Hedged signatures (fresh randomness, as `Wallet.sign` makes them) over three txs built and signed by `Wallet.sign` (one by a wallet on a `tboo`-pinned `Client`), with their wire form (`txBytes`), an SDK-built PoP, a key consent made by `Wallet.keyGrant`, and the node's vote, handoff and edge-case messages. All four validators of the test set sign the finalize vote; the SDK-built messages also carry the SDK's deterministic signature (`sig_det`). |

Each case records `namespace` (what pq.rs is given), `ctx` (the FIPS 204 context: the namespace, or BLAKE3 of it when it's over 255 bytes), `msg` and `sig`, all hex.

## What each side asserts

- **Rust** (`cargo test --manifest-path node/Cargo.toml kat::cross`):
  - `cross_rust_signed`: the node still produces exactly `rust-signed.json`, and derives the same keys.
  - `cross_ts_signed`: every SDK signature passes the node's verifiers (`crypto::verify` for txs, `pq::PublicKey::verify`, and simplex's own `Notarize`/`Nullify`/`Finalize::verify`) and fails under another namespace. The node reproduces the SDK's deterministic signatures, and the SDK's keys match the node's. Three of the SDK-signed finalize votes assemble into a `Finalization` certificate that verifies, also after an encode/decode round trip; two votes are refused as short of a quorum.
  - `cross_formats_are_current`: the recorded messages are still the node's encodings. Both files' txs parse, pass `check_format`, `check_prefix` and `verify_signature`, their signing bytes match, and their recorded wire form is `Tx::to_bytes` and parses back with `Tx::from_bytes`. The PoP, key-consent (and each AddKey's `verify_key_pop`), handoff and vote messages match too.
- **TypeScript** (`npm run test:kat -w @blockula/sdk`): noble accepts every node signature, rejects it under another context, and reproduces it byte for byte with `extraEntropy: false`. `Wallet.fromSeed` derives the node's key ids and addresses under both prefixes. The SDK's `signingBytes`, `txBytes`, `popMessage` and `keyPopMessage` encode both files' txs, PoPs and key consents to the recorded bytes, and `verifyKeyGrant` accepts the consents.

The signature checks run on the recorded bytes, so they keep passing when a message format changes. The format checks fail instead, with a message pointing here.

## Regenerating

Regenerate when a signed format changes on purpose (for example Tx v3), so the cross-checks cover the current format. Run from the repo root:

```sh
sh node/testdata/cross/generate.sh
```

This runs, in order: `cargo test --manifest-path node/Cargo.toml kat::write_rust_signed -- --ignored`, then `npm run test:kat -w @blockula/sdk -- --write` (the SDK signs the node's messages, so Rust goes first), then both test suites. The tx actions and PoP parameters are in `rust_signed()` (`node/src/kat.rs`) and `tsSigned()` (`sdk-ts/test/kat.ts`). If the tx JSON shape changes, update both before regenerating.

Deterministic parts are reproducible byte for byte. Hedged SDK signatures change on every run, so a regenerated `ts-signed.json` always shows a diff.
