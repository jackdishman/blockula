//! SEC-2 known-answer tests for ML-DSA-65 (FIPS 204), run through the paths the node signs and
//! verifies with: `crypto::Keys` / `crypto::verify` (txs: empty context) and `pq::PrivateKey` /
//! `pq::PublicKey` (consensus votes, PoPs, handoffs: the namespace is the context). Raw `ml-dsa`
//! calls cover the groups no node path reaches (hedged, `Sign_internal`, external µ).
//!
//! * NIST ACVP (`testdata/acvp/`) and Wycheproof (`testdata/wycheproof/`) vectors, trimmed by
//!   `testdata/acvp/trim.py`. A failure here is a critical finding, never a flaky test.
//! * The Rust half of the Rust⇄TS vectors (`testdata/cross/`): signatures the node makes that
//!   `@noble/post-quantum` must reproduce byte for byte and accept, and SDK signatures the node
//!   must accept. `sdk-ts/test/kat.ts` is the other half; `testdata/cross/README.md` regenerates.
#![allow(deprecated)] // `ExpandedSigningKey::to_expanded` is the only way to compare NIST's expanded `sk`.

use crate::{consensus, crypto, epochs, pq, types};
use commonware_codec::{Decode as _, DecodeExt as _, Encode as _};
use commonware_consensus::{
    simplex::{
        scheme::Namespace,
        types::{Finalization, Finalize, Notarize, Nullify, Proposal, Subject},
    },
    types::{Epoch, Round, View},
};
use commonware_cryptography::{
    Signer as _, Verifier as _,
    blake3::Digest,
    certificate::{Attestation, Namespace as _, Subject as _, Verifier as _},
};
use commonware_parallel::Sequential;
use commonware_utils::{Participant, iter::NonEmpty, ordered::Set, sys_rng};
use ml_dsa::{B32, EncodedSignature, EncodedVerifyingKey, MlDsa65, Seed, Signature, SigningKey, VerifyingKey, signature::Keypair as _};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{convert::Infallible, path::PathBuf};

// ------------------------------------------------------------------------------------ helpers

fn testdata(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata").join(rel)
}

fn load(rel: &str) -> Value {
    let bytes = std::fs::read(testdata(rel)).unwrap_or_else(|e| panic!("testdata/{rel}: {e}"));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("testdata/{rel}: {e}"))
}

/// Hex field (ACVP uses uppercase, Wycheproof lowercase); missing or null is empty.
fn hx(v: &Value, k: &str) -> Vec<u8> {
    v.get(k).and_then(Value::as_str).map(|s| hex::decode(s).unwrap_or_else(|e| panic!("{k}: {e}"))).unwrap_or_default()
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or_else(|| panic!("missing {k}"))
}

fn sha256(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn seed_of(v: &Value, k: &str) -> [u8; 32] {
    hx(v, k).try_into().expect("32-byte seed")
}

fn mu_of(t: &Value) -> [u8; 64] {
    hx(t, "mu").try_into().expect("64-byte mu")
}

fn vk(pk: &[u8]) -> Option<VerifyingKey<MlDsa65>> {
    Some(VerifyingKey::decode(&EncodedVerifyingKey::<MlDsa65>::try_from(pk).ok()?))
}

fn sig(b: &[u8]) -> Option<Signature<MlDsa65>> {
    Signature::decode(&EncodedSignature::<MlDsa65>::try_from(b).ok()?)
}

/// Consensus-path verification: codec decode, then `verify` with the namespace as context.
fn pq_verify(pk: &[u8], namespace: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    match (pq::PublicKey::from_bytes(pk), pq::Signature::decode(sig)) {
        (Some(pk), Ok(s)) => pk.verify(namespace, msg, &s),
        _ => false,
    }
}

/// Feeds ACVP's `rnd` to the hedged signing API (it draws exactly 32 bytes).
struct FixedRng([u8; 32]);
impl rand_core::TryRng for FixedRng {
    type Error = Infallible;
    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        unreachable!("ml-dsa only fills bytes")
    }
    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        unreachable!("ml-dsa only fills bytes")
    }
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        dst.copy_from_slice(&self.0[..dst.len()]);
        Ok(())
    }
}
impl rand_core::TryCryptoRng for FixedRng {}

/// Pass counts per group (printed with `--nocapture`) and every failure, asserted at the end so
/// one run reports all discrepancies.
#[derive(Default)]
struct Report {
    lines: Vec<String>,
    failures: Vec<String>,
    cases: usize,
}

