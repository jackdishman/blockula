//! Light-client vectors for the SDK's port of [`tree::verify`] (`sdk-ts/src/proof.ts`), in
//! `sdk-ts/test/proof-vectors.json`, which `sdk-ts/test/proof-vectors.ts` checks:
//! - `tree_golden`: the 5 leaves and root of `tree::tests::golden_roots`;
//! - `tree`: 200 records, their root, and inclusion and exclusion proofs (by an empty subtree and
//!   by another key's leaf);
//! - `state`: every record of `tests::golden_encodings`' state, its root, and proofs for an
//!   account, a scalar and an absent account;
//! - `records`: an account with keys and a name record (with its expiry), encoded by the node,
//!   with what the SDK's decoders must read from them.
//!
//! The tree sections depend only on the tree format (a consensus rule), so they must match a
//! fresh generation exactly. The state and record sections follow the state encoding, which
//! changes whenever a module adds a record type or a scalar; for those this test checks the file
//! against the node's tree code (roots and proofs) and only reports that it's out of date.
//! Regenerate with `cargo test --manifest-path node/Cargo.toml state::proof_vectors::write -- --ignored`.

use super::codec::encode;
use super::tree::{key_hash, root_of, value_hash, verify, Proof, Smt};
use super::*;
use crate::chain::{Account, KeyRecord, NameRecord};
use crate::types::KeyRole;
use serde_json::{json, Value};

/// `sdk-ts/test/proof-vectors.json`, found from the crate's directory (`node/`, or `node/fuzz/`,
/// which compiles these modules too).
fn file() -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    dir.ancestors().map(|d| d.join("sdk-ts/test/proof-vectors.json")).find(|p| p.exists()).unwrap_or_else(|| dir.join("../sdk-ts/test/proof-vectors.json"))
}

fn case(root: &Hash, namespace: u8, key: &[u8], value: Option<&[u8]>, proof: &Proof, valid: bool, label: &str) -> Value {
    assert_eq!(verify(root, namespace, key, value, proof), valid, "{label}");
    json!({ "label": label, "namespace": namespace, "key": hex::encode(key), "value": value.map(hex::encode), "proof": proof, "valid": valid })
}

fn tree_golden() -> Value {
    let mut leaves: Vec<(Hash, Hash)> = (0..5u32).map(|i| (key_hash(1, &i.to_be_bytes()), value_hash(&i.to_le_bytes()))).collect();
    leaves.sort();
    let root = root_of(&leaves);
    assert_eq!(hex::encode(root), "2daa89cfecc65603ee75d4fbdbad2f57d267e26634a7d8d957f39812c786a6b8");
    json!({ "leaves": leaves.iter().map(|(k, v)| [hex::encode(k), hex::encode(v)]).collect::<Vec<_>>(), "root": hex::encode(root) })
}

fn tree() -> Value {
    let records: Vec<(Vec<u8>, Vec<u8>)> = (0..200u32).map(|i| (i.to_be_bytes().to_vec(), i.to_le_bytes().to_vec())).collect();
    let mut ups: Vec<(Hash, Option<Hash>)> = records.iter().map(|(k, v)| (key_hash(1, k), Some(value_hash(v)))).collect();
    ups.sort();
    let mut t = Smt::default();
    let root = t.update(&ups).unwrap();
    let prove = |k: &[u8]| t.prove(&key_hash(1, k)).unwrap();
    let mut cases = vec![];
    for i in [0u32, 7, 199] {
        let (k, v) = (i.to_be_bytes(), i.to_le_bytes());
        cases.push(case(&root, 1, &k, Some(&v), &prove(&k), true, &format!("record {i} is present")));
        cases.push(case(&root, 1, &k, None, &prove(&k), false, &format!("record {i} isn't absent")));
        cases.push(case(&root, 1, &k, Some(b"forged"), &prove(&k), false, &format!("record {i} doesn't hold a forged value")));
        cases.push(case(&root, 2, &k, Some(&v), &prove(&k), false, &format!("record {i}'s proof doesn't hold in another namespace")));
    }
    // Absent keys: the first whose path ends at an empty subtree, and the first ending at another
    // key's lone leaf.
    let absent = |empty: bool| (200u32..).find(|i| prove(&i.to_be_bytes()).leaf.is_none() == empty).unwrap();
    for (i, how) in [(absent(true), "an empty subtree"), (absent(false), "another key's leaf")] {
        let k = i.to_be_bytes();
        cases.push(case(&root, 1, &k, None, &prove(&k), true, &format!("{i} is absent ({how})")));
        cases.push(case(&root, 1, &k, Some(b"x"), &prove(&k), false, &format!("{i} isn't present ({how})")));
    }
    // An exclusion proof for one key doesn't exclude a present one.
    cases.push(case(&root, 1, &3u32.to_be_bytes(), None, &prove(&absent(false).to_be_bytes()), false, "another key's exclusion proof doesn't exclude record 3"));
    json!({
        "records": records.iter().map(|(k, v)| json!([1, hex::encode(k), hex::encode(v)])).collect::<Vec<_>>(),
        "root": hex::encode(root),
        "proofs": cases,
    })
}

/// The state of `tests::golden_encodings`.
fn golden_state() -> State {
    let mut st = State::default();
    st.accounts.insert("boo1alice".into(), Account { balance: 5, nonce: 2, ..Default::default() });
    st.supply = 5;
    st.commit_block().unwrap();
    st
}

