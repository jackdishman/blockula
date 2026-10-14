//! Hybrid handshake tests: round trips, sizes, tampering, invalid keys, classical/hybrid
//! interop (must fail closed), and a pinned known-answer fixture.

use super::{mldsa, *};
use crate::{
    encrypted::{self, Config},
    utils::codec::{recv_frame, send_frame},
};
use commonware_codec::{DecodeExt, Encode, FixedSize};
use commonware_cryptography::{Signer, ed25519, handshake as classic};
use commonware_math::algebra::Random;
use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _, deterministic, mocks};
use ml_kem::array::typenum::Unsigned;
use rand_core::Rng as _;
use std::time::Duration;

const NS: &[u8] = b"blockula/test/p2p_STREAM";
const NOW: u64 = 500;
const WINDOW: Range<u64> = 0..1000;

/// Deterministic randomness from a label, independent of `rand`'s algorithms.
fn rng(label: &'static [u8]) -> impl CryptoRng {
    Transcript::new(b"blockula/hybrid-test", Version::V1).noise(label)
}

fn ctx<S, P>(me: S, peer: P) -> Context<S, P> {
    Context::new(NS, NOW, WINDOW, me, peer)
}

fn ed25519_pair() -> (ed25519::PrivateKey, ed25519::PrivateKey) {
    (
        ed25519::PrivateKey::from_seed(42),
        ed25519::PrivateKey::from_seed(24),
    )
}

fn mldsa_pair() -> (mldsa::PrivateKey, mldsa::PrivateKey) {
    (
        mldsa::PrivateKey::from_seed_bytes([1; 32]),
        mldsa::PrivateKey::from_seed_bytes([2; 32]),
    )
}

fn roundtrip<T: Encode + DecodeExt<()> + PartialEq + core::fmt::Debug>(value: &T) {
    assert_eq!(value, &T::decode(value.encode()).unwrap());
}

fn handshake<S: Signer>(dialer: S, listener: S) -> Result<(), Error>
where
    S::Signature: core::fmt::Debug,
{
    let (d_state, syn) = dial_start(rng(b"dialer"), ctx(dialer.clone(), listener.public_key()));
    roundtrip(&syn);
    let (l_state, syn_ack) =
        listen_start(rng(b"listener"), ctx(listener, dialer.public_key()), syn)?;
    roundtrip(&syn_ack);
    let (ack, mut d_send, mut d_recv) = dial_end(d_state, syn_ack)?;
    roundtrip(&ack);
    let (mut l_send, mut l_recv) = listen_end(l_state, ack)?;

    for msg in [&b"first"[..], b"", &[7u8; 5000]] {
        assert_eq!(l_recv.recv(&d_send.send(msg)?)?, msg);
        assert_eq!(d_recv.recv(&l_send.send(msg)?)?, msg);
    }
    Ok(())
}

#[test]
fn round_trip_ed25519() -> Result<(), Error> {
    let (d, l) = ed25519_pair();
    handshake(d, l)
}

#[test]
fn round_trip_mldsa() -> Result<(), Error> {
    let (d, l) = mldsa_pair();
    handshake(d, l)
}

#[test]
fn sizes() {
    // The constants match the ML-KEM-768 types.
    type EkSize = <EncapsulationKey768 as ml_kem::KeySizeUser>::KeySize;
    type CtSize = <ml_kem::MlKem768 as ml_kem::Kem>::CiphertextSize;
    assert_eq!(EkSize::USIZE, ML_KEM_EK_SIZE);
    assert_eq!(CtSize::USIZE, ML_KEM_CT_SIZE);

    assert_eq!(KeyShare::SIZE, 1216);
    assert_eq!(Reply::SIZE, 1120);
    assert_eq!(Ack::SIZE, 32);
    // With the node's ML-DSA-65 identities: +1,184 B and +1,088 B over the classical handshake.
    assert_eq!(Syn::<mldsa::Signature>::SIZE, 4533);
    assert_eq!(SynAck::<mldsa::Signature>::SIZE, 4469);
    assert_eq!(classic::Syn::<mldsa::Signature>::SIZE, 3349);
    assert_eq!(classic::SynAck::<mldsa::Signature>::SIZE, 3381);

    let (dialer, listener) = mldsa_pair();
    let (d_state, syn) = dial_start(rng(b"d"), ctx(dialer.clone(), listener.public_key()));
    assert_eq!(syn.encode().len(), 4533);
    let (_, syn_ack) = listen_start(rng(b"l"), ctx(listener, dialer.public_key()), syn).unwrap();
    assert_eq!(syn_ack.encode().len(), 4469);
    let (ack, _, _) = dial_end(d_state, syn_ack).unwrap();
    assert_eq!(ack.encode().len(), 32);
}

