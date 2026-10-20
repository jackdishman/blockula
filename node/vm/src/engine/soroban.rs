//! soroban-wasmi 29.0.0, the engine (D1): Stellar's fork of wasmi 0.31, pinned exactly.
//!
//! # The 4.2 settings, mapped
//!
//! | 4.2 setting | Here | Note |
//! |---|---|---|
//! | `consume_fuel(true)` | `consume_fuel(true)`, `fuel_consumption_mode(Eager)` | Eager as Soroban runs it: bulk and `memory.grow` charges land before the operation (bulk memory is off and `validate.rs` refuses `memory.grow`) |
//! | `operator_cost(schedule)` | `set_fuel_costs`: `base`, `entity`, `load`, `store`, `call` from `meter::OperatorCosts` | **Gap**: five classes only. Its private ratios stay at 1 fuel per 8 locals on entry, per 8 values kept on a branch, per 64 bytes of `memory.grow`, per 8 table elements |
//! | `fuel_cost(dynamic)` (translation free) | Nothing to set | Translation and instantiation never consume fuel; `exec.rs` charges `instantiate_fuel` |
//! | `compilation_mode(Eager)` | Inherent | `Module::new` translates every body |
//! | `floats(false)` | `floats(false)` | **Gap**: float→int conversions in unreachable code still validate: `validate.rs` refuses them |
//! | `wasm_mutable_global(true)` | `wasm_mutable_global(true)` | Gates only imported and exported mutable globals, which `validate.rs` refuses anyway |
//! | sign-extension, multi-value, bulk-memory, reference-types, saturating float→int, tail-call, extended-const off | each set `false` | Defaults are mostly `true`: every one is set |
//! | SIMD, relaxed SIMD, multi-memory, memory64 off | No setter | Hard-coded off |
//! | custom page sizes, wide arithmetic off | No setter | Unknown to its parser: refused |
//! | `allow_start_fn(false)` | `InstancePre::ensure_no_start` at instantiation | **Gap** at load: `validate.rs` refuses a start section |
//! | `ignore_custom_sections(true)` | None | **Gap**: kept in the `Module`; `validate.rs` refuses them |
//! | `set_max_recursion_depth(1_024)`, stack 64 KiB to 1 MiB | `set_stack_limits(StackLimits::new(8_192, 131_072, 1_024))` (8-byte cells) | Per `Func::call`, so per frame |
//! | `enforced_limits(strict)` | None | **Gap**: `validate.rs` enforces 4.2's limits |
//! | `deterministic` | None | Moot with floats off |
//! | cached stacks | `set_cached_stacks(2)` | Not consensus; set anyway |
//!
//! # What is safe inside a host call (confirmed by this module's tests)
//!
//! `EngineInner::execute_func` holds a read guard of the engine's spin `RwLock` for the whole of
//! `Func::call`, host calls included. Anything that takes the write guard on the *same* engine
//! from inside a host call spins forever:
//! * **Deadlocks:** `Module::new` (translation), `Func::wrap`/`Func::new`, `Linker::func_wrap`/
//!   `func_new` (each registers a function type). `soroban_inside_host_call_*_deadlocks`.
//! * **Safe:** `Store::new`, `Linker::instantiate` of a module translated before the call
//!   (trampolines and `Func`s go into the store, function types are only read), `get_typed_func`,
//!   and a nested `TypedFunc::call` (a nested read guard; spin readers don't wait for a waiting
//!   writer). `soroban_inside_host_call_store_instantiate_and_call_are_safe`. That's exactly
//!   what a nested `call_v1` does here, so every declared code must be loaded before anything
//!   runs on the engine (`BlockEngine::load` takes `&mut self`, `invoke` `&self`).
//! * Translating on a *different* engine from inside a host call is safe (another lock).

use super::{BlockEngine, CodeHash, CodeInfo, EngineKind, Host, HostPtr, Instance, Invoked, LoadError, NodeFault, Trap, TrapKind};
use crate::{abi, config, meter::Schedule};
use soroban_wasmi as sw;
use std::collections::BTreeMap;

