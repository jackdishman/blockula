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

use super::{BlockEngine, CodeHash, CodeInfo, EngineKind, Host, HostPtr, Instance, Invoked, LoadError, NodeFault, Trap, TrapKind};
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
pub fn config(_schedule: &Schedule) -> Result<w::Config, NodeFault> {
    let cells = |bytes: usize| bytes / 8;
    let stack = w::StackLimits::new(
        cells(config::MIN_STACK_BYTES),
        cells(config::MAX_STACK_BYTES),
        config::MAX_RECURSION_DEPTH as usize,
    )
    .map_err(|e| NodeFault::Engine(e.to_string()))?;
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
    Ok(c)
}

impl Wasmi038Engine {
    pub fn new(schedule: &Schedule) -> Result<Self, NodeFault> {
        let engine = w::Engine::new(&config(schedule)?);
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
                .map_err(|e| NodeFault::Engine(e.to_string()))?;
        }
        Ok(Wasmi038Engine { engine, linker, modules: BTreeMap::new() })
    }
}

fn fault(f: NodeFault) -> w::Error {
    w::Error::host(Trap::Fault(f))
}

fn host_call(index: usize, mut caller: w::Caller<'_, Slot>, params: &[w::Val], results: &mut [w::Val]) -> Result<(), w::Error> {
    let mut args = [0i64; 8];
    if params.len() > args.len() {
        return Err(fault(NodeFault::Host("a host function with more than 8 arguments")));
    }
    for (arg, p) in args.iter_mut().zip(params) {
        *arg = match p {
            w::Val::I32(v) => i64::from(*v),
            w::Val::I64(v) => *v,
            _ => return Err(fault(NodeFault::Host("a non-integer argument"))),
        };
    }
    let Slot { host, memory } = *caller.data();
    let memory = memory.ok_or_else(|| fault(NodeFault::Host("a host call before instantiation")))?;
    let mut instance = Frame { caller: &mut caller, memory };
    // SAFETY: we're inside the `invoke` that put `host` in this store.
    let out = unsafe { host.get() }.call(index, &args[..params.len()], &mut instance).map_err(w::Error::host)?;
    let result = abi::HOST_FNS.get(index).and_then(|f| f.results.first());
    match (results.first_mut(), result) {
        (Some(r), Some(abi::Ty::I32)) => *r = w::Val::I32(out as i32),
        (Some(r), Some(abi::Ty::I64)) => *r = w::Val::I64(out),
        (None, None) => {}
        _ => return Err(fault(NodeFault::Host("results unlike the ABI's"))),
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
    fn fuel(&self) -> Result<u64, NodeFault> {
        self.caller.get_fuel().map_err(|_| NodeFault::Fuel)
    }
    fn set_fuel(&mut self, fuel: u64) -> Result<(), NodeFault> {
        self.caller.set_fuel(fuel).map_err(|_| NodeFault::Fuel)
    }
}

fn from_error(e: w::Error) -> Trap {
    if let Some(ours) = e.downcast_ref::<Trap>() {
        return ours.clone();
    }
    use w::core::TrapCode as C;
    let Some(code) = e.as_trap_code() else { return Trap::Fault(NodeFault::Engine(e.to_string())) };
    Trap::Kind(match code {
        C::OutOfFuel => TrapKind::OutOfFuel,
        C::UnreachableCodeReached => TrapKind::Unreachable,
        C::MemoryOutOfBounds => TrapKind::MemoryOutOfBounds,
        C::TableOutOfBounds => TrapKind::TableOutOfBounds,
        C::IndirectCallToNull => TrapKind::IndirectCallToNull,
        C::IntegerDivisionByZero => TrapKind::DivisionByZero,
        C::IntegerOverflow => TrapKind::IntegerOverflow,
        C::StackOverflow => TrapKind::StackOverflow,
        C::BadSignature => TrapKind::BadSignature,
        C::BadConversionToInteger | C::GrowthOperationLimited => {
            return Trap::Fault(NodeFault::Engine(format!("{code:?} in validated code")));
        }
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
            return Invoked { result: Err(Trap::Fault(NodeFault::NotLoaded)), fuel_left: fuel };
        };
        let mut store = w::Store::new(&self.engine, Slot { host: HostPtr::new(host), memory: None });
        if store.set_fuel(fuel).is_err() {
            return Invoked { result: Err(Trap::Fault(NodeFault::Fuel)), fuel_left: fuel };
        }
        let result = (|| {
            let pre = self.linker.instantiate(&mut store, module).map_err(|e| NodeFault::Instantiate(e.to_string()))?;
            let instance = pre.ensure_no_start(&mut store).map_err(|e| NodeFault::Instantiate(e.to_string()))?;
            let memory = instance.get_memory(&store, "memory").ok_or(NodeFault::Instantiate("no memory export".into()))?;
            store.data_mut().memory = Some(memory);
            let func = instance.get_typed_func::<(), ()>(&store, export).map_err(|_| TrapKind::NoMethod)?;
            func.call(&mut store, ()).map_err(from_error)
        })();
        match store.get_fuel() {
            Ok(fuel_left) => Invoked { result, fuel_left },
            Err(_) => Invoked { result: result.and(Err(Trap::Fault(NodeFault::Fuel))), fuel_left: 0 },
        }
    }
}
