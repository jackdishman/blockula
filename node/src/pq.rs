//! ML-DSA-65 plugged into Commonware: identity keys for p2p (`Signer`/`PublicKey`/`Signature`)
//! and an attributable certificate scheme for simplex consensus.
//!
//! * Signing is **deterministic** (FIPS 204 deterministic variant). Simplex compares encoded votes
//!   to detect equivocation, so a hedged signer would look like an equivocator.
//! * Commonware's namespace becomes the FIPS 204 context string (domain separation). Namespaces
//!   longer than 255 bytes are hashed down to 32 bytes first.
//! * Certificates are a signer bitmap plus one 3309-byte signature per signer (no aggregation).
//! * A validator's scheme checks every vote with its slashing protection before signing it
//!   (`protection.rs`): a refused vote isn't signed at all, and simplex treats it as not cast.

use bytes::{Buf, BufMut};
use commonware_codec::{EncodeSize, Error as CodecError, FixedSize, Read, ReadRangeExt, Write, types::lazy::Lazy};
use commonware_consensus::simplex::{scheme::Namespace, types::Subject};
use commonware_cryptography::{
    Digest, Signer as _, Verifier as _,
    certificate::{self, AssemblyError, Attestation, Namespace as _, Signers, Subject as _},
};
use commonware_math::algebra::Random;
use commonware_parallel::Strategy;
use commonware_utils::{N3f1, Participant, iter::NonEmpty, ordered::{Quorum, Set}};
use ml_dsa::{EncodedSignature, EncodedVerifyingKey, MlDsa65, Seed, Signature as MlSig, SigningKey, VerifyingKey, signature::Keypair};
use rand_core::CryptoRng;
use std::{
    fmt,
    hash::{Hash, Hasher},
    ops::Deref,
    sync::Arc,
};

pub const PK_LEN: usize = 1952;
pub const SIG_LEN: usize = 3309;

fn context_for(namespace: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if namespace.len() <= 255 { namespace.into() } else { crate::crypto::hash(namespace).to_vec().into() }
}

macro_rules! bytes_newtype_common {
    ($t:ident, $len:expr, $label:literal) => {
        impl PartialEq for $t { fn eq(&self, o: &Self) -> bool { self.raw == o.raw } }
        impl Eq for $t {}
        impl PartialOrd for $t { fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(o)) } }
        impl Ord for $t { fn cmp(&self, o: &Self) -> std::cmp::Ordering { self.raw.cmp(&o.raw) } }
        impl Hash for $t { fn hash<H: Hasher>(&self, h: &mut H) { self.raw.hash(h) } }
        impl fmt::Debug for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, concat!($label, ":{}"), hex::encode(&self.raw[..6])) }
        }
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}", hex::encode(&self.raw[..6])) }
        }
        impl AsRef<[u8]> for $t { fn as_ref(&self) -> &[u8] { &self.raw[..] } }
        impl Deref for $t { type Target = [u8]; fn deref(&self) -> &[u8] { &self.raw[..] } }
        impl FixedSize for $t { const SIZE: usize = $len; }
        impl Write for $t { fn write(&self, buf: &mut impl BufMut) { buf.put_slice(&self.raw[..]); } }
        impl commonware_utils::Span for $t {}
        impl commonware_utils::Array for $t {}
    };
}

// ---------------------------------------------------------------- PublicKey

#[derive(Clone)]
pub struct PublicKey {
    raw: Box<[u8; PK_LEN]>,
    vk: Arc<VerifyingKey<MlDsa65>>,
}
bytes_newtype_common!(PublicKey, PK_LEN, "mldsa65");