impl Report {
    fn group<'a>(&mut self, label: &str, tests: impl IntoIterator<Item = &'a Value>, mut run: impl FnMut(&Value) -> Result<(), String>) {
        let (mut pass, mut n) = (0, 0);
        for t in tests {
            n += 1;
            match run(t) {
                Ok(()) => pass += 1,
                Err(e) => self.failures.push(format!("{label} tc {}: {e}", t["tcId"])),
            }
        }
        self.cases += n;
        self.lines.push(format!("{label}: {pass}/{n} passed"));
    }

    fn finish(self, what: &str, expect_cases: usize) {
        for l in &self.lines {
            println!("{what} {l}");
        }
        assert_eq!(self.cases, expect_cases, "{what}: the vector file changed size; update the expected count deliberately");
        assert!(self.failures.is_empty(), "{what}: {} failures (critical, see SEC-2):\n{}", self.failures.len(), self.failures.join("\n"));
    }
}

fn groups(file: &Value) -> &Vec<Value> {
    file["testGroups"].as_array().expect("testGroups")
}

fn tests(group: &Value) -> &Vec<Value> {
    group["tests"].as_array().expect("tests")
}

/// Wycheproof has many one-test groups: flatten to `{group, test}` and report per upstream source.
fn by_source(file: &Value) -> Vec<(String, Vec<Value>)> {
    let mut out: Vec<(String, Vec<Value>)> = vec![];
    for g in groups(file) {
        let source = g["source"]["name"].as_str().unwrap_or("?").to_string();
        let flat = tests(g).iter().map(|t| json!({ "group": g, "test": t, "tcId": t["tcId"] }));
        match out.iter_mut().find(|(s, _)| *s == source) {
            Some((_, v)) => v.extend(flat),
            None => out.push((source, flat.collect())),
        }
    }
    out
}

fn check(ok: bool, why: impl FnOnce() -> String) -> Result<(), String> {
    if ok { Ok(()) } else { Err(why()) }
}

// ------------------------------------------------------------------------------------ ACVP

/// ML-DSA.KeyGen_internal(ξ): both node key types derive NIST's public key, and the expanded
/// secret key matches too.
#[test]
fn acvp_keygen() {
    let file = load("acvp/ML-DSA-65-keyGen.json");
    let mut r = Report::default();
    for g in groups(&file) {
        r.group(&format!("keyGen tg{}", g["tgId"]), tests(g), |t| {
            let seed = seed_of(t, "seed");
            let pk = crypto::Keys::from_seed(seed).public_key();
            check(sha256(&pk) == str_of(t, "pkSha256"), || "crypto::Keys public key".into())?;
            check(sha256(pq::PrivateKey::from_seed_bytes(seed).public_key().as_ref()) == str_of(t, "pkSha256"), || "pq::PrivateKey public key".into())?;
            let sk = SigningKey::<MlDsa65>::from_seed(&Seed::from(seed));
            check(sha256(&sk.expanded_key().to_expanded()) == str_of(t, "skSha256"), || "expanded secret key".into())
        });
    }
    r.finish("ACVP", 25);
}

/// ML-DSA.Sign. The deterministic external group also runs the node's own signing paths
/// (`pq::PrivateKey::sign` with the context as namespace; `crypto::Keys::sign` when it's empty).
#[test]
fn acvp_siggen() {
    let file = load("acvp/ML-DSA-65-sigGen.json");
    let mut r = Report::default();
    for g in groups(&file) {
        assert_eq!(g["keyFormat"], "seed");
        let (iface, ext_mu, det) = (str_of(g, "signatureInterface"), g["externalMu"].as_bool().unwrap(), g["deterministic"].as_bool().unwrap());
        let label = format!("sigGen tg{} {iface}{} {}", g["tgId"], if ext_mu { " µ" } else { "" }, if det { "deterministic" } else { "hedged" });
        r.group(&label, tests(g), |t| {
            let seed = seed_of(t, "seed");
            let sk = SigningKey::<MlDsa65>::from_seed(&Seed::from(seed));
            let esk = sk.expanded_key();
            check(sha256(&sk.verifying_key().encode()) == str_of(t, "pkSha256"), || "public key".into())?;
            let rnd: [u8; 32] = if det { [0; 32] } else { hx(t, "rnd").try_into().expect("32-byte rnd") };
            let (msg, ctx) = (hx(t, "message"), hx(t, "context"));
            let got = match (iface, ext_mu) {
                ("external", false) if det => {
                    let raw = esk.sign_deterministic(&msg, &ctx).map_err(|_| "sign_deterministic failed".to_string())?;
                    let consensus = pq::PrivateKey::from_seed_bytes(seed).sign(&ctx, &msg);
                    check(consensus.as_ref() == raw.encode().as_slice(), || "pq::PrivateKey::sign differs from sign_deterministic".into())?;
                    if ctx.is_empty() {
                        check(crypto::Keys::from_seed(seed).sign(&msg) == raw.encode().as_slice(), || "crypto::Keys::sign differs".into())?;
                    }
                    raw
                }
                ("external", false) => esk.sign_randomized(&msg, &ctx, &mut FixedRng(rnd)).map_err(|_| "sign_randomized failed".to_string())?,
                ("internal", false) => esk.sign_internal(&[&msg], &B32::from(rnd)),
                ("internal", true) if det => esk.sign_mu_deterministic(&mu_of(t).into()),
                ("internal", true) => esk.sign_mu_randomized(&mu_of(t).into(), &mut FixedRng(rnd)).map_err(|_| "sign_mu_randomized failed".to_string())?,
                other => panic!("unexpected sigGen group {other:?}"),
            };
            check(sha256(&got.encode()) == str_of(t, "signatureSha256"), || "signature differs from NIST's".into())
        });
    }
    r.finish("ACVP", 90);
}

