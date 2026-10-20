//! The committed guest programs (`tests/programs/*.wasm`, built from `guests/` by
//! `guests/build.sh`) against what upload validation (docs/PROGRAMS.md 4.2) and host ABI v1 (10)
//! require, checked with wasmparser and `abi.rs` alone, so the guests are known good whatever
//! `validate.rs` and the backends do. Their documented behavior is checked on the real host
//! (upload, `exec::execute`, `MemHost`) in `tests/e2e.rs`.
//!
//! `cargo test --test guests -- --nocapture` prints each module's shape.

use std::path::{Path, PathBuf};

use blockula_vm::{abi, config};
use wasmparser::{ExternalKind, FuncType, MemoryType, Parser, Payload, TypeRef, ValType, Validator, WasmFeatures};

/// Each program and its methods, as its crate's doc comment documents them.
const PROGRAMS: &[(&str, &[&str])] = &[
    ("counter", &["get", "increment"]),
    ("escrow", &["open", "refund", "release"]),
    ("router", &["send", "try_send"]),
    ("token", &["balance_of", "init", "total_supply", "transfer"]),
];

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/programs")
}

fn wasm(name: &str) -> Vec<u8> {
    let path = dir().join(format!("{name}.wasm"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (run guests/build.sh)", path.display()))
}

/// MVP plus mutable globals (W3C Core 1.0, what wasm32v1-none emits) without floats: the
/// engine configuration's feature set (4.2).
fn features() -> WasmFeatures {
    WasmFeatures::WASM1.difference(WasmFeatures::FLOATS)
}

/// What the checks need from a module, read in one pass.
#[derive(Default)]
struct Shape {
    types: Vec<FuncType>,
    imports: Vec<(String, String, TypeRef)>,
    /// Type index of each defined function.
    funcs: Vec<u32>,
    memories: Vec<MemoryType>,
    tables: usize,
    globals: usize,
    exports: Vec<(String, ExternalKind, u32)>,
    start: bool,
    custom: Vec<String>,
    data_bytes: usize,
    code_bytes: usize,
    /// Any operator with a float in it, reachable or not.
    float_ops: Vec<String>,
}

fn shape(wasm: &[u8]) -> Shape {
    let mut s = Shape::default();
    for payload in Parser::new(0).parse_all(wasm) {
        match payload.unwrap() {
            Payload::TypeSection(r) => {
                for group in r {
                    for t in group.unwrap().into_types() {
                        s.types.push(t.unwrap_func().clone());
                    }
                }
            }
            Payload::ImportSection(r) => {
                for i in r {
                    let i = i.unwrap();
                    s.imports.push((i.module.into(), i.name.into(), i.ty));
                }
            }
            Payload::FunctionSection(r) => s.funcs = r.into_iter().map(Result::unwrap).collect(),
            Payload::TableSection(r) => s.tables = r.count() as usize,
            Payload::MemorySection(r) => s.memories = r.into_iter().map(Result::unwrap).collect(),
            Payload::GlobalSection(r) => s.globals = r.count() as usize,
            Payload::ExportSection(r) => {
                for e in r {
                    let e = e.unwrap();
                    s.exports.push((e.name.into(), e.kind, e.index));
                }
            }
            Payload::StartSection { .. } => s.start = true,
            Payload::DataSection(r) => {
                for d in r {
                    s.data_bytes += d.unwrap().data.len();
                }
            }
            Payload::CodeSectionStart { size, .. } => s.code_bytes = size as usize,
            Payload::CodeSectionEntry(body) => {
                for op in body.get_operators_reader().unwrap() {
                    let op = format!("{:?}", op.unwrap());
                    if op.contains("F32") || op.contains("F64") {
                        s.float_ops.push(op);
                    }
                }
            }
            Payload::CustomSection(c) => s.custom.push(c.name().into()),
            _ => {}
        }
    }
    s
}

fn tys(t: &[abi::Ty]) -> Vec<ValType> {
    t.iter().map(|t| if *t == abi::Ty::I32 { ValType::I32 } else { ValType::I64 }).collect()
}

impl Shape {
    /// The type of function `index` in the function index space (imports first).
    fn func_type(&self, index: u32) -> &FuncType {
        let imported: Vec<u32> = self.imports.iter().filter_map(|i| if let TypeRef::Func(t) = i.2 { Some(t) } else { None }).collect();
        let i = index as usize;
        let t = if i < imported.len() { imported[i] } else { self.funcs[i - imported.len()] };
        &self.types[t as usize]
    }
}

#[test]
fn exactly_the_documented_programs_are_committed() {
    let mut found: Vec<String> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".wasm"))
        .collect();
    found.sort();
    let expected: Vec<String> = PROGRAMS.iter().map(|(p, _)| format!("{p}.wasm")).collect();
    assert_eq!(found, expected);
}