impl PublicKey {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let raw: [u8; PK_LEN] = bytes.try_into().ok()?;
        let enc = EncodedVerifyingKey::<MlDsa65>::try_from(&raw[..]).ok()?;
        Some(Self { raw: Box::new(raw), vk: Arc::new(VerifyingKey::<MlDsa65>::decode(&enc)) })
    }
    /// This key's address on a chain whose addresses use `prefix`.
    pub fn address(&self, prefix: &str) -> String {
        crate::crypto::address_with_prefix(prefix, &self.raw[..])
    }
}
impl Read for PublicKey {
    type Cfg = ();
    fn read_cfg(buf: &mut impl Buf, _: &()) -> Result<Self, CodecError> {
        if buf.remaining() < PK_LEN {
            return Err(CodecError::EndOfBuffer);
        }
        let mut raw = vec![0u8; PK_LEN];
        buf.copy_to_slice(&mut raw);
        Self::from_bytes(&raw).ok_or(CodecError::Invalid("mldsa65::PublicKey", "malformed"))
    }
}
impl commonware_cryptography::Verifier for PublicKey {
    type Signature = Signature;
    fn verify(&self, namespace: &[u8], msg: &[u8], sig: &Signature) -> bool {
        self.vk.verify_with_context(msg, &context_for(namespace), &sig.sig)
    }
}
impl commonware_cryptography::PublicKey for PublicKey {}

// ---------------------------------------------------------------- Signature

#[derive(Clone)]
pub struct Signature {
    raw: Box<[u8; SIG_LEN]>,
    sig: MlSig<MlDsa65>,
}
bytes_newtype_common!(Signature, SIG_LEN, "mldsa65sig");

impl Read for Signature {
    type Cfg = ();
    fn read_cfg(buf: &mut impl Buf, _: &()) -> Result<Self, CodecError> {
        if buf.remaining() < SIG_LEN {
            return Err(CodecError::EndOfBuffer);
        }
        let mut raw = Box::new([0u8; SIG_LEN]);
        buf.copy_to_slice(&mut raw[..]);
        let enc = EncodedSignature::<MlDsa65>::try_from(&raw[..]).map_err(|_| CodecError::Invalid("mldsa65::Signature", "length"))?;
        let sig = MlSig::<MlDsa65>::decode(&enc).ok_or(CodecError::Invalid("mldsa65::Signature", "malformed"))?;
        Ok(Self { raw, sig })
    }
}
impl commonware_cryptography::Signature for Signature {}

// ---------------------------------------------------------------- PrivateKey

#[derive(Clone)]
pub struct PrivateKey {
    seed: [u8; 32],
    sk: Arc<SigningKey<MlDsa65>>,
    pk: PublicKey,
}
impl PrivateKey {
    /// Same derivation as `crypto::Keys::from_seed`, so a validator's consensus identity and its
    /// on-chain account (`boo1…` address) come from one seed.
    pub fn from_seed_bytes(seed: [u8; 32]) -> Self {
        let sk = SigningKey::<MlDsa65>::from_seed(&Seed::from(seed));
        let pk = PublicKey::from_bytes(&sk.verifying_key().encode()).expect("valid key");
        Self { seed, sk: Arc::new(sk), pk }
    }
}
impl fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PrivateKey({:?})", self.pk)
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
        let sig = self.sk.expanded_key().sign_deterministic(msg, &context_for(namespace)).expect("context <= 255 bytes");
        let mut raw = Box::new([0u8; SIG_LEN]);
        raw.copy_from_slice(&sig.encode());
        Signature { raw, sig }
    }
}
impl FixedSize for PrivateKey {
    const SIZE: usize = 32;
}
impl Write for PrivateKey {
    fn write(&self, buf: &mut impl BufMut) {
        buf.put_slice(&self.seed);
    }
}
impl Read for PrivateKey {
    type Cfg = ();
    fn read_cfg(buf: &mut impl Buf, _: &()) -> Result<Self, CodecError> {
        if buf.remaining() < 32 {
            return Err(CodecError::EndOfBuffer);
        }
        let mut seed = [0u8; 32];
        buf.copy_to_slice(&mut seed);
        Ok(Self::from_seed_bytes(seed))
    }
}
impl commonware_cryptography::PrivateKey for PrivateKey {}