/// ML-DSA.Verify. The external group runs through `pq::PublicKey` (codec decode + verify with
/// the context as namespace) and, for an empty context, `crypto::verify`, and both must agree
/// with the raw API.
#[test]
fn acvp_sigver() {
    let file = load("acvp/ML-DSA-65-sigVer.json");
    let mut r = Report::default();
    for g in groups(&file) {
        let (iface, ext_mu) = (str_of(g, "signatureInterface"), g["externalMu"].as_bool().unwrap());
        let label = format!("sigVer tg{} {iface}{}", g["tgId"], if ext_mu { " µ" } else { "" });
        r.group(&label, tests(g), |t| {
            let (pk, sigb, msg, ctx) = (hx(t, "pk"), hx(t, "signature"), hx(t, "message"), hx(t, "context"));
            let want = t["testPassed"].as_bool().unwrap();
            let (key, s) = (vk(&pk).expect("ACVP keys are well-formed"), sig(&sigb));
            let got = match (iface, ext_mu) {
                ("external", false) => {
                    let raw = s.as_ref().is_some_and(|s| key.verify_with_context(&msg, &ctx, s));
                    check(pq_verify(&pk, &ctx, &msg, &sigb) == raw, || "pq::PublicKey::verify disagrees with the raw API".into())?;
                    if ctx.is_empty() {
                        check(crypto::verify(&pk, &msg, &sigb).is_ok() == raw, || "crypto::verify disagrees with the raw API".into())?;
                    }
                    raw
                }
                ("internal", false) => s.as_ref().is_some_and(|s| key.verify_internal(&msg, s)),
                ("internal", true) => s.as_ref().is_some_and(|s| key.verify_mu(&mu_of(t).into(), s)),
                other => panic!("unexpected sigVer group {other:?}"),
            };
            check(got == want, || format!("got {got}, NIST says {want} ({})", str_of(t, "reason")))
        });
    }
    r.finish("ACVP", 45);
}

// ------------------------------------------------------------------------------------ Wycheproof

/// Verifier edge cases: non-canonical hints, ‖z‖∞ at and over the bound, wrong lengths, zero
/// public keys, over-long contexts, rejection-loop corner cases. These are the bug classes of
/// GHSA-5x2r-hc65-25f9 and GHSA-h37v-hp6w-2pp8 (fixed in ml-dsa 0.1.0-rc.4 and rc.5).
#[test]
fn wycheproof_verify() {
    let file = load("wycheproof/mldsa_65_verify_test.json");
    let mut r = Report::default();
    for (source, cases) in by_source(&file) {
        r.group(&format!("verify {source}"), &cases, |c| {
            let (g, t) = (&c["group"], &c["test"]);
            let pk = hx(g, "publicKey");
            let (sigb, msg, ctx) = (hx(t, "sig"), hx(t, "msg"), hx(t, "ctx"));
            let result = str_of(t, "result");
            let raw = matches!((vk(&pk), sig(&sigb)), (Some(k), Some(s)) if k.verify_with_context(&msg, &ctx, &s));
            if result == "acceptable" {
                return Ok(());
            }
            let want = result == "valid";
            check(raw == want, || format!("raw verify got {raw}, want {want} ({})", str_of(t, "comment")))?;
            // pq.rs hashes namespaces over 255 bytes into a context, so only shorter ones map 1:1.
            if ctx.len() <= 255 {
                check(pq_verify(&pk, &ctx, &msg, &sigb) == want, || "pq::PublicKey::verify".into())?;
            }
            if ctx.is_empty() {
                check(crypto::verify(&pk, &msg, &sigb).is_ok() == want, || "crypto::verify".into())?;
            }
            Ok(())
        });
    }
    r.finish("Wycheproof", 210);
}

