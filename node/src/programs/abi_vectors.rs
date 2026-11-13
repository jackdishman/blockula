//! Vectors for the SDK's ABI codec (sdk-ts/src/programs.ts `encodeProgramInput`,
//! `decodeProgramValue`): values of every interface type (docs/PROGRAMS-V0.md §3.2), each
//! encoded by the `borsh` crate the node pins, which is what "Borsh by convention" (PRG 11.3,
//! `blockula_vm::interface::Type`) means. The chain never reads a call's input against its
//! interface, so nothing here is consensus: these hold the wallet's encoder to the encoding
//! programs written in Rust decode, and its decoder to what Borsh refuses.
//!
//! Regenerate `sdk-ts/test/program-abi-vectors.json` with
//! `cargo test --manifest-path node/Cargo.toml programs::abi_vectors::write_abi_vectors -- --ignored`.

use super::interface_json::TypeJson;
use crate::crypto::{self, Keys};
use borsh::{BorshDeserialize, BorshSerialize};
use serde_json::{json, Value};

/// `sdk-ts/test/program-abi-vectors.json`, found from the crate's directory (`node/`, or
/// `node/fuzz/`, which compiles these modules too).
fn file() -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    dir.ancestors().map(|d| d.join("sdk-ts/test/program-abi-vectors.json")).find(|p| p.exists()).unwrap_or_else(|| dir.join("../sdk-ts/test/program-abi-vectors.json"))
}

fn text(max: u16) -> TypeJson {
    TypeJson::Text(max)
}
fn option(of: TypeJson) -> TypeJson {
    TypeJson::Option(Box::new(of))
}
fn vec_of(of: TypeJson, max: u16) -> TypeJson {
    TypeJson::Vec(Box::new(of), max)
}

/// One value: its type, the value as the SDK writes it in JSON (integers as decimal strings,
/// addresses as `boo1…`, hashes and bytes as hex, `null` or `{ "some": … }` for an option), and
/// Borsh's bytes for `rust`, the same value as a Rust type.
fn value<T: BorshSerialize>(label: &str, ty: TypeJson, rust: &T, shown: Value) -> Value {
    json!({ "label": label, "type": ty, "value": shown, "hex": hex::encode(borsh::to_vec(rust).unwrap()) })
}

/// Bytes Borsh refuses as a `T` (`from_slice` reads all of them or fails), which the SDK's
/// decoder must refuse as `ty` too.
fn refused<T: BorshDeserialize>(label: &str, ty: TypeJson, bytes: &str) -> Value {
    let raw = hex::decode(bytes).unwrap();
    assert!(borsh::from_slice::<T>(&raw).is_err(), "borsh reads {label} ({bytes})");
    json!({ "label": label, "type": ty, "hex": bytes })
}

