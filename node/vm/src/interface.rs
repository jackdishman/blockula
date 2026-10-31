//! Interface format 1 (docs/PROGRAMS-V0.md §3, which amends docs/PROGRAMS.md 11.1 by its A16): a
//! program's methods, uploaded beside its code (`UploadCode`), stored once under its
//! `interface_hash` and bound into every call through `version_digest`. Format 1 has methods only:
//! each one's name, kind and whether it takes value (all the chain enforces, through
//! `host::View::method`), and its argument and return types (for tooling: the chain never checks
//! an input against them, a program decodes its own). Names, docs, events, errors, named types and
//! storage layouts come with format 2, beside this one; format 1 stays valid forever.
//!
//! The codec is written by hand, so the bytes are the spec and a port to another language is a
//! page of code:
//!
//! ```text
//! interface = u8 format (= 1) ‖ u8 n (1..=64) ‖ n × method       methods strictly ascending by name, bytewise
//! method    = ident name ‖ u8 kind ‖ u8 flags ‖ u8 a (0..=16) ‖ a × arg ‖ u8 has_return (0 | 1) ‖ [type return]
//! arg       = ident name ‖ type
//! ident     = u8 len (1..=32) ‖ len bytes matching [a-z][a-z0-9_]*
//! kind      = 0 Call | 1 View | 2 Init
//! flags     = bit 0: payable; every other bit 0
//! type      = u8 tag ‖ payload
//!             1 u8 | 2 u16 | 3 u32 | 4 u64 | 5 u128 | 6 i64 | 7 i128 | 8 bool
//!             | 9 fang (u64 drops) | 10 address (32 bytes) | 11 name (a .boo label) | 12 hash32
//!             | 13 bytes ‖ u32 max (1..=32,768) | 14 text ‖ u16 max (1..=32,768)
//!             | 15 option ‖ type | 16 vec ‖ type ‖ u16 max (1..=1,024)
//! ```
//!
//! Integers are little-endian. [`decode`] checks every rule of §3.2 and refuses anything else:
//! - the format is 1; at most [`MAX_INTERFACE_BYTES`] bytes; nothing after the last method;
//! - 1 to [`MAX_METHODS`] methods, names strictly ascending (so no duplicates, and one encoding);
//! - at most one `Init` method; a `View` method isn't payable; an `Init` method returns nothing;
//! - at most [`MAX_ARGS`] arguments to a method, their names distinct;
//! - at most [`MAX_NESTING`] `option`/`vec` layers in a type; every `max` in its range; no unknown
//!   kind, flag, type tag or `has_return`;
//! - every identifier an [`abi::is_ident`];
//! - the methods' [`selector`]s distinct. 64 names can't collide by chance; the check is what lets
//!   a later version key agent grants on selectors (docs/PROGRAMS.md 9.1) without a new rule.
//!
//! So there's exactly one encoding of each interface: `encode(&decode(b)?)? == b` for every `b`
//! that decodes. It's stateless, so the node checks it in `check_format`: a transaction carrying an
//! interface that doesn't decode never reaches admission. At upload, and at create when the code
//! and the interface came from different uploads, [`check_exports`] matches the methods with the
//! code's `bk_` exports one to one (§3.3).
//!
//! Engine-neutral and free of node types, so the `interface_codec` fuzzer fuzzes it alone. The
//! node keeps the JSON form (`node/src/programs/interface_json.rs`).

use crate::abi;
use crate::config::{MAX_INTERFACE_BYTES, MAX_METHODS};
use crate::host::{MethodInfo, MethodKind};
use std::collections::BTreeSet;
use std::fmt;

/// The only format this release reads and writes.
pub const FORMAT: u8 = 1;
/// Arguments to one method.
pub const MAX_ARGS: usize = 16;
/// `option` and `vec` layers around a type: `option<vec<option<vec<u8>>>>` is as deep as it goes.
pub const MAX_NESTING: usize = 4;
/// The largest `max` of a `bytes` type, and of a `text` type (32 KiB, `config::MAX_INPUT`'s
/// size). A `bytes` max is a u32 so a later raise of the input limit needs no new format.
pub const MAX_BYTES_MAX: u32 = 32_768;
pub const MAX_TEXT_MAX: u16 = 32_768;
/// The largest `max` of a `vec` type.
pub const MAX_VEC_MAX: u16 = 1_024;

/// A method's selector: the first 8 bytes of a key derived from its name ([`selector`]).
pub type Selector = [u8; 8];
/// The BLAKE3 `derive_key` context of [`selector`] (docs/PROGRAMS-V0.md §2.1's fifth digest).
pub const SELECTOR_CONTEXT: &str = "blockula program method v1";

/// A method's selector: `derive_key("blockula program method v1", name)[0..8]`.
pub fn selector(name: &str) -> Selector {
    let key = blake3::derive_key(SELECTOR_CONTEXT, name.as_bytes());
    let mut s = [0; 8];
    s.copy_from_slice(&key[..8]);
    s
}

/// A decoded interface: its methods, ascending by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    pub methods: Vec<Method>,
}

/// One method. `kind` and `payable` are what execution enforces (`host::MethodInfo`); the rest is
/// the deployer's description, for tooling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Method {
    pub name: String,
    pub kind: MethodKind,
    pub payable: bool,
    pub args: Vec<Arg>,
    pub returns: Option<Type>,
}

