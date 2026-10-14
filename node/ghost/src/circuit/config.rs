//! The STARK configuration: KoalaBear with its quartic extension, Poseidon2 commitments,
//! hiding FRI. Prover and verifier must agree on every number here; changing any of them
//! changes [`super::CIRCUIT_ID`].

use p3_challenger::DuplexChallenger;
use p3_commit::ExtensionMmcs;
use p3_dft::Radix2DitParallel;
use p3_field::Field;
use p3_field::extension::BinomialExtensionField;
use p3_fri::{FriParameters, HidingFriPcs};
use p3_koala_bear::Poseidon2KoalaBear;
use p3_merkle_tree::MerkleTreeHidingMmcs;
use p3_symmetric::{PaddingFreeSponge, TruncatedPermutation};
use p3_uni_stark::StarkConfig;
use rand::SeedableRng;
use rand::rngs::StdRng;

use crate::field::F;
use crate::hash::permutation;

/// Challenge field: degree-4 extension of KoalaBear (~124 bits).
pub type Challenge = BinomialExtensionField<F, 4>;
type Perm = Poseidon2KoalaBear<16>;
type LeafHash = PaddingFreeSponge<Perm, 16, 8, 8>;
type NodeCompress = TruncatedPermutation<Perm, 2, 8, 16>;
/// Field elements of random salt per committed leaf (4 × 31 bits).
pub const SALT_ELEMS: usize = 4;
type ValMmcs = MerkleTreeHidingMmcs<
    <F as Field>::Packing,
    <F as Field>::Packing,
    LeafHash,
    NodeCompress,
    StdRng,
    2,
    8,
    SALT_ELEMS,
>;
type ChallengeMmcs = ExtensionMmcs<F, Challenge, ValMmcs>;
type Challenger = DuplexChallenger<F, Perm, 16, 8>;
type Dft = Radix2DitParallel<F>;
type Pcs = HidingFriPcs<F, Dft, ValMmcs, ChallengeMmcs, StdRng>;
/// The full proof-system configuration.
pub type Config = StarkConfig<Pcs, Challenge, Challenger>;

/// log2 of the FRI blowup (rate 1/16).
pub const LOG_BLOWUP: usize = 4;
/// FRI queries.
pub const NUM_QUERIES: usize = 48;
/// Proof-of-work bits before the query phase.
pub const QUERY_POW_BITS: usize = 16;
/// Proof-of-work bits before each FRI folding challenge.
pub const COMMIT_POW_BITS: usize = 0;
/// Proof-of-work bits before the batching challenge.
pub const BATCH_POW_BITS: usize = 16;
/// Proof-of-work bits before the out-of-domain point.
pub const OOD_POW_BITS: usize = 16;
/// log2 of the final FRI polynomial length (stopping early saves ~10% of the proof).
pub const LOG_FINAL_POLY_LEN: usize = 5;
/// log2 of the maximum FRI folding arity.
pub const MAX_LOG_ARITY: usize = 1;
/// Random codewords masking the trace (must be ≥ the extension degree).
pub const NUM_RANDOM_CODEWORDS: usize = 4;

/// The FRI parameters with the given commitment scheme.
pub fn fri_parameters<M>(mmcs: M) -> FriParameters<M> {
    FriParameters {
        log_blowup: LOG_BLOWUP,
        log_final_poly_len: LOG_FINAL_POLY_LEN,
        max_log_arity: MAX_LOG_ARITY,
        num_queries: NUM_QUERIES,
        batch_proof_of_work_bits: BATCH_POW_BITS,
        commit_proof_of_work_bits: COMMIT_POW_BITS,
        query_proof_of_work_bits: QUERY_POW_BITS,
        mmcs,
    }
}

/// A configuration whose hiding randomness (trace masks, Merkle salts) comes from `seed`. The
/// prover must use a fresh secret seed per proof; the verifier never draws randomness, so any
/// seed works there.
pub fn config(seed: [u8; 32]) -> Config {
    let mut rng = StdRng::from_seed(seed);
    let perm = permutation().clone();
    let val_mmcs = ValMmcs::new(
        LeafHash::new(perm.clone()),
        NodeCompress::new(perm.clone()),
        0,
        StdRng::from_rng(&mut rng),
    );
    let challenge_mmcs = ChallengeMmcs::new(val_mmcs.clone());
    let pcs = Pcs::new(
        Dft::default(),
        val_mmcs,
        fri_parameters(challenge_mmcs),
        NUM_RANDOM_CODEWORDS,
        StdRng::from_rng(&mut rng),
    );
    Config::new(pcs, Challenger::new(perm)).with_ood_proof_of_work_bits(OOD_POW_BITS)
}

