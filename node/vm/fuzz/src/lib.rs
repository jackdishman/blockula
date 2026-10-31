//! Fuzz harnesses for the programs engine (S4). The `fuzz_targets/*` binaries are thin
//! `fuzz_target!` wrappers over these functions, so the logic can be exercised on stable by the
//! `#[cfg(test)]` seed checks below.
//!
//! The invariants, from docs/PROGRAMS.md 4.2–4.4 and docs/PROGRAMS-V0.md §3 and §10.4:
//! * [`program_validate`]: `validate()` never panics, and everything it accepts uploads: the test
//!   translation neither panics nor refuses it. Inputs that start with the Wasm magic are checked as
//!   they are (the guests are their seeds); any other input drives a `wasm-smith` generator
//!   restricted to v0's subset ([`smith_module`]), since mutated bytes rarely get past validation.
//! * [`program_exec`]: the executor, host and overlay never panic, corrupt memory or report a node
//!   fault (`MemHost` is a sound view, so one would be the executor's bug), however a batch of calls
//!   to the hand-written guests is shaped: callees, methods, inputs, values, the kinds and
//!   payability their interfaces declare, the kind each call runs as, and stale version digests.
//!   Only validated code runs, as on a node.
//! * [`interface_codec`]: `interface::decode` never panics, and what it accepts is canonical and
//!   keeps every rule of §3.2; an interface built from the same bytes that `encode` accepts decodes
//!   back to itself.

use arbitrary::{Arbitrary, Unstructured};
use blockula_vm::config::{MAX_INTERFACE_BYTES, MAX_METHODS, MAX_PAGES, MAX_TABLE_ENTRIES};
use blockula_vm::engine::EngineKind;
use blockula_vm::exec::{ANY_VERSION, Call, TxLimits, execute};
use blockula_vm::host::{MethodInfo, MethodKind};
use blockula_vm::interface::{self, Arg, Interface, Method, Type};
use blockula_vm::meter::SCHEDULE_V0;
use blockula_vm::testing::{MemHost, addr};
use blockula_vm::upload::Refusal;
use std::collections::BTreeSet;

/// `validate()` must never panic, and every module it accepts must upload. `upload()` runs the
/// test translation under `catch_unwind` and reports a panic as a node fault, which we turn back
/// into a panic for the fuzzer to catch; a refusal is a disagreement between validation and the
/// engine, also a finding. Input that starts with the Wasm magic is the module; anything else is
/// the generator's choices ([`smith_module`]).
pub fn program_validate(data: &[u8]) {
    if data.starts_with(WASM_MAGIC) {
        validate_and_upload(data);
    } else if let Some(wasm) = smith_module(data) {
        validate_and_upload(&wasm);
    }
}

const WASM_MAGIC: &[u8] = b"\0asm";

fn validate_and_upload(wasm: &[u8]) {
    if blockula_vm::validate::validate(wasm).is_err() {
        return;
    }
    let mut host = MemHost::new();
    match host.upload(wasm) {
        Ok(Ok(_)) => {}
        Ok(Err(Refusal::Invalid(e))) => panic!("validate() accepted a module upload() then refused ({:?})", e.rule),
        Ok(Err(Refusal::Translate(_))) => panic!("validate() accepted a module the engine refused"),
        Err(fault) => panic!("validate() accepted a module whose upload is a node fault: {fault}"),
    }
}