/// One argument: a name and a type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arg {
    pub name: String,
    pub ty: Type,
}

/// An argument or return type. Values are Borsh-encoded in a call's input by convention
/// (docs/PROGRAMS.md 11.3), which the chain never checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    U8,
    U16,
    U32,
    U64,
    U128,
    I64,
    I128,
    Bool,
    /// An amount of FANG, in drops (a u64).
    Fang,
    /// An account's 32 address bytes.
    Address,
    /// A `.boo` name's label.
    Name,
    Hash32,
    /// At most `max` bytes, 1..=[`MAX_BYTES_MAX`].
    Bytes { max: u32 },
    /// At most `max` bytes of UTF-8, 1..=[`MAX_TEXT_MAX`].
    Text { max: u16 },
    Option(Box<Type>),
    /// At most `max` items, 1..=[`MAX_VEC_MAX`].
    Vec { of: Box<Type>, max: u16 },
}

impl Type {
    /// The type's tag byte.
    fn tag(&self) -> u8 {
        match self {
            Type::U8 => 1,
            Type::U16 => 2,
            Type::U32 => 3,
            Type::U64 => 4,
            Type::U128 => 5,
            Type::I64 => 6,
            Type::I128 => 7,
            Type::Bool => 8,
            Type::Fang => 9,
            Type::Address => 10,
            Type::Name => 11,
            Type::Hash32 => 12,
            Type::Bytes { .. } => 13,
            Type::Text { .. } => 14,
            Type::Option(_) => 15,
            Type::Vec { .. } => 16,
        }
    }
}

/// A method kind's byte.
fn kind_byte(kind: MethodKind) -> u8 {
    match kind {
        MethodKind::Call => 0,
        MethodKind::View => 1,
        MethodKind::Init => 2,
    }
}

/// The `flags` bit that marks a method payable; every other bit is 0.
const PAYABLE: u8 = 1;

impl Interface {
    /// What execution enforces of method `name` (`host::View::method`), or `None` if the
    /// interface has none by that name.
    pub fn method(&self, name: &str) -> Option<MethodInfo> {
        let m = self.methods.iter().find(|m| m.name == name)?;
        Some(MethodInfo { kind: m.kind, payable: m.payable })
    }

    /// The `Init` method, if the interface declares one: `CreateProgram` carries `init` exactly
    /// when it does, and names it (docs/PROGRAMS-V0.md §2.3).
    pub fn init(&self) -> Option<&Method> {
        self.methods.iter().find(|m| m.kind == MethodKind::Init)
    }
}

/// Which rule of docs/PROGRAMS-V0.md §3.2 an interface broke. Never in a receipt: an interface
/// that doesn't decode is refused before admission (`check_format`), so these aren't numbered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// More than [`MAX_INTERFACE_BYTES`].
    Size,
    /// It ends inside a field.
    Truncated,
    /// Bytes after the last method.
    Trailing,
    /// A format other than 1.
    Format,
    /// No methods, or more than [`MAX_METHODS`].
    MethodCount,
    /// A method or argument name that isn't 1 to 32 bytes matching `[a-z][a-z0-9_]*`.
    Ident,
    /// A method name not strictly above the one before it (bytewise): out of order or a duplicate.
    Order,
    /// A kind other than 0 (`Call`), 1 (`View`) or 2 (`Init`).
    Kind,
    /// A flag bit other than bit 0 (payable).
    Flags,
    /// More than [`MAX_ARGS`] arguments.
    ArgCount,
    /// Two arguments of one method with the same name.
    ArgName,
    /// A `has_return` other than 0 or 1.
    HasReturn,
    /// A type tag other than 1 to 16.
    TypeTag,
    /// A `bytes`, `text` or `vec` max out of its range.
    Max,
    /// More than [`MAX_NESTING`] `option`/`vec` layers.
    Nesting,
    /// A second `Init` method.
    TwoInits,
    /// A payable `View` method.
    PayableView,
    /// An `Init` method with a return type.
    InitReturns,
    /// Two methods with the same selector.
    Selector,
}

/// Why an interface was refused: the rule, where, and a message for the uploader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub rule: Rule,
    /// Byte offset of the field that broke it.
    pub offset: usize,
    /// What's wrong. It quotes only names that are valid identifiers, so never arbitrary bytes.
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "interface: {} (at byte {})", self.message, self.offset)
    }
}

impl std::error::Error for Error {}

fn err(rule: Rule, offset: usize, message: impl Into<String>) -> Error {
    Error { rule, offset, message: message.into() }
}