#[test]
fn they_validate_as_wasm1_without_floats() {
    for (name, _) in PROGRAMS {
        let wasm = wasm(name);
        Validator::new_with_features(features()).validate_all(&wasm).unwrap_or_else(|e| panic!("{name}: {e}"));
        let s = shape(&wasm);
        assert!(s.float_ops.is_empty(), "{name}: float operators {:?}", s.float_ops);
        assert!(wasm.len() <= config::MAX_CODE_BYTES, "{name}: {} bytes", wasm.len());
        println!(
            "{name:8} {:5} bytes: code {:5}, data {:3}, {} imports, {} functions, {} globals, {} tables",
            wasm.len(),
            s.code_bytes,
            s.data_bytes,
            s.imports.len(),
            s.funcs.len(),
            s.globals,
            s.tables
        );
    }
}

/// Neither the validator nor the engine is enough: wasmparser 0.228.0 accepts a float-to-int
/// conversion or reinterpret in unreachable code with floats off, and soroban-wasmi translates it.
/// It can never run, but 4.3 says any float instruction is refused: `validate.rs` has to scan
/// operators, as [`shape`] does here. If this test fails, a parser closed the gap.
#[test]
fn floats_off_still_lets_an_unreachable_float_conversion_through() {
    for op in ["i32.trunc_f32_s", "i64.trunc_f64_u", "i32.reinterpret_f32", "i64.reinterpret_f64"] {
        let wasm = wat::parse_str(format!("(module (func unreachable {op} drop))")).unwrap();
        Validator::new_with_features(features()).validate_all(&wasm).unwrap_or_else(|e| panic!("{op} refused: {e}"));
        assert_eq!(shape(&wasm).float_ops.len(), 1, "{op}");
    }
    // The engine's `floats(false)` doesn't refuse it either.
    let wasm = wat::parse_str("(module (func unreachable i32.trunc_f32_s drop))").unwrap();
    soroban_wasmi::Module::new(&soroban_engine(), &wasm).expect("soroban-wasmi refused it");
    // A float in reachable code, or a float type anywhere, is refused.
    for module in ["(module (func f32.const 1 drop))", "(module (func (local f64)))", "(module (func (param f32)))"] {
        let wasm = wat::parse_str(module).unwrap();
        assert!(Validator::new_with_features(features()).validate_all(&wasm).is_err(), "{module}");
    }
}

#[test]
fn every_import_is_host_abi_v1_with_its_exact_signature() {
    for (name, _) in PROGRAMS {
        let s = shape(&wasm(name));
        let mut seen = Vec::new();
        for (module, field, t) in &s.imports {
            assert_eq!(module, abi::MODULE, "{name}: import {module}.{field}");
            let TypeRef::Func(index) = t else { panic!("{name}: {field} isn't a function: {t:?}") };
            let host = abi::host_fn(field).unwrap_or_else(|| panic!("{name}: {field} isn't in HOST_FNS"));
            let got = &s.types[*index as usize];
            assert_eq!((got.params().to_vec(), got.results().to_vec()), (tys(host.params), tys(host.results)), "{name}: {field}");
            assert!(!seen.contains(field), "{name}: {field} imported twice");
            seen.push(field.clone());
        }
        println!("{name:8} imports {}", seen.join(", "));
    }
}

