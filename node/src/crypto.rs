//! Post-quantum primitives: ML-DSA-65 (FIPS 204) signatures, BLAKE3 hashing,
//! bech32m `boo1…` (mainnet) and `tboo1…` (testnet) addresses. Nothing in here relies on elliptic curves.

use anyhow::{anyhow, bail, Result};
use bech32::{Bech32m, Hrp};
use ml_dsa::{
    signature::{Keypair, Signer, Verifier},
    EncodedSignature, EncodedVerifyingKey, MlDsa65, Seed, Signature, SigningKey, VerifyingKey,
};

/// Mainnet (and dev chains) use `boo`; public testnets use `tboo` (DEC-7). The prefix is a
/// genesis parameter.
pub const ADDRESS_HRP: &str = "boo";
pub const TESTNET_HRP: &str = "tboo";
/// Every prefix an address may carry. Statelessly any of them is accepted; admission requires
/// the chain's own.
pub const KNOWN_PREFIXES: [&str; 2] = [ADDRESS_HRP, TESTNET_HRP];
pub const PUBLIC_KEY_LEN: usize = 1952;
pub const SIGNATURE_LEN: usize = 3309;

pub fn hash(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

/// An address is bech32m(prefix, BLAKE3(public_key)). A full 256-bit hash keeps
/// collision resistance at ~128 bits even against quantum (BHT) attacks. This one uses `boo`
/// (tests); chain code uses [`address_with_prefix`].
#[cfg_attr(not(test), allow(dead_code))]
pub fn address_from_public_key(public_key: &[u8]) -> String {
    address_with_prefix(ADDRESS_HRP, public_key)
}

/// The address of `public_key` on a chain whose addresses use `prefix`.
pub fn address_with_prefix(prefix: &str, public_key: &[u8]) -> String {
    render_address(prefix, &hash(public_key))
}

/// `boo1…` rendering of 32 address bytes (tests; chain code uses [`render_address`]).
#[cfg(test)]
pub fn address_from_bytes(bytes: &[u8; 32]) -> String {
    render_address(ADDRESS_HRP, bytes)
}

/// bech32m rendering of 32 address bytes with an explicit prefix (a known one, see
/// [`is_prefix`]; anything else panics).
pub fn render_address(prefix: &str, bytes: &[u8; 32]) -> String {
    assert!(is_prefix(prefix), "unknown address prefix {prefix}");
    bech32::encode::<Bech32m>(Hrp::parse(prefix).unwrap(), bytes).unwrap()
}

/// Parses a canonical address with a known prefix into (prefix, bytes). Only the exact
/// rendering is accepted: uppercase or a bech32 (non-m) checksum would alias the same account.
pub fn parse_address(s: &str) -> Option<(&'static str, [u8; 32])> {
    if s.len() > 90 {
        return None;
    }
    let (hrp, data) = bech32::decode(s).ok()?;
    let prefix = KNOWN_PREFIXES.into_iter().find(|p| hrp.as_str() == *p)?;
    let bytes: [u8; 32] = data.try_into().ok()?;
    (render_address(prefix, &bytes) == s).then_some((prefix, bytes))
}

pub fn is_prefix(prefix: &str) -> bool {
    KNOWN_PREFIXES.contains(&prefix)
}

/// Which network an address prefix belongs to, for error messages.
pub fn network_of(prefix: &str) -> &'static str {
    if prefix == TESTNET_HRP { "testnet" } else { "mainnet" }
}

/// Well-known protocol accounts that no one holds a key for, rendered with the chain's prefix.
pub fn system_address(prefix: &str, label: &str) -> String {
    render_address(prefix, &hash(format!("blockula/system/{label}").as_bytes()))
}

/// The 32 bytes inside an address of any known prefix.
pub fn address_bytes(s: &str) -> Option<[u8; 32]> {
    parse_address(s).map(|(_, b)| b)
}

/// Canonical (lowercase) address with any known prefix. Bech32 also accepts all-uppercase
/// strings, which would alias accounts, so they're rejected.
pub fn is_address(s: &str) -> bool {
    parse_address(s).is_some()
}

