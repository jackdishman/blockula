//! Programs' read API over HTTP (WBS S6a's acceptance), in `api::http_tests`' style: the router on
//! a summoned one-validator chain, called with `oneshot`. A chain is born at v1 (no schedule), at
//! v2 with programs off, or at v2 with programs on from genesis. No program action runs yet (WBS
//! S3, S4a), so [`with_program`] stores what an upload, a create and some writes would have left,
//! with every total consistent, and runs a block: the block's `debug_check` holds every invariant
//! (programs' included) and the root, and the view it publishes is what the routes read.

use super::*;
use crate::api::{router, ClientIpPolicy};
use crate::chain::{Account, Chain, Genesis, SummonOpts};
use crate::crypto::Keys;
use crate::programs::state::{storage_key, CodeRecord, Entry, InterfaceRecord, Pause, Versioned};
use crate::state::tree;
use crate::types::DROPS_PER_FANG;
use crate::upgrade::{ScheduledUpgrade, PROGRAMS_FEATURE, PROGRAMS_VERSION};
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::Request;
use parking_lot::Mutex;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt as _;

const FANG: u64 = DROPS_PER_FANG;
/// The token guest and its interface (stage V's): methods with arguments, an `Init`, views.
const WASM: &[u8] = include_bytes!("../../../vm/tests/programs/token.wasm");
const IFACE: &[u8] = include_bytes!("../../../vm/tests/programs/token.iface");
const IFACE_JSON: &str = include_str!("../../../vm/tests/programs/token.iface.json");

/// How a test chain was born.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Born {
    /// No upgrade schedule: v1 forever, as testnet-2.
    V1,
    /// v2 from block 1, programs off (no META), as `blockula-1` at launch.
    V2,
    /// v2 from block 1 with `programs` in the genesis: META exists from block 0.
    V2Programs,
}

/// A one-validator chain born as `born`, with block 1 run (the first v2 block on a chain born at
/// v2), and the API over it.
fn chain(tag: &str, born: Born) -> (PathBuf, Node) {
    let root = std::env::temp_dir().join(format!("blockula-programs-api-{tag}-{}-{}", crate::chain::now_ms(), std::process::id()));
    let homes = Chain::summon(&root, &SummonOpts::new(tag, 1)).unwrap();
    // As the simulator does: `summon` writes a v1 genesis, which the test rewrites before opening.
    let path = homes[0].join("genesis.json");
    let mut g = Genesis::load(&path).unwrap();
    if born != Born::V1 {
        g.upgrades = vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 1 }];
    }
    if born == Born::V2Programs {
        g.features = vec![PROGRAMS_FEATURE.into()];
    }
    std::fs::write(&path, serde_json::to_string_pretty(&g).unwrap()).unwrap();
    let chain = Arc::new(Mutex::new(Chain::open(&homes[0]).unwrap()));
    chain.lock().produce_block_with(vec![]).unwrap();
    (root, Node::new(chain, None, vec![], ClientIpPolicy::default()))
}

async fn call(n: &Node, uri: &str) -> (StatusCode, Value, String) {
    let app = router(n.clone()).layer(MockConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));
    let res = app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 << 20).await.unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null), text)
}

fn creator() -> String {
    Keys::from_seed([3; 32]).address()
}

/// What [`with_program`] stored.
struct Stored {
    address: String,
    wasm_hash: [u8; 32],
    interface_hash: [u8; 32],
}

/// An entry to store: its key, its value's length (filled with the key's first byte), and whether
/// the program paid its deposit itself (`SPONSOR`) or the creator did.
type Write<'a> = (&'a [u8], usize, bool);

