#!/bin/sh
# Regenerates the Rust ⇄ TypeScript ML-DSA-65 vectors (see README.md), then checks both sides.
# Run from the repo root. Rust first: the SDK signs the node's messages from rust-signed.json.
set -eu
cargo test --manifest-path node/Cargo.toml kat::write_rust_signed -- --ignored
npm run test:kat -w @blockula/sdk -- --write
cargo test --manifest-path node/Cargo.toml kat::
