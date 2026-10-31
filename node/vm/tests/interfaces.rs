//! The four guests' interfaces (docs/PROGRAMS-V0.md §3), committed beside their code as the JSON a
//! deployer writes (`tests/programs/<guest>.iface.json`, the form `blockula program upload
//! --interface` reads, §3.4) and the format-1 bytes an `UploadCode` carries
//! (`tests/programs/<guest>.iface`): what the simulator uploads (§10.2), the `interface_codec`
//! fuzzer's seeds and vectors for a later TypeScript port. The interfaces are written here, in
//! [`guests`]; both files must match them. After changing one, regenerate the files with
//! `cargo test --release --test interfaces write -- --ignored`.
//!
//! Each interface's methods are exactly its guest's `bk_` exports (§3.3), and each declares the
//! kinds and payability the guest is written for: `get`, `balance_of` and `total_supply` are
//! views; the token's `init` is its initializer, run once by `CreateProgram`; only the escrow's
//! `open` takes value. The guests run under them below.
//!
//! **Types.** The guests predate the interface format. Their inputs and returns are Borsh, the
//! convention the types describe (docs/PROGRAMS.md 11.3), wherever they're fixed-size fields (an
//! address is its 32 bytes, a `u64` 8 little-endian ones): every input but the router's `send`,
//! whose hops are a bare run of 0 to 8 addresses with no `u32` count before them. Format 1 can't
//! say that; its interface types the hops `vec<address, 8>`, the closest description, and a client
//! that encodes them with Borsh's count gets the router's `BAD_INPUT` back, never a wrong transfer.
//! The router's `try_send` returns `status i32 ‖ abort code i32 ‖ length u32 ‖ data`, which format 1
//! (no `i32`, no tuples) describes as raw `bytes` of at most 268 (12 + 256). The chain never reads
//! argument or return types (§3.1); only names, kinds and payability reach execution.

use std::path::{Path, PathBuf};

use blockula_vm::engine::{Address, EngineKind, TrapKind};
use blockula_vm::exec::{Call, FailCode, Receipt, TxLimits, execute};
use blockula_vm::host::MethodKind;
use blockula_vm::interface::{Arg, Interface, Method, Mismatch, Type, check_exports, decode, encode};
use blockula_vm::meter::SCHEDULE_V0;
use blockula_vm::testing::{MemHost, addr};
use blockula_vm::validate::validate;

const GUESTS: &[&str] = &["counter", "token", "escrow", "router"];

fn method(name: &str, kind: MethodKind, payable: bool, args: &[(&str, Type)], returns: Option<Type>) -> Method {
    let args = args.iter().map(|(name, ty)| Arg { name: name.to_string(), ty: ty.clone() }).collect();
    Method { name: name.into(), kind, payable, args, returns }
}

