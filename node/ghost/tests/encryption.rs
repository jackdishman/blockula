//! Note encryption: round trip, wrong keys, tampering, outgoing recovery.

use blockula_ghost::encryption::{ENC_CIPHERTEXT_BYTES, KEM_CIPHERTEXT_BYTES, MEMO_BYTES, OUT_CIPHERTEXT_BYTES, WrongRecipient};
use blockula_ghost::field::xof_to_digest;
use blockula_ghost::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

struct Setup {
    sender: SpendingKey,
    recipient: SpendingKey,
    note: Note,
    memo: [u8; MEMO_BYTES],
    enc: EncryptedNote,
}

fn setup(seed: u64) -> Setup {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut bytes = || {
        let mut b = [0u8; 32];
        rng.fill_bytes(&mut b);
        b
    };
    let sender = SpendingKey::from_seed(bytes());
    let recipient = SpendingKey::from_seed(bytes());
    let note = Note { value: 42_000_000, owner: recipient.address().owner(), rseed: bytes() };
    let mut memo = [0u8; MEMO_BYTES];
    memo[..11].copy_from_slice(b"hello ghost");
    let mut rng = StdRng::seed_from_u64(seed + 1);
    let enc = encrypt_note(&note, &recipient.address(), &memo, Some(&sender.full_viewing_key().outgoing()), &mut rng).unwrap();
    Setup { sender, recipient, note, memo, enc }
}

#[test]
fn round_trip_and_sizes() {
    let s = setup(1);
    assert_eq!(s.enc.enc.len(), ENC_CIPHERTEXT_BYTES);
    assert_eq!(s.enc.enc.len(), 1401);
    assert_eq!(s.enc.out.len(), OUT_CIPHERTEXT_BYTES);
    assert_eq!(s.enc.out.len(), 80);
    let ivk = s.recipient.full_viewing_key().incoming();
    let got = try_decrypt_incoming(&ivk, &s.note.commitment(), &s.enc.enc).expect("decrypts");
    assert_eq!(got.note, s.note);
    assert_eq!(got.memo, s.memo);
    // A scanner reused across outputs gives the same answer.
    let scanner = TrialDecryptor::new(&ivk);
    assert_eq!(scanner.try_decrypt(&s.note.commitment(), &s.enc.enc), Some(got));
}

#[test]
fn wrong_keys_get_nothing() {
    let s = setup(2);
    let cm = s.note.commitment();
    // The sender's own incoming key, and a stranger's.
    assert!(try_decrypt_incoming(&s.sender.full_viewing_key().incoming(), &cm, &s.enc.enc).is_none());
    let stranger = SpendingKey::from_seed([0xee; 32]);
    assert!(try_decrypt_incoming(&stranger.full_viewing_key().incoming(), &cm, &s.enc.enc).is_none());
    // The recipient's outgoing key isn't the sender's.
    assert!(try_decrypt_outgoing(&s.recipient.full_viewing_key().outgoing(), &cm, &s.enc.enc, &s.enc.out).is_none());
}

#[test]
fn tampering_is_detected() {
    let s = setup(3);
    let ivk = s.recipient.full_viewing_key().incoming();
    let cm = s.note.commitment();
    // Flip one bit in the KEM ciphertext, the AEAD body and the tag.
    for at in [0, KEM_CIPHERTEXT_BYTES - 1, KEM_CIPHERTEXT_BYTES, KEM_CIPHERTEXT_BYTES + 100, ENC_CIPHERTEXT_BYTES - 1] {
        let mut enc = s.enc.enc.clone();
        enc[at] ^= 0x10;
        assert!(try_decrypt_incoming(&ivk, &cm, &enc).is_none(), "flip at {at}");
    }
    // Truncated, extended, or attached to another commitment.
    assert!(try_decrypt_incoming(&ivk, &cm, &s.enc.enc[..ENC_CIPHERTEXT_BYTES - 1]).is_none());
    let mut longer = s.enc.enc.clone();
    longer.push(0);
    assert!(try_decrypt_incoming(&ivk, &cm, &longer).is_none());
    let other_cm = xof_to_digest("other", b"cm");
    assert!(try_decrypt_incoming(&ivk, &other_cm, &s.enc.enc).is_none());
    // Outgoing ciphertext tampering.
    let ovk = s.sender.full_viewing_key().outgoing();
    let mut out = s.enc.out.clone();
    out[5] ^= 1;
    assert!(try_decrypt_outgoing(&ovk, &cm, &s.enc.enc, &out).is_none());
}

