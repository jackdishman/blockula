//! # Ghost Mode cryptographic core (NOT AUDITED, DISABLED AT GENESIS)
//!
//! The library behind Blockula's shielded pool: keys and addresses, notes, the Poseidon2
//! commitment tree, the Action STARK (Plonky3 0.8, KoalaBear, hiding FRI) and ML-KEM-768 note
//! encryption. The design, constraint list, parameters and measured numbers are in
//! `docs/GHOST-MODE.md`.
//!
//! A soundness bug in a shielded pool is a silent infinite mint. This code has had no external
//! review. The node must not accept ghost transactions until governance activates the feature
//! after the audit and the other gates listed in the doc.
//!
//! **Build requirements for the node** (a dependent's profile, not this crate's, applies):
//! `panic = "unwind"`, because [`verify`] turns a verifier panic on a malformed proof into an
//! error with `catch_unwind` (enforced below); and `overflow-checks = true` for this crate and
//! Plonky3, the configuration every robustness test ran in (docs/GHOST-MODE.md section 7).

// Under `panic = "abort"`, a proof that makes Plonky3's verifier panic would abort the process
// instead of being rejected: a remote crash of every node that sees it. Refuse to build that
// way. WASM (wallets only, never a node) aborts on panic by default and is exempt.
#[cfg(not(any(panic = "unwind", target_family = "wasm")))]
compile_error!("blockula-ghost needs panic = \"unwind\": `verify` catches verifier panics with catch_unwind");

/// Secrets print as their type name only: `Type(..)`. Use for anything that holds key
/// material, note plaintexts or witness data, so `{:?}` (logs, panics, `dbg!`) can't leak it.
macro_rules! redacted_debug {
    ($($t:ident),*) => {$(
        impl core::fmt::Debug for $t {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(concat!(stringify!($t), "(..)"))
            }
        }
    )*};
}

pub mod circuit;
pub mod encryption;
pub mod field;
pub mod hash;
pub mod keys;
pub mod note;
pub mod tree;

pub use circuit::{
    ActionWitness, CIRCUIT_ID, MAX_PROOF_BYTES, ProveError, PublicInputs, SpendWitness, ValueBalance,
    VerifyError, prove, verify,
};
pub use encryption::{
    EncryptedNote, TrialDecryptor, dummy_output, encrypt_note, try_decrypt_incoming, try_decrypt_outgoing,
};
pub use field::Digest;
pub use keys::{Address, FullViewingKey, IncomingViewingKey, OutgoingViewingKey, SpendingKey};
pub use note::{Note, nullifier};
pub use tree::{Frontier, FullTree, MerklePath, RootHistory};
