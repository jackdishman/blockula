//! The five digests that name what programs are made of (docs/PROGRAMS-V0.md §2.1), each BLAKE3
//! in derive-key mode under a context of its own, as `types::action_digest` uses it:
//!
//! ```text
//! wasm_hash      = derive_key("blockula program code v1", wasm)
//! interface_hash = derive_key("blockula program interface v1", interface)
//! version_digest = derive_key("blockula program version v1", str address ‖ wasm_hash ‖ interface_hash ‖ version u32 LE)
//! address        = render(prefix, derive_key("blockula program address v1", creator's 32 address bytes ‖ nonce u64 LE))
//! selector       = derive_key("blockula program method v1", method name)[0..8]
//! ```
//!
//! `str address` is the program's address as the tx codec writes a string (`types::put_str`: a
//! u16 length, then its bytes). `version` is 1 for every v0 program (only a later upgrade action
//! raises it). `nonce` is the creating transaction's, so each create gets an address of its own,
//! and a failed one spends it (§2.3). The selector is the VM's (`blockula_vm::interface`), which
//! checks an interface's selectors are distinct; it's re-exported here, never defined twice.
//!
//! Frozen with protocol v2 (§9.2). The SDK computes the same (`sdk-ts`, §8.4), checked offline
//! against `sdk-ts/test/program-vectors.json`, which this module's tests write.

// The API serves each method's selector (`programs::api`); only tests read the context.
#[cfg_attr(not(test), allow(unused_imports))]
pub use blockula_vm::interface::{selector, SELECTOR_CONTEXT};

/// `wasm_hash`'s context: names a program's code (`CodeRecord`'s key).
pub const CODE_CONTEXT: &str = "blockula program code v1";
/// `interface_hash`'s context: names an interface (`InterfaceRecord`'s key).
pub const INTERFACE_CONTEXT: &str = "blockula program interface v1";
/// `version_digest`'s context: what a call binds (the program, its code and interface, its
/// version).
pub const VERSION_CONTEXT: &str = "blockula program version v1";
/// A program address's context.
pub const ADDRESS_CONTEXT: &str = "blockula program address v1";

/// The version of every v0 program: only an upgrade (a later protocol version) raises it.
pub const FIRST_VERSION: u32 = 1;

/// The hash that names a program's code.
pub fn wasm_hash(wasm: &[u8]) -> [u8; 32] {
    blake3::derive_key(CODE_CONTEXT, wasm)
}

/// The hash that names an interface (its format-1 bytes).
pub fn interface_hash(interface: &[u8]) -> [u8; 32] {
    blake3::derive_key(INTERFACE_CONTEXT, interface)
}

/// What a call to the program at `address` binds: its code, its interface and its version. A
/// call runs only while this is the program's digest in force, so it runs the code its signer
/// was shown.
pub fn version_digest(address: &str, wasm_hash: &[u8; 32], interface_hash: &[u8; 32], version: u32) -> [u8; 32] {
    let mut out = vec![];
    crate::types::put_str(&mut out, address);
    out.extend_from_slice(wasm_hash);
    out.extend_from_slice(interface_hash);
    out.extend_from_slice(&version.to_le_bytes());
    blake3::derive_key(VERSION_CONTEXT, &out)
}

/// The 32 address bytes of the program that `creator` (its address bytes) creates with the
/// transaction of `nonce`. A KDF output, so no key hashes to it: a program never signs.
pub fn program_address_bytes(creator: &[u8; 32], nonce: u64) -> [u8; 32] {
    let mut out = [0; 40];
    out[..32].copy_from_slice(creator);
    out[32..].copy_from_slice(&nonce.to_le_bytes());
    blake3::derive_key(ADDRESS_CONTEXT, &out)
}