/// What wasm-smith may generate: the subset upload validation accepts (Wasm 1.0's integer
/// subset: no floats, no proposal past MVP and mutable globals; one memory with a maximum of at
/// most `MAX_PAGES`; at most one funcref table of at most `MAX_TABLE_ENTRIES`; no start function,
/// no custom sections), with bounded counts so a module stays a fast input. [`v0_shape`] does what
/// no setting can.
///
/// No imports: wasm-smith picks them from a given list (ABI v1's) only through its `wasmparser`
/// feature, which would turn on wasmparser's `simd` feature in this crate, so the fuzzers' build
/// of the upload validator would no longer be the node's. Imports are fuzzed through the guests'
/// seeds on the raw path, which import ABI v1 functions.
fn smith_config() -> wasm_smith::Config {
    use wasm_smith::InstructionKind::{Control, MemoryInt, NumericInt, Parametric, Variable};
    wasm_smith::Config {
        allowed_instructions: wasm_smith::InstructionKinds::new(&[NumericInt, Parametric, Variable, MemoryInt, Control]),
        allow_floats: false,
        allow_start_export: false,
        generate_custom_sections: false,
        max_imports: 0,
        max_exports: 0,
        min_memories: 1,
        max_memories: 1,
        memory_max_size_required: true,
        max_memory32_bytes: u64::from(MAX_PAGES) << 16,
        max_tables: 1,
        max_table_elements: u64::from(MAX_TABLE_ENTRIES),
        // At least four of each, so most modules have bodies worth translating and a `[] -> []`
        // function to export as a method.
        min_funcs: 4,
        max_funcs: 32,
        min_types: 4,
        max_types: 16,
        max_globals: 16,
        max_data_segments: 4,
        max_element_segments: 4,
        max_elements: 32,
        max_instructions: 200,
        bulk_memory_enabled: false,
        custom_page_sizes_enabled: false,
        exceptions_enabled: false,
        extended_const_enabled: false,
        gc_enabled: false,
        memory64_enabled: false,
        multi_value_enabled: false,
        reference_types_enabled: false,
        relaxed_simd_enabled: false,
        saturating_float_to_int_enabled: false,
        shared_everything_threads_enabled: false,
        sign_extension_ops_enabled: false,
        simd_enabled: false,
        tail_call_enabled: false,
        threads_enabled: false,
        wide_arithmetic_enabled: false,
        ..wasm_smith::Config::default()
    }
}

/// A module wasm-smith generates from `data` under [`smith_config`], shaped for v0 by
/// [`v0_shape`], or `None` if `data` runs out before it's complete.
pub fn smith_module(data: &[u8]) -> Option<Vec<u8>> {
    let module = wasm_smith::Module::new(smith_config(), &mut Unstructured::new(data)).ok()?;
    v0_shape(&module.to_bytes())
}

/// What no wasm-smith setting asks for, done to its output with every section copied as it is
/// but two patched in place and one written anew:
/// - each `memory.grow` (`0x40 0x00`, `[i32] -> [i32]`) becomes `i32.clz; nop` (`0x67 0x01`), of the
///   same type and length: validation refuses a grow anywhere, and wasm-smith has no switch for it;
/// - the memory's initial size is raised to its maximum (validation wants a fixed memory);
/// - the exports are `memory` and, for each defined function of type `[] -> []`, `bk_f<index>` (at
///   most `MAX_METHODS`): the only exports validation allows.
///
/// `None` if wasmparser can't read what wasm-smith wrote (never expected).
fn v0_shape(wasm: &[u8]) -> Option<Vec<u8>> {
    use wasmparser::{Operator, Parser, Payload};
    let mut patched = wasm.to_vec();
    let mut nullary_types = Vec::new();
    let mut methods = Vec::new();
    let mut sections = Vec::new();
    for payload in Parser::new(0).parse_all(wasm) {
        let payload = payload.ok()?;
        match &payload {
            Payload::TypeSection(r) => {
                for ty in r.clone().into_iter_err_on_gc_types() {
                    let ty = ty.ok()?;
                    nullary_types.push(ty.params().is_empty() && ty.results().is_empty());
                }
            }
            // No imports, so a defined function's index is its place here.
            Payload::FunctionSection(r) => {
                for (index, ty) in r.clone().into_iter().enumerate() {
                    if nullary_types.get(usize::try_from(ty.ok()?).ok()?) == Some(&true) && methods.len() < MAX_METHODS {
                        methods.push(u32::try_from(index).ok()?);
                    }
                }
            }
            // One memory: its count (1), its flags (1: it has a maximum), then its initial and
            // maximum sizes, a byte each (at most `MAX_PAGES`).
            Payload::MemorySection(r) => {
                let at = r.range().start;
                if let Some(&[1, 1, initial, maximum]) = wasm.get(at..at + 4)
                    && initial <= maximum
                    && maximum < 0x80
                {
                    patched[at + 2] = maximum;
                }
            }
            Payload::CodeSectionEntry(body) => {
                let mut ops = body.get_operators_reader().ok()?;
                while !ops.eof() {
                    let (op, offset) = ops.read_with_offset().ok()?;
                    if matches!(op, Operator::MemoryGrow { mem: 0 }) && wasm.get(offset..offset + 2) == Some(&[0x40, 0x00][..]) {
                        patched[offset..offset + 2].copy_from_slice(&[0x67, 0x01]);
                    }
                }
            }
            _ => {}
        }
        if let Some((id, range)) = payload.as_section() {
            sections.push((id, range.end));
        }
    }
    // Every section from the end of the one before (its id and size included), wasm-smith's
    // exports replaced by ours, which go before the first section that follows exports.
    let mut out = patched[..8].to_vec();
    let mut at = 8;
    let mut exported = false;
    for (id, end) in sections {
        if !exported && matches!(id, 7..=12) {
            out.extend_from_slice(&exports(&methods));
            exported = true;
        }
        if id != 7 {
            out.extend_from_slice(&patched[at..end]);
        }
        at = end;
    }
    if !exported {
        out.extend_from_slice(&exports(&methods));
    }
    Some(out)
}

