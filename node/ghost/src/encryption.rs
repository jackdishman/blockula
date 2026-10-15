//! Note encryption: ML-KEM-768 to the recipient's address, then XChaCha20-Poly1305.
//!
//! ```text
//! (ct_kem, ss)  = ML-KEM-768.Encaps(ek)                       1,088 B
//! key ‖ nonce   = BLAKE3-KDF("… note encryption", ss ‖ ct_kem ‖ cm)
//! enc           = ct_kem ‖ XChaCha20-Poly1305(key, nonce, aad = cm,
//!                          version ‖ value ‖ rseed ‖ memo)    1,401 B
//! ock ‖ nonce'  = BLAKE3-KDF("… outgoing cipher key", ovk ‖ cm ‖ ct_kem)
//! out           = XChaCha20-Poly1305(ock, nonce', aad = cm, owner ‖ ss)   80 B
//! ```
//!
//! Each key encrypts one message, so the nonce is derived, not sent. The ciphertext carries
//! nothing that identifies the recipient. Poly1305 isn't key-committing, so a recipient accepts
//! a note only after recomputing its commitment and finding the on-chain `cm` (robustness).

use chacha20poly1305::aead::{AeadInOut, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ml_kem::kem::{Ciphertext, Decapsulate};
use ml_kem::{B32, MlKem768, ml_kem_768};
use rand::CryptoRng;

use crate::field::{DIGEST_BYTES, Digest, digest_from_bytes, digest_to_bytes, xof_bytes};
use crate::keys::{Address, IncomingViewingKey, OutgoingViewingKey, SpendingKey};
use crate::note::{NOTE_VERSION, Note};

/// Bytes of memo in every note (fixed, so ciphertexts don't leak memo length).
pub const MEMO_BYTES: usize = 256;
/// version (1) ‖ value (8) ‖ rseed (32) ‖ memo.
pub const NOTE_PLAINTEXT_BYTES: usize = 1 + 8 + 32 + MEMO_BYTES;
/// ML-KEM-768 ciphertext.
pub const KEM_CIPHERTEXT_BYTES: usize = 1088;
/// Poly1305 tag.
pub const TAG_BYTES: usize = 16;
/// The note ciphertext: `ct_kem ‖ AEAD(plaintext)`.
pub const ENC_CIPHERTEXT_BYTES: usize = KEM_CIPHERTEXT_BYTES + NOTE_PLAINTEXT_BYTES + TAG_BYTES;
/// The outgoing ciphertext: `AEAD(owner ‖ ss)`.
pub const OUT_CIPHERTEXT_BYTES: usize = DIGEST_BYTES + 32 + TAG_BYTES;

/// One output's ciphertexts, published next to its commitment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncryptedNote {
    /// For the recipient ([`ENC_CIPHERTEXT_BYTES`]).
    pub enc: Vec<u8>,
    /// For the sender's outgoing viewing key ([`OUT_CIPHERTEXT_BYTES`]).
    pub out: Vec<u8>,
}

/// A decrypted note and its memo. `Debug` prints `DecryptedNote(..)`.
#[derive(Clone, PartialEq, Eq)]
pub struct DecryptedNote {
    /// The note (its commitment matched).
    pub note: Note,
    /// The memo.
    pub memo: [u8; MEMO_BYTES],
}

redacted_debug!(DecryptedNote, TrialDecryptor);

/// Encryption failed because the note isn't addressed to `recipient`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrongRecipient;

fn note_key(ss: &[u8], ct_kem: &[u8], cm: &[u8; DIGEST_BYTES]) -> ([u8; 32], [u8; 24]) {
    let mut input = Vec::with_capacity(32 + KEM_CIPHERTEXT_BYTES + DIGEST_BYTES);
    input.extend_from_slice(ss);
    input.extend_from_slice(ct_kem);
    input.extend_from_slice(cm);
    split_key(xof_bytes::<56>("blockula ghost v1 note encryption", &input))
}

