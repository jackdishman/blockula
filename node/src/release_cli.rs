//! `blockula release keyid|sign|verify|selftest`: offline ML-DSA-65 signatures on release
//! checksums (docs/OPERATIONS.md "Releases"). The format is `scripts/release-sig.mjs`'s, and both
//! reproduce the same known-answer vector:
//!
//! * message: `"BLOCKULA_RELEASE_V1"` followed by the exact bytes of the file (`checksums.txt`);
//! * signature: FIPS 204 ML-DSA-65, empty context, deterministic (`crypto::Keys::sign`);
//! * key id: hex BLAKE3 of the public key;
//! * `.sig` file: one JSON line `{"format","key_id","public_key","signature"}` (hex).
//!
//! `scripts/install.sh` runs `blockula release verify` (from an already-installed binary) to
//! check a new release before installing it.

use crate::crypto::{self, Keys};
use anyhow::{anyhow, bail, ensure, Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FORMAT: &str = "blockula-release-signature-v1";
const DOMAIN: &[u8] = b"BLOCKULA_RELEASE_V1";

#[derive(Subcommand)]
pub enum ReleaseCmd {
    /// Print the release key id (hex BLAKE3 of the public key) of a key file
    Keyid {
        /// `blockula keygen` JSON, or a bare 32-byte hex seed
        #[arg(long)]
        key: PathBuf,
    },
    /// Sign a file (checksums.txt) on the offline machine; prints the .sig JSON line
    Sign {
        /// `blockula keygen` JSON, or a bare 32-byte hex seed
        #[arg(long)]
        key: PathBuf,
        file: PathBuf,
    },
    /// Verify a file's signature against the published release key id
    Verify {
        /// The release key id to trust (64 hex chars, published in SECURITY.md and OPERATIONS.md)
        #[arg(long = "key-id")]
        key_id: String,
        /// The signature file (or give it after the file)
        #[arg(long)]
        sig: Option<PathBuf>,
        file: PathBuf,
        sig_file: Option<PathBuf>,
    },
    /// Check this binary against the known-answer vector scripts/release-sig.mjs also passes
    Selftest,
}

/// A `.sig` file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseSignature {
    pub format: String,
    pub key_id: String,
    pub public_key: String,
    pub signature: String,
}

fn message(file: &[u8]) -> Vec<u8> {
    [DOMAIN, file].concat()
}

fn key_id(public_key: &[u8]) -> String {
    hex::encode(crypto::hash(public_key))
}

/// A key file: `blockula keygen` JSON (whose `address`, if present, must match the seed) or a
/// bare hex seed.
fn load_key(path: &Path) -> Result<Keys> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let text = text.trim();
    let (seed, address) = if text.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(text).with_context(|| format!("{} isn't valid JSON", path.display()))?;
        let seed = v["seed"].as_str().ok_or_else(|| anyhow!("{} has no \"seed\" field (blockula keygen output)", path.display()))?.to_string();
        (seed, v["address"].as_str().map(String::from))
    } else {
        (text.to_string(), None)
    };
    let seed: [u8; 32] = hex::decode(&seed).ok().and_then(|s| s.try_into().ok()).ok_or_else(|| anyhow!("{} must hold a 32-byte hex seed", path.display()))?;
    let keys = Keys::from_seed(seed);
    if let Some(address) = address {
        let (_, bytes) = crypto::parse_address(&address).ok_or_else(|| anyhow!("{} has an invalid address {address}", path.display()))?;
        ensure!(bytes == crypto::hash(&keys.public_key()), "{}: the seed doesn't match address {address}", path.display());
    }
    Ok(keys)
}

pub fn sign(keys: &Keys, file: &[u8]) -> ReleaseSignature {
    let public_key = keys.public_key();
    ReleaseSignature { format: FORMAT.into(), key_id: key_id(&public_key), public_key: hex::encode(&public_key), signature: hex::encode(keys.sign(&message(file))) }
}