/// Reads the bytes in order, refusing to read past their end.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(n).filter(|end| *end <= self.bytes.len());
        let end = end.ok_or_else(|| err(Rule::Truncated, self.at, "it ends inside a field"))?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// An identifier, `what` naming it in the message.
    fn ident(&mut self, what: &str) -> Result<String, Error> {
        let at = self.at;
        let len = usize::from(self.u8()?);
        // Checked before the bytes are read, so a bad length is refused where it stands rather
        // than as a truncation; `abi::is_ident` checks the length again below.
        if len == 0 || len > 32 {
            return Err(err(Rule::Ident, at, format!("{what} of {len} bytes: identifiers have 1 to 32")));
        }
        let bytes = self.take(len)?;
        match std::str::from_utf8(bytes) {
            Ok(s) if abi::is_ident(s) => Ok(s.to_string()),
            _ => Err(err(Rule::Ident, at, format!("{what} isn't an identifier ([a-z][a-z0-9_]*)"))),
        }
    }

    /// A type inside `layers` `option`/`vec` layers.
    fn ty(&mut self, layers: usize) -> Result<Type, Error> {
        let at = self.at;
        let layer = |layers: usize| {
            if layers == MAX_NESTING { Err(err(Rule::Nesting, at, format!("types nest at most {MAX_NESTING} option or vec layers deep"))) } else { Ok(layers + 1) }
        };
        Ok(match self.u8()? {
            1 => Type::U8,
            2 => Type::U16,
            3 => Type::U32,
            4 => Type::U64,
            5 => Type::U128,
            6 => Type::I64,
            7 => Type::I128,
            8 => Type::Bool,
            9 => Type::Fang,
            10 => Type::Address,
            11 => Type::Name,
            12 => Type::Hash32,
            13 => {
                let max = self.u32()?;
                if !(1..=MAX_BYTES_MAX).contains(&max) {
                    return Err(err(Rule::Max, at, format!("bytes of at most {max}: the max is 1 to {MAX_BYTES_MAX}")));
                }
                Type::Bytes { max }
            }
            14 => {
                let max = self.u16()?;
                if !(1..=MAX_TEXT_MAX).contains(&max) {
                    return Err(err(Rule::Max, at, format!("text of at most {max}: the max is 1 to {MAX_TEXT_MAX}")));
                }
                Type::Text { max }
            }
            15 => Type::Option(Box::new(self.ty(layer(layers)?)?)),
            16 => {
                let of = self.ty(layer(layers)?)?;
                let max = self.u16()?;
                if !(1..=MAX_VEC_MAX).contains(&max) {
                    return Err(err(Rule::Max, at, format!("a vec of at most {max}: the max is 1 to {MAX_VEC_MAX}")));
                }
                Type::Vec { of: Box::new(of), max }
            }
            tag => return Err(err(Rule::TypeTag, at, format!("type tag {tag}: format 1 has 1 to 16"))),
        })
    }

    /// One method, named after `previous` (the method before it, if any).
    fn method(&mut self, previous: Option<&str>) -> Result<Method, Error> {
        let at = self.at;
        let name = self.ident("a method name")?;
        if previous.is_some_and(|p| p >= name.as_str()) {
            return Err(err(Rule::Order, at, format!("method {name} isn't after the one before it: methods are strictly ascending by name")));
        }
        let kind_at = self.at;
        let kind = match self.u8()? {
            0 => MethodKind::Call,
            1 => MethodKind::View,
            2 => MethodKind::Init,
            k => return Err(err(Rule::Kind, kind_at, format!("method {name} has kind {k}: 0 call, 1 view, 2 init"))),
        };
        let flags_at = self.at;
        let flags = self.u8()?;
        if flags & !PAYABLE != 0 {
            return Err(err(Rule::Flags, flags_at, format!("method {name} has flags {flags:#04x}: only bit 0 (payable) is defined")));
        }
        let payable = flags & PAYABLE != 0;
        if payable && kind == MethodKind::View {
            return Err(err(Rule::PayableView, flags_at, format!("view method {name} is payable: a view takes no value")));
        }
        let count_at = self.at;
        let count = usize::from(self.u8()?);
        if count > MAX_ARGS {
            return Err(err(Rule::ArgCount, count_at, format!("method {name} has {count} arguments: at most {MAX_ARGS}")));
        }
        let mut args = Vec::with_capacity(count);
        let mut names = BTreeSet::new();
        for _ in 0..count {
            let arg_at = self.at;
            let arg = self.ident("an argument name")?;
            if !names.insert(arg.clone()) {
                return Err(err(Rule::ArgName, arg_at, format!("method {name} has two arguments named {arg}")));
            }
            args.push(Arg { name: arg, ty: self.ty(0)? });
        }
        let has_return_at = self.at;
        let returns = match self.u8()? {
            0 => None,
            1 => Some(self.ty(0)?),
            b => return Err(err(Rule::HasReturn, has_return_at, format!("method {name} has has_return {b}: 0 or 1"))),
        };
        if returns.is_some() && kind == MethodKind::Init {
            return Err(err(Rule::InitReturns, has_return_at, format!("init method {name} has a return type: an initializer returns nothing")));
        }
        Ok(Method { name, kind, payable, args, returns })
    }
}

/// Decodes and checks an interface (format 1, every rule of docs/PROGRAMS-V0.md §3.2). Never
/// panics, and reads nothing past `bytes`.
pub fn decode(bytes: &[u8]) -> Result<Interface, Error> {
    decode_with(bytes, selector)
}

