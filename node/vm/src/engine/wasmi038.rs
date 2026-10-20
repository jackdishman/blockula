//! wasmi 0.38.0, the last audited wasmi release: a differential oracle for tests only
//! (`--features ref038`), never a node's engine. Its fuel follows its own IR, so tests compare its
//! receipts with soroban-wasmi's on everything but fuel.
//!
//! # The 4.2 settings, mapped
//!
//! | 4.2 setting | Here | Note |
//! |---|---|---|
//! | `consume_fuel(true)` | `consume_fuel(true)` | |
//! | `operator_cost(schedule)` | None | **Gap**: costs are private and fixed: 1 per emitted wasmi IR instruction plus its copy and call costs, so fuel follows wasmi's register IR, not Wasm. `meter::OperatorCosts` doesn't reach it |
//! | `fuel_cost(dynamic)` | None | Translation in `Eager` mode is unmetered; `exec.rs` charges instantiation |
//! | `compilation_mode(Eager)` | `compilation_mode(Eager)` | Also its default |
//! | `floats(false)` | `floats(false)` | |
//! | `wasm_mutable_global(true)` | `wasm_mutable_global(true)` | |
//! | sign-extension, multi-value, multi-memory, bulk-memory, reference-types, saturating float→int, tail-call, extended-const off | each set `false` | |
//! | SIMD, relaxed SIMD, memory64, custom page sizes, wide arithmetic | None | Unknown to 0.38: refused |
//! | `allow_start_fn(false)` | `InstancePre::ensure_no_start` | **Gap** at load: `validate.rs` refuses a start section |
//! | `ignore_custom_sections(true)` | `ignore_custom_sections(true)` | |
//! | recursion 1,024, stack 64 KiB to 1 MiB | `set_stack_limits(StackLimits::new(8_192, 131_072, 1_024))` (8-byte cells) | Per `Func::call`, so per frame |
//! | `enforced_limits(strict)` | `enforced_limits(EnforcedLimits::strict())` | |
//! | `deterministic` | None | Moot with floats off. The crate feature `no-hash-maps` keeps its maps ordered |
//! | cached stacks | `set_cached_stacks(2)` | |

use super::{BlockEngine, CodeHash, CodeInfo, EngineKind, Host, HostPtr, Instance, Invoked, LoadError, Trap, WasmTrap};
use crate::{abi, config, meter::Schedule};
use std::collections::BTreeMap;
use wasmi as w;

#[derive(Clone, Copy)]
pub(crate) struct Slot {
    host: HostPtr,
    memory: Option<w::Memory>,
}

impl w::core::HostError for Trap {}

pub struct Wasmi038Engine {
    engine: w::Engine,
    linker: w::Linker<Slot>,
    modules: BTreeMap<CodeHash, (w::Module, CodeInfo)>,
}

/// The engine configuration: 4.2 as far as 0.38's `Config` reaches. The schedule's operator
/// costs have nowhere to go (the table above).
pub fn config(_schedule: &Schedule) -> w::Config {
    let cells = |bytes: usize| bytes / 8;
    let stack = w::StackLimits::new(
        cells(config::MIN_STACK_BYTES),
        cells(config::MAX_STACK_BYTES),
        config::MAX_RECURSION_DEPTH as usize,
    )
    .expect("the minimum stack is below the maximum");
    let mut c = w::Config::default();
    c.consume_fuel(true)
        .compilation_mode(w::CompilationMode::Eager)
        .floats(false)
        .wasm_mutable_global(true)
        .wasm_sign_extension(false)
        .wasm_saturating_float_to_int(false)
        .wasm_multi_value(false)
        .wasm_multi_memory(false)
        .wasm_bulk_memory(false)
        .wasm_reference_types(false)
        .wasm_tail_call(false)
        .wasm_extended_const(false)
        .ignore_custom_sections(true)
        .set_stack_limits(stack)
        .set_cached_stacks(2)
        .enforced_limits(w::EnforcedLimits::strict());
    c
}

impl Wasmi038Engine {
    pub fn new(schedule: &Schedule) -> Self {
        let engine = w::Engine::new(&config(schedule));
        let mut linker = w::Linker::new(&engine);
        for (index, f) in abi::HOST_FNS.iter().enumerate() {
            let ty = |t: &abi::Ty| match t {
                abi::Ty::I32 => w::core::ValType::I32,
                abi::Ty::I64 => w::core::ValType::I64,
            };
            let func_type = w::FuncType::new(f.params.iter().map(ty), f.results.iter().map(ty));
            linker
                .func_new(abi::MODULE, f.name, func_type, move |caller, params, results| {
                    host_call(index, caller, params, results)
                })
                .expect("ABI names are distinct");
        }
        Wasmi038Engine { engine, linker, modules: BTreeMap::new() }
    }
}