/// Stores by hand what the creator's upload of the token guest (once: the blobs are shared), its
/// create with `nonce` (an upgradable record, accepting transfers, holding 500 drops) and `writes`
/// would have left, keeping every total of §4.4 exact, then runs the next block, which checks them
/// and publishes the view. Deposits are made up (100 drops a byte); the supply grows by what's new.
fn with_program(n: &Node, nonce: u64, writes: &[Write]) -> Stored {
    let mut c = n.chain.lock();
    let validator = c.me().unwrap();
    let st = &mut c.state;
    let prefix = st.params.prefix();
    let creator_bytes = crypto::address_bytes(&creator()).unwrap();
    let address = digest::program_address(prefix, &creator_bytes, nonce);
    let program = crypto::address_bytes(&address).unwrap();
    let (wasm_hash, interface_hash) = (digest::wasm_hash(WASM), digest::interface_hash(IFACE));
    let mut escrow = 0;
    let mut new_bytes = 0;
    let p = &mut st.programs;
    if !p.code.contains_key(&wasm_hash) {
        let exports = blockula_vm::validate::validate(WASM).unwrap().exports;
        p.code.insert(wasm_hash, Versioned(CodeRecord { wasm: WASM.to_vec(), exports }));
        p.interfaces.insert(interface_hash, Versioned(InterfaceRecord::new(IFACE.to_vec())));
        for (kind, hash, size) in [(BLOB_CODE, wasm_hash, WASM.len()), (BLOB_INTERFACE, interface_hash, IFACE.len())] {
            let deposit = 100 * size as u64;
            p.blob_refs.insert(blob_key(kind, &hash), Versioned(BlobRef { uploader: creator(), deposit, size: size as u32, refs: 0, uploaded_at: 1, last_used_at: 1 }));
            escrow += deposit;
            new_bytes += size as u64;
        }
    }
    for kind in [BLOB_CODE, BLOB_INTERFACE] {
        let key = blob_key(kind, if kind == BLOB_CODE { &wasm_hash } else { &interface_hash });
        let mut b = p.blob_refs.get(&key).unwrap().clone();
        b.refs += 1;
        p.blob_refs.insert(key, b);
    }
    let (mut items, mut bytes, mut deposit, mut sponsored) = (0, 0, 0, 0);
    for (key, len, own) in writes {
        let capacity = blockula_vm::storage::capacity(key.len(), *len);
        let entry = Entry { value: vec![key[0]; *len], payer: if *own { program } else { creator_bytes }, deposit: 100 * u64::from(capacity), capacity };
        (items, bytes, deposit) = (items + 1, bytes + u64::from(capacity), deposit + entry.deposit);
        if *own {
            sponsored += entry.deposit;
        }
        p.storage.insert(storage_key(&program, key), Versioned(entry));
    }
    let record_deposit = 100 * RECORD_BYTES;
    let record = ProgramRecord {
        creator: creator(),
        created_at: 1,
        wasm_hash,
        interface_hash,
        version: 1,
        authority: Some(validator.clone()),
        upgrade_delay_blocks: 172_800,
        policy: 1,
        accepts_transfers: true,
        items,
        bytes,
        deposit,
        sponsored,
        record_deposit,
        record_payer: creator(),
    };
    p.records.insert(address.clone(), Versioned(record));
    escrow += deposit + record_deposit;
    let m = p.meta.as_mut().expect("programs on");
    m.programs += 1;
    m.state_bytes += new_bytes + bytes + RECORD_BYTES;
    m.value_held += 500;
    m.sponsored += sponsored;
    m.escrow += escrow;
    st.accounts.insert(address.clone(), Account { balance: 500, ..Default::default() });
    st.supply += 500 + escrow;
    c.produce_block_with(vec![]).unwrap();
    Stored { address, wasm_hash, interface_hash }
}

use crate::programs::RECORD_BYTES;

/// `GET /api/program/{address}`: the record, what follows from it (the version digest a call
/// binds, the upgradability sentence, the balance), its methods with the deployer's names as
/// `untrusted_name`, and a proof path that serves the record with a Merkle proof verifying
/// against the state root, exactly as a light client checks it.
#[tokio::test]
async fn a_program_is_served_with_its_record_methods_and_proof() {
    let (root, n) = chain("papi-show", Born::V2Programs);
    let p = with_program(&n, 0, &[(b"a", 8, false), (b"own", 40, true)]);
    let validator = n.chain.lock().me().unwrap();
    let (s, j, _) = call(&n, &format!("/api/program/{}", p.address)).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!((j["address"].as_str(), j["creator"]["address"].as_str(), j["created_at"].as_u64()), (Some(p.address.as_str()), Some(creator().as_str()), Some(1)));
    assert_eq!(j["code"], json!({ "wasm_hash": hex::encode(p.wasm_hash), "size": WASM.len(), "interface_hash": hex::encode(p.interface_hash) }));
    assert_eq!((j["version"].as_u64(), j["version_digest"].as_str()), (Some(1), Some(hex::encode(digest::version_digest(&p.address, &p.wasm_hash, &p.interface_hash, 1)).as_str())));
    assert_eq!((j["authority"]["address"].as_str(), j["upgrade_delay_blocks"].as_u64(), j["policy"].as_str()), (Some(validator.as_str()), Some(172_800), Some("additive")));
    assert_eq!(j["upgradability"], format!("no upgrades exist yet; once a protocol version adds them, {validator} may upgrade it with 172800 blocks' notice"));
    assert_eq!((j["accepts_transfers"].as_bool(), j["balance"].as_str(), j["record_deposit"].as_str()), (Some(true), Some("500"), Some((100 * RECORD_BYTES).to_string().as_str())));
    let (cap_a, cap_own) = (u64::from(blockula_vm::storage::capacity(1, 8)), u64::from(blockula_vm::storage::capacity(3, 40)));
    assert_eq!(j["storage"], json!({ "items": 2, "bytes": cap_a + cap_own, "deposit": (100 * (cap_a + cap_own)).to_string(), "sponsored": (100 * cap_own).to_string() }));
    assert_eq!(j["record_payer"]["address"], creator());
    assert_eq!(j["height"].as_u64(), Some(n.live.view().height));
    // Methods: the interface's, in its order, each deployer-written name under `untrusted_name`
    // (never `name`), with the selector a client computes and what execution enforces.
    let methods = j["methods"].as_array().unwrap();
    let names: Vec<&str> = methods.iter().map(|m| m["untrusted_name"].as_str().unwrap()).collect();
    assert_eq!(names, ["balance_of", "init", "total_supply", "transfer"]);
    for m in methods {
        let name = m["untrusted_name"].as_str().unwrap();
        assert!(m.get("name").is_none() && m["args"].as_array().unwrap().iter().all(|a| a.get("name").is_none() && a["untrusted_name"].is_string()), "{m}");
        assert_eq!(m["selector"], hex::encode(digest::selector(name)));
    }
    assert_eq!(methods[0], json!({ "untrusted_name": "balance_of", "selector": hex::encode(digest::selector("balance_of")), "kind": "view", "payable": false, "args": [{ "untrusted_name": "account", "type": "address" }], "returns": "u64" }));
    assert_eq!((methods[1]["kind"].as_str(), methods[1]["returns"].is_null()), (Some("init"), true));
    // The proof path: the record (namespace 66, the address's bytes as its key) and its proof
    // against the root `/api/status` reports.
    let path = j["proof_path"].as_str().unwrap();
    assert_eq!(path, format!("/api/proof/66/{}", hex::encode(p.address.as_bytes())));
    let (s, proof, _) = call(&n, path).await;
    assert_eq!(s, StatusCode::OK, "{proof}");
    let (_, status, _) = call(&n, "/api/status").await;
    assert_eq!(proof["root"], status["state_root"]);
    let state_root: [u8; 32] = hex::decode(proof["root"].as_str().unwrap()).unwrap().try_into().unwrap();
    let value = hex::decode(proof["value"].as_str().unwrap()).unwrap();
    let merkle: tree::Proof = serde_json::from_value(proof["proof"].clone()).unwrap();
    assert!(tree::verify(&state_root, ns::PROGRAMS, p.address.as_bytes(), Some(&value), &merkle));
    assert!(!tree::verify(&state_root, ns::PROGRAMS, p.address.as_bytes(), Some(&value[1..]), &merkle), "a forged record doesn't verify");
    let record = crate::state::codec::decode::<Versioned<ProgramRecord>>(&value).unwrap();
    assert_eq!(record.0, n.live.view().state.programs.records[&p.address].0, "the proven record is the one served");
    std::fs::remove_dir_all(root).ok();
}