/// Signing from a seed: rejection-loop and sampling corner cases, external µ, hedged signing,
/// invalid seed lengths and contexts.
#[test]
fn wycheproof_sign() {
    let file = load("wycheproof/mldsa_65_sign_seed_test.json");
    let mut r = Report::default();
    for (source, cases) in by_source(&file) {
        r.group(&format!("sign_seed {source}"), &cases, |c| {
            let (g, t) = (&c["group"], &c["test"]);
            let want = str_of(t, "result") == "valid";
            let got = (|| -> Option<Result<bool, String>> {
                let seed: [u8; 32] = hx(g, "privateSeed").try_into().ok()?; // IncorrectPrivateKeyLength stops here
                let sk = SigningKey::<MlDsa65>::from_seed(&Seed::from(seed));
                if let Some(pk) = g["publicKeySha256"].as_str() {
                    if sha256(&sk.verifying_key().encode()) != pk {
                        return Some(Err("public key".into()));
                    }
                }
                let esk = sk.expanded_key();
                let ctx = hx(t, "ctx");
                let s = match (t.get("msg"), t.get("rnd")) {
                    (Some(_), Some(_)) => esk.sign_randomized(&hx(t, "msg"), &ctx, &mut FixedRng(hx(t, "rnd").try_into().ok()?)).ok()?,
                    (Some(_), None) => {
                        let msg = hx(t, "msg");
                        let raw = esk.sign_deterministic(&msg, &ctx).ok()?; // contexts over 255 bytes are refused
                        if pq::PrivateKey::from_seed_bytes(seed).sign(&ctx, &msg).as_ref() != raw.encode().as_slice() {
                            return Some(Err("pq::PrivateKey::sign differs".into()));
                        }
                        if ctx.is_empty() && crypto::Keys::from_seed(seed).sign(&msg) != raw.encode().as_slice() {
                            return Some(Err("crypto::Keys::sign differs".into()));
                        }
                        raw
                    }
                    (None, _) => esk.sign_mu_deterministic(&mu_of(t).into()),
                };
                Some(Ok(sha256(&s.encode()) == str_of(t, "sigSha256")))
            })()
            .unwrap_or(Ok(false))?;
            check(got == want, || format!("got {got}, want {want} ({})", str_of(t, "comment")))
        });
    }
    r.finish("Wycheproof", 105);
}

/// pq.rs hashes namespaces longer than FIPS 204's 255-byte context limit down to BLAKE3(namespace).
#[test]
fn long_namespaces_are_hashed_to_a_context() {
    let seed = [3; 32];
    let key = pq::PrivateKey::from_seed_bytes(seed);
    let raw = SigningKey::<MlDsa65>::from_seed(&Seed::from(seed));
    for len in [255, 256, 300] {
        let ns = vec![b'n'; len];
        let ctx = if len <= 255 { ns.clone() } else { crypto::hash(&ns).to_vec() };
        let ours = key.sign(&ns, b"msg");
        assert_eq!(ours.as_ref(), raw.expanded_key().sign_deterministic(b"msg", &ctx).unwrap().encode().as_slice(), "namespace of {len} bytes");
        assert!(key.public_key().verify(&ns, b"msg", &ours));
        assert!(!key.public_key().verify(&ns[1..], b"msg", &ours));
    }
}

// ------------------------------------------------------------------------------------ Rust ⇄ TS

const CROSS_CHAIN: &str = "blockula-kat";
/// The cross vectors' chain instance (the genesis hash every tx signs after the chain id, and
/// part of the consensus namespace votes are signed under).
fn cross_genesis() -> String {
    hex::encode(crypto::hash(b"blockula-kat genesis"))
}
/// The same instance, as handoffs bind it (F02).
fn cross_instance() -> epochs::Instance {
    epochs::Instance::new(CROSS_CHAIN, crypto::hash(b"blockula-kat genesis"))
}
/// Consensus keys of the 4-validator set used for the vote and certificate vectors.
const VALIDATOR_SEEDS: [[u8; 32]; 4] = [[11; 32], [12; 32], [13; 32], [14; 32]];
const POP: (&str, &str) = ("203.0.113.7:7071", "https://validator.example:7070");
const HANDOFF_EPOCH: u64 = 4;

/// Seeds whose public key, key id and address both implementations derive.
fn cross_seeds() -> Vec<[u8; 32]> {
    vec![[0; 32], [0xff; 32], std::array::from_fn(|i| i as u8), [7; 32], VALIDATOR_SEEDS[0]]
}

fn validator_set() -> Set<pq::PublicKey> {
    Set::try_from(VALIDATOR_SEEDS.map(|s| pq::PrivateKey::from_seed_bytes(s).public_key()).to_vec()).unwrap()
}