fn host_call(index: usize, mut caller: w::Caller<'_, Slot>, params: &[w::Val], results: &mut [w::Val]) -> Result<(), w::Error> {
    let mut args = [0i64; 8];
    for (arg, p) in args.iter_mut().zip(params) {
        *arg = match p {
            w::Val::I32(v) => i64::from(*v),
            w::Val::I64(v) => *v,
            _ => return Err(w::Error::host(Trap::Fault("a non-integer argument".into()))),
        };
    }
    let Slot { host, memory } = *caller.data();
    let memory = memory.ok_or_else(|| w::Error::host(Trap::Fault("a host call before instantiation".into())))?;
    let mut instance = Frame { caller: &mut caller, memory };
    // SAFETY: we're inside the `invoke` that put `host` in this store.
    let out = unsafe { host.get() }.call(index, &args[..params.len()], &mut instance).map_err(w::Error::host)?;
    if let Some(r) = results.first_mut() {
        *r = match abi::HOST_FNS[index].results[0] {
            abi::Ty::I32 => w::Val::I32(out as i32),
            abi::Ty::I64 => w::Val::I64(out),
        };
    }
    Ok(())
}

struct Frame<'a, 'b> {
    caller: &'a mut w::Caller<'b, Slot>,
    memory: w::Memory,
}

impl Instance for Frame<'_, '_> {
    fn memory(&mut self) -> &mut [u8] {
        self.memory.data_mut(&mut *self.caller)
    }
    fn fuel(&self) -> u64 {
        self.caller.get_fuel().unwrap_or(0)
    }
    fn set_fuel(&mut self, fuel: u64) {
        let _ = self.caller.set_fuel(fuel);
    }
}

fn from_error(e: w::Error) -> Trap {
    if let Some(ours) = e.downcast_ref::<Trap>() {
        return ours.clone();
    }
    use w::core::TrapCode as C;
    let Some(code) = e.as_trap_code() else { return Trap::Fault(e.to_string()) };
    Trap::Wasm(match code {
        C::OutOfFuel => return Trap::OutOfFuel,
        C::UnreachableCodeReached => WasmTrap::Unreachable,
        C::MemoryOutOfBounds => WasmTrap::MemoryOutOfBounds,
        C::TableOutOfBounds => WasmTrap::TableOutOfBounds,
        C::IndirectCallToNull => WasmTrap::IndirectCallToNull,
        C::IntegerDivisionByZero => WasmTrap::DivisionByZero,
        C::IntegerOverflow => WasmTrap::IntegerOverflow,
        C::BadConversionToInteger => WasmTrap::BadConversionToInteger,
        C::StackOverflow => WasmTrap::StackOverflow,
        C::BadSignature => WasmTrap::BadSignature,
        C::GrowthOperationLimited => WasmTrap::Other,
    })
}

impl BlockEngine for Wasmi038Engine {
    fn kind(&self) -> EngineKind {
        EngineKind::Wasmi038
    }

    fn load(&mut self, hash: CodeHash, wasm: &[u8]) -> Result<(), LoadError> {
        if self.modules.contains_key(&hash) {
            return Ok(());
        }
        let info = CodeInfo::read(wasm)?;
        let module = w::Module::new(&self.engine, wasm).map_err(|e| LoadError::Translate(e.to_string()))?;
        self.modules.insert(hash, (module, info));
        Ok(())
    }

    fn info(&self, hash: &CodeHash) -> Option<CodeInfo> {
        self.modules.get(hash).map(|(_, info)| *info)
    }

    fn invoke(&self, hash: &CodeHash, export: &str, fuel: u64, host: &mut dyn Host) -> Invoked {
        let Some((module, _)) = self.modules.get(hash) else {
            return Invoked { result: Err(Trap::Fault("code not loaded".into())), fuel_left: fuel };
        };
        let mut store = w::Store::new(&self.engine, Slot { host: HostPtr::new(host), memory: None });
        store.set_fuel(fuel).expect("metering is on");
        let result = (|| {
            let pre = self.linker.instantiate(&mut store, module).map_err(from_error)?;
            let instance = pre.ensure_no_start(&mut store).map_err(|e| Trap::Fault(e.to_string()))?;
            let memory = instance.get_memory(&store, "memory").ok_or_else(|| Trap::Fault("no memory export".into()))?;
            store.data_mut().memory = Some(memory);
            let func = instance.get_typed_func::<(), ()>(&store, export).map_err(|_| Trap::NoMethod)?;
            func.call(&mut store, ()).map_err(from_error)
        })();
        Invoked { result, fuel_left: store.get_fuel().unwrap_or(0) }
    }
}