/// `GET /api/program/{address}/storage`: the program's entries in key order, as hex, a page at a
/// time (`limit`, then `after` = the last page's `next`), reading only the entries it returns and
/// one more, never another program's.
#[tokio::test]
async fn storage_pages_through_one_programs_entries_in_key_order() {
    let (root, n) = chain("papi-storage", Born::V2Programs);
    let a = with_program(&n, 0, &[(b"c", 3, false), (b"a", 1, true), (b"b", 2, false)]);
    // A second program whose entries sit around the first's in the map (its address sorts
    // wherever it does): the first's pages never read them.
    let keys: Vec<[u8; 1]> = (0..50u8).map(|i| [0x10 + i]).collect();
    let b = with_program(&n, 1, &keys.iter().map(|k| (k.as_slice(), 4, false)).collect::<Vec<Write>>());
    let page = |after: Option<&str>, limit: &str| {
        let mut q = format!("/api/program/{}/storage?limit={limit}", a.address);
        if let Some(after) = after {
            q += &format!("&after={after}");
        }
        q
    };
    let read = crate::state::tmap::visits();
    let (s, j, _) = call(&n, &page(None, "2")).await;
    let walked = crate::state::tmap::visits() - read;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(walked, 3, "two entries and the one after them");
    let keys: Vec<&str> = j["entries"].as_array().unwrap().iter().map(|e| e["key"].as_str().unwrap()).collect();
    assert_eq!((keys, j["next"].as_str(), j["program"].as_str()), (vec!["61", "62"], Some("62"), Some(a.address.as_str())));
    let first = &j["entries"][0];
    let cap = blockula_vm::storage::capacity(1, 1);
    assert_eq!(first, &json!({ "key": "61", "value": "61", "payer": label(&n.live.view(), &a.address), "deposit": (100 * u64::from(cap)).to_string(), "capacity": cap }));
    assert_eq!(j["entries"][1]["payer"]["address"], creator(), "the creator paid for b");
    let (_, j, _) = call(&n, &page(Some("62"), "2")).await;
    assert_eq!((j["entries"].as_array().unwrap().len(), j["entries"][0]["key"].as_str(), j["entries"][0]["value"].as_str(), j["next"].is_null()), (1, Some("63"), Some("636363"), true), "the last page says so");
    // The default (and largest) page holds all three; past the last key, nothing.
    let (_, j, _) = call(&n, &format!("/api/program/{}/storage", a.address)).await;
    assert_eq!((j["entries"].as_array().unwrap().len(), j["next"].is_null()), (3, true));
    let (_, j, _) = call(&n, &page(Some("ff"), "500")).await;
    assert_eq!((j["entries"].as_array().unwrap().len(), j["next"].is_null()), (0, true));
    // The second program's pages are its own, 100 entries at most each.
    let (_, j, _) = call(&n, &format!("/api/program/{}/storage?limit=1000", b.address)).await;
    assert_eq!((j["entries"].as_array().unwrap().len(), j["entries"][0]["key"].as_str()), (50, Some("10")));
    // `after` is a key: lowercase hex, at most 128 bytes.
    for after in ["zz", "6", "6A", &"00".repeat(129)] {
        let (s, e, _) = call(&n, &page(Some(after), "2")).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{after}: {e}");
    }
    std::fs::remove_dir_all(root).ok();
}