fn state() -> Value {
    let st = golden_state();
    let root = st.root_hash();
    let mut records = vec![];
    st.for_each_record(|n, k, v| records.push(json!([n, hex::encode(k), hex::encode(v)])));
    let alice = st.record(ns::ACCOUNTS, b"boo1alice").unwrap();
    let supply = st.record(ns::META, meta::SUPPLY).unwrap();
    let cases = vec![
        case(&root, ns::ACCOUNTS, b"boo1alice", Some(&alice), &st.prove(ns::ACCOUNTS, b"boo1alice").unwrap(), true, "account boo1alice"),
        case(&root, ns::META, meta::SUPPLY, Some(&supply), &st.prove(ns::META, meta::SUPPLY).unwrap(), true, "the supply scalar"),
        case(&root, ns::ACCOUNTS, b"boo1nobody", None, &st.prove(ns::ACCOUNTS, b"boo1nobody").unwrap(), true, "boo1nobody has no account"),
    ];
    json!({ "records": records, "root": hex::encode(root), "proofs": cases, "alice": { "balance": "5", "nonce": 2 } })
}

fn records() -> Value {
    let session = KeyRecord { label: "claude".into(), role: KeyRole::Session { limit: 10_000_000_000, period_blocks: 86_400, expires_at: 2_000_000 }, added_at: 12, spent: 3, window_start: 40 };
    let owner = KeyRecord { label: "phone".into(), role: KeyRole::Owner, added_at: 9, spent: 0, window_start: 0 };
    let keys = [("ab".repeat(32), owner), ("cd".repeat(32), session)].into_iter().collect();
    let account = Account { balance: 123_456_789_012_345, nonce: 77, keys, ..Default::default() };
    // A name record as the state stores it; the SDK reads its fields in order, so a version that
    // appends fields keeps working (schema 6 appended `offered_to`, set here: a pending offer;
    // schema 7 `grace_blocks`, the grace it was registered with).
    let name = encode(&NameRecord { name: "alice".into(), owner: "boo1owner".into(), registered_at: 1234, expires_at: 5678, offered_to: Some("boo1taker".into()), grace_blocks: 2_592_000 });
    json!({
        "account": {
            "value": hex::encode(encode(&account)),
            "balance": "123456789012345",
            "nonce": 77,
            "keys": [
                { "key_id": "ab".repeat(32), "label": "phone", "role": { "type": "owner" }, "added_at": 9 },
                { "key_id": "cd".repeat(32), "label": "claude", "role": { "type": "session", "limit": "10000000000", "period_blocks": 86400, "expires_at": 2000000 }, "added_at": 12 },
            ],
        },
        "name": { "value": hex::encode(name), "name": "alice", "owner": "boo1owner", "registered_at": 1234, "expires_at": 5678, "offered_to": "boo1taker", "grace_blocks": 2_592_000 },
    })
}

fn vectors() -> Value {
    json!({
        "description": "State proof vectors (node/src/state/proof_vectors.rs) for the SDK's light-client checks (sdk-ts/src/proof.ts, a port of node/src/state/tree.rs). Records are [namespace, key hex, value hex]; proofs are /api/proof's `proof` field. Regenerate: cargo test --manifest-path node/Cargo.toml state::proof_vectors::write -- --ignored",
        "tree_golden": tree_golden(),
        "tree": tree(),
        "state": state(),
        "records": records(),
    })
}

/// Checks a section's root against its records, and every proof, with the node's tree code.
fn check_section(section: &Value, label: &str) {
    let mut leaves: Vec<(Hash, Hash)> = section["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (key_hash(r[0].as_u64().unwrap() as u8, &hex::decode(r[1].as_str().unwrap()).unwrap()), value_hash(&hex::decode(r[2].as_str().unwrap()).unwrap())))
        .collect();
    leaves.sort();
    let root: Hash = hex::decode(section["root"].as_str().unwrap()).unwrap().try_into().unwrap();
    assert_eq!(root_of(&leaves), root, "{label}: the root doesn't match the records");
    for c in section["proofs"].as_array().unwrap() {
        let proof: Proof = serde_json::from_value(c["proof"].clone()).unwrap();
        let value = c["value"].as_str().map(|v| hex::decode(v).unwrap());
        let ok = verify(&root, c["namespace"].as_u64().unwrap() as u8, &hex::decode(c["key"].as_str().unwrap()).unwrap(), value.as_deref(), &proof);
        assert_eq!(ok, c["valid"].as_bool().unwrap(), "{label}: {}", c["label"]);
    }
}

#[test]
fn proof_vectors_match_the_node() {
    let file: Value = serde_json::from_str(&std::fs::read_to_string(file()).expect("sdk-ts/test/proof-vectors.json")).unwrap();
    let fresh = vectors();
    let regen = "regenerate sdk-ts/test/proof-vectors.json (see node/src/state/proof_vectors.rs)";
    assert_eq!(file["tree_golden"], fresh["tree_golden"], "the tree format changed (a consensus change): {regen}");
    assert_eq!(file["tree"], fresh["tree"], "the tree format changed (a consensus change): {regen}");
    check_section(&file["tree"], "tree");
    check_section(&file["state"], "state");
    for section in ["state", "records"] {
        if file[section] != fresh[section] {
            eprintln!("note: the state encoding changed since sdk-ts/test/proof-vectors.json's `{section}` section was written; {regen}");
        }
    }
}

#[test]
#[ignore = "generator, run by hand (see the module docs)"]
fn write() {
    std::fs::write(file(), serde_json::to_string_pretty(&vectors()).unwrap() + "\n").unwrap();
}