/// Soundness of the configuration, from Plonky3's own calculator (`p3-uni-stark` `security.rs`
/// over `p3-security`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecurityReport {
    /// Proven round-by-round soundness, best of unique- and list-decoding (bits).
    pub proven_bits: usize,
    /// Proven, unique-decoding regime.
    pub proven_unique_decoding_bits: usize,
    /// Proven, list-decoding (Johnson) regime.
    pub proven_list_decoding_bits: usize,
    /// Conjectured ("random words", eprint 2025/2010).
    pub conjectured_bits: usize,
    /// Number of AIR constraints.
    pub num_constraints: usize,
    /// Maximum constraint degree.
    pub max_constraint_degree: usize,
    /// Committed quotient chunks.
    pub num_quotient_chunks: usize,
    /// Functions batched into the one FRI instance.
    pub num_batched_functions: usize,
}

/// Bits of the challenge field, rounded down: 4 × log2(p) ≈ 123.96.
pub const CHALLENGE_FIELD_BITS: usize = 123;
/// Collision resistance of the Poseidon2 commitment hash (8 × 31-bit digests).
pub const HASH_COLLISION_BITS: usize = 124;

/// The security report for these parameters and the Action AIR.
pub fn security_report() -> SecurityReport {
    security_report_for(LOG_BLOWUP, NUM_QUERIES, QUERY_POW_BITS, COMMIT_POW_BITS, BATCH_POW_BITS, OOD_POW_BITS)
}

/// The report for other FRI parameters (for choosing them).
pub fn security_report_for(
    log_blowup: usize,
    num_queries: usize,
    query_pow: usize,
    commit_pow: usize,
    batch_pow: usize,
    ood_pow: usize,
) -> SecurityReport {
    use p3_field::PrimeCharacteristicRing;
    use p3_field::coset::TwoAdicMultiplicativeCoset;
    use p3_security::fri::FriRegime;
    use p3_uni_stark::{AirLayout, ConjecturedSecurity, GrindingSites, OpeningShape, ProvenSecurity, StarkSecurityParams};

    let air = super::air::ActionAir::default();
    let fri = FriRegime {
        log_blowup,
        num_queries,
        log_final_poly_len: LOG_FINAL_POLY_LEN,
        max_log_arity: MAX_LOG_ARITY,
        commit_pow_bits: commit_pow,
        query_pow_bits: query_pow,
    };
    let grinding = GrindingSites { batch_combination: batch_pow, out_of_domain: ood_pow, ..GrindingSites::NONE };
    let domain = TwoAdicMultiplicativeCoset::new(F::ONE, super::layout::LOG_HEIGHT).expect("small domain");
    let params = StarkSecurityParams::from_air::<F, Challenge, _>(
        fri,
        &air,
        AirLayout::from_air::<F>(&air),
        domain,
        CHALLENGE_FIELD_BITS,
        HASH_COLLISION_BITS,
        2,
        OpeningShape::hiding(NUM_RANDOM_CODEWORDS),
        grinding,
    );
    // Hiding doubles the committed height.
    let degree_bits = super::layout::LOG_HEIGHT + 1;
    let proven = ProvenSecurity::compute_from_proof(degree_bits, &params);
    SecurityReport {
        proven_bits: proven.security_bits(),
        proven_unique_decoding_bits: proven.unique_decoding_bits,
        proven_list_decoding_bits: proven.list_decoding_bits,
        conjectured_bits: ConjecturedSecurity::compute_from_params(&params, degree_bits).security_bits,
        num_constraints: params.num_constraints,
        max_constraint_degree: params.air_max_constraint_degree,
        num_quotient_chunks: params.num_quotient_chunks,
        num_batched_functions: params.num_batched_functions,
    }
}