/// A page ends at the entry that reaches [`STORAGE_PAGE_BYTES`] of keys and values, whatever the
/// limit: twenty 16 KiB values come in pages of 16 and 4.
#[tokio::test]
async fn a_storage_page_is_bounded_in_bytes() {
    let (root, n) = chain("papi-big", Born::V2Programs);
    let keys: Vec<[u8; 2]> = (0..20u8).map(|i| [b'k', i]).collect();
    let writes: Vec<Write> = keys.iter().map(|k| (k.as_slice(), blockula_vm::config::MAX_VALUE_BYTES, false)).collect();
    let p = with_program(&n, 0, &writes);
    let (_, j, text) = call(&n, &format!("/api/program/{}/storage", p.address)).await;
    let per = 2 + blockula_vm::config::MAX_VALUE_BYTES;
    let first = STORAGE_PAGE_BYTES.div_ceil(per);
    assert_eq!((first, j["entries"].as_array().unwrap().len(), j["next"].as_str()), (16, 16, Some("6b0f")));
    assert!(text.len() < 2 * STORAGE_PAGE_BYTES + 2 * per + 64 * 1024, "{} bytes", text.len());
    let (_, j, _) = call(&n, &format!("/api/program/{}/storage?after=6b0f", p.address)).await;
    assert_eq!((j["entries"].as_array().unwrap().len(), j["next"].is_null()), (4, true));
    std::fs::remove_dir_all(root).ok();
}

/// `GET /api/code/{hash}` and `GET /api/interface/{hash}`: a stored blob's facts and its
/// `BlobRef`; the code's method names as `untrusted_exports`, the code itself only with
/// `?wasm=1`; the interface's bytes as hex and its §3.4 JSON as `untrusted_interface`, which is
/// exactly the committed `.iface.json` the bytes were encoded from.
#[tokio::test]
async fn code_and_interfaces_are_served_by_hash() {
    let (root, n) = chain("papi-blobs", Born::V2Programs);
    let p = with_program(&n, 0, &[]);
    with_program(&n, 1, &[]);
    let (s, c, _) = call(&n, &format!("/api/code/{}", hex::encode(p.wasm_hash))).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    let exports = blockula_vm::validate::validate(WASM).unwrap().exports;
    assert_eq!((c["wasm_hash"].as_str(), c["size"].as_u64(), c["untrusted_exports"].clone()), (Some(hex::encode(p.wasm_hash).as_str()), Some(WASM.len() as u64), json!(exports)));
    assert_eq!((c["uploader"]["address"].as_str(), c["deposit"].as_str(), c["refs"].as_u64(), c["uploaded_at"].as_u64(), c["last_used_at"].as_u64()), (Some(creator().as_str()), Some((100 * WASM.len()).to_string().as_str()), Some(2), Some(1), Some(1)));
    assert!(c.get("wasm").is_none() && c.get("exports").is_none());
    let (_, c, _) = call(&n, &format!("/api/code/{}?wasm=1", hex::encode(p.wasm_hash))).await;
    assert_eq!(c["wasm"], hex::encode(WASM));
    let (s, i, _) = call(&n, &format!("/api/interface/{}", hex::encode(p.interface_hash))).await;
    assert_eq!(s, StatusCode::OK, "{i}");
    assert_eq!((i["hex"].as_str(), i["size"].as_u64(), i["refs"].as_u64()), (Some(hex::encode(IFACE).as_str()), Some(IFACE.len() as u64), Some(2)));
    assert_eq!(i["untrusted_interface"], serde_json::from_str::<Value>(IFACE_JSON).unwrap());
    assert!(i.get("interface").is_none());
    std::fs::remove_dir_all(root).ok();
}