#[test]
fn mismatched_namespace_fails() {
    let (dialer, listener) = ed25519_pair();
    let (_, syn) = dial_start(
        rng(b"d"),
        Context::new(
            b"namespace_a",
            NOW,
            WINDOW,
            dialer.clone(),
            listener.public_key(),
        ),
    );
    let result = listen_start(
        rng(b"l"),
        Context::new(b"namespace_b", NOW, WINDOW, listener, dialer.public_key()),
        syn,
    );
    assert!(matches!(result, Err(Error::HandshakeFailed)));
}

#[test]
fn wrong_identity_fails() {
    // The dialer signs for a different listener than the one that answers.
    let (dialer, listener) = ed25519_pair();
    let other = ed25519::PrivateKey::from_seed(7);
    let (_, syn) = dial_start(rng(b"d"), ctx(dialer.clone(), other.public_key()));
    let result = listen_start(rng(b"l"), ctx(listener, dialer.public_key()), syn);
    assert!(matches!(result, Err(Error::HandshakeFailed)));
}

#[test]
fn timestamps_outside_the_window_fail() {
    let (dialer, listener) = ed25519_pair();
    let (_, syn) = dial_start(
        rng(b"d"),
        Context::new(NS, 2000, WINDOW, dialer.clone(), listener.public_key()),
    );
    let result = listen_start(rng(b"l"), ctx(listener.clone(), dialer.public_key()), syn);
    assert!(matches!(result, Err(Error::InvalidTimestamp(2000, _))));

    let (d_state, syn) = dial_start(rng(b"d"), ctx(dialer.clone(), listener.public_key()));
    let (_, syn_ack) = listen_start(
        rng(b"l"),
        Context::new(NS, 2000, WINDOW, listener, dialer.public_key()),
        syn,
    )
    .unwrap();
    assert!(matches!(
        dial_end(d_state, syn_ack),
        Err(Error::InvalidTimestamp(2000, _))
    ));
}