fn kat_proposal() -> Proposal<Digest> {
    Proposal::new(Round::new(Epoch::new(3), View::new(42)), View::new(40), Digest(crypto::hash(b"blockula kat block")))
}

/// The handoff vector's outgoing set (all four) and incoming set (the last three), sorted.
fn handoff_keys() -> (Vec<String>, Vec<String>) {
    let sorted = |seeds: &[[u8; 32]]| {
        let mut keys: Vec<String> = seeds.iter().map(|s| hex::encode(pq::PrivateKey::from_seed_bytes(*s).public_key().as_ref())).collect();
        keys.sort();
        keys
    };
    (sorted(&VALIDATOR_SEEDS), sorted(&VALIDATOR_SEEDS[1..]))
}

/// Seed → public key, key id, and its address under both prefixes (mainnet `boo`, testnet `tboo`).
fn key_json(seed: [u8; 32]) -> Value {
    let k = crypto::Keys::from_seed(seed);
    let testnet = k.address_with(crypto::TESTNET_HRP);
    assert_eq!(testnet, pq::PrivateKey::from_seed_bytes(seed).public_key().address(crypto::TESTNET_HRP), "tx and consensus keys derive the same address");
    json!({ "seed": hex::encode(seed), "public_key_sha256": sha256(&k.public_key()), "key_id": types::key_id(&k.public_key()), "address": k.address(), "testnet_address": testnet })
}

/// What FIPS 204 sees as the context for a pq.rs namespace.
fn context_for(namespace: &[u8]) -> Vec<u8> {
    if namespace.len() <= 255 { namespace.to_vec() } else { crypto::hash(namespace).to_vec() }
}

fn case(label: &str, seed: [u8; 32], namespace: &[u8], msg: &[u8], sig: &[u8]) -> Value {
    json!({ "label": label, "seed": hex::encode(seed), "namespace": hex::encode(namespace), "ctx": hex::encode(context_for(namespace)), "msg": hex::encode(msg), "sig": hex::encode(sig) })
}