/// Every program route answers 404 for what isn't stored and 400 for what can't name it: a name
/// for a program (addresses only, PRG 8.1), a hash that isn't 64 lowercase hex characters.
#[tokio::test]
async fn unknown_programs_and_hashes_are_404_and_malformed_ones_400() {
    let (root, n) = chain("papi-404", Born::V2Programs);
    let p = with_program(&n, 0, &[]);
    let stranger = Keys::from_seed([9; 32]).address();
    for (uri, want) in [
        (format!("/api/program/{stranger}"), StatusCode::NOT_FOUND),
        (format!("/api/program/{stranger}/storage"), StatusCode::NOT_FOUND),
        (format!("/api/program/{}", creator()), StatusCode::NOT_FOUND),
        ("/api/program/alice.boo".into(), StatusCode::BAD_REQUEST),
        (format!("/api/program/{}", p.address.to_uppercase()), StatusCode::BAD_REQUEST),
        (format!("/api/code/{}", "00".repeat(32)), StatusCode::NOT_FOUND),
        (format!("/api/code/{}", hex::encode(p.interface_hash)), StatusCode::NOT_FOUND),
        (format!("/api/interface/{}", hex::encode(p.wasm_hash)), StatusCode::NOT_FOUND),
        ("/api/code/xyz".into(), StatusCode::BAD_REQUEST),
        (format!("/api/code/{}", hex::encode(p.wasm_hash).to_uppercase()), StatusCode::BAD_REQUEST),
        (format!("/api/interface/{}", "0".repeat(63)), StatusCode::BAD_REQUEST),
    ] {
        let (s, e, _) = call(&n, &uri).await;
        assert_eq!(s, want, "{uri}: {e}");
        assert!(e["error"].is_string(), "{uri}: an error body like every route's");
    }
    let (_, e, _) = call(&n, &format!("/api/program/{stranger}")).await;
    assert_eq!(e["error"], "program not found");
    std::fs::remove_dir_all(root).ok();
}

/// Once programs exist, the existing endpoints gain their sections: a program's account says what
/// it is (an ordinary account doesn't change), search finds programs and code, `/api/fees` prices
/// fuel, `/api/params` lists programs' parameters with their bounds and class and META's totals,
/// and `/metrics` reports the gauges.
#[tokio::test]
async fn existing_endpoints_gain_their_program_sections_once_programs_exist() {
    let (root, n) = chain("papi-sections", Born::V2Programs);
    {
        // A non-default parameter, a fuel rate above the floor, and fuel the next block counts.
        let mut c = n.chain.lock();
        let next = c.height() + 1;
        let m = c.state.programs.meta.as_mut().unwrap();
        m.params.insert(Param::ValueCap.key().into(), 5 * FANG);
        m.fuel_rate = 300;
        c.state.programs.block = crate::programs::state::BlockRun { height: next, fuel: 1_234, ..crate::programs::state::BlockRun::default() };
    }
    let p = with_program(&n, 0, &[(b"own", 40, true)]);
    let (validator, h) = (n.chain.lock().me().unwrap(), n.live.view().height);
    let meta = n.live.view().state.programs.meta.clone().unwrap();
    // The block `with_program` ran counted 1,234 fuel of 40M, far below its half-full target, so
    // the fuel controller (`fuel::end_block`) lowered the rate by almost a whole step:
    // 300 - ⌊300 × 12.5% × (20M - 1,234) / 20M⌋ = 263, still above the floor (250).
    assert_eq!(meta.fuel_rate, 263);
    // The program's account, and an ordinary one.
    let (_, a, _) = call(&n, &format!("/api/account/{}", p.address)).await;
    let digest = hex::encode(digest::version_digest(&p.address, &p.wasm_hash, &p.interface_hash, 1));
    assert_eq!(a["program"], json!({ "creator": label(&n.live.view(), &creator()), "created_at": 1, "version_digest": digest, "upgradability": format!("no upgrades exist yet; once a protocol version adds them, {validator} may upgrade it with 172800 blocks' notice"), "accepts_transfers": true, "link": format!("/api/program/{}", p.address) }));
    let (_, a, _) = call(&n, &format!("/api/account/{validator}")).await;
    assert!(a.get("program").is_none() && a["balance"].is_string());
    // Search: a program, a stored code; an interface hash isn't a search target.
    assert_eq!(call(&n, &format!("/api/search/{}", p.address)).await.1, json!({ "type": "program", "id": p.address }));
    assert_eq!(call(&n, &format!("/api/search/{}", hex::encode(p.wasm_hash))).await.1, json!({ "type": "code", "id": hex::encode(p.wasm_hash) }));
    assert_eq!(call(&n, &format!("/api/search/{}", hex::encode(p.interface_hash))).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&n, &format!("/api/search/{validator}")).await.1, json!({ "type": "account", "id": validator }));
    // `/api/fees`' fuel section (§5.5). `suggested_rate` is 263 raised by a full block's step
    // (an eighth) 17 times: 1,920.
    let (_, f, _) = call(&n, "/api/fees").await;
    let fuel = &f["fuel"];
    let control = meta.value_held + meta.sponsored;
    let price = blockula_vm::storage::price(100_000, 64 << 20, meta.state_bytes).unwrap();
    assert_eq!(
        fuel,
        &json!({
            "status": "on", "rate": 263, "floor_rate": 250, "suggested_rate": 1_920, "rate_unit": "milli-drops per fuel", "block_fuel": 1_234,
            "max_tx_fuel": 10_000_000, "max_block_fuel": 40_000_000, "schedule": 0,
            "load_base": 10_000, "load_per_byte": 40, "upload_base": 50_000, "upload_per_byte": 71,
            "max_tx_code_bytes": 524_288, "storage_price": price.to_string(), "state_bytes": meta.state_bytes,
            "state_soft_bytes": 67_108_864, "value_cap": (5 * FANG).to_string(), "value_held": "500",
            "sponsored": meta.sponsored.to_string(), "value_headroom": (5 * FANG - control).to_string(), "paused_until": null,
        })
    );
    // `/api/params`' programs section (§8.2, §4.5): every key with its class.
    let (_, pr, _) = call(&n, "/api/params").await;
    let ps = &pr["programs"];
    assert_eq!((ps["status"].as_str(), ps["paused_until"].is_null(), ps["fuel_rate"].as_u64(), ps["programs"].as_u64()), (Some("on"), true, Some(263), Some(1)));
    assert_eq!((ps["value_held"].as_str(), ps["escrow"].as_str(), ps["state_bytes"].as_u64()), (Some("500"), Some(meta.escrow.to_string().as_str()), Some(meta.state_bytes)));
    assert_eq!((ps["pause"].is_null(), ps["votes"].as_array().map(Vec::len), ps["pause_signed"].as_array().map(Vec::len)), (true, Some(0), Some(0)));
    let params = n.live.view().state.params.clone();
    let listed: Vec<&String> = ps["params"].as_object().unwrap().keys().collect();
    assert_eq!(listed, Param::ALL.map(Param::key).to_vec());
    for k in Param::ALL {
        let (min, max) = k.bounds(&params);
        let class = if k == Param::MinUpgradeDelayBlocks { "tokens_only" } else { "council" };
        let value = if k == Param::ValueCap { 5 * FANG } else { k.default(&params) };
        assert_eq!(ps["params"][k.key()], json!({ "value": value.to_string(), "default": k.default(&params).to_string(), "min": min.to_string(), "max": max.to_string(), "class": class }), "{k:?}");
    }
    assert!(pr["params"].get(Param::ValueCap.key()).is_none(), "programs' keys aren't ChainParams'");
    // `/metrics`.
    let (_, _, text) = call(&n, "/metrics").await;
    for want in ["blockula_programs_status 1\n", "blockula_program_block_fuel 1234\n", "blockula_program_fuel_rate 263\n", "blockula_program_value_held_drops 500\n", &format!("blockula_program_sponsored_drops {}\n", meta.sponsored), &format!("blockula_program_state_bytes {}\n", meta.state_bytes)] {
        assert!(text.contains(want), "{want} in {text}");
    }
    assert!(text.contains("# TYPE blockula_program_fuel_rate gauge"));
    assert_eq!(h, n.live.view().height);
    std::fs::remove_dir_all(root).ok();
}

