//! Ghost keys and addresses.
//!
//! Everything derives from one 32-byte seed. Derivations outside the circuit (the seed to `sk`,
//! the ML-KEM seed and the outgoing viewing key) use BLAKE3 in key-derivation mode with
//! versioned context strings: it's fast, has a standard KDF mode, and is what the rest of
//! Blockula uses. Derivations the circuit must recompute (`ak`, `nk`, `owner`) use the Poseidon2
//! sponge, because BLAKE3 inside a STARK over a 31-bit field costs far more constraints.
//!
//! ```text
//! seed ──BLAKE3──> sk ──P2──> ak ─┐
//!                     └─P2──> nk ─┴─P2──> owner
//! seed ──BLAKE3──> kem seed (64 B) ──ML-KEM-768──> (ek, dk)
//! seed ──BLAKE3──> ovk
//! address = owner (32 B) ‖ ek (1,184 B) = 1,216 B
//! ```
//!
//! Spending needs `sk` (the proof shows knowledge of it). The full viewing key `(ak, nk, kem
//! seed, ovk)` sees everything, including which notes are spent, but can't spend: that needs a
//! preimage of `ak`. The incoming viewing key `(owner, kem seed)` sees incoming notes only. The
//! outgoing viewing key decrypts what this wallet sent.

use ml_kem::{KeyExport, Seed, ml_kem_768};
use zeroize::Zeroize;

use crate::field::{DIGEST_BYTES, Digest, digest_from_bytes, digest_to_bytes, xof_bytes, xof_to_digest};
use crate::hash::{Domain, sponge};

/// Bytes in an ML-KEM-768 encapsulation key.
pub const EK_BYTES: usize = 1184;
/// Bytes in an ML-KEM-768 seed (the stored form of a decapsulation key).
pub const KEM_SEED_BYTES: usize = 64;
/// Bytes in an encoded address: `owner ‖ ek`.
pub const ADDRESS_BYTES: usize = DIGEST_BYTES + EK_BYTES;
/// Version byte that starts every encoded viewing key.
pub const KEY_VERSION: u8 = 1;

/// A ghost spending key: the seed everything else derives from. Session keys and Familiars
/// never get one (there is no spend limit inside the circuit).
#[derive(Clone)]
pub struct SpendingKey {
    seed: [u8; 32],
}

impl SpendingKey {
    /// Wraps a 32-byte seed (from a wallet seed or a passkey PRF, via the caller's own KDF).
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self { seed }
    }

    /// The `account`-th key of a master seed. ML-KEM keys can't be diversified, so each
    /// unlinkable address is its own account.
    pub fn from_master(master: &[u8; 32], account: u32) -> Self {
        let mut input = [0u8; 36];
        input[..32].copy_from_slice(master);
        input[32..].copy_from_slice(&account.to_le_bytes());
        Self::from_seed(xof_bytes("blockula ghost v1 account", &input))
    }

    /// The seed.
    pub fn seed(&self) -> [u8; 32] {
        self.seed
    }

    /// `sk` as 8 field elements, the circuit's secret witness.
    pub fn sk(&self) -> Digest {
        xof_to_digest("blockula ghost v1 spending key", &self.seed)
    }

    /// The owner of the key whose field elements are `sk`: `H(H_ak(sk) ‖ H_nk(sk))`.
    pub fn owner_of(sk: &Digest) -> Digest {
        let mut input = [Default::default(); 16];
        input[..8].copy_from_slice(&sponge(Domain::SpendAuth, sk));
        input[8..].copy_from_slice(&sponge(Domain::NullifierKey, sk));
        sponge(Domain::Owner, &input)
    }

    /// The full viewing key.
    pub fn full_viewing_key(&self) -> FullViewingKey {
        let sk = self.sk();
        FullViewingKey {
            ak: sponge(Domain::SpendAuth, &sk),
            nk: sponge(Domain::NullifierKey, &sk),
            kem_seed: xof_bytes("blockula ghost v1 kem seed", &self.seed),
            ovk: OutgoingViewingKey(xof_bytes("blockula ghost v1 outgoing viewing key", &self.seed)),
        }
    }

    /// This key's address.
    pub fn address(&self) -> Address {
        self.full_viewing_key().address()
    }
}

