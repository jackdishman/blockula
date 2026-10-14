//! Poseidon2 hashing: the fixed-length sponge used for keys, notes and nullifiers, and the
//! 2-to-1 compression used for the commitment tree.
//!
//! The permutation is Plonky3's stock KoalaBear Poseidon2 at width 16 (8 full rounds, 20 partial
//! rounds, x^3 S-box, the Grain-LFSR round constants in `p3-koala-bear`). Whether its margin is
//! enough is an open item for the cryptanalyst sign-off (docs/GHOST-MODE.md). Every use is
//! domain-separated by a versioned IV, so a later pool can switch hashes without ambiguity.

use std::sync::OnceLock;

use p3_koala_bear::{Poseidon2KoalaBear, default_koalabear_poseidon2_16};
use p3_symmetric::Permutation;

use crate::field::{DIGEST_ELEMS, Digest, F, xof_to_digest};

/// Poseidon2 state width.
pub const WIDTH: usize = 16;
/// Sponge rate (elements absorbed per permutation).
pub const RATE: usize = 8;

/// What a sponge call hashes. Each domain has a fixed input length and its own IV, so inputs of
/// different domains or lengths never share a permutation input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    /// `ak = H(sk)`: the spend-authorizing key (8 elements in).
    SpendAuth,
    /// `nk = H(sk)`: the nullifier key (8 in).
    NullifierKey,
    /// `owner = H(ak ‖ nk)`: the address part that goes in notes (16 in).
    Owner,
    /// `cm = H(owner ‖ value limbs ‖ rcm)`: note commitment, note version 1 (20 in).
    NoteCommitment,
    /// `nf = H(nk ‖ cm ‖ position lo ‖ position hi)` (18 in).
    Nullifier,
}

impl Domain {
    /// All domains, for tests and IV tables.
    pub const ALL: [Domain; 5] = [
        Domain::SpendAuth,
        Domain::NullifierKey,
        Domain::Owner,
        Domain::NoteCommitment,
        Domain::Nullifier,
    ];

    /// Versioned name. Changing anything about a domain means a new name.
    pub const fn name(self) -> &'static str {
        match self {
            Domain::SpendAuth => "ak",
            Domain::NullifierKey => "nk",
            Domain::Owner => "owner",
            Domain::NoteCommitment => "note-commitment-v1",
            Domain::Nullifier => "nullifier",
        }
    }

    /// Input length in field elements.
    pub const fn input_len(self) -> usize {
        match self {
            Domain::SpendAuth | Domain::NullifierKey => 8,
            Domain::Owner => 16,
            Domain::NoteCommitment => 20,
            Domain::Nullifier => 18,
        }
    }

    /// Permutations per call (input zero-padded to whole chunks of [`RATE`]).
    pub const fn chunks(self) -> usize {
        self.input_len().div_ceil(RATE)
    }

    /// The capacity IV: BLAKE3-KDF("blockula ghost v1 poseidon2 sponge iv", "name/len").
    pub fn iv(self) -> Digest {
        static IVS: OnceLock<[Digest; 5]> = OnceLock::new();
        let ivs = IVS.get_or_init(|| {
            Domain::ALL.map(|d| {
                let input = format!("{}/{}", d.name(), d.input_len());
                xof_to_digest("blockula ghost v1 poseidon2 sponge iv", input.as_bytes())
            })
        });
        ivs[Domain::ALL.iter().position(|d| *d == self).expect("listed")]
    }
}

/// The width-16 KoalaBear Poseidon2 permutation.
pub fn permutation() -> &'static Poseidon2KoalaBear<WIDTH> {
    static PERM: OnceLock<Poseidon2KoalaBear<WIDTH>> = OnceLock::new();
    PERM.get_or_init(default_koalabear_poseidon2_16)
}

/// Applies the permutation.
pub fn permute(state: [F; WIDTH]) -> [F; WIDTH] {
    permutation().permute(state)
}

/// The sponge states a call goes through: the input of each permutation, in order. The circuit
/// proves exactly these permutations, so trace generation reuses this.
pub fn sponge_inputs(domain: Domain, input: &[F]) -> Vec<[F; WIDTH]> {
    assert_eq!(input.len(), domain.input_len(), "fixed-length domain {}", domain.name());
    let mut state = [F::default(); WIDTH];
    state[RATE..].copy_from_slice(&domain.iv());
    let mut inputs = Vec::with_capacity(domain.chunks());
    for chunk in input.chunks(RATE) {
        // Overwrite mode: the rate part takes the next chunk (zero-padded), the capacity carries.
        state[..RATE].fill(F::default());
        state[..chunk.len()].copy_from_slice(chunk);
        inputs.push(state);
        state = permute(state);
    }
    inputs
}

/// Fixed-length Poseidon2 sponge: IV in the capacity, overwrite absorption at rate 8, output the
/// first 8 elements of the final state.
pub fn sponge(domain: Domain, input: &[F]) -> Digest {
    let last = *sponge_inputs(domain, input).last().expect("at least one chunk");
    permute(last)[..DIGEST_ELEMS].try_into().expect("8 elements")
}

/// Commitment-tree node: `Trunc8(P(l ‖ r) + (l ‖ r))`, Poseidon2's compression mode with a
/// feed-forward, so inverting the permutation doesn't give preimages or collisions.
pub fn compress(left: &Digest, right: &Digest) -> Digest {
    let mut state = [F::default(); WIDTH];
    state[..DIGEST_ELEMS].copy_from_slice(left);
    state[DIGEST_ELEMS..].copy_from_slice(right);
    let out = permute(state);
    core::array::from_fn(|i| out[i] + left[i])
}

/// The value of an empty leaf. No note commitment can equal it without a Poseidon2 preimage.
pub fn uncommitted_leaf() -> Digest {
    static LEAF: OnceLock<Digest> = OnceLock::new();
    *LEAF.get_or_init(|| xof_to_digest("blockula ghost v1 uncommitted leaf", b""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ivs_distinct() {
        for (i, a) in Domain::ALL.iter().enumerate() {
            for b in &Domain::ALL[i + 1..] {
                assert_ne!(a.iv(), b.iv());
            }
            assert_eq!(a.iv(), a.iv());
        }
    }

    #[test]
    fn sponge_is_input_sensitive() {
        let x: Vec<F> = (0..8).map(F::new).collect();
        let a = sponge(Domain::SpendAuth, &x);
        let b = sponge(Domain::NullifierKey, &x);
        assert_ne!(a, b);
        let mut y = x.clone();
        y[7] = F::new(8);
        assert_ne!(sponge(Domain::SpendAuth, &y), a);
    }

    #[test]
    fn compress_is_ordered() {
        let a = uncommitted_leaf();
        let b = compress(&a, &a);
        assert_ne!(compress(&a, &b), compress(&b, &a));
    }
}
