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
    // Every byte is looked at, with no early exit, which lets the loop run a vector at a time:
    // a tx's key and signature are 10,522 characters of this, checked each time it is.
    s.len().is_multiple_of(2) && s.bytes().fold(true, |ok, b| ok & (b.is_ascii_digit() | (b'a'..=b'f').contains(&b)))
}

/// The bytes of canonical lowercase hex: `Some` exactly when [`is_lower_hex`] holds, and then what
/// `hex::decode` returns. Any other string (odd length, a character outside `0-9a-f`, uppercase
/// included) is `None`, for the caller to hand to `hex::decode` if it wants that function's answer.
/// A tx pays this for its 1,952-byte key and 3,309-byte signature each time it is checked,
/// encoded or named, and one table lookup a digit is several times faster than `hex::decode`.
pub fn decode_lower_hex(s: &str) -> Option<Vec<u8>> {
    decode_lower(s.as_bytes())
}

/// [`decode_lower_hex`] on bytes, so a test can try every byte value.
fn decode_lower(s: &[u8]) -> Option<Vec<u8>> {
    /// The value of each lowercase hex digit; every other byte maps to 0xff (past any nibble).
    const NIBBLE: [u8; 256] = {
        let mut table = [0xff; 256];
        let mut i = 0;
        while i < 16 {
            table[b"0123456789abcdef"[i] as usize] = i as u8;
            i += 1;
        }
        table
    };
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let (pairs, _) = s.as_chunks::<2>();
    let mut out = vec![0u8; pairs.len()];
    // Every nibble is 0–15, so `bad` stays under 16 exactly when every character was a digit.
    let mut bad = 0u8;
    for (byte, &[high, low]) in out.iter_mut().zip(pairs) {
        let (high, low) = (NIBBLE[usize::from(high)], NIBBLE[usize::from(low)]);
        bad |= high | low;
        *byte = (high << 4) | low;
    }
    (bad < 16).then_some(out)
}

/// `hex::encode` (lowercase) of `bytes`, written into a buffer a pair of digits at a time.
pub fn encode_lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = vec![0u8; bytes.len() * 2];
    for (pair, &b) in out.as_chunks_mut::<2>().0.iter_mut().zip(bytes) {
        *pair = [DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 15)]];
    }
    String::from_utf8(out).expect("hex digits are ASCII")
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

    /// `is_lower_hex` as it was before it became a `fold` (it was `all`, which stops at the
    /// first bad byte), word for word.
    #[allow(clippy::manual_is_multiple_of)]
    fn before_is_lower_hex(s: &str) -> bool {
        s.len() % 2 == 0 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    /// What the hex helpers must say for `s`, from `hex` and nothing of ours: the bytes if every
    /// byte is a lowercase hex digit and there is an even number of them, else `None`.
    fn oracle(s: &[u8]) -> Option<Vec<u8>> {
        (s.len().is_multiple_of(2) && s.iter().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))).then(|| hex::decode(s).unwrap())
    }

    /// Both bytes of every pair, all 65,536 of them (so every byte value in the high and the low
    /// place), and every length of one, are all the table decoder's behaviour depends on: it
    /// looks at each pair alone, and at the length's parity.
    #[test]
    fn decode_lower_hex_is_hex_decode_for_lowercase_hex_and_nothing_else_on_every_pair_of_bytes() {
        assert_eq!(decode_lower(b""), Some(vec![]));
        for a in 0..=255u8 {
            assert_eq!(decode_lower(&[a]), None, "an odd number of characters ({a:#x})");
            for b in 0..=255u8 {
                assert_eq!(decode_lower(&[a, b]), oracle(&[a, b]), "{a:#x} {b:#x}");
            }
        }
        // Several pairs, one wrong anywhere: the bad nibble is never lost in the `bad` accumulator.
        for bad in [0u8, b'G', b'A', b'/', b':', b'`', b'g', 0x80, 0xff] {
            for at in 0..16 {
                let mut s = *b"0123456789abcdef";
                s[at] = bad;
                assert_eq!(decode_lower(&s), oracle(&s), "{bad:#x} at {at}");
            }
        }
    }

    /// Whole strings, as `&str`: every character the language has, in the position of a digit
    /// (all the multi-byte forms among them), on both sides of a good one; and the length checks.
    #[test]
    fn the_hex_checks_agree_with_the_old_ones_and_with_hex_on_every_character() {
        for c in (0..=char::MAX as u32).filter_map(char::from_u32) {
            for s in [format!("{c}0"), format!("0{c}"), format!("a{c}f0"), c.to_string()] {
                assert_eq!(is_lower_hex(&s), before_is_lower_hex(&s), "{c:?} in {s:?}");
                assert_eq!(decode_lower_hex(&s), oracle(s.as_bytes()), "{c:?} in {s:?}");
            }
        }
        for n in 0..40 {
            let (s, odd) = ("0f".repeat(n), format!("{}a", "0f".repeat(n)));
            assert!(is_lower_hex(&s) && !is_lower_hex(&odd));
            assert_eq!(decode_lower_hex(&s), Some(vec![0x0f; n]));
            assert_eq!(decode_lower_hex(&odd), None);
        }
        assert!(is_lower_hex("") && decode_lower_hex("") == Some(vec![]));
    }

    /// `encode_lower_hex` is `hex::encode`: every byte in each place of a pair, and a longer input
    /// decoded back.
    #[test]
    fn encode_lower_hex_is_hex_encode() {
        assert_eq!(encode_lower_hex(&[]), "");
        for a in 0..=255u8 {
            assert_eq!(encode_lower_hex(&[a]), hex::encode([a]));
            for b in 0..=255u8 {
                assert_eq!(encode_lower_hex(&[a, b]), hex::encode([a, b]));
            }
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(encode_lower_hex(&all), hex::encode(&all));
        assert_eq!(decode_lower_hex(&encode_lower_hex(&all)), Some(all));
    }

    proptest::proptest! {
        /// Strings over an alphabet that has every kind of mistake (both cases, a character past
        /// `f`, a non-ASCII one, a space), up to 70 characters (odd lengths and even).
        #[test]
        fn the_hex_checks_agree_with_the_old_ones_and_with_hex_on_any_string(s in "[0-9a-fA-Fg é-]{0,70}") {
            proptest::prop_assert_eq!(is_lower_hex(&s), before_is_lower_hex(&s));
            proptest::prop_assert_eq!(decode_lower_hex(&s), oracle(s.as_bytes()));
        }

        /// Hex of any bytes (as long as a signature), and the same with one character changed.
        #[test]
        fn encode_then_decode_is_the_identity(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..3400), at in proptest::prelude::any::<proptest::sample::Index>()) {
            let hex = encode_lower_hex(&bytes);
            proptest::prop_assert_eq!(&hex, &hex::encode(&bytes));
            proptest::prop_assert_eq!(decode_lower_hex(&hex), Some(bytes.clone()));
            proptest::prop_assert!(is_lower_hex(&hex));
            if !hex.is_empty() {
                let mut upper = hex.clone().into_bytes();
                let i = at.index(upper.len());
                upper[i] = upper[i].to_ascii_uppercase();
                let upper = String::from_utf8(upper).unwrap();
                proptest::prop_assert_eq!(decode_lower_hex(&upper).is_some(), upper == hex);
                proptest::prop_assert_eq!(is_lower_hex(&upper), upper == hex);
            }
        }
    }
}