/// Sees every note to and from this key and which are spent; can't spend.
#[derive(Clone, PartialEq, Eq)]
pub struct FullViewingKey {
    ak: Digest,
    nk: Digest,
    kem_seed: [u8; KEM_SEED_BYTES],
    ovk: OutgoingViewingKey,
}

impl FullViewingKey {
    /// Encoded length: version ‖ ak ‖ nk ‖ kem seed ‖ ovk.
    pub const BYTES: usize = 1 + 2 * DIGEST_BYTES + KEM_SEED_BYTES + 32;

    /// `ak`.
    pub fn ak(&self) -> Digest {
        self.ak
    }

    /// The nullifier key.
    pub fn nk(&self) -> Digest {
        self.nk
    }

    /// `owner = H(ak ‖ nk)`. A holder can check the key matches an address.
    pub fn owner(&self) -> Digest {
        let mut input = [Default::default(); 16];
        input[..8].copy_from_slice(&self.ak);
        input[8..].copy_from_slice(&self.nk);
        sponge(Domain::Owner, &input)
    }

    /// The incoming viewing key.
    pub fn incoming(&self) -> IncomingViewingKey {
        IncomingViewingKey { owner: self.owner(), kem_seed: self.kem_seed }
    }

    /// The outgoing viewing key.
    pub fn outgoing(&self) -> OutgoingViewingKey {
        self.ovk.clone()
    }

    /// The address.
    pub fn address(&self) -> Address {
        self.incoming().address()
    }

    /// Canonical encoding.
    pub fn to_bytes(&self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        out[0] = KEY_VERSION;
        out[1..33].copy_from_slice(&digest_to_bytes(&self.ak));
        out[33..65].copy_from_slice(&digest_to_bytes(&self.nk));
        out[65..129].copy_from_slice(&self.kem_seed);
        out[129..].copy_from_slice(&self.ovk.0);
        out
    }

    /// Strict inverse of [`Self::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::BYTES || bytes[0] != KEY_VERSION {
            return None;
        }
        Some(Self {
            ak: digest_from_bytes(bytes[1..33].try_into().ok()?)?,
            nk: digest_from_bytes(bytes[33..65].try_into().ok()?)?,
            kem_seed: bytes[65..129].try_into().ok()?,
            ovk: OutgoingViewingKey(bytes[129..].try_into().ok()?),
        })
    }
}

/// Sees incoming notes (value, memo), not spends.
#[derive(Clone, PartialEq, Eq)]
pub struct IncomingViewingKey {
    owner: Digest,
    kem_seed: [u8; KEM_SEED_BYTES],
}

impl IncomingViewingKey {
    /// Encoded length: version ‖ owner ‖ kem seed.
    pub const BYTES: usize = 1 + DIGEST_BYTES + KEM_SEED_BYTES;

    /// The owner every incoming note commits to.
    pub fn owner(&self) -> Digest {
        self.owner
    }

    /// The ML-KEM-768 decapsulation key. Crate-private: `ml-kem` derives `Debug` for it, which
    /// prints the secret key, so it is only handed out inside [`crate::encryption::TrialDecryptor`].
    pub(crate) fn decapsulation_key(&self) -> ml_kem_768::DecapsulationKey {
        ml_kem_768::DecapsulationKey::from_seed(Seed::from(self.kem_seed))
    }

    /// The address.
    pub fn address(&self) -> Address {
        let ek = self.decapsulation_key().encapsulation_key().to_bytes();
        Address { owner: self.owner, ek: ek.as_slice().try_into().expect("1,184-byte key") }
    }

    /// Canonical encoding.
    pub fn to_bytes(&self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        out[0] = KEY_VERSION;
        out[1..33].copy_from_slice(&digest_to_bytes(&self.owner));
        out[33..].copy_from_slice(&self.kem_seed);
        out
    }

    /// Strict inverse of [`Self::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::BYTES || bytes[0] != KEY_VERSION {
            return None;
        }
        Some(Self {
            owner: digest_from_bytes(bytes[1..33].try_into().ok()?)?,
            kem_seed: bytes[33..].try_into().ok()?,
        })
    }
}

/// Decrypts the outgoing ciphertexts of notes this wallet sent.
#[derive(Clone, PartialEq, Eq)]
pub struct OutgoingViewingKey(pub [u8; 32]);

/// A ghost address: where shielded notes go. Published on-chain under a `.boo` name by
/// `SetGhostAddress`, because 1,216 bytes is unusable as text.
#[derive(Clone, PartialEq, Eq)]
pub struct Address {
    owner: Digest,
    ek: [u8; EK_BYTES],
}