#[test]
fn exports_are_memory_and_the_documented_methods() {
    for (name, methods) in PROGRAMS {
        let s = shape(&wasm(name));
        let mut got = Vec::new();
        for (export, kind, index) in &s.exports {
            match kind {
                ExternalKind::Memory => assert!(export == "memory" && *index == 0, "{name}: memory exported as {export}"),
                ExternalKind::Func => {
                    let method = export.strip_prefix(abi::EXPORT_PREFIX).unwrap_or_else(|| panic!("{name}: exports {export}"));
                    assert!(abi::is_ident(method), "{name}: {export}");
                    let t = s.func_type(*index);
                    assert!(t.params().is_empty() && t.results().is_empty(), "{name}: {export} has type {t:?}");
                    got.push(method.to_string());
                }
                _ => panic!("{name}: exports {export} as {kind:?}"),
            }
        }
        got.sort();
        assert_eq!(got, *methods, "{name}");
        assert_eq!(s.exports.iter().filter(|e| e.1 == ExternalKind::Memory).count(), 1, "{name}: memory export");
    }
}

#[test]
fn memory_is_fixed_at_32_pages_and_there_is_no_start() {
    for (name, _) in PROGRAMS {
        let s = shape(&wasm(name));
        assert!(!s.imports.iter().any(|i| matches!(i.2, TypeRef::Memory(_))), "{name}: imports a memory");
        let [m] = s.memories[..] else { panic!("{name}: {} memories", s.memories.len()) };
        assert_eq!((m.initial, m.maximum), (config::MAX_PAGES as u64, Some(config::MAX_PAGES as u64)), "{name}");
        assert!(!m.memory64 && !m.shared && m.page_size_log2.is_none(), "{name}: {m:?}");
        assert!(!s.start, "{name}: has a start section");
        // Stripped: no name, producers or target_features section, so no paths or toolchain
        // strings ride along. Only rustc's __stack_pointer global; no table (no call_indirect).
        assert!(s.custom.is_empty(), "{name}: custom sections {:?}", s.custom);
        assert_eq!((s.globals, s.tables), (1, 0), "{name}");
    }
}

/// guests/rust-toolchain.toml adds the wasm target to the repo's compiler; the committed bytes
/// are reproducible only with that compiler, so the two pins must agree.
#[test]
fn guests_build_with_the_repo_compiler() {
    let channel = |path: &Path| {
        let text = std::fs::read_to_string(path).unwrap();
        let line = text.lines().find(|l| l.trim_start().starts_with("channel")).unwrap().to_string();
        line.split('"').nth(1).unwrap().to_string()
    };
    let vm = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(channel(&vm.join("guests/rust-toolchain.toml")), channel(&vm.join("../../rust-toolchain.toml")));
}

/// The engine translates every guest under the design's configuration (4.2).
#[test]
fn the_engine_loads_them_under_the_design_config() {
    let soroban = soroban_engine();
    for (name, _) in PROGRAMS {
        soroban_wasmi::Module::new(&soroban, &wasm(name)).unwrap_or_else(|e| panic!("{name} on soroban-wasmi: {e}"));
    }
}

/// soroban-wasmi under the design's configuration (4.2), schedule v1's operator classes.
fn soroban_engine() -> soroban_wasmi::Engine {
    use soroban_wasmi as w;
    let ops = blockula_vm::meter::V1.ops;
    let mut costs = w::FuelCosts::default();
    (costs.base, costs.entity, costs.load, costs.store, costs.call) = (ops.base.into(), ops.entity.into(), ops.load.into(), ops.store.into(), ops.call.into());
    let mut c = w::Config::default();
    c.consume_fuel(true)
        .fuel_consumption_mode(w::FuelConsumptionMode::Eager)
        .set_fuel_costs(costs)
        .floats(false)
        .wasm_mutable_global(true)
        .wasm_sign_extension(false)
        .wasm_multi_value(false)
        .wasm_bulk_memory(false)
        .wasm_reference_types(false)
        .wasm_saturating_float_to_int(false)
        .wasm_tail_call(false)
        .wasm_extended_const(false)
        .set_stack_limits(w::StackLimits::new(8_192, 131_072, 1_024).unwrap());
    w::Engine::new(&c)
}