/// [`program_address_bytes`], rendered with the chain's address prefix.
pub fn program_address(prefix: &str, creator: &[u8; 32], nonce: u64) -> String {
    crate::crypto::render_address(prefix, &program_address_bytes(creator, nonce))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{self, Keys};
    use crate::types::tests::{vector_interface, vector_wasm};
    use serde_json::{json, Value};

    /// `sdk-ts/test/program-vectors.json`, found from the crate's directory (`node/`, or
    /// `node/fuzz/`, which compiles these modules too).
    fn file() -> std::path::PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        dir.ancestors().map(|d| d.join("sdk-ts/test/program-vectors.json")).find(|p| p.exists()).unwrap_or_else(|| dir.join("../sdk-ts/test/program-vectors.json"))
    }

    /// Digest vectors for the SDK's port (`versionDigest`, `programAddress`, `wasmHash`,
    /// `interfaceHash`, `methodSelector` in sdk-ts/src/tx.ts, WBS S7a): each digest's context,
    /// then inputs and outputs (bytes as hex).
    fn program_vectors() -> Value {
        let (wasm, interface) = (vector_wasm(), vector_interface());
        let blobs = |f: fn(&[u8]) -> [u8; 32]| [(String::new(), vec![]), ("the Wasm header and a type section".into(), wasm.clone()), ("an interface: get, increment, init".into(), interface.clone()), ("1,025 bytes of 0xab (past one BLAKE3 chunk)".into(), vec![0xab; 1025])].map(|(label, b)| json!({ "label": label, "input": hex::encode(&b), "hash": hex::encode(f(&b)) }));
        let creators = [Keys::from_seed([1; 32]), Keys::from_seed([2; 32])].map(|k| crypto::address_bytes(&k.address()).unwrap());
        let addresses = [(creators[0], 0), (creators[0], 1), (creators[1], 7), (creators[1], u64::MAX)].map(|(c, nonce)| {
            json!({ "creator": hex::encode(c), "nonce": nonce.to_string(), "bytes": hex::encode(program_address_bytes(&c, nonce)), "boo": program_address(crypto::ADDRESS_HRP, &c, nonce), "tboo": program_address(crypto::TESTNET_HRP, &c, nonce) })
        });
        let (w, i) = (wasm_hash(&wasm), interface_hash(&interface));
        let program = program_address(crypto::ADDRESS_HRP, &creators[0], 0);
        let testnet = program_address(crypto::TESTNET_HRP, &creators[0], 0);
        let versions = [(program.clone(), w, i, FIRST_VERSION), (testnet, w, i, FIRST_VERSION), (program.clone(), i, w, FIRST_VERSION), (program, w, i, u32::MAX)]
            .map(|(address, w, i, version)| json!({ "address": address, "wasm_hash": hex::encode(w), "interface_hash": hex::encode(i), "version": version, "digest": hex::encode(version_digest(&address, &w, &i, version)) }));
        let selectors = ["a", "get", "increment", "init", "transfer", "a_method_name_of_32_characters_x"].map(|name| json!({ "method": name, "selector": hex::encode(selector(name)) }));
        json!({
            "description": "Programs' digests (node/src/programs/digest.rs `program_vectors`), for sdk-ts's ports (docs/PROGRAMS-V0.md §2.1). Regenerate: cargo test --manifest-path node/Cargo.toml programs::digest::tests::write_program_vectors -- --ignored",
            "contexts": { "code": CODE_CONTEXT, "interface": INTERFACE_CONTEXT, "version": VERSION_CONTEXT, "address": ADDRESS_CONTEXT, "method": SELECTOR_CONTEXT },
            "wasm_hashes": blobs(wasm_hash),
            "interface_hashes": blobs(interface_hash),
            "program_addresses": addresses,
            "version_digests": versions,
            "selectors": selectors,
        })
    }

    /// The digests are consensus rules: the SDK's vector file must match exactly.
    #[test]
    fn program_vectors_match_the_sdk_file() {
        let file: Value = serde_json::from_str(&std::fs::read_to_string(file()).expect("sdk-ts/test/program-vectors.json")).unwrap();
        assert!(file == program_vectors(), "a program digest or its vectors changed: regenerate sdk-ts/test/program-vectors.json (programs::digest::tests::write_program_vectors)");
    }

    #[test]
    #[ignore = "generator, run by hand (see program_vectors)"]
    fn write_program_vectors() {
        std::fs::write(file(), serde_json::to_string_pretty(&program_vectors()).unwrap() + "\n").unwrap();
    }

    /// Each digest is pinned (§9.2 freezes them), and each is what §2.1 says it is: its own
    /// context over exactly the bytes listed, so no two digests of different things meet. (The
    /// pinned values were computed again from §2.1's formulas by another implementation,
    /// @noble/hashes' BLAKE3 and @scure/base's bech32m, when they were written.)
    #[test]
    fn digests_are_pinned_and_domain_separated() {
        let (wasm, interface) = (vector_wasm(), vector_interface());
        assert_eq!(hex::encode(wasm_hash(&wasm)), "a1abd0dc7a017101838b64f6e46c876811adc1c7ea15e996bdf32113fef57942");
        assert_eq!(hex::encode(interface_hash(&interface)), "eedaaa7493e4c2c3dd1026c0fbd19e42bf176fbd59c30a342cdf84443589a396");
        // The same bytes hash apart as code and as an interface.
        assert_ne!(wasm_hash(&interface), interface_hash(&interface));
        let creator = crypto::address_bytes(&Keys::from_seed([1; 32]).address()).unwrap();
        let program = program_address(crypto::ADDRESS_HRP, &creator, 0);
        assert_eq!(program, "boo1r8t9laxrw4l2n6wdtya7ljxz8va293favpwccy8xpuzn2mhds3rqkjfhj5");
        // The address is the KDF over the creator's bytes and the nonce, little-endian.
        let mut input = creator.to_vec();
        input.extend_from_slice(&0u64.to_le_bytes());
        assert_eq!(program_address_bytes(&creator, 0), blake3::derive_key("blockula program address v1", &input));
        assert_ne!(program_address_bytes(&creator, 0), program_address_bytes(&creator, 1), "each nonce its own address");
        assert_eq!(crypto::address_bytes(&program_address(crypto::TESTNET_HRP, &creator, 0)), Some(program_address_bytes(&creator, 0)), "the prefix only renders it");
        // The version digest: the address as `put_str` writes it (u16 LE length, its bytes), the
        // two hashes and the version, u32 LE.
        let (w, i) = (wasm_hash(&wasm), interface_hash(&interface));
        let mut input = (program.len() as u16).to_le_bytes().to_vec();
        input.extend_from_slice(program.as_bytes());
        input.extend_from_slice(&w);
        input.extend_from_slice(&i);
        input.extend_from_slice(&1u32.to_le_bytes());
        assert_eq!(version_digest(&program, &w, &i, FIRST_VERSION), blake3::derive_key("blockula program version v1", &input));
        assert_eq!(hex::encode(version_digest(&program, &w, &i, FIRST_VERSION)), "2460883bca10960e0331c2fb223de83689cd1dd17bfd6a54ef18af47366410d8");
        // Each part moves it: another address (even the same bytes with another prefix), code,
        // interface or version.
        let testnet = program_address(crypto::TESTNET_HRP, &creator, 0);
        let digests = [version_digest(&program, &w, &i, 1), version_digest(&testnet, &w, &i, 1), version_digest(&program, &i, &w, 1), version_digest(&program, &w, &i, 2)];
        assert_eq!(digests.iter().collect::<std::collections::BTreeSet<_>>().len(), digests.len());
        // The selector is the VM's: the context §2.1 names, the first 8 bytes.
        assert_eq!(SELECTOR_CONTEXT, "blockula program method v1");
        assert_eq!(selector("init"), blake3::derive_key("blockula program method v1", b"init")[..8]);
        assert_eq!(hex::encode(selector("init")), "be9269d978ccde0c");
    }
}
