//! Hybrid X25519 + ML-KEM-768 authenticated key exchange (Blockula).
//!
//! This is [commonware_cryptography::handshake] (three messages, mutual authentication by
//! signatures over a BLAKE3 [Transcript], ChaCha20-Poly1305 afterwards) with one change: the
//! session secret comes from two key exchanges instead of one.
//!
//! - [Syn] (dialer to listener) carries a [KeyShare]: an ephemeral ML-KEM-768 encapsulation key
//!   (1,184 bytes) followed by an ephemeral X25519 public key (32 bytes).
//! - [SynAck] (listener to dialer) carries a [Reply]: an ML-KEM-768 ciphertext for that key
//!   (1,088 bytes) followed by the listener's ephemeral X25519 public key (32 bytes).
//! - [Ack] is unchanged.
//!
//! Both shared secrets are committed to the transcript as one 64-byte packet, ML-KEM first
//! (`ss_ML-KEM ‖ ss_X25519`, the byte order of TLS `X25519MLKEM768`). By then the transcript has
//! bound the namespace, both timestamps, both identities, the key share and the reply, and the
//! AEAD keys and key confirmations are derived from it as upstream does. Hashing both secrets with
//! every public input is the standard KEM combiner: the session keys stay secret as long as either
//! X25519 or ML-KEM-768 holds.
//!
//! There is one suite and nothing is negotiated, so there is nothing to downgrade. A peer running
//! the classical handshake fails closed: its messages have different sizes, and its signatures
//! cover a transcript with a different protocol label.
//!
//! Every random byte comes from the caller's RNG (the runtime context), so handshakes stay
//! reproducible under Commonware's deterministic runtime.
//!
//! # Transcript
//!
//! ```text
//! T = Transcript::new(namespace, V1).fork("_BLOCKULA_STREAM_HANDSHAKE_X25519MLKEM768_V1")
//! T.commit(time_D).commit(pk_L).commit(pk_D).commit(share_D)    -> dialer signs
//! T.commit(time_L).commit(reply_L)                              -> listener signs
//! T.commit(ss_ML-KEM ‖ ss_X25519)
//! ciphers:       T.noise("cipher_l2d"), T.noise("cipher_d2l")
//! confirmations: T.fork("confirmation_l2d"), T.fork("confirmation_d2l")
//! ```

use commonware_codec::{Encode, Error as CodecError, FixedSize, Read, ReadExt, Write};
use commonware_cryptography::{
    PublicKey, Secret, Signature, Signer, Verifier,
    handshake::{RecvCipher, SendCipher},
    transcript::{Summary, Transcript, Version},
};
use commonware_runtime::{Buf, BufMut};
use core::ops::Range;
use ml_kem::{
    Decapsulate, DecapsulationKey768, Encapsulate, EncapsulationKey768, KeyExport, KeyInit,
    TryKeyInit,
};
use rand_core::CryptoRng;
use zeroize::Zeroizing;

pub use commonware_cryptography::handshake::Error;

#[cfg(test)]
mod ml_kem_vectors;
#[cfg(test)]
mod mldsa;
#[cfg(test)]
mod tests;

/// Protocol label. Differs from upstream's `_COMMONWARE_CRYPTOGRAPHY_HANDSHAKE`, so no signature
/// or key from one handshake is valid in the other. Public so the node can report and pin it.
pub const PROTOCOL: &[u8] = b"_BLOCKULA_STREAM_HANDSHAKE_X25519MLKEM768_V1";
/// What [crate::encrypted] runs, in words. The node builds `/api/status`'s
/// `crypto.p2p_transport` from this and [PROTOCOL]: neither exists in upstream's crate, so a node
/// build that links upstream's (classical) stream instead of this copy fails to compile.
pub const SUITE: &str = "hybrid X25519 + ML-KEM-768 key exchange, ChaCha20-Poly1305";
const LABEL_CIPHER_L2D: &[u8] = b"cipher_l2d";
const LABEL_CIPHER_D2L: &[u8] = b"cipher_d2l";
const LABEL_CONFIRMATION_L2D: &[u8] = b"confirmation_l2d";
const LABEL_CONFIRMATION_D2L: &[u8] = b"confirmation_d2l";

/// V1 framing is injective for any packet history, so the schema needs no V0 uniqueness proof.
const TRANSCRIPT_VERSION: Version = Version::V1;