// ---------------------------------------------------------------- certificate scheme

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Certificate {
    pub signers: Signers,
    pub signatures: Vec<Lazy<Signature>>,
}
impl Write for Certificate {
    fn write(&self, w: &mut impl BufMut) {
        self.signers.write(w);
        self.signatures.write(w);
    }
}
impl EncodeSize for Certificate {
    fn encode_size(&self) -> usize {
        self.signers.encode_size() + self.signatures.encode_size()
    }
}
impl Read for Certificate {
    type Cfg = usize;
    fn read_cfg(r: &mut impl Buf, n: &usize) -> Result<Self, CodecError> {
        let signers = Signers::read_cfg(r, n)?;
        let signatures = Vec::<Lazy<Signature>>::read_range(r, ..=*n)?;
        if signers.count() == 0 || signers.count() != signatures.len() {
            return Err(CodecError::Invalid("mldsa65::Certificate", "bad signer count"));
        }
        Ok(Self { signers, signatures })
    }
}

/// Attributable ML-DSA-65 scheme for simplex (modeled on Commonware's secp256r1 scheme).
#[derive(Clone, Debug)]
pub struct MlDsaScheme {
    participants: Set<PublicKey>,
    signer: Option<(Participant, PrivateKey)>,
    namespace: Namespace,
    /// Consulted (and updated) before every signature.
    protection: Option<Arc<crate::protection::Protection>>,
}

impl MlDsaScheme {
    pub fn signer(namespace: &[u8], participants: Set<PublicKey>, key: PrivateKey) -> Option<Self> {
        let idx = participants.position(&key.public_key())?;
        Some(Self { participants, signer: Some((Participant::from_usize(idx), key)), namespace: Namespace::derive(namespace), protection: None })
    }
    pub fn verifier(namespace: &[u8], participants: Set<PublicKey>) -> Self {
        Self { participants, signer: None, namespace: Namespace::derive(namespace), protection: None }
    }
    /// Signs only what `protection` allows.
    pub fn with_protection(mut self, protection: Option<Arc<crate::protection::Protection>>) -> Self {
        self.protection = protection;
        self
    }
    fn check(&self, subject: &Subject<'_, impl Digest>, signer: Participant, sig: &Lazy<Signature>) -> bool {
        let Some(pk) = self.participants.get(usize::from(signer)) else { return false };
        let Some(sig) = sig.get() else { return false };
        pk.verify(subject.namespace(&self.namespace), &subject.message(), sig)
    }
}

impl certificate::Verifier for MlDsaScheme {
    type Subject<'a, D: Digest> = Subject<'a, D>;
    type Faults = N3f1;
    type PublicKey = PublicKey;
    type Certificate = Certificate;

    fn verify_certificate<R: CryptoRng, D: Digest>(&self, _rng: &mut R, subject: Subject<'_, D>, cert: &Certificate, _strategy: &impl Strategy) -> bool {
        if cert.signers.len() != self.participants.len()
            || cert.signers.count() != cert.signatures.len()
            || cert.signers.count() < self.participants.quorum::<N3f1>() as usize
        {
            return false;
        }
        cert.signers.iter().zip(&cert.signatures).all(|(s, sig)| self.check(&subject, s, sig))
    }
    fn is_batchable() -> bool {
        false
    }
    fn certificate_codec_config(&self) -> usize {
        self.participants.len()
    }
    fn certificate_codec_config_unbounded() -> usize {
        u32::MAX as usize
    }
}

impl certificate::Scheme for MlDsaScheme {
    type Signature = Signature;