/// `/api/fees`' `suggested_rate` covers `FEE_LAG` blocks of full fuel (§5.5): a call priced as the
/// spec tells clients to, `max_fee(size) + suggested_rate × reserved / 1000` from the node's own
/// answers (`suggested`, `rate_scale`, the fuel section's schedule and `/api/program`'s code size),
/// still covers its base fee after each of 17 blocks in a row whose fuel fills
/// `program_max_block_fuel`, where the rate reaches `suggested_rate` exactly; the 18th is the
/// first block whose rate it may not cover.
#[tokio::test]
async fn a_fee_priced_at_the_suggested_rate_covers_seventeen_full_blocks() {
    use crate::programs::fixtures as fx;
    let (root, n) = chain("papi-suggested", Born::V2Programs);
    let p = with_program(&n, 0, &[]);
    let (_, f, _) = call(&n, "/api/fees").await;
    let section = &f["fuel"];
    let int = |v: &Value| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap();
    let (rate, suggested, limit) = (int(&section["rate"]), int(&section["suggested_rate"]), int(&section["max_block_fuel"]));
    assert_eq!((rate, suggested), (250, 1_833), "at the floor, §5.5's example");
    // A call of 1M fuel on the program, reserving its own code's load on top.
    let (_, j, _) = call(&n, &format!("/api/program/{}", p.address)).await;
    let reserved = 1_000_000 + int(&section["load_base"]) + int(&section["load_per_byte"]) * int(&j["code"]["size"]);
    let version = hex::encode(digest::version_digest(&p.address, &p.wasm_hash, &p.interface_hash, 1));
    let action = fx::calls(vec![fx::call(&p.address, &version, "transfer", 0)], 1_000_000, vec![]);
    let signer = Keys::from_seed([3; 32]);
    let unpriced = fx::signed(&n.chain.lock(), &signer, 0, 0, action.clone());
    let size = unpriced.size();
    let max_fee = int(&f["suggested"]["flat"]) + int(&f["suggested"]["rate"]) * size / int(&f["rate_scale"]);
    let tx = fx::signed(&n.chain.lock(), &signer, 0, max_fee + suggested * reserved / 1_000, action);
    assert_eq!(tx.size(), size);
    {
        let c = n.chain.lock();
        assert_eq!(fuel::reserved(&c.state, &tx, crate::econ::now(&c.state)), reserved, "the client's reservation is the node's");
    }
    for k in 1..=crate::econ::FEE_LAG + 1 {
        {
            let mut c = n.chain.lock();
            let next = c.height() + 1;
            c.state.programs.block = crate::programs::state::BlockRun { height: next, fuel: limit, ..crate::programs::state::BlockRun::default() };
            c.produce_block_with(vec![]).unwrap();
            let (now, rate) = (crate::econ::now(&c.state), fuel::rate(&c.state));
            let check = crate::econ::check_fee(&c.state, &tx, now);
            if k <= crate::econ::FEE_LAG {
                assert!(rate <= suggested, "after {k} full blocks the rate is {rate}");
                assert!(check.is_ok(), "after {k} full blocks: {check:?}");
            } else {
                assert!(rate > suggested, "one block past FEE_LAG");
                assert!(check.unwrap_err().to_string().starts_with("fee too low"));
            }
        }
        if k == crate::econ::FEE_LAG {
            assert_eq!(int(&call(&n, "/api/fees").await.1["fuel"]["rate"]), suggested, "17 full blocks reach it exactly");
        }
    }
    std::fs::remove_dir_all(root).ok();
}