/// Everything the node signs, made by the node's own code (`testdata/cross/rust-signed.json`).
fn rust_signed() -> Value {
    let mut cases = vec![];
    // Txs (v3, bound to a chain instance by its genesis hash): `Tx::sign` signs the signing bytes
    // with an empty context. Each case also records the wire form (`Tx::to_bytes`) that blocks and
    // gossip carry. One acts from a testnet address.
    let owner = crypto::Keys::from_seed([7; 32]);
    let testnet_to = crypto::Keys::from_seed([0xff; 32]).address_with(crypto::TESTNET_HRP);
    // The added key (seed 0xff) consents to the owner's account on the vectors' chain instance.
    let added = crypto::Keys::from_seed([0xff; 32]);
    let added_pk = hex::encode(added.public_key());
    let added_pop = types::key_pop(&added, CROSS_CHAIN, &cross_genesis(), &owner.address());
    let actions = [
        ("tx/transfer", owner.address(), json!({ "type": "transfer", "to": "alice.boo", "amount": "1500000000" })),
        ("tx/add_key", owner.address(), json!({ "type": "add_key", "key_id": types::key_id(&added.public_key()), "label": "agent", "role": { "type": "session", "limit": "5000000000", "period_blocks": 600, "expires_at": 100000 }, "public_key": added_pk, "pop": added_pop })),
        ("tx/transfer-tboo", owner.address_with(crypto::TESTNET_HRP), json!({ "type": "transfer", "to": testnet_to, "amount": "42" })),
    ];
    for (label, from, action) in actions {
        let unsigned = json!({ "chain_id": CROSS_CHAIN, "genesis_hash": cross_genesis(), "from": from, "nonce": 7, "fee": "123456", "tip": "250", "valid_until": 4242, "action": action, "memo": "🧛 kat", "public_key": "", "signature": "" });
        let tx: types::Tx = serde_json::from_value(unsigned).unwrap_or_else(|e| panic!("tx JSON shape changed ({e}): update rust_signed() and regenerate testdata/cross"));
        let tx = tx.sign(&owner);
        let mut c = case(label, [7; 32], b"", &tx.signing_bytes(), &hex::decode(&tx.signature).unwrap());
        c["tx"] = serde_json::to_value(&tx).unwrap();
        c["wire"] = json!(hex::encode(tx.to_bytes()));
        cases.push(c);
    }
    // Consensus votes, signed by simplex through `MlDsaScheme` exactly as validators sign them.
    let scheme = pq::MlDsaScheme::signer(&consensus::namespace(CROSS_CHAIN, &cross_genesis()), validator_set(), pq::PrivateKey::from_seed_bytes(VALIDATOR_SEEDS[0])).unwrap();
    let ns = Namespace::derive(&consensus::namespace(CROSS_CHAIN, &cross_genesis()));
    let p = kat_proposal();
    let votes = [
        ("consensus/notarize", Subject::Notarize { proposal: &p }, Notarize::sign(&scheme, p.clone()).unwrap().attestation),
        ("consensus/finalize", Subject::Finalize { proposal: &p }, Finalize::sign(&scheme, p.clone()).unwrap().attestation),
        ("consensus/nullify", Subject::Nullify { round: p.round }, Nullify::sign::<Digest>(&scheme, p.round).unwrap().attestation),
    ];
    for (label, subject, att) in votes {
        let mut c = case(label, VALIDATOR_SEEDS[0], subject.namespace(&ns), &subject.message(), att.signature.get().unwrap().as_ref());
        c["signer"] = json!(usize::from(att.signer));
        cases.push(c);
    }
    // Validator proof of possession and epoch handoff (pq.rs keys with their own contexts).
    let v0 = pq::PrivateKey::from_seed_bytes(VALIDATOR_SEEDS[0]);
    let pop_msg = types::pop_message(CROSS_CHAIN, &owner.address(), POP.0, POP.1);
    let mut c = case("validator-pop", VALIDATOR_SEEDS[0], types::POP_CONTEXT, &pop_msg, v0.sign(types::POP_CONTEXT, &pop_msg).as_ref());
    c["pop"] = json!({ "chain_id": CROSS_CHAIN, "owner": owner.address(), "p2p_addr": POP.0, "http_addr": POP.1 });
    cases.push(c);
    // A key's proof of possession for being added to an account (AddKey, audit gap-7-2): the
    // tx keys' derivation, deterministic, under KEY_POP_CONTEXT.
    let key_pop = json!({ "chain_id": CROSS_CHAIN, "genesis_hash": cross_genesis(), "account": owner.address(), "key_id": types::key_id(&added.public_key()) });
    let msg = types::key_pop_message(CROSS_CHAIN, &cross_genesis(), &owner.address(), &types::key_id(&added.public_key()));
    let mut c = case("key-pop", [0xff; 32], types::KEY_POP_CONTEXT, &msg, &added.sign_with_context(types::KEY_POP_CONTEXT, &msg));
    c["key_pop"] = key_pop;
    cases.push(c);
    let (outgoing, incoming) = handoff_keys();
    let handoff = epochs::handoff_message(&cross_instance(), HANDOFF_EPOCH, &outgoing, &incoming);
    cases.push(case("epoch-handoff", VALIDATOR_SEEDS[0], epochs::HANDOFF_CONTEXT, &handoff, v0.sign(epochs::HANDOFF_CONTEXT, &handoff).as_ref()));
    // Context edge cases: empty message, the longest context, and a namespace pq.rs hashes.
    let k = pq::PrivateKey::from_seed_bytes([0xff; 32]);
    for (label, ns, msg) in [("empty", vec![], vec![]), ("context-255", vec![0xab; 255], vec![0]), ("namespace-300", vec![b'n'; 300], b"long".to_vec())] {
        cases.push(case(label, [0xff; 32], &ns, &msg, k.sign(&ns, &msg).as_ref()));
    }
    json!({
        "generator": "node/src/kat.rs `write_rust_signed` (ml-dsa 0.1.1); see README.md",
        "chain_id": CROSS_CHAIN,
        "genesis_hash": cross_genesis(),
        "keys": cross_seeds().into_iter().map(key_json).collect::<Vec<_>>(),
        "cases": cases,
    })
}

fn cases_of(file: &Value) -> &Vec<Value> {
    file["cases"].as_array().expect("cases")
}

fn find<'a>(file: &'a Value, label: &str) -> Vec<&'a Value> {
    cases_of(file).iter().filter(|c| c["label"] == label).collect()
}

/// Seed → public key, key id and address agree with what the other side derived.
fn check_keys(file: &Value, side: &str) {
    let keys = file["keys"].as_array().expect("keys");
    assert_eq!(keys.len(), cross_seeds().len(), "{side} keys");
    for k in keys {
        assert_eq!(*k, key_json(seed_of(k, "seed")), "{side}: seed → key id / address");
    }
}

