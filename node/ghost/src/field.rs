//! The KoalaBear field, 8-element digests and their canonical byte encodings, and the BLAKE3
//! maps from bytes to field elements.

use p3_field::{PrimeCharacteristicRing, PrimeField32, PrimeField64};
pub use p3_koala_bear::KoalaBear as F;

/// Elements in a digest. 8 × 31 bits = 248 bits, so ~124-bit collision resistance.
pub const DIGEST_ELEMS: usize = 8;
/// A Poseidon2 digest: note commitments, nullifiers, tree nodes, keys.
pub type Digest = [F; DIGEST_ELEMS];
/// Bytes in an encoded digest (8 little-endian u32s).
pub const DIGEST_BYTES: usize = 32;

/// The field modulus, p = 2^31 - 2^24 + 1.
pub const P: u32 = 0x7f00_0001;

/// The all-zero digest.
pub const ZERO_DIGEST: Digest = [F::ZERO; DIGEST_ELEMS];

/// Encodes a digest as 8 little-endian u32s.
pub fn digest_to_bytes(d: &Digest) -> [u8; DIGEST_BYTES] {
    let mut out = [0u8; DIGEST_BYTES];
    for (chunk, x) in out.as_chunks_mut::<4>().0.iter_mut().zip(d) {
        *chunk = x.as_canonical_u32().to_le_bytes();
    }
    out
}

/// Decodes a digest. Every element must be canonical (< p), so each digest has exactly one
/// encoding.
pub fn digest_from_bytes(bytes: &[u8; DIGEST_BYTES]) -> Option<Digest> {
    let mut out = ZERO_DIGEST;
    for (x, chunk) in out.iter_mut().zip(bytes.as_chunks::<4>().0) {
        let v = u32::from_le_bytes(*chunk);
        if v >= P {
            return None;
        }
        *x = F::new(v);
    }
    Some(out)
}

/// `n` field elements from BLAKE3 in key-derivation mode: `context` is a versioned,
/// hard-coded string, `input` the key material. Each element is a little-endian u64 of the XOF
/// output reduced mod p (statistical distance from uniform < 2^-32 per element).
pub fn xof_to_fields(context: &str, input: &[u8], n: usize) -> Vec<F> {
    let mut hasher = blake3::Hasher::new_derive_key(context);
    hasher.update(input);
    let mut reader = hasher.finalize_xof();
    (0..n)
        .map(|_| {
            let mut buf = [0u8; 8];
            reader.fill(&mut buf);
            F::from_u64(u64::from_le_bytes(buf) % u64::from(P))
        })
        .collect()
}

/// [`xof_to_fields`] for exactly one digest.
pub fn xof_to_digest(context: &str, input: &[u8]) -> Digest {
    xof_to_fields(context, input, DIGEST_ELEMS).try_into().expect("8 elements")
}

/// Bytes from BLAKE3 in key-derivation mode.
pub fn xof_bytes<const N: usize>(context: &str, input: &[u8]) -> [u8; N] {
    let mut hasher = blake3::Hasher::new_derive_key(context);
    hasher.update(input);
    let mut out = [0u8; N];
    hasher.finalize_xof().fill(&mut out);
    out
}

/// A u64 as four 16-bit limbs, least significant first. Values are carried this way in the
/// circuit because a 31-bit field can't hold a u64.
pub fn u64_to_limbs(v: u64) -> [F; 4] {
    core::array::from_fn(|i| F::from_u64((v >> (16 * i)) & 0xffff))
}

/// Canonical u64 of a field element.
pub fn to_u64(x: F) -> u64 {
    x.as_canonical_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_round_trip_and_canonical() {
        let d = xof_to_digest("blockula ghost v1 test", b"x");
        assert_eq!(digest_from_bytes(&digest_to_bytes(&d)), Some(d));
        let mut bytes = digest_to_bytes(&d);
        bytes[0..4].copy_from_slice(&P.to_le_bytes());
        assert_eq!(digest_from_bytes(&bytes), None);
        bytes[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(digest_from_bytes(&bytes), None);
    }

    #[test]
    fn limbs() {
        let v = 0x1234_5678_9abc_def0u64;
        let l = u64_to_limbs(v);
        let back: u64 = l.iter().enumerate().map(|(i, x)| to_u64(*x) << (16 * i)).sum();
        assert_eq!(back, v);
        assert_eq!(u64_to_limbs(u64::MAX), [F::from_u64(0xffff); 4]);
    }
}