/// A running pause and exit-only read as such everywhere: `/api/fees`, `/api/params` and
/// `/metrics` say so, and the program routes keep serving (only `Dormant` hides them).
#[tokio::test]
async fn a_pause_and_exit_only_read_as_such() {
    let (root, n) = chain("papi-pause", Born::V2Programs);
    let p = with_program(&n, 0, &[]);
    let validator = n.chain.lock().me().unwrap();
    {
        let mut c = n.chain.lock();
        let next = c.height() + 1;
        let m = c.state.programs.meta.as_mut().unwrap();
        // A run that started at the block about to run, and its signer's record: the next height
        // after it is inside [from, until).
        m.pause = Some(Pause { from: next, until: next + 100, started: next, signers: vec![validator.clone()] });
        m.pause_signed.insert(validator.clone(), next - 1);
        c.produce_block_with(vec![]).unwrap();
    }
    let until = n.live.view().state.programs.meta.as_ref().unwrap().pause.as_ref().unwrap().until;
    let (_, f, _) = call(&n, "/api/fees").await;
    assert_eq!((f["fuel"]["status"].as_str(), f["fuel"]["paused_until"].as_u64()), (Some("paused"), Some(until)));
    let (_, pr, _) = call(&n, "/api/params").await;
    let ps = &pr["programs"];
    assert_eq!((ps["status"].as_str(), ps["paused_until"].as_u64(), ps["pause"]["until"].as_u64(), ps["pause"]["signers"][0]["address"].as_str()), (Some("paused"), Some(until), Some(until), Some(validator.as_str())));
    assert_eq!((ps["pause_signed"][0]["validator"]["address"].as_str(), ps["pause_signed"][0]["at"].as_u64()), (Some(validator.as_str()), Some(until - 101)));
    assert!(call(&n, "/metrics").await.2.contains("blockula_programs_status 3\n"));
    assert_eq!(call(&n, &format!("/api/program/{}", p.address)).await.0, StatusCode::OK);
    // Switched off (META kept): exit-only, once the pause has been lapsed by hand.
    {
        let mut c = n.chain.lock();
        c.state.upgrade.features.remove(PROGRAMS_FEATURE);
        c.state.programs.meta.as_mut().unwrap().pause = None;
        c.produce_block_with(vec![]).unwrap();
    }
    let (_, f, _) = call(&n, "/api/fees").await;
    assert_eq!((f["fuel"]["status"].as_str(), f["fuel"]["paused_until"].is_null()), (Some("exit_only"), true));
    assert_eq!(call(&n, "/api/params").await.1["programs"]["status"], "exit_only");
    assert!(call(&n, "/metrics").await.2.contains("blockula_programs_status 2\n"));
    assert_eq!(call(&n, &format!("/api/program/{}", p.address)).await.0, StatusCode::OK);
    std::fs::remove_dir_all(root).ok();
}

/// The top-level keys of each endpoint programs extend, as they were before programs were built
/// (`automation/programs-v0` at `296a466b`, checked against that binary's answers too): a dormant
/// chain must answer with exactly these, so v1 clients and the explorer's recorded vectors see no
/// change.
const FEES_KEYS: &[&str] = &["account_creation_fee", "account_creation_fee_display", "floor", "height", "lag", "lowest", "max_change_bps", "rate_scale", "slots", "suggested", "target_fullness_bps", "tip_cap_multiple"];
const PARAMS_KEYS: &[&str] = &["burned", "classes", "genesis", "issuance", "params", "supply"];
const ACCOUNT_KEYS: &[&str] = &["account", "balance", "balance_display", "contribution", "creation_fee", "exists", "familiars", "github", "hunts_created", "hunts_won", "keys", "locked", "locked_display", "locks", "names", "next_nonce", "nonce", "recent_txs", "transferable", "transferable_display"];

fn keys_of(v: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    keys
}