/// Size of an ML-KEM-768 encapsulation key (FIPS 203, `384k + 32`).
pub const ML_KEM_EK_SIZE: usize = 1184;
/// Size of an ML-KEM-768 ciphertext (FIPS 203, `32(d_u k + d_v)`).
pub const ML_KEM_CT_SIZE: usize = 1088;
/// Size of an X25519 public key.
pub const X25519_SIZE: usize = 32;
const KEY_SHARE_SIZE: usize = ML_KEM_EK_SIZE + X25519_SIZE;
const REPLY_SIZE: usize = ML_KEM_CT_SIZE + X25519_SIZE;
const SECRET_SIZE: usize = 32 + 32;

/// The dialer's ephemeral public keys: `ek_ML-KEM-768 ‖ X25519`.
///
/// Decoding validates the encapsulation key (the FIPS 203 §7.2 modulus check), so a malformed key
/// is dropped as a codec error before any signature is checked.
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct KeyShare {
    raw: Box<[u8; KEY_SHARE_SIZE]>,
    ek: EncapsulationKey768,
    x25519: x25519_dalek::PublicKey,
}

impl KeyShare {
    fn from_raw(raw: Box<[u8; KEY_SHARE_SIZE]>) -> Result<Self, CodecError> {
        let (ek, x25519) = raw.split_at(ML_KEM_EK_SIZE);
        let ek = EncapsulationKey768::new_from_slice(ek)
            .map_err(|_| CodecError::Invalid("hybrid::KeyShare", "invalid ML-KEM-768 key"))?;
        let x25519: [u8; X25519_SIZE] = x25519.try_into().expect("split at a fixed offset");
        Ok(Self {
            raw,
            ek,
            x25519: x25519.into(),
        })
    }
}

impl FixedSize for KeyShare {
    const SIZE: usize = KEY_SHARE_SIZE;
}

impl Write for KeyShare {
    fn write(&self, buf: &mut impl BufMut) {
        buf.put_slice(&self.raw[..]);
    }
}

impl Read for KeyShare {
    type Cfg = ();

    fn read_cfg(buf: &mut impl Buf, _cfg: &Self::Cfg) -> Result<Self, CodecError> {
        Self::from_raw(read_boxed(buf)?)
    }
}

/// The listener's answer to a [KeyShare]: `ct_ML-KEM-768 ‖ X25519`.
///
/// Any bytes decode. A forged ciphertext decapsulates to an unrelated secret (ML-KEM's implicit
/// rejection), which then fails key confirmation; a low-order X25519 key fails the exchange.
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct Reply {
    raw: Box<[u8; REPLY_SIZE]>,
}

impl Reply {
    fn ciphertext(&self) -> &[u8] {
        &self.raw[..ML_KEM_CT_SIZE]
    }

    fn x25519(&self) -> x25519_dalek::PublicKey {
        let x25519: [u8; X25519_SIZE] = self.raw[ML_KEM_CT_SIZE..]
            .try_into()
            .expect("split at a fixed offset");
        x25519.into()
    }
}

impl FixedSize for Reply {
    const SIZE: usize = REPLY_SIZE;
}

impl Write for Reply {
    fn write(&self, buf: &mut impl BufMut) {
        buf.put_slice(&self.raw[..]);
    }
}

impl Read for Reply {
    type Cfg = ();

    fn read_cfg(buf: &mut impl Buf, _cfg: &Self::Cfg) -> Result<Self, CodecError> {
        Ok(Self {
            raw: read_boxed(buf)?,
        })
    }
}

fn read_boxed<const N: usize>(buf: &mut impl Buf) -> Result<Box<[u8; N]>, CodecError> {
    if buf.remaining() < N {
        return Err(CodecError::EndOfBuffer);
    }
    let mut raw = Box::new([0u8; N]);
    buf.copy_to_slice(&mut raw[..]);
    Ok(raw)
}

/// The dialer's ephemeral secrets, held between [Syn] and [SynAck].
struct DialSecret {
    /// ML-KEM-768 seed `d ‖ z`; the decapsulation key is expanded from it when needed.
    ml_kem: Zeroizing<ml_kem::Seed>,
    x25519: Secret<x25519_dalek::EphemeralSecret>,
}

/// `ss_ML-KEM ‖ ss_X25519`, committed to the transcript as one packet.
struct SharedSecret(Zeroizing<[u8; SECRET_SIZE]>);