    fn me(&self) -> Option<Participant> {
        self.signer.as_ref().map(|(i, _)| *i)
    }
    fn participants(&self) -> &Set<PublicKey> {
        &self.participants
    }
    fn sign<D: Digest>(&self, subject: Subject<'_, D>) -> Option<Attestation<Self>> {
        let (idx, key) = self.signer.as_ref()?;
        let message = subject.message();
        if let Some(p) = &self.protection {
            let (kind, round) = crate::protection::subject_parts(&subject);
            if !p.check_and_record(round.epoch().get(), round.view().get(), kind, &message) {
                return None;
            }
        }
        let sig = key.sign(subject.namespace(&self.namespace), &message);
        Some(Attestation { signer: *idx, signature: sig.into() })
    }
    fn verify_attestation<R: CryptoRng, D: Digest>(&self, _rng: &mut R, subject: Subject<'_, D>, a: &Attestation<Self>, _strategy: &impl Strategy) -> bool {
        self.check(&subject, a.signer, &a.signature)
    }
    fn assemble<I>(&self, atts: NonEmpty<I>, _strategy: &impl Strategy) -> Result<Certificate, AssemblyError>
    where
        I: Iterator<Item = Attestation<Self>> + Send,
    {
        let mut entries = Vec::new();
        for Attestation { signer, signature } in atts {
            if self.participants.get(usize::from(signer)).is_none() {
                return Err(AssemblyError::UnknownSigner(signer));
            }
            if signature.get().is_none() {
                return Err(AssemblyError::MalformedSignature(signer));
            }
            entries.push((signer, signature));
        }
        entries.sort_by_key(|(s, _)| *s);
        let (idx, signatures): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
        let signers = Signers::try_from((&self.participants, idx))?;
        let q = self.participants.quorum::<N3f1>();
        if (signers.count() as u32) < q {
            return Err(AssemblyError::InsufficientAttestations(q, signers.count() as u32));
        }
        Ok(Certificate { signers, signatures })
    }
    fn is_attributable() -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonware_cryptography::Signer;

    #[test]
    fn same_seed_same_address_as_tx_keys() {
        let seed = [9u8; 32];
        let a = crate::crypto::Keys::from_seed(seed);
        let b = PrivateKey::from_seed_bytes(seed);
        assert_eq!(a.address(), b.public_key().address("boo"));
        let sig = b.sign(b"ns", b"msg");
        assert!(b.public_key().verify(b"ns", b"msg", &sig));
        assert!(!b.public_key().verify(b"other", b"msg", &sig));
        assert_eq!(sig, b.sign(b"ns", b"msg"), "deterministic");
    }

    /// Votes go through slashing protection: a conflicting one is never signed at all.
    #[test]
    fn the_scheme_signs_only_what_slashing_protection_allows() {
        use commonware_consensus::simplex::types::{Notarize, Nullify, Proposal};
        use commonware_consensus::types::{Epoch, Round, View};
        use commonware_cryptography::{blake3::Digest, certificate::Scheme as _};
        let key = PrivateKey::from_seed_bytes([3; 32]);
        let set = Set::try_from(vec![key.public_key()]).unwrap();
        let p = crate::protection::Protection::in_memory(key.public_key().as_ref(), crate::protection::Timing::for_block_time(1_000));
        p.enter_epoch(1, Some(0), 1, 0);
        let scheme = MlDsaScheme::signer(b"ns", set, key).unwrap().with_protection(Some(p.clone()));
        let round = Round::new(Epoch::new(1), View::new(4));
        let proposal = |d: u8| Proposal::new(round, View::new(3), Digest([d; 32]));
        let a = Notarize::sign(&scheme, proposal(1)).expect("first notarize");
        assert!(Notarize::sign(&scheme, proposal(1)) == Some(a), "the identical vote again (deterministic)");
        assert!(Notarize::sign(&scheme, proposal(2)).is_none(), "a conflicting notarize is never signed");
        assert!(Nullify::sign::<Digest>(&scheme, round).is_some());
        let recorded = hex::encode(crate::crypto::hash(&Subject::Notarize { proposal: &proposal(1) }.message()));
        assert_eq!(p.record().votes[&4].notarize, Some(recorded));
        // A verifier-only scheme never consults it.
        assert!(MlDsaScheme::verifier(b"ns", Set::try_from(vec![PrivateKey::from_seed_bytes([4; 32]).public_key()]).unwrap()).sign::<Digest>(Subject::Nullify { round }).is_none());
    }
}
