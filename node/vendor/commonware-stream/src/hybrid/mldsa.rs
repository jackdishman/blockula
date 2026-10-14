//! Test-only ML-DSA-65 identity keys, the same construction as Blockula's `node/src/pq.rs`
//! (deterministic FIPS 204 signing, the Commonware namespace as the context string). They give
//! the handshake tests the real message sizes and signing path the node uses.

use bytes::{Buf, BufMut};
use commonware_codec::{Error as CodecError, FixedSize, Read, Write};
use commonware_math::algebra::Random;
use ml_dsa::{
    EncodedSignature, EncodedVerifyingKey, MlDsa65, Seed, Signature as MlSig, SigningKey,
    VerifyingKey, signature::Keypair,
};
use rand_core::CryptoRng;
use std::{
    fmt,
    hash::{Hash, Hasher},
    ops::Deref,
    sync::Arc,
};

pub const PK_LEN: usize = 1952;
pub const SIG_LEN: usize = 3309;

macro_rules! bytes_newtype {
    ($t:ident, $len:expr) => {
        impl PartialEq for $t {
            fn eq(&self, o: &Self) -> bool {
                self.raw == o.raw
            }
        }
        impl Eq for $t {}
        impl PartialOrd for $t {
            fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(o))
            }
        }
        impl Ord for $t {
            fn cmp(&self, o: &Self) -> std::cmp::Ordering {
                self.raw.cmp(&o.raw)
            }
        }
        impl Hash for $t {
            fn hash<H: Hasher>(&self, h: &mut H) {
                self.raw.hash(h)
            }
        }
        impl fmt::Debug for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", commonware_formatting::hex(&self.raw[..6]))
            }
        }
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Debug::fmt(self, f)
            }
        }
        impl AsRef<[u8]> for $t {
            fn as_ref(&self) -> &[u8] {
                &self.raw[..]
            }
        }
        impl Deref for $t {
            type Target = [u8];
            fn deref(&self) -> &[u8] {
                &self.raw[..]
            }
        }
        impl FixedSize for $t {
            const SIZE: usize = $len;
        }
        impl Write for $t {
            fn write(&self, buf: &mut impl BufMut) {
                buf.put_slice(&self.raw[..]);
            }
        }
        impl commonware_utils::Span for $t {}
        impl commonware_utils::Array for $t {}
    };
}

fn read_raw<const N: usize>(buf: &mut impl Buf) -> Result<Box<[u8; N]>, CodecError> {
    if buf.remaining() < N {
        return Err(CodecError::EndOfBuffer);
    }
    let mut raw = Box::new([0u8; N]);
    buf.copy_to_slice(&mut raw[..]);
    Ok(raw)
}

/// An ML-DSA-65 verifying key.
#[derive(Clone)]
pub struct PublicKey {
    raw: Box<[u8; PK_LEN]>,
    vk: Arc<VerifyingKey<MlDsa65>>,
}
bytes_newtype!(PublicKey, PK_LEN);

impl Read for PublicKey {
    type Cfg = ();
    fn read_cfg(buf: &mut impl Buf, _: &()) -> Result<Self, CodecError> {
        let raw = read_raw::<PK_LEN>(buf)?;
        let enc = EncodedVerifyingKey::<MlDsa65>::try_from(&raw[..])
            .map_err(|_| CodecError::Invalid("mldsa65::PublicKey", "length"))?;
        let vk = Arc::new(VerifyingKey::<MlDsa65>::decode(&enc));
        Ok(Self { raw, vk })
    }
}

impl commonware_cryptography::Verifier for PublicKey {
    type Signature = Signature;
    fn verify(&self, namespace: &[u8], msg: &[u8], sig: &Signature) -> bool {
        self.vk.verify_with_context(msg, namespace, &sig.sig)
    }
}
impl commonware_cryptography::PublicKey for PublicKey {}

/// An ML-DSA-65 signature.
#[derive(Clone)]
pub struct Signature {
    raw: Box<[u8; SIG_LEN]>,
    sig: MlSig<MlDsa65>,
}
bytes_newtype!(Signature, SIG_LEN);

impl Read for Signature {
    type Cfg = ();
    fn read_cfg(buf: &mut impl Buf, _: &()) -> Result<Self, CodecError> {
        let raw = read_raw::<SIG_LEN>(buf)?;
        let enc = EncodedSignature::<MlDsa65>::try_from(&raw[..])
            .map_err(|_| CodecError::Invalid("mldsa65::Signature", "length"))?;
        let sig = MlSig::<MlDsa65>::decode(&enc)
            .ok_or(CodecError::Invalid("mldsa65::Signature", "malformed"))?;
        Ok(Self { raw, sig })
    }
}
impl commonware_cryptography::Signature for Signature {}

/// An ML-DSA-65 signing key, from a 32-byte seed.
#[derive(Clone)]
pub struct PrivateKey {
    sk: Arc<SigningKey<MlDsa65>>,
    pk: PublicKey,
}

impl PrivateKey {
    pub fn from_seed_bytes(seed: [u8; 32]) -> Self {
        let sk = SigningKey::<MlDsa65>::from_seed(&Seed::from(seed));
        let enc = sk.verifying_key().encode();
        let pk = PublicKey::read_cfg(&mut &enc[..], &()).expect("valid key");
        Self {
            sk: Arc::new(sk),
            pk,
        }
    }
}

impl Random for PrivateKey {
    fn random(mut rng: impl CryptoRng) -> Self {
        let mut seed = [0u8; 32];
        rng.fill_bytes(&mut seed);
        Self::from_seed_bytes(seed)
    }
}

impl commonware_cryptography::Signer for PrivateKey {
    type Signature = Signature;
    type PublicKey = PublicKey;
    fn public_key(&self) -> PublicKey {
        self.pk.clone()
    }
    fn sign(&self, namespace: &[u8], msg: &[u8]) -> Signature {
        let sig = self
            .sk
            .expanded_key()
            .sign_deterministic(msg, namespace)
            .expect("context <= 255 bytes");
        let mut raw = Box::new([0u8; SIG_LEN]);
        raw.copy_from_slice(&sig.encode());
        Signature { raw, sig }
    }
}