/// The guests' interfaces, as their crates' docs (`guests/<guest>/src/lib.rs`) describe them.
fn guests() -> Vec<(&'static str, Interface)> {
    use MethodKind::{Call, Init, View};
    use Type::{Address, U64};
    let hops = Type::Vec { of: Box::new(Address), max: 8 };
    vec![
        ("counter", Interface { methods: vec![method("get", View, false, &[], Some(U64)), method("increment", Call, false, &[], Some(U64))] }),
        (
            "token",
            Interface {
                methods: vec![
                    method("balance_of", View, false, &[("account", Address)], Some(U64)),
                    method("init", Init, false, &[("owner", Address), ("supply", U64)], None),
                    method("total_supply", View, false, &[], Some(U64)),
                    method("transfer", Call, false, &[("to", Address), ("amount", U64)], None),
                ],
            },
        ),
        (
            "escrow",
            Interface {
                methods: vec![
                    method("open", Call, true, &[("id", U64), ("seller", Address)], None),
                    method("refund", Call, false, &[("id", U64)], None),
                    method("release", Call, false, &[("id", U64)], None),
                ],
            },
        ),
        (
            "router",
            Interface {
                methods: vec![
                    method("send", Call, false, &[("hops", hops), ("token", Address), ("to", Address), ("amount", U64)], Some(U64)),
                    method("try_send", Call, false, &[("token", Address), ("to", Address), ("amount", U64)], Some(Type::Bytes { max: 268 })),
                ],
            },
        ),
    ]
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/programs")
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A JSON value, written the way `serde_json::to_string_pretty` lays one out (two-space indent,
/// keys in the order given), so the committed files read as any JSON tool writes them.
enum Json {
    Null,
    Bool(bool),
    Num(u64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(&'static str, Json)>),
}

impl Json {
    fn write(&self, indent: usize, out: &mut String) {
        let pad = |n: usize| "  ".repeat(n);
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => out.push_str(&n.to_string()),
            // Identifiers and type names only: nothing to escape.
            Json::Str(s) => out.push_str(&format!("\"{s}\"")),
            Json::Arr(items) if items.is_empty() => out.push_str("[]"),
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    out.push_str(&pad(indent + 1));
                    item.write(indent + 1, out);
                }
                out.push('\n');
                out.push_str(&pad(indent));
                out.push(']');
            }
            Json::Obj(fields) => {
                out.push('{');
                for (i, (key, value)) in fields.iter().enumerate() {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    out.push_str(&format!("{}\"{key}\": ", pad(indent + 1)));
                    value.write(indent + 1, out);
                }
                out.push('\n');
                out.push_str(&pad(indent));
                out.push('}');
            }
        }
    }
}

/// A type in §3.4's JSON form: scalars are strings; `{ "bytes": max }`, `{ "text": max }`,
/// `{ "option": type }` and `{ "vec": [type, max] }` are the rest.
fn type_json(ty: &Type) -> Json {
    let s = |s: &str| Json::Str(s.into());
    match ty {
        Type::U8 => s("u8"),
        Type::U16 => s("u16"),
        Type::U32 => s("u32"),
        Type::U64 => s("u64"),
        Type::U128 => s("u128"),
        Type::I64 => s("i64"),
        Type::I128 => s("i128"),
        Type::Bool => s("bool"),
        Type::Fang => s("fang"),
        Type::Address => s("address"),
        Type::Name => s("name"),
        Type::Hash32 => s("hash32"),
        Type::Bytes { max } => Json::Obj(vec![("bytes", Json::Num(u64::from(*max)))]),
        Type::Text { max } => Json::Obj(vec![("text", Json::Num(u64::from(*max)))]),
        Type::Option(of) => Json::Obj(vec![("option", type_json(of))]),
        Type::Vec { of, max } => Json::Obj(vec![("vec", Json::Arr(vec![type_json(of), Json::Num(u64::from(*max))]))]),
    }
}

/// `interface` in §3.4's JSON form, as the committed `.iface.json` holds it.
fn json(interface: &Interface) -> String {
    let kind = |k: MethodKind| match k {
        MethodKind::Call => "call",
        MethodKind::View => "view",
        MethodKind::Init => "init",
    };
    let methods = interface
        .methods
        .iter()
        .map(|m| {
            let args = m.args.iter().map(|a| Json::Obj(vec![("name", Json::Str(a.name.clone())), ("type", type_json(&a.ty))])).collect();
            Json::Obj(vec![
                ("name", Json::Str(m.name.clone())),
                ("kind", Json::Str(kind(m.kind).into())),
                ("payable", Json::Bool(m.payable)),
                ("args", Json::Arr(args)),
                ("returns", m.returns.as_ref().map_or(Json::Null, type_json)),
            ])
        })
        .collect();
    let mut out = String::new();
    Json::Obj(vec![("format", Json::Num(1)), ("methods", Json::Arr(methods))]).write(0, &mut out);
    out.push('\n');
    out
}

/// Writes both files of every guest. `cargo test --release --test interfaces write -- --ignored`.
#[test]
#[ignore]
fn write() {
    for (name, interface) in guests() {
        std::fs::write(dir().join(format!("{name}.iface")), encode(&interface).unwrap()).unwrap();
        std::fs::write(dir().join(format!("{name}.iface.json")), json(&interface)).unwrap();
    }
}

#[test]
fn the_committed_files_are_the_guests_interfaces() {
    let regenerate = "out of date: `cargo test --release --test interfaces write -- --ignored` rewrites it";
    for (name, interface) in guests() {
        let bytes = read(&dir().join(format!("{name}.iface")));
        assert_eq!(bytes, encode(&interface).unwrap(), "{name}.iface {regenerate}");
        assert_eq!(decode(&bytes).unwrap(), interface, "{name}.iface decodes to its interface");
        assert_eq!(encode(&decode(&bytes).unwrap()).unwrap(), bytes, "{name}.iface is canonical");
        let text = String::from_utf8(read(&dir().join(format!("{name}.iface.json")))).unwrap();
        assert_eq!(text, json(&interface), "{name}.iface.json {regenerate}");
    }
    // No stray interface without a guest.
    let mut files: Vec<String> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|f| f.ends_with(".iface") || f.ends_with(".iface.json"))
        .collect();
    files.sort();
    let mut expected: Vec<String> = GUESTS.iter().flat_map(|g| [format!("{g}.iface"), format!("{g}.iface.json")]).collect();
    expected.sort();
    assert_eq!(files, expected);
}