/// [`decode`], with the selector function `select`: the tests give it one that collides.
fn decode_with(bytes: &[u8], select: impl Fn(&str) -> Selector) -> Result<Interface, Error> {
    if bytes.len() > MAX_INTERFACE_BYTES {
        return Err(err(Rule::Size, MAX_INTERFACE_BYTES, format!("{} bytes: at most {MAX_INTERFACE_BYTES}", bytes.len())));
    }
    let mut r = Reader { bytes, at: 0 };
    let format = r.u8()?;
    if format != FORMAT {
        return Err(err(Rule::Format, 0, format!("format {format}: this release reads format {FORMAT}")));
    }
    let count = usize::from(r.u8()?);
    if count == 0 || count > MAX_METHODS {
        return Err(err(Rule::MethodCount, 1, format!("{count} methods: 1 to {MAX_METHODS}")));
    }
    let mut methods: Vec<Method> = Vec::with_capacity(count);
    // Where each method starts, for the errors found once they're all read.
    let mut starts = Vec::with_capacity(count);
    let mut init = false;
    for _ in 0..count {
        let at = r.at;
        starts.push(at);
        let method = r.method(methods.last().map(|m| m.name.as_str()))?;
        if method.kind == MethodKind::Init {
            if init {
                return Err(err(Rule::TwoInits, at, format!("method {} is a second init method: at most one", method.name)));
            }
            init = true;
        }
        methods.push(method);
    }
    if r.at != bytes.len() {
        return Err(err(Rule::Trailing, r.at, format!("{} bytes after the last method", bytes.len() - r.at)));
    }
    let mut selectors = BTreeSet::new();
    for (m, at) in methods.iter().zip(starts) {
        if !selectors.insert(select(&m.name)) {
            return Err(err(Rule::Selector, at, format!("method {}'s selector is another method's", m.name)));
        }
    }
    Ok(Interface { methods })
}

/// Writes the bytes.
struct Writer {
    out: Vec<u8>,
}

impl Writer {
    /// A count or length as the u8 the format writes, or the rule that bounds it.
    fn u8_of(&mut self, n: usize, rule: Rule, what: &str) -> Result<(), Error> {
        let n = u8::try_from(n).map_err(|_| err(rule, self.out.len(), format!("{what} of {n}: past a u8")))?;
        self.out.push(n);
        Ok(())
    }

    fn ident(&mut self, name: &str) -> Result<(), Error> {
        self.u8_of(name.len(), Rule::Ident, "a name")?;
        self.out.extend_from_slice(name.as_bytes());
        Ok(())
    }

    fn ty(&mut self, ty: &Type) -> Result<(), Error> {
        self.out.push(ty.tag());
        match ty {
            Type::Bytes { max } => self.out.extend_from_slice(&max.to_le_bytes()),
            Type::Text { max } => self.out.extend_from_slice(&max.to_le_bytes()),
            Type::Option(of) => self.ty(of)?,
            Type::Vec { of, max } => {
                self.ty(of)?;
                self.out.extend_from_slice(&max.to_le_bytes());
            }
            _ => {}
        }
        Ok(())
    }
}

/// Encodes `interface` in format 1, and checks the result as [`decode`] does: it returns only
/// bytes that decode back to `interface`, so a hand-built interface that breaks a rule (methods
/// out of order, a payable view, …) is refused with the rule's [`Error`], as decoding would refuse
/// its bytes.
pub fn encode(interface: &Interface) -> Result<Vec<u8>, Error> {
    let mut w = Writer { out: vec![FORMAT] };
    w.u8_of(interface.methods.len(), Rule::MethodCount, "a method count")?;
    for m in &interface.methods {
        w.ident(&m.name)?;
        w.out.push(kind_byte(m.kind));
        w.out.push(if m.payable { PAYABLE } else { 0 });
        w.u8_of(m.args.len(), Rule::ArgCount, "an argument count")?;
        for a in &m.args {
            w.ident(&a.name)?;
            w.ty(&a.ty)?;
        }
        match &m.returns {
            None => w.out.push(0),
            Some(ty) => {
                w.out.push(1);
                w.ty(ty)?;
            }
        }
    }
    // Every field above is written as decoding reads it, and every count and length was checked
    // to fit its byte, so the bytes decode to `interface` exactly when it keeps every rule.
    decode(&w.out)?;
    Ok(w.out)
}

/// Why an interface doesn't match its code (docs/PROGRAMS-V0.md §3.3): the first difference, by
/// name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// A method the code doesn't export as `bk_<name>`.
    NoExport(String),
    /// A `bk_<name>` export the interface has no method for.
    NoMethod(String),
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mismatch::NoExport(m) => write!(f, "the interface's method {m} has no bk_{m} export in the code"),
            Mismatch::NoMethod(m) => write!(f, "the code's export bk_{m} has no method in the interface"),
        }
    }
}

impl std::error::Error for Mismatch {}