/// While programs are dormant (a v1 chain, and a chain born at v2 before programs first switch
/// on) nothing of programs shows: every program route is 404, and `/api/fees` (no `fuel`, which
/// reads as `null`), `/api/params`, `/api/account`, `/api/search` and `/metrics` answer exactly as
/// before programs were built.
#[tokio::test]
async fn dormant_chains_answer_exactly_as_before() {
    for born in [Born::V1, Born::V2] {
        let (root, n) = chain(&format!("papi-dormant-{born:?}").to_lowercase(), born);
        let version = crate::upgrade::version_at(&n.live.view().state, n.live.view().height);
        assert_eq!(version, if born == Born::V1 { 1 } else { 2 }, "{born:?}: the block that ran");
        assert_eq!(status_now(&n.live.view().state), None, "{born:?}");
        let validator = n.chain.lock().me().unwrap();
        // Anything a program route could name: the routes don't look.
        let prefix = n.live.view().state.params.prefix().to_string();
        let address = digest::program_address(&prefix, &crypto::address_bytes(&validator).unwrap(), 0);
        for uri in [format!("/api/program/{address}"), format!("/api/program/{validator}/storage"), "/api/program/x".into(), format!("/api/code/{}", "ab".repeat(32)), format!("/api/interface/{}", "ab".repeat(32)), "/api/code/xyz".into()] {
            let (s, e, _) = call(&n, &uri).await;
            assert_eq!((s, e["error"].as_str()), (StatusCode::NOT_FOUND, Some("no programs exist on this chain: programs have never been switched on")), "{born:?} {uri}");
        }
        let (_, f, _) = call(&n, "/api/fees").await;
        assert_eq!(keys_of(&f), FEES_KEYS, "{born:?}");
        assert!(f["fuel"].is_null(), "{born:?}: a client reads no fuel section as null");
        let (_, p, _) = call(&n, "/api/params").await;
        assert_eq!(keys_of(&p), PARAMS_KEYS, "{born:?}");
        assert!(p["params"].as_object().unwrap().keys().all(|k| !k.starts_with(params::PREFIX)), "{born:?}");
        let (_, a, _) = call(&n, &format!("/api/account/{validator}")).await;
        assert_eq!(keys_of(&a), ACCOUNT_KEYS, "{born:?}");
        assert_eq!(call(&n, &format!("/api/search/{validator}")).await.1, json!({ "type": "account", "id": validator }), "{born:?}");
        assert_eq!(call(&n, &format!("/api/search/{}", "ab".repeat(32))).await.0, StatusCode::NOT_FOUND, "{born:?}");
        let (_, _, text) = call(&n, "/metrics").await;
        assert!(text.contains("blockula_height ") && !text.contains("program"), "{born:?}: {text}");
        std::fs::remove_dir_all(root).ok();
    }
}

/// `/api/params`' programs section says whether a fix is pending (§7.2, §8.2: `pause::fix_pending`
/// at the next height, which waives a validator's window between pauses): `false` on a fresh chain,
/// `true` once an upgrade is scheduled above the height, as the CLI's `pause` reads it; and it's part
/// of the section, which is absent with the rest of programs' answers while they're dormant.
#[tokio::test]
async fn params_say_whether_a_fix_is_pending_once_programs_exist() {
    for born in [Born::V1, Born::V2] {
        let (root, n) = chain(&format!("papi-nofix-{born:?}").to_lowercase(), born);
        let (_, p, _) = call(&n, "/api/params").await;
        assert!(p.get("programs").is_none() && !p.to_string().contains("fix_pending"), "{born:?}: nothing of programs while they're dormant: {p}");
        std::fs::remove_dir_all(root).ok();
    }
    let (root, n) = chain("papi-fix", Born::V2Programs);
    let fix_pending = |n: &Node| {
        let v = n.live.view();
        (crate::programs::pause::fix_pending(&v.state, crate::econ::now(&v.state)), v.height)
    };
    let (_, p, _) = call(&n, "/api/params").await;
    assert_eq!((p["programs"]["fix_pending"].clone(), fix_pending(&n).0), (json!(false), false));
    {
        let mut c = n.chain.lock();
        let at = c.height() + 1_000_000;
        c.state.upgrade.schedule.push(ScheduledUpgrade { version: 3, activation_height: at });
        c.produce_block_with(vec![]).unwrap();
    }
    let (_, p, _) = call(&n, "/api/params").await;
    assert_eq!((p["programs"]["fix_pending"].clone(), fix_pending(&n).0), (json!(true), true), "an upgrade scheduled above the height is a fix pending");
    std::fs::remove_dir_all(root).ok();
}

/// A record's policy byte reads as `CreateProgram`'s JSON names it, and an immutable program
/// says so.
#[test]
fn policy_names_and_upgradability() {
    use crate::types::ProgramPolicy;
    assert_eq!((json!(policy_name(0)), json!(policy_name(1)), policy_name(2)), (json!(ProgramPolicy::Open), json!(ProgramPolicy::Additive), "unknown"));
    let record = ProgramRecord {
        creator: creator(),
        created_at: 1,
        wasm_hash: [1; 32],
        interface_hash: [2; 32],
        version: 1,
        authority: None,
        upgrade_delay_blocks: 0,
        policy: 0,
        accepts_transfers: false,
        items: 0,
        bytes: 0,
        deposit: 0,
        sponsored: 0,
        record_deposit: 0,
        record_payer: creator(),
    };
    assert_eq!(upgradability(&record), "immutable");
}
