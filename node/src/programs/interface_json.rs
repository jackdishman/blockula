//! Interface format 1 as JSON (docs/PROGRAMS-V0.md §3.4): the form a deployer writes (`blockula
//! program upload --interface iface.json`, `blockula program interface encode`, WBS S6b) and the
//! form `GET /api/interface/{hash}` serves beside the raw bytes (§8.2), so v0's clients need no
//! decoder of their own.
//!
//! ```json
//! { "format": 1, "methods": [
//!   { "name": "transfer", "kind": "call", "payable": false,
//!     "args": [{ "name": "to", "type": "address" }, { "name": "amount", "type": "fang" }], "returns": null },
//!   { "name": "balance_of", "kind": "view", "payable": false,
//!     "args": [{ "name": "who", "type": "address" }], "returns": "u128" } ] }
//! ```
//!
//! A scalar type is a string (`u8`, `u16`, `u32`, `u64`, `u128`, `i64`, `i128`, `bool`, `fang`,
//! `address`, `name`, `hash32`); `{ "bytes": max }`, `{ "text": max }`, `{ "option": type }` and
//! `{ "vec": [type, max] }` are the rest. A method's `kind` is `call`, `view` or `init`.
//!
//! **The bytes are the interface.** `blockula_vm::interface` is the only codec and the only place
//! §3.2's rules live: [`encode`] turns JSON into bytes through `interface::encode`, so JSON that
//! breaks a rule (methods out of order, a payable view, a fifth nesting layer, …) is refused with
//! the codec's own error, and [`InterfaceJson::from`] renders what `interface::decode` read. JSON
//! is tooling, never consensus: nothing the chain checks reads it. Every identifier in it is the
//! deployer's claim (PRG 9.7): the API serves it as such (`programs::api`).

use anyhow::{bail, Context, Result};
use blockula_vm::host::MethodKind;
use blockula_vm::interface::{self, Arg, Interface, Method, Type};
use serde::{Deserialize, Serialize};

/// An interface in §3.4's JSON form. Serialized in this field order (what the committed
/// `node/vm/tests/programs/*.iface.json` hold); parsed strictly: an unknown key is refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceJson {
    /// 1: the only format this release reads and writes.
    pub format: u8,
    pub methods: Vec<MethodJson>,
}

/// One method.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MethodJson {
    pub name: String,
    pub kind: KindJson,
    pub payable: bool,
    pub args: Vec<ArgJson>,
    /// `null` for a method that returns nothing.
    pub returns: Option<TypeJson>,
}

/// One argument.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgJson {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: TypeJson,
}

/// A method's kind: `call`, `view` or `init` (`host::MethodKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindJson {
    Call,
    View,
    Init,
}

/// A type, as §3.4 writes it: serde's externally tagged form gives exactly its shapes (a unit
/// variant is its name, `Bytes(32)` is `{ "bytes": 32 }`, `Vec(t, 8)` is `{ "vec": [t, 8] }`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeJson {
    U8,
    U16,
    U32,
    U64,
    U128,
    I64,
    I128,
    Bool,
    Fang,
    Address,
    Name,
    Hash32,
    Bytes(u32),
    Text(u16),
    Option(Box<TypeJson>),
    Vec(Box<TypeJson>, u16),
}

impl From<MethodKind> for KindJson {
    fn from(kind: MethodKind) -> KindJson {
        match kind {
            MethodKind::Call => KindJson::Call,
            MethodKind::View => KindJson::View,
            MethodKind::Init => KindJson::Init,
        }
    }
}

impl From<KindJson> for MethodKind {
    fn from(kind: KindJson) -> MethodKind {
        match kind {
            KindJson::Call => MethodKind::Call,
            KindJson::View => MethodKind::View,
            KindJson::Init => MethodKind::Init,
        }
    }
}

impl From<&Type> for TypeJson {
    fn from(ty: &Type) -> TypeJson {
        match ty {
            Type::U8 => TypeJson::U8,
            Type::U16 => TypeJson::U16,
            Type::U32 => TypeJson::U32,
            Type::U64 => TypeJson::U64,
            Type::U128 => TypeJson::U128,
            Type::I64 => TypeJson::I64,
            Type::I128 => TypeJson::I128,
            Type::Bool => TypeJson::Bool,
            Type::Fang => TypeJson::Fang,
            Type::Address => TypeJson::Address,
            Type::Name => TypeJson::Name,
            Type::Hash32 => TypeJson::Hash32,
            Type::Bytes { max } => TypeJson::Bytes(*max),
            Type::Text { max } => TypeJson::Text(*max),
            Type::Option(of) => TypeJson::Option(Box::new(TypeJson::from(&**of))),
            Type::Vec { of, max } => TypeJson::Vec(Box::new(TypeJson::from(&**of)), *max),
        }
    }
}