impl SharedSecret {
    fn new(ml_kem: &[u8], x25519: &x25519_dalek::SharedSecret) -> Self {
        let mut out = Zeroizing::new([0u8; SECRET_SIZE]);
        out[..32].copy_from_slice(ml_kem);
        out[32..].copy_from_slice(x25519.as_bytes());
        Self(out)
    }
}

/// Generates the dialer's ephemeral keys: the ML-KEM seed first, then the X25519 secret.
fn generate(rng: &mut impl CryptoRng) -> (DialSecret, KeyShare) {
    let mut seed = Zeroizing::new(ml_kem::Seed::default());
    rng.fill_bytes(&mut seed[..]);
    let dk = <DecapsulationKey768 as KeyInit>::new(&seed);
    let ek = dk.encapsulation_key().clone();
    let x25519 = x25519_dalek::EphemeralSecret::random_from_rng(rng);
    let x25519_public = x25519_dalek::PublicKey::from(&x25519);

    let mut raw = Box::new([0u8; KEY_SHARE_SIZE]);
    raw[..ML_KEM_EK_SIZE].copy_from_slice(&ek.to_bytes());
    raw[ML_KEM_EK_SIZE..].copy_from_slice(x25519_public.as_bytes());
    (
        DialSecret {
            ml_kem: seed,
            x25519: Secret::new(x25519),
        },
        KeyShare {
            raw,
            ek,
            x25519: x25519_public,
        },
    )
}

/// The listener's half: encapsulates to the ML-KEM key, then runs X25519 with a fresh secret.
/// Returns `None` if the X25519 exchange is non-contributory (a low-order dialer key).
fn encapsulate(rng: &mut impl CryptoRng, share: &KeyShare) -> Option<(Reply, SharedSecret)> {
    let (ct, ml_kem) = share.ek.encapsulate_with_rng(rng);
    let ml_kem = Zeroizing::new(ml_kem);
    let x25519 = x25519_dalek::EphemeralSecret::random_from_rng(rng);
    let x25519_public = x25519_dalek::PublicKey::from(&x25519);
    let x25519 = x25519.diffie_hellman(&share.x25519);
    if !x25519.was_contributory() {
        return None;
    }

    let mut raw = Box::new([0u8; REPLY_SIZE]);
    raw[..ML_KEM_CT_SIZE].copy_from_slice(&ct);
    raw[ML_KEM_CT_SIZE..].copy_from_slice(x25519_public.as_bytes());
    Some((Reply { raw }, SharedSecret::new(&ml_kem, &x25519)))
}

/// The dialer's half. Returns `None` if the X25519 exchange is non-contributory. ML-KEM never
/// fails here: a bad ciphertext yields a pseudorandom secret and key confirmation catches it.
fn decapsulate(secret: DialSecret, reply: &Reply) -> Option<SharedSecret> {
    let x25519 = secret
        .x25519
        .expose_unwrap()
        .diffie_hellman(&reply.x25519());
    if !x25519.was_contributory() {
        return None;
    }
    let dk = <DecapsulationKey768 as KeyInit>::new(&secret.ml_kem);
    let ml_kem = Zeroizing::new(
        dk.decapsulate_slice(reply.ciphertext())
            .expect("ciphertext has a fixed size"),
    );
    Some(SharedSecret::new(&ml_kem, &x25519))
}

/// Commits the [Syn] fields. Both identities go in before the dialer signs.
fn commit_syn(
    transcript: &mut Transcript,
    time_ms: u64,
    dialer: &impl Encode,
    listener: &impl Encode,
    share: &KeyShare,
) {
    transcript
        .commit(time_ms.encode())
        .commit(listener.encode())
        .commit(dialer.encode())
        .commit(&share.raw[..]);
}

/// Commits the [SynAck] fields the listener signs.
fn commit_syn_ack(transcript: &mut Transcript, time_ms: u64, reply: &Reply) {
    transcript.commit(time_ms.encode()).commit(&reply.raw[..]);
}

/// Which end of the connection is deriving keys.
#[derive(Clone, Copy)]
enum Role {
    Dialer,
    Listener,
}

/// Session keys and key confirmations, from one side's point of view.
struct Keys {
    send: SendCipher,
    recv: RecvCipher,
    /// The confirmation this side sends.
    confirm: Summary,
    /// The confirmation this side expects from its peer.
    expect: Summary,
}