/// `rust-signed.json` is exactly what the node's signing paths produce for the recorded
/// (namespace, message) pairs. `sdk-ts/test/kat.ts` checks that noble reproduces and accepts it.
#[test]
fn cross_rust_signed() {
    let file = load("cross/rust-signed.json");
    check_keys(&file, "rust-signed");
    assert_eq!(cases_of(&file).len(), 12);
    for c in cases_of(&file) {
        let (label, seed, ns, msg) = (str_of(c, "label"), seed_of(c, "seed"), hx(c, "namespace"), hx(c, "msg"));
        assert_eq!(hx(c, "ctx"), context_for(&ns), "{label}: context");
        let sig = if label.starts_with("tx/") { crypto::Keys::from_seed(seed).sign(&msg) } else { pq::PrivateKey::from_seed_bytes(seed).sign(&ns, &msg).as_ref().to_vec() };
        assert_eq!(hex::encode(&sig), str_of(c, "sig"), "{label}: the node no longer signs these bytes the same way");
    }
}

/// `ts-signed.json` was signed by the SDK (hedged, as `Wallet.sign` does): the node's verify
/// paths accept every signature, reject it under another namespace, and reproduce the SDK's
/// deterministic signatures. Four SDK-signed finalize votes assemble into a certificate that
/// simplex accepts.
#[test]
fn cross_ts_signed() {
    let file = load("cross/ts-signed.json");
    check_keys(&file, "ts-signed");
    assert_eq!(cases_of(&file).len(), 15);
    for c in cases_of(&file) {
        let (label, seed, ns, msg, sigb) = (str_of(c, "label"), seed_of(c, "seed"), hx(c, "namespace"), hx(c, "msg"), hx(c, "sig"));
        assert_eq!(hx(c, "ctx"), context_for(&ns), "{label}: context");
        let pk = crypto::Keys::from_seed(seed).public_key();
        if label.starts_with("tx/") {
            assert!(ns.is_empty());
            crypto::verify(&pk, &msg, &sigb).unwrap_or_else(|e| panic!("{label}: crypto::verify: {e}"));
        }
        assert!(pq_verify(&pk, &ns, &msg, &sigb), "{label}: pq::PublicKey::verify");
        assert!(!pq_verify(&pk, b"blockula/other", &msg, &sigb), "{label}: verifies under another namespace");
        if let Some(det) = c.get("sig_det").and_then(Value::as_str) {
            let ours = if label.starts_with("tx/") { crypto::Keys::from_seed(seed).sign(&msg) } else { pq::PrivateKey::from_seed_bytes(seed).sign(&ns, &msg).as_ref().to_vec() };
            assert_eq!(hex::encode(ours), det, "{label}: deterministic signatures differ between Rust and TS");
        }
    }

    // Votes: simplex's own verification, then a quorum certificate built from SDK signatures.
    let set = validator_set();
    let verifier = pq::MlDsaScheme::verifier(&consensus::namespace(CROSS_CHAIN, &cross_genesis()), set.clone());
    let p = kat_proposal();
    let attestation = |c: &Value| -> Attestation<pq::MlDsaScheme> {
        let pk = pq::PrivateKey::from_seed_bytes(seed_of(c, "seed")).public_key();
        let signer = Participant::from_usize(set.position(&pk).expect("a validator"));
        Attestation { signer, signature: pq::Signature::decode(hx(c, "sig").as_slice()).unwrap().into() }
    };
    let mut rng = sys_rng();
    for c in find(&file, "consensus/notarize") {
        assert!(Notarize { proposal: p.clone(), attestation: attestation(c) }.verify(&mut rng, &verifier, &Sequential), "notarize");
    }
    for c in find(&file, "consensus/nullify") {
        assert!(Nullify { round: p.round, attestation: attestation(c) }.verify::<_, Digest>(&mut rng, &verifier, &Sequential), "nullify");
    }
    let finalizes: Vec<Finalize<pq::MlDsaScheme, Digest>> = find(&file, "consensus/finalize").into_iter().map(|c| Finalize { proposal: p.clone(), attestation: attestation(c) }).collect();
    assert_eq!(finalizes.len(), VALIDATOR_SEEDS.len());
    for f in &finalizes {
        assert!(f.verify(&mut rng, &verifier, &Sequential), "finalize by {}", f.attestation.signer);
    }
    // A quorum (3 of 4) of SDK-signed votes certifies the block, through the codec and back.
    let fin = Finalization::from_finalizes(&verifier, NonEmpty::try_new(finalizes.iter().skip(1)).unwrap(), &Sequential).unwrap();
    assert!(fin.verify(&mut rng, &verifier, &Sequential));
    let decoded = Finalization::<pq::MlDsaScheme, Digest>::decode_cfg(fin.encode(), &verifier.certificate_codec_config()).unwrap();
    assert!(decoded.verify(&mut rng, &verifier, &Sequential));
    assert!(Finalization::from_finalizes(&verifier, NonEmpty::try_new(finalizes.iter().skip(2)).unwrap(), &Sequential).is_err(), "2 of 4 isn't a quorum");
}