/// What every store of this engine holds: the host for the running `invoke`, and the instance's
/// memory once it's instantiated.
#[derive(Clone, Copy)]
pub(crate) struct Slot {
    host: HostPtr,
    memory: Option<sw::Memory>,
}

impl sw::core::HostError for Trap {}

pub struct SorobanEngine {
    engine: sw::Engine,
    linker: sw::Linker<Slot>,
    modules: BTreeMap<CodeHash, (sw::Module, CodeInfo)>,
}

/// The engine configuration: 4.2 as far as soroban-wasmi's `Config` reaches (the table above).
pub fn config(schedule: &Schedule) -> Result<sw::Config, NodeFault> {
    let mut costs = sw::FuelCosts::default();
    costs.base = schedule.ops.base.into();
    costs.entity = schedule.ops.entity.into();
    costs.load = schedule.ops.load.into();
    costs.store = schedule.ops.store.into();
    costs.call = schedule.ops.call.into();
    let cells = |bytes: usize| bytes / 8;
    let stack = sw::StackLimits::new(
        cells(config::MIN_STACK_BYTES),
        cells(config::MAX_STACK_BYTES),
        config::MAX_RECURSION_DEPTH as usize,
    )
    .map_err(|e| NodeFault::Engine(e.to_string()))?;
    let mut c = sw::Config::default();
    c.consume_fuel(true)
        .fuel_consumption_mode(sw::FuelConsumptionMode::Eager)
        .set_fuel_costs(costs)
        .floats(false)
        .wasm_mutable_global(true)
        .wasm_sign_extension(false)
        .wasm_saturating_float_to_int(false)
        .wasm_multi_value(false)
        .wasm_bulk_memory(false)
        .wasm_reference_types(false)
        .wasm_tail_call(false)
        .wasm_extended_const(false)
        .set_stack_limits(stack)
        .set_cached_stacks(2);
    Ok(c)
}

impl SorobanEngine {
    pub fn new(schedule: &Schedule) -> Result<Self, NodeFault> {
        let engine = sw::Engine::new(&config(schedule)?);
        let mut linker = sw::Linker::new(&engine);
        for (index, f) in abi::HOST_FNS.iter().enumerate() {
            let ty = |t: &abi::Ty| match t {
                abi::Ty::I32 => sw::core::ValueType::I32,
                abi::Ty::I64 => sw::core::ValueType::I64,
            };
            let func_type = sw::FuncType::new(f.params.iter().map(ty), f.results.iter().map(ty));
            linker
                .func_new(abi::MODULE, f.name, func_type, move |caller, params, results| {
                    host_call(index, caller, params, results)
                })
                .map_err(|e| NodeFault::Engine(e.to_string()))?;
        }
        Ok(SorobanEngine { engine, linker, modules: BTreeMap::new() })
    }
}

/// Every import lands here: unpack the arguments, hand the call to the executor, pack the result.
fn host_call(
    index: usize,
    mut caller: sw::Caller<'_, Slot>,
    params: &[sw::Value],
    results: &mut [sw::Value],
) -> Result<(), sw::core::Trap> {
    let mut args = [0i64; 8];
    if params.len() > args.len() {
        return Err(Trap::Fault(NodeFault::Host("a host function with more than 8 arguments")).into());
    }
    for (arg, p) in args.iter_mut().zip(params) {
        *arg = match p {
            sw::Value::I32(v) => i64::from(*v),
            sw::Value::I64(v) => *v,
            _ => return Err(Trap::Fault(NodeFault::Host("a non-integer argument")).into()),
        };
    }
    let Slot { host, memory } = *caller.data();
    let memory = memory.ok_or(Trap::Fault(NodeFault::Host("a host call before instantiation")))?;
    let mut instance = Frame { caller: &mut caller, memory };
    // SAFETY: we're inside the `invoke` that put `host` in this store.
    let out = unsafe { host.get() }.call(index, &args[..params.len()], &mut instance)?;
    let result = abi::HOST_FNS.get(index).and_then(|f| f.results.first());
    match (results.first_mut(), result) {
        (Some(r), Some(abi::Ty::I32)) => *r = sw::Value::I32(out as i32),
        (Some(r), Some(abi::Ty::I64)) => *r = sw::Value::I64(out),
        (None, None) => {}
        _ => return Err(Trap::Fault(NodeFault::Host("results unlike the ABI's")).into()),
    }
    Ok(())
}