/// Commits the shared secret and derives the session keys from the whole transcript.
fn derive(transcript: &mut Transcript, secret: SharedSecret, role: Role) -> Keys {
    transcript.commit(&secret.0[..]);
    let cipher_l2d = transcript.noise(LABEL_CIPHER_L2D);
    let cipher_d2l = transcript.noise(LABEL_CIPHER_D2L);
    let confirmation_l2d = transcript.fork(LABEL_CONFIRMATION_L2D).summarize();
    let confirmation_d2l = transcript.fork(LABEL_CONFIRMATION_D2L).summarize();
    match role {
        Role::Dialer => Keys {
            send: SendCipher::new(cipher_d2l),
            recv: RecvCipher::new(cipher_l2d),
            confirm: confirmation_d2l,
            expect: confirmation_l2d,
        },
        Role::Listener => Keys {
            send: SendCipher::new(cipher_l2d),
            recv: RecvCipher::new(cipher_d2l),
            confirm: confirmation_l2d,
            expect: confirmation_d2l,
        },
    }
}

/// First handshake message, sent by the dialer: its timestamp, [KeyShare] and signature.
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct Syn<S: Signature> {
    time_ms: u64,
    share: KeyShare,
    sig: S,
}

impl<S: Signature> FixedSize for Syn<S> {
    const SIZE: usize = u64::SIZE + KeyShare::SIZE + S::SIZE;
}

impl<S: Signature + Write> Write for Syn<S> {
    fn write(&self, buf: &mut impl BufMut) {
        self.time_ms.write(buf);
        self.share.write(buf);
        self.sig.write(buf);
    }
}

impl<S: Signature + Read> Read for Syn<S> {
    type Cfg = S::Cfg;

    fn read_cfg(buf: &mut impl Buf, cfg: &Self::Cfg) -> Result<Self, CodecError> {
        Ok(Self {
            time_ms: ReadExt::read(buf)?,
            share: ReadExt::read(buf)?,
            sig: Read::read_cfg(buf, cfg)?,
        })
    }
}

/// Second handshake message, sent by the listener: its timestamp, [Reply], signature and key
/// confirmation.
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct SynAck<S: Signature> {
    time_ms: u64,
    reply: Reply,
    sig: S,
    confirmation: Summary,
}

impl<S: Signature> FixedSize for SynAck<S> {
    const SIZE: usize = u64::SIZE + Reply::SIZE + S::SIZE + Summary::SIZE;
}

impl<S: Signature + Write> Write for SynAck<S> {
    fn write(&self, buf: &mut impl BufMut) {
        self.time_ms.write(buf);
        self.reply.write(buf);
        self.sig.write(buf);
        self.confirmation.write(buf);
    }
}

impl<S: Signature + Read> Read for SynAck<S> {
    type Cfg = S::Cfg;

    fn read_cfg(buf: &mut impl Buf, cfg: &Self::Cfg) -> Result<Self, CodecError> {
        Ok(Self {
            time_ms: ReadExt::read(buf)?,
            reply: ReadExt::read(buf)?,
            sig: Read::read_cfg(buf, cfg)?,
            confirmation: ReadExt::read(buf)?,
        })
    }
}

/// Third handshake message, sent by the dialer: its key confirmation.
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct Ack {
    confirmation: Summary,
}

impl FixedSize for Ack {
    const SIZE: usize = Summary::SIZE;
}

impl Write for Ack {
    fn write(&self, buf: &mut impl BufMut) {
        self.confirmation.write(buf);
    }
}

impl Read for Ack {
    type Cfg = ();

    fn read_cfg(buf: &mut impl Buf, _cfg: &Self::Cfg) -> Result<Self, CodecError> {
        Ok(Self {
            confirmation: ReadExt::read(buf)?,
        })
    }
}

/// State kept by the dialer between [dial_start] and [dial_end].
pub struct DialState<P> {
    secret: DialSecret,
    peer_identity: P,
    transcript: Transcript,
    ok_timestamps: Range<u64>,
}

/// State kept by the listener between [listen_start] and [listen_end].
pub struct ListenState {
    confirmation: Summary,
    send: SendCipher,
    recv: RecvCipher,
}

/// Handshake context: the namespace, our clock and accepted timestamps, and both identities.
pub struct Context<S, P> {
    transcript: Transcript,
    current_time: u64,
    ok_timestamps: Range<u64>,
    my_identity: S,
    peer_identity: P,
}