impl Address {
    /// The owner notes to this address commit to.
    pub fn owner(&self) -> Digest {
        self.owner
    }

    /// The ML-KEM-768 encapsulation key.
    pub fn encapsulation_key(&self) -> ml_kem_768::EncapsulationKey {
        let key = ml_kem::kem::Key::<ml_kem_768::EncapsulationKey>::try_from(&self.ek[..])
            .expect("1,184 bytes");
        ml_kem_768::EncapsulationKey::new(&key).expect("validated on decode")
    }

    /// `owner ‖ ek`, 1,216 bytes.
    pub fn to_bytes(&self) -> [u8; ADDRESS_BYTES] {
        let mut out = [0u8; ADDRESS_BYTES];
        out[..DIGEST_BYTES].copy_from_slice(&digest_to_bytes(&self.owner));
        out[DIGEST_BYTES..].copy_from_slice(&self.ek);
        out
    }

    /// Strict inverse of [`Self::to_bytes`]: canonical owner, and an encapsulation key that
    /// passes FIPS 203 validation.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != ADDRESS_BYTES {
            return None;
        }
        let owner = digest_from_bytes(bytes[..DIGEST_BYTES].try_into().ok()?)?;
        let ek: [u8; EK_BYTES] = bytes[DIGEST_BYTES..].try_into().ok()?;
        let key = ml_kem::kem::Key::<ml_kem_768::EncapsulationKey>::try_from(&ek[..]).ok()?;
        let parsed = ml_kem_768::EncapsulationKey::new(&key).ok()?;
        // Reject non-canonical encodings of the same key.
        if parsed.to_bytes().as_slice() != ek {
            return None;
        }
        Some(Self { owner, ek })
    }

    /// A short fingerprint for people to compare: 80 bits of BLAKE3, as 5 groups of 4 hex
    /// digits.
    pub fn fingerprint(&self) -> String {
        let h: [u8; 10] = xof_bytes("blockula ghost v1 address fingerprint", &self.to_bytes());
        h.chunks(2).map(|c| format!("{:02x}{:02x}", c[0], c[1])).collect::<Vec<_>>().join("-")
    }
}

impl core::fmt::Debug for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Address({})", self.fingerprint())
    }
}

redacted_debug!(SpendingKey, FullViewingKey, IncomingViewingKey, OutgoingViewingKey);

// Each key wipes its own secret bytes when dropped (best effort: copies a caller makes with
// `seed()`, `Clone` or `.0`, and the derived field elements `sk`, `ak`, `nk`, aren't wiped;
// docs/GHOST-MODE.md section 12).
impl Drop for SpendingKey {
    fn drop(&mut self) {
        self.seed.zeroize();
    }
}

impl Drop for FullViewingKey {
    fn drop(&mut self) {
        // `ovk` wipes itself.
        self.kem_seed.zeroize();
    }
}

impl Drop for IncomingViewingKey {
    fn drop(&mut self) {
        self.kem_seed.zeroize();
    }
}

impl Drop for OutgoingViewingKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_round_trip() {
        let sk = SpendingKey::from_seed([7; 32]);
        let addr = sk.address();
        assert_eq!(addr.to_bytes().len(), 1216);
        assert_eq!(Address::from_bytes(&addr.to_bytes()), Some(addr.clone()));
        assert_eq!(addr.fingerprint().len(), 24);
        let mut bad = addr.to_bytes();
        bad[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Address::from_bytes(&bad).is_none());
        assert!(Address::from_bytes(&addr.to_bytes()[1..]).is_none());
    }

    #[test]
    fn viewing_keys_round_trip_and_agree() {
        let sk = SpendingKey::from_master(&[1; 32], 3);
        let fvk = sk.full_viewing_key();
        assert_eq!(FullViewingKey::from_bytes(&fvk.to_bytes()), Some(fvk.clone()));
        let ivk = fvk.incoming();
        assert_eq!(IncomingViewingKey::from_bytes(&ivk.to_bytes()), Some(ivk.clone()));
        assert_eq!(ivk.address(), sk.address());
        assert_eq!(ivk.owner(), fvk.owner());
        assert_ne!(SpendingKey::from_master(&[1; 32], 4).address(), sk.address());
    }
}