/// The calling instance, as `Host::call` sees it.
struct Frame<'a, 'b> {
    caller: &'a mut sw::Caller<'b, Slot>,
    memory: sw::Memory,
}

impl Instance for Frame<'_, '_> {
    fn memory(&mut self) -> &mut [u8] {
        self.memory.data_mut(&mut *self.caller)
    }
    fn fuel(&self) -> Result<u64, NodeFault> {
        let total = self.caller.fuel_total().ok_or(NodeFault::Fuel)?;
        let consumed = self.caller.fuel_consumed().ok_or(NodeFault::Fuel)?;
        total.checked_sub(consumed).ok_or(NodeFault::Fuel)
    }
    fn set_fuel(&mut self, fuel: u64) -> Result<(), NodeFault> {
        // The fork's own API (no `set_fuel`): zero both counters, then add.
        self.caller.reset_fuel().map_err(|_| NodeFault::Fuel)?;
        self.caller.add_fuel(fuel).map_err(|_| NodeFault::Fuel)
    }
}

/// The engine-neutral trap behind one of the engine's trap codes. Float conversions and growth
/// limits can't trap in validated code (floats and `memory.grow` are refused at upload; there's
/// no `table.grow`), so they're node faults.
fn wasm_trap(code: sw::core::TrapCode) -> Trap {
    use sw::core::TrapCode as C;
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

/// The engine-neutral trap behind a soroban-wasmi trap: ours if a host function raised it.
fn from_trap(t: sw::core::Trap) -> Trap {
    if let Some(ours) = t.downcast_ref::<Trap>() {
        return ours.clone();
    }
    match t.trap_code() {
        Some(code) => wasm_trap(code),
        None => Trap::Fault(NodeFault::Engine(t.to_string())),
    }
}

/// Fuel left in `store`, or why it can't be read.
fn fuel_left(store: &sw::Store<Slot>) -> Result<u64, NodeFault> {
    let total = store.fuel_total().ok_or(NodeFault::Fuel)?;
    let consumed = store.fuel_consumed().ok_or(NodeFault::Fuel)?;
    total.checked_sub(consumed).ok_or(NodeFault::Fuel)
}

impl BlockEngine for SorobanEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Soroban
    }

    fn load(&mut self, hash: CodeHash, wasm: &[u8]) -> Result<(), LoadError> {
        if self.modules.contains_key(&hash) {
            return Ok(());
        }
        let info = CodeInfo::read(wasm)?;
        let module = sw::Module::new(&self.engine, wasm).map_err(|e| LoadError::Translate(e.to_string()))?;
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
        let mut store = sw::Store::new(&self.engine, Slot { host: HostPtr::new(host), memory: None });
        if store.add_fuel(fuel).is_err() {
            return Invoked { result: Err(Trap::Fault(NodeFault::Fuel)), fuel_left: fuel };
        }
        let result = (|| {
            let pre = self.linker.instantiate(&mut store, module).map_err(|e| NodeFault::Instantiate(e.to_string()))?;
            let instance = pre.ensure_no_start(&mut store).map_err(|e| NodeFault::Instantiate(e.to_string()))?;
            let memory = instance.get_memory(&store, "memory").ok_or(NodeFault::Instantiate("no memory export".into()))?;
            store.data_mut().memory = Some(memory);
            let func = instance.get_typed_func::<(), ()>(&store, export).map_err(|_| TrapKind::NoMethod)?;
            func.call(&mut store, ()).map_err(from_trap)
        })();
        match fuel_left(&store) {
            Ok(fuel_left) => Invoked { result, fuel_left },
            // A node fault in the frame wins over the engine's fuel trouble: both are the node's.
            Err(fault) => Invoked { result: result.and(Err(Trap::Fault(fault))), fuel_left: 0 },
        }
    }
}