impl<S, P> Context<S, P> {
    /// Creates a new handshake context.
    pub fn new(
        namespace: &[u8],
        current_time_ms: u64,
        ok_timestamps: Range<u64>,
        my_identity: S,
        peer_identity: P,
    ) -> Self {
        Self {
            transcript: Transcript::new(namespace, TRANSCRIPT_VERSION).fork(PROTOCOL),
            current_time: current_time_ms,
            ok_timestamps,
            my_identity,
            peer_identity,
        }
    }
}

/// Starts a handshake as the dialer. Returns the dialer's state and the [Syn] to send.
pub fn dial_start<S: Signer, P: PublicKey>(
    mut rng: impl CryptoRng,
    ctx: Context<S, P>,
) -> (DialState<P>, Syn<<S as Signer>::Signature>) {
    let Context {
        mut transcript,
        current_time,
        ok_timestamps,
        my_identity,
        peer_identity,
    } = ctx;
    let (secret, share) = generate(&mut rng);
    commit_syn(
        &mut transcript,
        current_time,
        &my_identity.public_key(),
        &peer_identity,
        &share,
    );
    let sig = transcript.sign(&my_identity);
    (
        DialState {
            secret,
            peer_identity,
            transcript,
            ok_timestamps,
        },
        Syn {
            time_ms: current_time,
            share,
            sig,
        },
    )
}

/// Completes a handshake as the dialer: checks the listener's [SynAck] and returns the [Ack] to
/// send plus the session ciphers.
pub fn dial_end<P: PublicKey>(
    state: DialState<P>,
    msg: SynAck<<P as Verifier>::Signature>,
) -> Result<(Ack, SendCipher, RecvCipher), Error> {
    let DialState {
        secret,
        peer_identity,
        mut transcript,
        ok_timestamps,
    } = state;
    if !ok_timestamps.contains(&msg.time_ms) {
        return Err(Error::InvalidTimestamp(msg.time_ms, ok_timestamps));
    }
    commit_syn_ack(&mut transcript, msg.time_ms, &msg.reply);
    if !transcript.verify(&peer_identity, &msg.sig) {
        return Err(Error::HandshakeFailed);
    }
    let shared = decapsulate(secret, &msg.reply).ok_or(Error::HandshakeFailed)?;
    let keys = derive(&mut transcript, shared, Role::Dialer);
    // `Summary` equality is constant time (`blake3::Hash`).
    if msg.confirmation != keys.expect {
        return Err(Error::HandshakeFailed);
    }
    Ok((
        Ack {
            confirmation: keys.confirm,
        },
        keys.send,
        keys.recv,
    ))
}

/// Answers a dialer's [Syn] as the listener. Verifies the dialer's signature before doing any
/// key-exchange work, and returns the listener's state and the [SynAck] to send.
pub fn listen_start<S: Signer, P: PublicKey>(
    mut rng: impl CryptoRng,
    ctx: Context<S, P>,
    msg: Syn<<P as Verifier>::Signature>,
) -> Result<(ListenState, SynAck<<S as Signer>::Signature>), Error> {
    let Context {
        mut transcript,
        current_time,
        ok_timestamps,
        my_identity,
        peer_identity,
    } = ctx;
    if !ok_timestamps.contains(&msg.time_ms) {
        return Err(Error::InvalidTimestamp(msg.time_ms, ok_timestamps));
    }
    commit_syn(
        &mut transcript,
        msg.time_ms,
        &peer_identity,
        &my_identity.public_key(),
        &msg.share,
    );
    if !transcript.verify(&peer_identity, &msg.sig) {
        return Err(Error::HandshakeFailed);
    }
    let (reply, shared) = encapsulate(&mut rng, &msg.share).ok_or(Error::HandshakeFailed)?;
    commit_syn_ack(&mut transcript, current_time, &reply);
    let sig = transcript.sign(&my_identity);
    let keys = derive(&mut transcript, shared, Role::Listener);
    Ok((
        ListenState {
            confirmation: keys.expect,
            send: keys.send,
            recv: keys.recv,
        },
        SynAck {
            time_ms: current_time,
            reply,
            sig,
            confirmation: keys.confirm,
        },
    ))
}

/// Completes a handshake as the listener: checks the dialer's [Ack] and returns the session
/// ciphers.
pub fn listen_end(state: ListenState, msg: Ack) -> Result<(SendCipher, RecvCipher), Error> {
    if msg.confirmation != state.confirmation {
        return Err(Error::HandshakeFailed);
    }
    Ok((state.send, state.recv))
}
