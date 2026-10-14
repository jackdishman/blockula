//! Exchange messages over arbitrary transport.
//!
//! Blockula's vendored copy of `commonware-stream` 2026.9.0: [encrypted] runs the hybrid
//! X25519 + ML-KEM-768 handshake in [hybrid] instead of X25519 alone. See `VENDORED.md`.
//!
//! # Status
//!
//! Stability varies by primitive. See [README](https://github.com/commonwarexyz/monorepo#stability) for details.

#![doc(
    html_logo_url = "https://commonware.xyz/imgs/rustdoc_logo.svg",
    html_favicon_url = "https://commonware.xyz/favicon.ico"
)]

commonware_macros::stability_scope!(BETA {
    pub mod encrypted;
    pub mod hybrid;
    pub mod utils;
});
