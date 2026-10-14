//! The Action circuit: one STARK proves two spends and two outputs (either may be a dummy).
//!
//! Statement, for public `(anchor, nf[2], cm[2], value_balance, sighash, circuit id)`: the
//! prover knows, for each spend, `sk`, a note `(value, rseed)` and a path such that
//!
//! * C2 `owner = H(H(sk) ‖ H'(sk))` (ak, nk), so the note belongs to the prover's key;
//! * C3 `cm = H(owner ‖ value ‖ rcm)`;
//! * C4 `cm` is in the tree with root `anchor`, unless the spend is a dummy;
//! * C5 `nf = H(nk ‖ cm ‖ position)`;
//! * C6 values are u64 (four range-checked 16-bit limbs), and a dummy spends 0;
//!
//! and for each output a note whose commitment is the public `cm` (C8), with C7
//! `Σ spent = Σ output + value_balance` over the integers. C9: the sighash is a public value, so
//! the proof only verifies for this transaction.
//!
//! The node checks the rest: `nf[0] ≠ nf[1]` (also enforced by [`verify`]), nullifiers unspent,
//! anchor recent, the turnstile.

pub mod air;
pub mod config;
pub mod layout;
mod trace;

#[cfg(test)]
mod tests;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;

use p3_air::check_all_constraints;
use p3_field::PrimeCharacteristicRing;
use p3_uni_stark::Proof;
use rand::rngs::{StdRng, SysRng};
use rand::{CryptoRng, Rng, SeedableRng};

use crate::field::{Digest, F, u64_to_limbs, xof_to_digest};
use crate::hash::{Domain, sponge};
use crate::keys::SpendingKey;
use crate::note::{Note, nullifier};
use crate::tree::{DEPTH, MerklePath};
use air::ActionAir;
use config::{Config, config};
use layout::{LOG_HEIGHT, pv};

/// Version of the statement, layout and proof parameters. Any change to them bumps it; the
/// node pins the version it accepts (a governance-scheduled upgrade).
pub const CIRCUIT_ID: u32 = 1;

/// Largest proof [`verify`] will look at.
pub const MAX_PROOF_BYTES: usize = 192 * 1024;

/// Value that leaves (positive) or enters (negative) the shielded pool: `Σ spent − Σ output`.
/// Transfers have the fee here, unshields amount + fee, shields −amount.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ValueBalance {
    negative: bool,
    magnitude: u64,
}

impl ValueBalance {
    /// Zero.
    pub const ZERO: Self = Self { negative: false, magnitude: 0 };

    /// `None` unless `|v| < 2^64`.
    pub fn new(v: i128) -> Option<Self> {
        let magnitude = u64::try_from(v.unsigned_abs()).ok()?;
        Some(Self { negative: v < 0 && magnitude != 0, magnitude })
    }

    /// As an integer.
    pub fn value(&self) -> i128 {
        let m = i128::from(self.magnitude);
        if self.negative { -m } else { m }
    }

    /// True if value enters the pool.
    pub fn is_negative(&self) -> bool {
        self.negative
    }

    /// |value|.
    pub fn magnitude(&self) -> u64 {
        self.magnitude
    }
}

/// What one spend proves knowledge of.
#[derive(Clone, Debug)]
pub struct SpendWitness {
    /// The spending key's field elements ([`SpendingKey::sk`]).
    pub sk: Digest,
    /// The note. Its `owner` is ignored: the circuit derives the owner from `sk`.
    pub note: Note,
    /// Position and siblings of the note's commitment.
    pub path: MerklePath,
    /// A dummy spends nothing: value 0, no membership check.
    pub dummy: bool,
}

impl SpendWitness {
    /// Spends `note` (owned by `key`) found at `path`.
    pub fn new(key: &SpendingKey, note: Note, path: MerklePath) -> Self {
        Self { sk: key.sk(), note, path, dummy: false }
    }

