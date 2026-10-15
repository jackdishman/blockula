//! Notes, note commitments and nullifiers.

use crate::field::{Digest, F, u64_to_limbs, xof_to_digest};
use crate::hash::{Domain, sponge};

/// The note format this crate makes and accepts. It selects the commitment domain.
pub const NOTE_VERSION: u8 = 1;

/// A shielded note: `value` drops owned by `owner`, hidden by randomness derived from `rseed`.
/// Its contents are what the pool hides, so `Debug` prints `Note(..)`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Note {
    /// Amount in drops.
    pub value: u64,
    /// The recipient's `owner` (from their address).
    pub owner: Digest,
    /// Fresh randomness; the commitment's `rcm` derives from it.
    pub rseed: [u8; 32],
}

redacted_debug!(Note);

impl Note {
    /// The commitment randomness: 8 field elements (~248 bits) from BLAKE3 of `rseed`.
    pub fn rcm(&self) -> Digest {
        xof_to_digest("blockula ghost v1 note rcm", &self.rseed)
    }

    /// The value as four 16-bit limbs.
    pub fn limbs(&self) -> [F; 4] {
        u64_to_limbs(self.value)
    }

    /// The 20-element commitment input: `owner ‖ limbs ‖ rcm`.
    pub fn commitment_input(&self) -> [F; 20] {
        let mut input = [F::default(); 20];
        input[..8].copy_from_slice(&self.owner);
        input[8..12].copy_from_slice(&self.limbs());
        input[12..].copy_from_slice(&self.rcm());
        input
    }

    /// The note commitment `cm`, a leaf of the commitment tree.
    pub fn commitment(&self) -> Digest {
        sponge(Domain::NoteCommitment, &self.commitment_input())
    }
}

/// The position as two 16-bit halves, as the circuit carries it (2^32 doesn't fit the field).
pub fn position_halves(position: u32) -> [F; 2] {
    [F::new(position & 0xffff), F::new(position >> 16)]
}

/// The 18-element nullifier input: `nk ‖ cm ‖ position lo ‖ position hi`.
pub fn nullifier_input(nk: &Digest, cm: &Digest, position: u32) -> [F; 18] {
    let mut input = [F::default(); 18];
    input[..8].copy_from_slice(nk);
    input[8..16].copy_from_slice(cm);
    input[16..].copy_from_slice(&position_halves(position));
    input
}

/// The nullifier of the note `cm` at `position`. Binding the position means two notes with the
/// same commitment (a sender can make those) still have different nullifiers, so both stay
/// spendable ("faerie gold").
pub fn nullifier(nk: &Digest, cm: &Digest, position: u32) -> Digest {
    sponge(Domain::Nullifier, &nullifier_input(nk, cm, position))
}