/// Lowercase hex only (no uppercase aliases of the same bytes).
pub fn is_lower_hex(s: &str) -> bool {
    s.len() % 2 == 0 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub struct Keys {
    signing: SigningKey<MlDsa65>,
    seed: [u8; 32],
}

impl Keys {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let signing = SigningKey::<MlDsa65>::from_seed(&Seed::from(seed));
        Self { signing, seed }
    }

    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).expect("os randomness");
        Self::from_seed(seed)
    }

    /// A second handle on the same key (re-derived from the seed).
    #[cfg(test)]
    pub fn clone_keys(&self) -> Self {
        Self::from_seed(self.seed)
    }

    pub fn seed(&self) -> [u8; 32] {
        self.seed
    }

    pub fn seed_hex(&self) -> String {
        hex::encode(self.seed)
    }

    pub fn public_key(&self) -> Vec<u8> {
        self.signing.verifying_key().encode().to_vec()
    }

    /// This key's `boo1…` address (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn address(&self) -> String {
        address_from_public_key(&self.public_key())
    }

    /// This key's address on a chain whose addresses use `prefix`.
    pub fn address_with(&self, prefix: &str) -> String {
        address_with_prefix(prefix, &self.public_key())
    }

    pub fn sign(&self, msg: &[u8]) -> Vec<u8> {
        self.signing.sign(msg).encode().to_vec()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    /// A deterministic FIPS 204 signature under a context string (domain separation, at most
    /// 255 bytes), e.g. a key's proof of possession (`types::KEY_POP_CONTEXT`).
    pub fn sign_with_context(&self, context: &[u8], msg: &[u8]) -> Vec<u8> {
        self.signing.expanded_key().sign_deterministic(msg, context).expect("context <= 255 bytes").encode().to_vec()
    }
}

/// Checks a FIPS 204 signature made under `context` (see [`Keys::sign_with_context`]).
pub fn verify_with_context(public_key: &[u8], context: &[u8], msg: &[u8], signature: &[u8]) -> bool {
    if public_key.len() != PUBLIC_KEY_LEN || signature.len() != SIGNATURE_LEN {
        return false;
    }
    let Ok(vk) = EncodedVerifyingKey::<MlDsa65>::try_from(public_key) else { return false };
    let vk = VerifyingKey::<MlDsa65>::decode(&vk);
    let Ok(sig) = EncodedSignature::<MlDsa65>::try_from(signature) else { return false };
    let Some(sig) = Signature::<MlDsa65>::decode(&sig) else { return false };
    vk.verify_with_context(msg, context, &sig)
}

pub fn verify(public_key: &[u8], msg: &[u8], signature: &[u8]) -> Result<()> {
    if public_key.len() != PUBLIC_KEY_LEN {
        bail!("public key must be {PUBLIC_KEY_LEN} bytes (ML-DSA-65)");
    }
    if signature.len() != SIGNATURE_LEN {
        bail!("signature must be {SIGNATURE_LEN} bytes (ML-DSA-65)");
    }
    let vk = VerifyingKey::<MlDsa65>::decode(
        &EncodedVerifyingKey::<MlDsa65>::try_from(public_key).map_err(|_| anyhow!("bad public key"))?,
    );
    let sig = Signature::<MlDsa65>::decode(
        &EncodedSignature::<MlDsa65>::try_from(signature).map_err(|_| anyhow!("bad signature"))?,
    )
    .ok_or_else(|| anyhow!("malformed signature"))?;
    vk.verify(msg, &sig).map_err(|_| anyhow!("invalid signature"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_round_trip() {
        let k = Keys::from_seed([7u8; 32]);
        let sig = k.sign(b"hello crypt");
        verify(&k.public_key(), b"hello crypt", &sig).unwrap();
        assert!(verify(&k.public_key(), b"tampered", &sig).is_err());
        assert!(k.address().starts_with("boo1"));
        assert!(is_address(&k.address()));
    }

    #[test]
    fn prefixes_render_and_parse() {
        let k = Keys::from_seed([7u8; 32]);
        let (main, test) = (k.address(), k.address_with(TESTNET_HRP));
        assert!(test.starts_with("tboo1"));
        assert_eq!(parse_address(&main), Some((ADDRESS_HRP, hash(&k.public_key()))));
        assert_eq!(parse_address(&test), Some((TESTNET_HRP, hash(&k.public_key()))));
        assert!(is_address(&test), "any known prefix is an address, statelessly");
        assert_eq!(address_bytes(&main), address_bytes(&test));
        assert!(!is_address(&test.to_uppercase()));
        let other = bech32::encode::<Bech32m>(Hrp::parse("bc").unwrap(), &hash(b"x")).unwrap();
        assert!(!is_address(&other), "unknown prefixes aren't addresses");
        let classic = bech32::encode::<bech32::Bech32>(Hrp::parse("boo").unwrap(), &hash(&k.public_key())).unwrap();
        assert!(!is_address(&classic), "a bech32 (non-m) checksum would alias the account");
        assert_ne!(system_address(ADDRESS_HRP, "treasury"), system_address(TESTNET_HRP, "treasury"));
        // Same vectors in sdk-ts/test/e2e.ts (the SDK derives the same key from the same seed).
        let v = Keys::from_seed([4u8; 32]);
        assert_eq!(v.address(), "boo15le8r6nlt2mpl4k9df9zd5ragyw35m9hm5spjr6td8h93kdkps7qaltngu");
        assert_eq!(v.address_with(TESTNET_HRP), "tboo15le8r6nlt2mpl4k9df9zd5ragyw35m9hm5spjr6td8h93kdkps7qscv9f0");
        assert_eq!(network_of(TESTNET_HRP), "testnet");
    }
}