/// `Ok` only if `sig` is a valid signature on `file` by the key whose id is `expected_key_id`.
pub fn verify(sig: &ReleaseSignature, file: &[u8], expected_key_id: &str) -> Result<()> {
    ensure!(sig.format == FORMAT, "unknown signature format {}", sig.format);
    let public_key = hex::decode(&sig.public_key).map_err(|_| anyhow!("public_key isn't hex"))?;
    let signature = hex::decode(&sig.signature).map_err(|_| anyhow!("signature isn't hex"))?;
    ensure!(public_key.len() == crypto::PUBLIC_KEY_LEN && signature.len() == crypto::SIGNATURE_LEN, "malformed ML-DSA-65 key or signature");
    let id = key_id(&public_key);
    ensure!(sig.key_id == id, "key_id doesn't match the public key");
    ensure!(expected_key_id.len() == 64 && crypto::is_lower_hex(expected_key_id), "pass the release key id to trust (--key-id, 64 lowercase hex chars)");
    ensure!(id == expected_key_id, "signed by key {id}, not the release key {expected_key_id}");
    crypto::verify(&public_key, &message(file), &signature).map_err(|_| anyhow!("INVALID signature"))
}

/// Shared with `scripts/release-sig.mjs` (VECTOR): seed 07×32 signs this file.
pub const VECTOR_SEED: [u8; 32] = [7; 32];
pub const VECTOR_FILE: &str = "0123abcd  blockula-x86_64-unknown-linux-gnu.tar.gz\n";
pub const VECTOR_KEY_ID: &str = "b470b86b94891081659c41ca43129ca4fd136cd247ecbd0b9d3f037f5215fedd";
pub const VECTOR_SIGNATURE_BLAKE3: &str = "390d0f3d0c35684e1f9bf14044eaa58edb00ea8849c6eaecc14d782f2338b579";

/// The known-answer vector, then the rejections `release-sig.mjs selftest` checks.
pub fn selftest() -> Result<()> {
    let keys = Keys::from_seed(VECTOR_SEED);
    let file = VECTOR_FILE.as_bytes();
    let sig = sign(&keys, file);
    let got = hex::encode(crypto::hash(&hex::decode(&sig.signature)?));
    ensure!(sig.key_id == VECTOR_KEY_ID && got == VECTOR_SIGNATURE_BLAKE3, "known-answer mismatch: key id {}, signature BLAKE3 {got}", sig.key_id);
    verify(&sig, file, VECTOR_KEY_ID)?;
    let other = Keys::from_seed([0; 32]);
    let mut flipped = hex::decode(&sig.signature)?;
    flipped[100] ^= 1;
    let rejects: [(&str, Result<()>); 5] = [
        ("a modified file", verify(&sig, VECTOR_FILE.replace("0123", "0124").as_bytes(), VECTOR_KEY_ID)),
        ("another key", verify(&self::sign(&other, file), file, VECTOR_KEY_ID)),
        ("a swapped public key", verify(&ReleaseSignature { public_key: hex::encode(other.public_key()), ..sig.clone() }, file, VECTOR_KEY_ID)),
        ("a modified signature", verify(&ReleaseSignature { signature: hex::encode(&flipped), ..sig.clone() }, file, VECTOR_KEY_ID)),
        ("a tx-domain signature", verify(&ReleaseSignature { signature: hex::encode(keys.sign(file)), ..sig.clone() }, file, VECTOR_KEY_ID)),
    ];
    for (what, r) in rejects {
        if r.is_ok() {
            bail!("accepted {what}");
        }
    }
    Ok(())
}