/// §3.3: each interface's methods are exactly its code's `bk_` exports, as upload validation
/// reports them.
#[test]
fn each_interface_matches_its_guests_exports() {
    for (name, interface) in guests() {
        let info = validate(&read(&dir().join(format!("{name}.wasm")))).unwrap_or_else(|e| panic!("{name}: {e}"));
        let methods: Vec<&str> = interface.methods.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(info.exports, methods, "{name}");
        assert_eq!(check_exports(&interface, &info.exports), Ok(()), "{name}");
    }
}

/// A missing method, an extra one, a renamed one or another guest's interface: refused.
#[test]
fn a_mismatch_with_the_code_is_refused() {
    let all = guests();
    for (name, interface) in &all {
        let exports = validate(&read(&dir().join(format!("{name}.wasm")))).unwrap().exports;
        let first = interface.methods[0].name.clone();

        let mut fewer = interface.clone();
        fewer.methods.remove(0);
        if !fewer.methods.is_empty() {
            assert_eq!(check_exports(&fewer, &exports), Err(Mismatch::NoMethod(first.clone())), "{name} without {first}");
        }
        let mut more = interface.clone();
        more.methods.push(method("zzz", MethodKind::Call, false, &[], None));
        assert_eq!(check_exports(&more, &exports), Err(Mismatch::NoExport("zzz".into())), "{name} with zzz");
        let mut renamed = interface.clone();
        renamed.methods[0].name.push('_');
        assert_eq!(check_exports(&renamed, &exports), Err(Mismatch::NoExport(format!("{first}_"))), "{name} renamed");

        for (other, theirs) in &all {
            if other != name {
                assert!(check_exports(theirs, &exports).is_err(), "{other}'s interface on {name}'s code");
            }
        }
    }
}

/// Runs `f` on a thread with the program thread's stack (64 MiB), as the node does.
fn on_program_thread<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    std::thread::scope(|s| std::thread::Builder::new().stack_size(64 << 20).spawn_scoped(s, f).unwrap().join().unwrap())
}

/// The guests deployed with their interfaces' kinds and payability, as the node's `View` answers
/// them from the stored interface.
fn chain() -> MemHost {
    let mut host = MemHost::new();
    host.fund(addr("alice"), 10_000_000_000);
    for (name, interface) in guests() {
        host.deploy(addr(name), &read(&dir().join(format!("{name}.wasm"))));
        for m in &interface.methods {
            host.set_method(addr(name), &m.name, m.kind, m.payable);
            assert_eq!(blockula_vm::host::View::method(&host, &addr(name), &m.name), interface.method(&m.name));
        }
    }
    host
}