fn outgoing_key(ovk: &OutgoingViewingKey, cm: &[u8; DIGEST_BYTES], ct_kem: &[u8]) -> ([u8; 32], [u8; 24]) {
    let mut input = Vec::with_capacity(32 + DIGEST_BYTES + KEM_CIPHERTEXT_BYTES);
    input.extend_from_slice(&ovk.0);
    input.extend_from_slice(cm);
    input.extend_from_slice(ct_kem);
    split_key(xof_bytes::<56>("blockula ghost v1 outgoing cipher key", &input))
}

fn split_key(okm: [u8; 56]) -> ([u8; 32], [u8; 24]) {
    (okm[..32].try_into().expect("32"), okm[32..].try_into().expect("24"))
}

fn seal(key: &[u8; 32], nonce: &[u8; 24], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut buf = plaintext.to_vec();
    cipher.encrypt_in_place(&XNonce::from(*nonce), aad, &mut buf).expect("buffer grows");
    buf
}

fn open(key: &[u8; 32], nonce: &[u8; 24], aad: &[u8], ciphertext: &[u8]) -> Option<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut buf = ciphertext.to_vec();
    cipher.decrypt_in_place(&XNonce::from(*nonce), aad, &mut buf).ok()?;
    Some(buf)
}

fn note_plaintext(note: &Note, memo: &[u8; MEMO_BYTES]) -> [u8; NOTE_PLAINTEXT_BYTES] {
    let mut pt = [0u8; NOTE_PLAINTEXT_BYTES];
    pt[0] = NOTE_VERSION;
    pt[1..9].copy_from_slice(&note.value.to_le_bytes());
    pt[9..41].copy_from_slice(&note.rseed);
    pt[41..].copy_from_slice(memo);
    pt
}

/// Encrypts `note` to `recipient`. With `ovk`, the sender can recover it later; without (e.g.
/// dummy outputs), the outgoing ciphertext is random bytes of the same length.
pub fn encrypt_note<R: CryptoRng + ?Sized>(
    note: &Note,
    recipient: &Address,
    memo: &[u8; MEMO_BYTES],
    ovk: Option<&OutgoingViewingKey>,
    rng: &mut R,
) -> Result<EncryptedNote, WrongRecipient> {
    if note.owner != recipient.owner() {
        return Err(WrongRecipient);
    }
    let cm = digest_to_bytes(&note.commitment());
    let mut m = [0u8; 32];
    rng.fill_bytes(&mut m);
    let (ct_kem, ss) = recipient.encapsulation_key().encapsulate_deterministic(&B32::from(m));
    let (key, nonce) = note_key(&ss, &ct_kem, &cm);
    let mut enc = ct_kem.to_vec();
    enc.extend_from_slice(&seal(&key, &nonce, &cm, &note_plaintext(note, memo)));
    let out = match ovk {
        Some(ovk) => {
            let (ock, nonce) = outgoing_key(ovk, &cm, &ct_kem);
            let mut pt = [0u8; DIGEST_BYTES + 32];
            pt[..DIGEST_BYTES].copy_from_slice(&digest_to_bytes(&note.owner));
            pt[DIGEST_BYTES..].copy_from_slice(&ss);
            seal(&ock, &nonce, &cm, &pt)
        }
        None => {
            let mut out = vec![0u8; OUT_CIPHERTEXT_BYTES];
            rng.fill_bytes(&mut out);
            out
        }
    };
    Ok(EncryptedNote { enc, out })
}

/// A zero-value output to a throwaway address, for padding an action to two outputs. Its
/// commitment and ciphertexts look like any other output's.
pub fn dummy_output<R: CryptoRng + ?Sized>(rng: &mut R) -> (Note, EncryptedNote) {
    let mut seed = [0u8; 32];
    rng.fill_bytes(&mut seed);
    let address = SpendingKey::from_seed(seed).address();
    let mut rseed = [0u8; 32];
    rng.fill_bytes(&mut rseed);
    let note = Note { value: 0, owner: address.owner(), rseed };
    let enc = encrypt_note(&note, &address, &[0; MEMO_BYTES], None, rng).expect("own address");
    (note, enc)
}