/// Whether `interface`'s methods are exactly the code's methods, `exports` (its `bk_` function
/// exports with the prefix removed: `validate::ModuleInfo::exports`, or a stored code's record),
/// as sets. Validation already refuses any other export, so every method then has an export and
/// every export a method, and a call to `bk_<method>` never meets a missing export.
pub fn check_exports(interface: &Interface, exports: &[String]) -> Result<(), Mismatch> {
    let methods: BTreeSet<&str> = interface.methods.iter().map(|m| m.name.as_str()).collect();
    let exports: BTreeSet<&str> = exports.iter().map(String::as_str).collect();
    if let Some(m) = methods.difference(&exports).next() {
        return Err(Mismatch::NoExport(m.to_string()));
    }
    if let Some(e) = exports.difference(&methods).next() {
        return Err(Mismatch::NoMethod(e.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method(name: &str, kind: MethodKind, payable: bool, args: &[(&str, Type)], returns: Option<Type>) -> Method {
        let args = args.iter().map(|(n, ty)| Arg { name: n.to_string(), ty: ty.clone() }).collect();
        Method { name: name.into(), kind, payable, args, returns }
    }

    /// docs/PROGRAMS-V0.md §3.4's example, in order.
    fn example() -> Interface {
        Interface {
            methods: vec![
                method("balance_of", MethodKind::View, false, &[("who", Type::Address)], Some(Type::U128)),
                method(
                    "init",
                    MethodKind::Init,
                    true,
                    &[("memo", Type::Text { max: 64 }), ("owners", Type::Vec { of: Box::new(Type::Address), max: 8 })],
                    None,
                ),
                method("transfer", MethodKind::Call, false, &[("to", Type::Address), ("amount", Type::Fang)], None),
            ],
        }
    }

    /// The example's bytes, field by field: the format is these bytes.
    const EXAMPLE: &str = concat!(
        "01", "03",                                       // format 1, 3 methods
        "0a", "62616c616e63655f6f66", "01", "00", "01",   // "balance_of", view, not payable, 1 argument
        "03", "77686f", "0a",                             //   who: address
        "01", "05",                                       //   returns u128
        "04", "696e6974", "02", "01", "02",               // "init", init, payable, 2 arguments
        "04", "6d656d6f", "0e", "4000",                   //   memo: text of at most 64
        "06", "6f776e657273", "10", "0a", "0800",         //   owners: vec of addresses, at most 8
        "00",                                             //   returns nothing
        "08", "7472616e73666572", "00", "00", "02",       // "transfer", call, not payable, 2 arguments
        "02", "746f", "0a",                               //   to: address
        "06", "616d6f756e74", "09",                       //   amount: fang
        "00",                                             //   returns nothing
    );

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn refused(bytes: &[u8]) -> Rule {
        match decode(bytes) {
            Err(e) => e.rule,
            Ok(i) => panic!("accepted {i:?}"),
        }
    }

    /// One method `m`, a call with the one argument `x` of type `ty` (its bytes) returning
    /// nothing, as bytes.
    fn one_arg(ty: &[u8]) -> Vec<u8> {
        [&hex("0101016d0000010178")[..], ty, &[0]].concat()
    }

    #[test]
    fn the_example_has_these_bytes_and_round_trips() {
        let bytes = hex(EXAMPLE);
        assert_eq!(encode(&example()).unwrap(), bytes);
        assert_eq!(decode(&bytes).unwrap(), example());
        assert_eq!(one_arg(&[1]), hex("0101016d00000101780100"), "the helper writes `m(x: u8)`");
        assert_eq!(decode(&one_arg(&[1])).unwrap().methods[0].args[0], Arg { name: "x".into(), ty: Type::U8 });
    }

    /// Every type, at the bounds of its `max`, round-trips; so does every kind and flag
    /// combination a method may have.
    #[test]
    fn every_type_and_method_shape_round_trips() {
        let nest = |mut ty: Type, layers: usize| {
            for i in 0..layers {
                ty = if i % 2 == 0 { Type::Option(Box::new(ty)) } else { Type::Vec { of: Box::new(ty), max: MAX_VEC_MAX } };
            }
            ty
        };
        let types = vec![
            Type::U8,
            Type::U16,
            Type::U32,
            Type::U64,
            Type::U128,
            Type::I64,
            Type::I128,
            Type::Bool,
            Type::Fang,
            Type::Address,
            Type::Name,
            Type::Hash32,
            Type::Bytes { max: 1 },
            Type::Bytes { max: MAX_BYTES_MAX },
            Type::Text { max: 1 },
            Type::Text { max: MAX_TEXT_MAX },
            Type::Option(Box::new(Type::U8)),
            Type::Vec { of: Box::new(Type::U8), max: 1 },
            nest(Type::Name, MAX_NESTING),
        ];
        assert_eq!(types.iter().map(Type::tag).collect::<BTreeSet<_>>(), (1..=16).collect(), "every tag");
        let args: Vec<(String, Type)> = types.iter().enumerate().map(|(i, t)| (format!("a{i}"), t.clone())).collect();
        let (first, rest) = args.split_at(MAX_ARGS);
        let args = |a: &[(String, Type)]| a.iter().map(|(n, t)| Arg { name: n.clone(), ty: t.clone() }).collect::<Vec<_>>();
        let i = Interface {
            methods: vec![
                Method { name: "a".into(), kind: MethodKind::Call, payable: false, args: args(first), returns: Some(nest(Type::Bytes { max: 9 }, MAX_NESTING)) },
                Method { name: "b".into(), kind: MethodKind::Call, payable: true, args: args(rest), returns: None },
                Method { name: "c".into(), kind: MethodKind::View, payable: false, args: vec![], returns: Some(Type::Bool) },
                Method { name: "d".into(), kind: MethodKind::Init, payable: true, args: vec![], returns: None },
            ],
        };
        let bytes = encode(&i).unwrap();
        assert_eq!(decode(&bytes).unwrap(), i);
        assert_eq!(encode(&decode(&bytes).unwrap()).unwrap(), bytes);
    }

    /// Canonical: of every one-byte change to the example's bytes, those that decode encode back
    /// to exactly the changed bytes, so no interface has two encodings.
    #[test]
    fn every_decoded_interface_has_one_encoding() {
        let bytes = hex(EXAMPLE);
        let mut decoded = 0;
        for i in 0..bytes.len() {
            for v in 0..=255u8 {
                let mut b = bytes.clone();
                b[i] = v;
                if let Ok(iface) = decode(&b) {
                    assert_eq!(encode(&iface).unwrap(), b, "byte {i} = {v}");
                    decoded += 1;
                }
            }
        }
        assert!(decoded > bytes.len(), "{decoded} variants decoded");
    }

    #[test]
    fn method_reports_kind_and_payable() {
        let i = example();
        assert_eq!(i.method("balance_of"), Some(MethodInfo { kind: MethodKind::View, payable: false }));
        assert_eq!(i.method("init"), Some(MethodInfo { kind: MethodKind::Init, payable: true }));
        assert_eq!(i.method("transfer"), Some(MethodInfo { kind: MethodKind::Call, payable: false }));
        for absent in ["", "a", "balance", "transfer_", "zzz", "Transfer"] {
            assert_eq!(i.method(absent), None, "{absent}");
        }
        assert_eq!(i.init().map(|m| m.name.as_str()), Some("init"));
        assert_eq!(Interface { methods: vec![example().methods[2].clone()] }.init(), None);
    }

    /// Selectors are the first 8 bytes of the derived key, pinned here (the SDK mirrors them), and
    /// distinct across a full interface's names.
    #[test]
    fn selectors_are_pinned_and_distinct() {
        assert_eq!(selector("transfer").to_vec(), blake3::derive_key("blockula program method v1", b"transfer")[..8].to_vec());
        assert_ne!(selector("transfer"), selector("transfer_"));
        let names: Vec<String> = (0..MAX_METHODS).map(|i| format!("m{i:02}")).collect();
        let selectors: BTreeSet<Selector> = names.iter().map(|n| selector(n)).collect();
        assert_eq!(selectors.len(), MAX_METHODS);
    }

    // --- every rule of §3.2, one by one ---

    #[test]
    fn refuses_more_than_16_kib_and_accepts_exactly_16_kib() {
        // 64 methods of 16 `bytes` arguments, the last argument's name grown to land on the cap.
        let mut i = Interface { methods: vec![] };
        for m in 0..MAX_METHODS {
            let args = (0..MAX_ARGS).map(|a| Arg { name: format!("a{a:02}"), ty: Type::Bytes { max: 1 } }).collect();
            i.methods.push(Method { name: format!("m{m:02}"), kind: MethodKind::Call, payable: false, args, returns: Some(Type::Bytes { max: 1 }) });
        }
        // Each character an argument's name gains is one byte more, up to 32 a name.
        let mut len = encode(&i).unwrap().len();
        for arg in i.methods.iter_mut().flat_map(|m| m.args.iter_mut()) {
            while arg.name.len() < 32 && len < MAX_INTERFACE_BYTES {
                arg.name.push('x');
                len += 1;
            }
        }
        let bytes = encode(&i).unwrap();
        assert_eq!(bytes.len(), MAX_INTERFACE_BYTES);
        assert_eq!(decode(&bytes).unwrap(), i);
        let mut over = bytes;
        over.push(0);
        assert_eq!(refused(&over), Rule::Size);
        let last = i.methods.last_mut().unwrap();
        last.args[0].name.push('y');
        assert_eq!(encode(&i).unwrap_err().rule, Rule::Size);
    }

    #[test]
    fn refuses_truncated_and_trailing_bytes() {
        let bytes = hex(EXAMPLE);
        for n in 0..bytes.len() {
            assert_eq!(refused(&bytes[..n]), Rule::Truncated, "the first {n} bytes");
        }
        for extra in [&[0u8][..], &[1], &[0; 100]] {
            assert_eq!(refused(&[&bytes[..], extra].concat()), Rule::Trailing);
        }
    }

    #[test]
    fn refuses_another_format() {
        let mut bytes = hex(EXAMPLE);
        for f in [0, 2, 255] {
            bytes[0] = f;
            assert_eq!(refused(&bytes), Rule::Format, "{f}");
        }
    }

    #[test]
    fn refuses_no_methods_and_more_than_64() {
        assert_eq!(refused(&[1, 0]), Rule::MethodCount);
        assert_eq!(refused(&[1, 65]), Rule::MethodCount);
        assert_eq!(refused(&[1, 255]), Rule::MethodCount);
        let methods = (0..=MAX_METHODS).map(|m| method(&format!("m{m:02}"), MethodKind::Call, false, &[], None)).collect::<Vec<_>>();
        assert_eq!(encode(&Interface { methods: methods[..MAX_METHODS].to_vec() }).map(|b| b.len()), Ok(2 + MAX_METHODS * 8));
        assert_eq!(encode(&Interface { methods }).unwrap_err().rule, Rule::MethodCount);
        assert_eq!(encode(&Interface { methods: vec![] }).unwrap_err().rule, Rule::MethodCount);
        let many = (0..300).map(|m| method(&format!("m{m:03}"), MethodKind::Call, false, &[], None)).collect();
        assert_eq!(encode(&Interface { methods: many }).unwrap_err().rule, Rule::MethodCount, "a count past a u8 isn't wrapped");
    }

    #[test]
    fn refuses_names_that_arent_identifiers() {
        let bad: [&[u8]; 9] = [b"", b"Transfer", b"9lives", b"_x", b"a-b", b"a b", "caf\u{e9}".as_bytes(), b"x\0", &[b'a'; 33]];
        for name in bad {
            let mut as_method = vec![1, 1, name.len() as u8];
            as_method.extend_from_slice(name);
            as_method.extend_from_slice(&[0, 0, 0, 0]);
            assert_eq!(refused(&as_method), Rule::Ident, "method {name:?}");
            let mut as_arg = vec![1, 1, 1, b'm', 0, 0, 1, name.len() as u8];
            as_arg.extend_from_slice(name);
            as_arg.extend_from_slice(&[1, 0]);
            assert_eq!(refused(&as_arg), Rule::Ident, "argument {name:?}");
        }
        for len in [0, 33, 255] {
            let e = decode(&[1, 1, len]).unwrap_err();
            assert_eq!((e.rule, e.offset), (Rule::Ident, 2), "a method name's length {len} is refused before its bytes are read");
            let e = decode(&[1, 1, 1, b'm', 0, 0, 1, len]).unwrap_err();
            assert_eq!((e.rule, e.offset), (Rule::Ident, 7), "an argument name's length {len} is refused before its bytes are read");
        }
        let longest = "a".repeat(32);
        assert!(decode(&[&[1, 1, 32][..], longest.as_bytes(), &[0, 0, 0, 0]].concat()).is_ok(), "32 bytes is an identifier");
        let mut i = example();
        i.methods[2].args[0].name = "x".repeat(256);
        assert_eq!(encode(&i).unwrap_err().rule, Rule::Ident, "a length past a u8 isn't wrapped");
    }

    #[test]
    fn refuses_methods_out_of_order_or_twice() {
        let two = |a: &str, b: &str| [&[1u8, 2, a.len() as u8][..], a.as_bytes(), &[0, 0, 0, 0, b.len() as u8], b.as_bytes(), &[0, 0, 0, 0]].concat();
        assert!(decode(&two("a", "b")).is_ok());
        assert!(decode(&two("a", "a_")).is_ok(), "a prefix sorts first");
        assert!(decode(&two("a9", "a_")).is_ok(), "digits sort before '_'");
        assert_eq!(refused(&two("b", "a")), Rule::Order);
        assert_eq!(refused(&two("a_", "a")), Rule::Order);
        assert_eq!(refused(&two("a", "a")), Rule::Order, "a duplicate");
        let mut i = example();
        i.methods.swap(0, 2);
        assert_eq!(encode(&i).unwrap_err().rule, Rule::Order);
    }

    #[test]
    fn refuses_unknown_kinds_flags_and_has_return() {
        let base = hex("0101016d00000000");
        for (i, v, rule) in [(4, 3, Rule::Kind), (4, 255, Rule::Kind), (5, 2, Rule::Flags), (5, 3, Rule::Flags), (5, 0x80, Rule::Flags), (7, 2, Rule::HasReturn), (7, 255, Rule::HasReturn)] {
            let mut b = base.clone();
            b[i] = v;
            assert_eq!(refused(&b), rule, "byte {i} = {v}");
        }
        assert!(decode(&base).is_ok());
    }

    #[test]
    fn refuses_more_than_16_arguments_and_duplicate_names() {
        let with = |n: u8| {
            let mut b = vec![1, 1, 1, b'm', 0, 0, n];
            for a in 0..n {
                b.extend_from_slice(&[2, b'a', b'a' + a, 1]);
            }
            b.push(0);
            b
        };
        assert!(decode(&with(16)).is_ok());
        assert_eq!(refused(&with(17)), Rule::ArgCount);
        assert_eq!(refused(&hex("0101016d00000201780101780100")), Rule::ArgName, "m(x: u8, x: u8)");
        let mut i = example();
        i.methods[2].args = (0..17).map(|a| Arg { name: format!("a{a}"), ty: Type::U8 }).collect();
        assert_eq!(encode(&i).unwrap_err().rule, Rule::ArgCount);
        i.methods[2].args = (0..300).map(|a| Arg { name: format!("a{a}"), ty: Type::U8 }).collect();
        assert_eq!(encode(&i).unwrap_err().rule, Rule::ArgCount, "a count past a u8 isn't wrapped");
    }

    #[test]
    fn refuses_unknown_type_tags() {
        for tag in [0, 17, 18, 255] {
            assert_eq!(refused(&one_arg(&[tag])), Rule::TypeTag, "argument {tag}");
            assert_eq!(refused(&one_arg(&[15, tag])), Rule::TypeTag, "inside an option, {tag}");
            assert_eq!(refused(&hex(&format!("0101016d00000001{tag:02x}"))), Rule::TypeTag, "returned, {tag}");
        }
        for tag in 1..=12 {
            assert!(decode(&one_arg(&[tag])).is_ok(), "{tag}");
        }
    }

    #[test]
    fn refuses_maxes_out_of_range() {
        let bytes = |max: u32| [&[13u8][..], &max.to_le_bytes()].concat();
        let text = |max: u16| [&[14u8][..], &max.to_le_bytes()].concat();
        let vec = |max: u16| [&[16u8, 1][..], &max.to_le_bytes()].concat();
        for ok in [bytes(1), bytes(MAX_BYTES_MAX), text(1), text(MAX_TEXT_MAX), vec(1), vec(MAX_VEC_MAX)] {
            assert!(decode(&one_arg(&ok)).is_ok(), "{ok:?}");
        }
        for bad in [bytes(0), bytes(MAX_BYTES_MAX + 1), bytes(u32::MAX), text(0), text(MAX_TEXT_MAX + 1), text(u16::MAX), vec(0), vec(MAX_VEC_MAX + 1), vec(u16::MAX)] {
            assert_eq!(refused(&one_arg(&bad)), Rule::Max, "{bad:?}");
        }
        let mut i = example();
        i.methods[1].args[0].ty = Type::Text { max: 0 };
        assert_eq!(encode(&i).unwrap_err().rule, Rule::Max);
    }

    #[test]
    fn refuses_types_nested_past_four_layers() {
        // option and vec layers in any mix: four around a scalar, never five.
        for layers in [[15u8, 15, 15, 15], [16, 16, 16, 16], [15, 16, 15, 16], [16, 15, 16, 15]] {
            let encode_layers = |inner: &[u8], layers: &[u8]| {
                let mut ty = inner.to_vec();
                for l in layers.iter().rev() {
                    ty = if *l == 15 { [&[15u8][..], &ty].concat() } else { [&[16u8][..], &ty, &[1, 0]].concat() };
                }
                ty
            };
            assert!(decode(&one_arg(&encode_layers(&[1], &layers))).is_ok(), "{layers:?}");
            for fifth in [15u8, 16] {
                let five = [&layers[..], &[fifth]].concat();
                assert_eq!(refused(&one_arg(&encode_layers(&[1], &five))), Rule::Nesting, "{five:?}");
            }
        }
        let mut deep = Type::U8;
        for _ in 0..5 {
            deep = Type::Option(Box::new(deep));
        }
        let mut i = example();
        i.methods[2].returns = Some(deep);
        assert_eq!(encode(&i).unwrap_err().rule, Rule::Nesting);
    }

    #[test]
    fn refuses_a_second_init_a_payable_view_and_an_init_that_returns() {
        let mut i = example();
        i.methods[2].kind = MethodKind::Init;
        assert_eq!(encode(&i).unwrap_err().rule, Rule::TwoInits);
        let mut i = example();
        i.methods[0].payable = true;
        assert_eq!(encode(&i).unwrap_err().rule, Rule::PayableView);
        let mut i = example();
        i.methods[1].returns = Some(Type::U8);
        assert_eq!(encode(&i).unwrap_err().rule, Rule::InitReturns);
        // The same, from bytes.
        assert_eq!(refused(&hex("0102016102000000016202000000")), Rule::TwoInits);
        assert_eq!(refused(&hex("0101016d01010000")), Rule::PayableView);
        assert_eq!(refused(&hex("0101016d0200000101")), Rule::InitReturns);
        // What is allowed: a payable init, a view with arguments and a return, an interface
        // without an init.
        assert!(decode(&hex("0101016d02010000")).is_ok());
        assert!(decode(&hex("0101016d01000101780a0101")).is_ok());
    }

    #[test]
    fn refuses_colliding_selectors() {
        let bytes = hex(EXAMPLE);
        assert!(decode_with(&bytes, selector).is_ok());
        // A selector function under which every name longer than four bytes collides: here
        // "balance_of" and "transfer".
        let collide = |name: &str| {
            let mut s = [0; 8];
            s[0] = u8::from(name.len() > 4);
            s
        };
        let e = decode_with(&bytes, collide).unwrap_err();
        assert_eq!((e.rule, e.offset), (Rule::Selector, 51), "transfer, at byte 51, collides with balance_of");
        assert!(decode_with(&hex("0101016d00000000"), |_| [0; 8]).is_ok(), "one method collides with nothing");
    }

    #[test]
    fn check_exports_matches_methods_and_exports_one_to_one() {
        let i = example();
        let exports = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(check_exports(&i, &exports(&["balance_of", "init", "transfer"])), Ok(()));
        assert_eq!(check_exports(&i, &exports(&["transfer", "init", "balance_of"])), Ok(()), "in any order");
        assert_eq!(check_exports(&i, &exports(&["balance_of", "transfer"])), Err(Mismatch::NoExport("init".into())));
        assert_eq!(check_exports(&i, &exports(&["balance_of", "init", "mint", "transfer"])), Err(Mismatch::NoMethod("mint".into())));
        assert_eq!(check_exports(&i, &exports(&["balance_of", "init", "transfer_"])), Err(Mismatch::NoExport("transfer".into())));
        assert_eq!(check_exports(&i, &[]), Err(Mismatch::NoExport("balance_of".into())));
    }

    #[test]
    fn errors_say_where_and_what() {
        let e = decode(&hex("0101016d00000002")).unwrap_err();
        assert_eq!((e.rule, e.offset), (Rule::HasReturn, 7));
        assert_eq!(e.to_string(), "interface: method m has has_return 2: 0 or 1 (at byte 7)");
        let e = decode(&[1, 1, 3, b'B', 0xff, b'x', 0, 0, 0, 0]).unwrap_err();
        assert_eq!(e.rule, Rule::Ident);
        assert!(!e.message.contains('B'), "an invalid name isn't quoted: {e}");
    }
}