/// Byte ranges of each field in an encoding, by field sizes.
fn fields(sizes: &[(&'static str, usize)]) -> Vec<(&'static str, Range<usize>)> {
    let mut start = 0;
    sizes
        .iter()
        .map(|&(name, len)| {
            start += len;
            (name, start - len..start)
        })
        .collect()
}

/// Bit positions to flip in a field: first, middle and last byte, low and high bit.
fn flips(range: &Range<usize>) -> Vec<usize> {
    let bytes = [range.start, (range.start + range.end) / 2, range.end - 1];
    bytes.iter().flat_map(|b| [b * 8, b * 8 + 7]).collect()
}

fn flipped(bytes: &[u8], bit: usize) -> Vec<u8> {
    let mut out = bytes.to_vec();
    out[bit / 8] ^= 1 << (bit % 8);
    out
}

type Sig = <ed25519::PrivateKey as Signer>::Signature;

#[test]
fn tampered_syn_fails() {
    let (dialer, listener) = ed25519_pair();
    let layout = fields(&[
        ("time", 8),
        ("ek_ml_kem", ML_KEM_EK_SIZE),
        ("x25519", X25519_SIZE),
        ("sig", Sig::SIZE),
    ]);
    let mut reached_handshake = 0;
    for (name, range) in layout {
        for bit in flips(&range) {
            let (_, syn) = dial_start(rng(b"d"), ctx(dialer.clone(), listener.public_key()));
            let bytes = flipped(&syn.encode(), bit);
            let Ok(syn) = Syn::<Sig>::decode(&bytes[..]) else {
                continue; // rejected at decode
            };
            reached_handshake += 1;
            let result = listen_start(rng(b"l"), ctx(listener.clone(), dialer.public_key()), syn);
            assert!(result.is_err(), "flipped bit {bit} in {name} was accepted");
        }
    }
    // At least the timestamp and X25519 flips decode and must be caught by the signature.
    assert!(
        reached_handshake >= 12,
        "only {reached_handshake} flips reached the handshake"
    );
}

#[test]
fn tampered_syn_ack_fails() {
    let (dialer, listener) = ed25519_pair();
    let layout = fields(&[
        ("time", 8),
        ("ct_ml_kem", ML_KEM_CT_SIZE),
        ("x25519", X25519_SIZE),
        ("sig", Sig::SIZE),
        ("confirmation", Summary::SIZE),
    ]);
    let mut reached_handshake = 0;
    for (name, range) in layout {
        for bit in flips(&range) {
            let (d_state, syn) = dial_start(rng(b"d"), ctx(dialer.clone(), listener.public_key()));
            let (_, syn_ack) =
                listen_start(rng(b"l"), ctx(listener.clone(), dialer.public_key()), syn).unwrap();
            let bytes = flipped(&syn_ack.encode(), bit);
            let Ok(syn_ack) = SynAck::<Sig>::decode(&bytes[..]) else {
                continue;
            };
            reached_handshake += 1;
            assert!(
                dial_end(d_state, syn_ack).is_err(),
                "flipped bit {bit} in {name} was accepted"
            );
        }
    }
    // Every field but the signature accepts any bytes, so those flips all reach `dial_end`.
    assert!(
        reached_handshake >= 24,
        "only {reached_handshake} flips reached the handshake"
    );
}

#[test]
fn tampered_ack_fails() {
    let (dialer, listener) = ed25519_pair();
    for bit in flips(&(0..Ack::SIZE)) {
        let (d_state, syn) = dial_start(rng(b"d"), ctx(dialer.clone(), listener.public_key()));
        let (l_state, syn_ack) =
            listen_start(rng(b"l"), ctx(listener.clone(), dialer.public_key()), syn).unwrap();
        let (ack, _, _) = dial_end(d_state, syn_ack).unwrap();
        let ack = Ack::decode(&flipped(&ack.encode(), bit)[..]).unwrap();
        assert!(matches!(
            listen_end(l_state, ack),
            Err(Error::HandshakeFailed)
        ));
    }
}

/// A listener that encapsulates honestly, then signs a reply whose ML-KEM ciphertext it
/// altered (or whose X25519 key it replaced). The signature is valid, so only decapsulation and
/// key confirmation stand between it and a session.
fn forged_syn_ack(
    listener: &ed25519::PrivateKey,
    dialer: &ed25519::PrivateKey,
    syn: &Syn<Sig>,
    tamper: impl FnOnce(&mut Reply),
) -> SynAck<Sig> {
    let mut transcript = ctx(listener.clone(), dialer.public_key()).transcript;
    commit_syn(
        &mut transcript,
        syn.time_ms,
        &dialer.public_key(),
        &listener.public_key(),
        &syn.share,
    );
    assert!(transcript.verify(&dialer.public_key(), &syn.sig));
    let (mut reply, shared) = encapsulate(&mut rng(b"l"), &syn.share).unwrap();
    tamper(&mut reply);
    commit_syn_ack(&mut transcript, NOW, &reply);
    let sig = transcript.sign(listener);
    let keys = derive(&mut transcript, shared, Role::Listener);
    SynAck {
        time_ms: NOW,
        reply,
        sig,
        confirmation: keys.confirm,
    }
}

#[test]
fn resigned_bad_ciphertext_fails_confirmation() {
    let (dialer, listener) = ed25519_pair();

    // Control: the hand-built path is accepted when nothing is altered.
    let (d_state, syn) = dial_start(rng(b"d"), ctx(dialer.clone(), listener.public_key()));
    let syn_ack = forged_syn_ack(&listener, &dialer, &syn, |_| {});
    assert!(dial_end(d_state, syn_ack).is_ok());

    for byte in [0, ML_KEM_CT_SIZE / 2, ML_KEM_CT_SIZE - 1] {
        let (d_state, syn) = dial_start(rng(b"d"), ctx(dialer.clone(), listener.public_key()));
        let syn_ack = forged_syn_ack(&listener, &dialer, &syn, |reply| reply.raw[byte] ^= 1);
        assert!(matches!(
            dial_end(d_state, syn_ack),
            Err(Error::HandshakeFailed)
        ));
    }
}

/// Curve25519 points of small order (and non-canonical encodings of them); X25519 with any of
/// them outputs all zeros.
const LOW_ORDER: [&str; 7] = [
    "0000000000000000000000000000000000000000000000000000000000000000",
    "0100000000000000000000000000000000000000000000000000000000000000",
    "e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800",
    "5f9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f1157",
    "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
];

fn low_order(hex_point: &str) -> [u8; 32] {
    hex::decode(hex_point).unwrap().try_into().unwrap()
}

#[test]
fn low_order_x25519_from_dialer_fails() {
    let (dialer, listener) = ed25519_pair();
    let resigned = |x25519: Option<[u8; 32]>| {
        let (_, syn) = dial_start(rng(b"d"), ctx(dialer.clone(), listener.public_key()));
        let mut raw = syn.share.raw.clone();
        if let Some(x25519) = x25519 {
            raw[ML_KEM_EK_SIZE..].copy_from_slice(&x25519);
        }
        let share = KeyShare::from_raw(raw).unwrap();
        let mut transcript = ctx(dialer.clone(), listener.public_key()).transcript;
        commit_syn(
            &mut transcript,
            NOW,
            &dialer.public_key(),
            &listener.public_key(),
            &share,
        );
        let sig = transcript.sign(&dialer);
        let syn = Syn {
            time_ms: NOW,
            share,
            sig,
        };
        listen_start(rng(b"l"), ctx(listener.clone(), dialer.public_key()), syn)
    };
    assert!(resigned(None).is_ok());
    for point in LOW_ORDER {
        assert!(
            matches!(
                resigned(Some(low_order(point))),
                Err(Error::HandshakeFailed)
            ),
            "accepted low-order point {point}"
        );
    }
}

#[test]
fn low_order_x25519_from_listener_fails() {
    let (dialer, listener) = ed25519_pair();
    for point in LOW_ORDER {
        let (d_state, syn) = dial_start(rng(b"d"), ctx(dialer.clone(), listener.public_key()));
        let syn_ack = forged_syn_ack(&listener, &dialer, &syn, |reply| {
            reply.raw[ML_KEM_CT_SIZE..].copy_from_slice(&low_order(point))
        });
        assert!(matches!(
            dial_end(d_state, syn_ack),
            Err(Error::HandshakeFailed)
        ));
    }
}

#[test]
fn invalid_ml_kem_key_fails_decode() {
    let (dialer, listener) = ed25519_pair();
    let (_, syn) = dial_start(rng(b"d"), ctx(dialer, listener.public_key()));
    let good = syn.encode().to_vec();
    assert!(Syn::<Sig>::decode(&good[..]).is_ok());

    // Coefficient 0 of the key is bytes 8 and 9 (low nibble); q = 3329 is out of range.
    for coefficient in [3329u16, 4095] {
        let mut bad = good.clone();
        bad[8] = coefficient as u8;
        bad[9] = (bad[9] & 0xf0) | (coefficient >> 8) as u8;
        assert!(matches!(
            Syn::<Sig>::decode(&bad[..]),
            Err(CodecError::Invalid("hybrid::KeyShare", _))
        ));
    }
    let mut ones = good;
    ones[8..8 + ML_KEM_EK_SIZE].fill(0xff);
    assert!(matches!(
        Syn::<Sig>::decode(&ones[..]),
        Err(CodecError::Invalid(..))
    ));
}

#[test]
fn arbitrary_bytes_never_panic() {
    let mut noise = commonware_utils::test_rng();
    fn lengths(size: usize) -> [usize; 5] {
        [0, size - 1, size, size + 1, 2 * size]
    }
    for _ in 0..64 {
        for len in lengths(Syn::<Sig>::SIZE) {
            let mut bytes = vec![0u8; len];
            noise.fill_bytes(&mut bytes);
            let _ = Syn::<Sig>::decode(&bytes[..]);
            let _ = Syn::<mldsa::Signature>::decode(&bytes[..]);
        }
        for len in lengths(SynAck::<Sig>::SIZE) {
            let mut bytes = vec![0u8; len];
            noise.fill_bytes(&mut bytes);
            let _ = SynAck::<Sig>::decode(&bytes[..]);
            let _ = SynAck::<mldsa::Signature>::decode(&bytes[..]);
        }
        // Any reply decodes; garbage must still fail the handshake rather than panic.
        let (dialer, listener) = ed25519_pair();
        let (d_state, _) = dial_start(rng(b"d"), ctx(dialer, listener.public_key()));
        let mut bytes = vec![0u8; SynAck::<Sig>::SIZE];
        noise.fill_bytes(&mut bytes);
        bytes[..8].copy_from_slice(&NOW.to_be_bytes());
        if let Ok(syn_ack) = SynAck::<Sig>::decode(&bytes[..]) {
            assert!(dial_end(d_state, syn_ack).is_err());
        }
    }
}

// ---------------------------------------------------------------- classical vs hybrid

#[test]
fn classical_syn_in_hybrid_layout_fails() {
    // Even with the frame forced to the hybrid size (the classical X25519 key in the X25519
    // slot, a valid all-zero ML-KEM key), the classical signature covers another transcript.
    let (dialer, listener) = ed25519_pair();
    let (_, syn) = classic::dial_start(
        rng(b"d"),
        classic::Context::new(NS, NOW, WINDOW, dialer.clone(), listener.public_key()),
    );
    let bytes = syn.encode();
    let mut forced = bytes[..8].to_vec();
    forced.extend_from_slice(&[0u8; ML_KEM_EK_SIZE]);
    forced.extend_from_slice(&bytes[8..]);
    let syn = Syn::<Sig>::decode(&forced[..]).expect("same layout as a hybrid Syn");
    let result = listen_start(rng(b"l"), ctx(listener, dialer.public_key()), syn);
    assert!(matches!(result, Err(Error::HandshakeFailed)));
}

#[test]
fn hybrid_syn_in_classical_layout_fails() {
    // And the reverse: the hybrid Syn cut down to the classical layout (keep the X25519 key).
    let (dialer, listener) = ed25519_pair();
    let (_, syn) = dial_start(rng(b"d"), ctx(dialer.clone(), listener.public_key()));
    let bytes = syn.encode();
    let mut forced = bytes[..8].to_vec();
    forced.extend_from_slice(&bytes[8 + ML_KEM_EK_SIZE..]);
    let syn = classic::Syn::<Sig>::decode(&forced[..]).expect("same layout as a classical Syn");
    let result = classic::listen_start(
        rng(b"l"),
        classic::Context::new(NS, NOW, WINDOW, listener, dialer.public_key()),
        syn,
    );
    assert!(matches!(result, Err(Error::HandshakeFailed)));
}

fn stream_config<S>(signing_key: S) -> Config<S> {
    Config {
        signing_key,
        namespace: NS.to_vec(),
        max_message_size: 64 * 1024,
        synchrony_bound: Duration::from_secs(1),
        max_handshake_age: Duration::from_secs(1),
        handshake_timeout: Duration::from_secs(1),
    }
}

#[test]
fn classical_dialer_cannot_reach_hybrid_listener() {
    // An unpatched node dials a patched one: its Syn is the wrong size for the hybrid decoder.
    deterministic::Runner::default().start(|context| async move {
        let (dialer, listener) = mldsa_pair();
        let (mut dialer_sink, listener_stream) = mocks::Channel::init();
        let (listener_sink, _dialer_stream) = mocks::Channel::init();
        let config = stream_config(listener.clone());
        let handle = context.child("listener").spawn(move |context| async move {
            encrypted::listen(
                context,
                |_| async { true },
                config,
                listener_stream,
                listener_sink,
            )
            .await
        });

        let max = 64 * 1024;
        send_frame(&mut dialer_sink, dialer.public_key().encode(), max)
            .await
            .unwrap();
        let (current_time, ok) = stream_config(dialer.clone()).time_information(&context);
        let (_, syn) = classic::dial_start(
            context.child("dialer"),
            classic::Context::new(NS, current_time, ok, dialer, listener.public_key()),
        );
        send_frame(&mut dialer_sink, syn.encode(), max)
            .await
            .unwrap();

        let result = handle.await.unwrap();
        assert!(matches!(result, Err(encrypted::Error::UnableToDecode(_))));
    });
}

#[test]
fn hybrid_dialer_cannot_reach_classical_listener() {
    // A patched node dials an unpatched one: the classical listener caps the Syn frame at its
    // own fixed size, so the larger hybrid Syn is refused, and the dialer never gets a SynAck.
    deterministic::Runner::default().start(|context| async move {
        let (dialer, listener) = mldsa_pair();
        let (dialer_sink, mut listener_stream) = mocks::Channel::init();
        let (_listener_sink, dialer_stream) = mocks::Channel::init();
        let config = stream_config(dialer);
        let peer = listener.public_key();
        let handle = context.child("dialer").spawn(move |context| async move {
            encrypted::dial(context, config, peer, dialer_stream, dialer_sink).await
        });

        let identity = recv_frame(&mut listener_stream, mldsa::PK_LEN as u32)
            .await
            .unwrap();
        assert_eq!(identity.len(), mldsa::PK_LEN);
        let cap = classic::Syn::<mldsa::Signature>::SIZE as u32;
        let refused = recv_frame(&mut listener_stream, cap).await;
        assert!(matches!(refused, Err(encrypted::Error::RecvTooLarge(4533))));

        assert!(handle.await.unwrap().is_err());
    });
}

#[test]
fn invalid_ml_kem_key_is_dropped_by_the_listener() {
    deterministic::Runner::default().start(|context| async move {
        let (dialer, listener) = mldsa_pair();
        let (mut dialer_sink, listener_stream) = mocks::Channel::init();
        let (listener_sink, _dialer_stream) = mocks::Channel::init();
        let config = stream_config(listener.clone());
        let handle = context.child("listener").spawn(move |context| async move {
            encrypted::listen(
                context,
                |_| async { true },
                config,
                listener_stream,
                listener_sink,
            )
            .await
        });

        let max = 64 * 1024;
        send_frame(&mut dialer_sink, dialer.public_key().encode(), max)
            .await
            .unwrap();
        let (current_time, ok) = stream_config(dialer.clone()).time_information(&context);
        let (_, syn) = dial_start(
            context.child("dialer"),
            Context::new(NS, current_time, ok, dialer, listener.public_key()),
        );
        let mut bytes = syn.encode().to_vec();
        bytes[8..8 + ML_KEM_EK_SIZE].fill(0xff);
        send_frame(&mut dialer_sink, bytes, max).await.unwrap();

        let result = handle.await.unwrap();
        assert!(matches!(
            result,
            Err(encrypted::Error::UnableToDecode(CodecError::Invalid(
                "hybrid::KeyShare",
                _
            )))
        ));
    });
}

#[test]
fn stream_round_trip_mldsa() {
    deterministic::Runner::default().start(|context| async move {
        let (dialer, listener) = mldsa_pair();
        let (dialer_sink, listener_stream) = mocks::Channel::init();
        let (listener_sink, dialer_stream) = mocks::Channel::init();
        let config = stream_config(listener.clone());
        let handle = context.child("listener").spawn(move |context| async move {
            encrypted::listen(
                context,
                |_| async { true },
                config,
                listener_stream,
                listener_sink,
            )
            .await
        });
        let (mut d_send, mut d_recv) = encrypted::dial(
            context,
            stream_config(dialer.clone()),
            listener.public_key(),
            dialer_stream,
            dialer_sink,
        )
        .await
        .unwrap();
        let (peer, mut l_send, mut l_recv) = handle.await.unwrap().unwrap();
        assert_eq!(peer, dialer.public_key());
        for msg in [&b"vote"[..], &[3u8; 1000][..]] {
            d_send.send(msg).await.unwrap();
            assert_eq!(l_recv.recv().await.unwrap().coalesce(), msg);
            l_send.send(msg).await.unwrap();
            assert_eq!(d_recv.recv().await.unwrap().coalesce(), msg);
        }
    });
}

// ---------------------------------------------------------------- known answer

/// Runs a full handshake from fixed keys, clocks and RNG streams, and hashes every byte on the
/// wire plus one encrypted message each way. Any change to the transcript schema, labels, key
/// derivation, message layout or ML-KEM/X25519 use changes this hash.
fn known_answer<S: Signer>(dialer: S, listener: S) -> String {
    const KAT_NS: &[u8] = b"blockula/blockula-kat/p2p_STREAM";
    let kat_rng =
        |label: &'static [u8]| Transcript::new(b"blockula/hybrid-kat", Version::V1).noise(label);
    let (d_state, syn) = dial_start(
        kat_rng(b"dialer"),
        Context::new(
            KAT_NS,
            1_760_000_000_000,
            1_759_999_990_000..1_760_000_001_000,
            dialer.clone(),
            listener.public_key(),
        ),
    );
    let syn_bytes = syn.encode();
    let (l_state, syn_ack) = listen_start(
        kat_rng(b"listener"),
        Context::new(
            KAT_NS,
            1_760_000_000_250,
            1_759_999_990_250..1_760_000_001_250,
            listener,
            dialer.public_key(),
        ),
        syn,
    )
    .unwrap();
    let syn_ack_bytes = syn_ack.encode();
    let (ack, mut d_send, _) = dial_end(d_state, syn_ack).unwrap();
    let ack_bytes = ack.encode();
    let (mut l_send, _) = listen_end(l_state, ack).unwrap();

    let mut wire = blake3::Hasher::new();
    wire.update(&syn_bytes);
    wire.update(&syn_ack_bytes);
    wire.update(&ack_bytes);
    wire.update(&d_send.send(b"dialer to listener").unwrap());
    wire.update(&l_send.send(b"listener to dialer").unwrap());
    wire.finalize().to_hex().to_string()
}

#[test]
fn known_answer_ed25519() {
    let (d, l) = (
        ed25519::PrivateKey::from_seed(1),
        ed25519::PrivateKey::from_seed(2),
    );
    assert_eq!(
        known_answer(d, l),
        "06e60795302a4df1a47dc1291f0bb37566166c18b2523175641e7d76fba118df"
    );
}

#[test]
fn known_answer_mldsa() {
    let (d, l) = mldsa_pair();
    assert_eq!(
        known_answer(d, l),
        "e3c03a1fa0d9a5b921ccdd78c9b2399ce19bc714cd965f2f122cdfe0c377025e"
    );
}

#[test]
fn random_identities_handshake() {
    // Sanity: keys drawn from an RNG (the path the node's `Random` impls take) work too.
    let mut r = rng(b"keys");
    handshake(
        mldsa::PrivateKey::random(&mut r),
        mldsa::PrivateKey::random(&mut r),
    )
    .unwrap();
}

#[test]
#[ignore = "timing; run with --release -- --ignored --nocapture"]
fn handshake_cost() {
    use std::time::Instant;
    const N: u32 = 200;
    let (dialer, listener) = mldsa_pair();

    let start = Instant::now();
    for _ in 0..N {
        let (d, syn) = dial_start(rng(b"d"), ctx(dialer.clone(), listener.public_key()));
        let (l, syn_ack) =
            listen_start(rng(b"l"), ctx(listener.clone(), dialer.public_key()), syn).unwrap();
        let (ack, _, _) = dial_end(d, syn_ack).unwrap();
        listen_end(l, ack).unwrap();
    }
    let hybrid = start.elapsed() / N;

    let start = Instant::now();
    for _ in 0..N {
        let classic_ctx = |me: &mldsa::PrivateKey, peer: &mldsa::PrivateKey| {
            classic::Context::new(NS, NOW, WINDOW, me.clone(), peer.public_key())
        };
        let (d, syn) = classic::dial_start(rng(b"d"), classic_ctx(&dialer, &listener));
        let (l, syn_ack) =
            classic::listen_start(rng(b"l"), classic_ctx(&listener, &dialer), syn).unwrap();
        let (ack, _, _) = classic::dial_end(d, syn_ack).unwrap();
        classic::listen_end(l, ack).unwrap();
    }
    let classical = start.elapsed() / N;

    let start = Instant::now();
    for _ in 0..N {
        let (secret, share) = generate(&mut rng(b"d"));
        let (reply, _) = encapsulate(&mut rng(b"l"), &share).unwrap();
        decapsulate(secret, &reply).unwrap();
    }
    let key_exchange = start.elapsed() / N;

    println!(
        "per handshake with ML-DSA-65 identities: hybrid {hybrid:?}, classical {classical:?}; \
         hybrid key exchange alone (both sides) {key_exchange:?}"
    );
}