/// Decrypts the AEAD part of `enc` with a known shared secret and checks the commitment.
fn open_note(ss: &[u8], owner: Digest, cm: &Digest, enc: &[u8]) -> Option<DecryptedNote> {
    if enc.len() != ENC_CIPHERTEXT_BYTES {
        return None;
    }
    let cm_bytes = digest_to_bytes(cm);
    let (ct_kem, sealed) = enc.split_at(KEM_CIPHERTEXT_BYTES);
    let (key, nonce) = note_key(ss, ct_kem, &cm_bytes);
    let pt = open(&key, &nonce, &cm_bytes, sealed)?;
    if pt.len() != NOTE_PLAINTEXT_BYTES || pt[0] != NOTE_VERSION {
        return None;
    }
    let note = Note {
        value: u64::from_le_bytes(pt[1..9].try_into().ok()?),
        owner,
        rseed: pt[9..41].try_into().ok()?,
    };
    // Robustness: accept only if the plaintext really opens the on-chain commitment.
    if note.commitment() != *cm {
        return None;
    }
    Some(DecryptedNote { note, memo: pt[41..].try_into().ok()? })
}

/// Trial decryption for one incoming viewing key. Holds the expanded ML-KEM key, so scanning
/// many outputs doesn't re-run key generation. The key is wiped on drop, and `Debug` prints
/// `TrialDecryptor(..)`.
pub struct TrialDecryptor {
    owner: Digest,
    dk: ml_kem_768::DecapsulationKey,
}

impl TrialDecryptor {
    /// Prepares to scan with `ivk`.
    pub fn new(ivk: &IncomingViewingKey) -> Self {
        Self { owner: ivk.owner(), dk: ivk.decapsulation_key() }
    }

    /// Returns the note if output `(cm, enc)` is ours, `None` otherwise (any other key, any
    /// tampering, a wrong `cm`).
    pub fn try_decrypt(&self, cm: &Digest, enc: &[u8]) -> Option<DecryptedNote> {
        if enc.len() != ENC_CIPHERTEXT_BYTES {
            return None;
        }
        let ct = Ciphertext::<MlKem768>::try_from(&enc[..KEM_CIPHERTEXT_BYTES]).ok()?;
        // ML-KEM uses implicit rejection: a foreign ciphertext yields a pseudorandom secret,
        // and the AEAD then fails.
        let ss = self.dk.decapsulate(&ct);
        open_note(&ss, self.owner, cm, enc)
    }
}

/// [`TrialDecryptor::try_decrypt`] for a single output.
pub fn try_decrypt_incoming(ivk: &IncomingViewingKey, cm: &Digest, enc: &[u8]) -> Option<DecryptedNote> {
    TrialDecryptor::new(ivk).try_decrypt(cm, enc)
}

/// Recovers a note this wallet sent, from its outgoing viewing key.
pub fn try_decrypt_outgoing(
    ovk: &OutgoingViewingKey,
    cm: &Digest,
    enc: &[u8],
    out: &[u8],
) -> Option<DecryptedNote> {
    if enc.len() != ENC_CIPHERTEXT_BYTES || out.len() != OUT_CIPHERTEXT_BYTES {
        return None;
    }
    let cm_bytes = digest_to_bytes(cm);
    let (ock, nonce) = outgoing_key(ovk, &cm_bytes, &enc[..KEM_CIPHERTEXT_BYTES]);
    let pt = open(&ock, &nonce, &cm_bytes, out)?;
    let owner = digest_from_bytes(pt[..DIGEST_BYTES].try_into().ok()?)?;
    open_note(&pt[DIGEST_BYTES..], owner, cm, enc)
}