/// An export section: `memory`, then `bk_f<index>` for each function index in `methods`.
fn exports(methods: &[u32]) -> Vec<u8> {
    fn leb(mut n: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }
    let mut entries = Vec::new();
    leb(1 + methods.len() as u64, &mut entries);
    let mut entry = |name: &str, kind: u8, index: u32| {
        leb(name.len() as u64, &mut entries);
        entries.extend_from_slice(name.as_bytes());
        entries.push(kind);
        leb(u64::from(index), &mut entries);
    };
    entry("memory", 0x02, 0);
    for &f in methods {
        entry(&format!("bk_f{f}"), 0x00, f);
    }
    let mut section = vec![7];
    leb(entries.len() as u64, &mut section);
    section.extend_from_slice(&entries);
    section
}

/// `interface::decode` must never panic, and whatever it accepts must be the one encoding of an
/// interface that keeps docs/PROGRAMS-V0.md §3.2's rules: it encodes back to exactly `data`, its
/// methods ascend with distinct selectors and at most one initializer, `method` answers for each,
/// and it matches exactly the exports its methods name. Then the same bytes build an interface
/// field by field ([`arbitrary_interface`], rules broken as often as kept): if `encode` accepts it,
/// it decodes back to itself.
pub fn interface_codec(data: &[u8]) {
    if let Ok(i) = interface::decode(data) {
        assert!(data.len() <= MAX_INTERFACE_BYTES);
        assert_eq!(interface::encode(&i).as_deref(), Ok(data), "decode accepted a second encoding");
        assert!((1..=MAX_METHODS).contains(&i.methods.len()));
        assert!(i.methods.windows(2).all(|w| w[0].name < w[1].name), "methods out of order");
        assert!(i.methods.iter().filter(|m| m.kind == MethodKind::Init).count() <= 1);
        let selectors: BTreeSet<_> = i.methods.iter().map(|m| interface::selector(&m.name)).collect();
        assert_eq!(selectors.len(), i.methods.len(), "two methods share a selector");
        for m in &i.methods {
            assert_eq!(i.method(&m.name), Some(MethodInfo { kind: m.kind, payable: m.payable }));
            assert!(!(m.payable && m.kind == MethodKind::View) && !(m.kind == MethodKind::Init && m.returns.is_some()));
        }
        let names: Vec<String> = i.methods.iter().map(|m| m.name.clone()).collect();
        assert_eq!(interface::check_exports(&i, &names), Ok(()));
    }
    if let Ok(built) = arbitrary_interface(&mut Unstructured::new(data))
        && let Ok(bytes) = interface::encode(&built)
    {
        assert_eq!(interface::decode(&bytes), Ok(built), "encode wrote bytes that decode to another interface");
    }
}