/// One transaction of `calls` from alice, every guest declared, on every engine (the receipts
/// agree but for fuel). A successful one is applied.
fn run(host: &mut MemHost, calls: Vec<(&str, &str, Vec<u8>, u64, MethodKind)>) -> Receipt {
    let calls: Vec<Call> = calls
        .into_iter()
        .map(|(program, method, input, value, kind)| host.bind(Call { program: addr(program), version_digest: [0; 32], method: method.into(), input, value, kind }))
        .collect();
    let declared: Vec<Address> = GUESTS.iter().map(|g| addr(g)).collect();
    let limits = TxLimits { fuel_limit: 10_000_000, storage_limit: 1_000_000_000 };
    let h = &*host;
    let receipts: Vec<Receipt> = EngineKind::ALL
        .iter()
        .map(|&kind| {
            on_program_thread(|| {
                let (engine, _) = h.engine_for(kind, &SCHEDULE_V0, &declared).unwrap();
                execute(engine.as_ref(), h, addr("alice"), &calls, &MemHost::any_version(&declared), limits, &SCHEDULE_V0).unwrap()
            })
        })
        .collect();
    for r in &receipts[1..] {
        assert_eq!(Receipt { fuel_used: receipts[0].fuel_used, ..r.clone() }, receipts[0]);
    }
    let r = receipts[0].clone();
    if r.ok() {
        host.apply(&r.diff);
    }
    r
}

fn trap(r: &Receipt) -> Option<TrapKind> {
    match &r.fail {
        Some(FailCode::Trapped { kind, .. }) => Some(*kind),
        _ => None,
    }
}

/// What the interfaces declare is what the guests are written for: each runs under its own
/// interface, and the host enforces kinds and payability before a guest's own checks.
#[test]
fn the_guests_run_under_their_interfaces() {
    use MethodKind::{Call, Init, View};
    let mut host = chain();
    let (alice, bob, router) = (addr("alice"), addr("bob"), addr("router"));
    let le = |n: u64| n.to_le_bytes().to_vec();

    // The counter: `increment` is a call, `get` a view no transaction calls.
    let r = run(&mut host, vec![("counter", "increment", vec![], 0, Call)]);
    assert_eq!(r.returns, vec![le(1)]);
    assert_eq!(trap(&run(&mut host, vec![("counter", "get", vec![], 0, Call)])), Some(TrapKind::NotCallable));

    // The token: `init` runs only as its initializer (as `CreateProgram` runs it), never as a call.
    let init = [&router[..], &le(1_000)].concat();
    assert_eq!(trap(&run(&mut host, vec![("token", "init", init.clone(), 0, Call)])), Some(TrapKind::NotCallable));
    assert!(run(&mut host, vec![("token", "init", init, 0, Init)]).ok());
    assert_eq!(trap(&run(&mut host, vec![("token", "total_supply", vec![], 0, Call)])), Some(TrapKind::NotCallable));
    assert_eq!(trap(&run(&mut host, vec![("token", "transfer", [&bob[..], &le(1)].concat(), 1, Call)])), Some(TrapKind::NotPayable));

    // The router calls the token's `transfer` (a call) and `balance_of` (a view) for its own
    // balance, which a call frame may.
    let r = run(&mut host, vec![("router", "send", [&addr("token")[..], &bob, &le(400)].concat(), 0, Call)]);
    assert!(r.ok(), "{:?}", r.fail);
    assert_eq!(r.returns, vec![le(600)]);
    let r = run(&mut host, vec![("router", "try_send", [&addr("token")[..], &bob, &le(10_000)].concat(), 0, Call)]);
    assert!(r.ok(), "{:?}", r.fail);
    assert_eq!(r.returns[0][..4], 2i32.to_le_bytes(), "ERR_ABORT: the token aborted 'balance short'");

    // The escrow: only `open` takes value.
    let open = [&le(1)[..], &bob].concat();
    assert!(run(&mut host, vec![("escrow", "open", open, 5_000, Call)]).ok());
    assert_eq!(trap(&run(&mut host, vec![("escrow", "release", le(1), 1, Call)])), Some(TrapKind::NotPayable));
    let r = run(&mut host, vec![("escrow", "refund", le(1), 0, Call)]);
    assert!(matches!(r.fail, Some(FailCode::Aborted { code: 24, .. })), "not the seller: {:?}", r.fail);

    // A view method runs in a read-only call (`/view`, §8.2).
    assert_eq!(run(&mut host, vec![("token", "balance_of", bob.to_vec(), 0, View)]).returns, vec![le(400)]);
    assert_eq!(run(&mut host, vec![("counter", "get", vec![], 0, View)]).returns, vec![le(1)]);
    assert_eq!(host.balances[&alice], 10_000_000_000 - 5_000 - u64::try_from(host.escrow).unwrap(), "alice paid the value and every deposit");
}