/// The recorded messages are still the node's encodings: tx signing bytes (for both files' txs,
/// which also pass `check_format` and `verify_signature`), simplex vote messages and namespaces,
/// PoP and handoff messages. A failure means a format changed on one side: if intended, regenerate
/// `testdata/cross` (README.md there) so the crypto checks keep covering the current format.
#[test]
fn cross_formats_are_current() {
    let stale = |label: &str| format!("{label}: encoding changed; regenerate node/testdata/cross (see its README.md)");
    let now = rust_signed();
    let rust = load("cross/rust-signed.json");
    for c in cases_of(&rust) {
        let label = str_of(c, "label");
        let fresh = find(&now, label)[0];
        assert_eq!((&c["namespace"], &c["msg"]), (&fresh["namespace"], &fresh["msg"]), "{}", stale(label));
    }
    let ts = load("cross/ts-signed.json");
    for c in cases_of(&rust).iter().chain(cases_of(&ts)) {
        let label = str_of(c, "label");
        if let Some(tx) = c.get("tx") {
            let tx: types::Tx = serde_json::from_value(tx.clone()).unwrap_or_else(|e| panic!("{}: {e}", stale(label)));
            assert_eq!(hex::encode(tx.signing_bytes()), str_of(c, "msg"), "{}", stale(label));
            tx.check_format().unwrap_or_else(|e| panic!("{label}: check_format: {e}"));
            tx.verify_signature().unwrap_or_else(|e| panic!("{label}: verify_signature: {e}"));
            // The wire form both sides recorded is the node's, and parses back to the same tx.
            let wire = hx(c, "wire");
            assert_eq!(hex::encode(tx.to_bytes()), hex::encode(&wire), "{}", stale(label));
            assert_eq!(types::Tx::from_bytes(&wire).unwrap_or_else(|e| panic!("{label}: Tx::from_bytes: {e}")), tx, "{label}: wire round trip");
            let (prefix, _) = crypto::parse_address(&tx.from).unwrap_or_else(|| panic!("{label}: `from` isn't an address"));
            tx.check_prefix(prefix).unwrap_or_else(|e| panic!("{label}: check_prefix: {e}"));
            // An AddKey's proof of possession is the node's to check too.
            if let types::Action::AddKey { key_id, public_key, pop, .. } = &tx.action {
                types::verify_key_pop(&tx.chain_id, &tx.genesis_hash, &tx.from, key_id, public_key, pop).unwrap_or_else(|e| panic!("{label}: verify_key_pop: {e}"));
            }
        }
        if let Some(p) = c.get("key_pop") {
            let msg = types::key_pop_message(str_of(p, "chain_id"), str_of(p, "genesis_hash"), str_of(p, "account"), str_of(p, "key_id"));
            assert_eq!((hx(c, "namespace"), hx(c, "msg")), (types::KEY_POP_CONTEXT.to_vec(), msg), "{}", stale(label));
            let pk = crypto::Keys::from_seed(seed_of(c, "seed")).public_key();
            assert_eq!(types::key_id(&pk), str_of(p, "key_id"), "{label}: the signer is the key");
            types::verify_key_pop(str_of(p, "chain_id"), str_of(p, "genesis_hash"), str_of(p, "account"), str_of(p, "key_id"), &hex::encode(&pk), str_of(c, "sig")).unwrap_or_else(|e| panic!("{label}: verify_key_pop: {e}"));
        }
        if let Some(pop) = c.get("pop") {
            let msg = types::pop_message(str_of(pop, "chain_id"), str_of(pop, "owner"), str_of(pop, "p2p_addr"), str_of(pop, "http_addr"));
            assert_eq!((hx(c, "namespace"), hx(c, "msg")), (types::POP_CONTEXT.to_vec(), msg), "{}", stale(label));
        }
    }
    // The SDK signed the node's vote and handoff messages verbatim.
    for c in cases_of(&ts).iter().filter(|c| c.get("tx").is_none() && c.get("pop").is_none()) {
        let label = str_of(c, "label");
        let r = find(&now, label)[0];
        assert_eq!((&c["namespace"], &c["msg"]), (&r["namespace"], &r["msg"]), "{}", stale(label));
    }
}

/// Regenerates `testdata/cross/rust-signed.json`:
/// `cargo test --manifest-path node/Cargo.toml kat::write_rust_signed -- --ignored`.
#[test]
#[ignore = "generator, run by hand (see testdata/cross/README.md)"]
fn write_rust_signed() {
    let path = testdata("cross/rust-signed.json");
    std::fs::write(&path, serde_json::to_string_pretty(&rust_signed()).unwrap() + "\n").unwrap();
    println!("wrote {}", path.display());
}