    /// A dummy spend with a random key, randomness and path, so its nullifier looks like any
    /// other.
    pub fn dummy<R: CryptoRng + ?Sized>(rng: &mut R) -> Self {
        let key = SpendingKey::from_seed(random_bytes(rng));
        let note = Note { value: 0, owner: key.full_viewing_key().owner(), rseed: random_bytes(rng) };
        let siblings = core::array::from_fn(|_| xof_to_digest("blockula ghost v1 dummy path", &random_bytes::<_, 32>(rng)));
        let path = MerklePath { position: rng.next_u32(), siblings };
        Self { sk: key.sk(), note, path, dummy: true }
    }

    /// The nullifier this spend reveals.
    pub fn nullifier(&self) -> Digest {
        let fvk_nk = sponge(Domain::NullifierKey, &self.sk);
        let ak = sponge(Domain::SpendAuth, &self.sk);
        let mut own = [F::ZERO; 16];
        own[..8].copy_from_slice(&ak);
        own[8..].copy_from_slice(&fvk_nk);
        let note = Note { owner: sponge(Domain::Owner, &own), ..self.note };
        nullifier(&fvk_nk, &note.commitment(), self.path.position)
    }
}

fn random_bytes<R: CryptoRng + ?Sized, const N: usize>(rng: &mut R) -> [u8; N] {
    let mut out = [0u8; N];
    rng.fill_bytes(&mut out);
    out
}

/// Everything the prover knows.
#[derive(Clone, Debug)]
pub struct ActionWitness {
    /// Exactly two spends (pad with [`SpendWitness::dummy`]).
    pub spends: [SpendWitness; 2],
    /// Exactly two outputs (pad with zero-value notes to a random owner).
    pub outputs: [Note; 2],
    /// The tree root the real spends are in.
    pub anchor: Digest,
    /// The transaction's sighash (computed by the node from everything else in the tx).
    pub sighash: [u8; 32],
}

/// The public statement of one Action proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicInputs {
    /// Commitment-tree root.
    pub anchor: Digest,
    /// Spend nullifiers.
    pub nullifiers: [Digest; 2],
    /// Output note commitments.
    pub commitments: [Digest; 2],
    /// `Σ spent − Σ output`.
    pub value_balance: ValueBalance,
    /// The transaction's sighash.
    pub sighash: [u8; 32],
}

/// The sighash as 8 field elements: BLAKE3-KDF of the 32-byte sighash.
pub fn sighash_fields(sighash: &[u8; 32]) -> Digest {
    xof_to_digest("blockula ghost v1 sighash", sighash)
}

impl PublicInputs {
    /// The public values in circuit order (see [`layout::pv`]).
    pub fn to_fields(&self) -> Vec<F> {
        let mut v = vec![F::ZERO; pv::COUNT];
        v[pv::ANCHOR..pv::ANCHOR + 8].copy_from_slice(&self.anchor);
        for i in 0..2 {
            v[pv::NF + 8 * i..pv::NF + 8 * i + 8].copy_from_slice(&self.nullifiers[i]);
            v[pv::CM + 8 * i..pv::CM + 8 * i + 8].copy_from_slice(&self.commitments[i]);
        }
        v[pv::VB_NEG] = F::from_bool(self.value_balance.is_negative());
        v[pv::VB..pv::VB + 4].copy_from_slice(&u64_to_limbs(self.value_balance.magnitude()));
        v[pv::SIGHASH..pv::SIGHASH + 8].copy_from_slice(&sighash_fields(&self.sighash));
        v[pv::CIRCUIT] = F::from_u32(CIRCUIT_ID);
        v
    }
}

/// Why a proof wasn't produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProveError {
    /// `|Σ spent − Σ output| ≥ 2^64`.
    ValueBalanceOutOfRange,
    /// Both spends have the same nullifier (the same note twice).
    DuplicateNullifier,
    /// The witness doesn't satisfy the circuit (wrong path, wrong key, dummy with value, ...).
    Unsatisfied(String),
    /// The proof system failed (e.g. the hiding budget).
    Prover(String),
}

/// Why a proof was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// Longer than [`MAX_PROOF_BYTES`].
    TooLarge,
    /// The two nullifiers are equal.
    DuplicateNullifier,
    /// Not a canonical encoding of a proof of the expected shape.
    Malformed,
    /// A well-formed proof that doesn't verify.
    Invalid,
    /// The verifier panicked (caught; treated as invalid).
    Panicked,
}