#[cfg(test)]
mod tests {
    //! What may run inside a host call on soroban-wasmi (the module comment). The deadlocking
    //! probes leave a thread spinning, so they're `#[ignore]`d: run them with
    //! `cargo test soroban_inside_host_call -- --ignored`.
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    const CALLEE: &str = r#"(module (memory (export "memory") 1 1) (func (export "bk_run")))"#;
    const OUTER: &str = r#"(module
        (import "probe" "go" (func $go))
        (memory (export "memory") 1 1)
        (func (export "bk_run") (call $go)))"#;

    /// Runs `OUTER`, whose one import does `inside` on the same engine, and reports whether the
    /// call came back within two seconds.
    fn probe(inside: fn(&mut sw::Caller<'_, ()>, &sw::Engine, &sw::Module)) -> bool {
        let (done, wait) = mpsc::channel();
        std::thread::spawn(move || {
            let engine = sw::Engine::new(&config(&crate::meter::SCHEDULE_V0).unwrap());
            let callee = std::sync::Arc::new(sw::Module::new(&engine, wat::parse_str(CALLEE).unwrap()).unwrap());
            let outer = sw::Module::new(&engine, wat::parse_str(OUTER).unwrap()).unwrap();
            let mut linker = sw::Linker::<()>::new(&engine);
            let (e2, c2) = (engine.clone(), callee.clone());
            linker
                .func_wrap("probe", "go", move |mut caller: sw::Caller<'_, ()>| inside(&mut caller, &e2, &c2))
                .unwrap();
            let mut store = sw::Store::new(&engine, ());
            store.add_fuel(1_000_000).unwrap();
            let instance = linker.instantiate(&mut store, &outer).unwrap().ensure_no_start(&mut store).unwrap();
            let run = instance.get_typed_func::<(), ()>(&store, "bk_run").unwrap();
            run.call(&mut store, ()).unwrap();
            let _ = done.send(());
        });
        wait.recv_timeout(Duration::from_secs(2)).is_ok()
    }

    #[test]
    fn soroban_inside_host_call_store_instantiate_and_call_are_safe() {
        assert!(probe(|_, engine, callee| {
            let mut store = sw::Store::new(engine, ());
            store.add_fuel(1_000).unwrap();
            let linker = sw::Linker::<()>::new(engine);
            let instance = linker.instantiate(&mut store, callee).unwrap().ensure_no_start(&mut store).unwrap();
            instance.get_typed_func::<(), ()>(&store, "bk_run").unwrap().call(&mut store, ()).unwrap();
        }));
    }

    #[test]
    fn soroban_inside_host_call_translating_on_another_engine_is_safe() {
        assert!(probe(|_, _, _| {
            let other = sw::Engine::new(&config(&crate::meter::SCHEDULE_V0).unwrap());
            sw::Module::new(&other, wat::parse_str(CALLEE).unwrap()).unwrap();
        }));
    }

    #[test]
    #[ignore = "deadlocks by design: leaves a thread spinning"]
    fn soroban_inside_host_call_translating_deadlocks() {
        assert!(!probe(|_, engine, _| {
            sw::Module::new(engine, wat::parse_str(CALLEE).unwrap()).unwrap();
        }));
    }

    #[test]
    #[ignore = "deadlocks by design: leaves a thread spinning"]
    fn soroban_inside_host_call_defining_a_host_function_deadlocks() {
        assert!(!probe(|caller, _, _| {
            sw::Func::wrap(caller, || {});
        }));
    }
}