fn abi_vectors() -> Value {
    let a = crypto::address_bytes(&Keys::from_seed([1; 32]).address()).unwrap();
    let b = crypto::address_bytes(&Keys::from_seed([2; 32]).address()).unwrap();
    let boo = |x: &[u8; 32]| crypto::render_address(crypto::ADDRESS_HRP, x);
    // The escrow guest's `open(id: u64, seller: address)` exactly as `blockula dev --programs` carried it
    // (sdk-ts/test/program-fixtures.json `escrow.tx_open`): a real call's input.
    let seller: [u8; 32] = hex::decode("ab69b25473cf117573037e7702f8ab644107dd24a65ef646616c8c53e835368e").unwrap().try_into().unwrap();

    let values = vec![
        value("u8 0", TypeJson::U8, &0u8, json!("0")),
        value("u8 255", TypeJson::U8, &255u8, json!("255")),
        value("u16 513", TypeJson::U16, &513u16, json!("513")),
        value("u32 max", TypeJson::U32, &u32::MAX, json!(u32::MAX.to_string())),
        value("u64 1", TypeJson::U64, &1u64, json!("1")),
        value("u64 max", TypeJson::U64, &u64::MAX, json!(u64::MAX.to_string())),
        value("u128 2^64", TypeJson::U128, &(1u128 << 64), json!((1u128 << 64).to_string())),
        value("u128 max", TypeJson::U128, &u128::MAX, json!(u128::MAX.to_string())),
        value("i64 -1", TypeJson::I64, &-1i64, json!("-1")),
        value("i64 min", TypeJson::I64, &i64::MIN, json!(i64::MIN.to_string())),
        value("i64 max", TypeJson::I64, &i64::MAX, json!(i64::MAX.to_string())),
        value("i128 -2", TypeJson::I128, &-2i128, json!("-2")),
        value("i128 min", TypeJson::I128, &i128::MIN, json!(i128::MIN.to_string())),
        value("bool true", TypeJson::Bool, &true, json!(true)),
        value("bool false", TypeJson::Bool, &false, json!(false)),
        value("fang 1.5 FANG in drops", TypeJson::Fang, &1_500_000_000u64, json!("1500000000")),
        value("an address", TypeJson::Address, &a, json!(boo(&a))),
        value("a name's label", TypeJson::Name, &"alice".to_string(), json!("alice")),
        value("a hash32", TypeJson::Hash32, &[0xab_u8; 32], json!("ab".repeat(32))),
        value("bytes", TypeJson::Bytes(8), &vec![0u8, 1, 2, 255], json!("000102ff")),
        value("no bytes", TypeJson::Bytes(8), &Vec::<u8>::new(), json!("")),
        value("text", text(32), &"hello".to_string(), json!("hello")),
        value("empty text", text(32), &String::new(), json!("")),
        value("text of 7 characters and 11 bytes, at its bound", text(11), &"héllo 👻".to_string(), json!("héllo 👻")),
        value("text with a byte-order mark first", text(8), &"\u{feff}hi".to_string(), json!("\u{feff}hi")),
        value("option none", option(TypeJson::U64), &None::<u64>, json!(null)),
        value("option some", option(TypeJson::U64), &Some(7u64), json!({ "some": "7" })),
        value("option of text", option(text(32)), &Some("x".to_string()), json!({ "some": "x" })),
        value("option of an option: none", option(option(TypeJson::Bool)), &None::<Option<bool>>, json!(null)),
        value("option of an option: some none", option(option(TypeJson::Bool)), &Some(None::<bool>), json!({ "some": null })),
        value("option of an option: some some", option(option(TypeJson::Bool)), &Some(Some(true)), json!({ "some": { "some": true } })),
        value("vec of addresses", vec_of(TypeJson::Address, 8), &vec![a, b], json!([boo(&a), boo(&b)])),
        value("empty vec", vec_of(TypeJson::U64, 4), &Vec::<u64>::new(), json!([])),
        value("vec of options of text", vec_of(option(text(8)), 3), &vec![Some("a".to_string()), None], json!([{ "some": "a" }, null])),
        value("four layers", option(vec_of(option(vec_of(TypeJson::U8, 2)), 2)), &Some(vec![Some(vec![1u8, 2]), None]), json!({ "some": [{ "some": ["1", "2"] }, null] })),
    ];

    // A call's input: its arguments back to back, which is Borsh's encoding of them as a tuple.
    let input = |label: &str, args: Vec<TypeJson>, bytes: Vec<u8>, shown: Value| json!({ "label": label, "args": args, "values": shown, "hex": hex::encode(bytes) });
    let inputs = vec![
        input("no arguments", vec![], borsh::to_vec(&()).unwrap(), json!([])),
        input("escrow's open(id: u64, seller: address), as the recorded call carried it", vec![TypeJson::U64, TypeJson::Address], borsh::to_vec(&(1u64, seller)).unwrap(), json!(["1", boo(&seller)])),
        input("token's transfer(to: address, amount: u64)", vec![TypeJson::Address, TypeJson::U64], borsh::to_vec(&(b, 250u64)).unwrap(), json!([boo(&b), "250"])),
        input(
            "register(label: text(32), handle: option<text(32)>, tweet: option<text(200)>)",
            vec![text(32), option(text(32)), option(text(200))],
            borsh::to_vec(&("Darkoh".to_string(), Some("darkoh_x".to_string()), None::<String>)).unwrap(),
            json!(["Darkoh", { "some": "darkoh_x" }, null]),
        ),
        input("every kind once", vec![TypeJson::Bool, TypeJson::Fang, TypeJson::Name, TypeJson::Hash32, TypeJson::Bytes(4), TypeJson::I128], borsh::to_vec(&(true, 5u64, "bat".to_string(), [7u8; 32], vec![9u8], -3i128)).unwrap(), json!([true, "5", "bat", "07".repeat(32), "09", "-3"])),
    ];

    let refusals = vec![
        refused::<bool>("a bool of 2", TypeJson::Bool, "02"),
        refused::<Option<u8>>("an option tagged 2", option(TypeJson::U8), "0201"),
        refused::<String>("text that isn't UTF-8", text(32), "02000000c328"),
        refused::<String>("text cut short", text(32), "05000000616263"),
        refused::<u64>("a u64 of 7 bytes", TypeJson::U64, "01000000000000"),
        refused::<u8>("a byte left over", TypeJson::U8, "0100"),
        refused::<Vec<u8>>("a vec that says 5 items and holds 2", vec_of(TypeJson::U8, 8), "050000000102"),
        refused::<[u8; 32]>("an address of 31 bytes", TypeJson::Address, &"ab".repeat(31)),
    ];

    json!({
        "description": "The ABI's Borsh encoding (node/src/programs/abi_vectors.rs), for sdk-ts's codec (sdk-ts/src/programs.ts encodeProgramInput, decodeProgramValue; docs/PROGRAMS-V0.md §3.2). Every hex is the borsh crate's. Values: integers and drops as decimal strings, addresses as boo1…, hash32 and bytes as hex, an option as null or { some }. Regenerate: cargo test --manifest-path node/Cargo.toml programs::abi_vectors::write_abi_vectors -- --ignored",
        "values": values,
        "inputs": inputs,
        "refused": refusals,
    })
}

/// The SDK's vector file is these vectors: regenerate it when they change.
#[test]
fn abi_vectors_match_the_sdk_file() {
    let file: Value = serde_json::from_str(&std::fs::read_to_string(file()).expect("sdk-ts/test/program-abi-vectors.json")).unwrap();
    assert!(file == abi_vectors(), "the ABI vectors changed: regenerate sdk-ts/test/program-abi-vectors.json (programs::abi_vectors::write_abi_vectors)");
}

#[test]
#[ignore = "generator, run by hand (see the module's docs)"]
fn write_abi_vectors() {
    std::fs::write(file(), serde_json::to_string_pretty(&abi_vectors()).unwrap() + "\n").unwrap();
}