/// An interface from `u`'s bytes, field by field, with every rule as likely broken as kept:
/// counts past their limits, names that aren't identifiers, methods out of order, maxes out of
/// range, types nested too deep.
fn arbitrary_interface(u: &mut Unstructured) -> arbitrary::Result<Interface> {
    fn name(u: &mut Unstructured) -> arbitrary::Result<String> {
        const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_aeiouxyzA-\xc3";
        let len = u.int_in_range(0usize..=34)?;
        (0..len).map(|_| Ok(char::from(*u.choose(CHARS)?))).collect()
    }
    fn ty(u: &mut Unstructured, depth: u32) -> arbitrary::Result<Type> {
        let scalars = [Type::U8, Type::U16, Type::U32, Type::U64, Type::U128, Type::I64, Type::I128, Type::Bool, Type::Fang, Type::Address, Type::Name, Type::Hash32];
        Ok(match u.int_in_range(0u8..=15)? {
            12 => Type::Bytes { max: if u.arbitrary()? { u.int_in_range(0..=40_000)? } else { u.arbitrary()? } },
            13 => Type::Text { max: u.arbitrary()? },
            14 if depth < 6 => Type::Option(Box::new(ty(u, depth + 1)?)),
            15 if depth < 6 => Type::Vec { of: Box::new(ty(u, depth + 1)?), max: u.int_in_range(0..=1_100)? },
            n => scalars[usize::from(n) % scalars.len()].clone(),
        })
    }
    let count = u.int_in_range(0..=MAX_METHODS + 2)?;
    let mut methods = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = *u.choose(&[MethodKind::Call, MethodKind::View, MethodKind::Init])?;
        let args = (0..u.int_in_range(0..=18)?).map(|_| Ok(Arg { name: name(u)?, ty: ty(u, 0)? })).collect::<arbitrary::Result<_>>()?;
        let returns = if u.arbitrary()? { Some(ty(u, 0)?) } else { None };
        methods.push(Method { name: name(u)?, kind, payable: u.arbitrary()?, args, returns });
    }
    if u.arbitrary()? {
        methods.sort_by(|a, b| a.name.cmp(&b.name));
    }
    Ok(Interface { methods })
}

/// The four hand-written guests (S2), deployed for every run so the executor has real code to run.
const GUESTS: &[(&str, &[u8])] = &[
    ("counter", include_bytes!("../../tests/programs/counter.wasm")),
    ("token", include_bytes!("../../tests/programs/token.wasm")),
    ("escrow", include_bytes!("../../tests/programs/escrow.wasm")),
    ("router", include_bytes!("../../tests/programs/router.wasm")),
];

/// The guests' interfaces, as `UploadCode` carries them beside their code.
#[cfg(test)]
const GUEST_INTERFACES: &[(&str, &[u8])] = &[
    ("counter", include_bytes!("../../tests/programs/counter.iface")),
    ("token", include_bytes!("../../tests/programs/token.iface")),
    ("escrow", include_bytes!("../../tests/programs/escrow.iface")),
    ("router", include_bytes!("../../tests/programs/router.iface")),
];

/// Method names the guests export (`bk_<method>`), plus one they don't, so the "no such export"
/// path is fuzzed too.
const METHODS: &[&str] = &[
    "get", "increment", "transfer", "balance_of", "total_supply", "init", "open", "release",
    "refund", "send", "try_send", "mint", "nope",
];

#[derive(Arbitrary, Debug)]
pub struct Scenario {
    engine: u8,
    /// Interface entries to change before the batch runs: (guest, method, kind, payable).
    methods: Vec<(u8, u8, u8, bool)>,
    calls: Vec<CallSpec>,
}