#[test]
fn commitment_mismatch_is_rejected_even_with_the_right_key() {
    // Robustness: a ciphertext that decrypts but doesn't open the on-chain commitment (here,
    // a note to the right owner whose commitment we lie about) is refused.
    let s = setup(4);
    let ivk = s.recipient.full_viewing_key().incoming();
    let mut wrong = s.note;
    wrong.value += 1;
    assert!(try_decrypt_incoming(&ivk, &wrong.commitment(), &s.enc.enc).is_none());
}

#[test]
fn outgoing_recovery() {
    let s = setup(5);
    let ovk = s.sender.full_viewing_key().outgoing();
    let got = try_decrypt_outgoing(&ovk, &s.note.commitment(), &s.enc.enc, &s.enc.out).expect("recovers");
    assert_eq!(got.note, s.note);
    assert_eq!(got.memo, s.memo);
}

#[test]
fn without_ovk_the_outgoing_part_is_noise() {
    let s = setup(6);
    let mut rng = StdRng::seed_from_u64(60);
    let enc = encrypt_note(&s.note, &s.recipient.address(), &s.memo, None, &mut rng).unwrap();
    assert_eq!(enc.out.len(), OUT_CIPHERTEXT_BYTES);
    let ovk = s.sender.full_viewing_key().outgoing();
    assert!(try_decrypt_outgoing(&ovk, &s.note.commitment(), &enc.enc, &enc.out).is_none());
    // The recipient still decrypts.
    let ivk = s.recipient.full_viewing_key().incoming();
    assert!(try_decrypt_incoming(&ivk, &s.note.commitment(), &enc.enc).is_some());
}

#[test]
fn encrypting_to_the_wrong_address_fails() {
    let s = setup(7);
    let mut rng = StdRng::seed_from_u64(70);
    let r = encrypt_note(&s.note, &s.sender.address(), &s.memo, None, &mut rng);
    assert_eq!(r, Err(WrongRecipient));
}

#[test]
fn ciphertexts_are_fresh() {
    // Same note, two encryptions: unrelated bytes (new KEM randomness each time).
    let s = setup(8);
    let mut rng = StdRng::seed_from_u64(80);
    let again = encrypt_note(&s.note, &s.recipient.address(), &s.memo, None, &mut rng).unwrap();
    assert_ne!(again.enc[..KEM_CIPHERTEXT_BYTES], s.enc.enc[..KEM_CIPHERTEXT_BYTES]);
    assert_ne!(again.enc[KEM_CIPHERTEXT_BYTES..], s.enc.enc[KEM_CIPHERTEXT_BYTES..]);
}

#[test]
fn random_garbage_never_decrypts() {
    let s = setup(9);
    let ivk = s.recipient.full_viewing_key().incoming();
    let scanner = TrialDecryptor::new(&ivk);
    let mut rng = StdRng::seed_from_u64(90);
    for _ in 0..200 {
        let mut enc = vec![0u8; ENC_CIPHERTEXT_BYTES];
        rng.fill_bytes(&mut enc);
        assert!(scanner.try_decrypt(&s.note.commitment(), &enc).is_none());
    }
}

#[test]
fn dummy_outputs_have_the_same_shape() {
    let mut rng = StdRng::seed_from_u64(100);
    let (note, enc) = dummy_output(&mut rng);
    assert_eq!(note.value, 0);
    assert_eq!(enc.enc.len(), ENC_CIPHERTEXT_BYTES);
    assert_eq!(enc.out.len(), OUT_CIPHERTEXT_BYTES);
    let (note2, _) = dummy_output(&mut rng);
    assert_ne!(note.owner, note2.owner);
}