impl From<&TypeJson> for Type {
    fn from(ty: &TypeJson) -> Type {
        match ty {
            TypeJson::U8 => Type::U8,
            TypeJson::U16 => Type::U16,
            TypeJson::U32 => Type::U32,
            TypeJson::U64 => Type::U64,
            TypeJson::U128 => Type::U128,
            TypeJson::I64 => Type::I64,
            TypeJson::I128 => Type::I128,
            TypeJson::Bool => Type::Bool,
            TypeJson::Fang => Type::Fang,
            TypeJson::Address => Type::Address,
            TypeJson::Name => Type::Name,
            TypeJson::Hash32 => Type::Hash32,
            TypeJson::Bytes(max) => Type::Bytes { max: *max },
            TypeJson::Text(max) => Type::Text { max: *max },
            TypeJson::Option(of) => Type::Option(Box::new(Type::from(&**of))),
            TypeJson::Vec(of, max) => Type::Vec { of: Box::new(Type::from(&**of)), max: *max },
        }
    }
}

/// What `interface::decode` read, as JSON. Every decoded interface has a JSON form.
impl From<&Interface> for InterfaceJson {
    fn from(interface: &Interface) -> InterfaceJson {
        let method = |m: &Method| MethodJson {
            name: m.name.clone(),
            kind: m.kind.into(),
            payable: m.payable,
            args: m.args.iter().map(|a| ArgJson { name: a.name.clone(), ty: TypeJson::from(&a.ty) }).collect(),
            returns: m.returns.as_ref().map(TypeJson::from),
        };
        InterfaceJson { format: interface::FORMAT, methods: interface.methods.iter().map(method).collect() }
    }
}

/// The interface `json` describes, if its bytes keep every rule of §3.2 (the codec checks them:
/// `interface::encode` returns only bytes that decode back to what it was given).
// The CLI's `upload` and `interface encode` (WBS S6b) are the first callers outside tests.
#[cfg_attr(not(test), allow(dead_code))]
pub fn to_interface(json: &InterfaceJson) -> Result<Interface> {
    if json.format != interface::FORMAT {
        bail!("interface format {}: this release writes format {}", json.format, interface::FORMAT);
    }
    let method = |m: &MethodJson| Method {
        name: m.name.clone(),
        kind: m.kind.into(),
        payable: m.payable,
        args: m.args.iter().map(|a| Arg { name: a.name.clone(), ty: Type::from(&a.ty) }).collect(),
        returns: m.returns.as_ref().map(Type::from),
    };
    let interface = Interface { methods: json.methods.iter().map(method).collect() };
    interface::encode(&interface)?;
    Ok(interface)
}

