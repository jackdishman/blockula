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