#[derive(Arbitrary, Debug)]
pub struct CallSpec {
    program: u8,
    method: u8,
    /// The kind the call runs as (`Call`, `View` or `Init`).
    kind: u8,
    /// Sign a digest of zeros instead of the version in force.
    stale: bool,
    value: u64,
    input: Vec<u8>,
}

const KINDS: [MethodKind; 3] = [MethodKind::Call, MethodKind::View, MethodKind::Init];

/// Drive the executor with a batch of calls to the guests, shaped by the fuzzer. The engine, the
/// callees, the methods, the inputs and the values are all fuzzer-chosen; any panic or ASan report
/// is a finding. The receipt is discarded — a failed transaction is a valid outcome, not a bug.
pub fn program_exec(scenario: Scenario) {
    // `ref038` is off, so ALL is the node's engine alone.
    let engines = EngineKind::ALL;
    let kind = engines[usize::from(scenario.engine) % engines.len()];

    let mut host = MemHost::new();
    let signer = addr("signer");
    host.fund(signer, 1_000_000_000_000_000);
    let declared: Vec<_> = GUESTS.iter().map(|(name, _)| addr(name)).collect();
    for (name, wasm) in GUESTS {
        // The committed guests always upload; deploy would only panic if that regressed.
        host.deploy(addr(name), wasm);
    }
    for (program, method, kind, payable) in scenario.methods.into_iter().take(8) {
        let program = addr(GUESTS[usize::from(program) % GUESTS.len()].0);
        let method = METHODS[usize::from(method) % METHODS.len()];
        if host.interfaces[&program].methods.contains_key(method) {
            host.set_method(program, method, KINDS[usize::from(kind) % KINDS.len()], payable);
        }
    }

    let calls: Vec<Call> = scenario
        .calls
        .into_iter()
        .take(6)
        .map(|c| {
            let (name, _) = GUESTS[usize::from(c.program) % GUESTS.len()];
            let mut input = c.input;
            input.truncate(8192);
            let call = Call {
                program: addr(name),
                version_digest: ANY_VERSION,
                method: METHODS[usize::from(c.method) % METHODS.len()].to_string(),
                input,
                value: c.value,
                kind: KINDS[usize::from(c.kind) % KINDS.len()],
            };
            if c.stale { call } else { host.bind(call) }
        })
        .collect();
    if calls.is_empty() {
        return;
    }

    let Ok((engine, _loaded)) = host.engine_for(kind, &SCHEDULE_V0, &declared) else {
        panic!("the guests' code doesn't load");
    };
    let limits = TxLimits { fuel_limit: 10_000_000, storage_limit: 1_000_000_000 };
    if let Err(fault) = execute(engine.as_ref(), &host, signer, &calls, &MemHost::any_version(&declared), limits, &SCHEDULE_V0) {
        panic!("a node fault on a sound view: {fault}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbitrary::Unstructured;
    use std::fs;
    use std::path::Path;

    fn seeds(target: &str) -> Vec<Vec<u8>> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds").join(target);
        fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .map(|e| fs::read(e.unwrap().path()).unwrap())
            .collect()
    }

    // Every seed runs without a panic, on stable.
    #[test]
    fn program_validate_accepts_its_seeds() {
        for seed in seeds("program_validate") {
            program_validate(&seed);
        }
        // The guests validate and upload.
        for (_, wasm) in GUESTS {
            program_validate(wasm);
        }
        // The generator's seeds make modules that validate, so the translation runs on them.
        let generated: Vec<Vec<u8>> = seeds("program_validate").into_iter().filter(|s| !s.starts_with(WASM_MAGIC)).collect();
        assert!(!generated.is_empty());
        for seed in generated {
            let wasm = smith_module(&seed).expect("a module");
            blockula_vm::validate::validate(&wasm).expect("a generator seed's module validates");
        }
    }

    #[test]
    fn program_exec_accepts_its_seeds() {
        for seed in seeds("program_exec") {
            if let Ok(scenario) = Scenario::arbitrary_take_rest(Unstructured::new(&seed)) {
                program_exec(scenario);
            }
        }
    }

    /// Inputs for the generator: `n` bytes from a xorshift seeded with `seed`, never starting with
    /// the Wasm magic.
    fn noise(start: u64, n: usize) -> Vec<u8> {
        let mut x = start.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        let mut out: Vec<u8> = (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect();
        out[0] |= 1;
        out
    }

    /// The generator does its job: most of what it writes passes upload validation (and so the
    /// test translation runs on it), and what validation refuses, it refuses for the generator's
    /// limits (body sizes, segments), never for the shape `v0_shape` gives every module.
    /// `cargo test --release -- the_generator --nocapture` prints the refusals.
    #[test]
    fn the_generator_stays_in_v0s_subset() {
        use blockula_vm::validate::{Rule, validate};
        let (runs, mut valid, mut bytes, mut methods, mut grows) = (200usize, 0, 0, 0, 0);
        let mut refused = std::collections::BTreeMap::<Rule, usize>::new();
        for seed in 0..runs {
            let data = noise(seed as u64, 4_096);
            let Some(wasm) = smith_module(&data) else { continue };
            let unshaped = wasm_smith::Module::new(smith_config(), &mut Unstructured::new(&data)).unwrap().to_bytes();
            for payload in wasmparser::Parser::new(0).parse_all(&unshaped) {
                if let wasmparser::Payload::CodeSectionEntry(body) = payload.unwrap() {
                    let mut ops = body.get_operators_reader().unwrap();
                    while !ops.eof() {
                        grows += usize::from(matches!(ops.read().unwrap(), wasmparser::Operator::MemoryGrow { .. }));
                    }
                }
            }
            match validate(&wasm) {
                Ok(info) => {
                    valid += 1;
                    bytes += wasm.len();
                    methods += info.exports.len();
                    validate_and_upload(&wasm);
                }
                Err(e) => *refused.entry(e.rule).or_default() += 1,
            }
        }
        println!("{valid} of {runs} generated modules validate ({bytes} bytes, {methods} methods, {grows} memory.grow replaced); refused: {refused:?}");
        assert!(refused.keys().all(|r| matches!(r, Rule::Segment | Rule::BodySize)), "a generated module broke a rule its shape keeps: {refused:?}");
        assert!(valid * 4 >= runs * 3, "only {valid} of {runs} generated modules validate: {refused:?}");
        assert!(methods > 0 && grows > 0, "the shaping did nothing to check: {methods} methods, {grows} grows");
    }

    /// The guests' interfaces (`tests/programs/*.iface`) and docs/PROGRAMS-V0.md §3.4's example:
    /// each a seed, and each decodes.
    #[test]
    fn interface_codec_accepts_its_seeds() {
        let seeds = seeds("interface_codec");
        assert_eq!(seeds.len(), 5);
        for seed in &seeds {
            interface::decode(seed).expect("every seed is an interface");
            interface_codec(seed);
        }
        for (name, iface) in GUEST_INTERFACES {
            assert!(seeds.iter().any(|s| s == iface), "{name}.iface is a seed");
        }
    }

    /// Every one-byte change and every cut of each seed, with no panic and the harness's checks holding.
    #[test]
    fn interface_codec_runs_on_mutations() {
        for seed in seeds("interface_codec") {
            for i in 0..seed.len() {
                interface_codec(&seed[..i]);
                for v in [0, 1, 2, 0x10, 0x20, 0x41, 0x7f, 0x80, 0xff] {
                    let mut m = seed.clone();
                    m[i] = v;
                    interface_codec(&m);
                }
            }
        }
        for seed in 0..200 {
            interface_codec(&noise(seed, 512));
        }
    }

    // A couple of inputs that reach past validate(), for coverage of the exec path itself.
    #[test]
    fn program_exec_runs_a_handful_of_scenarios() {
        for bytes in [vec![0u8; 64], vec![1u8; 200], (0u8..=255).collect::<Vec<_>>()] {
            if let Ok(scenario) = Scenario::arbitrary_take_rest(Unstructured::new(&bytes)) {
                program_exec(scenario);
            }
        }
    }
}