/// The format-1 bytes of the interface §3.4's JSON `text` describes: what an `UploadCode` carries
/// and `interface_hash` names.
// As `to_interface`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn encode(text: &str) -> Result<Vec<u8>> {
    let json: InterfaceJson = serde_json::from_str(text).context("an interface in docs/PROGRAMS-V0.md §3.4's JSON form")?;
    Ok(interface::encode(&to_interface(&json)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The four guests' interfaces as committed by stage V (`node/vm/tests/interfaces.rs` writes
    /// both files from the same Rust value): the JSON a deployer writes, and its bytes.
    const GUESTS: [(&str, &str, &[u8]); 4] = [
        ("counter", include_str!("../../vm/tests/programs/counter.iface.json"), include_bytes!("../../vm/tests/programs/counter.iface")),
        ("token", include_str!("../../vm/tests/programs/token.iface.json"), include_bytes!("../../vm/tests/programs/token.iface")),
        ("escrow", include_str!("../../vm/tests/programs/escrow.iface.json"), include_bytes!("../../vm/tests/programs/escrow.iface")),
        ("router", include_str!("../../vm/tests/programs/router.iface.json"), include_bytes!("../../vm/tests/programs/router.iface")),
    ];

    /// Each guest's `.iface.json` encodes to exactly its committed `.iface` bytes, and those bytes
    /// render back to the same JSON, text for text (serde_json's pretty layout, which stage V
    /// wrote them in): the JSON form and the codec describe the same interfaces.
    #[test]
    fn the_guests_json_encodes_to_their_committed_bytes() {
        for (name, text, bytes) in GUESTS {
            assert_eq!(encode(text).unwrap_or_else(|e| panic!("{name}: {e:#}")), bytes, "{name}.iface.json encodes to {name}.iface");
            let rendered = InterfaceJson::from(&interface::decode(bytes).unwrap());
            assert_eq!(serde_json::to_string_pretty(&rendered).unwrap() + "\n", text, "{name}.iface renders to {name}.iface.json");
        }
    }

    /// §3.4's example parses (its interface keeps every rule), and every type's shape round-trips:
    /// the scalars as strings, the four compound types as their one-key objects.
    #[test]
    fn every_type_has_its_section_3_4_shape() {
        let example = json!({ "format": 1, "methods": [
            { "name": "balance_of", "kind": "view", "payable": false, "args": [{ "name": "who", "type": "address" }], "returns": "u128" },
            { "name": "init", "kind": "init", "payable": true, "args": [{ "name": "memo", "type": { "text": 64 } }, { "name": "owners", "type": { "vec": ["address", 8] } }], "returns": null },
            { "name": "transfer", "kind": "call", "payable": false, "args": [{ "name": "to", "type": "address" }, { "name": "amount", "type": "fang" }], "returns": null } ] });
        let bytes = encode(&example.to_string()).unwrap();
        assert_eq!(serde_json::to_value(InterfaceJson::from(&interface::decode(&bytes).unwrap())).unwrap(), example);

        let scalars = [("u8", Type::U8), ("u16", Type::U16), ("u32", Type::U32), ("u64", Type::U64), ("u128", Type::U128), ("i64", Type::I64), ("i128", Type::I128), ("bool", Type::Bool), ("fang", Type::Fang), ("address", Type::Address), ("name", Type::Name), ("hash32", Type::Hash32)];
        let mut cases: Vec<(serde_json::Value, Type)> = scalars.into_iter().map(|(s, t)| (json!(s), t)).collect();
        cases.extend([
            (json!({ "bytes": 32768 }), Type::Bytes { max: 32_768 }),
            (json!({ "text": 1 }), Type::Text { max: 1 }),
            (json!({ "option": "u8" }), Type::Option(Box::new(Type::U8))),
            (json!({ "vec": [{ "option": { "bytes": 7 } }, 1024] }), Type::Vec { of: Box::new(Type::Option(Box::new(Type::Bytes { max: 7 }))), max: 1_024 }),
        ]);
        for (value, ty) in cases {
            let parsed: TypeJson = serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("{value}: {e}"));
            assert_eq!(Type::from(&parsed), ty, "{value}");
            assert_eq!(serde_json::to_value(TypeJson::from(&ty)).unwrap(), value, "{ty:?}");
        }
    }

    /// JSON the codec would refuse is refused, in the codec's words: the interface rules live in
    /// `blockula_vm::interface` alone. So are unknown keys, another format, and numbers past
    /// their field's width.
    #[test]
    fn json_breaking_a_rule_is_refused() {
        let with = |methods: serde_json::Value| encode(&json!({ "format": 1, "methods": methods }).to_string()).map_err(|e| format!("{e:#}"));
        let get = |kind: &str, payable: bool| json!({ "name": "get", "kind": kind, "payable": payable, "args": [], "returns": "u64" });
        with(json!([get("view", false)])).unwrap();
        // The codec's rules, quoted from its errors.
        assert!(with(json!([get("view", true)])).unwrap_err().contains("view method get is payable"));
        assert!(with(json!([])).unwrap_err().contains("0 methods: 1 to 64"));
        let set = json!({ "name": "set", "kind": "call", "payable": false, "args": [], "returns": null });
        assert!(with(json!([set, get("view", false)])).unwrap_err().contains("methods are strictly ascending by name"));
        assert!(with(json!([{ "name": "Get", "kind": "call", "payable": false, "args": [], "returns": null }])).unwrap_err().contains("isn't an identifier"));
        let deep = |layers: usize| (0..layers).fold(json!("u8"), |t, _| json!({ "option": t }));
        let nested = |layers: usize| with(json!([{ "name": "get", "kind": "view", "payable": false, "args": [], "returns": deep(layers) }]));
        nested(4).unwrap();
        assert!(nested(5).unwrap_err().contains("types nest at most 4 option or vec layers deep"));
        assert!(with(json!([{ "name": "get", "kind": "view", "payable": false, "args": [{ "name": "b", "type": { "bytes": 0 } }], "returns": null }])).unwrap_err().contains("the max is 1 to 32768"));
        // The JSON form's own: another format, unknown keys, unknown kinds and types, numbers past
        // their width, a compound type with a second key.
        assert!(encode(&json!({ "format": 2, "methods": [get("view", false)] }).to_string()).unwrap_err().to_string().contains("interface format 2: this release writes format 1"));
        assert!(encode(&json!({ "format": 1, "methods": [get("view", false)], "docs": "x" }).to_string()).is_err());
        let mut extra = get("view", false);
        extra["selector"] = json!("00");
        assert!(with(json!([extra])).is_err());
        assert!(with(json!([get("pure", false)])).is_err());
        for ty in [json!("f64"), json!({ "bytes": 4_294_967_296u64 }), json!({ "text": 65_536 }), json!({ "vec": ["u8"] }), json!({ "bytes": 1, "text": 1 })] {
            let method = json!({ "name": "get", "kind": "view", "payable": false, "args": [], "returns": ty });
            assert!(with(json!([method])).is_err(), "{ty}");
        }
        assert!(encode("not json").is_err());
    }
}