fn action_air() -> &'static ActionAir {
    static AIR: OnceLock<ActionAir> = OnceLock::new();
    AIR.get_or_init(ActionAir::default)
}

/// Checks the witness against every constraint and returns the statement it proves.
pub fn check_witness(w: &ActionWitness) -> Result<PublicInputs, ProveError> {
    let built = trace::build(w, None);
    let vb = ValueBalance::new(built.balance).ok_or(ProveError::ValueBalanceOutOfRange)?;
    let public = trace::public_inputs(w, &built, vb);
    if public.nullifiers[0] == public.nullifiers[1] {
        return Err(ProveError::DuplicateNullifier);
    }
    let report = check_all_constraints(action_air(), &built.trace, &public.to_fields(), Some(4));
    if !report.is_ok() {
        let rows: Vec<String> = report.failures.iter().map(|f| format!("row {} {}", f.row, f)).collect();
        return Err(ProveError::Unsatisfied(rows.join(", ")));
    }
    Ok(public)
}

/// Proves an action with fresh OS randomness for the zero-knowledge masks.
pub fn prove(w: &ActionWitness) -> Result<(PublicInputs, Vec<u8>), ProveError> {
    let mut seed = [0u8; 32];
    StdRng::try_from_rng(&mut SysRng)
        .map_err(|e| ProveError::Prover(format!("os randomness: {e}")))?
        .fill_bytes(&mut seed);
    prove_with_seed(w, seed)
}

/// Proves with the masks drawn from `seed`. Only for reproducible tests: reusing a seed for two
/// different witnesses can leak them.
pub fn prove_with_seed(w: &ActionWitness, seed: [u8; 32]) -> Result<(PublicInputs, Vec<u8>), ProveError> {
    let public = check_witness(w)?;
    let built = trace::build(w, Some(public.value_balance));
    let proof = p3_uni_stark::prove(&config(seed), action_air(), built.trace, &public.to_fields())
        .map_err(|e| ProveError::Prover(format!("{e:?}")))?;
    let bytes = postcard::to_allocvec(&proof).map_err(|e| ProveError::Prover(e.to_string()))?;
    Ok((public, bytes))
}

/// Verifies an Action proof. Never panics: decoding is strict and canonical, and the Plonky3
/// verifier (which "may panic on certain malformed proofs") runs under `catch_unwind`.
pub fn verify(public: &PublicInputs, proof: &[u8]) -> Result<(), VerifyError> {
    if proof.len() > MAX_PROOF_BYTES {
        return Err(VerifyError::TooLarge);
    }
    if public.nullifiers[0] == public.nullifiers[1] {
        return Err(VerifyError::DuplicateNullifier);
    }
    let fields = public.to_fields();
    catch_unwind(AssertUnwindSafe(|| verify_fields(&fields, proof))).unwrap_or(Err(VerifyError::Panicked))
}

/// Decodes a proof: postcard, canonical (re-encodes to the same bytes), the expected height.
pub fn decode_proof(bytes: &[u8]) -> Result<Proof<Config>, VerifyError> {
    let proof: Proof<Config> = postcard::from_bytes(bytes).map_err(|_| VerifyError::Malformed)?;
    if postcard::to_allocvec(&proof).ok().as_deref() != Some(bytes) {
        return Err(VerifyError::Malformed);
    }
    // Hiding doubles the committed height.
    if proof.degree_bits != LOG_HEIGHT + 1 {
        return Err(VerifyError::Malformed);
    }
    Ok(proof)
}

fn verify_fields(fields: &[F], bytes: &[u8]) -> Result<(), VerifyError> {
    let proof = decode_proof(bytes)?;
    p3_uni_stark::verify(&config([0; 32]), action_air(), &proof, fields).map_err(|_| VerifyError::Invalid)
}

/// The tree depth the circuit proves membership at.
pub const MERKLE_DEPTH: usize = DEPTH;