pub fn run(cmd: ReleaseCmd) -> Result<()> {
    match cmd {
        ReleaseCmd::Keyid { key } => println!("{}", key_id(&load_key(&key)?.public_key())),
        ReleaseCmd::Sign { key, file } => {
            let keys = load_key(&key)?;
            let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let sig = sign(&keys, &bytes);
            verify(&sig, &bytes, &sig.key_id)?;
            println!("{}", serde_json::to_string(&sig)?);
            eprintln!("signed {} with key {}", file.display(), sig.key_id);
        }
        ReleaseCmd::Verify { key_id, sig, file, sig_file } => {
            let sig_path = match (sig, sig_file) {
                (Some(s), None) | (None, Some(s)) => s,
                _ => bail!("give the signature file once: --sig <file.sig> <file>, or <file> <file.sig>"),
            };
            let sig: ReleaseSignature = serde_json::from_slice(&std::fs::read(&sig_path).with_context(|| format!("reading {}", sig_path.display()))?).with_context(|| format!("{} isn't a release signature", sig_path.display()))?;
            let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            verify(&sig, &bytes, &key_id).map_err(|e| anyhow!("✗ {}: {e}", file.display()))?;
            println!("✓ {} is signed by release key {key_id}", file.display());
        }
        ReleaseCmd::Selftest => {
            selftest()?;
            println!("✓ release signature selftest passed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vector `scripts/release-sig.mjs` checks (with @noble/post-quantum): the two
    /// implementations produce byte-identical signatures.
    #[test]
    fn known_answer_vector_and_rejections() {
        selftest().unwrap();
    }

    #[test]
    fn sign_and_verify_through_files() {
        let dir = std::env::temp_dir().join(format!("blockula-release-{}-{}", crate::chain::now_ms(), std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let keys = Keys::from_seed([9; 32]);
        let json = dir.join("release-key.json");
        std::fs::write(&json, serde_json::json!({ "address": keys.address(), "seed": keys.seed_hex() }).to_string()).unwrap();
        let bare = dir.join("seed.hex");
        std::fs::write(&bare, keys.seed_hex()).unwrap();
        assert_eq!(load_key(&json).unwrap().public_key(), load_key(&bare).unwrap().public_key());
        // A keygen file whose address doesn't match its seed is refused.
        let wrong = dir.join("wrong.json");
        std::fs::write(&wrong, serde_json::json!({ "address": Keys::from_seed([1; 32]).address(), "seed": keys.seed_hex() }).to_string()).unwrap();
        assert!(load_key(&wrong).err().unwrap().to_string().contains("doesn't match"));

        let file = dir.join("checksums.txt");
        std::fs::write(&file, "abc  blockula.tar.gz\n").unwrap();
        let sig = sign(&keys, &std::fs::read(&file).unwrap());
        let line = serde_json::to_string(&sig).unwrap();
        let back: ReleaseSignature = serde_json::from_str(&line).unwrap();
        let id = key_id(&keys.public_key());
        verify(&back, &std::fs::read(&file).unwrap(), &id).unwrap();
        assert!(verify(&back, b"abc  blockula.tar.gz\nextra\n", &id).is_err());
        assert!(verify(&back, &std::fs::read(&file).unwrap(), &"00".repeat(32)).unwrap_err().to_string().contains("not the release key"));
        assert!(verify(&back, &std::fs::read(&file).unwrap(), "short").unwrap_err().to_string().contains("--key-id"));
        assert!(serde_json::from_str::<ReleaseSignature>(&line.replace("\"format\"", "\"extra\":1,\"format\"")).is_err(), "unknown fields");
        // The CLI paths.
        let sig_path = dir.join("checksums.txt.sig");
        std::fs::write(&sig_path, format!("{line}\n")).unwrap();
        run(ReleaseCmd::Verify { key_id: id.clone(), sig: Some(sig_path.clone()), file: file.clone(), sig_file: None }).unwrap();
        run(ReleaseCmd::Verify { key_id: id.clone(), sig: None, file: file.clone(), sig_file: Some(sig_path.clone()) }).unwrap();
        assert!(run(ReleaseCmd::Verify { key_id: id, sig: None, file: file.clone(), sig_file: None }).is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
